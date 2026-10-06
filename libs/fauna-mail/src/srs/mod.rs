//! SRS (Sender Rewriting Scheme) envelope encode/decode — the shared core
//! behind `docs/goal/behavior/mail-forwarding.md` § SRS.
//!
//! This module is **pure**: no tokio, no DNS, no `mail-parser`. It is
//! WASM-safe so a client SPA can parse/validate a forward envelope using the
//! *same* code the nest forward-dispatch path uses (priority #2). The SRS
//! secret is taken as a `&[u8]` parameter — its storage + rotation live in
//! nest (`mail-forwarding.md` § SRS secret).
//!
//! # Why SRS
//!
//! A naive forward (keep the envelope `MAIL FROM`, re-emit downstream) fails
//! the downstream MX's SPF check (`mail-forwarding.md:57`). SRS rewrites the
//! envelope `MAIL FROM` to our own domain so SPF aligns, while leaving the
//! `From:` header + original DKIM signature untouched so DMARC-via-DKIM still
//! passes for the original sender (`mail-forwarding.md:59-63`). The rewrite is
//! reversible so a bounce to the rewritten address decodes back to the
//! forwarding actor.
//!
//! # Envelope shape (`mail-forwarding.md:102`)
//!
//! ```text
//! SRS0=HHH=TT=<forwarder-actor-id>=<sender-domain>=<sender-localpart>@<our-domain>
//! ```
//!
//! - `HHH` — 4 base32 chars of `HMAC-SHA-256(secret, payload)[:20 bits]`
//!   (`:79`). The MAC authenticates a bounce-decode as round-tripping our own
//!   encoding (anti-forgery: an attacker forging an `SRS0=` sender cannot mint
//!   a valid `HHH`).
//! - `TT` — 2 base32 chars of `day mod 1024` (`:80`); decode rejects bounces
//!   older than `max_bounce_age_days` (default [`DEFAULT_SRS_MAX_BOUNCE_AGE_DAYS`]).
//! - The standard SRS "sender domain" slot is **overloaded** with the Fauna
//!   forwarder actor-id so the inbound bounce-decode routes to the forwarder
//!   directly (`:102`, `:111` — a compile-time Fauna extension).
//!
//! `SRS1=` is the chained form (`:86-88`): a `MAIL FROM` that is *already*
//! `SRS0=`/`SRS1=` (we are re-forwarding a peer-forwarded message) is rewritten
//! to `SRS1=`, structurally identical here but tagged for peer interop; the
//! preserved peer payload sits in the `<sender-domain>`/`<sender-localpart>`
//! slots. (Bounce *routing* for the chained case — Fauna stop-at-forwarder vs.
//! canonical up-chain re-bounce — is the consuming track's call, not this
//! codec's.)
//!
//! # Wire decisions (delegated to the consuming track, `mail-forwarding.md:6`)
//!
//! - **The HMAC covers the full payload** (`TT || actor || domain || local`),
//!   not the doc's literal `:79` triple, so the actor-id (`:102`) cannot be
//!   swapped without invalidating `HHH`. The doc's `## Implementation status
//!   today` records this and reconciles `:79`/`:102`.
//! - **base32 alphabet = RFC 4648 upper, no padding.** `HHH`/`TT` are decoded
//!   only by us (peers round-trip the whole address opaquely, `:102`), so the
//!   alphabet is an internal choice, not an interop surface.
//! - **payload fields percent-escape `=`, `@`, `%`** (and non-printable /
//!   non-ASCII bytes) so the `=`-split decode is unambiguous even when a
//!   localpart contains a literal `=` (`:81`).

use hmac::{Hmac, Mac};
use sha2::Sha256;
use thiserror::Error;

type HmacSha256 = Hmac<Sha256>;

// ── Compile-time constants (NOT configurable, `mail-forwarding.md:106-111`) ──

