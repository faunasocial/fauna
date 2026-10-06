//! The offline-affordance gate's UniFFI face — W4 (account-data-plane.md § Workstreams) phase 4 for the native apps.
//!
//! `fauna_protocol::offline_class::affordance(kind, connection_state)` is the
//! ONE rule that decides whether a surface may offer an affordance right now
//! (`docs/goal/architecture/account-data-plane.md` § The offline-mutation
//! contract → *How a surface asks*). tui reads it directly because it is Rust;
//! Swift, Kotlin and C# cannot, so this is how they reach the same function
//! instead of re-deriving `class == OnlineOnly` for themselves — which is
//! exactly the per-app copy priority #2 exists to prevent, and which the three
//! rulings (only class 3 desensitizes, an unregistered kind stays available,
//! only *known* offline words count as offline) make easy to get subtly wrong.
//!
//! **One call, both halves.** An app leg's gate is a single decision point —
//! desensitize the control *and* say why beside it — so the face returns both
//! at once rather than making each caller ask twice and risk the two answers
//! disagreeing. It mirrors the shared [`fauna_protocol::offline_class::Affordance`]
//! exactly: `available` is `Affordance::is_available`, `reason` is
//! `Affordance::reason`, and the reason is per-affordance because the charter
//! forbids a global "you are offline" banner (§ R11).

/// UniFFI face of [`fauna_protocol::offline_class::Affordance`] — a record
/// rather than an enum because both halves are wanted at the same instant, and
/// because `reason` is the enum's *derived* text, not a second variant payload.
///
/// `reason` is `None` exactly when `available` is `true`.
#[derive(uniffi::Record, Clone, Debug, PartialEq, Eq)]
pub struct FfiAffordance {
    /// `true` when the affordance may be offered — the boolean an app hands its
    /// widget's own sensitivity flag.
    pub available: bool,
    /// The localized reason to show beside a desensitized affordance
    /// (`common.needs_nest`), or `None` when it is available. Resolved through
    /// each app's own i18n runtime, like every other shared render decision.
    pub reason: Option<fauna_core::localized::LocalizedText>,
}

/// UniFFI face of [`fauna_protocol::offline_class::affordance`] — may a surface
/// offer an affordance that issues `kind`, given the app's current
/// `connection_state`?
///
/// `connection_state` is the same lowercase wire word
/// `fauna_core::format::connection_state_label` takes (`"connected"`,
/// `"connecting"`, `"disconnected"`, `"unreachable"`) — the word every app
/// family already carries, which is why the indicator and the gate cannot
/// disagree about what "connected" means.
///
/// Gated behind `value-format` for the SAME reason as `nostr_key_source_label`
/// / `reminder_label`: a bare `fauna_core::LocalizedText` crosses the boundary,
/// which `uniffi-bindgen-go` emits as an uncompilable cross-namespace import in
/// the Go mail-bridge's `--no-default-features` build. That build has no UI at
/// all, so it wants no gate → dropping the export there is harmless, and the
/// tracked Go binding tree needs no regen.
#[cfg(feature = "value-format")]
#[uniffi::export]
pub fn offline_affordance(kind: String, connection_state: String) -> FfiAffordance {
    let verdict = fauna_protocol::offline_class::affordance(&kind, &connection_state);
    FfiAffordance {
        available: verdict.is_available(),
        reason: verdict.reason(),
    }
}

/// UniFFI face of [`fauna_protocol::offline_class::is_online`] — is this
/// transport word one the offline gate treats as **online**?
///
/// The same `connection_state` word [`offline_affordance`] takes, and the same
/// rule behind it, split out because an app sometimes wants the transport
/// verdict on its own — with no kind in hand — most notably to publish it as an
/// e2e observable (`fauna_e2e_agent::CONNECTION_KEY`, the cross-app
/// **connection barrier**: every app greys an `OnlineOnly` affordance while the
/// word is offline, `"connecting"` is one of the offline words, so a test
/// driving an online-only control on a freshly launched app races the WS
/// handshake and loses under load).
///
/// ⚠ **This exists so that no app writes `state == "connected"`.** The gate's
/// polarity is deliberately asymmetric — *online unless the word is a KNOWN
/// offline word* — so that an older app meeting a future state word keeps its
/// controls live instead of greying them on a guess. An equality test inverts
/// that ruling, and inverts it in the direction that **hangs**: a barrier
/// waiting for `"connected"` blocks to its full ceiling on exactly the case the
/// gate was built to tolerate. Carrying the boolean across the boundary already
/// decided keeps [`fauna_protocol::offline_class::OFFLINE_STATE_WORDS`] with
/// one owner, the same bargain [`offline_affordance`] strikes for the verdict
/// and web strikes for both.
///
/// Pair it with [`crate::nest_client::connection_state_word`], never a
/// hand-rolled enum→word `match`, for the reason that function's own docs give.
///
/// Gated `value-format` for the same reason as its neighbours: it is only ever
/// wanted by the UI clients, and the Go mail-bridge's `--no-default-features`
/// build has neither a UI nor an e2e agent — so the export stays out of its
/// tracked binding tree and needs no regen.
#[cfg(feature = "value-format")]
#[uniffi::export]
pub fn connection_is_online(state: String) -> bool {
    fauna_protocol::offline_class::is_online(&state)
}

