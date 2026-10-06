//! Mail's inline ceilings on the WS-RPC frame.
//!
//! Owner doc: `docs/goal/behavior/smtp-server.md` § Message size limits (cap
//! rationale: `docs/goal/architecture/transport.md` § Max frame). The 2 MiB
//! `fauna_core::transport::MAX_RPC_WS_MESSAGE_SIZE` is permanent for every
//! caller class; a sealed mail body above the inline ceiling rides the
//! bulk-byte plane as a reference, and — since the 2026-07-18 continuation-write
//! flip — rests as frame-sized continuation records rather than one CARv2
//! record, so no body is "too large to rest" at any size.
//!
//! **The at-rest ceiling is retired.** The product ceiling is now
//! `max_message_bytes` (the admin knob) *alone*, computed by
//! [`effective_max_raw_message_bytes`] — the single source the SMTP `Data`
//! paths, the EHLO `SIZE` advertisement, nest-side import/APPEND admission, and
//! `fauna.email.send` all read (uniffi-exported), so enforcement can never
//! drift. The former `MAX_SEALED_BODY_AT_REST_BYTES` / `MAX_RAW_MESSAGE_BYTES_AT_REST`
//! pair was deleted with ceiling retirement (continuation records dissolved the
//! single-record ceiling they encoded).

/// Largest raw RFC 5322 message whose per-recipient sealed copy still rides
/// WS-RPC inline. This is the inline/reference **switchover**, not a perimeter
/// ceiling: a larger body is not refused — it crosses the bulk-byte plane by
/// reference. The perimeter ceiling is `max_message_bytes` alone
/// ([`effective_max_raw_message_bytes`]).
///
/// Sized so a sealed copy (raw + per-recipient HPKE/X-Wing overhead) plus a
/// typical encrypted index hint fits [`INLINE_MAIL_REQUEST_BUDGET_BYTES`]
/// with margin. The hint is input-dependent (a unique-word-dense body grows
/// it toward the body's own size), which is why the precise post-seal guard
/// below exists in addition to this pre-parse value.
pub const MAX_INLINE_RAW_MESSAGE_BYTES: u32 = 1_500_000;

/// Byte budget for the sealed body + sealed index hint of a single
/// mail-carrying WS-RPC request (`ingest_inbound_mail` / `append` /
/// `import_message` / `submit_inbound_mail`): the 2 MiB frame minus a 64 KiB
/// allowance for every other request field plus the frame envelope. A request
/// whose two ciphertexts exceed this cannot cross the wire — the transport
/// would sever the connection — so the producer must refuse it *permanently*
/// (`552 5.3.4` at the SMTP perimeter) before calling.
///
/// Value is `MAX_RPC_WS_MESSAGE_SIZE - 64 KiB` — pinned by unit test against
/// `fauna_core::transport::MAX_RPC_WS_MESSAGE_SIZE` (a dev-dependency here;
/// the prod dep is optional and this module must not require it).
pub const INLINE_MAIL_REQUEST_BUDGET_BYTES: u32 = 2 * 1024 * 1024 - 64 * 1024;

/// Largest **part** a continuation writer emits — a raw ciphertext range of one
/// sealed body (`message-segment-store.md` § Continuation records;
/// `smtp-server.md` § Message size limits). An over-cap sealed body is split
/// into consecutive ranges of at most this many bytes, each stored as its own
/// segment record, so that **every** stored record (parts and the v3 head) is
/// frame-sized and the nest↔nest relay can forward it inside the 2 MiB
/// federation frame (`deployment-home-with-public-relay.md` § Relay frame
/// budget, rule 2).
///
/// 1 MiB, sized well under [`INLINE_MAIL_REQUEST_BUDGET_BYTES`] (2,031,616): a
/// part's full relay wire tuple is the raw range bytes + the canonical
/// `MailFloorMetadata` (a few hundred bytes) + the per-record framing overhead,
/// which at 1 MiB leaves ~950 KiB of headroom. Unlike the at-rest inline
/// envelope, a part pays **no** array-of-integers expansion — its bytes are
/// stored verbatim as one CARv2 block — so the range size *is* the record size.
/// Nest-internal (the split happens at the `append_record` choke point); not a
/// uniffi surface.
pub const MAIL_BODY_PART_CAP_BYTES: u32 = 1024 * 1024;

