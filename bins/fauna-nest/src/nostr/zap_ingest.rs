//! The NIP-57 trust gate, shared by both zap ingress points.
//!
//! `docs/goal/behavior/monetization.md` § Zap receipts — the trust model
//! ratifies that **the gate is applied at ingest, at every ingress, never at
//! read**. There are two ingresses — the external-relay sweep
//! (`sync_worker::process_inbound_event`) and the nest's own relay endpoint
//! (`relay_endpoint::handle_zap_receipt_inbox`) — and this module is the one
//! place either of them asks the trust question, so the two doors cannot
//! drift apart. A future third ingress gets the guarantee by calling
//! [`classify_incoming_zap`] rather than by reimplementing it.
//!
//! The split is deliberate: [`classify_incoming_zap`] answers *may we believe
//! this?* and [`record_zap`] performs the accounting write. The relay
//! endpoint needs them apart because it must refuse before storing the event
//! verbatim; the sweep calls them back to back.
//!
//! **A third shared question sits between them: [`gate_receipt_ingest`]** — the
//! `zaps.receipt.ingest` feature gate (`dynamic-features.md` § Evaluation
//! points). It is here for exactly the reason the trust question is: two doors
//! asking it separately drift, and a door that forgets it ingests ungated.
//! Both production ingresses call all three, in order — classify, gate, record.

use rusqlite::Connection;

use fauna_bridge_nostr::nip57::{TrustedZap, ZapVerdict, classify_zap_receipt, parse_zap_receipt};
use fauna_bridge_nostr::types::Event;

use crate::nostr::{db, store};

/// **Gate surface `zaps.receipt.ingest`** — the second half of the trust
/// question, asked once for both doors (`dynamic-features.md` § Charter
/// members: *"receipt ingestion → tip/purchase resolution"*).
///
/// `classify_incoming_zap` answers *may we believe this?*; this answers *may
/// this account run the zaps plane at all, and has it run it too much?* Both
/// are per-ingress questions with one implementation, for the same reason:
/// two doors asking them separately drift.
///
/// **Whose account.** The **payee** — the local account the receipt names. A
/// zap arrives without its recipient doing anything, which is exactly why the
/// registry gates *ingestion* rather than some send-side act the recipient
/// never performs, and why tier 1 sizes zaps' operation ceiling above the
/// payments plane's ("receipts arrive without the account's action").
///
/// **The counterparty delta** is resolved against the zaps plane's own records
/// — `nostr_zaps.sender_pubkey` for this payee. A sender who has zapped them
/// before is standing and costs `0`; a stranger costs `1`. The count itself is
/// never re-derived from those rows (they only answer *new?*), so nothing a
/// user can delete refunds a counterparty unit.
///
/// **The magnitude** is the receipt's own `amount_msats` — zaps declare
/// `Millisats`, and a receipt without a `bolt11` amount moves nothing this
/// nest can compare, so it contributes `0`.
///
/// A refusal means the receipt is **not ingested at all**: not stored, not
/// summed, not resolved into a tip or a purchase. That is the point of an
/// availability deny on this member (the Damus shape — the zap surface gone
/// while the rest of the product stands), and § Fail posture's "fails closed
/// for the operation, never open" for a quota that has run out.
pub async fn gate_receipt_ingest(
    state: &std::sync::Arc<crate::AppState>,
    zap: &TrustedZap,
) -> Result<(), fauna_protocol::RpcError> {
    let receipt = zap.receipt();

    // The payee's actor id. `classify_incoming_zap` has already refused a payee
    // this box does not host (their designated-signer set is empty), so this
    // read resolves for every `TrustedZap`; a miss can only mean the account
    // vanished between the two reads, and refusing then is the fail-closed
    // answer rather than gating somebody else's plane.
    let conn = state.db.conn().await;
    // A receipt this box already recorded is a redelivery, not a new operation:
    // it spends nothing and is refused nothing. See [`db::has_zap`] for why
    // skipping it is load-bearing rather than an optimization — without it the
    // gate becomes an amplifier for anyone holding a copy of a public receipt.
    if db::has_zap(&conn, zap.zap_event_id()).unwrap_or(false) {
        return Ok(());
    }
    let payee = db::get_account_by_pubkey(&conn, &receipt.target_pubkey)
        .ok()
        .flatten();
    let standing = receipt.sender_pubkey.as_deref().is_some_and(|sender| {
        db::has_zap_from_sender(&conn, &receipt.target_pubkey, sender).unwrap_or(false)
    });
    drop(conn);

    let Some(payee_actor) =
        payee.and_then(|acct| fauna_core::identity::ActorId::from_hex(&acct.actor_id).ok())
    else {
        return Err(fauna_protocol::RpcError::new(
            crate::feature_gate::CODE_FEATURE_DENIED,
            "error.features.denied",
        ));
    };

    crate::feature_gate::gate(
        state,
        &payee_actor.0,
        &fauna_core::feature_gate::GateOp {
            feature: fauna_core::feature_gate::GatedFeature::Zaps,
            surface: fauna_core::feature_gate::SURFACE_ZAPS_RECEIPT_INGEST,
            // An anonymous zap (no `sender_pubkey`) names no counterparty this
            // box could ever call standing, so it is a new one every time —
            // over-counting against the payee, the direction a bound tolerates.
            new_counterparties: u64::from(!standing),
            magnitude: receipt.amount_msats.unwrap_or(0),
        },
    )
    .await
}

