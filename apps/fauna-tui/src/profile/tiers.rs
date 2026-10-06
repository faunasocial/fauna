//! The profile page's SELF Tiers-tab **author management** — §§1–5 of
//! `docs/goal/ui/profile.md` § Tiers tab (`monetization.md` § Pillar 1 for
//! §§1–3, § Pillar 3 for §§4–5).
//!
//! The OTHER half of the Tiers tab (the offers browse) lives in the page shell
//! ([`super`]); this module is the `is_self` branch: §1 My tiers, §2 Pending
//! requests, §3 Subscribers roster, §4 Payment providers, §5 Manual claim codes.
//! The reference implementation is `apps/fauna-linux/src/views/profile/tiers.rs`
//! — same ui.yaml ids, same shared-crate calls, same section order (priority #1).
//!
//! **All logic is shared Rust** (priority #2): every mutation is a
//! `fauna_client_subscriptions::{SubscriptionsClient, orchestration::
//! SubscriptionsAuthor}` or `fauna_client_payments::PaymentsClient` call, and
//! every derived string comes from `fauna_core::format` (`provider_status_label`,
//! `claim_status_label`). This module owns no business rule — an approve's
//! mint+upload, a removal's roster rotation, and the webhook-URL shape are all
//! decided in the shared crates.
//!
//! **Row addressing.** Every row leaf is registered
//! `.within("<row-id>", i)`, which keeps it addressable *both* flat
//! (`get_text(id, index=i)` — the registry keeps registration order) *and*
//! scoped (`scope="subscription-request-row[0]"`, which
//! `actions/subscriptions.py::request_paid` uses). A top-level element has an
//! empty scope path and so can never match a scoped query, which is exactly why
//! the conditional `subscription-request-paid-badge` has to hang off its row.

use fauna_ui_ids as ids;
use std::sync::Arc;

use fauna_client::NestClient;
#[cfg(feature = "payments")]
use fauna_client_payments::PaymentsClient;
#[cfg(feature = "payments")]
use fauna_client_payments::payments::{ClaimItem, ProviderItem};
use fauna_client_subscriptions::SubscriptionsClient;
use fauna_client_subscriptions::orchestration::SubscriptionsAuthor;
#[cfg(feature = "payments")]
use fauna_core::format;
use fauna_core::identity::ActorKeypair;
use fauna_core::secret::SecretArray32;
use fauna_i18n::strings::subscriptions as s;
use fauna_protocol::subscriptions::{PendingRequest, SubscriberEntry, TierItem};

use crate::element::{Element, Field, Gesture, SelectTarget};

use super::{Action, ProfileField, ProfileState};

/// The §1 create/edit form buffers. Open ⇒ `Some` on [`AuthorState::form`].
#[derive(Default, Clone)]
pub struct TierForm {
    /// `None` = create; `Some(name)` = editing that tier. The name is the
    /// server-side key, so an edit carries the original through
    /// `tiers_update` even if the user retypes the name field.
    pub editing: Option<String>,
    pub name: String,
    pub rank: String,
    pub description: String,
    pub price_hint: String,
    /// The machine-comparable sats price (`monetization.md` § The asking
    /// price) — independent of `price_hint`, never parsed from it. Empty
    /// means unpriced; on an edit, empty is sent as "keep current", never
    /// "clear" (`fauna.subscriptions.tiers.update`'s merge rule).
    pub asking_price: String,
    pub payment_url: String,
    pub auto_approve: bool,
}

/// The §4 provider add/edit form buffers.
///
/// `payments`-gated with the section it buffers (`dynamic-features.md`
/// § Platform-family surface excision): a store-safe build paints no §4, so a
/// form for it would be state nothing can open.
#[cfg(feature = "payments")]
#[derive(Default, Clone)]
pub struct ProviderForm {
    /// One of `fauna_client_payments::known_kinds()`; empty until picked.
    pub kind: String,
    pub secret: String,
    /// The tier this provider's payments entitle.
    pub tier: String,
}

