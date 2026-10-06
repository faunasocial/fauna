//! Re-exports the page-level ATProto login-plane settings state machine so its
//! UniFFI exports surface in the generated Swift / Kotlin / C# bindings, plus a
//! free-fn constructor that builds the machine over an [`FfiNestClient`]'s
//! WS-RPC connection. The machine itself lives in
//! libs/fauna-atproto-settings-machine; this file is a thin glue layer
//! (mirrors src/labeler_catalog.rs).

use std::sync::Arc;

pub use fauna_atproto_settings_machine::{
    AppCredentialRow, AtprotoSessionRow, AtprotoSettingsError, AtprotoSettingsMachine,
    AtprotoSettingsObserver, AtprotoSettingsSnapshot,
};

use crate::{FfiError, FfiNestClient};

/// Build an [`AtprotoSettingsMachine`] for the ATProto settings page's
/// login-plane groups (app credentials + connected apps + the external-apps
/// kill-switch, `atproto-pds-full.md` § App surface) over `nest`'s
/// authenticated WS-RPC connection. `observer` ticks on every snapshot change.
///
/// Like [`crate::build_mail_settings_machine`] and unlike the admin machines,
/// this one needs the actor's 32-byte ed25519 `secret`: the machine signs the
/// D10 delegation with it, and its rotation-key custody reads the actor's
/// custody rows, sealed under the BackupKey derived from that seed. The
/// credential secrets it mints rest on the account plane (`fauna.state.atproto`,
/// wired below — the nest holds only the Argon2id verifier and can never
/// recover them). Returns `Result` because the keypair derivation is fallible.
#[uniffi::export]
pub fn build_atproto_settings_machine(
    nest: Arc<FfiNestClient>,
    secret: Vec<u8>,
    observer: Arc<dyn AtprotoSettingsObserver>,
) -> Result<Arc<AtprotoSettingsMachine>, FfiError> {
    let keypair = crate::keypair_from_bytes(&secret)?;
    #[cfg(feature = "account-runtime")]
    let consent_grants = consent_grant_seams(&keypair);
    let machine = fauna_atproto_settings_machine::build_atproto_settings_machine(
        nest.nest_arc(),
        keypair,
        observer,
        // The process-wide registry (critical-alerts.md § Mechanism) —
        // the custody check posts here on every status convergence; the
        // client renders it via `critical_alerts_registry()`.
        Some(crate::critical_alerts::critical_alerts_registry()),
    );
    machine.set_identity_store(atproto_identity_store());
    // Where the minted secrets rest: the account plane's `fauna.state.atproto`,
    // through the shared seam over this seat's handle, read fresh per call —
    // wired here, the ONE seam android/windows/macOS/iOS funnel through
    // (`crate::devices::build_devices_machine`'s fleet door is the precedent).
    // A build without the account runtime (the Go mail bridge) leaves it
    // unwired, which refuses every credential read and write.
    #[cfg(feature = "account-runtime")]
    machine.set_credential_store(Arc::new(
        fauna_client_account_runtime::atproto_credentials::RuntimeAtprotoCredentials::new(
            crate::account_runtime::handle,
        ),
    ));
    // The consent-time grant an approve of a records consent mints, over the
    // same handle — so all four UniFFI apps answer a third-party app's records
    // request at once. Without the runtime the approve is refused.
    #[cfg(feature = "account-runtime")]
    machine.set_consent_grant_seams(consent_grants);
    Ok(machine)
}

/// The consent-time grant's seams (`fauna_atproto_settings_machine::
/// consent_grant`) over this process's account runtime — shared by the AT
/// Protocol page's builder above and the Connected apps tray's
/// (`crate::connected_apps::wire_connected_apps_consent_grant`).
#[cfg(feature = "account-runtime")]
pub(crate) fn consent_grant_seams(
    keypair: &fauna_core::identity::ActorKeypair,
) -> Arc<fauna_atproto_settings_machine::ConsentGrantSeams> {
    let door = Arc::new(fauna_client_config::ResolvingLedgerStore::new(
        crate::account_runtime::handle,
    ));
    Arc::new(
        fauna_atproto_settings_machine::ConsentGrantSeams::from_keypair(
            keypair,
            door.clone(),
            door,
        ),
    )
}