/// The shipped default `max_message_bytes` (50 MB) — the value
/// [`effective_max_raw_message_bytes`] returns for a `0` (no-snapshot-yet) knob.
///
/// Mirrors `fauna_protocol`'s `SpamPolicyThresholds::default().max_message_bytes`
/// (this crate cannot depend on `fauna-protocol` without a dependency cycle, so
/// the value is duplicated here — both sites cite each other, and a unit test
/// below pins them against drift via a dev-dependency). It exists **only**
/// so a missing snapshot never yields an *uncapped* perimeter: go-smtp treats a
/// `0` `MaxMessageBytes` as unlimited, so `0` must fall back to a real bound.
/// Before ceiling retirement the `0` knob fell back to the (now-deleted) at-rest
/// ceiling; the product default is its safe replacement.
const DEFAULT_MAX_MESSAGE_BYTES: u32 = 50_000_000;

/// Upper bound on the admin's `max_message_bytes` product ceiling: 250,000,000
/// (250 MB, decimal like the knob itself). Ruled 2026-08-26; owner
/// `docs/goal/behavior/mail-message-size.md` § Message size limits.
///
/// **Why a bound exists at all.** The ClamAV gate scans *everything the
/// perimeter accepts* — its cap is derived from this same knob, never chosen
/// separately (`mail-content-scanning.md` § Oversize messages) — and the scan
/// sidecar's own stream/scan/file limits are shipped by the deployment bundle
/// at a fixed value at or above this constant. So a ceiling the scanner cannot
/// be configured to cover is not a ceiling the product supports: bounding the
/// knob here is what keeps "no accepted message goes unscanned" true at *every*
/// admin setting rather than only at the shipped default.
///
/// 250 MB clears every mainstream provider's attachment limit with headroom
/// while keeping one message's scan cost bounded; larger transfers belong to
/// the file plane, not SMTP.
///
/// Enforced at the write: the nest's `put_spam_policy` refuses a larger value
/// with `fauna.protocol.malformed`, surfaced in all 7 apps through
/// `MailPolicySnapshot::error` (the precedent is the Bayesian-ramp refusal),
/// so no stored knob exceeds it.
pub const MAX_MESSAGE_BYTES_CEILING: u32 = 250_000_000;

/// Bytes the seal adds to a raw body, generously bounded.
///
/// A sealed body is canonical dag-cbor over `fauna_mls::wrapped_blob::HpkeWire`,
/// whose `enc` and `ct` are `ByteBuf` (i.e. real CBOR byte strings — the payload
/// is *not* subject to the array-of-integers expansion that inflates the at-rest
/// segment envelope). So the seal costs a small constant, independent of body
/// size: the encapsulated key (32 bytes classical X25519, ~1,120 for X-Wing's
/// ML-KEM-768) plus the Poly1305 tag plus the map framing — comfortably under
/// 2 KiB.
///
/// Its use is to convert the raw product ceiling into a **sealed-body** bound for
/// a leg that can only see the sealed bytes. Nest's APPEND admission
/// (`bridge_imap_handlers::append_message_handler`) has only the sealed
/// `ciphertext_size`, never the raw literal, so it admits up to
/// `effective_max_raw_message_bytes(knob) + SEAL_ENVELOPE_ALLOWANCE_BYTES` — any
/// raw message within the product ceiling whose seal grew it by at most this
/// much. 64 KiB is deliberate over-provisioning; with the at-rest ceiling retired
/// the erring directions inverted from what they once were: erring high now only
/// admits a sliver *over* the raw ceiling (harmless — continuation records rest
/// any size), while erring low would refuse a raw message legitimately within it.
/// Pinned falsifiable by `mail_body_ref_round_trip.rs::
/// the_seal_allowance_covers_what_the_seal_actually_adds`.
pub const SEAL_ENVELOPE_ALLOWANCE_BYTES: u32 = 64 * 1024;

