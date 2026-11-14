//! Re-pricing a sold post — the save plan every app's *Change price…* dialog
//! runs (`monetization.md` § The asking price → *Re-pricing a sold post*), and
//! the asking-price text parse the sell composer shares with it.
//!
//! **One plan, no app decides it.** The dialog shows the selling tier's two
//! price fields pre-filled ([`sale_price_prefill`]); on save the app hands the
//! two field strings to [`plan_sale_price_edit`], which answers exactly what to
//! send: at most one `tiers.update` carrying the changed non-empty fields
//! (every field the dialog does not carry is absent, so the nest keeps it), and
//! one `tiers.clear_field` per field the author emptied from a stored value —
//! here an empty field means *clear*, because the field was shown filled. A
//! dialog saved untouched sends nothing at all. [`SubscriptionsClient::
//! apply_sale_price_edit`] runs the plan in the ratified order, the update
//! before any clear.
//!
//! **The parse.** [`parse_asking_price_sats`] is the one text → sats rule: an
//! empty field is "no price", anything else must be a whole number of sats
//! that converts to the msat wire value. The composer used to turn a bad number
//! into "no price" silently; both surfaces now refuse it with the same typed
//! error, so the dialog stays open showing it.

use fauna_protocol::RpcRequester;
use fauna_protocol::subscriptions::{
    TierAskingPrice, TierClearFieldReply, TierClearFieldRequest, TierClearableField, TierItem,
    TierUpdateRequest,
};

use crate::SubscriptionsClient;

/// Why an asking-price field's text was refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AskingPriceParseError {
    /// Not a whole, non-negative number of sats (letters, a sign, a decimal
    /// point, a separator).
    NotAWholeNumber,
    /// A whole number too large to express in msats on the wire.
    TooLarge,
}

impl AskingPriceParseError {
    /// The catalog key every app shows for this refusal, so no app maps the
    /// variants to strings of its own.
    pub fn i18n_key(self) -> &'static str {
        match self {
            Self::NotAWholeNumber => "feed.compose_sell_price_not_whole",
            Self::TooLarge => "feed.compose_sell_price_invalid",
        }
    }
}

impl std::fmt::Display for AskingPriceParseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::NotAWholeNumber => "asking price is not a whole number of sats",
            Self::TooLarge => "asking price too large",
        })
    }
}

impl std::error::Error for AskingPriceParseError {}

/// The asking-price field's text as sats: `Ok(None)` for an empty (or
/// whitespace-only) field, `Ok(Some(sats))` for a whole number the wire can
/// carry, an error otherwise.
pub fn parse_asking_price_sats(text: &str) -> Result<Option<u64>, AskingPriceParseError> {
    let t = text.trim();
    if t.is_empty() {
        return Ok(None);
    }
    if !t.bytes().all(|b| b.is_ascii_digit()) {
        return Err(AskingPriceParseError::NotAWholeNumber);
    }
    let sats: u64 = t.parse().map_err(|_| AskingPriceParseError::TooLarge)?;
    TierAskingPrice::from_sats(sats).ok_or(AskingPriceParseError::TooLarge)?;
    Ok(Some(sats))
}

/// The dialog's two pre-filled fields from the selling tier: the free-text
/// `price_hint` and the machine price in sats (empty when unset, or when the
/// stored price is in a unit this build cannot show as sats — then saving the
/// field untouched leaves it alone, since the plan compares against this).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct SalePricePrefill {
    pub price_hint: String,
    pub asking_price: String,
}

/// See [`SalePricePrefill`].
pub fn sale_price_prefill(tier: &TierItem) -> SalePricePrefill {
    prefill_of(tier.price_hint.as_deref(), tier.asking_price.as_ref())
}

fn prefill_of(
    price_hint: Option<&str>,
    asking_price: Option<&TierAskingPrice>,
) -> SalePricePrefill {
    SalePricePrefill {
        price_hint: price_hint.unwrap_or_default().to_string(),
        asking_price: asking_price
            .and_then(TierAskingPrice::to_sats)
            .map(|s| s.to_string())
            .unwrap_or_default(),
    }
}

/// What a *Change price…* save sends. Empty (no update, no clears) when the
/// dialog was saved untouched.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct SalePriceEdit {
    /// The selling tier's name — the key every verb below names.
    pub tier: String,
    /// The one `tiers.update`, carrying only the changed non-empty fields.
    pub update: Option<TierUpdateRequest>,
    /// One `tiers.clear_field` per field emptied from a stored value, sent
    /// after the update.
    pub clears: Vec<TierClearableField>,
}

impl SalePriceEdit {
    /// Nothing to send.
    pub fn is_empty(&self) -> bool {
        self.update.is_none() && self.clears.is_empty()
    }

