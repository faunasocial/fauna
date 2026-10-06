//! Greylisting decision logic — pure tuple-key derivation + defer/pass verdict.
//!
//! Greylist **state lives nest-side** in `greylist_tuples`
//! (`docs/goal/behavior/smtp-server.md` § Greylisting, `:172`): the MTA bridge
//! forwards `(MAIL FROM, RCPT TO, peer IP)` over `fauna.bridges.check_greylist`
//! and holds **no** local state. That is what gives "uniform behavior across
//! bridge restart" (`:62`, `:172`) under the supervisor-restart lifecycle
//! (`docs/goal/architecture/apps/bridges.md` § Lifecycle) — an in-process map
//! is wiped on every restart and re-defers legitimate senders.
//!
//! This module is the **pure half**: the tuple-key derivation (`:160–164`) and
//! the defer/pass decision over an existing row + policy thresholds (`:166–170`).
//! All I/O (reading/upserting the `greylist_tuples` row) is the nest handler's
//! job. Plain Rust — no tokio, no net, no uniffi — because the only caller is
//! nest-side Rust (cf. `crate::aliases::classify_role_address`).
//!
//! **Timestamps are `i64` Unix seconds.** Greylisting is coarse-grained
//! (60 s / 4 h / 30 d windows), so second resolution is ample and keeps the
//! type dag-cbor-friendly (no floats, no `SystemTime` across the wire).

use std::net::IpAddr;

/// The greylist tuple key (`smtp-server.md:160–164`).
///
/// Broader than `(ip, sender, recipient)` on two axes deliberately: the sender
/// **localpart is discarded** (bulk senders rotate it per-recipient but keep the
/// domain stable) and the peer IP is grouped into its **/24 (IPv4) or /64
/// (IPv6)** (senders rotate within a subnet; per-IP greylisting is too
/// aggressive). The recipient is kept in full (`:163`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GreylistTuple {
    /// Normalized sender domain — the part after the last `@`, ASCII-lowercased.
    /// Empty for the null sender `<>` or an address with no `@`.
    pub sender_domain: String,
    /// Full recipient address, ASCII-lowercased (`:163`).
    pub recipient: String,
    /// Client `/24` (IPv4) or `/64` (IPv6) prefix in CIDR form; the raw string
    /// (lowercased) when the IP does not parse.
    pub subnet: String,
}

/// Derive the greylist tuple from the envelope `MAIL FROM` address, the
/// `RCPT TO` address, and the peer IP.
///
/// Note vs. the retired in-process Go map (`policy.go::greylistKey`), which keyed
/// on the **full** `from` rather than the domain: this follows the spec (`:162`,
/// localpart discarded) — a stricter, bulk-sender-resistant key.
pub fn tuple_key(from: &str, to: &str, client_ip: &str) -> GreylistTuple {
    GreylistTuple {
        sender_domain: sender_domain_of(from),
        recipient: to.trim().to_ascii_lowercase(),
        subnet: subnet_of(client_ip),
    }
}

/// Extract the case-folded sender domain (part after the last `@`). Empty when
/// there is no `@` (null sender `<>`, or a bare/malformed address).
fn sender_domain_of(from: &str) -> String {
    match from.trim().rsplit_once('@') {
        Some((_local, domain)) => domain.to_ascii_lowercase(),
        None => String::new(),
    }
}

/// Group the peer IP into its `/24` (IPv4) or `/64` (IPv6) prefix, rendered as
/// `<network>/<bits>`. Falls back to the lowercased raw string for an
/// unparseable IP (so we still greylist consistently on a malformed peer addr).
fn subnet_of(client_ip: &str) -> String {
    match client_ip.trim().parse::<IpAddr>() {
        Ok(IpAddr::V4(v4)) => {
            let o = v4.octets();
            format!("{}.{}.{}.0/24", o[0], o[1], o[2])
        }
        Ok(IpAddr::V6(v6)) => {
            let mut o = v6.octets();
            // Zero everything past the first 64 bits.
            for b in o.iter_mut().skip(8) {
                *b = 0;
            }
            format!("{}/64", std::net::Ipv6Addr::from(o))
        }
        Err(_) => client_ip.trim().to_ascii_lowercase(),
    }
}

/// One persisted greylist row. `None` for `accepted_at` means the tuple has not
/// yet had a successful retry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GreylistRow {
    /// Unix seconds of the first attempt that opened (or last re-opened) the
    /// hold window.
    pub first_seen: i64,
    /// Unix seconds of the most recent attempt (every attempt updates this).
    pub last_attempt: i64,
    /// Unix seconds of the first successful retry, or `None` until one passes.
    pub accepted_at: Option<i64>,
}