/// First-hop SRS prefix (`mail-forwarding.md:78`).
pub const SRS0_PREFIX: &str = "SRS0=";
/// Chained SRS-of-SRS prefix (`mail-forwarding.md:86`).
pub const SRS1_PREFIX: &str = "SRS1=";
/// `TT` runs through one full cycle per 1024-day window (`mail-forwarding.md:80,:110`).
pub const TT_MODULUS_DAYS: u64 = 1024;
/// Truncation of the HMAC for `HHH` — 20 bits → 4 base32 chars (`:79,:109`).
pub const HHH_BITS: u32 = 20;
/// Default max bounce age before an `SRS0=`/`SRS1=` decode is rejected as
/// expired (`mail.forward.srs_max_bounce_age_days`, `mail-forwarding.md:80,:101`).
pub const DEFAULT_SRS_MAX_BOUNCE_AGE_DAYS: u64 = 28;

const TT_CHARS: usize = 2;
const HHH_CHARS: usize = 4;
/// RFC 4648 base32 alphabet (upper-case, no padding).
const B32_ALPHABET: &[u8; 32] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZ234567";

// ── Errors ───────────────────────────────────────────────────────────

/// Why an SRS decode failed. The SMTP reply each maps to is the caller's
/// (the MTA RCPT-TO stage) responsibility; the texts below mirror
/// `mail-forwarding.md:100-101`.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum SrsError {
    /// The local-part is not an `SRS0=`/`SRS1=` address — not an SRS bounce.
    #[error("not an SRS address")]
    NotSrs,
    /// Structurally malformed SRS address (wrong field count, bad base32, …).
    #[error("malformed SRS address")]
    Malformed,
    /// HMAC mismatch — forged or corrupted. Caller hard-rejects
    /// `550 5.1.1 SRS verification failed` (NOT tempfail, `mail-forwarding.md:100`).
    #[error("SRS verification failed")]
    MacFail,
    /// `TT` age exceeds the max — caller rejects `550 5.4.4 SRS bounce expired`
    /// (`mail-forwarding.md:101`).
    #[error("SRS bounce expired")]
    Expired,
}

/// The wire discriminator of `fauna.bridges.decode_srs_bounce` — the `outcome`
/// field of `fauna_protocol::bridge_routing::DecodeSrsBounceReply`, which the
/// Go MTA switches on at RCPT-TO to decide accept / `550` / tempfail
/// (`mail-forwarding.md` § Bounce decode).
///
/// **This is the single owner of the six tokens.** Until 2026-08-23 the Rust
/// side hand-wrote all six as `"…".into()` against a `pub outcome: String`
/// while the *Go* side carried the named `wsrpc.SrsBounceOutcome` const family
/// — the reverse of the MTA-STS case, where Go had the bare literals. The
/// weaker side was ours.
///
/// It is a wider set than [`SrsError`] on purpose. Two of the six are not
/// decode failures at all: `ok` is a verified bounce whose forwarding row still
/// exists, and `orphan` is a verified bounce whose row is gone (account deleted
/// / row pruned) — a distinction only the caller holding the outbound table can
/// draw, and one that matters because the orphan payload carries the original
/// sender's PII and must be dropped rather than delivered anywhere.
///
/// One-way: nothing echoes this back, so there is no parser here. The Go
/// `default:` arm answers an unrecognised outcome with a `451` tempfail — the
/// safe direction, and also the reason drift here would be quiet: **every** SRS
/// bounce would tempfail forever rather than fail loudly.
/// `libs/fauna-mail/tests/go_wire_outcome_contract.rs` pins these tokens
/// against the Go mirror.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SrsBounceOutcome {
    /// Verified, our-issued bounce whose forwarding row is live; the reply's
    /// `forwarder_actor_id` / `original_sender` / `original_destination` are
    /// populated and the MTA delivers the DSN to the forwarder.
    Ok,
    /// Not an `SRS0=`/`SRS1=` address — the MTA treats it as a normal
    /// recipient and falls through to `validate_recipient`.
    NotSrs,
    /// Structurally invalid SRS → `550`, no retry.
    Malformed,
    /// HMAC mismatch (forged or corrupt) → `550 5.1.1`, no retry
    /// (`mail-forwarding.md:100`).
    MacFail,
    /// `TT` age over the max → `550 5.4.4` (`:101`).
    Expired,
    /// Verified, but the forwarding row is gone. The MTA accepts the RCPT so
    /// the sending MX stops retrying, then drops at DATA and counts it —
    /// never the admin mailbox, because the payload carries the original
    /// sender's PII (`:104,:117`).
    Orphan,
}