/// Decide what a **signature-verified** kind-9735 event is worth, reading the
/// payee's designated-signer list from this nest's trust root.
///
/// The caller MUST have verified the event's own Schnorr signature first —
/// both ingress points do, and neither the pure verdict nor this wrapper
/// re-checks it. A signature proves only *who signed*; this answers *whose
/// signature counts*, which is the question that matters for a kind-9735.
///
/// Whose trust root applies is decided by the receipt's `p` tag. A payee
/// pubkey belonging to no local account resolves to the empty designation set
/// — the same answer as "this payee designated nobody" — so an unknown payee
/// fails closed through the ordinary path, with no separate arm to get wrong.
/// A DB error likewise degrades to the empty set: failing closed on a read
/// error is correct here, because the alternative is believing a receipt we
/// could not check.
///
/// The subject binding is read here too: when the receipt
/// names a zapped event, this looks up that event's stored author on the box
/// and hands it to the pure classifier, which refuses a receipt attributing a
/// zap to an event the payee did not author. A store-read error, like the
/// trust-root read error, degrades to `None` (not-held) so the classifier
/// fails closed rather than believing an unchecked subject.
pub fn classify_incoming_zap(conn: &Connection, event: &Event) -> ZapVerdict {
    let Ok(receipt) = parse_zap_receipt(event) else {
        return ZapVerdict::Untrusted(
            fauna_bridge_nostr::nip57::ZapUntrustedReason::NotAZapReceipt,
        );
    };
    let trusted =
        db::trusted_zap_signers_for_pubkey(conn, &receipt.target_pubkey).unwrap_or_default();
    let held_subject_author = receipt
        .target_event_id
        .as_deref()
        .and_then(|eid| store::event_author(conn, eid).unwrap_or(None));
    classify_zap_receipt(
        event,
        &receipt.target_pubkey,
        &trusted,
        held_subject_author.as_deref(),
    )
}

/// Record a believed zap in the accounting table the tip and purchase
/// surfaces read.
///
/// Both ingress points write the same row shape through this one function, so
/// every downstream consumer inherits the trust guarantee structurally
/// whichever door the receipt arrived through — which is the whole point of
/// gating at ingest rather than at read.
///
/// The only argument is a [`TrustedZap`], which is unconstructible outside the
/// verdict — it comes exclusively out of [`ZapVerdict::Trusted`]. So there is
/// no expressible way to record an unjudged receipt: a caller cannot fabricate
/// the witness, and the write reads the zap's own event id and timestamp from
/// the witness rather than a separately-passed event, so an event/receipt
/// mismatch is unrepresentable.
pub fn record_zap(conn: &Connection, zap: &TrustedZap) -> bool {
    record_zap_as(conn, zap, None)
}

