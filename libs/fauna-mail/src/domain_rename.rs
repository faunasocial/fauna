//! Shared state vocabulary for the **primary-domain rename** state machine
//! (`docs/goal/behavior/mail-primary-domain-rename.md`).
//!
//! The rename moves the deployment's *primary* `mail_domains` row from one
//! domain to another (promote-another-then-demote-old) with a peer-MTA
//! cache-flush grace window. The full lifecycle spans cert re-issue, an atomic
//! anchor-flip transaction, per-domain DNS rewrites, listener dual-binding and a
//! grace watcher — all nest-side. What lives *here* (priority #2) is the pure,
//! I/O-free vocabulary that the nest storage layer, the admin RPC validators and
//! any future client "rename status" preview all interpret identically: the
//! [`RenameState`] enum + its wire-string mapping, the grace-window bounds, the
//! Let's Encrypt SAN cap and the TLS-posture monotonicity comparator.
//!
//! No clock, no DB, no I/O → WASM-safe and unit-testable in isolation. The
//! `state` column (nest) and the `state` wire field (`fauna-protocol`) are both
//! stored as the [`RenameState::as_str`] string, so the enum is the single
//! source of truth for the vocabulary while the persisted/transported shape
//! stays a plain string (additive-evolution friendly).

use serde::{Deserialize, Serialize};

/// The lifecycle state of a single `mail_domain_renames` row
/// (`mail-primary-domain-rename.md` § Lifecycle). Transitions are driven
/// nest-side; the admin RPCs are entry points (`start`, `complete`, `abort`) and
/// a periodic watcher promotes `grace` → `ready_to_complete` on wall-clock.
///
/// The `#[serde(rename_all = "snake_case")]` mapping is kept byte-identical to
/// [`RenameState::as_str`] / [`RenameState::from_wire`] (asserted by the tests),
/// so serde and the manual string conversion never drift.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RenameState {
    /// Admin called `start`; preconditions validated; row inserted. No DNS /
    /// cert / binding mutations yet.
    Requested,
    /// The certbot driver is acquiring the superset cert covering `mail.<new>`.
    CertIssuance,
    /// The new superset cert is acquired and idle in the store; one transaction
    /// away from the anchor flip.
    CertReady,
    /// The atomic flip transaction is in progress (is_primary flip + per-domain
    /// TXT rewrites + listener dual-binding).
    AnchorFlip,
    /// Dual-binding is live; both old and new anchors serve; peer caches
    /// converge on the new records over their TTLs.
    Grace,
    /// The grace watcher observed `NOW() > grace_ends_at`; awaiting the admin's
    /// `complete` sign-off.
    ReadyToComplete,
    /// Terminal: the old-primary anchor bindings were torn down; the row is
    /// audit-only.
    Completed,
    /// Terminal: the rename was unwound; the new domain stays a regular
    /// additional.
    Aborted,
}

/// Default peer-MTA cache-flush grace window (`mail-primary-domain-rename.md`
/// § Behavior — peer-MTA cache invalidation): 7 days covers the dominant cache
/// budget (MTA-STS `max_age`, default 86400 s) with a comfortable margin.
pub const DEFAULT_GRACE_DAYS: i64 = 7;
/// Minimum admin-selectable grace window (`start_primary_domain_rename`
/// `grace_days` range `[1, 30]`).
pub const GRACE_DAYS_MIN: i64 = 1;
/// Maximum admin-selectable grace window.
pub const GRACE_DAYS_MAX: i64 = 30;
/// Let's Encrypt's per-certificate SAN cap (`mail-primary-domain-rename.md`
/// § Behavior — cert chain re-issue ordering). With the default `expand_primary`
/// cert mode the deployment's cert carries `1 + 2 × N` SANs (N = active local
/// domains); a rename adds one (`mail.<new>`), so the post-rename graph is
/// `2 + 2 × N` — the `cert_san_limit_exceeded` refusal fires when that exceeds
/// this cap.
pub const LETSENCRYPT_SAN_LIMIT: usize = 100;