/// The SELF Tiers-tab author-management state, hung off [`ProfileState`].
///
/// Observer-free like the rest of the page: each section holds the last read,
/// and a mutation re-reads (`profile.md` § Persistence).
#[derive(Default)]
pub struct AuthorState {
    /// §1 — the author's own tiers. Per-post pay-to-unlock tiers are filtered
    /// out at read time (`TierItem::unlocks_post` — `monetization.md`: the
    /// unlock affordance renders on the post, not in the management list).
    pub tiers: Vec<TierItem>,
    /// §2 — pending subscribe/unsubscribe requests.
    pub requests: Vec<PendingRequest>,
    /// §3 — the confirmed roster of [`Self::roster_tier`].
    pub subscribers: Vec<SubscriberEntry>,
    /// §3 — which tier's roster is shown (`subscription-subscribers-tier-select`).
    pub roster_tier: String,
    /// §4 — the configured payment providers.
    #[cfg(feature = "payments")]
    pub providers: Vec<ProviderItem>,
    /// §5 — minted claim codes (manual + webhook), newest first.
    #[cfg(feature = "payments")]
    pub claims: Vec<ClaimItem>,
    /// §5 — which tier a freshly minted code entitles
    /// (`subscription-claim-tier-select`).
    #[cfg(feature = "payments")]
    pub claim_tier: String,
    /// §1 create/edit form, when open.
    pub form: Option<TierForm>,
    /// §4 add/edit form, when open.
    #[cfg(feature = "payments")]
    pub provider_form: Option<ProviderForm>,
    /// `subscription-request-busy` — an approve's mint+upload is in flight.
    pub busy: bool,
}

impl AuthorState {
    /// The tier names offered by every tier picker on the tab (§3 roster, §4
    /// tier-map, §5 claim mint). One derivation so the three selects can never
    /// disagree about what the author owns — and deliberately over the FULL
    /// list, designated per-post unlock tiers included: only §1 hides those
    /// ([`manageable_tiers`]).
    pub fn tier_names(&self) -> Vec<String> {
        self.tiers.iter().map(|t| t.name.clone()).collect()
    }

    /// Drop everything a re-open must not inherit from the previous actor.
    pub fn reset(&mut self) {
        *self = AuthorState::default();
    }
}

// ── The five reads ───────────────────────────────────────────────────────────

/// Everything the SELF Tiers tab paints, read in one op.
///
/// A failed §4/§5 read degrades to empty rather than failing the whole tab: the
/// Pillar-3 kinds are optional surfaces (a nest built without payments may not route
/// `fauna.payments.*` at all), and losing them must not blank §§1–3.
#[derive(Debug)]
pub struct AuthorSnapshot {
    pub tiers: Vec<TierItem>,
    pub requests: Vec<PendingRequest>,
    pub subscribers: Vec<SubscriberEntry>,
    pub roster_tier: String,
    #[cfg(feature = "payments")]
    pub providers: Vec<ProviderItem>,
    #[cfg(feature = "payments")]
    pub claims: Vec<ClaimItem>,
}

/// Read §§1–5. `preferred_roster` keeps the §3 select's choice across a re-read;
/// it falls back to the first tier so the roster is never stuck on a tier the
/// author just deleted.
pub async fn fetch_author(
    nest: &Arc<NestClient>,
    preferred_roster: &str,
) -> Result<AuthorSnapshot, String> {
    let subs = SubscriptionsClient::new(Arc::clone(nest));
    // ⚠ The author's FULL tier list, per-post unlock tiers included — do NOT
    // filter here. The §1 management list excludes designated tiers at its own
    // RENDER ([`manageable_tiers`]), which is the shape every sibling app uses
    // (linux `render_tier_rows`, web/android `manageableTiers`, apple
    // `SubscriptionsVM.myTiers`); §§3–5 deliberately keep them, because a
    // designated tier still needs a claim minted (§5), a payment provider
    // mapped (§4), and — above all — its BUYERS viewed (§3). Filtering at the
    // read starved all three: a seller whose only tier was the post they sold
    // got an empty roster select, so `subscribers_list` was never called and §3
    // showed zero buyers however many the nest had granted, with no error
    // surfaced (`monetization.md` § Per-post pay-to-unlock, gap (2b)).
    let tiers: Vec<TierItem> = subs
        .tiers_list()
        .await
        .map_err(|e| format!("load tiers: {e}"))?;
    // §2
    let requests = subs
        .requests_list()
        .await
        .map_err(|e| format!("load requests: {e}"))?;
    // §3 — keep the current selection if it still exists, else the first tier.
    let roster_tier = pick_tier(&tiers, preferred_roster);
    let subscribers = if roster_tier.is_empty() {
        Vec::new()
    } else {
        subs.subscribers_list(roster_tier.clone())
            .await
            .map_err(|e| format!("load subscribers: {e}"))?
    };
    // §§4–5 — optional surfaces, so a transport error degrades to empty. Not
    // merely unpainted in a store-safe build but never REQUESTED: criterion 5
    // (no re-enable path) is about the wire too, and a read is a sender.
    #[cfg(feature = "payments")]
    let (providers, claims) = {
        let payments = PaymentsClient::new(Arc::clone(nest));
        (
            payments.providers_list().await.unwrap_or_default(),
            payments.claims_list().await.unwrap_or_default(),
        )
    };
    Ok(AuthorSnapshot {
        tiers,
        requests,
        subscribers,
        roster_tier,
        #[cfg(feature = "payments")]
        providers,
        #[cfg(feature = "payments")]
        claims,
    })
}