/// Greylist policy thresholds (`smtp-server.md:166–170`).
#[derive(Debug, Clone, Copy)]
pub struct GreylistPolicy {
    /// Minimum age before a retry passes — default 60 s (`:168`).
    pub min_hold_seconds: i64,
    /// Maximum age of an un-accepted hold; a retry past this re-greylists —
    /// default 4 h = 14 400 s (`:169`).
    pub retry_window_seconds: i64,
    /// How long an accepted tuple is whitelisted — default 30 d (`:170`).
    pub whitelist_seconds: i64,
}

/// Retention floor for the greylist GC: a row untouched for this long can never
/// affect a decision again (it is past the longest — whitelist — window). 30 d,
/// matching the whitelist default (`:170`).
pub const GREYLIST_RETENTION_SECONDS: i64 = 30 * 24 * 60 * 60;

impl Default for GreylistPolicy {
    fn default() -> Self {
        Self {
            min_hold_seconds: 60,
            retry_window_seconds: 4 * 60 * 60,
            whitelist_seconds: GREYLIST_RETENTION_SECONDS,
        }
    }
}

/// The verdict for one attempt.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GreylistVerdict {
    /// Tempfail — the bridge returns `451 4.7.1 Greylisted`.
    Defer,
    /// Accept — the bridge continues the transaction (silent on whitelist).
    Pass,
}

/// The result of [`decide`]: the verdict plus the row to persist.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GreylistOutcome {
    pub verdict: GreylistVerdict,
    /// The row the nest handler should upsert after this attempt.
    pub next_row: GreylistRow,
}

/// Decide defer/pass for one attempt against the existing row (or `None` for a
/// first contact) at time `now`, under `policy`.
///
/// Acceptance window is `[min_hold, retry_window]` measured from `first_seen`:
/// * **No row** → `Defer`; open a hold (`first_seen = now`).
/// * **Un-accepted, age < min_hold** → `Defer` (retried too fast); keep the
///   hold, bump `last_attempt`.
/// * **Un-accepted, min_hold ≤ age ≤ retry_window** → `Pass`; mark
///   `accepted_at = now` (the tuple is now whitelisted).
/// * **Un-accepted, age > retry_window** → `Defer`; re-greylist (reset the hold
///   to `now`) — defends against a sender that probes once then waits days
///   (`:169`).
/// * **Accepted, within whitelist** → `Pass` (silent, no verdict surfaced).
/// * **Accepted, whitelist expired** → `Defer`; re-greylist. (The GC normally
///   prunes these first; this keeps `decide` correct on a stale row.)
pub fn decide(row: Option<GreylistRow>, now: i64, policy: GreylistPolicy) -> GreylistOutcome {
    let Some(row) = row else {
        return GreylistOutcome {
            verdict: GreylistVerdict::Defer,
            next_row: GreylistRow {
                first_seen: now,
                last_attempt: now,
                accepted_at: None,
            },
        };
    };

    match row.accepted_at {
        Some(accepted_at) => {
            if now.saturating_sub(accepted_at) <= policy.whitelist_seconds {
                GreylistOutcome {
                    verdict: GreylistVerdict::Pass,
                    next_row: GreylistRow {
                        last_attempt: now,
                        ..row
                    },
                }
            } else {
                re_greylist(now)
            }
        }
        None => {
            let age = now.saturating_sub(row.first_seen);
            if age < policy.min_hold_seconds {
                GreylistOutcome {
                    verdict: GreylistVerdict::Defer,
                    next_row: GreylistRow {
                        last_attempt: now,
                        ..row
                    },
                }
            } else if age <= policy.retry_window_seconds {
                GreylistOutcome {
                    verdict: GreylistVerdict::Pass,
                    next_row: GreylistRow {
                        last_attempt: now,
                        accepted_at: Some(now),
                        ..row
                    },
                }
            } else {
                re_greylist(now)
            }
        }
    }
}

