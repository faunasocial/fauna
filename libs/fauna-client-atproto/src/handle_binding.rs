//! Feeder #3 — is the identity this client protects still published under a
//! handle at the user's own deployment?
//!
//! # The gap this closes, and the much larger one it does NOT
//!
//! Feeder #1 ([`crate::genesis_verify`]) asks *"does the DID the nest reported
//! carry my rotation key?"*. `atproto-pds-bridge.md` § State & data shape
//! records the residual it leaves: **a hostile box publishing a different DID
//! under the user's handle passes feeder #1 completely** — the DID it hands the
//! client is genuinely the user's, while the handle the world resolves points
//! somewhere else.
//!
//! That residual's literal threat is **not client-side detectable**, and this
//! module deliberately does not pretend otherwise. A Fauna handle lives at
//! `<localpart>.<the deployment's domain>`, and the handle → DID binding is a
//! record in *that domain's* zone. **Handle custody is domain custody** — the
//! same statement `atproto-pds-bridge.md` already makes about did:web. A box
//! that owns the domain can bind the handle to any DID it likes, and no check
//! running on the client changes that. The route the residual sketched (compare
//! the `_atproto` rows of `fauna.dns.list_records`) is worse than incomplete:
//! that RPC is admin-gated (`bins/fauna-nest/src/dns_handlers.rs`), so it cannot
//! serve the ordinary users the threat targets, and both halves of its answer —
//! the expected value *and* the resolved status — are computed by the nest, so
//! on the very box the check exists to catch it reads the attacker's own
//! answer. It is not built, and § State & data shape says why.
//!
//! What IS soundly checkable is the **converse direction**, and it is checkable
//! for free, for every user, on every app:
//!
//! > The DID this client's rotation key protects must be *published* under a
//! > handle at the user's own deployment.
//!
//! The published handle is `alsoKnownAs` in the DID's PLC operation log, which
//! this crate already fetches over its own HTTPS connection with no nest in the
//! path. So the comparison is tamper-evident in the way that matters: the log is
//! append-only and public, so a box that published a rogue handle at any point
//! **cannot hide it by changing its answers now**.
//!
//! # What it catches, stated honestly
//!
//! * The box publishes the user's repo under a handle at a domain the user has
//!   nothing to do with (`alice.evil.example`) — the world sees the user's posts
//!   under someone else's name, and today nothing says so.
//! * The published identity claims **no** handle at all — the user is
//!   unreachable by handle and has no way to find out.
//! * The sloppy form of the residual's own threat: a decoy DID handed to this
//!   client that the box never bothered to dress in the user's handle.
//!
//! It does **not** catch a box that lies consistently — one that both publishes
//! a rogue binding *and* reports a matching handle domain. Nothing client-side
//! does; see the custody paragraph above.
//!
//! # Why the comparison is on the DOMAIN, not the whole handle
//!
//! The crying-wolf bar (`critical-alerts.md` § Goal) decides this. The ATProto
//! handle is derived at read time from the current Fauna handle, so a **rename**
//! legitimately leaves the published `alsoKnownAs` naming the old localpart
//! until the nest's PLC update lands. A whole-handle equality check would fire
//! on that window — and because the sweep is one-shot per session, a false alarm
//! raised there would stand, non-dismissable, for the entire session. A rename
//! never changes the *domain*, so the domain comparison is silent across it
//! while staying loud for every case above. The localpart divergence is still
//! **reported** ([`HandleBindingVerdict::Bound::published_handle`]) so a
//! permanently-stale binding is visible in the sweep's log without a banner.

use fauna_client_alerts::CriticalAlerts;
use fauna_core::localized::LocalizedText;

use crate::genesis_verify::{VerifyFailure, parse_audit_log};

/// i18n key for the alarm — published under a name that is not the user's.
const UNBOUND_ALARM_KEY: &str = "critical_alerts.atproto_handle_unbound";
/// i18n key for the alarm's other shape — published under no handle at all.
/// A separate key rather than the one above with an empty argument: the two are
/// different findings, and an English fallback composed here would not
/// translate.
const UNBOUND_NONE_ALARM_KEY: &str = "critical_alerts.atproto_handle_unbound_none";

