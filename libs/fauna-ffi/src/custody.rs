//! UniFFI façade for the **T16 custody facet** — the boundary the android, macOS,
//! iOS and windows legs render their `custody-holder-*` family from.
//!
//! Nothing is projected here. The boundary rows live ONE level down, in
//! `fauna_client_capabilities::custody_view`, carrying `Serialize`/`Deserialize`
//! always and their `uniffi::Record` derives behind that crate's off-by-default
//! `uniffi` feature — which this crate turns on. The web SPA's wasm build
//! leaves it off and reads the same rows through serde, so the four native apps
//! and web cannot drift on which receipt state reads which way or on whether a
//! nest-anchored custody is skipped. Two projections of one fold is exactly the
//! divergence priority #2 forbids, and it is the mistake this module was
//! written to make and then corrected.
//!
//! So what remains here is only the exports: load, drive, the owner's revoke,
//! the host's five held-side gestures (accept, decline, set-budget, stop,
//! remove), and the mint pair (candidates + send). The semantics behind them
//! are shared too — `fauna-client-custody` owns the act assembly
//! (`run_custody_act`, `load_custody_facet`, `mint_candidates`, `spawn_drive`),
//! including the two orderings a per-app re-derivation gets wrong (the accept
//! binds the T10 writer key; revoke hits the nest *before* recording).
//!
//! Every export threads `crate::account_runtime::handle()` (the W3
//! (account-data-plane.md § Workstreams) account store hosted for
//! windows/macOS/iOS) through rather than hardcoding `store: None`, mirroring
//! `offline_share.rs`/`nest_client.rs`. `Ok(None)`/a dropped store is still a
//! **correct** fold for the owner side, not a degraded one — a held row with no
//! readable registry row keeps its accept-seeded budget — and the store-writing
//! gestures (`custody_set_budget`/`custody_stop`/`custody_remove`) answer with
//! an error rather than pretending when it is absent.
//!
//! ## Why the held-side gestures cross together
//!
//! `devices.md` § Custody facet defines piece 3's host-side card *as* the
//! metered budget + stop + remove controls beside the consent card's accept and
//! decline, and forbids a card with dead controls. Until the whole set crossed,
//! this face exported none of them (and piece 3 rendered on tui alone); they
//! land as one surface so a leg can paint the card with every control live. The
//! keyless-posture marker (piece 1) reads through the Devices face instead —
//! `devices_keyless_posture` in `crate::devices`.
//!
//! ## The mint, and why it is here after all
//!
//! Offer initiation (`custody-mint-*`) was absent from the first cut of this
//! face with the reason *"a mint recorded with no session is never posted"* —
//! true about the act, but never a property of the boundary: `custody_drive`
//! below has taken an `Arc<ConversationsSession>` across it since that same
//! cut, exactly as `folders_author` does. The session was never the blocker;
//! it simply was not threaded into the act's `CustodyCtx`. It is now, and
//! required rather than optional, so the recorded-but-unpostable mint is
//! unrepresentable here (and refused in `run_custody_act` for the native apps
//! that build a `CustodyCtx` by hand).
//!
//! The mint stays **native-only** for a different and still-true reason
//! (`devices.md` § Where logic lives): it posts over an MLS conversation
//! channel, and the web SPA has no session to post on. So this pair has no
//! wasm twin — the one place the two faces deliberately differ.
//!
//! Gated behind its own default-on `custody` feature, dropped from the Go
//! mail-bridge `--no-default-features` build (same shape as `muted-keywords` /
//! `sync-prefs` / `member-review`): the bridge has no settings UI, and gating
//! keeps the checked-in Go binding byte-identical. The gate also carries
//! `dep:fauna-client-custody`, which is what turns on
//! `fauna-sync-engine/account-runtime` — default-off there precisely so the lean
//! deployments do not pay for it.

use std::sync::Arc;

