//! Forward-loop detection — the shared core behind
//! `docs/goal/behavior/mail-forwarding.md` § Loop detection.
//!
//! Two independent floors; either one trips ⇒ the forward is suppressed (the
//! **local delivery still completes** — loop detection kills the forward, not
//! the mail, `mail-forwarding.md:121,:133`):
//!
//! 1. **Received: chain count** — more than [`MAX_RECEIVED_HOPS`] `Received:`
//!    headers (ours + others') ⇒ suppress (`:121`).
//! 2. **`X-Fauna-Forwarded-By`** — each forward stamps one header; on inbound,
//!    if any such header's `actor=` equals *our own* forwarding actor, the
//!    message has already been forwarded by us and re-forwarding would tornado
//!    ⇒ suppress (`:125-133`). A peer's *different* `actor=` does not suppress
//!    (we forward through, accreting the chain until the Received: floor wins).
//!
//! This module is **pure** (std only): no tokio, no DNS, no parser. The header
//! format is shared so nest (the suppress decision) and a client (rule preview)
//! agree on exactly one spelling (priority #2). The header is **never stripped**
//! on outbound (`:137`) — peer forwarders need it to participate.

/// Hop ceiling matching the broader email-loop convention (postfix
/// `hopcount_limit`); RFC 5321 §6.3 says SHOULD reject "after some maximum"
/// without pinning the number (`mail-forwarding.md:121`).
pub const MAX_RECEIVED_HOPS: usize = 10;

/// The loop-detection header each forward stamps (`mail-forwarding.md:128`).
pub const HEADER_FORWARDED_BY: &str = "X-Fauna-Forwarded-By";

/// True when a message's `Received:` chain is too long to forward (suppress the
/// forward; local delivery still completes). The caller supplies the count of
/// `Received:` header fields on the inbound message.
pub fn received_chain_exceeded(received_header_count: usize) -> bool {
    received_header_count > MAX_RECEIVED_HOPS
}

/// Build the `X-Fauna-Forwarded-By` header **value** (without the field name)
/// (`mail-forwarding.md:128`): `actor=<id>; t=<unix>; rule=<rule-id|forward-all>`.
/// `rule` is a rule id, or the literal `forward-all` for the per-account shape.
pub fn forwarded_by_value(actor_id: &str, unix_time: i64, rule: &str) -> String {
    format!("actor={actor_id}; t={unix_time}; rule={rule}")
}

/// Extract the `actor=` token from one `X-Fauna-Forwarded-By` header value.
/// Tolerant of token order and surrounding whitespace; `None` if absent.
pub fn parse_forwarded_by_actor(value: &str) -> Option<&str> {
    value.split(';').find_map(|tok| {
        let tok = tok.trim();
        tok.strip_prefix("actor=").map(str::trim)
    })
}

/// True if any of the inbound message's `X-Fauna-Forwarded-By` header values was
/// stamped by *our own* forwarding actor (`mail-forwarding.md:133`). The caller
/// passes the values of every `X-Fauna-Forwarded-By` header present.
pub fn self_already_forwarded<'a>(
    forwarded_by_values: impl IntoIterator<Item = &'a str>,
    our_actor_id: &str,
) -> bool {
    forwarded_by_values
        .into_iter()
        .filter_map(parse_forwarded_by_actor)
        .any(|actor| actor == our_actor_id)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn received_floor_is_strictly_greater_than_ten() {
        assert!(!received_chain_exceeded(0));
        assert!(!received_chain_exceeded(10)); // exactly 10 still forwards
        assert!(received_chain_exceeded(11)); // the 11th hop suppresses
    }

    #[test]
    fn forwarded_by_value_shape() {
        assert_eq!(
            forwarded_by_value("act1", 1_700_000_000, "forward-all"),
            "actor=act1; t=1700000000; rule=forward-all",
        );
        assert_eq!(
            forwarded_by_value("act1", 42, "rule-7"),
            "actor=act1; t=42; rule=rule-7",
        );
    }

    #[test]
    fn parse_actor_round_trips_and_tolerates_order() {
        let v = forwarded_by_value("actABC", 1, "forward-all");
        assert_eq!(parse_forwarded_by_actor(&v), Some("actABC"));
        // Token order / extra whitespace tolerated.
        assert_eq!(
            parse_forwarded_by_actor("t=1;  actor=xy ; rule=r"),
            Some("xy")
        );
        assert_eq!(parse_forwarded_by_actor("t=1; rule=r"), None);
    }

    #[test]
    fn self_seen_suppresses_only_for_our_actor() {
        let mine = forwarded_by_value("me", 1, "forward-all");
        let peer = forwarded_by_value("peer", 2, "rule-3");
        // Our own stamp present → suppress.
        assert!(self_already_forwarded([mine.as_str(), peer.as_str()], "me"));
        // Only a peer's stamp → do not suppress (we forward through).
        assert!(!self_already_forwarded([peer.as_str()], "me"));
        // No stamps → do not suppress.
        assert!(!self_already_forwarded(std::iter::empty::<&str>(), "me"));
    }
}