impl RenameState {
    /// The canonical wire/storage string for this state (kept identical to the
    /// serde `snake_case` mapping).
    pub fn as_str(self) -> &'static str {
        match self {
            RenameState::Requested => "requested",
            RenameState::CertIssuance => "cert_issuance",
            RenameState::CertReady => "cert_ready",
            RenameState::AnchorFlip => "anchor_flip",
            RenameState::Grace => "grace",
            RenameState::ReadyToComplete => "ready_to_complete",
            RenameState::Completed => "completed",
            RenameState::Aborted => "aborted",
        }
    }

    /// Parse a persisted/transported `state` string back into the enum.
    /// Returns `None` for an unrecognized value (a forward-compat state a newer
    /// nest wrote — the caller decides how to treat the unknown). Named
    /// `from_wire` rather than `from_str` to avoid `clippy::should_implement_trait`.
    pub fn from_wire(s: &str) -> Option<Self> {
        Some(match s {
            "requested" => RenameState::Requested,
            "cert_issuance" => RenameState::CertIssuance,
            "cert_ready" => RenameState::CertReady,
            "anchor_flip" => RenameState::AnchorFlip,
            "grace" => RenameState::Grace,
            "ready_to_complete" => RenameState::ReadyToComplete,
            "completed" => RenameState::Completed,
            "aborted" => RenameState::Aborted,
            _ => return None,
        })
    }

    /// A terminal state — `completed` or `aborted`. Terminal rows are audit-only
    /// and never advance; the single-active-rename invariant excludes them.
    pub fn is_terminal(self) -> bool {
        matches!(self, RenameState::Completed | RenameState::Aborted)
    }

    /// A pre-flip state — `requested`, `cert_issuance` or `cert_ready`. In these
    /// states no anchor binding has been mutated, so `abort` is a cheap state
    /// change with nothing to unwind (`mail-primary-domain-rename.md`
    /// § Lifecycle: "abort is a no-op database delete").
    pub fn is_pre_flip(self) -> bool {
        matches!(
            self,
            RenameState::Requested | RenameState::CertIssuance | RenameState::CertReady
        )
    }

    /// A post-flip **non-terminal** state — `anchor_flip`, `grace` or
    /// `ready_to_complete`: the `is_primary` flip has committed and the rename is
    /// still in flight (the grace window). The complement of [`is_pre_flip`] among
    /// non-terminal states. Two nest-side effects key on this
    /// (`mail-primary-domain-rename.md`): the DNS assembler keeps `mail.<old>`
    /// alive (§ Data — Mutations to per-domain DNS records, grace-window keep-
    /// alive) and the cert-lifecycle loop keeps `mail.<old>` in the desired SAN
    /// set (§ Behavior — cert chain re-issue ordering, During grace). (In this
    /// codebase `anchor_flip` is never a *persisted* resting state — the flip is
    /// atomic `cert_ready → grace` — but it is included so the classifier stays
    /// forward-safe if a later slice ever parks a row there.)
    pub fn is_post_flip_active(self) -> bool {
        matches!(
            self,
            RenameState::AnchorFlip | RenameState::Grace | RenameState::ReadyToComplete
        )
    }
}