    /// The edit removes the machine price — on an advertised post the nest
    /// then ends the advertisement (`monetization.md` § The Nostr
    /// advertisement of a sold post → *Lifecycle* (2)).
    pub fn clears_asking_price(&self) -> bool {
        self.clears.contains(&TierClearableField::AskingPrice)
    }
}

/// The save plan: `tier` is the selling tier as the author's own `tiers.list`
/// answered it, `price_hint` and `asking_price` the dialog's two field strings.
///
/// A field is untouched when its text equals the pre-fill (trimmed; for the
/// asking price, when it parses to the stored sats), and then sends nothing. A
/// changed non-empty field rides the update; a field emptied from a stored
/// value is a clear; emptying a field that held nothing sends nothing. A bad
/// asking price refuses the whole plan.
pub fn plan_sale_price_edit(
    tier: &TierItem,
    price_hint: &str,
    asking_price: &str,
) -> Result<SalePriceEdit, AskingPriceParseError> {
    plan_sale_price_edit_stored(
        &tier.name,
        tier.price_hint.as_deref(),
        tier.asking_price.as_ref(),
        price_hint,
        asking_price,
    )
}

/// [`plan_sale_price_edit`] over the only three fields of the selling tier
/// the plan reads — its `name` and its stored `price_hint` and
/// `asking_price` — for a binding whose tier record carries just those
/// (UniFFI's `FfiTierItem` holds the price as sats, web's tier JSON
/// likewise). Same rule, same answer.
pub fn plan_sale_price_edit_stored(
    name: &str,
    stored_price_hint: Option<&str>,
    stored_asking_price: Option<&TierAskingPrice>,
    price_hint: &str,
    asking_price: &str,
) -> Result<SalePriceEdit, AskingPriceParseError> {
    let prefill = prefill_of(stored_price_hint, stored_asking_price);
    let mut update = TierUpdateRequest {
        name: name.to_string(),
        ..Default::default()
    };
    let mut clears = Vec::new();

    let hint = price_hint.trim();
    if hint != prefill.price_hint.trim() {
        if hint.is_empty() {
            if stored_price_hint.is_some() {
                clears.push(TierClearableField::PriceHint);
            }
        } else {
            update.price_hint = Some(hint.to_string());
        }
    }

    let asking = asking_price.trim();
    if asking != prefill.asking_price.trim() {
        match parse_asking_price_sats(asking)? {
            None => {
                if stored_asking_price.is_some() {
                    clears.push(TierClearableField::AskingPrice);
                }
            }
            Some(sats) => {
                let price =
                    TierAskingPrice::from_sats(sats).ok_or(AskingPriceParseError::TooLarge)?;
                if stored_asking_price != Some(&price) {
                    update.asking_price = Some(price);
                }
            }
        }
    }

    let changed = update.price_hint.is_some() || update.asking_price.is_some();
    Ok(SalePriceEdit {
        tier: name.to_string(),
        update: changed.then_some(update),
        clears,
    })
}

impl<R: RpcRequester> SubscriptionsClient<R> {
    /// `fauna.subscriptions.tiers.clear_field` — wipe one optional field of a
    /// tier back to unset, the one thing `tiers.update`'s keep-when-absent
    /// merge cannot say. Returns whether the nest cleared it.
    pub async fn tiers_clear_field(
        &self,
        name: impl Into<String>,
        field: TierClearableField,
    ) -> Result<bool, R::Error> {
        let reply: TierClearFieldReply = self
            .nest
            .request(
                "fauna.subscriptions.tiers.clear_field",
                TierClearFieldRequest {
                    name: name.into(),
                    field,
                    extra: Default::default(),
                },
            )
            .await?;
        Ok(reply.cleared)
    }