/// The effective SMTP perimeter ceiling: the admin's `max_message_bytes` knob,
/// and nothing else.
///
/// Since ceiling retirement (2026-07-18 continuation-write flip + this slice)
/// the product ceiling is `max_message_bytes` *alone* — there is no longer an
/// at-rest limit to clamp against, because continuation records rest a body of
/// any size. This function remains the single source the SMTP `Data` paths, the
/// EHLO `SIZE` advertisement, nest-side import/APPEND admission, and
/// `fauna.email.send` read, so those five enforcement points can never disagree
/// on the ceiling (`docs/goal/behavior/smtp-server.md` § Message size limits).
///
/// A `0` knob (no snapshot yet) does **not** mean uncapped: go-smtp treats a `0`
/// `MaxMessageBytes` as unlimited, so `0` falls back to [`DEFAULT_MAX_MESSAGE_BYTES`]
/// (the shipped product default), never to 0 and never to ∞.
///
/// Every other knob is returned as is: the nest refuses a write above
/// [`MAX_MESSAGE_BYTES_CEILING`], so no stored knob exceeds it.
#[cfg_attr(feature = "uniffi", uniffi::export)]
pub fn effective_max_raw_message_bytes(max_message_bytes: u32) -> u32 {
    if max_message_bytes == 0 {
        return DEFAULT_MAX_MESSAGE_BYTES;
    }
    max_message_bytes
}

/// Uniffi getter for [`MAX_INLINE_RAW_MESSAGE_BYTES`] (Go/Kotlin/Swift/C#
/// bindings cannot import Rust consts).
#[cfg_attr(feature = "uniffi", uniffi::export)]
pub fn max_inline_raw_message_bytes() -> u32 {
    MAX_INLINE_RAW_MESSAGE_BYTES
}

/// Uniffi getter for [`INLINE_MAIL_REQUEST_BUDGET_BYTES`].
#[cfg_attr(feature = "uniffi", uniffi::export)]
pub fn inline_mail_request_budget_bytes() -> u32 {
    INLINE_MAIL_REQUEST_BUDGET_BYTES
}

/// Uniffi getter for [`MAX_MESSAGE_BYTES_CEILING`] — the Go mail bridge reads it
/// to pin the scan sidecar's shipped limits against the same number the nest
/// refuses above.
#[cfg_attr(feature = "uniffi", uniffi::export)]
pub fn max_message_bytes_ceiling() -> u32 {
    MAX_MESSAGE_BYTES_CEILING
}