/// The ATProto identity custody door (`fauna.state.atproto-identity`) over
/// this process's account runtime — the one impl every seat wires, read
/// fresh per call, so an absent runtime refuses rather than answering an
/// empty ring. Shared by the settings machine above and the critical-alert
/// sweep (`crate::critical_alerts`), so all four UniFFI apps get it at once.
pub(crate) fn atproto_identity_store()
-> Arc<dyn fauna_client_atproto::identity_store::AtprotoIdentityStore> {
    #[cfg(feature = "account-runtime")]
    {
        Arc::new(
            fauna_client_account_runtime::atproto_identity::RuntimeAtprotoIdentity::new(
                crate::account_runtime::handle,
            ),
        )
    }
    // A build with no account runtime has no plane to hold the custody: every
    // read and write is refused, never kept anywhere else (the kind is
    // plane-only).
    #[cfg(not(feature = "account-runtime"))]
    {
        Arc::new(fauna_client_atproto::identity_store::NoAccountRuntime)
    }
}

/// Move the D10 delegation row's **render** clock, for the cross-app
/// `atproto_delegation_advance_clock` agent command. Seconds; `0` resets.
///
/// The seam a lapse journey needs: `expiring_soon`/`expired` sit ~76 and ~90
/// days into the grant window, so reaching them by waiting is impossible and
/// reaching them by `sleep` is the defunct-test pattern convention 14 forbids.
/// Scoped to the liveness comparison ONLY — never the mint clock, which always
/// stamps a freshly minted cert with the real wall clock (faking that would
/// mint a future-dated cert the nest's provision-time check has never seen for
/// real). Full rationale: `fauna_atproto_settings_machine::delegation_clock`.
///
/// ⚠ **Process-wide, and nothing auto-resets it** — a test that leaves an offset
/// behind silently lapses the very next delegation the process mints.
///
/// Compiled only into the test-flavored native FFI build (apple-ffi/windows-ffi
/// enable `test-helpers`, which forwards the machine crate's `e2e-agent` — the
/// setter it calls is itself gated on `debug_assertions OR e2e-agent`, and these
/// builds are `--release`). Inert in production, absent from the Go bridge.
/// Mirrors [`crate::FfiFeedManager::inject_posts_for_test`]'s gating exactly.
#[cfg(feature = "test-helpers")]
#[uniffi::export]
pub fn set_delegation_clock_offset_secs(offset_secs: i64) {
    fauna_atproto_settings_machine::set_delegation_clock_offset_secs(offset_secs);
}

/// UniFFI face of
/// [`fauna_atproto_settings_machine::delegation_capability_label`] — one D10
/// granted capability's **wire spelling** in user voice, for
/// `atproto-delegation-scope`.
///
/// The row's `capabilities` cross this boundary as wire spellings (`"Post"`,
/// `"UpdateProfile"`), so a native shell that renders the leaf has to turn each
/// into an i18n key. tui and linux hold the row as Rust and call
/// `DelegationRow::capability_labels`; before this export the only option over
/// here was a hand-written Swift/Kotlin/C# copy of the same two-arm map, and the
/// fourth copy is where they start disagreeing (priority #4). An unrecognized
/// capability comes back as its own wire form rather than vanishing — dropping
/// one would *understate* a grant, the single direction an audit surface must
/// never err in.
///
/// Gated behind `value-format` for the SAME reason as `nostr_key_source_label` /
/// `reminder_label`: a bare `fauna_core::LocalizedText` crosses the boundary,
/// which `uniffi-bindgen-go` emits as an uncompilable cross-namespace import in
/// the Go mail-bridge's `--no-default-features` build (no Bluesky UI there, so
/// dropping it is harmless → no `mail-bridge-ffi` regen).
#[cfg(feature = "value-format")]
#[uniffi::export]
pub fn delegation_capability_label(capability: String) -> fauna_core::localized::LocalizedText {
    fauna_atproto_settings_machine::delegation_capability_label(&capability)
}

