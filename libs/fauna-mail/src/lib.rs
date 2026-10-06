//! `fauna-mail`: shared mail-handling Rust core for the bridge and clients.
//!
//! Wraps `mail-parser` (RFC 5322), `mail-auth` (SPF/DKIM/DMARC/ARC), provides
//! a deterministic Unicode tokenizer for encrypted-search index hints, and
//! ports the bridge daemon's spam-scoring policy. UniFFI-exposed; consumed by
//! all client areas via `libs/fauna-ffi/` and by the future Go bridges.

#[cfg(feature = "uniffi")]
uniffi::setup_scaffolding!("fauna_mail");

// Pure per-account alias validators (+ later the RCPT-TO matcher), per
// docs/goal/behavior/mail-aliases.md. WASM-safe (no tokio/dns/parser).
#[cfg(feature = "aliases")]
pub mod aliases;
#[cfg(feature = "auth")]
pub mod auth;
#[cfg(feature = "parser")]
pub mod bodysection;
#[cfg(feature = "parser")]
pub mod bodystructure;
// Per-domain + nest-host DNS-record body builders for multi-domain mail hosting
// (docs/goal/behavior/mail-multidomain.md § Per-domain DNS records +
// dns-management.md § Records covered). The pure builders live under the
// WASM-safe `dns-records` feature so the onboarding provisioner shares them; the
// `SigningAlg`/MTA-STS-hash extras inside are `#[cfg(feature = "multidomain")]`.
// Accessed via `fauna_mail::dns::{per_domain,host,verify}::*`.
#[cfg(feature = "dns-records")]
pub mod dns;
// Published `_dmarc.<domain>` TXT body assembler (docs/goal/behavior/
// dmarc-reporting.md § Record shape). Pure (serde_json only); WASM-safe — shared
// by the nest DNS surface and the onboarding provisioner so both publish the
// identical body. Distinct from `auth::DmarcPolicy` (inbound verify side).
#[cfg(feature = "dmarc-policy")]
pub mod dmarc_publish;
// Primary-domain-rename state vocabulary (mail-primary-domain-rename.md): the
// pure `RenameState` enum + grace-window bounds + Let's Encrypt SAN cap + the
// TLS-posture monotonicity comparator, shared by the nest storage/RPC layer and
// any future client "rename status" preview (priority #2).
#[cfg(feature = "domain-rename")]
pub mod domain_rename;
#[cfg(feature = "parser")]
pub mod envelope;
// Mailbox-export serializers (docs/goal/behavior/mail-export.md § Format
// choices + § Container shape): mbox / Maildir++ / EML zip, the deterministic
// zip container and the zstd wrapper. Runs on the user's client, never on the
// nest (§ Export pipeline), so it stays WASM-clean.
#[cfg(feature = "mail-export")]
pub mod export;
// Server-side email filter-rule evaluation (docs/goal/behavior/smtp-server.md
// § Email filter rules; docs/goal/behavior/mail-forwarding.md § perimeter eval).
// Pure first-match-wins evaluator; runs at the Go MTA perimeter pre-seal.
#[cfg(feature = "filter")]
pub mod filter;
// Forward-target address validation (docs/goal/behavior/mail-forwarding.md
// :31,:244). Pure std; WASM-safe.
#[cfg(feature = "forward-config")]
pub mod forward_config;
// Forward-loop detection (docs/goal/behavior/mail-forwarding.md § Loop
// detection). Pure std; WASM-safe.
#[cfg(feature = "forward-loop")]
pub mod forward_loop;
// Greylisting tuple-key + defer/pass decision (docs/goal/behavior/
// smtp-server.md § Greylisting). Pure std; nest-side caller only.
#[cfg(feature = "greylist")]
pub mod greylist;
// Pure deliverability-diagnostic primitives (docs/goal/behavior/
// mail-deliverability.md § Symptom diagnostics + § Blocklist self-check): SPF
// lint, DMARC policy-mode parse, MTA-STS/TLSRPT/DKIM presence + DKIM pubkey
// match, DNSBL query-name builder + listed interpreter. PURE std — the async
// DNS/STARTTLS I/O is the nest orchestrator's job; these are the shared verdicts
// (priority #2). Gated `deliverability` (in `default`).
#[cfg(feature = "deliverability")]
pub mod deliverability;
#[cfg(feature = "icalendar")]
pub mod icalendar;
// The shared source-side IMAP client (docs/goal/behavior/mailbox-migration.md
// § Client-driven streaming model): the protocol state machine, the
// per-source-server throttle, batch packing, and cursor resume — generic over
// an AFIT `ImapTransport` byte seam, so the platform shell owns the socket and
// terminates TLS. Client-only: nest and the Go bridges never import from a
// foreign mailbox. Gated `imap-client` (opt-in, not in `default`).
#[cfg(feature = "imap-client")]
pub mod imap_client;
#[cfg(feature = "kind-registry")]
pub mod kind_registry;
#[cfg(feature = "caldav-schedule")]
pub mod scheduling;
// Continuation-aware RFC 5322 header walk shared by `outbound::received_strip`,
// `lists::stamp`, `received_header::strip_fauna_headers`, and
// `aliases::read_spam_threshold_stamp` (one impl, no drift — priority #2).
// Pure std (WASM-safe); gated to its consumers so it never compiles dead —
// which means this list must be exactly the union of the gates its consumers
// carry. `aliases` IS among them again: `read_spam_threshold_stamp` is gated
// on `aliases` since the shared on-device INBOX scorer
// (`fauna-client-mail-settings::inbox_scorer`, every app including the wasm
// web build) reads the stamp too, not only the native Go MDA. The gate has
// failed in BOTH directions before: `aliases` was added here on 2026-08-18
// when the function was ungated, then dropped when the
// function gained a native-only gate, which the on-device
// scorer's adoption of the reader then broke on wasm (unresolved import).
// An `aliases`-only build compiles just `find_body_offset`/`parse_header`
// here (`strip_headers_where` carries its own narrower gate), so nothing is
// dead under `-D dead-code`. `default` turns everything on, so neither
// direction is visible to a default-features build — check a focused
// `--no-default-features --features aliases` build and the wasm chunks.
#[cfg(any(
    feature = "aliases",
    feature = "lists",
    feature = "mail-export",
    feature = "outbound",
    feature = "received-header",
    feature = "sender-auth"
))]
pub(crate) mod header_walk;
// The authenticated-sender delivery stamp: what a filing door verified about
// the sender, written inside the sealed copy for the one consumer that sees
// only raw bytes (the Fauna app's mailed-`REPLY` merge — caldav-server.md
// § Who may mutate an existing event over the inbound rail → *The mail rail*).
// Pure std, WASM-safe; the builder is uniffi-exported for the Go MTA doors.
#[cfg(feature = "sender-auth")]
pub mod sender_auth;
// RFC 5322 §3.6 From-field count, refused on at the mail doors
// (smtp-server.md § Architectural rules). Pure std, WASM-safe, so ungated.
pub mod from_field;
#[cfg(feature = "lists")]
pub mod lists;
#[cfg(feature = "outbound")]
pub mod outbound;
#[cfg(feature = "parser")]
pub mod parser;
#[cfg(feature = "post-text")]
pub mod post_text;
// Inbound trace-header provenance (smtp-server.md § Architectural rules): the
// canonical `Received:` builder (`build_received_header`) and the forged-stamp
// stripper (`strip_fauna_headers` — drops sender copies of the reserved
// `X-Fauna-*` delivery-stamp namespace before the genuine ones are prepended).
// Pure; the I/O (clock + queue-id rand) stays Go-side. Sibling of
// outbound::received_strip. Needs chrono for the RFC 5322 §3.3 date.
#[cfg(feature = "received-header")]
pub mod received_header;
#[cfg(feature = "routing")]
pub mod routing;
#[cfg(feature = "scan")]
pub mod scan;
// Gated on the light `segments-codec` (which `nest-segments` implies): the pure
// `MailRecordEnvelope`/`MailFloorMetadata` codec + bucket/path helpers compile
// for clients without the nest-only `fauna-segment-store` stack. The
// `SegmentManager`-touching `ops`/`placement` submodules stay `nest-segments`-only.
#[cfg(feature = "segments-codec")]
pub mod segments;
// The `spam` module compiles whenever the WASM-safe classifier is wanted
// (`spam-classifier`); the auth-pulling disposition seam inside it is gated on
// the full `spam` feature. `spam` implies `spam-classifier`, so a native
// consumer enabling `spam` gets the whole module unchanged.
#[cfg(feature = "spam-classifier")]
pub mod spam;
// SRS envelope encode/decode for mail forwarding (docs/goal/behavior/
// mail-forwarding.md § SRS). Pure HMAC-SHA-256; WASM-safe (no tokio/dns).
#[cfg(feature = "srs")]
pub mod srs;
// Canonical report-hash for distributed report sharing
// (docs/goal/behavior/report-sharing.md § Content identity). PURE — computed
// at the Go MTA perimeter pre-seal via the UniFFI binding, like `tokenize`.
#[cfg(feature = "report-hash")]
pub mod report_hash;
// Canonical per-actor mail dedup key (docs/goal/behavior/mailbox-migration.md
// § Dedup). PURE — computed where the plaintext lives (Go MDA at APPEND, Go MTA
// pre-seal, the user's client at import) via the UniFFI binding, never nest-side.
#[cfg(feature = "dedup-key")]
pub mod dedup_key;
// The self-describing Fauna Message-ID mint + provenance verify
// (family-safety.md § The mail gate). PURE (sha2 only, WASM-safe);
// `mint_local` takes caller randomness so the wasm compose path brings its
// own getrandom, while the RNG-carrying `new_fauna_msgid_local` rides
// `msgid-mint` (native-only — the Go submission stamp reaches it via the
// UniFFI binding, like `dedup_key`).
#[cfg(feature = "msgid")]
pub mod msgid;
#[cfg(feature = "tokenizer")]
pub mod tokenizer;
// Mail's inline ceilings on the 2 MiB WS-RPC frame (smtp-server.md § Message
// size limits). PURE consts + uniffi getters; ungated — every consumer (Go
// MTA/MDA perimeter, shared import client, nest handlers) reads the same two
// values, so the enforcement can never drift from the frame it protects.
pub mod transport_limits;
// Splitting a sealed mail body across the bulk-byte plane and joining it back
// (smtp-server.md § Message size limits). A body over the inline budget crosses
// as a reference — the ordered blake3 chunk hashes — instead of inline bytes.
// One implementation for all three legs (Go MTA stages, nest rejoins, Go MDA
// re-fetches), so a chunk-boundary or store-key disagreement can't corrupt mail.
#[cfg(feature = "body-ref")]
pub mod body_ref;
// One-shot AEAD envelope for the PLAINTEXT legs' staged bytes (client import,
// the outbound queue pair) — plaintext must never enter the open-download
// chunk store, so these legs seal here before splitting with body_ref's rule
// (smtp-server.md § Message size limits, the staged-envelope rule).
#[cfg(feature = "staged-envelope")]
pub mod staged_envelope;
// Pure fresh-IP outbound warm-up schedule (docs/goal/behavior/
// mail-deliverability.md § Fresh-IP warm-up → § The ramp): the day→max-mails
// cap curve. PURE std only — the day-counter state + submission-time
// enforcement are nest-side. Gated `warmup` (in `default`; in nest's list).
#[cfg(feature = "warmup")]
pub mod warmup;
// Pure mail-health fold for the admin readout (docs/goal/behavior/
// mail-deliverability.md § The mail health readout): worst-wins state + the
// seven check rows over nest facts. PURE std; the nest gathers the inputs and
// calls it. Gated `health` (in `default`; in nest's list).
#[cfg(feature = "health")]
pub mod health;