// These tests are compile-time-constant tripwires ON PURPOSE — each pins a
// relationship between limits that a "harmless" constant edit would silently
// break (the doc-comment on each names the regression it guards).
#[allow(clippy::assertions_on_constants)]
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn budget_is_the_rpc_frame_minus_the_envelope_allowance() {
        assert_eq!(
            INLINE_MAIL_REQUEST_BUDGET_BYTES as usize,
            fauna_core::transport::MAX_RPC_WS_MESSAGE_SIZE - 64 * 1024,
        );
    }

    #[test]
    fn a_part_plus_its_floor_and_framing_fits_the_relay_frame() {
        // rule 2 of the relay frame budget: every stored record (a continuation
        // part included) must fit one 2 MiB federation frame. A part's wire
        // tuple is the raw range bytes + the canonical MailFloorMetadata + the
        // per-record framing overhead. Bound the floor + overhead generously at
        // 64 KiB — orders of magnitude above a real floor — and require the sum
        // to stay under the assembled-page budget. If this fails, the part cap
        // was raised past what the relay can carry.
        const FLOOR_AND_FRAMING_BOUND: u32 = 64 * 1024;
        assert!(
            MAIL_BODY_PART_CAP_BYTES + FLOOR_AND_FRAMING_BOUND < INLINE_MAIL_REQUEST_BUDGET_BYTES,
            "a part ({MAIL_BODY_PART_CAP_BYTES}) + floor/framing must fit the relay frame \
             budget ({INLINE_MAIL_REQUEST_BUDGET_BYTES}) — see \
             deployment-home-with-public-relay.md § Relay frame budget rule 2",
        );
    }

    #[test]
    fn default_matches_the_protocol_spam_policy_default() {
        // DEFAULT_MAX_MESSAGE_BYTES is a documented duplicate of
        // fauna_protocol::SpamPolicyThresholds::default().max_message_bytes
        // (this crate's unconditional transport_limits module cannot take a
        // prod dep on fauna-protocol without pulling it into every build,
        // wasm32 included) — this pin is the drift guard the doc comment
        // promises.
        assert_eq!(
            DEFAULT_MAX_MESSAGE_BYTES,
            fauna_protocol::bridge_routing::SpamPolicyThresholds::default().max_message_bytes,
        );
    }

    #[test]
    fn the_perimeter_no_longer_clamps_to_the_inline_ceiling() {
        // The point of the reference legs. Before them the perimeter refused
        // anything over the inline ceiling with a 552, because an over-frame body
        // could not cross at all. Now it crosses by reference, so the inline
        // ceiling is a switchover, not a perimeter value.
        //
        // If this ever fails, someone has re-pinned the perimeter to the frame and
        // silently reintroduced the 1.5 MB mail limit.
        assert!(
            effective_max_raw_message_bytes(DEFAULT_MAX_MESSAGE_BYTES)
                > MAX_INLINE_RAW_MESSAGE_BYTES,
            "the effective ceiling must exceed the inline ceiling \
             ({MAX_INLINE_RAW_MESSAGE_BYTES}) — a body above the inline ceiling rides the \
             bulk-byte plane, it is not refused",
        );
    }

    #[test]
    fn an_unset_knob_falls_back_to_the_product_default_not_uncapped() {
        // go-smtp treats a 0 MaxMessageBytes as *unlimited*, so a missing snapshot
        // must fall back to a real bound — the shipped product default — never 0
        // and never ∞. (Before ceiling retirement this fell back to the at-rest
        // ceiling; that ceiling is gone.)
        assert_eq!(
            effective_max_raw_message_bytes(0),
            DEFAULT_MAX_MESSAGE_BYTES
        );
    }

    #[test]
    fn the_knob_is_the_ceiling_now() {
        // Since ceiling retirement the product ceiling is max_message_bytes alone:
        // the function is the identity on any non-zero knob, with no at-rest
        // clamp, so the shipped 50 MB default is genuinely deliverable. The
        // knob's upper bound is the nest's write refusal (a knob above
        // MAX_MESSAGE_BYTES_CEILING is never stored), not a read clamp.
        assert_eq!(effective_max_raw_message_bytes(1_000), 1_000);
        assert_eq!(effective_max_raw_message_bytes(50_000_000), 50_000_000);
        assert_eq!(
            effective_max_raw_message_bytes(MAX_MESSAGE_BYTES_CEILING),
            MAX_MESSAGE_BYTES_CEILING
        );
    }

    #[test]
    fn the_ceiling_is_above_the_shipped_default_and_fits_u32() {
        // Two ways the ruling's number could be broken by a "harmless" edit:
        // dropping it below the shipped default would clamp every stock
        // deployment's own default (silently shrinking the product), and a value
        // past u32::MAX would not survive the wire type
        // (SpamPolicyThresholds.max_message_bytes is u32).
        assert!(
            MAX_MESSAGE_BYTES_CEILING > DEFAULT_MAX_MESSAGE_BYTES,
            "the ceiling ({MAX_MESSAGE_BYTES_CEILING}) must leave the shipped default              ({DEFAULT_MAX_MESSAGE_BYTES}) reachable",
        );
        assert_eq!(MAX_MESSAGE_BYTES_CEILING, 250_000_000);
        assert_eq!(max_message_bytes_ceiling(), MAX_MESSAGE_BYTES_CEILING);
    }

    #[test]
    fn ceiling_leaves_seal_and_hint_headroom_inside_the_budget() {
        // A ceiling-sized raw body must fit the budget with at least ~25%
        // headroom for the seal overhead + a typical index hint. The
        // pathological hint (unique-word-dense body) is covered by the
        // precise post-seal guard, not this margin.
        assert!(
            MAX_INLINE_RAW_MESSAGE_BYTES + MAX_INLINE_RAW_MESSAGE_BYTES / 4
                < INLINE_MAIL_REQUEST_BUDGET_BYTES
        );
    }
}