/// [`record_zap`]'s full form: record the believed zap, naming the tier it
/// bought when it bought one.
///
/// `purchased_tier` is what makes the two consequence classes distinguishable
/// at rest. It is written **at ingest**, from the verdict this box already
/// reached, and no reader re-derives it — the same discipline the trust gate
/// follows one layer up. That is why the tip surface can exclude purchases
/// without re-judging anything: it reads a class that was decided once.
///
/// Returns `true` only when this call wrote the row — the receipt's first
/// arrival. A redelivery (the same event from a second relay, a sweep replay)
/// answers `false`, so a consequence keyed on it fires once; a write error
/// also answers `false`, because nothing was recorded to announce.
pub fn record_zap_as(conn: &Connection, zap: &TrustedZap, purchased_tier: Option<&str>) -> bool {
    let receipt = zap.receipt();
    db::insert_zap(
        conn,
        zap.zap_event_id(),
        receipt.target_event_id.as_deref(),
        &receipt.target_pubkey,
        receipt.sender_pubkey.as_deref(),
        receipt.amount_msats.map(|a| a as i64),
        zap.created_at() as i64,
        purchased_tier,
    )
    .unwrap_or(false)
}

/// The localized sentence a tip notification carries
/// (`notifications.md` § Localized body): the key names who tipped — a
/// display handle, or nobody the receipt identifies — and how much, with the
/// unit split decided once by [`fauna_core::format::tip_amount`] so a sub-sat
/// tip keeps its msats rather than reading "0 sats". The args are data.
pub fn tip_notification_body(
    sender: Option<&str>,
    amount_msats: Option<i64>,
) -> fauna_protocol::LocalizedText {
    let who = if sender.is_some() {
        "row_tip"
    } else {
        "row_tip_anonymous"
    };
    let amount = amount_msats.map(fauna_core::format::tip_amount);
    let key = match amount.as_ref().map(|a| a.key.as_str()) {
        Some("tips.sats") => format!("notifications.{who}_sats"),
        Some(_) => format!("notifications.{who}_msats"),
        None => format!("notifications.{who}"),
    };
    let mut body = fauna_protocol::LocalizedText::new(key);
    if let Some(sender) = sender {
        body = body.with_arg("sender", sender);
    }
    if let Some(value) = amount.as_ref().and_then(|a| a.args.get("value")) {
        body = body.with_arg("value", value);
    }
    body
}

/// Ring the post's author for a credited tip (`monetization.md` § Tips — a
/// tip's consequence is attribution, display and notification, never a
/// grant): one `tip` row on the notifications surface plus its push twin.
///
/// Both ingress points call this right after [`record_zap_as`], with what they
/// already hold, so the decision of *whether* a receipt notifies lives here
/// once and the two doors cannot drift. It notifies only when all three hold:
///
/// * the receipt was **newly recorded** — a redelivery rings nobody twice;
/// * it named a Fauna post ([`resolve_zap_subject`] answered) — a profile zap
///   or a zap on an event this box never published has no post to open;
/// * it stayed a **tip** — a receipt the split classed as a purchase is a
///   grant, which the subscriber flow announces, not a tip.
///
/// Call it with no SQLite connection held: the notification write takes the
/// same mutex. A failed write is logged and dropped — the tip itself is
/// already recorded and attributed, which is what the sender is owed.
pub async fn notify_tip(
    state: &std::sync::Arc<crate::AppState>,
    zap: &TrustedZap,
    subject: Option<&ZapSubject>,
    purchased_tier: Option<&str>,
    newly_recorded: bool,
) {
    let Some(subject) = subject else { return };
    if !newly_recorded || purchased_tier.is_some() {
        return;
    }
    let (Ok(fauna_core::identity::ActorId(payee)), Ok(post_id)) = (
        fauna_core::identity::ActorId::from_hex(&subject.payee_actor_id),
        hex::decode(&subject.fauna_post_id),
    ) else {
        return;
    };
    let receipt = zap.receipt();
    let sender_actor = subject
        .sender_actor_id
        .as_deref()
        .and_then(|hex_id| hex::decode(hex_id).ok());
    // Who to name: the tipper's local actor when this box knows them, else
    // the Nostr key their zap request was signed with — the same
    // mechanism-native fallback the tip list renders. Short hex, the like
    // row's precedent.
    let sender_handle = subject
        .sender_actor_id
        .as_deref()
        .or(receipt.sender_pubkey.as_deref())
        .map(|id| id.chars().take(8).collect::<String>());
    let text = crate::db::notifications::NotificationText::localized(tip_notification_body(
        sender_handle.as_deref(),
        receipt.amount_msats.map(|a| a as i64),
    ));
    let notif_type = fauna_protocol::notifications::NotifType::Tip;
    let now = fauna_core::data::Timestamp::now().as_i64();
    let inserted = state
        .db
        .insert_notification_undeduped(
            &payee,
            &notif_type,
            "fauna",
            sender_actor.as_deref(),
            Some(&post_id),
            &text,
            now,
        )
        .await;
    match inserted {
        Ok(notification_id) => state.ws.notify_push(
            &payee,
            fauna_protocol::PushEvent::Notification(
                fauna_protocol::push_events::NotificationPayload {
                    notification_id,
                    notif_type,
                    source: "fauna".into(),
                    sender_id: subject.sender_actor_id.clone(),
                    content_id: Some(subject.fauna_post_id.clone()),
                    summary: text.summary().to_string(),
                    body: text.body().cloned(),
                    timestamp: fauna_core::data::Timestamp::now_secs() as u64,
                    extra: std::collections::BTreeMap::new(),
                },
            ),
        ),
        Err(e) => tracing::warn!(
            zap_event_id = %zap.zap_event_id(),
            "tip notification not written: {e}"
        ),
    }
}