use fauna_client_capabilities::custody_view::{
    CustodyFacetView, CustodyMintCandidateView, CustodyOfferRowView,
};
use fauna_client_capabilities::view_model as vm;
use fauna_client_custody::{CustodyAct, CustodyCtx, load_custody_facet, run_custody_act};
use fauna_conversations::ConversationsSession;

use crate::crypto::secret32;
use crate::nest_client::FfiNestClient;
use crate::{FfiError, general_err};

/// The outcome of a custody gesture: the re-folded facet, and the error string
/// if the act failed.
///
/// Both halves cross because a custody gesture must **never be silently
/// dropped** (e2e convention 11) — the leg puts `error` on the page's
/// `error-message` element and re-renders from `facet`. `facet` is `None` only
/// when the config was unreadable on the refold pass, in which case the leg
/// keeps its previous rows rather than painting an empty list over live ones: an
/// unreadable config is a transient, not "the user has no custodians".
///
/// This is the one record defined at this boundary rather than shared, because
/// it describes the *call*, not the facet — the wasm face returns the same two
/// halves through its own idiom (a thrown JS error beside the snapshot).
#[derive(uniffi::Record, Clone, Debug, PartialEq, Eq)]
pub struct FfiCustodyActOutcome {
    pub facet: Option<CustodyFacetView>,
    pub error: Option<String>,
}

/// The i18n key for the degraded marker that rides a receipt whose `degraded`
/// is set. Exported as a function rather than duplicated per app so the legs
/// agree on the key instead of each hardcoding it.
#[uniffi::export]
pub fn custody_degraded_badge_key() -> String {
    vm::CUSTODY_DEGRADED_BADGE_KEY.to_string()
}

/// Load and fold the custody facet — the account-store read (the ceremony records
/// and the grant-event log) plus the shared three-family fold, projected
/// through the shared boundary rows.
///
/// Reads the account store through `crate::account_runtime::handle()`
/// (windows/macOS/iOS; `None` elsewhere or before the assembly lands), so the
/// R14 (account-data-plane.md § The ratified decisions) registry overlay
/// applies whenever a handle is live; see the module docs for what a missing
/// handle still bounds. `Ok(None)` = the config was unreadable this pass,
/// which is a transient: keep the previous facet rather than painting an
/// empty one.
#[fauna_uniffi_async::export]
pub async fn custody_facet_load(
    nest: Arc<FfiNestClient>,
    owner_secret: Vec<u8>,
) -> Result<Option<CustodyFacetView>, FfiError> {
    // The fold reads the account store alone now (the ceremony records are
    // `fauna.state.custody-ceremony` rows); the secret is still checked so a
    // face handing a malformed one hears it, as at every custody export.
    secret32(&owner_secret)?;
    Ok(
        load_custody_facet(nest.nest_arc(), crate::account_runtime::handle())
            .await
            .as_ref()
            .map(CustodyFacetView::from_snapshot),
    )
}

/// Who holds this account's generation-key escrow, as lowercase-hex 32-byte
/// identities — the Nests page's escrow-holder role badge
/// (`participant-escrow-holder-badge`) renders on the `nests-item` row whose
/// `nest_id` is in this list (`participants.md` § The participant model →
/// Roles). The shared `AccountStoreHandle::escrow_holders` derivation tui and
/// linux read natively: recorded `fauna.state.escrow-receipt` rows, never a
/// nest assertion. A local store read. `None` = no account runtime yet or an
/// unreadable pass — keep the previous set rather than blanking the badge.
#[fauna_uniffi_async::export]
pub async fn custody_escrow_holders() -> Option<Vec<String>> {
    let ids = crate::account_runtime::handle()?
        .escrow_holders()
        .await
        .ok()?;
    Some(ids.iter().map(hex::encode).collect())
}