/// §1's management list: everything the author can actually *manage*. A
/// per-post pay-to-unlock designated tier is auto-minted by the sale and is
/// sold on the post, so it never appears here (`monetization.md` § Per-post
/// pay-to-unlock) — the one place on this tab that filters. Row actions key on
/// `tier.name`, so the filtered enumeration index is only a row scope.
pub fn manageable_tiers(tiers: &[TierItem]) -> Vec<&TierItem> {
    tiers.iter().filter(|t| t.unlocks_post.is_none()).collect()
}

/// Re-point a tier picker after a fresh read: keep the author's current pick if
/// it still exists, else fall back to the first tier, so a select is never left
/// armed against a tier that was just deleted. Empty only when the author owns
/// no tiers at all. Shared by §3's roster and §5's claim-mint pick — one rule,
/// so the two selects cannot drift apart.
pub fn pick_tier(tiers: &[TierItem], preferred: &str) -> String {
    tiers
        .iter()
        .any(|t| t.name == preferred)
        .then(|| preferred.to_string())
        .or_else(|| tiers.first().map(|t| t.name.clone()))
        .unwrap_or_default()
}

/// Re-read just §3 for `tier` — the `subscription-subscribers-tier-select`
/// change path (the other four sections are unaffected by the pick).
pub async fn fetch_roster(
    nest: &Arc<NestClient>,
    tier: &str,
) -> Result<Vec<SubscriberEntry>, String> {
    if tier.is_empty() {
        return Ok(Vec::new());
    }
    SubscriptionsClient::new(Arc::clone(nest))
        .subscribers_list(tier.to_string())
        .await
        .map_err(|e| format!("load subscribers: {e}"))
}

/// Build the author orchestrator — the mint+upload half of §1 create / §2
/// approve / §3 remove. Its custody writes need the actor keypair, so it is
/// rebuilt per op from the page's stored secret (linux's shape).
pub fn author(
    nest: Arc<NestClient>,
    secret: &SecretArray32,
    period_keys: fauna_client_subscriptions::SharedPeriodKeyStore,
) -> SubscriptionsAuthor<Arc<NestClient>> {
    let keypair = ActorKeypair::from_secret(secret.to_array());
    SubscriptionsAuthor::over(nest, keypair, period_keys)
}

// ── Field access (the two forms) ─────────────────────────────────────────────

/// Read one form buffer. Returns `""` when the owning form is closed — a read
/// of a hidden field is never an error (the element isn't painted anyway).
pub fn field(st: &ProfileState, field: &TierField) -> String {
    match field {
        TierField::FormName => form(st).map(|f| f.name.clone()),
        TierField::FormRank => form(st).map(|f| f.rank.clone()),
        TierField::FormDescription => form(st).map(|f| f.description.clone()),
        TierField::FormPriceHint => form(st).map(|f| f.price_hint.clone()),
        TierField::FormAskingPrice => form(st).map(|f| f.asking_price.clone()),
        TierField::FormPaymentUrl => form(st).map(|f| f.payment_url.clone()),
        #[cfg(feature = "payments")]
        TierField::ProviderFormSecret => provider_form(st).map(|f| f.secret.clone()),
    }
    .unwrap_or_default()
}