/// Outcome of a successfully *read* log.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HandleBindingVerdict {
    /// The standing head op publishes a handle under the expected domain.
    Bound {
        /// The matching handle exactly as published — carried so the caller can
        /// log a localpart that no longer matches the user's current Fauna
        /// handle (a stale rename), which is reported but never alarmed.
        published_handle: String,
    },
    /// The standing head op publishes no handle under the expected domain —
    /// the alarm case. `published` is what it *did* claim (possibly empty),
    /// for the alert's identifying line.
    Unbound {
        /// Every `alsoKnownAs` entry of the standing head, verbatim.
        published: Vec<String>,
    },
    /// The standing head is not a `plc_operation` — in practice a
    /// `plc_tombstone`, the terminal retirement ([`crate::tombstone`]). A
    /// retired identity publishes no handle *by design*, so the absence that is
    /// otherwise the alarm is the expected end state here. Silent.
    Retired,
}

/// Which handle the *published* identity claims, checked against the domain the
/// user's handle is supposed to live at. Pure — no I/O; the unit under test.
///
/// Only the **standing head** op is read. Feeder #1 walks every standing op
/// because a box-senior genesis can nullify a later op inside PLC's 72 h contest
/// window; the published *handle*, by contrast, is whatever the head says today
/// — an older op legitimately carries an older handle after a rename, and
/// alarming on that would be crying wolf about a resolved past.
///
/// `expected_domain` is compared as a suffix (`…{.expected_domain}`) so a
/// deeper-nested handle still binds, and the localpart must be non-empty — an
/// `at://example.com` naming the bare domain is not this user's handle.
pub fn verify_handle_binding(
    audit_json: &[u8],
    expected_domain: &str,
) -> Result<HandleBindingVerdict, VerifyFailure> {
    let entries = parse_audit_log(audit_json)?;
    let head = entries
        .iter()
        .rfind(|e| !e.nullified)
        .ok_or(VerifyFailure::NoStandingOps)?;

    if head.operation.op_type != crate::genesis_verify::OP_TYPE_OPERATION {
        return Ok(HandleBindingVerdict::Retired);
    }

    let published = &head.operation.also_known_as;
    match published
        .iter()
        .find(|aka| handle_is_under(aka, expected_domain))
    {
        Some(aka) => Ok(HandleBindingVerdict::Bound {
            published_handle: strip_at_uri(aka).to_string(),
        }),
        None => Ok(HandleBindingVerdict::Unbound {
            published: published.clone(),
        }),
    }
}

/// `at://alice.example.com` under `example.com` → true.
///
/// DNS names are case-insensitive and a trailing dot is the same name, so both
/// sides are normalized before comparison — a box must not be able to dodge the
/// check by publishing `at://Alice.Example.COM`.
fn handle_is_under(aka: &str, expected_domain: &str) -> bool {
    let expected = expected_domain
        .trim()
        .trim_end_matches('.')
        .to_ascii_lowercase();
    if expected.is_empty() {
        return false;
    }
    let handle = strip_at_uri(aka)
        .trim()
        .trim_end_matches('.')
        .to_ascii_lowercase();
    // A non-`at://` entry is not a handle claim at all (the URI scheme is what
    // makes it one), and is deliberately not matched.
    if !aka.trim().starts_with("at://") {
        return false;
    }
    handle
        .strip_suffix(&expected)
        .and_then(|localpart| localpart.strip_suffix('.'))
        .is_some_and(|localpart| !localpart.is_empty())
}

/// `at://alice.example.com` → `alice.example.com`; anything else unchanged.
fn strip_at_uri(aka: &str) -> &str {
    let aka = aka.trim();
    aka.strip_prefix("at://").unwrap_or(aka)
}

/// The alert's registry key for `did`.
///
/// Feeder-scoped and identity-scoped per `critical-alerts.md` § Mechanism, and
/// deliberately **distinct from** feeder #1's `atproto-custody:<did>`: a wrong
/// senior key and an unbound handle are different accusations, so they must not
/// overwrite each other's text. The caller keeps them from ever both standing
/// for one event (see [`sync_handle_binding_alert`]).
pub fn alert_key(did: &str) -> String {
    format!("atproto-handle:{did}")
}

/// How many published handles the alarm names before it stops listing, and how
/// long each may be.
///
/// The list comes from a **hostile-controllable** field: `alsoKnownAs` on a PLC
/// operation someone else published. Uncapped it reaches the banner verbatim,
/// and tui sizes its alert band with `Constraint::Length(alerts.len())`
/// (`apps/fauna-tui/src/ui.rs`), so an operation carrying a thousand `at://`
/// entries inflates that band without limit. Nothing is forged by it — tui
/// sanitizes at paint and wraps on whitespace, and linux renders a markup-off
/// `gtk::Label` — but the alarm only has to be *identifiable*, not exhaustive:
/// three names are enough to recognise a hijack, and the audit log is the place
/// to read the rest.
const MAX_PUBLISHED_SHOWN: usize = 3;
const MAX_PUBLISHED_ENTRY_CHARS: usize = 64;