#[cfg(feature = "aliases")]
pub use aliases::{
    AliasValidationError, DEFAULT_RESERVED_LOCAL_PARTS, EXACT_ALIASES_MAX_DEFAULT,
    MAX_LOCAL_PART_LEN, is_reserved_local_part, validate_exact_local_part,
};
#[cfg(feature = "auth")]
pub use auth::{
    ArcVerdict, AuthError, AuthVerdicts, DkimVerdict, DmarcPolicy, DmarcVerdict, SpfVerdict,
    verify_inbound,
};
#[cfg(feature = "parser")]
pub use bodysection::{
    BinarySectionSpec, BodySectionPartial, BodySectionSpec, fetch_binary_section,
    fetch_binary_size, fetch_body_section,
};
#[cfg(feature = "parser")]
pub use bodystructure::{BodyStructure, MimeParam, derive_body_structure};
#[cfg(feature = "dmarc-policy")]
pub use dmarc_publish::{
    DmarcAlignment, DmarcForensicOptions, DmarcMode, DmarcOverrides, DmarcPublishPolicy,
    apply_overrides, apply_overrides_json,
};
#[cfg(feature = "domain-rename")]
pub use domain_rename::{
    DEFAULT_GRACE_DAYS, GRACE_DAYS_MAX, GRACE_DAYS_MIN, LETSENCRYPT_SAN_LIMIT, RenameState,
    tls_posture_rank,
};
#[cfg(feature = "parser")]
pub use envelope::{Envelope, EnvelopeAddress, derive_envelope};
#[cfg(feature = "filter")]
pub use filter::{
    FilterAction, FilterCombination, FilterCondition, FilterContext, FilterHeader, FilterMatch,
    StoredFilter, evaluate,
};
#[cfg(feature = "forward-config")]
pub use forward_config::{
    FORWARD_MAX_PER_ACCOUNT_PER_HOUR_CEILING, FORWARD_PER_HOUR_DEFAULT,
    FORWARD_QUEUE_CEILING_MULTIPLIER, ForwardPerHourError, ForwardTargetError,
    validate_forward_per_hour, validate_forward_target,
};
#[cfg(feature = "forward-loop")]
pub use forward_loop::{
    HEADER_FORWARDED_BY, MAX_RECEIVED_HOPS, forwarded_by_value, parse_forwarded_by_actor,
    received_chain_exceeded, self_already_forwarded,
};
#[cfg(feature = "icalendar")]
pub use icalendar::{
    ExpandedOccurrence, ICalComponent, ICalDocument, ICalError, ICalParameter, ICalProperty,
    expand_recurrence, parse_icalendar,
};
#[cfg(feature = "kind-registry")]
pub use kind_registry::{KindMetadata, lookup_kind};
#[cfg(feature = "parser")]
pub use parser::{
    ParseError, ParsedHeader, ParsedMessage, ParsedMimePart, TextCalendarPart,
    extract_text_calendar_part, parse_rfc5322,
};
#[cfg(feature = "post-text")]
pub use post_text::{SearchableText, extract_post_searchable_text};
#[cfg(feature = "routing")]
pub use routing::{merge_recipients, partition_recipients};
#[cfg(feature = "scan")]
pub use scan::{
    ClamavAction, ClamavVerdict, RspamdRuleContribution, RspamdScore, ScanAction, ScanError,
    ScanPolicy, clamd_parse_reply, decide_scan_action, rspamd_parse_reply,
};
#[cfg(feature = "segments-receive")]
pub use segments::receive::{
    InboundOpenError, open_inbound_record, open_inbound_record_epoch,
    open_inbound_record_epoch_hybrid, open_inbound_record_hybrid, open_inbound_record_with_keys,
    open_sealed_inner_record, open_sealed_inner_record_hybrid, open_sealed_inner_record_with_keys,
};
// WASM-safe surface: the classifier (reached as `fauna_mail::spam::SpamModel`
// via the module's own `pub use classifier::*`), plus the auth-free disposition
// primitives. `decide_spam_disposition` needs `crate::auth`, so it stays under
// the full `spam` feature below.
#[cfg(feature = "dedup-key")]
pub use dedup_key::{
    MailDedupKeyPair, envelope_keys_agree, mail_dedup_keys, mail_dedup_keys_from_slice,
    normalize_message_id, require_dedup_pair,
};
#[cfg(feature = "report-hash")]
pub use report_hash::report_hash;
#[cfg(feature = "spam")]
pub use spam::decide_spam_disposition;
#[cfg(feature = "spam-classifier")]
pub use spam::{SpamDisposition, SpamPolicy, combined_spam_score_milli};
#[cfg(feature = "srs")]
pub use srs::{
    DEFAULT_SRS_MAX_BOUNCE_AGE_DAYS, SrsDecoded, SrsError, TT_MODULUS_DAYS, srs_decode, srs_forward,
};
#[cfg(feature = "tokenizer")]
pub use tokenizer::{CanonicalTokenSet, PositionalToken, tokenize, tokenize_positional};