/// Write one form buffer. A write to a closed form is dropped (same reasoning
/// as [`field`]).
pub fn set_field(st: &mut ProfileState, field: TierField, value: String) {
    match field {
        TierField::FormName => {
            if let Some(f) = st.author.form.as_mut() {
                f.name = value;
            }
        }
        TierField::FormRank => {
            if let Some(f) = st.author.form.as_mut() {
                f.rank = value;
            }
        }
        TierField::FormDescription => {
            if let Some(f) = st.author.form.as_mut() {
                f.description = value;
            }
        }
        TierField::FormPriceHint => {
            if let Some(f) = st.author.form.as_mut() {
                f.price_hint = value;
            }
        }
        TierField::FormAskingPrice => {
            if let Some(f) = st.author.form.as_mut() {
                f.asking_price = value;
            }
        }
        TierField::FormPaymentUrl => {
            if let Some(f) = st.author.form.as_mut() {
                f.payment_url = value;
            }
        }
        #[cfg(feature = "payments")]
        TierField::ProviderFormSecret => {
            if let Some(f) = st.author.provider_form.as_mut() {
                f.secret = value;
            }
        }
    }
}

fn form(st: &ProfileState) -> Option<&TierForm> {
    st.author.form.as_ref()
}

#[cfg(feature = "payments")]
fn provider_form(st: &ProfileState) -> Option<&ProviderForm> {
    st.author.provider_form.as_ref()
}

/// A Tiers-tab editable field. Carried inside [`ProfileField::Tier`] so the page
/// keeps one `Field::Profile` arm (the page shell owns the dispatch).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TierField {
    /// `subscription-tier-form-name`
    FormName,
    /// `subscription-tier-form-rank`
    FormRank,
    /// `subscription-tier-form-description`
    FormDescription,
    /// `subscription-tier-form-price-hint`
    FormPriceHint,
    /// `subscription-tier-form-asking-price`
    FormAskingPrice,
    /// `subscription-tier-form-payment-url`
    FormPaymentUrl,
    /// `subscription-provider-form-secret`
    #[cfg(feature = "payments")]
    ProviderFormSecret,
}

// ── Derived strings ──────────────────────────────────────────────────────────

/// The §4 form's live webhook-URL preview — the exact URL the author registers
/// at their provider's dashboard. The shape is owned by
/// `fauna_payments::webhook_url` (the nest builds its ingress route from the
/// same function), so this never restates it.
#[cfg(feature = "payments")]
pub fn webhook_url(st: &ProfileState) -> String {
    let kind = provider_form(st).map(|f| f.kind.as_str()).unwrap_or("");
    fauna_client_payments::webhook_url(st.nest_url(), st.self_actor_id_hex(), kind)
}

/// The evidence-based §4 status badge (`provider_status_label` — no ping, the
/// nest's verify/reject stamps decide).
#[cfg(feature = "payments")]
fn provider_status(p: &ProviderItem) -> String {
    crate::wizard::localized(&format::provider_status_label(
        p.last_verified_at,
        p.last_rejected_at,
    ))
}

/// The §5 three-state code badge (`claim_status_label`).
#[cfg(feature = "payments")]
fn claim_status(c: &ClaimItem) -> String {
    crate::wizard::localized(&format::claim_status_label(
        c.redeemed_by.is_some(),
        c.voided_at.is_some(),
    ))
}

// ── Elements ─────────────────────────────────────────────────────────────────