/// One published handle as the alarm names it: the `at://` prefix stripped, and
/// clipped to [`MAX_PUBLISHED_ENTRY_CHARS`] **characters** (not bytes — a byte
/// slice would panic mid-codepoint on the non-ASCII handles this field can
/// legitimately carry).
fn published_entry_for_display(raw: &str) -> String {
    let stripped = strip_at_uri(raw);
    let mut out: String = stripped.chars().take(MAX_PUBLISHED_ENTRY_CHARS).collect();
    if stripped.chars().count() > MAX_PUBLISHED_ENTRY_CHARS {
        out.push('…');
    }
    out
}

/// The alarm's lines for an unbound identity.
///
/// Pure, so the copy is testable on every platform. The `published` list is
/// attacker-supplied, so it is capped on both axes — see
/// [`MAX_PUBLISHED_SHOWN`].
pub fn handle_unbound_alert_lines(
    expected_domain: &str,
    published: &[String],
) -> Vec<LocalizedText> {
    // An identity claiming nothing is its own finding, not the same sentence
    // with a blank in it.
    let mut line = LocalizedText::key(if published.is_empty() {
        UNBOUND_NONE_ALARM_KEY
    } else {
        UNBOUND_ALARM_KEY
    });
    line.args
        .insert("domain".into(), expected_domain.to_string());
    if !published.is_empty() {
        let mut shown: Vec<String> = published
            .iter()
            .take(MAX_PUBLISHED_SHOWN)
            .map(|a| published_entry_for_display(a))
            .collect();
        if let Some(rest) = published.len().checked_sub(MAX_PUBLISHED_SHOWN)
            && rest > 0
        {
            // Deliberately a bare count, not a localized key: this rides inside
            // an existing key's `{published}` argument, and a nested
            // `LocalizedText` cannot. The number is the whole message.
            shown.push(format!("(+{rest})"));
        }
        line.args.insert("published".into(), shown.join(", "));
    }
    vec![line]
}