/// The Fauna coordinates a believed zap names, resolved from Nostr ones.
///
/// Everything here is a *lookup*, never a judgement: the judgement — is this
/// enough to be a purchase? — needs the tier row, which lives behind the async
/// `CacheDb` and therefore cannot be read while this box's single SQLite
/// connection is held. Splitting the two is what keeps the whole path
/// deadlock-free (`state.db` and the `conn` these functions take are the same
/// mutex).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ZapSubject {
    /// The Fauna post the zapped Nostr event was published for.
    pub fauna_post_id: String,
    /// Hex actor id of the payee — the local author being zapped.
    pub payee_actor_id: String,
    /// Hex actor id of the tipper, when their Nostr pubkey is linked to an
    /// account on **this** box. `None` for a stranger, which is the ordinary
    /// case and is why the waist's unbound-buyer fallback exists.
    pub sender_actor_id: Option<String>,
}

/// Resolve the Fauna coordinates of a believed zap, or `None` when it names
/// none — in which case the receipt can only ever be a tip.
///
/// Four ways to answer `None`, all ordinary rather than exceptional:
///
/// 1. **No `e` tag.** A profile zap names no subject at all, so there is no
///    post to buy.
/// 2. **The zapped event is not one we published.** Only `direction =
///    'outbound'` rows map a Nostr event to a *local* author's Fauna post;
///    an inbound row mirrors somebody else's event, whose zaps are not ours
///    to spend. This is the same restriction the tip read applies for the
///    same reason.
/// 3. **The zapped event has no Fauna post behind it** (no map row) — a
///    Nostr-native event this box relayed but never authored as a post.
/// 4. **The payee pubkey is not linked to a local account.** The trust gate
///    already refuses an unknown payee (their designated-signer set is empty),
///    so this is belt-and-braces; it stays because the purchase leg must
///    resolve an actor id it can put in an entitlement, and inferring one
///    would be exactly the guess this model refuses.
pub fn resolve_zap_subject(conn: &Connection, zap: &TrustedZap) -> Option<ZapSubject> {
    let receipt = zap.receipt();
    let target_event_id = receipt.target_event_id.as_deref()?;

    let entry = db::get_event_by_nostr_id(conn, target_event_id)
        .ok()
        .flatten()?;
    if entry.direction != "outbound" {
        return None;
    }

    let payee = db::get_account_by_pubkey(conn, &receipt.target_pubkey)
        .ok()
        .flatten()?;

    // The tipper is resolved on a best-effort basis: a zap from outside this
    // box is the common case, and it must still be able to buy — the waist
    // mints a claim code for an unbound buyer rather than dropping a payment
    // somebody really made (`monetization.md` § Pillar 3 Q4).
    let sender_actor_id = receipt
        .sender_pubkey
        .as_deref()
        .and_then(|pk| db::get_account_by_pubkey(conn, pk).ok().flatten())
        .map(|acct| acct.actor_id);

    Some(ZapSubject {
        fauna_post_id: entry.fauna_post_id,
        payee_actor_id: payee.actor_id,
        sender_actor_id,
    })
}