/// Set the trust facet's render-clock offset
/// (`fauna_client_capabilities::trust_clock::set_clock_offset_secs`) — the
/// `trust_facet_advance_clock` agent command's mechanism, moving grant
/// liveness and custody receipt freshness without sleeping out their windows
/// (testing.md convention 14). Gated on `test-helpers` so the symbol is absent
/// from a release binding (convention 15); `test-helpers` forwards the
/// capabilities crate's `e2e-agent` so the setter exists in the release-profile
/// `*-ffi-test` builds. Mirrors `set_backup_audit_clock_offset_secs`.
///
/// ⚠ Process-wide, and nothing auto-resets it — zero it once the lapse
/// assertions are done.
#[cfg(feature = "test-helpers")]
#[uniffi::export]
pub fn set_trust_clock_offset_secs(offset_secs: i64) {
    fauna_client_capabilities::trust_clock::set_clock_offset_secs(offset_secs);
}

/// Revoke a custody grant (`custody-holder-revoke-button`) — piece 2's one
/// gesture, and store-free.
///
/// `holder` is the row's `custodian_key`; a row still `pending` has none and the
/// act answers with an error rather than pretending, which is why the leg
/// disables the control while `pending` is set.
///
/// The load-bearing ordering — the nest's revoke BEFORE the signed record, so a
/// recorded revoke the nest never saw cannot leave the capability live — lives
/// in `fauna_client_custody::run_custody_act` and is not re-derived here.
///
/// The leg's revoke copy MUST state the honest bound (`ui/nests.md` § Trust
/// facet — custody rows): revocation stops future carriage and serving on honest
/// boxes; copies already held stay held.
#[fauna_uniffi_async::export]
pub async fn custody_revoke(
    nest: Arc<FfiNestClient>,
    owner_secret: Vec<u8>,
    grant_id: Vec<u8>,
    holder: Option<Vec<u8>>,
) -> Result<FfiCustodyActOutcome, FfiError> {
    let secret = secret32(&owner_secret)?;
    let holder = match holder {
        None => None,
        Some(bytes) => Some(
            <[u8; 32]>::try_from(bytes.as_slice())
                .map_err(|_| general_err("a custodian key must be exactly 32 bytes".to_string()))?,
        ),
    };
    // Revoke does not need the store — it rides the capabilities RPC and the
    // ceremony-row write — but the re-folded facet reads it for the registry-row
    // overlay.
    Ok(act_outcome(&nest, secret, None, CustodyAct::Revoke { grant_id, holder }).await)
}

/// Accept a custody offer (`custody-offer-accept-button`) — the consent card's
/// yes. `on_nest` is the target select's answer: `false` binds THIS device's
/// principal (the T10 writer key, never the roster's `device.db` id), `true`
/// binds the host's pinned NEST identity — offer the choice only where
/// [`custody_offer_shows_target_select`] says so; the act re-checks the pin
/// and answers with an error if it is missing.
///
/// The accept is RECORDED here and POSTED by the drive pass it spawns over
/// `session`, which is why the session is required rather than optional —
/// the mint's reason, and the same shape. The REQUIRED floor copy
/// (`custody-offer-floor-note`) renders before this control.
#[fauna_uniffi_async::export]
pub async fn custody_accept(
    nest: Arc<FfiNestClient>,
    owner_secret: Vec<u8>,
    session: Arc<ConversationsSession>,
    grant_id: Vec<u8>,
    on_nest: bool,
) -> Result<FfiCustodyActOutcome, FfiError> {
    let secret = secret32(&owner_secret)?;
    Ok(act_outcome(
        &nest,
        secret,
        Some(session),
        CustodyAct::Accept { grant_id, on_nest },
    )
    .await)
}

/// Decline a custody offer (`custody-offer-decline-button`) — the offer's
/// ceremony record is marked declined on the account plane, and the card
/// goes away on the re-folded facet.
#[fauna_uniffi_async::export]
pub async fn custody_decline(
    nest: Arc<FfiNestClient>,
    owner_secret: Vec<u8>,
    grant_id: Vec<u8>,
) -> Result<FfiCustodyActOutcome, FfiError> {
    let secret = secret32(&owner_secret)?;
    Ok(act_outcome(&nest, secret, None, CustodyAct::Decline { grant_id }).await)
}