/// Rank the TLS posture of an MTA-STS mode for the rename's monotonicity check
/// (`mail-primary-domain-rename.md` § Goal #3 / § Don't do these — TLS-posture
/// monotonicity): the new primary's posture must **match or exceed** the old
/// primary's, so `start_primary_domain_rename` refuses when
/// `tls_posture_rank(new) < tls_posture_rank(old)`. `testing < enforce`; any
/// other stored value ranks `0` (treated as the weakest, so it can never be a
/// silent *upgrade* that masks a regression). No human sets the stored mode
/// (`mail-multidomain.md` § The advance), so the one refusal this produces — a
/// target still in its `testing` window under an `enforce` primary — is a wait.
pub fn tls_posture_rank(mode: &str) -> u8 {
    match mode {
        "enforce" => 2,
        "testing" => 1,
        // Anything unexpected ranks lowest.
        _ => 0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ALL: &[RenameState] = &[
        RenameState::Requested,
        RenameState::CertIssuance,
        RenameState::CertReady,
        RenameState::AnchorFlip,
        RenameState::Grace,
        RenameState::ReadyToComplete,
        RenameState::Completed,
        RenameState::Aborted,
    ];

    #[test]
    fn as_str_from_wire_roundtrip_all_states() {
        for &s in ALL {
            assert_eq!(
                RenameState::from_wire(s.as_str()),
                Some(s),
                "roundtrip {s:?}"
            );
        }
        assert_eq!(RenameState::from_wire("not_a_state"), None);
        assert_eq!(RenameState::from_wire(""), None);
    }

    #[test]
    fn serde_snake_case_agrees_with_as_str() {
        // The serde `snake_case` derive must produce byte-identical strings to
        // `as_str`, so the enum's wire form is single-sourced.
        for &s in ALL {
            let json = serde_json::to_string(&s).unwrap();
            assert_eq!(json, format!("\"{}\"", s.as_str()), "serde vs as_str {s:?}");
            let back: RenameState = serde_json::from_str(&json).unwrap();
            assert_eq!(back, s);
        }
    }

    #[test]
    fn terminal_classification() {
        assert!(RenameState::Completed.is_terminal());
        assert!(RenameState::Aborted.is_terminal());
        for &s in ALL {
            if !matches!(s, RenameState::Completed | RenameState::Aborted) {
                assert!(!s.is_terminal(), "{s:?} must be non-terminal");
            }
        }
    }

    #[test]
    fn pre_flip_classification() {
        assert!(RenameState::Requested.is_pre_flip());
        assert!(RenameState::CertIssuance.is_pre_flip());
        assert!(RenameState::CertReady.is_pre_flip());
        // Anchor-flip onward has bindings to unwind → not pre-flip.
        assert!(!RenameState::AnchorFlip.is_pre_flip());
        assert!(!RenameState::Grace.is_pre_flip());
        assert!(!RenameState::ReadyToComplete.is_pre_flip());
        assert!(!RenameState::Completed.is_pre_flip());
        assert!(!RenameState::Aborted.is_pre_flip());
    }

    #[test]
    fn post_flip_active_classification() {
        // The flip has committed and the rename is still in flight (grace window).
        assert!(RenameState::AnchorFlip.is_post_flip_active());
        assert!(RenameState::Grace.is_post_flip_active());
        assert!(RenameState::ReadyToComplete.is_post_flip_active());
        // Pre-flip states are not post-flip.
        assert!(!RenameState::Requested.is_post_flip_active());
        assert!(!RenameState::CertIssuance.is_post_flip_active());
        assert!(!RenameState::CertReady.is_post_flip_active());
        // Terminal states are not "active".
        assert!(!RenameState::Completed.is_post_flip_active());
        assert!(!RenameState::Aborted.is_post_flip_active());
        // pre-flip and post-flip-active partition the non-terminal states.
        for &s in ALL {
            if !s.is_terminal() {
                assert_ne!(
                    s.is_pre_flip(),
                    s.is_post_flip_active(),
                    "{s:?} must be exactly one of pre-flip / post-flip-active"
                );
            }
        }
    }

    #[test]
    fn tls_posture_rank_ordering() {
        assert!(tls_posture_rank("none") < tls_posture_rank("testing"));
        assert!(tls_posture_rank("testing") < tls_posture_rank("enforce"));
        // Unknown ranks lowest (never a silent upgrade).
        assert_eq!(tls_posture_rank("bogus"), 0);
        assert_eq!(tls_posture_rank("none"), 0);
    }
}