/// The provider token every zap-borne entitlement carries into the waist.
///
/// The waist records it for audit and claim-idempotency scoping and never
/// branches on it — `PaymentEntitlement::provider` is documented as exactly
/// that. It matches [`fauna_payments::tips::TipMechanism::NostrZap`]'s wire
/// token so both consequence classes name the same carrier the same way.
pub const ZAP_PROVIDER: &str = "nostr_zap";

/// Decide, and apply, what a believed zap on a Fauna post is worth — the
/// tip↔purchase split (`monetization.md` § Per-post pay-to-unlock, § The
/// asking price).
///
/// Returns the tier name when the receipt bought one, `None` when it stays a
/// tip. **Every `None` path is a tip, never an error**: a zap is irrevocable,
/// so the ratified rule is that anything short of a met threshold is recorded
/// and attributed rather than refused — refusing returns no sats and only
/// destroys the attribution the sender is owed.
///
/// Call this **after** dropping the SQLite connection: it takes `state.db`,
/// which is the same mutex the sync half holds.
///
/// The order of the whole path is deliberate — *resolve → apply → record*.
/// Applying before recording means a crash in between leaves no accounting
/// row, so the next sweep re-ingests the same receipt and re-applies it: the
/// waist is idempotent on `external_ref` (the zap's own event id), so the
/// replay converges instead of double-granting. The opposite order would
/// record a purchase that never happened and dedup the retry that would have
/// fixed it.
pub async fn apply_zap_purchase(
    state: &std::sync::Arc<crate::AppState>,
    zap: &TrustedZap,
    subject: &ZapSubject,
) -> Option<String> {
    // An amount-less receipt buys nothing: `bolt11` is optional in the wild,
    // and a threshold comparison needs a number. It is still a tip.
    let amount_msats = zap.receipt().amount_msats?;

    let payee = fauna_core::identity::ActorId::from_hex(&subject.payee_actor_id).ok()?;
    // The tier that genuinely SELLS this post — the post is gated to it *and*
    // it designates the post. Resolving on the designation alone would let a
    // tier sell a post it has nothing to do with: `unlocks_post` is a one-way
    // client claim, unverifiable at create time and not unique, so on its own
    // it would class a zap on an ordinary public post as a sale — and a sale is
    // excluded from the post's tip list, destroying exactly the attribution
    // § The asking price refuses to destroy. See
    // [`CacheDb::get_tier_selling_post`] for the full two-direction rule.
    let tier = state
        .db
        .get_tier_selling_post(&payee.0, &subject.fauna_post_id)
        .await
        .ok()
        .flatten()?;

    // The tier exists but names no machine price ⇒ tip. That is the ratified
    // permanent behavior, not a gap: inferred sale is opt-in, and no free-text
    // parsing of `price_hint` ever infers a number.
    // The row hands back the ungated WIRE price (an excised nest still stores and
    // re-serves one); the comparison type lives in the excised crate, so the
    // conversion happens here — the single site that judges whether money met a
    // price, which is the act the `payments` member gates.
    let wire = tier.asking_price()?;
    let asking = fauna_payments::asking_price::AskingPrice {
        value: wire.value,
        unit: wire.unit,
    };
    if !asking.is_met_by(amount_msats, fauna_payments::asking_price::UNIT_MSAT) {
        return None;
    }

    let entitlement = fauna_payments::PaymentEntitlement {
        provider: ZAP_PROVIDER.to_string(),
        payee,
        buyer: match subject
            .sender_actor_id
            .as_deref()
            .and_then(|hex| fauna_core::identity::ActorId::from_hex(hex).ok())
        {
            Some(actor) => fauna_payments::Buyer::Actor(actor),
            None => fauna_payments::Buyer::Unbound,
        },
        tier: tier.name.clone(),
        // Perpetual, per the degenerate-tier rule: "a purchase maps to
        // `valid_until = None`". A zap carries no window to express one with.
        valid_until_secs: None,
        // The zap's own event id — a stable, mechanism-supplied identifier, so
        // a redelivered receipt is the same payment to the waist.
        external_ref: zap.zap_event_id().to_string(),
        return_ref: None,
    };

    // **Gate surface `payments.unlock.purchase`**, the payments-plane half of a
    // zap that bought something (`dynamic-features.md` § Charter members). It is
    // a *second* spend, on a *different* member, and that is the ratified shape:
    // "quota bounds deliberately do not inherit — each member's operations count
    // against its own dimensions", so a zap purchase costs one zap ingest and
    // one payments purchase. The subset edge means a `payments` **deny** already
    // reached the ingest gate above, so nothing can arrive here under one.
    //
    // A refusal degrades to a **tip**, never to a lost receipt — the rule this
    // whole path is built on: the sats really arrived, so the attribution is
    // owed whatever the payments plane says about the entitlement.
    let purchase_op = match crate::payment_core::purchase_gate_op(state, &entitlement, amount_msats)
        .await
    {
        Ok(op) => op,
        Err(e) => {
            tracing::warn!(
                zap_event_id = %zap.zap_event_id(),
                "zap purchase gate could not resolve its counterparty delta ({e}) — recording as a tip"
            );
            return None;
        }
    };
    if let Err(e) = crate::feature_gate::gate(state, &entitlement.payee.0, &purchase_op).await {
        tracing::info!(
            zap_event_id = %zap.zap_event_id(),
            post_id = %subject.fauna_post_id,
            tier = %tier.name,
            code = %e.code,
            "zap met the asking price but the payments gate refused the purchase — recording as a tip"
        );
        return None;
    }

    match crate::payment_core::apply_payment(state, &entitlement).await {
        Ok(applied) => {
            tracing::info!(
                zap_event_id = %zap.zap_event_id(),
                post_id = %subject.fauna_post_id,
                tier = %tier.name,
                ?applied,
                "zap met a tier's asking price — purchase applied"
            );
            Some(tier.name)
        }
        Err(e) => {
            // Degrade to a tip rather than losing the receipt. The sats really
            // did arrive, so the attribution is owed whatever went wrong here,
            // and leaving no accounting row would strand them entirely. The
            // buyer's recourse is the author's ordinary sales audit — the same
            // one that covers a claim code nobody redeemed.
            tracing::warn!(
                zap_event_id = %zap.zap_event_id(),
                post_id = %subject.fauna_post_id,
                tier = %tier.name,
                "zap met the asking price but the grant failed ({e}) — recording as a tip"
            );
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::tip_notification_body;

    /// Every key the tip notification can name exists in the English catalog
    /// with exactly the placeholders it is given — a key the catalog lacks
    /// would render the bare key on every app.
    #[test]
    fn every_tip_body_names_a_catalog_sentence_with_its_args() {
        for sender in [Some("abcd1234"), None] {
            for amount in [Some(21_000), Some(500), None] {
                let body = tip_notification_body(sender, amount);
                let sentence = fauna_i18n::strings::lookup(&body.key)
                    .unwrap_or_else(|| panic!("no catalog entry for {}", body.key));
                for name in body.args.keys() {
                    assert!(
                        sentence.contains(&format!("{{{name}}}")),
                        "{} lacks {{{name}}}",
                        body.key
                    );
                }
                assert_eq!(
                    sentence.matches('{').count(),
                    body.args.len(),
                    "{}",
                    body.key
                );
            }
        }
    }

    #[test]
    fn the_unit_split_is_tip_amounts() {
        let sats = tip_notification_body(Some("abcd1234"), Some(21_000));
        assert_eq!(sats.key, "notifications.row_tip_sats");
        assert_eq!(sats.args.get("value").map(String::as_str), Some("21"));
        assert_eq!(
            sats.args.get("sender").map(String::as_str),
            Some("abcd1234")
        );
        // A sub-sat tip keeps its msats rather than reading "0 sats".
        let msats = tip_notification_body(None, Some(500));
        assert_eq!(msats.key, "notifications.row_tip_anonymous_msats");
        assert_eq!(msats.args.get("value").map(String::as_str), Some("500"));
        // An amount-less receipt is still a tip, with no figure invented.
        let bare = tip_notification_body(Some("abcd1234"), None);
        assert_eq!(bare.key, "notifications.row_tip");
        assert!(!bare.args.contains_key("value"));
    }
}