/// Change a held custody's retained-bytes budget
/// (`custody-held-budget-input` commit) — writes the host's registry row
/// (device form) or re-deposits the nest hosting row (nest form). `cap` is the
/// new cap in bytes.
#[fauna_uniffi_async::export]
pub async fn custody_set_budget(
    nest: Arc<FfiNestClient>,
    owner_secret: Vec<u8>,
    grant_id: Vec<u8>,
    cap: u64,
) -> Result<FfiCustodyActOutcome, FfiError> {
    let secret = secret32(&owner_secret)?;
    Ok(act_outcome(&nest, secret, None, CustodyAct::SetBudget { grant_id, cap }).await)
}

/// Stop holding (`custody-held-stop-button`) — pauses the pull and KEEPS the
/// bytes already held; [`custody_remove`] is what gives the space back. A
/// budget edit afterwards never silently un-stops it.
#[fauna_uniffi_async::export]
pub async fn custody_stop(
    nest: Arc<FfiNestClient>,
    owner_secret: Vec<u8>,
    grant_id: Vec<u8>,
) -> Result<FfiCustodyActOutcome, FfiError> {
    let secret = secret32(&owner_secret)?;
    Ok(act_outcome(&nest, secret, None, CustodyAct::Stop { grant_id }).await)
}

/// Remove a held custody (`custody-held-remove-button`) — the RECLAIM: drops
/// the nest hosting row (nest form) or zeroes and stops the device row
/// (device form) so the next metering pass frees the bytes.
#[fauna_uniffi_async::export]
pub async fn custody_remove(
    nest: Arc<FfiNestClient>,
    owner_secret: Vec<u8>,
    grant_id: Vec<u8>,
) -> Result<FfiCustodyActOutcome, FfiError> {
    let secret = secret32(&owner_secret)?;
    Ok(act_outcome(&nest, secret, None, CustodyAct::Remove { grant_id }).await)
}

/// Whether the consent card renders its target select
/// (`custody-offer-target-select`) for `offer`: only when a nest can hold it
/// (the owner named its nest) AND this app holds a pinned identity for its home nest
/// (TOFU state — never the nest's own claim). Otherwise the select is ABSENT,
/// never disabled, and the accept binds this device.
///
/// A local read of the pin store; never a network call.
#[uniffi::export]
pub fn custody_offer_shows_target_select(
    nest: Arc<FfiNestClient>,
    offer: CustodyOfferRowView,
) -> bool {
    offer.nest_can_hold
        && fauna_anon_client::trust::pinned_nest_custodian_identity(&nest.nest_arc().nest_url())
            .is_some()
}

/// Run one custody act and project both halves across the boundary — the
/// re-folded facet and the error that must reach the page's `error-message`.
/// Every act reads the account store through `crate::account_runtime::handle()`
/// for the re-fold's registry overlay, and the store-writing ones for the write.
async fn act_outcome(
    nest: &FfiNestClient,
    secret: [u8; 32],
    session: Option<Arc<ConversationsSession>>,
    act: CustodyAct,
) -> FfiCustodyActOutcome {
    let ctx = CustodyCtx {
        nest: nest.nest_arc(),
        secret,
        store: crate::account_runtime::handle(),
        session,
    };
    let (facet, error) = run_custody_act(ctx, act).await;
    FfiCustodyActOutcome {
        facet: facet.as_ref().map(CustodyFacetView::from_snapshot),
        error,
    }
}

/// The offer flow's host options (`custody-mint-host-select`) — every 1:1
/// conversation this account could send a custody offer over.
///
/// The ceremony rides an EXISTING conversation channel: creating the DM is a
/// shipped user act, not ceremony business, which is why the options are the
/// conversations rather than a contact picker.
///
/// An **empty** list means "there is no one to ask yet". The leg must refuse to
/// open the flow and say so — `devices.custody_mint_no_contacts` ("Start a
/// conversation with them first — the request travels over it.") — rather than
/// presenting an empty picker whose confirm can never succeed. Answering the
/// gesture is the point (e2e convention 11); `apps/fauna-tui`'s
/// `Action::CustodyMintOpen` is the reference.
#[uniffi::export]
pub fn custody_mint_candidates(
    owner_secret: Vec<u8>,
    session: Arc<ConversationsSession>,
) -> Result<Vec<CustodyMintCandidateView>, FfiError> {
    let secret = secret32(&owner_secret)?;
    Ok(fauna_client_custody::mint_candidates(&session, secret))
}