/// Open a fresh hold at `now` and defer (used for first-contact-style resets).
fn re_greylist(now: i64) -> GreylistOutcome {
    GreylistOutcome {
        verdict: GreylistVerdict::Defer,
        next_row: GreylistRow {
            first_seen: now,
            last_attempt: now,
            accepted_at: None,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── tuple_key ────────────────────────────────────────────────────

    #[test]
    fn tuple_discards_sender_localpart_and_casefolds() {
        let t = tuple_key(
            "Alice.Bulk+tag@Mail.Example.COM",
            "Bob@Fauna.test",
            "203.0.113.7",
        );
        assert_eq!(t.sender_domain, "mail.example.com");
        assert_eq!(t.recipient, "bob@fauna.test");
    }

    #[test]
    fn tuple_null_sender_has_empty_domain() {
        let t = tuple_key("", "bob@fauna.test", "203.0.113.7");
        assert_eq!(t.sender_domain, "");
    }

    #[test]
    fn tuple_ipv4_groups_to_slash24() {
        let a = tuple_key("a@x.test", "b@y.test", "203.0.113.7");
        let b = tuple_key("a@x.test", "b@y.test", "203.0.113.250");
        // Sibling IPs in the same /24 share an entry (`:164`).
        assert_eq!(a.subnet, "203.0.113.0/24");
        assert_eq!(a.subnet, b.subnet);
    }

    #[test]
    fn tuple_ipv6_groups_to_slash64() {
        let a = tuple_key("a@x.test", "b@y.test", "2001:db8:abcd:1::1");
        let b = tuple_key("a@x.test", "b@y.test", "2001:db8:abcd:1:ffff::9");
        assert_eq!(a.subnet, "2001:db8:abcd:1::/64");
        assert_eq!(a.subnet, b.subnet);
    }

    #[test]
    fn tuple_unparseable_ip_falls_back_to_raw() {
        let t = tuple_key("a@x.test", "b@y.test", "not-an-ip");
        assert_eq!(t.subnet, "not-an-ip");
    }

    // ── decide ───────────────────────────────────────────────────────

    fn pol() -> GreylistPolicy {
        GreylistPolicy::default()
    }

    #[test]
    fn first_contact_defers_and_opens_hold() {
        let out = decide(None, 1_000, pol());
        assert_eq!(out.verdict, GreylistVerdict::Defer);
        assert_eq!(out.next_row.first_seen, 1_000);
        assert_eq!(out.next_row.last_attempt, 1_000);
        assert_eq!(out.next_row.accepted_at, None);
    }

    #[test]
    fn retry_within_min_hold_defers() {
        let row = GreylistRow {
            first_seen: 1_000,
            last_attempt: 1_000,
            accepted_at: None,
        };
        // 59 s < 60 s min hold.
        let out = decide(Some(row), 1_059, pol());
        assert_eq!(out.verdict, GreylistVerdict::Defer);
        assert_eq!(out.next_row.first_seen, 1_000); // hold preserved
        assert_eq!(out.next_row.last_attempt, 1_059);
        assert_eq!(out.next_row.accepted_at, None);
    }

    #[test]
    fn retry_after_min_hold_within_window_passes_and_whitelists() {
        let row = GreylistRow {
            first_seen: 1_000,
            last_attempt: 1_000,
            accepted_at: None,
        };
        // 90 s ≥ 60 s and ≤ 4 h.
        let out = decide(Some(row), 1_090, pol());
        assert_eq!(out.verdict, GreylistVerdict::Pass);
        assert_eq!(out.next_row.accepted_at, Some(1_090));
        assert_eq!(out.next_row.first_seen, 1_000);
    }

    #[test]
    fn retry_past_window_unaccepted_regreylists() {
        let row = GreylistRow {
            first_seen: 1_000,
            last_attempt: 1_000,
            accepted_at: None,
        };
        // 4 h + 1 s after first_seen, never accepted.
        let out = decide(Some(row), 1_000 + 4 * 3600 + 1, pol());
        assert_eq!(out.verdict, GreylistVerdict::Defer);
        assert_eq!(out.next_row.first_seen, 1_000 + 4 * 3600 + 1); // hold reset
        assert_eq!(out.next_row.accepted_at, None);
    }

    #[test]
    fn accepted_within_whitelist_passes_silently() {
        let row = GreylistRow {
            first_seen: 1_000,
            last_attempt: 1_090,
            accepted_at: Some(1_090),
        };
        // 29 d later — still inside the 30 d whitelist.
        let out = decide(Some(row), 1_090 + 29 * 86400, pol());
        assert_eq!(out.verdict, GreylistVerdict::Pass);
        assert_eq!(out.next_row.accepted_at, Some(1_090)); // unchanged
        assert_eq!(out.next_row.last_attempt, 1_090 + 29 * 86400);
    }

    #[test]
    fn accepted_after_whitelist_expiry_regreylists() {
        let row = GreylistRow {
            first_seen: 1_000,
            last_attempt: 1_090,
            accepted_at: Some(1_090),
        };
        // 31 d later — whitelist expired.
        let out = decide(Some(row), 1_090 + 31 * 86400, pol());
        assert_eq!(out.verdict, GreylistVerdict::Defer);
        assert_eq!(out.next_row.accepted_at, None);
        assert_eq!(out.next_row.first_seen, 1_090 + 31 * 86400);
    }

    #[test]
    fn zero_min_hold_passes_on_first_retry() {
        // greylist_delay_secs=0 edge (though the handler short-circuits when the
        // feature is disabled; this proves decide is still well-defined).
        let p = GreylistPolicy {
            min_hold_seconds: 0,
            ..pol()
        };
        let row = GreylistRow {
            first_seen: 1_000,
            last_attempt: 1_000,
            accepted_at: None,
        };
        let out = decide(Some(row), 1_000, p);
        assert_eq!(out.verdict, GreylistVerdict::Pass);
    }
}