impl SrsBounceOutcome {
    /// The wire token. Exhaustive by construction — never add a `_` arm.
    pub fn as_wire(&self) -> &'static str {
        match self {
            Self::Ok => "ok",
            Self::NotSrs => "not_srs",
            Self::Malformed => "malformed",
            Self::MacFail => "mac_fail",
            Self::Expired => "expired",
            Self::Orphan => "orphan",
        }
    }
}

impl From<&SrsError> for SrsBounceOutcome {
    /// Every decode failure has exactly one wire token, so the failure half of
    /// the vocabulary is derived from the error type rather than restated.
    /// (The two success-side outcomes — `Ok` / `Orphan` — are not reachable
    /// from an `SrsError` and are named by the caller, which is the only place
    /// that knows whether the forwarding row survived.)
    fn from(err: &SrsError) -> Self {
        match err {
            SrsError::NotSrs => Self::NotSrs,
            SrsError::Malformed => Self::Malformed,
            SrsError::MacFail => Self::MacFail,
            SrsError::Expired => Self::Expired,
        }
    }
}

/// The result of decoding an `SRS0=`/`SRS1=` bounce recipient.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SrsDecoded {
    /// The forwarding-config owner's actor id (the Fauna extension payload
    /// field) — the bounce routes here, NOT the original sender
    /// (`mail-forwarding.md:102,:178`).
    pub forwarder_actor_id: String,
    /// The original `MAIL FROM` that was rewritten (`<localpart>@<domain>`).
    pub original_sender: String,
}

// ── Public API ───────────────────────────────────────────────────────

/// Rewrite an envelope `MAIL FROM` for a forward (`mail-forwarding.md:69-73`).
///
/// `now_day` is days since a caller-fixed epoch (deployment epoch or Unix
/// epoch — must be consistent with the value passed to [`srs_decode`]). If
/// `original_mail_from` is already `SRS0=`/`SRS1=` the result is the chained
/// `SRS1=` form (`:86-88`).
///
/// Returns the rewritten address `SRS0=…@local_domain` (or `SRS1=…`).
pub fn srs_forward(
    secret: &[u8],
    local_domain: &str,
    now_day: u64,
    forwarder_actor_id: &str,
    original_mail_from: &str,
) -> Result<String, SrsError> {
    let (local, domain) = split_addr(original_mail_from).ok_or(SrsError::Malformed)?;
    // An already-SRS'd MAIL FROM (we are re-forwarding a peer's forward) gets
    // the chained `SRS1=` prefix (`mail-forwarding.md:86-88`); otherwise `SRS0=`.
    let prefix = if strip_srs_prefix(local).is_some() {
        SRS1_PREFIX
    } else {
        SRS0_PREFIX
    };
    let tt = (now_day % TT_MODULUS_DAYS) as u16;
    let hhh = compute_hhh(secret, tt, forwarder_actor_id, domain, local);
    let tt_b32 = b32_encode(tt as u32, TT_CHARS);
    Ok(format!(
        "{prefix}{hhh}={tt_b32}={}={}={}@{local_domain}",
        pct_encode(forwarder_actor_id),
        pct_encode(domain),
        pct_encode(local),
    ))
}