/// Send a custody offer (`custody-mint-confirm-button`) — v1 offers the
/// **Account** scope with the default grant window, the shape
/// `ui/devices.md` § Custody facet ratified (a scope/duration control is a
/// future ask, and shared-audience scopes stay explicit-only per the charter
/// carve-out).
///
/// `host` and `channel_hex` come from a [`custody_mint_candidates`] row, passed
/// back unchanged — the leg picks a row, it does not assemble a channel.
///
/// The `session` is **required, not optional**: the act records the offer on a
/// fresh ceremony record and the drive pass posts it over this channel, so a
/// mint with no session would record an offer that is never sent and never
/// reported. Requiring it here makes that unrepresentable at this boundary
/// (`run_custody_act` also refuses it, for the native apps that assemble a
/// `CustodyCtx` by hand).
///
/// ⚠ The REQUIRED floor copy (`custody-mint-floor-note`,
/// `devices.custody_mint_floor`) renders **before** the confirm — a custodian
/// sees the shape of the owner's data, and `ui/nests.md` § Trust facet — custody
/// rows makes stating that a precondition of the gesture, not a nicety.
#[fauna_uniffi_async::export]
pub async fn custody_mint(
    nest: Arc<FfiNestClient>,
    owner_secret: Vec<u8>,
    session: Arc<ConversationsSession>,
    host: Vec<u8>,
    channel_hex: String,
) -> Result<FfiCustodyActOutcome, FfiError> {
    let secret = secret32(&owner_secret)?;
    let host = <[u8; 32]>::try_from(host.as_slice())
        .map_err(|_| general_err("a host actor id must be exactly 32 bytes".to_string()))?;
    // The mint does not need the store — it writes a ceremony record to
    // the account plane, and the host's registry row is the HOST's side of the
    // ceremony, not the owner's — but the re-folded facet reads it for the
    // registry overlay.
    Ok(act_outcome(
        &nest,
        secret,
        Some(session),
        CustodyAct::Mint {
            host: fauna_core::identity::ActorId(host),
            channel_hex,
        },
    )
    .await)
}