/// UniFFI face of
/// [`fauna_atproto_settings_machine::delegation_liveness_label`] — the D10
/// row's liveness in user voice, for `atproto-delegation-status`'s prose.
///
/// Same cross-boundary and `value-format` reasoning as
/// [`delegation_capability_label`]. ⚠ The **wire** spelling, not this text, is
/// what an e2e asserts — it rides the leaf's own `state` attr — so a wording
/// change never breaks a test and a test never pins prose.
#[cfg(feature = "value-format")]
#[uniffi::export]
pub fn delegation_liveness_label(liveness: String) -> fauna_core::localized::LocalizedText {
    fauna_atproto_settings_machine::delegation_liveness_label(&liveness)
}

/// UniFFI face of [`fauna_atproto_settings_machine::identity_status_label`] —
/// the identity summary's status in user voice, for `atproto-hosted-handle`.
///
/// The one reading of `IdentitySummaryRow::status` (tui and linux call it
/// in-process): without this export android, apple and windows each hand-wrote
/// the same five-arm match. An unrecognized status comes back as its own wire
/// word rather than blanking the row. Gated behind `value-format` for the SAME
/// reason as [`delegation_liveness_label`].
#[cfg(feature = "value-format")]
#[uniffi::export]
pub fn identity_status_label(status: String) -> fauna_core::localized::LocalizedText {
    fauna_atproto_settings_machine::identity_status_label(&status)
}

/// The **pre-fetch** page state — what a shell renders after mount and before
/// its first `refresh()` resolves. This is `AtprotoSettingsSnapshot::default()`,
/// reached across the boundary.
///
/// **Why this needs its own free function.** UniFFI generates no constructor
/// for a `Record`'s `Default`, so before this existed every shell hand-rolled a
/// stand-in — and a hand-rolled literal can express a combination the record's
/// own invariants forbid. It did, twice, on android alone (`hostedGateReason`,
/// then `showDidMethodRadio`), and windows shipped the inverse of the contract
/// outright: a pre-fetch page whose hosted gate rendered **open**.
///
/// The default is not "all fields empty" — five of its fields are deliberately
/// non-zero (`level: "off"`, `hosted_gate_reason: Some(pending)`,
/// `did_method: "plc"`, `show_did_method_radio: true`,
/// `external_apps_enabled: true`), and the gate-reason one is a **ratified UI
/// obligation**: a closed gate must always say why (`ui/README.md` § Copy
/// comprehensibility rule 5). That is the test for whether a machine's default
/// belongs on the seams at all — export it when the default is anything other
/// than the type's own zero value, because only then can a local literal drift
/// from it *silently*.
#[uniffi::export]
pub fn atproto_settings_prefetch_snapshot() -> AtprotoSettingsSnapshot {
    AtprotoSettingsSnapshot::default()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The seam returns the RATIFIED default, not a second opinion.
    ///
    /// Cheap, and it is the pin that matters: every native shell now paints its
    /// pre-fetch page off this function, so a future edit that "helpfully"
    /// tweaks what it returns would silently re-open the drift this export
    /// exists to close — and would do it on three apps at once, in the one
    /// window no test drives.
    #[test]
    fn the_prefetch_seam_returns_the_ratified_default() {
        assert_eq!(
            atproto_settings_prefetch_snapshot(),
            AtprotoSettingsSnapshot::default(),
        );
        let snap = atproto_settings_prefetch_snapshot();
        assert!(!snap.hosted_allowed, "the gate is closed pre-fetch");
        assert!(
            snap.hosted_gate_reason.is_some(),
            "and it says why (ui/README.md rule 5) — a closed gate with no \
             reason is exactly what each hand-rolled stand-in got wrong"
        );
    }
}