/// Decode + verify an inbound `SRS0=`/`SRS1=` recipient local-part
/// (`mail-forwarding.md:96-104`). `local_part` is the part *before*
/// `@<our-domain>` (the caller strips the domain at RCPT-TO).
pub fn srs_decode(
    secret: &[u8],
    now_day: u64,
    max_bounce_age_days: u64,
    local_part: &str,
) -> Result<SrsDecoded, SrsError> {
    let rest = strip_srs_prefix(local_part).ok_or(SrsError::NotSrs)?;
    // `HHH=TT=actor=domain=local` — the payload fields percent-escape `=`, so
    // exactly four separators remain.
    let fields: Vec<&str> = rest.split('=').collect();
    if fields.len() != 5 {
        return Err(SrsError::Malformed);
    }
    let hhh = fields[0];
    let tt = b32_decode(fields[1], TT_CHARS).ok_or(SrsError::Malformed)? as u16;
    let actor = pct_decode(fields[2]).ok_or(SrsError::Malformed)?;
    let domain = pct_decode(fields[3]).ok_or(SrsError::Malformed)?;
    let local = pct_decode(fields[4]).ok_or(SrsError::Malformed)?;

    // Verify the MAC before trusting any field (`mail-forwarding.md:100` —
    // mismatch hard-rejects, the caller does not tempfail/retry).
    let expected = compute_hhh(secret, tt, &actor, &domain, &local);
    if !expected.eq_ignore_ascii_case(hhh) {
        return Err(SrsError::MacFail);
    }

    // Reject bounces older than the max age (`mail-forwarding.md:101`). `tt` is
    // `day mod 1024`, so the age is computed modulo the cycle (handles wrap).
    let now_mod = (now_day % TT_MODULUS_DAYS) as u32;
    let age = (now_mod + TT_MODULUS_DAYS as u32 - tt as u32) % TT_MODULUS_DAYS as u32;
    if age as u64 > max_bounce_age_days {
        return Err(SrsError::Expired);
    }

    Ok(SrsDecoded {
        forwarder_actor_id: actor,
        original_sender: format!("{local}@{domain}"),
    })
}

// ── Internal helpers ─────────────────────────────────────────────────

/// Encode the low `5 * n_chars` bits of `value` as `n_chars` base32 chars,
/// most-significant char first.
fn b32_encode(value: u32, n_chars: usize) -> String {
    let mut out = Vec::with_capacity(n_chars);
    for i in 0..n_chars {
        let shift = 5 * (n_chars - 1 - i);
        let idx = ((value >> shift) & 0x1f) as usize;
        out.push(B32_ALPHABET[idx]);
    }
    // Safe: every byte is from the ASCII B32 alphabet.
    String::from_utf8(out).expect("base32 alphabet is ASCII")
}

/// Inverse of [`b32_encode`]; `None` on a wrong length or a non-alphabet char.
fn b32_decode(s: &str, n_chars: usize) -> Option<u32> {
    if s.len() != n_chars {
        return None;
    }
    let mut value = 0u32;
    for &b in s.as_bytes() {
        // Case-insensitive: a downstream MX may lower-case the local-part on
        // the bounce RCPT TO before it returns to us.
        let idx = B32_ALPHABET
            .iter()
            .position(|&x| x == b.to_ascii_uppercase())?;
        value = (value << 5) | idx as u32;
    }
    Some(value)
}

/// Strip a leading `SRS0=`/`SRS1=` prefix (case-insensitive), returning the
/// remainder, or `None` if the local-part is not an SRS address.
fn strip_srs_prefix(s: &str) -> Option<&str> {
    let head = s.get(..SRS0_PREFIX.len())?;
    if head.eq_ignore_ascii_case(SRS0_PREFIX) || head.eq_ignore_ascii_case(SRS1_PREFIX) {
        Some(&s[SRS0_PREFIX.len()..])
    } else {
        None
    }
}