/// Post or clear this DID's handle-binding alert from an already-read verdict.
///
/// Both outcomes route through one call for the same reason feeder #2's does: a
/// binding that came back must take its banner with it, and a caller that only
/// knew how to post would leave a non-dismissable alarm about a resolved state.
///
/// **One accusation per event.** A `custody_alarm_standing` DID is one feeder #1
/// has already accused of a genesis-seniority mismatch; this feeder then stays
/// silent rather than stacking a second banner on the same compromise. It does
/// **not** require feeder #1 to have *succeeded* — the comparison here needs no
/// rotation keyring, so gating on that would make the check unavailable exactly
/// when a client's ring is unsynced, which is when a user most needs to be told
/// their identity is published under someone else's name.
pub fn sync_handle_binding_alert(
    alerts: &CriticalAlerts,
    did: &str,
    expected_domain: &str,
    verdict: &HandleBindingVerdict,
    custody_alarm_standing: bool,
) {
    let key = alert_key(did);
    match verdict {
        HandleBindingVerdict::Unbound { published } if !custody_alarm_standing => {
            tracing::warn!(
                did,
                expected_domain,
                published = ?published,
                "the published identity claims no handle at this deployment — raising the critical alert"
            );
            alerts.post(key, handle_unbound_alert_lines(expected_domain, published));
        }
        _ => alerts.clear(&key),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn log(aka: &[&str], nullified: bool) -> Vec<u8> {
        entries(&[(aka.to_vec(), nullified)])
    }

    fn entries(ops: &[(Vec<&str>, bool)]) -> Vec<u8> {
        let rows: Vec<serde_json::Value> = ops
            .iter()
            .map(|(aka, nullified)| {
                serde_json::json!({
                    "cid": "bafyfake",
                    "nullified": nullified,
                    "operation": {
                        "type": "plc_operation",
                        "rotationKeys": ["did:key:zUser"],
                        "alsoKnownAs": aka,
                        "services": {},
                        "prev": null,
                        "sig": "fakesig"
                    }
                })
            })
            .collect();
        serde_json::to_vec(&rows).unwrap()
    }

    #[test]
    fn head_published_under_the_expected_domain_is_bound() {
        let v = verify_handle_binding(&log(&["at://alice.example.com"], false), "example.com")
            .expect("reads");
        assert_eq!(
            v,
            HandleBindingVerdict::Bound {
                published_handle: "alice.example.com".into()
            }
        );
    }

    #[test]
    fn a_handle_at_another_domain_is_unbound() {
        let v = verify_handle_binding(&log(&["at://alice.evil.example"], false), "example.com")
            .expect("reads");
        assert_eq!(
            v,
            HandleBindingVerdict::Unbound {
                published: vec!["at://alice.evil.example".into()]
            }
        );
    }

    #[test]
    fn an_identity_claiming_no_handle_is_unbound() {
        let v = verify_handle_binding(&log(&[], false), "example.com").expect("reads");
        assert_eq!(v, HandleBindingVerdict::Unbound { published: vec![] });
    }

    /// The rename window — the whole reason the comparison is on the domain.
    /// The published localpart is stale, the user's identity is not in danger,
    /// and a one-shot sweep must not raise a session-long banner over it.
    #[test]
    fn a_stale_localpart_at_the_same_domain_stays_bound() {
        let v = verify_handle_binding(&log(&["at://oldname.example.com"], false), "example.com")
            .expect("reads");
        assert_eq!(
            v,
            HandleBindingVerdict::Bound {
                published_handle: "oldname.example.com".into()
            }
        );
    }

    /// The head is the *current* binding: an older op naming an older handle is
    /// a resolved past, and a newer op naming a rogue one is the live state.
    #[test]
    fn only_the_standing_head_decides() {
        let body = entries(&[
            (vec!["at://alice.example.com"], false),
            (vec!["at://alice.evil.example"], false),
        ]);
        assert!(matches!(
            verify_handle_binding(&body, "example.com").expect("reads"),
            HandleBindingVerdict::Unbound { .. }
        ));
    }

    /// A nullified op is not the published state — the head is the last
    /// *standing* one, exactly as the seniority check reads it.
    #[test]
    fn nullified_ops_are_not_the_head() {
        let body = entries(&[
            (vec!["at://alice.example.com"], false),
            (vec!["at://alice.evil.example"], true),
        ]);
        assert!(matches!(
            verify_handle_binding(&body, "example.com").expect("reads"),
            HandleBindingVerdict::Bound { .. }
        ));
    }

    /// A retired identity publishes no handle by design, so the absence that is
    /// otherwise the alarm must be silent here.
    #[test]
    fn a_tombstoned_head_is_retired_not_unbound() {
        let body = serde_json::to_vec(&serde_json::json!([
            {
                "cid": "bafyfake",
                "nullified": false,
                "operation": {
                    "type": "plc_operation",
                    "rotationKeys": ["did:key:zUser"],
                    "alsoKnownAs": ["at://alice.example.com"],
                    "services": {}, "prev": null, "sig": "fakesig"
                }
            },
            {
                "cid": "bafytomb",
                "nullified": false,
                "operation": {
                    "type": "plc_tombstone",
                    "prev": "bafyfake", "sig": "fakesig"
                }
            }
        ]))
        .unwrap();
        assert_eq!(
            verify_handle_binding(&body, "example.com").expect("reads"),
            HandleBindingVerdict::Retired
        );
    }

    #[test]
    fn retired_clears_rather_than_posting() {
        let alerts = CriticalAlerts::new();
        let did = "did:plc:abc";
        sync_handle_binding_alert(
            &alerts,
            did,
            "example.com",
            &HandleBindingVerdict::Unbound { published: vec![] },
            false,
        );
        assert_eq!(alerts.active().len(), 1);
        sync_handle_binding_alert(
            &alerts,
            did,
            "example.com",
            &HandleBindingVerdict::Retired,
            false,
        );
        assert!(alerts.active().is_empty());
    }

    #[test]
    fn a_log_with_no_standing_op_is_a_failure_not_an_alarm() {
        let body = entries(&[(vec!["at://alice.example.com"], true)]);
        assert!(matches!(
            verify_handle_binding(&body, "example.com"),
            Err(VerifyFailure::NoStandingOps)
        ));
    }

    #[test]
    fn an_unparseable_log_is_a_failure_not_an_alarm() {
        assert!(matches!(
            verify_handle_binding(b"not json", "example.com"),
            Err(VerifyFailure::Parse(_))
        ));
    }

    /// A box must not dodge the check with case or a trailing dot.
    #[test]
    fn comparison_is_dns_case_and_dot_insensitive() {
        assert!(matches!(
            verify_handle_binding(&log(&["at://Alice.Example.COM."], false), "EXAMPLE.com")
                .expect("reads"),
            HandleBindingVerdict::Bound { .. }
        ));
    }

    /// The bare domain is not a handle — a localpart is what makes it one.
    #[test]
    fn the_bare_domain_alone_does_not_bind() {
        assert!(matches!(
            verify_handle_binding(&log(&["at://example.com"], false), "example.com")
                .expect("reads"),
            HandleBindingVerdict::Unbound { .. }
        ));
    }

    /// A suffix that is not a label boundary is a different domain
    /// (`notexample.com` must not satisfy `example.com`).
    #[test]
    fn a_non_label_boundary_suffix_does_not_bind() {
        assert!(matches!(
            verify_handle_binding(&log(&["at://alice.notexample.com"], false), "example.com")
                .expect("reads"),
            HandleBindingVerdict::Unbound { .. }
        ));
    }

    /// An empty expected domain can never bind — with nothing to compare
    /// against, no published handle can be shown to be the user's. The caller
    /// skips the feeder entirely in that state; this is the belt-and-braces.
    #[test]
    fn an_empty_expected_domain_never_binds() {
        assert!(matches!(
            verify_handle_binding(&log(&["at://alice.example.com"], false), "").expect("reads"),
            HandleBindingVerdict::Unbound { .. }
        ));
    }

    #[test]
    fn unbound_posts_and_bound_clears() {
        let alerts = CriticalAlerts::new();
        let did = "did:plc:abc";
        sync_handle_binding_alert(
            &alerts,
            did,
            "example.com",
            &HandleBindingVerdict::Unbound {
                published: vec!["at://alice.evil.example".into()],
            },
            false,
        );
        assert_eq!(alerts.active().len(), 1);

        sync_handle_binding_alert(
            &alerts,
            did,
            "example.com",
            &HandleBindingVerdict::Bound {
                published_handle: "alice.example.com".into(),
            },
            false,
        );
        assert!(alerts.active().is_empty());
    }

    /// One accusation per event: feeder #1 already has a banner up for this
    /// DID, so a second one about the same compromise would only dilute it.
    #[test]
    fn a_standing_custody_alarm_suppresses_this_one() {
        let alerts = CriticalAlerts::new();
        sync_handle_binding_alert(
            &alerts,
            "did:plc:abc",
            "example.com",
            &HandleBindingVerdict::Unbound { published: vec![] },
            true,
        );
        assert!(alerts.active().is_empty());
    }

    // ── The published list is attacker-supplied, so it is capped ──────────────────────────────────────────────────────

    /// A hostile PLC operation can carry any number of `alsoKnownAs` entries,
    /// and the alarm text reaches a tui band sized by `alerts.len()`. Three
    /// names identify a hijack; the rest becomes a count.
    #[test]
    fn a_flood_of_published_handles_is_capped_to_a_count() {
        let flood: Vec<String> = (0..500).map(|i| format!("at://h{i}.evil.test")).collect();

        let lines = handle_unbound_alert_lines("mine.test", &flood);

        let published = &lines[0].args["published"];
        assert_eq!(
            published, "h0.evil.test, h1.evil.test, h2.evil.test, (+497)",
            "three names then a count — not 500 joined handles"
        );
    }

    /// The per-entry clip is by CHARACTER, not byte: a byte slice would panic
    /// mid-codepoint on the non-ASCII handles this field can legitimately hold.
    #[test]
    fn an_over_long_entry_is_clipped_without_splitting_a_codepoint() {
        let long = format!("at://{}", "é".repeat(200));

        let lines = handle_unbound_alert_lines("mine.test", &[long]);

        let published = &lines[0].args["published"];
        assert_eq!(published.chars().count(), MAX_PUBLISHED_ENTRY_CHARS + 1);
        assert!(
            published.ends_with('\u{2026}'),
            "clipped entries say so: {published}"
        );
    }

    /// The common case is untouched: a short list still reads in full, with no
    /// count appended.
    #[test]
    fn a_short_published_list_is_unchanged() {
        let lines = handle_unbound_alert_lines("mine.test", &["at://alice.other.test".to_string()]);
        assert_eq!(lines[0].args["published"], "alice.other.test");
    }
}
