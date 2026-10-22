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
//! A third floor sits beside them: the spoof floor
//! ([`unauthenticated_local_from`]), which keeps a forward from vouching for an
//! unauthenticated local `From:` (`mail-forwarding.md` § Spoof floor).
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

/// The spoof floor (`mail-forwarding.md` § Spoof floor — a local `From:` must
/// have authenticated): true when forwarding this message would hand a
/// downstream MX a `From:` our SRS envelope aligns with, for mail whose
/// `From:` never authenticated at our perimeter — suppress the forward (local
/// delivery still completes).
///
/// A forward is never DKIM-signed and its envelope is SRS-rewritten onto the
/// deployment's primary mail domain (`primary_domain`), so a `From:` under the
/// primary's organizational domain SPF-aligns relaxed through that envelope and
/// passes DMARC downstream. A message whose inbound DMARC verdict was `pass`
/// (`dmarc_pass`) authenticated as that domain on arrival and forwards as
/// before. Any other verdict — none, a fail the perimeter admitted (log-only,
/// DMARC enforcement off, or the domain's own `p=quarantine` / `p=none`), a
/// temp/perm error — is suppressed for a `From:` host that is:
///
/// - a served domain, beneath one, or above one — the
///   same-domain-or-one-beneath-the-other test; an ancestor is counted only
///   while it keeps two labels, so a bare TLD never matches. Conservative for
///   served domains the envelope does not sit on; or
/// - at or beneath the primary's last two labels — the shortest name a
///   public-suffix list could make the primary's organizational domain (every
///   organizational domain keeps two labels), so a sibling of the primary
///   (`hr.example.com` beside `mail.example.com`) is caught without one. Under
///   a multi-label public suffix (`example.co.uk`) it reaches the whole suffix:
///   that over-refuses forwards, never admits a spoof.
///
/// `primary_domain` empty (none configured) turns the second arm off.
/// `from_hosts` are the domains of the `From:` mailboxes; none parsed ⇒
/// nothing aligns ⇒ false.
pub fn unauthenticated_local_from<'a>(
    from_hosts: impl IntoIterator<Item = &'a str>,
    local_domains: &[&str],
    primary_domain: &str,
    dmarc_pass: bool,
) -> bool {
    if dmarc_pass {
        return false;
    }
    let primary_floor = two_label_suffix(primary_domain);
    from_hosts.into_iter().any(|host| {
        local_domains
            .iter()
            .any(|local| domains_related(host, local))
            || primary_floor
                .as_deref()
                .is_some_and(|floor| domains_related(host, floor))
    })
}

/// The last two labels of `domain` (lower-cased, a trailing root dot ignored),
/// or `None` when it has fewer than two.
fn two_label_suffix(domain: &str) -> Option<String> {
    let domain = domain.trim().trim_end_matches('.').to_ascii_lowercase();
    let labels: Vec<&str> = domain.split('.').filter(|l| !l.is_empty()).collect();
    (labels.len() >= 2).then(|| labels[labels.len() - 2..].join("."))
}

/// Same domain, or one a subdomain of the other — case-insensitively, a
/// trailing root dot ignored, and the ancestor holding at least two labels.
fn domains_related(a: &str, b: &str) -> bool {
    let a = a.trim().trim_end_matches('.').to_ascii_lowercase();
    let b = b.trim().trim_end_matches('.').to_ascii_lowercase();
    if a.is_empty() || b.is_empty() {
        return false;
    }
    if a == b {
        return true;
    }
    let beneath = |child: &str, parent: &str| {
        parent.split('.').filter(|l| !l.is_empty()).count() >= 2
            && child
                .strip_suffix(parent)
                .is_some_and(|head| head.ends_with('.'))
    };
    beneath(&a, &b) || beneath(&b, &a)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn spoof_floor_suppresses_an_unauthenticated_local_from() {
        let local = ["test.example", "other.example"];
        // A served domain, a subdomain of one, a parent of one: all align.
        assert!(unauthenticated_local_from(
            ["test.example"],
            &local,
            "",
            false
        ));
        assert!(unauthenticated_local_from(
            ["TEST.Example."],
            &local,
            "",
            false
        ));
        assert!(unauthenticated_local_from(
            ["hr.test.example"],
            &local,
            "",
            false
        ));
        let sub = ["mail.corp.example"];
        assert!(unauthenticated_local_from(
            ["corp.example"],
            &sub,
            "",
            false
        ));
        // Any one local mailbox among several trips it.
        assert!(unauthenticated_local_from(
            ["far.example", "other.example"],
            &local,
            "",
            false
        ));
    }

    #[test]
    fn spoof_floor_suppresses_a_sibling_of_the_primary() {
        // The envelope is on the primary; when the primary sits below its
        // organizational domain, a sibling under that domain aligns relaxed.
        let local = ["mail.example.com", "other.example"];
        let primary = "mail.example.com";
        assert!(unauthenticated_local_from(
            ["hr.example.com"],
            &local,
            primary,
            false
        ));
        // A deeper primary: its organizational domain may be any ancestor
        // keeping two labels, so the floor reaches up to the last two.
        let deep = ["a.b.example.com"];
        assert!(unauthenticated_local_from(
            ["hr.example.com"],
            &deep,
            "a.b.example.com",
            false
        ));
        assert!(unauthenticated_local_from(
            ["x.y.example.com"],
            &deep,
            "A.B.Example.COM.",
            false
        ));
        // DMARC pass still forwards.
        assert!(!unauthenticated_local_from(
            ["hr.example.com"],
            &local,
            primary,
            true
        ));
        // A look-alike outside the primary's two-label suffix forwards.
        assert!(!unauthenticated_local_from(
            ["hr.example.net"],
            &local,
            primary,
            false
        ));
        assert!(!unauthenticated_local_from(
            ["evilexample.com"],
            &local,
            primary,
            false
        ));
        // A sibling of a served domain that is not the primary is not under
        // our envelope's organizational domain.
        assert!(!unauthenticated_local_from(
            ["hr.other.example"],
            &["mail.other.example", "example.com"],
            "example.com",
            false
        ));
        // A single-label primary has no two-label suffix to reach up to.
        assert!(!unauthenticated_local_from(
            ["hr.localhost"],
            &["localhost"],
            "localhost",
            false
        ));
        // No primary known: the arm is off.
        assert!(!unauthenticated_local_from(
            ["hr.example.com"],
            &local,
            "",
            false
        ));
    }

    #[test]
    fn spoof_floor_forwards_authenticated_or_foreign_mail() {
        let local = ["test.example"];
        // DMARC passed on arrival: the domain vouched for it.
        assert!(!unauthenticated_local_from(
            ["test.example"],
            &local,
            "",
            true
        ));
        // A foreign From cannot align with our envelope.
        assert!(!unauthenticated_local_from(
            ["example.org"],
            &local,
            "",
            false
        ));
        // A suffix that is not a label boundary is not a subdomain.
        assert!(!unauthenticated_local_from(
            ["attest.example"],
            &local,
            "",
            false
        ));
        // A bare TLD is never an ancestor.
        assert!(!unauthenticated_local_from(["example"], &local, "", false));
        // No parseable From mailbox: nothing aligns.
        assert!(!unauthenticated_local_from(
            std::iter::empty::<&str>(),
            &local,
            "",
            false
        ));
    }

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