/// Percent-escape the structural separators (`=`, `@`, `%`) and any
/// non-printable / non-ASCII byte, so a field can hold an arbitrary localpart.
fn pct_encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for &b in s.as_bytes() {
        match b {
            b'%' | b'=' | b'@' => out.push_str(&format!("%{b:02X}")),
            0x21..=0x7e => out.push(b as char),
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

/// Inverse of [`pct_encode`]; `None` on a truncated/invalid `%XX` escape.
fn pct_decode(s: &str) -> Option<String> {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' {
            if i + 2 >= bytes.len() {
                return None;
            }
            let hi = (bytes[i + 1] as char).to_digit(16)?;
            let lo = (bytes[i + 2] as char).to_digit(16)?;
            out.push((hi * 16 + lo) as u8);
            i += 3;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    String::from_utf8(out).ok()
}

/// `HHH` = 4 base32 chars of `HMAC-SHA-256(secret, payload)[:20 bits]`, where
/// the payload is the full set of mutable fields (`TT || actor || domain ||
/// local`, NUL-separated). See the module-level "Wire decisions".
fn compute_hhh(
    secret: &[u8],
    tt: u16,
    actor: &str,
    sender_domain: &str,
    sender_local: &str,
) -> String {
    let mut mac = HmacSha256::new_from_slice(secret).expect("HMAC accepts any key length");
    mac.update(&tt.to_be_bytes());
    mac.update(&[0]);
    mac.update(actor.as_bytes());
    mac.update(&[0]);
    mac.update(sender_domain.as_bytes());
    mac.update(&[0]);
    mac.update(sender_local.as_bytes());
    let out = mac.finalize().into_bytes();
    // Top 20 bits, MSB-first: out[0] (8) | out[1] (8) | high nibble of out[2].
    let bits20 = ((out[0] as u32) << 12) | ((out[1] as u32) << 4) | ((out[2] as u32) >> 4);
    b32_encode(bits20, HHH_CHARS)
}

/// Split an `addr` on its last `@` into `(localpart, domain)`.
fn split_addr(addr: &str) -> Option<(&str, &str)> {
    let at = addr.rfind('@')?;
    let (local, rest) = (&addr[..at], &addr[at + 1..]);
    if local.is_empty() || rest.is_empty() {
        return None;
    }
    Some((local, rest))
}

#[cfg(test)]
mod tests {
    use super::*;

    const SECRET: &[u8] = b"0123456789abcdef0123456789abcdef"; // 32 bytes
    const DOMAIN: &str = "fauna.example";

    #[test]
    fn srs0_round_trip() {
        let enc = srs_forward(SECRET, DOMAIN, 100, "act1", "alice@example.com").unwrap();
        assert!(enc.starts_with(SRS0_PREFIX), "got {enc}");
        assert!(enc.ends_with("@fauna.example"), "got {enc}");
        let local_part = enc.strip_suffix("@fauna.example").unwrap();
        let dec = srs_decode(SECRET, 100, DEFAULT_SRS_MAX_BOUNCE_AGE_DAYS, local_part).unwrap();
        assert_eq!(dec.forwarder_actor_id, "act1");
        assert_eq!(dec.original_sender, "alice@example.com");
    }

    #[test]
    fn srs1_chained_round_trip() {
        // A MAIL FROM already SRS0-rewritten by a peer.
        let peer = "SRS0=ABCD=AA=peeract=gmail.com=alice@peer.com";
        let enc = srs_forward(SECRET, DOMAIN, 200, "act2", peer).unwrap();
        assert!(
            enc.starts_with(SRS1_PREFIX),
            "chained input must yield SRS1=, got {enc}"
        );
        let local_part = enc.strip_suffix("@fauna.example").unwrap();
        let dec = srs_decode(SECRET, 200, DEFAULT_SRS_MAX_BOUNCE_AGE_DAYS, local_part).unwrap();
        assert_eq!(dec.forwarder_actor_id, "act2");
        assert_eq!(
            dec.original_sender, peer,
            "chained decode preserves the peer SRS0 payload"
        );
    }

    #[test]
    fn literal_equals_and_at_round_trip() {
        // Localpart + domain both carry literal `=` (the escaping must survive).
        let weird = "od=d@ex=ample.com";
        let enc = srs_forward(SECRET, DOMAIN, 5, "a=b", weird).unwrap();
        let local_part = enc.strip_suffix("@fauna.example").unwrap();
        let dec = srs_decode(SECRET, 5, DEFAULT_SRS_MAX_BOUNCE_AGE_DAYS, local_part).unwrap();
        assert_eq!(dec.forwarder_actor_id, "a=b");
        assert_eq!(dec.original_sender, weird);
    }

    #[test]
    fn tampered_actor_fails_mac() {
        let enc = srs_forward(SECRET, DOMAIN, 10, "good", "alice@example.com").unwrap();
        let local_part = enc.strip_suffix("@fauna.example").unwrap();
        // Swap the actor field (4th `=`-segment) to a same-length value.
        let parts: Vec<&str> = local_part.splitn(2, "=good=").collect();
        assert_eq!(parts.len(), 2, "actor field present");
        let forged = format!("{}=evil={}", parts[0], parts[1]);
        assert_eq!(
            srs_decode(SECRET, 10, DEFAULT_SRS_MAX_BOUNCE_AGE_DAYS, &forged),
            Err(SrsError::MacFail),
        );
    }

    #[test]
    fn wrong_secret_fails_mac() {
        let enc = srs_forward(SECRET, DOMAIN, 10, "act", "alice@example.com").unwrap();
        let local_part = enc.strip_suffix("@fauna.example").unwrap();
        let other: &[u8] = b"ffffffffffffffffffffffffffffffff";
        assert_eq!(
            srs_decode(other, 10, DEFAULT_SRS_MAX_BOUNCE_AGE_DAYS, local_part),
            Err(SrsError::MacFail),
        );
    }

    #[test]
    fn expired_and_boundary() {
        let enc = srs_forward(SECRET, DOMAIN, 0, "act", "alice@example.com").unwrap();
        let local_part = enc.strip_suffix("@fauna.example").unwrap();
        // Exactly at the max age → ok; one day past → expired.
        assert!(srs_decode(SECRET, 28, 28, local_part).is_ok());
        assert_eq!(
            srs_decode(SECRET, 29, 28, local_part),
            Err(SrsError::Expired),
        );
    }

    #[test]
    fn tt_wraps_modulo_1024() {
        // Encoded near the end of the TT cycle, decoded just after wrap.
        let enc = srs_forward(SECRET, DOMAIN, 1020, "act", "alice@example.com").unwrap();
        let local_part = enc.strip_suffix("@fauna.example").unwrap();
        // now_day = 1030 → 10 days later across the 1024 wrap.
        let dec = srs_decode(SECRET, 1030, DEFAULT_SRS_MAX_BOUNCE_AGE_DAYS, local_part).unwrap();
        assert_eq!(dec.original_sender, "alice@example.com");
    }

    #[test]
    fn non_srs_localpart() {
        assert_eq!(srs_decode(SECRET, 0, 28, "alice"), Err(SrsError::NotSrs));
        assert_eq!(
            srs_decode(SECRET, 0, 28, "SRS2=whatever"),
            Err(SrsError::NotSrs)
        );
    }

    #[test]
    fn malformed_field_count() {
        assert_eq!(
            srs_decode(SECRET, 0, 28, "SRS0=ABCD=AA=onlythree"),
            Err(SrsError::Malformed)
        );
    }

    #[test]
    fn forward_rejects_address_without_at() {
        assert_eq!(
            srs_forward(SECRET, DOMAIN, 0, "act", "not-an-address"),
            Err(SrsError::Malformed),
        );
    }

    #[test]
    fn base32_helper_round_trips() {
        for v in [0u32, 1, 31, 1023] {
            assert_eq!(b32_decode(&b32_encode(v, 2), 2), Some(v));
        }
        for v in [0u32, 0xfffff, 12345] {
            assert_eq!(b32_decode(&b32_encode(v, 4), 4), Some(v));
        }
    }
}