/// Paint §§1–5 in ui.yaml's declared order. Called by the page shell for the
/// SELF Tiers tab only.
pub fn elements(st: &ProfileState) -> Vec<Element> {
    let a = &st.author;
    let mut out = Vec::new();
    let tier_names = a.tier_names();

    // ── §1 My tiers ──────────────────────────────────────────────────────
    out.push(Element::label(ids::SUBSCRIPTION_TIERS_SECTION, " "));
    out.push(Element::gesture_button(
        ids::SUBSCRIPTION_TIER_CREATE_BUTTON,
        s::CREATE_TIER,
        true,
        Gesture::Profile(Action::OpenTierForm(None)),
    ));
    out.push(Element::label(ids::SUBSCRIPTION_TIER_LIST, " "));
    for (i, tier) in manageable_tiers(&a.tiers).into_iter().enumerate() {
        out.push(
            Element::label(ids::SUBSCRIPTION_TIER_ROW, " ").within(ids::SUBSCRIPTION_TIER_ROW, i),
        );
        out.push(
            Element::label(ids::SUBSCRIPTION_TIER_NAME, tier.name.clone())
                .within(ids::SUBSCRIPTION_TIER_ROW, i),
        );
        out.push(
            Element::label(ids::SUBSCRIPTION_TIER_RANK, tier.rank.to_string())
                .within(ids::SUBSCRIPTION_TIER_ROW, i),
        );
        // Always emitted (empty allowed) so the flat per-id index stays aligned
        // with the row index — the offers-browse convention.
        out.push(
            Element::label(
                ids::SUBSCRIPTION_TIER_PRICE,
                tier.price_hint.clone().unwrap_or_default(),
            )
            .within(ids::SUBSCRIPTION_TIER_ROW, i),
        );
        out.push(
            Element::gesture_button(
                ids::SUBSCRIPTION_TIER_EDIT_BUTTON,
                s::EDIT,
                true,
                Gesture::Profile(Action::OpenTierForm(Some(tier.name.clone()))),
            )
            .within(ids::SUBSCRIPTION_TIER_ROW, i),
        );
        out.push(
            Element::gesture_button(
                ids::SUBSCRIPTION_TIER_DELETE_BUTTON,
                s::DELETE,
                true,
                Gesture::Profile(Action::DeleteTier(tier.name.clone())),
            )
            .within(ids::SUBSCRIPTION_TIER_ROW, i),
        );
    }

    // §1 create/edit form.
    if let Some(f) = &a.form {
        out.push(Element::label(ids::SUBSCRIPTION_TIER_FORM, " "));
        out.push(
            Element::input(
                ids::SUBSCRIPTION_TIER_FORM_NAME,
                f.name.clone(),
                Field::Profile(ProfileField::Tier(TierField::FormName)),
            )
            .labelled(s::TIER_NAME),
        );
        out.push(
            Element::input(
                ids::SUBSCRIPTION_TIER_FORM_RANK,
                f.rank.clone(),
                Field::Profile(ProfileField::Tier(TierField::FormRank)),
            )
            .labelled(s::RANK),
        );
        out.push(
            Element::input(
                ids::SUBSCRIPTION_TIER_FORM_DESCRIPTION,
                f.description.clone(),
                Field::Profile(ProfileField::Tier(TierField::FormDescription)),
            )
            .labelled(s::DESCRIPTION),
        );
        out.push(
            Element::input(
                ids::SUBSCRIPTION_TIER_FORM_PRICE_HINT,
                f.price_hint.clone(),
                Field::Profile(ProfileField::Tier(TierField::FormPriceHint)),
            )
            .labelled(s::PRICE_HINT),
        );
        out.push(
            Element::input(
                ids::SUBSCRIPTION_TIER_FORM_ASKING_PRICE,
                f.asking_price.clone(),
                Field::Profile(ProfileField::Tier(TierField::FormAskingPrice)),
            )
            .labelled(s::ASKING_PRICE),
        );
        out.push(
            Element::input(
                ids::SUBSCRIPTION_TIER_FORM_PAYMENT_URL,
                f.payment_url.clone(),
                Field::Profile(ProfileField::Tier(TierField::FormPaymentUrl)),
            )
            .labelled(s::PAYMENT_URL),
        );
        out.push(Element::checkbox_gesture(
            ids::SUBSCRIPTION_TIER_FORM_AUTO_APPROVE,
            s::AUTO_APPROVE,
            f.auto_approve,
            Gesture::Profile(Action::ToggleAutoApprove),
        ));
        out.push(Element::gesture_button(
            ids::SUBSCRIPTION_TIER_FORM_SAVE,
            s::SAVE,
            true,
            Gesture::Profile(Action::SaveTierForm),
        ));
        out.push(Element::gesture_button(
            ids::SUBSCRIPTION_TIER_FORM_CANCEL,
            s::CANCEL,
            true,
            Gesture::Profile(Action::CancelTierForm),
        ));
    }

    // ── §2 Pending requests ──────────────────────────────────────────────
    out.push(Element::label(ids::SUBSCRIPTION_REQUESTS_SECTION, " "));
    // The transient "Approving — minting keys…" state. Painted only while the
    // mint+upload is in flight, so its presence *is* the assertion.
    if a.busy {
        out.push(Element::label(ids::SUBSCRIPTION_REQUEST_BUSY, s::APPROVING));
    }
    out.push(Element::label(ids::SUBSCRIPTION_REQUEST_LIST, " "));
    for (i, req) in a.requests.iter().enumerate() {
        out.push(
            Element::label(ids::SUBSCRIPTION_REQUEST_ROW, " ")
                .within(ids::SUBSCRIPTION_REQUEST_ROW, i),
        );
        out.push(
            Element::label(
                ids::SUBSCRIPTION_REQUEST_SUBSCRIBER,
                req.subscriber_id.to_hex(),
            )
            .within(ids::SUBSCRIPTION_REQUEST_ROW, i),
        );
        out.push(
            Element::label(ids::SUBSCRIPTION_REQUEST_TIER, req.tier_name.clone())
                .within(ids::SUBSCRIPTION_REQUEST_ROW, i),
        );
        out.push(
            Element::label(ids::SUBSCRIPTION_REQUEST_KIND, req.kind.clone())
                .within(ids::SUBSCRIPTION_REQUEST_ROW, i),
        );
        // Conditional — read only through the row scope
        // (`actions/subscriptions.py::request_paid`), never a flat index, so
        // omitting it on an unpaid row cannot misalign anything.
        if req.payment_entitled {
            out.push(
                Element::label(ids::SUBSCRIPTION_REQUEST_PAID_BADGE, s::PAID)
                    .within(ids::SUBSCRIPTION_REQUEST_ROW, i),
            );
        }
        out.push(
            Element::gesture_button(
                ids::SUBSCRIPTION_REQUEST_APPROVE_BUTTON,
                s::APPROVE,
                true,
                Gesture::Profile(Action::ApproveRequest(i)),
            )
            .within(ids::SUBSCRIPTION_REQUEST_ROW, i),
        );
        out.push(
            Element::gesture_button(
                ids::SUBSCRIPTION_REQUEST_REJECT_BUTTON,
                s::REJECT,
                true,
                Gesture::Profile(Action::RejectRequest(req.request_id)),
            )
            .within(ids::SUBSCRIPTION_REQUEST_ROW, i),
        );
    }

    // ── §3 Subscribers roster ────────────────────────────────────────────
    out.push(Element::label(ids::SUBSCRIPTION_SUBSCRIBERS_SECTION, " "));
    out.push(
        Element::select(
            ids::SUBSCRIPTION_SUBSCRIBERS_TIER_SELECT,
            a.roster_tier.clone(),
            SelectTarget::SubscriberRosterTier,
            tier_names.clone(),
        )
        .labelled(s::TIER_SELECT_LABEL),
    );
    out.push(Element::label(ids::SUBSCRIPTION_SUBSCRIBER_LIST, " "));
    for (i, sub) in a.subscribers.iter().enumerate() {
        out.push(
            Element::label(ids::SUBSCRIPTION_SUBSCRIBER_ROW, " ")
                .within(ids::SUBSCRIPTION_SUBSCRIBER_ROW, i),
        );
        out.push(
            Element::label(
                ids::SUBSCRIPTION_SUBSCRIBER_HANDLE,
                sub.subscriber_id.to_hex(),
            )
            .within(ids::SUBSCRIPTION_SUBSCRIBER_ROW, i),
        );
        out.push(
            Element::gesture_button(
                ids::SUBSCRIPTION_SUBSCRIBER_REMOVE_BUTTON,
                s::REMOVE,
                true,
                Gesture::Profile(Action::RemoveSubscriber(i)),
            )
            .within(ids::SUBSCRIPTION_SUBSCRIBER_ROW, i),
        );
    }

    // ── §§4–5, the money plane — `payments`-gated ────────────────────────
    //
    // Criterion 1 of `dynamic-features.md` § What "completely compiled away"
    // means is a `strings`-grep for ELEMENT IDS, so the gate has to sit on the
    // RENDER, not only on the reads above: a section that paints nothing
    // because its data is empty still ships every id it would have painted.
    #[cfg(feature = "payments")]
    {
        // ── §4 Payment providers ─────────────────────────────────────────────
        out.push(Element::label(ids::SUBSCRIPTION_PROVIDER_SECTION, " "));
        out.push(Element::gesture_button(
            ids::SUBSCRIPTION_PROVIDER_ADD_BUTTON,
            s::ADD_PROVIDER,
            true,
            Gesture::Profile(Action::OpenProviderForm),
        ));
        out.push(Element::label(ids::SUBSCRIPTION_PROVIDER_LIST, " "));
        for (i, p) in a.providers.iter().enumerate() {
            out.push(
                Element::label(ids::SUBSCRIPTION_PROVIDER_ROW, " ")
                    .within(ids::SUBSCRIPTION_PROVIDER_ROW, i),
            );
            out.push(
                Element::label(ids::SUBSCRIPTION_PROVIDER_KIND, p.kind.clone())
                    .within(ids::SUBSCRIPTION_PROVIDER_ROW, i),
            );
            out.push(
                Element::label(ids::SUBSCRIPTION_PROVIDER_STATUS, provider_status(p))
                    .within(ids::SUBSCRIPTION_PROVIDER_ROW, i),
            );
            out.push(
                Element::gesture_button(
                    ids::SUBSCRIPTION_PROVIDER_REMOVE_BUTTON,
                    s::REMOVE,
                    true,
                    Gesture::Profile(Action::RemoveProvider(p.kind.clone())),
                )
                .within(ids::SUBSCRIPTION_PROVIDER_ROW, i),
            );
        }

        // §4 add/edit form.
        if let Some(f) = &a.provider_form {
            out.push(Element::label(ids::SUBSCRIPTION_PROVIDER_FORM, " "));
            out.push(
                Element::select(
                    ids::SUBSCRIPTION_PROVIDER_FORM_KIND,
                    f.kind.clone(),
                    SelectTarget::ProviderFormKind,
                    fauna_client_payments::known_kinds()
                        .iter()
                        .map(|k| k.to_string())
                        .collect(),
                )
                .labelled(s::PROVIDER_KIND_LABEL),
            );
            out.push(
                Element::input(
                    ids::SUBSCRIPTION_PROVIDER_FORM_SECRET,
                    f.secret.clone(),
                    Field::Profile(ProfileField::Tier(TierField::ProviderFormSecret)),
                )
                .labelled(s::WEBHOOK_SECRET),
            );
            out.push(
                Element::select(
                    ids::SUBSCRIPTION_PROVIDER_FORM_TIER_MAP,
                    f.tier.clone(),
                    SelectTarget::ProviderFormTier,
                    tier_names.clone(),
                )
                .labelled(s::PROVIDER_TIER_LABEL),
            );
            // Recomputes with the kind select — it is derived from the form, not
            // stored, so there is no stale-preview state to keep in sync.
            out.push(
                Element::label(ids::SUBSCRIPTION_PROVIDER_FORM_WEBHOOK_URL, webhook_url(st))
                    .labelled(s::WEBHOOK_URL_LABEL),
            );
            out.push(Element::gesture_button(
                ids::SUBSCRIPTION_PROVIDER_FORM_WEBHOOK_URL_COPY_BUTTON,
                s::CLAIM_CODE,
                true,
                Gesture::Profile(Action::CopyWebhookUrl),
            ));
            out.push(Element::gesture_button(
                ids::SUBSCRIPTION_PROVIDER_FORM_SAVE,
                s::SAVE,
                true,
                Gesture::Profile(Action::SaveProviderForm),
            ));
            out.push(Element::gesture_button(
                ids::SUBSCRIPTION_PROVIDER_FORM_CANCEL,
                s::CANCEL,
                true,
                Gesture::Profile(Action::CancelProviderForm),
            ));
        }

        // ── §5 Manual claim codes ────────────────────────────────────────────
        out.push(Element::label(ids::SUBSCRIPTION_CLAIM_SECTION, " "));
        out.push(
            Element::select(
                ids::SUBSCRIPTION_CLAIM_TIER_SELECT,
                a.claim_tier.clone(),
                SelectTarget::ClaimTier,
                tier_names,
            )
            .labelled(s::TIER_SELECT_LABEL),
        );
        out.push(Element::gesture_button(
            ids::SUBSCRIPTION_CLAIM_MINT_BUTTON,
            s::MINT_CLAIM,
            true,
            Gesture::Profile(Action::MintClaim),
        ));
        out.push(Element::label(ids::SUBSCRIPTION_CLAIM_LIST, " "));
        for (i, c) in a.claims.iter().enumerate() {
            out.push(
                Element::label(ids::SUBSCRIPTION_CLAIM_ROW, " ")
                    .within(ids::SUBSCRIPTION_CLAIM_ROW, i),
            );
            out.push(
                Element::label(ids::SUBSCRIPTION_CLAIM_CODE, c.code.clone())
                    .within(ids::SUBSCRIPTION_CLAIM_ROW, i),
            );
            out.push(
                Element::label(ids::SUBSCRIPTION_CLAIM_TIER, c.tier.clone())
                    .within(ids::SUBSCRIPTION_CLAIM_ROW, i),
            );
            out.push(
                Element::label(ids::SUBSCRIPTION_CLAIM_STATUS, claim_status(c))
                    .within(ids::SUBSCRIPTION_CLAIM_ROW, i),
            );
        }
    }

    out
}