#[cfg(all(test, feature = "value-format"))]
mod tests {
    use super::*;

    /// The face must not restate the rule. These pin that it *forwards* — a
    /// kind of each class, in both connection directions — so a later
    /// reclassification moves the face's answer with the table instead of
    /// against it.
    fn a_kind_of(class: fauna_protocol::offline_class::OfflineClass) -> String {
        let registry = fauna_protocol::kind::KindRegistry::full();
        registry
            .iter()
            .map(|(name, _)| name.to_string())
            .find(|name| fauna_protocol::offline_class::offline_class(name) == Some(class))
            .unwrap_or_else(|| panic!("no registered kind is classified {class:?}"))
    }

    #[test]
    fn an_online_only_kind_is_unavailable_offline_and_says_why() {
        let verdict = offline_affordance(
            a_kind_of(fauna_protocol::offline_class::OfflineClass::OnlineOnly),
            "disconnected".to_string(),
        );
        assert!(!verdict.available);
        assert_eq!(
            verdict
                .reason
                .expect("a desensitized affordance says why")
                .key,
            "common.needs_nest"
        );
    }

    #[test]
    fn an_offline_capable_kind_stays_live_offline_with_no_reason() {
        for class in [
            fauna_protocol::offline_class::OfflineClass::OfflineSafe,
            fauna_protocol::offline_class::OfflineClass::OfflineQueued,
            fauna_protocol::offline_class::OfflineClass::Read,
        ] {
            let verdict = offline_affordance(a_kind_of(class), "disconnected".to_string());
            assert!(verdict.available, "{class:?} must stay live offline");
            assert!(verdict.reason.is_none());
        }
    }

    #[test]
    fn connected_gates_nothing() {
        let verdict = offline_affordance(
            a_kind_of(fauna_protocol::offline_class::OfflineClass::OnlineOnly),
            "connected".to_string(),
        );
        assert!(verdict.available);
        assert!(verdict.reason.is_none());
    }

    /// Ruling 2, carried across the boundary: an unregistered kind is a typo the
    /// bijection test must catch, never a dead button in a user's hands.
    #[test]
    fn an_unregistered_kind_stays_available() {
        assert!(
            offline_affordance("fauna.not.a.kind".to_string(), "disconnected".to_string())
                .available
        );
    }

    /// Ruling 3, carried across the boundary: an older app meeting a future
    /// state word keeps its controls live rather than greying them on a guess.
    #[test]
    fn an_unknown_state_word_keeps_controls_live() {
        assert!(
            offline_affordance(
                a_kind_of(fauna_protocol::offline_class::OfflineClass::OnlineOnly),
                "reticulating".to_string(),
            )
            .available
        );
    }

    /// The barrier's polarity, pinned in BOTH directions on the face itself —
    /// the three known offline words are offline, and everything else (today's
    /// `"connected"`, a *future* word, and the empty string an app publishes
    /// before it knows) is online. A copy of this rule that drifted to
    /// `== "connected"` would pass the first assert and fail only the future
    /// one, which is exactly the direction that hangs a barrier forever.
    #[test]
    fn is_online_forwards_the_gates_asymmetric_polarity() {
        for offline in ["connecting", "disconnected", "unreachable"] {
            assert!(
                !connection_is_online(offline.to_string()),
                "{offline:?} is a known offline word"
            );
        }
        for online in ["connected", "reticulating", ""] {
            assert!(
                connection_is_online(online.to_string()),
                "{online:?} is not a known offline word, so the gate keeps controls live"
            );
        }
    }

    /// The face and its sibling must never disagree: whatever
    /// `connection_is_online` says about a word is what `offline_affordance`
    /// does to an `OnlineOnly` kind on that same word. Two doors onto one rule
    /// is only safe while nothing can wedge between them.
    #[test]
    fn is_online_agrees_with_the_affordance_it_was_split_out_of() {
        let online_only = a_kind_of(fauna_protocol::offline_class::OfflineClass::OnlineOnly);
        for word in [
            "connected",
            "connecting",
            "disconnected",
            "unreachable",
            "reticulating",
            "",
        ] {
            assert_eq!(
                connection_is_online(word.to_string()),
                offline_affordance(online_only.clone(), word.to_string()).available,
                "the two faces disagree about {word:?}"
            );
        }
    }
}