/// Fire one ceremony drive pass in the background — the record-then-act loop's
/// "act" half, and what makes the owner-side receipt freshness real: the pass
/// fetches the receipts custodian nests deposited at this account's own nest and
/// folds each through the recorded-accept verify path.
///
/// Cheap when settled, so a leg calls it freely on its edges: session start
/// (crash recovery) and every ceremony-moved notification. Fire-and-forget by
/// design — the state is durable and the next edge retries, so there is no
/// outcome to await. Without a `session` the pass has no channel to post on and
/// returns immediately, which is why the parameter is required rather than
/// silently defaulted.
///
/// ⚠ **Async ONLY so the spawn has a runtime — keep it that way.** The call
/// returns as soon as the pass is spawned, so there is still nothing to await
/// for. But `spawn_drive` hands the pass to `tokio::spawn`, which needs an
/// entered runtime context. Only an `async_runtime = "tokio"` export enters one
/// (`native-async-execution.md` § The execution model); a UniFFI caller's
/// thread has none. As a plain `#[uniffi::export]` this panicked "there is no
/// reactor running" on every call, on every UniFFI app. Every app swallowed it,
/// so the drive never ran anywhere (pinned by
/// `FaunaApp.Tests.CustodyDriveFfiTests`).
#[fauna_uniffi_async::export]
pub async fn custody_drive(
    nest: Arc<FfiNestClient>,
    owner_secret: Vec<u8>,
    session: Arc<ConversationsSession>,
) -> Result<(), FfiError> {
    let secret = secret32(&owner_secret)?;
    fauna_client_custody::spawn_drive(
        nest.nest_arc(),
        secret,
        Some(session),
        crate::account_runtime::handle(),
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The degraded badge key is the shared constant, not a per-app string.
    ///
    /// (The projection's own behaviour is pinned where it lives — the eight
    /// tests in `fauna_client_capabilities::custody_view`, which cover both this
    /// face and web's, since both read the same rows.)
    /// Port 1 refuses instantly: every act below fails on its first nest
    /// read, which is the point — the failure must come back as the outcome's
    /// `error` (for the page's `error-message`), never as a silent `Ok` with
    /// nothing to show (e2e convention 11), and the unreadable re-fold as a
    /// `None` facet the leg keeps its previous rows over.
    fn unreachable_nest() -> Arc<FfiNestClient> {
        FfiNestClient::new("ws://127.0.0.1:1".to_string(), vec![9u8; 32]).unwrap()
    }

    fn assert_reported(outcome: FfiCustodyActOutcome, act: &str) {
        assert!(
            outcome.error.is_some(),
            "{act} against an unreachable nest must report, never drop silently"
        );
        assert!(
            outcome.facet.is_none(),
            "{act}: an unreadable re-fold is a transient"
        );
    }

    #[tokio::test]
    async fn the_held_side_gestures_report_their_failure() {
        // The acts read the account store off the process-global host, and a
        // store another test installed answers them without the nest.
        let _host = crate::account_runtime::tests::host_with_no_runtime().await;
        let secret = vec![9u8; 32];
        let grant = vec![1u8; 16];
        assert_reported(
            custody_decline(unreachable_nest(), secret.clone(), grant.clone())
                .await
                .unwrap(),
            "decline",
        );
        assert_reported(
            custody_set_budget(unreachable_nest(), secret.clone(), grant.clone(), 1 << 20)
                .await
                .unwrap(),
            "set-budget",
        );
        assert_reported(
            custody_stop(unreachable_nest(), secret.clone(), grant.clone())
                .await
                .unwrap(),
            "stop",
        );
        assert_reported(
            custody_remove(unreachable_nest(), secret, grant)
                .await
                .unwrap(),
            "remove",
        );
    }

    #[tokio::test]
    async fn a_malformed_owner_secret_is_refused_at_the_boundary() {
        assert!(
            custody_stop(unreachable_nest(), vec![9u8; 31], vec![1u8; 16])
                .await
                .is_err()
        );
    }

    /// No owner nest on the offer → no select, whatever the pin store holds.
    #[test]
    fn an_offer_no_nest_can_hold_never_shows_the_target_select() {
        let offer = CustodyOfferRowView {
            grant_id: vec![1u8; 16],
            owner: vec![2u8; 32],
            scopes: fauna_client_capabilities::custody_view::CustodyScopesView {
                whole_account: true,
                scopes: Vec::new(),
            },
            offered_at_secs: 0,
            nest_can_hold: false,
        };
        assert!(!custody_offer_shows_target_select(
            unreachable_nest(),
            offer
        ));
    }

    /// An offer a nest can hold, with no pin for the home nest: the select
    /// stays absent (the escrow-holder rule's no-pin arm).
    #[test]
    fn a_nest_holdable_offer_without_a_pin_never_shows_the_target_select() {
        let offer = CustodyOfferRowView {
            grant_id: vec![1u8; 16],
            owner: vec![2u8; 32],
            scopes: fauna_client_capabilities::custody_view::CustodyScopesView {
                whole_account: true,
                scopes: Vec::new(),
            },
            offered_at_secs: 0,
            nest_can_hold: true,
        };
        assert!(!custody_offer_shows_target_select(
            unreachable_nest(),
            offer
        ));
    }

    #[test]
    fn the_degraded_badge_key_is_the_shared_constant() {
        assert_eq!(
            custody_degraded_badge_key(),
            vm::CUSTODY_DEGRADED_BADGE_KEY.to_string()
        );
    }
}