    /// Run a [`SalePriceEdit`]: the update first, then each clear, stopping
    /// at the first refusal. An empty plan sends nothing.
    pub async fn apply_sale_price_edit(&self, edit: &SalePriceEdit) -> Result<(), R::Error> {
        if let Some(update) = &edit.update {
            self.tiers_update(
                update.name.clone(),
                None,
                None,
                update.price_hint.clone(),
                None,
                None,
                update.asking_price.clone(),
            )
            .await?;
        }
        for field in &edit.clears {
            self.tiers_clear_field(edit.tier.clone(), *field).await?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fauna_client_testkit::{RejectingRequester, block_on};
    use fauna_core::data::Timestamp;
    use fauna_protocol::subscriptions::TierUpdateReply;
    use std::sync::Arc;

    fn tier(hint: Option<&str>, sats: Option<u64>) -> TierItem {
        TierItem {
            name: "unlock-ab12".into(),
            rank: 2,
            description: None,
            price_hint: hint.map(str::to_string),
            payment_url: None,
            auto_approve: false,
            created_at: Timestamp(1000),
            unlocks_post: Some("11".repeat(32)),
            asking_price: sats.map(|s| TierAskingPrice::from_sats(s).unwrap()),
            hidden: false,
            extra: Default::default(),
        }
    }

    fn price(sats: u64) -> Option<TierAskingPrice> {
        TierAskingPrice::from_sats(sats)
    }

    /// The bindings' form (`FfiTierItem`, web's tier JSON) carries only the
    /// three fields the plan reads; over every case it must answer exactly
    /// what the `TierItem` form answers, or the six follow-on apps would plan
    /// differently from tui.
    #[test]
    fn the_stored_fields_form_plans_exactly_as_the_tier_form() {
        let stored = [
            (None, None),
            (Some("$3"), None),
            (None, Some(2100)),
            (Some("$3"), Some(2100)),
        ];
        let inputs = [
            ("", ""),
            ("$3", "2100"),
            ("$4", "2100"),
            ("$3", "3000"),
            ("", "2100"),
            ("$3", ""),
            ("  $3 ", " 2100 "),
            ("$3", "21.5"),
        ];
        for (hint, sats) in stored {
            let t = tier(hint, sats);
            for (h, a) in inputs {
                assert_eq!(
                    plan_sale_price_edit(&t, h, a),
                    plan_sale_price_edit_stored(
                        &t.name,
                        t.price_hint.as_deref(),
                        t.asking_price.as_ref(),
                        h,
                        a
                    ),
                    "stored {hint:?}/{sats:?}, typed {h:?}/{a:?}"
                );
            }
        }
    }

    #[test]
    fn the_parse_reads_empty_as_no_price_and_refuses_what_is_not_a_whole_number() {
        assert_eq!(parse_asking_price_sats(""), Ok(None));
        assert_eq!(parse_asking_price_sats("   "), Ok(None));
        assert_eq!(parse_asking_price_sats(" 2100 "), Ok(Some(2100)));
        assert_eq!(parse_asking_price_sats("0"), Ok(Some(0)));
        for bad in ["21.5", "-3", "+3", "1,000", "abc", "2 100", "1e3"] {
            assert_eq!(
                parse_asking_price_sats(bad),
                Err(AskingPriceParseError::NotAWholeNumber),
                "{bad}"
            );
        }
        assert_eq!(
            parse_asking_price_sats(&u64::MAX.to_string()),
            Err(AskingPriceParseError::TooLarge)
        );
        assert_eq!(
            parse_asking_price_sats("99999999999999999999999"),
            Err(AskingPriceParseError::TooLarge)
        );
    }

    #[test]
    fn the_prefill_shows_both_stored_fields() {
        let p = sale_price_prefill(&tier(Some("5 EUR"), Some(2100)));
        assert_eq!(p.price_hint, "5 EUR");
        assert_eq!(p.asking_price, "2100");
        assert_eq!(
            sale_price_prefill(&tier(None, None)),
            SalePricePrefill::default()
        );
    }

    #[test]
    fn an_untouched_dialog_sends_nothing() {
        let t = tier(Some("5 EUR"), Some(2100));
        let edit = plan_sale_price_edit(&t, "5 EUR", "2100").unwrap();
        assert!(edit.is_empty(), "{edit:?}");
        // Surrounding whitespace and a leading zero are not a change.
        assert!(
            plan_sale_price_edit(&t, " 5 EUR ", "02100")
                .unwrap()
                .is_empty()
        );
        assert!(
            plan_sale_price_edit(&tier(None, None), "", "")
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn a_changed_hint_alone_rides_one_update() {
        let edit = plan_sale_price_edit(&tier(Some("5 EUR"), Some(2100)), "6 EUR", "2100").unwrap();
        let u = edit.update.expect("an update");
        assert_eq!(u.name, "unlock-ab12");
        assert_eq!(u.price_hint.as_deref(), Some("6 EUR"));
        assert_eq!(
            u.asking_price, None,
            "the untouched price is absent, so kept"
        );
        assert!(edit.clears.is_empty());
    }

    #[test]
    fn a_changed_asking_price_alone_rides_one_update() {
        let edit = plan_sale_price_edit(&tier(Some("5 EUR"), Some(2100)), "5 EUR", "3000").unwrap();
        assert!(!edit.clears_asking_price());
        let u = edit.update.expect("an update");
        assert_eq!(u.price_hint, None);
        assert_eq!(u.asking_price, price(3000));
        assert!(edit.clears.is_empty());
    }

    #[test]
    fn both_changed_ride_the_same_update() {
        let edit = plan_sale_price_edit(&tier(Some("5 EUR"), Some(2100)), "6 EUR", "3000").unwrap();
        let u = edit.update.expect("an update");
        assert_eq!(u.price_hint.as_deref(), Some("6 EUR"));
        assert_eq!(u.asking_price, price(3000));
        assert!(edit.clears.is_empty());
    }

    #[test]
    fn an_emptied_asking_price_is_a_clear() {
        let edit = plan_sale_price_edit(&tier(Some("5 EUR"), Some(2100)), "5 EUR", "").unwrap();
        assert_eq!(edit.update, None);
        assert_eq!(edit.clears, vec![TierClearableField::AskingPrice]);
        assert!(edit.clears_asking_price());
    }

    #[test]
    fn an_emptied_hint_is_a_clear() {
        let edit = plan_sale_price_edit(&tier(Some("5 EUR"), Some(2100)), "  ", "2100").unwrap();
        assert_eq!(edit.update, None);
        assert_eq!(edit.clears, vec![TierClearableField::PriceHint]);
    }

    #[test]
    fn a_price_set_where_none_was_stored_is_an_update() {
        let edit = plan_sale_price_edit(&tier(Some("5 EUR"), None), "5 EUR", "2100").unwrap();
        assert_eq!(edit.update.expect("an update").asking_price, price(2100));
        assert!(edit.clears.is_empty());
    }

    #[test]
    fn a_change_and_a_clear_together_keep_both() {
        let edit = plan_sale_price_edit(&tier(Some("5 EUR"), Some(2100)), "", "3000").unwrap();
        assert_eq!(edit.update.expect("an update").asking_price, price(3000));
        assert_eq!(edit.clears, vec![TierClearableField::PriceHint]);
    }

    #[test]
    fn a_bad_number_refuses_the_whole_plan() {
        let t = tier(Some("5 EUR"), Some(2100));
        assert_eq!(
            plan_sale_price_edit(&t, "6 EUR", "21.5"),
            Err(AskingPriceParseError::NotAWholeNumber)
        );
        assert_eq!(
            plan_sale_price_edit(&t, "6 EUR", &u64::MAX.to_string()),
            Err(AskingPriceParseError::TooLarge)
        );
    }

    #[test]
    fn the_refusals_name_their_catalog_keys() {
        assert_eq!(
            AskingPriceParseError::TooLarge.i18n_key(),
            "feed.compose_sell_price_invalid"
        );
        assert_eq!(
            AskingPriceParseError::NotAWholeNumber.i18n_key(),
            "feed.compose_sell_price_not_whole"
        );
    }

    fn nest() -> Arc<RejectingRequester> {
        Arc::new(
            RejectingRequester::new()
                .reply(
                    "fauna.subscriptions.tiers.update",
                    &TierUpdateReply {
                        updated: true,
                        extra: Default::default(),
                    },
                )
                .reply(
                    "fauna.subscriptions.tiers.clear_field",
                    &TierClearFieldReply {
                        cleared: true,
                        extra: Default::default(),
                    },
                ),
        )
    }

    #[test]
    fn tiers_clear_field_composes_kind_and_payload() {
        let rec = nest();
        let client = SubscriptionsClient::new(rec.clone());
        assert!(
            block_on(client.tiers_clear_field("gold", TierClearableField::AskingPrice)).unwrap()
        );
        let calls = rec.payloads();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].0, "fauna.subscriptions.tiers.clear_field");
        assert_eq!(calls[0].1["name"], "gold");
        assert_eq!(calls[0].1["field"], "asking_price");
    }

    #[test]
    fn the_update_runs_before_any_clear() {
        let rec = nest();
        let client = SubscriptionsClient::new(rec.clone());
        let edit = plan_sale_price_edit(&tier(Some("5 EUR"), Some(2100)), "", "3000").unwrap();
        block_on(client.apply_sale_price_edit(&edit)).unwrap();
        assert_eq!(
            rec.kinds(),
            vec![
                "fauna.subscriptions.tiers.update",
                "fauna.subscriptions.tiers.clear_field"
            ]
        );
        let calls = rec.payloads();
        assert_eq!(calls[0].1["name"], "unlock-ab12");
        assert_eq!(calls[0].1["asking_price"]["value"], 3_000_000);
        assert!(
            calls[0].1["unlocks_post"].is_null(),
            "the designation is never sent"
        );
        assert_eq!(calls[1].1["name"], "unlock-ab12");
        assert_eq!(calls[1].1["field"], "price_hint");
    }

    #[test]
    fn an_empty_plan_sends_nothing() {
        let rec = nest();
        let client = SubscriptionsClient::new(rec.clone());
        let edit = plan_sale_price_edit(&tier(None, Some(1)), "", "1").unwrap();
        block_on(client.apply_sale_price_edit(&edit)).unwrap();
        assert!(rec.kinds().is_empty());
    }
}
