//! Per-account alias validation (and, in a later slice, the RCPT-TO
//! resolver matcher) — the shared core behind
//! `docs/goal/behavior/mail-aliases.md`.
//!
//! This module is **pure**: no tokio, no DNS, no `mail-parser`. It is
//! WASM-safe so a client SPA can pre-validate an alias pattern in the
//! add-sheet (the doc's "kind-aware validation", § Account-detail Aliases
//! section UX) using the *same* predicates the nest `create_account_alias`
//! handler enforces — priority #2 (write the rule once, share it).
//!
//! Slice A2.1 ships the exact-kind
//! create-time validators: the strict ASCII character class, the RFC-5321
//! local-part length ceiling, and the uncircumventable reserved-local-part
//! set. The fixed-order resolver (exact → +suffix → disposable → wildcard
//! → catch-all → 550) and the wildcard-prefix conflict checks extend this
//! module in A2.2.

use thiserror::Error;

/// Per-domain role-address override map (`mail_domains.role_address_overrides`).
/// `role-overrides`-gated (needs only serde_json + hex — WASM-safe, so the
/// client picker shares it; `multidomain` implies the feature for the nest). The
/// always-pure `classify_role_address` below stays ungated. See the module doc.
#[cfg(feature = "role-overrides")]
pub mod role_overrides;

/// Canonical `account_aliases.kind` strings — the single source of truth for
/// the kind discriminator shared between the nest DB layer (which re-exports
/// these), the WS-RPC handlers, and this matcher. `catchall` is intentionally
/// absent: the catch-all is a policy key (`mail.inbound.catch_all_actor` /
/// per-domain `mail_domains.catch_all_actor_id`), never an `account_aliases`
/// row (`mail-aliases.md:198`).
pub const ALIAS_KIND_EXACT: &str = "exact";
pub const ALIAS_KIND_WILDCARD_PREFIX: &str = "wildcard_prefix";
/// Minted by `generate_disposable_alias` (no production writer until A2.3).
pub const ALIAS_KIND_DISPOSABLE: &str = "disposable";
/// Admin-configured external forwarder (`mail-aliases.md` § Kind 7): an
/// address with no local mailbox whose inbound is forwarded to an external
/// `forward_target`, attributed to the managing admin actor. Created via the
/// Admin-class `create_forwarder` RPC, never the user alias CRUD; resolves at
/// the exact-key tier (§ Resolution order step 2) to a [`RecipientResolution::
/// Forward`] outcome rather than a local route.
pub const ALIAS_KIND_FORWARDER: &str = "forwarder";
/// Mailing list (`docs/goal/behavior/mail-mass-mailing.md` § List as a sixth
/// alias kind): an outbound-only address (`<list-name>@<our-domain>`) whose
/// `mail_lists` row drives RFC 8058 list-mode submission. Created via the
/// User-class `create_account_list` RPC, never the generic alias CRUD. Inbound
/// mail to a list address rejects (`550` — lists are one-way outbound); a
/// MAIL FROM matching a `list` row triggers the list-mode submission pipeline
/// (header stamping + per-list rate accounting).
pub const ALIAS_KIND_LIST: &str = "list";

/// Resolution-order feature defaults, the exact-alias cap, and the
/// reserved-local-part set — **one definition**, in
/// [`fauna_core::mail_aliases`], which owns the wire-shape rationale
/// (`fauna_protocol::bridge_routing::AliasPolicy`'s `Default` impl used to
/// hand-copy these four literals, since `fauna-protocol` cannot depend on
/// `fauna-mail`). Re-exported here so every `fauna_mail::aliases::*_DEFAULT`
/// path keeps working.
pub use fauna_core::mail_aliases::{
    DEFAULT_RESERVED_LOCAL_PARTS, EXACT_ALIASES_MAX_DEFAULT, SUBADDRESSING_ENABLED_DEFAULT,
    WILDCARD_PREFIX_ENABLED_DEFAULT,
};

/// The RFC 5233 sub-address separator (`bob+work@` → base `bob`, suffix `work`).
pub const SUBADDRESS_SEPARATOR: char = '+';

/// Minimum characters before a wildcard prefix's trailing `-`
/// (`mail-aliases.md` § Don't `:333` — `b-*` is too greedy).
pub const MIN_WILDCARD_PREFIX_LEAD: usize = 2;

/// `X-Fauna-Address-*` headers the resolver asks the MDA to stamp before the
/// message hits the user's filter rules (`mail-aliases.md` § Kind 2/3/4/5).
/// Exact matches stamp nothing.
pub const HEADER_ADDRESS_SUFFIX: &str = "X-Fauna-Address-Suffix";
pub const HEADER_WILDCARD_SUFFIX: &str = "X-Fauna-Address-Wildcard-Suffix";
pub const HEADER_CATCHALL: &str = "X-Fauna-Address-Catchall";
/// Stamped alongside [`HEADER_CATCHALL`] on a catch-all match, valued with the
/// RCPT domain the catch-all fired on (`mail-multidomain.md` § On match at RCPT
/// TO). Lets the receiving actor's filters distinguish which of the deployment's
/// domains caught the mail (e.g. low-priority a community-domain catch-all).
pub const HEADER_CATCHALL_DOMAIN: &str = "X-Fauna-Address-Catchall-Domain";
/// Disposable-match header — carries the matched token (`mail-aliases.md`
/// § Kind 5 `:99`). Header-only, **never** body-stamped (§ Don't `:330`):
/// the token is public (it's in the address the sender used) but body-stamping
/// would leak it to a body-scanning recipient.
pub const HEADER_DISPOSABLE: &str = "X-Fauna-Address-Disposable";

/// The delivery-time spam-threshold stamp (`mail-aliases.md` § Spam-threshold
/// override — "nest folds alias > account > default at ingest into a single
/// per-message threshold, stamped into the message as an `X-Fauna-*`
/// delivery-stamp"). Valued with [`resolve_delivery_spam_threshold`]'s result in
/// whole spam-points, and read back by the MDA's SELECT-time scorer as *that
/// message's* `spam_folder` tier. Deliberately NOT an `X-Fauna-Address-*` name:
/// it is a delivery-policy fact, not an alias-match fact — but it shares the
/// `X-Fauna-` namespace, so the forgery strip every filing door runs already
/// covers it by prefix (the export's transit strip deliberately leaves the
/// namespace alone — `mail-export.md` § UX shape step 2).
pub const HEADER_SPAM_THRESHOLD: &str = "X-Fauna-Spam-Threshold";

/// Fold the three spam-threshold tiers into the ONE number that rides with a
/// message (`mail-aliases.md:153` — "Resolution at delivery time: per-alias
/// override > per-account override > admin-tier default").
///
/// `0` is a meaningful resolved value, not "unset": it is the disabled tier
/// (`mail-spam.md` § Pipeline step 4 — a `0` `spam_folder` means no auto-Junk
/// routing), which is why the tiers are `Option`s and only `None` inherits.
pub fn resolve_delivery_spam_threshold(
    alias_override: Option<u32>,
    account_override: Option<u32>,
    admin_default: u32,
) -> u32 {
    alias_override.or(account_override).unwrap_or(admin_default)
}

/// Prepend the resolver's `(name, value)` delivery stamps to a raw RFC 5322
/// message, returning the copy that gets sealed for that recipient.
///
/// The Rust twin of the Go MTA's `prependHeaders` + `stampHeaderLines` pair
/// (`bins/fauna-bridges/internal/mta/scan_gate.go`), for the nest-side delivery
/// paths that seal a local copy themselves — `fauna.email.send`'s in-domain half
/// and the `enqueue_outbound_mail` partition. Empty stamps return the message
/// untouched, so a system-generated sender (a DSN, a security notification) pays
/// nothing and copies nothing.
///
/// CRLF-terminated per RFC 5322 §2.2, and prepended — never appended — so the
/// genuine stamp precedes any same-named header further down and
/// [`read_spam_threshold_stamp`]'s first-match wins on it.
pub fn prepend_stamped_headers(raw_message: &[u8], stamps: &[(String, String)]) -> Vec<u8> {
    if stamps.is_empty() {
        return raw_message.to_vec();
    }
    let mut out = Vec::with_capacity(raw_message.len() + stamps.len() * 64);
    for (name, value) in stamps {
        out.extend_from_slice(name.as_bytes());
        out.extend_from_slice(b": ");
        out.extend_from_slice(value.as_bytes());
        out.extend_from_slice(b"\r\n");
    }
    out.extend_from_slice(raw_message);
    out
}

/// Read the [`HEADER_SPAM_THRESHOLD`] stamp back off a delivered message — the
/// consuming half of the fold, used by the MDA's SELECT-time scorer to learn
/// *this message's* `spam_folder` tier instead of re-resolving the alias chain
/// (`mail-aliases.md` § Spam-threshold override — the rejected reading).
///
/// `None` means "no stamp": mail delivered before this shipped, or a copy that
/// never went through the resolver. The caller falls back to its session
/// policy there — never to a hard-coded number.
///
/// Lives here, in shared Rust, rather than as a Go-side header scan, so the
/// stamper and the reader cannot drift on the field name or the parse. The
/// match is case-insensitive on the field name (RFC 5322 §2.2.3) and the value
/// must be a plain non-negative integer of whole spam-points; anything else
/// reads as `None` (fail-safe to the session policy rather than to `0`, which
/// would silently disable Junk routing for that message). The FIRST occurrence
/// wins: the genuine stamp is prepended at delivery, ahead of any body-supplied
/// duplicate, and the inbound forgery strip has already removed sender-supplied
/// `X-Fauna-*` headers anyway.
///
/// Two consumers: the Go MDA's SELECT-time scorer
/// (`mailfauna.ReadSpamThresholdStamp`, via `fauna-ffi`) and the shared
/// on-device INBOX scorer (`fauna-client-mail-settings::inbox_scorer`, every
/// app including the wasm web build). So it compiles wherever its `aliases`
/// module does; `header_walk` (pure std) lists `aliases` among its gates for
/// exactly this caller (`lib.rs`, the gate's comment).
pub fn read_spam_threshold_stamp(raw_message: &[u8]) -> Option<u32> {
    use crate::header_walk::{find_body_offset, parse_header};
    let body_offset = find_body_offset(raw_message).unwrap_or(raw_message.len());
    let header_section = &raw_message[..body_offset];

    let mut i = 0;
    while i < header_section.len() {
        let (header_end, name) = parse_header(&header_section[i..]);
        if header_end == 0 {
            break; // Walker stalled — fail safe to "no stamp".
        }
        if name
            .trim_ascii()
            .eq_ignore_ascii_case(HEADER_SPAM_THRESHOLD.as_bytes())
        {
            let field = &header_section[i..i + header_end];
            let colon = field.iter().position(|b| *b == b':')?;
            let value = &field[colon + 1..];
            return std::str::from_utf8(value).ok()?.trim().parse::<u32>().ok();
        }
        i += header_end;
    }
    None
}

/// The per-message threshold a fired `Allow` filter rule stamps: the disabled
/// tier, so no post-delivery scorer re-files the message to Junk
/// (`email-filters.md` § Multi-action composition — `Allow` overrides the spam
/// disposition at every scoring position, not only at delivery).
pub const FILTER_ALLOW_SPAM_THRESHOLD: u32 = 0;

/// Carry a fired `Allow` past delivery: return the recipient's stamped copy
/// with every [`HEADER_SPAM_THRESHOLD`] field removed from its header section
/// and ONE `X-Fauna-Spam-Threshold: 0` ([`FILTER_ALLOW_SPAM_THRESHOLD`])
/// prepended in their place.
///
/// The override REPLACES the resolver's RCPT-time stamp rather than sitting
/// beside it, because [`read_spam_threshold_stamp`] takes the first match —
/// and so both post-delivery scorers (the MDA's SELECT-time pass and the
/// shared on-device INBOX scorer) read `0` and keep the message in INBOX. The
/// body is copied verbatim. Called by the Go MTA on the copy it seals for a
/// recipient whose final placement action was `Allow` (`fauna-ffi`
/// `stamp_filter_allow`), which is where `Allow` is decided (after DATA).
pub fn stamp_filter_allow(raw_message: &[u8]) -> Vec<u8> {
    use crate::header_walk::{find_body_offset, parse_header};
    let body_offset = find_body_offset(raw_message).unwrap_or(raw_message.len());
    let stamp = format!("{HEADER_SPAM_THRESHOLD}: {FILTER_ALLOW_SPAM_THRESHOLD}\r\n");
    let mut out = Vec::with_capacity(raw_message.len() + stamp.len());
    out.extend_from_slice(stamp.as_bytes());
    let mut i = 0;
    while i < body_offset {
        let (header_end, name) = parse_header(&raw_message[i..body_offset]);
        if header_end == 0 {
            break;
        }
        if !name
            .trim_ascii()
            .eq_ignore_ascii_case(HEADER_SPAM_THRESHOLD.as_bytes())
        {
            out.extend_from_slice(&raw_message[i..i + header_end]);
        }
        i += header_end;
    }
    out.extend_from_slice(&raw_message[i..]);
    out
}

// ── Disposable-alias shape + defaults (`mail-aliases.md` § Kind 5) ──────

/// The recognizable middle segment of a disposable address
/// `<handle>-temp-<token>@<domain>` (`mail-aliases.md` § Kind 5 `:80`,
/// § Don't `:339`). Deployment-wide fixed, never user-customizable — it is
/// the downstream-filtering tag that lets a recipient's MUA spot disposables.
pub const DISPOSABLE_INFIX: &str = "-temp-";

/// Disposable token length — 6 base32 chars = 32^6 ≈ 1 G possibilities
/// (`mail-aliases.md` § Don't `:336`). The per-day generate cap +
/// mint-time collision check guard against enumeration.
pub const DISPOSABLE_TOKEN_LEN: usize = 6;

/// Lowercase RFC 4648 base32 alphabet (`A-Z2-7` → lowercased) the mint draws
/// the token from. Lookup is case-insensitive ([`is_base32_token`] /
/// [`split_disposable`] accept either case); the row stores the lowercased
/// token (the resolver lower-cases the RCPT local-part before matching).
pub const DISPOSABLE_TOKEN_ALPHABET: &[u8] = b"abcdefghijklmnopqrstuvwxyz234567";

/// Disposable mint defaults (`mail-aliases.md` § Kind 5 `:84-88`). These are
/// per-user Tier-3 knobs (`mail.account.disposable_*`) the A3 audit placed in
/// **Bucket C** (neither projected nor admin-writable today) — enforced as
/// constants here until the policy write-path lands the knobs, exactly like
/// [`fauna_core::mail_aliases::EXACT_ALIASES_MAX_DEFAULT`].
///
/// `DISPOSABLE_DEFAULT_TTL_DAYS` — the outer time bound (`expires_at =
/// created_at + ttl`). `DISPOSABLE_DEFAULT_USES` — 1 use (first inbound
/// routes, then dead); a mint param of `0` means **unlimited** (stored as a
/// `NULL` `uses_remaining`, see [`disposable_alive`]).
/// `DISPOSABLE_GENERATE_PER_DAY_DEFAULT` — the anti-enumeration mint cap
/// (`mail-aliases.md` § Don't `:327`; admin ceiling
/// `mail.outbound.disposable_generate_max_per_day` = 200 is Bucket-C too).
/// `DISPOSABLE_RATE_LIMIT_PER_DAY_DEFAULT` — the hard inbound rate cap every
/// disposable carries (`mail-aliases.md` § Per-alias controls `:145`).
pub const DISPOSABLE_DEFAULT_TTL_DAYS: u32 = 30;
pub const DISPOSABLE_DEFAULT_USES: u32 = 1;
pub const DISPOSABLE_GENERATE_PER_DAY_DEFAULT: u32 = 50;
pub const DISPOSABLE_RATE_LIMIT_PER_DAY_DEFAULT: i64 = 100;

/// RFC 5321 §4.5.3.1.1 local-part length ceiling. Patterns longer than this
/// are rejected at create time.
pub const MAX_LOCAL_PART_LEN: usize = 64;

/// Retention for `alias_hits` audit rows (`mail-aliases.md` § Per-alias-hit
/// audit list `:247` — "the 30-day retention (`mail.observability.
/// verdict_retention_days`)"). Hardcoded here (A3 Bucket C — not yet projected
/// nor admin-writable, like
/// [`fauna_core::mail_aliases::EXACT_ALIASES_MAX_DEFAULT`]) until the
/// observability policy write-path lands the admin knob; the nest spawns a
/// periodic sweeper deleting rows older than this.
pub const ALIAS_HITS_RETENTION_DAYS: i64 = 30;

/// Local-part families reserved at alias-create time but **deliberately not**
/// role addresses — the create-side half of the reserved-set split
/// (`mail-mass-mailing.md` § Reserved local-part). Each entry `f` reserves the
/// exact name `f@` plus the `f-*@` family. Unlike [`DEFAULT_RESERVED_LOCAL_PARTS`]
/// these are **never** fed to [`classify_role_address`] (which routes its members
/// to the admin mailbox): `unsubscribe+<token>@` routes to the list-unsubscribe
/// mailto handler at envelope time, so it must be uncircumventably refused as a
/// *claimable* alias without ever becoming a never-reject admin role address.
///
/// The reservation is uncircumventable (independent of the admin-tunable
/// `mail.inbound.reserved_local_parts`): it is enforced inside
/// [`validate_exact_local_part`] / [`validate_wildcard_prefix`] regardless of
/// the `reserved` argument, so every create path (exact alias, wildcard, list)
/// picks it up for free.
pub const CREATION_RESERVED_LOCAL_PART_FAMILIES: &[&str] = &["unsubscribe"];

/// True iff `local_part` is, or is a `<family>-*` child of, a
/// [`CREATION_RESERVED_LOCAL_PART_FAMILIES`] entry (case-insensitive).
/// `unsubscribe` and `unsubscribe-weekly` match; `unsubscribed` and
/// `unsubscribe.x` do **not** (only the exact name or a `-`-delimited child).
pub fn is_creation_reserved_local_part(local_part: &str) -> bool {
    let lower = local_part.to_ascii_lowercase();
    CREATION_RESERVED_LOCAL_PART_FAMILIES
        .iter()
        .any(|fam| lower == *fam || lower.starts_with(&format!("{fam}-")))
}

/// Why an alias pattern was rejected at create/update time. The nest
/// handler maps `Reserved` → the wire code `fauna.bridges.reserved_local_part`
/// and every other variant → `fauna.protocol.malformed`.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum AliasValidationError {
    #[error("alias local-part must not be empty")]
    Empty,
    #[error("alias local-part exceeds the {MAX_LOCAL_PART_LEN}-char limit ({0} chars)")]
    TooLong(usize),
    #[error(
        "alias local-part contains an invalid character {0:?}; allowed: \
         ASCII letters, digits, '.', '_', '-'"
    )]
    InvalidChar(char),
    #[error("'{0}' is a reserved local-part and cannot be claimed")]
    Reserved(String),
    #[error("wildcard prefix must end with '-' (e.g. 'bob-')")]
    WildcardMissingDash,
    #[error("wildcard prefix needs at least {MIN_WILDCARD_PREFIX_LEAD} characters before the '-'")]
    WildcardPrefixTooShort,
    #[error("wildcard prefix '{0}' would shadow a reserved local-part")]
    ReservedInWildcard(String),
}

/// True iff `local_part` (case-insensitively) matches one of `reserved`.
/// `reserved` entries are expected lowercase (e.g.
/// [`DEFAULT_RESERVED_LOCAL_PARTS`]).
pub fn is_reserved_local_part(local_part: &str, reserved: &[&str]) -> bool {
    let lower = local_part.to_ascii_lowercase();
    reserved.iter().any(|r| *r == lower)
}

/// Where a reserved role local-part routes inbound mail (`smtp-server.md`
/// § abuse@ / postmaster@ role-address routing `:191–194`, § TLSRPT `:180`).
/// Recognition shares [`DEFAULT_RESERVED_LOCAL_PARTS`]; this splits the
/// recognized set into its routing destinations. Every variant
/// except `NotReserved` **never rejects** at recipient-validate time
/// (RFC 5321 §4.5.1 / RFC 2142) and bypasses per-mailbox quota + greylisting —
/// that behavior is enforced by the recipient-resolution wiring, not here.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RoleAddressRoute {
    /// postmaster / abuse / noc / security → the deployment admin's mailbox
    /// (the actor who claimed the box). Also the default for any admin-added
    /// reserved local-part that isn't a known report processor.
    AdminMailbox,
    /// tlsrpt → the nest-side TLSRPT report processor
    /// (`fauna.bridges.ingest_tlsrpt_report`), not the admin mailbox.
    TlsrptProcessor,
    /// dmarc-report → the nest-side DMARC aggregate-report processor.
    DmarcReportProcessor,
    /// Not a reserved role local-part — resolve normally (alias lookup).
    NotReserved,
}

/// Classify a local-part's inbound role-address routing, case-insensitively,
/// against the reserved set (`reserved` expected lowercase, e.g.
/// [`DEFAULT_RESERVED_LOCAL_PARTS`]). A non-reserved local-part →
/// [`RoleAddressRoute::NotReserved`] (resolve via the normal alias lookup);
/// otherwise the destination per `smtp-server.md` § role-address: the report
/// processors (`tlsrpt`, `dmarc-report`) route to their nest-side processors,
/// every other reserved local-part to the admin mailbox.
pub fn classify_role_address(local_part: &str, reserved: &[&str]) -> RoleAddressRoute {
    if !is_reserved_local_part(local_part, reserved) {
        return RoleAddressRoute::NotReserved;
    }
    match local_part.to_ascii_lowercase().as_str() {
        "tlsrpt" => RoleAddressRoute::TlsrptProcessor,
        "dmarc-report" => RoleAddressRoute::DmarcReportProcessor,
        _ => RoleAddressRoute::AdminMailbox,
    }
}

/// Validate an **exact** alias local-part for create/update:
///
/// 1. non-empty,
/// 2. ≤ [`MAX_LOCAL_PART_LEN`] chars,
/// 3. strict ASCII character class `[A-Za-z0-9._-]` (`mail-aliases.md:321`),
/// 4. not a reserved local-part (case-insensitive, against `reserved`).
///
/// Returns the first violation found. The character-class check runs before
/// the reserved check so a pathological pattern surfaces as `InvalidChar`
/// rather than being silently lowercased into a reserved match.
pub fn validate_exact_local_part(
    local_part: &str,
    reserved: &[&str],
) -> Result<(), AliasValidationError> {
    if local_part.is_empty() {
        return Err(AliasValidationError::Empty);
    }
    // Count chars (== bytes here, since every legal char is ASCII) but read
    // the length on the char iterator so a multibyte char trips InvalidChar
    // below rather than an inflated byte-count TooLong.
    if local_part.chars().count() > MAX_LOCAL_PART_LEN {
        return Err(AliasValidationError::TooLong(local_part.chars().count()));
    }
    if let Some(bad) = local_part
        .chars()
        .find(|c| !(c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-')))
    {
        return Err(AliasValidationError::InvalidChar(bad));
    }
    if is_reserved_local_part(local_part, reserved) || is_creation_reserved_local_part(local_part) {
        return Err(AliasValidationError::Reserved(local_part.to_string()));
    }
    Ok(())
}

/// Validate a **wildcard_prefix** literal prefix (the stored `pattern`, e.g.
/// `bob-`) at create time, per `mail-aliases.md` § Kind 3 + § Don't
/// `:56-64,333`:
///
/// 1. non-empty, ≤ [`MAX_LOCAL_PART_LEN`],
/// 2. strict ASCII class `[A-Za-z0-9._-]` (so a literal `*` is rejected — the
///    client strips the `*` before submitting the prefix),
/// 3. ends in `-` (the wildcard shape is `<prefix>-*`),
/// 4. ≥ [`MIN_WILDCARD_PREFIX_LEAD`] characters before that trailing `-`,
/// 5. the glob `<prefix>*` must not match any reserved local-part — i.e. no
///    reserved name starts with `<prefix>` (case-insensitive). Example:
///    `dmarc-` shadows `dmarc-report` and is refused.
///
/// Reserved-glob violations return [`AliasValidationError::ReservedInWildcard`]
/// (the handler maps it to the dedicated wire code
/// `fauna.bridges.reserved_local_part_in_wildcard`); structural violations
/// map to `fauna.protocol.malformed`. Cross-user *exact*-vs-wildcard and
/// duplicate-wildcard conflicts are detected against the live row set by the
/// nest handler (they need DB state this pure validator can't see).
pub fn validate_wildcard_prefix(
    prefix: &str,
    reserved: &[&str],
) -> Result<(), AliasValidationError> {
    if prefix.is_empty() {
        return Err(AliasValidationError::Empty);
    }
    if prefix.chars().count() > MAX_LOCAL_PART_LEN {
        return Err(AliasValidationError::TooLong(prefix.chars().count()));
    }
    if let Some(bad) = prefix
        .chars()
        .find(|c| !(c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-')))
    {
        return Err(AliasValidationError::InvalidChar(bad));
    }
    if !prefix.ends_with('-') {
        return Err(AliasValidationError::WildcardMissingDash);
    }
    // Chars before the trailing '-'. Every legal char is ASCII, so byte len
    // == char count and `len() - 1` drops exactly the trailing '-'.
    if prefix.len() - 1 < MIN_WILDCARD_PREFIX_LEAD {
        return Err(AliasValidationError::WildcardPrefixTooShort);
    }
    let lower = prefix.to_ascii_lowercase();
    if reserved.iter().any(|r| r.starts_with(&lower)) {
        return Err(AliasValidationError::ReservedInWildcard(prefix.to_string()));
    }
    // Also block any wildcard whose glob `<prefix>*` would capture the
    // creation-reserved `unsubscribe@` / `unsubscribe-*@` family (the mailto
    // unsubscribe route, not a role address — `mail-mass-mailing.md`
    // § Reserved local-part). `unsubscribe-` (the only valid lead, since the
    // family name has no internal dash) is caught by `lower.starts_with(fam)`.
    if CREATION_RESERVED_LOCAL_PART_FAMILIES
        .iter()
        .any(|fam| fam.starts_with(&lower) || lower.starts_with(fam))
    {
        return Err(AliasValidationError::ReservedInWildcard(prefix.to_string()));
    }
    Ok(())
}

/// Outcome of an RFC 5233 sub-address split of a RCPT local-part on the first
/// [`SUBADDRESS_SEPARATOR`]. The resolver's step 2 (`mail-aliases.md`
/// § Resolution order `:111`) consumes this.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SubaddressSplit {
    /// No `+` separator — not a sub-address.
    None,
    /// `+` present; the suffix passed the char-class + length floor.
    Valid { base: String, suffix: String },
    /// `+` present but the suffix violates `[A-Za-z0-9._-]+` / length 1–64 —
    /// the resolver rejects with `550 5.1.1 Invalid sub-address`
    /// (`mail-aliases.md` § Kind 2 `:46`).
    Invalid,
}

/// Split a RCPT local-part on the first `+` for sub-addressing. The base is
/// the exact-alias to look up; the suffix is stamped as
/// [`HEADER_ADDRESS_SUFFIX`] on a match.
pub fn split_subaddress(local_part: &str) -> SubaddressSplit {
    match local_part.split_once(SUBADDRESS_SEPARATOR) {
        None => SubaddressSplit::None,
        Some((base, suffix)) => {
            let n = suffix.chars().count();
            let valid = (1..=MAX_LOCAL_PART_LEN).contains(&n)
                && suffix
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'));
            if valid {
                SubaddressSplit::Valid {
                    base: base.to_string(),
                    suffix: suffix.to_string(),
                }
            } else {
                SubaddressSplit::Invalid
            }
        }
    }
}

/// The per-alias control overrides the resolver hands back to the bridge
/// (`mail-aliases.md` § Per-alias controls). `spam_threshold_override` is in
/// whole spam-points (DAG-CBOR forbids floats); the rate caps are
/// `451`-tempfail ceilings the **nest** enforces at resolve time
/// ([`rate_cap_exceeded`]). `None` = inherit / unlimited.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ResolvedControls {
    pub spam_threshold_override: Option<u32>,
    pub rate_limit_per_hour: Option<i64>,
    pub rate_limit_per_day: Option<i64>,
}

impl ResolvedControls {
    /// `true` iff either rate cap is set, i.e. iff a resolve owes the windowed
    /// `alias_hits` count. The overwhelmingly common uncapped alias answers
    /// `false` and pays no query at RCPT time.
    pub fn has_rate_cap(&self) -> bool {
        self.rate_limit_per_hour.is_some() || self.rate_limit_per_day.is_some()
    }
}

/// The trailing window `rate_limit_per_hour` counts over (`mail-aliases.md`
/// § Per-alias rate-cap `:163`).
pub const RATE_CAP_HOUR_WINDOW_MS: i64 = 60 * 60 * 1000;

/// The trailing window `rate_limit_per_day` counts over (same §, `:165` —
/// "same semantics over 24 h window").
pub const RATE_CAP_DAY_WINDOW_MS: i64 = 24 * RATE_CAP_HOUR_WINDOW_MS;

/// The SMTP code an over-quota alias answers — a **tempfail**, so a legitimate
/// sender retries once the window rolls off (`mail-aliases.md`
/// § Per-alias rate-cap `:163` + § Architectural rules `:401`). The Go MTA's
/// `rejectFromResolver` derives the enhanced code from this class, putting
/// `451 4.7.0 Rate limit exceeded` on the wire.
pub const RATE_CAP_REJECT_CODE: u16 = 451;

/// The reason text that rides the [`RATE_CAP_REJECT_CODE`] reply.
pub const RATE_CAP_REJECT_REASON: &str = "Rate limit exceeded";

/// Is this resolve over one of the alias's rate caps?
///
/// `hour_hits` / `day_hits` are the alias's `alias_hits` counts over the
/// trailing [`RATE_CAP_HOUR_WINDOW_MS`] / [`RATE_CAP_DAY_WINDOW_MS`] windows,
/// counting **accepted** deliveries only — a rejected resolve logs no hit, so
/// a tempfailed sender's retries never push their own window further out (the
/// counter would otherwise self-perpetuate and the cap could never clear).
///
/// The comparison is `hits >= cap`: the cap is how many messages the window
/// admits, so the `cap`-th accepted hit fills it and the next one tempfails.
/// A `0` cap therefore blocks every message — deliberate and reachable, since
/// the create/update doors accept `0` and refuse only a negative
/// (`mail-aliases.md` § Per-alias rate-cap "The doors refuse a negative cap");
/// it is the tempfail "pause this alias" to `disabled`'s permanent `550`.
/// `None` = unlimited.
pub fn rate_cap_exceeded(controls: &ResolvedControls, hour_hits: i64, day_hits: i64) -> bool {
    controls
        .rate_limit_per_hour
        .is_some_and(|cap| hour_hits >= cap)
        || controls
            .rate_limit_per_day
            .is_some_and(|cap| day_hits >= cap)
}

/// The matcher's native view of an exact (or `+suffix`-base) alias row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExactCandidate {
    /// 16-byte UUID — the stored row a match logs a hit against (A2.4).
    pub alias_id: [u8; 16],
    pub actor_id: [u8; 32],
    pub disabled: bool,
    pub controls: ResolvedControls,
}

/// The matcher's native view of a `kind=forwarder` alias row matched on the
/// exact `(local_domain, pattern)` key (`mail-aliases.md` § Kind 7 + § Resolution
/// order step 2 `:124`). The handler fetches this only when the exact lookup
/// missed (exact and forwarder are mutually exclusive on one key, and exact
/// wins the tier), so its presence at step 2 means "forward this RCPT".
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ForwarderCandidate {
    /// 16-byte UUID of the forwarder row (the audit-log anchor; no hit is
    /// logged this slice — there is no admin forwarder-hit-list RPC yet).
    pub alias_id: [u8; 16],
    /// The external destination the forward dispatch delivers to.
    pub forward_target: String,
    /// The **managing admin** actor — the accountable principal for the forward
    /// (SRS attribution + forward rate-cap + NDR recipient, `mail-aliases.md:112`),
    /// not a local delivery target.
    pub forwarder_actor_id: [u8; 32],
    /// Soft-off flag (the shared `account_aliases.disabled` column). No
    /// `revoke_forwarder` RPC exists yet, so this is always `false` in
    /// production today; the matcher honours it for uniformity with the other
    /// candidate kinds and so a future revoke path needs no matcher change.
    pub disabled: bool,
}

/// The matcher's native view of one `kind=wildcard_prefix` alias row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WildcardCandidate {
    /// 16-byte UUID — the stored row a match logs a hit against (A2.4).
    pub alias_id: [u8; 16],
    pub actor_id: [u8; 32],
    /// The literal prefix incl. the trailing `-` (e.g. `bob-`).
    pub prefix: String,
    pub disabled: bool,
    pub controls: ResolvedControls,
}

/// Longest-prefix match of `local_part` against wildcard candidates
/// (`mail-aliases.md` § Resolution order `:113` — "longer prefix wins").
/// Returns the winner + the matched suffix (everything after the prefix).
/// A candidate matches only when it is a *strict* prefix (the suffix is
/// non-empty).
pub fn match_wildcard<'a>(
    local_part: &str,
    candidates: &'a [WildcardCandidate],
) -> Option<(&'a WildcardCandidate, String)> {
    candidates
        .iter()
        .filter(|c| local_part.len() > c.prefix.len() && local_part.starts_with(&c.prefix))
        .max_by_key(|c| c.prefix.len())
        .map(|c| (c, local_part[c.prefix.len()..].to_string()))
}

// ── Disposable-alias decode + aliveness (`mail-aliases.md` § Kind 5) ────

/// True iff `s` is a valid disposable token: exactly [`DISPOSABLE_TOKEN_LEN`]
/// RFC 4648 base32 chars (`A-Z2-7`, case-insensitive).
pub fn is_base32_token(s: &str) -> bool {
    s.len() == DISPOSABLE_TOKEN_LEN
        && s.bytes()
            .all(|b| b.is_ascii_alphabetic() || (b'2'..=b'7').contains(&b))
}

/// Decode a disposable RCPT local-part `<handle>-temp-<6-base32>` to its
/// token (`mail-aliases.md` § Resolution order step 3 `:112`). Returns the
/// lower-cased token iff the local-part is a non-empty handle, then the
/// [`DISPOSABLE_INFIX`] (`-temp-`), then exactly 6 base32 chars; `None`
/// otherwise (the resolver then falls through to wildcard / catch-all). Pure
/// decode only — the row lookup, aliveness, and the atomic `uses_remaining`
/// decrement are the handler's (the one resolver step with a side effect).
///
/// The `<handle>` is **not** validated against the alias owner — routing is by
/// token alone (the token is unique per `(local_domain, pattern)`), the handle
/// is the recognizable display tag.
pub fn split_disposable(local_part: &str) -> Option<String> {
    // base32 + the infix are ASCII; bail on any non-ASCII so the byte-slice
    // below can never split a multibyte char.
    if !local_part.is_ascii() {
        return None;
    }
    let token_start = local_part.len().checked_sub(DISPOSABLE_TOKEN_LEN)?;
    let token = &local_part[token_start..];
    if !is_base32_token(token) {
        return None;
    }
    let handle = local_part[..token_start].strip_suffix(DISPOSABLE_INFIX)?;
    if handle.is_empty() {
        return None;
    }
    Some(token.to_ascii_lowercase())
}

/// Build a disposable address local-part `<handle>-temp-<token>` (the mint
/// composes the full address as `disposable_local_part(...) + "@" + domain`).
pub fn disposable_local_part(handle: &str, token: &str) -> String {
    format!("{handle}{DISPOSABLE_INFIX}{token}")
}

/// Disposable aliveness (`mail-aliases.md` § Kind 5 `:90,:97`): alive iff the
/// TTL outer bound has not passed **and** uses remain. `now_millis` is the
/// caller's clock (the rule is pure given it).
///
/// - `expires_at`: `Some(e)` → alive while `now <= e`; `None` → no TTL bound.
/// - `uses_remaining`: `Some(n)` → alive while `n > 0` (decremented per route,
///   `0` = exhausted/dead); `None` → **unlimited** uses (the mint param
///   `uses = 0` stores `NULL`; only the TTL bounds it). This resolves the
///   goal-doc's "`0` = unlimited" / "`uses_remaining > 0`" contradiction: the
///   unlimited sentinel is `NULL`, never `0`.
pub fn disposable_alive(
    expires_at: Option<i64>,
    uses_remaining: Option<i64>,
    now_millis: i64,
) -> bool {
    let ttl_ok = expires_at.is_none_or(|e| now_millis <= e);
    let uses_ok = uses_remaining.is_none_or(|n| n > 0);
    ttl_ok && uses_ok
}

/// The matcher's native view of a `kind=disposable` row whose token the
/// handler decoded from the RCPT (via [`split_disposable`]) and looked up.
/// `alive` is the handler-computed [`disposable_alive`] verdict (it owns the
/// clock); the matcher only orders + signals the consume. Present only when
/// the local-part had the disposable shape **and** a row matched the token.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DisposableCandidate {
    /// 16-byte UUID — the row the handler must decrement on a live match.
    pub alias_id: [u8; 16],
    pub actor_id: [u8; 32],
    /// The matched token, stamped as [`HEADER_DISPOSABLE`].
    pub token: String,
    pub disabled: bool,
    pub alive: bool,
    pub controls: ResolvedControls,
}

/// The pre-fetched candidate set + feature flags the [`resolve_recipient`]
/// matcher decides over. The nest handler does the I/O (catch-all from
/// `mail_domains`, exact + `+suffix`-base lookups, the disposable-token row,
/// the domain's wildcard rows); the matcher owns the *order* as a single
/// pure, unit-tested unit.
pub struct ResolverInput<'a> {
    /// Lower-cased RCPT local-part.
    pub local_part: &'a str,
    /// Lower-cased RCPT domain — the local-domain the address resolves within.
    /// Used to value [`HEADER_CATCHALL_DOMAIN`] on a catch-all match.
    pub domain: &'a str,
    pub subaddressing_enabled: bool,
    pub wildcard_prefix_enabled: bool,
    /// Per-domain catch-all actor (`mail_domains.catch_all_actor_id`);
    /// `None` = no catch-all.
    pub catch_all_actor: Option<[u8; 32]>,
    /// Exact alias matching `local_part` (kind=exact), if any.
    pub exact: Option<ExactCandidate>,
    /// Admin forwarder matching `local_part` on the same exact key (kind=
    /// forwarder), if any. The handler builds this only when `exact` missed
    /// (mutual exclusion, exact wins the tier) — its presence resolves to a
    /// [`RecipientResolution::Forward`] at step 2.
    pub forwarder: Option<ForwarderCandidate>,
    /// Exact alias matching the `+suffix`-stripped base — the handler only
    /// looks this up when sub-addressing is on and a `+` is present.
    pub subaddress_base: Option<ExactCandidate>,
    /// The decoded-and-looked-up disposable row (kind=disposable). The handler
    /// builds this only when `exact` missed and [`split_disposable`] yielded a
    /// token that matched a row — so its mere presence means "resolve as
    /// disposable" at step 3.
    pub disposable: Option<DisposableCandidate>,
    /// The domain's wildcard rows.
    pub wildcards: &'a [WildcardCandidate],
    /// The actor to route a reserved role local-part to, when the local-part is
    /// an RFC 2142 reserved name (`classify_role_address` != `NotReserved`)
    /// **and** a route is resolvable. The handler resolves this target (I/O): for
    /// the overridable roles (postmaster/abuse/noc/security) it is the per-domain
    /// override actor if set (`mail_domains.role_address_overrides`), else the
    /// deployment admin; for `tlsrpt`/`dmarc-report` it is always the
    /// deployment-wide processor (the admin mailbox today), the override ignored.
    /// `None` when the local-part is not reserved, or when role routing does not
    /// apply. The matcher only orders it (step 6, before catch-all). A reserved
    /// name with no admin claimed is the handler's 451 (never-reject invariant),
    /// so it never arrives here as `None`-with-intent — `None` simply means "no
    /// role route".
    pub role_address_target: Option<[u8; 32]>,
    /// `true` iff `local_part@domain` is a list's posting address (a
    /// `kind='list'` row on the exact key). A list only sends
    /// (`mail-mass-mailing.md` § Pattern), so the matcher refuses it at step 2
    /// with [`LIST_SUBMISSIONS_REFUSED`] — ahead of wildcard, role-address and
    /// catch-all, none of which may swallow list mail. The handler looks it up
    /// only when exact and forwarder missed (the three share the exact key).
    pub list_address: bool,
}

/// The bare reason text of the `550` an inbound RCPT to a list address gets
/// (`mail-mass-mailing.md` § Pattern). Bare because the bridge's
/// `rejectFromResolver` prepends `550 5.1.1`.
pub const LIST_SUBMISSIONS_REFUSED: &str = "List submissions not accepted at this address";

/// The stored `account_aliases` row a successful resolve matched, signalled
/// to the handler for the resolve-time side effects (A2.4 hit logging + the
/// `last_hit_at`/`hit_count` bump, and — for disposables — the atomic
/// `uses_remaining` decrement). The matcher only *signals*; the handler does
/// all the I/O. `consume_disposable` is the one kind whose match additionally
/// owes the decrement — it's a strict subset of "matched a row", so one field
/// carrying both keeps the invariant un-misreadable (no `(None, Some)` state).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MatchedAlias {
    /// 16-byte UUID of the matched row (exact / +suffix-base / wildcard /
    /// disposable). The handler logs an `alias_hits` row + bumps the counters.
    pub alias_id: [u8; 16],
    /// `true` iff the match is a live disposable owing the **atomic decrement**
    /// of its `uses_remaining` (the one resolver step with a write). A finite
    /// row decrements; an unlimited row (`uses_remaining IS NULL`) is a no-op
    /// the DB layer reports back, so the matcher flags both and lets the
    /// handler's guarded `UPDATE` distinguish them (and catch the lost
    /// last-use race). `false` for exact / +suffix / wildcard.
    pub consume_disposable: bool,
}

/// The resolver verdict for one RCPT TO. `Resolved` carries the routing
/// actor, the `X-Fauna-Address-*` headers to stamp, and the per-alias control
/// overrides; `Forward` carries the external destination + the managing-admin
/// actor for the forward dispatch (no local delivery); `Reject` carries the
/// SMTP code + reason the bridge surfaces.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RecipientResolution {
    Resolved {
        actor_id: [u8; 32],
        stamped_headers: Vec<(String, String)>,
        controls: ResolvedControls,
        /// The stored row this resolve matched (for hit logging + the
        /// disposable decrement). `None` only for **catch-all** — the
        /// per-domain designated actor has no `account_aliases` row, so it
        /// logs no hit. The matcher only signals; the handler does the I/O.
        matched: Option<MatchedAlias>,
    },
    /// An admin forwarder matched (§ Resolution order step 2 `:124`): no local
    /// delivery, no header stamp; the MTA hands `{forward_target,
    /// forwarder_actor_id}` to the forward dispatch (`mail-forwarding.md`
    /// § Admin external forwarders). `redirect`-shaped — no local copy.
    Forward {
        forward_target: String,
        forwarder_actor_id: [u8; 32],
    },
    /// An RFC 2142 reserved role local-part (postmaster@/abuse@/…) with no
    /// explicit alias matched the **never-reject** route (`mail-aliases.md`
    /// § Resolution order step 6; `smtp-server.md` § abuse@/postmaster@ routing):
    /// route to the resolved role-address target, bypassing per-mailbox quota +
    /// greylisting so postmaster-to-postmaster mail always lands. Takes precedence
    /// over catch-all (step 7) — role-address mail must reach the admin, not a
    /// non-admin catch-all actor. No header stamp, no `alias_hits` row (there is
    /// no stored alias row — the route is the RFC default). The `target_actor_id`
    /// is pre-resolved by the handler (the matcher does no I/O): the per-domain
    /// role-address override actor if set, else the deployment admin
    /// (`mail-multidomain.md` § Per-domain role-address routing). A reserved name
    /// with **no** admin claimed never reaches here — the handler 451s it to hold
    /// the never-reject invariant.
    RoleAddress {
        target_actor_id: [u8; 32],
    },
    Reject {
        smtp_code: u16,
        reason: String,
    },
}

impl RecipientResolution {
    fn disabled() -> Self {
        RecipientResolution::Reject {
            smtp_code: 550,
            reason: "Address disabled".into(),
        }
    }

    /// Post-expiry / exhausted disposable (`mail-aliases.md` § Kind 5 `:90`,
    /// § Resolution order step 3 `:112`).
    fn address_expired() -> Self {
        RecipientResolution::Reject {
            smtp_code: 550,
            reason: "Address expired".into(),
        }
    }
}

/// The fixed-order RCPT-TO resolver (`mail-aliases.md` § Resolution order
/// `:119-138`, § Architectural rules `:329`): **exact → forwarder / list →
/// +suffix → disposable → wildcard → role → catch-all → 550**. The order is a contract, not a
/// knob. Pure over the pre-fetched [`ResolverInput`]. Exact and forwarder share
/// the exact-key tier and are mutually exclusive on one address, so exact
/// (step 1) wins whenever both are somehow present.
pub fn resolve_recipient(input: &ResolverInput<'_>) -> RecipientResolution {
    // 1. Exact — routes with no header stamping.
    if let Some(m) = &input.exact {
        return resolve_exact(m, Vec::new());
    }
    // 2. Forwarder — an admin external forwarder on the exact key. No local
    // delivery, no header stamp; hands the external destination + the managing
    // admin actor to the forward dispatch (`mail-forwarding.md` § Admin external
    // forwarders). A disabled forwarder rejects like a disabled exact alias.
    if let Some(f) = &input.forwarder {
        if f.disabled {
            return RecipientResolution::disabled();
        }
        return RecipientResolution::Forward {
            forward_target: f.forward_target.clone(),
            forwarder_actor_id: f.forwarder_actor_id,
        };
    }
    // 2 (cont.). List — a list's posting address, on the same exact key. A
    // list only sends, so inbound refuses here, before any pattern step or
    // catch-all could deliver it to a mailbox (`mail-mass-mailing.md`
    // § Pattern).
    if input.list_address {
        return RecipientResolution::Reject {
            smtp_code: 550,
            reason: LIST_SUBMISSIONS_REFUSED.into(),
        };
    }
    // 3. +suffix sub-addressing — strip on the first '+', re-match the base.
    if input.subaddressing_enabled {
        match split_subaddress(input.local_part) {
            SubaddressSplit::None => {}
            SubaddressSplit::Invalid => {
                return RecipientResolution::Reject {
                    smtp_code: 550,
                    reason: "Invalid sub-address".into(),
                };
            }
            SubaddressSplit::Valid { suffix, .. } => {
                if let Some(m) = &input.subaddress_base {
                    return resolve_exact(m, vec![(HEADER_ADDRESS_SUFFIX.to_string(), suffix)]);
                }
                // base miss → fall through to wildcard / catch-all.
            }
        }
    }
    // 4. Disposable — `<handle>-temp-<token>`. The handler decoded the token
    // and looked up the row; we order + signal the consume (the decrement is
    // the handler's, the one step with a side effect). A decoded-but-dead
    // disposable rejects `550 Address expired`; a *non-existent* token never
    // reaches here (the handler builds no candidate), so it falls through to
    // wildcard / catch-all, exactly per § Resolution order step 3.
    if let Some(d) = &input.disposable {
        if d.disabled {
            return RecipientResolution::disabled();
        }
        if !d.alive {
            return RecipientResolution::address_expired();
        }
        return RecipientResolution::Resolved {
            actor_id: d.actor_id,
            stamped_headers: vec![(HEADER_DISPOSABLE.to_string(), d.token.clone())],
            controls: d.controls.clone(),
            matched: Some(MatchedAlias {
                alias_id: d.alias_id,
                consume_disposable: true,
            }),
        };
    }
    // 5. Wildcard prefix — longest-prefix winner.
    if input.wildcard_prefix_enabled
        && let Some((c, matched)) = match_wildcard(input.local_part, input.wildcards)
    {
        if c.disabled {
            return RecipientResolution::disabled();
        }
        return RecipientResolution::Resolved {
            actor_id: c.actor_id,
            stamped_headers: vec![(HEADER_WILDCARD_SUFFIX.to_string(), matched)],
            controls: c.controls.clone(),
            matched: Some(MatchedAlias {
                alias_id: c.alias_id,
                consume_disposable: false,
            }),
        };
    }
    // 6. Role address — an RFC 2142 reserved local-part (postmaster@/abuse@/…)
    // with no explicit alias above. Never-reject route to the resolved target
    // (per-domain override actor if set, else the deployment admin),
    // **ahead of catch-all** so role-address mail reaches the admin rather than a
    // non-admin catch-all actor (`smtp-server.md` § abuse@/postmaster@ routing;
    // `mail-aliases.md` § Resolution order step 6; `mail-multidomain.md`
    // § Per-domain role-address routing). The handler pre-resolved the target
    // (or already 451'd a reserved-name-with-no-admin); `None` here just means
    // "not a role address".
    if let Some(target) = input.role_address_target {
        return RecipientResolution::RoleAddress {
            target_actor_id: target,
        };
    }
    // 7. Catch-all — the per-domain designated actor, if set. No stored row,
    // so `matched: None` (catch-all logs no `alias_hits`).
    if let Some(actor) = input.catch_all_actor {
        return RecipientResolution::Resolved {
            actor_id: actor,
            stamped_headers: vec![
                (HEADER_CATCHALL.to_string(), "true".into()),
                (HEADER_CATCHALL_DOMAIN.to_string(), input.domain.to_string()),
            ],
            controls: ResolvedControls::default(),
            matched: None,
        };
    }
    // 8. No match.
    RecipientResolution::Reject {
        smtp_code: 550,
        reason: "User unknown".into(),
    }
}

fn resolve_exact(m: &ExactCandidate, headers: Vec<(String, String)>) -> RecipientResolution {
    if m.disabled {
        RecipientResolution::disabled()
    } else {
        RecipientResolution::Resolved {
            actor_id: m.actor_id,
            stamped_headers: headers,
            controls: m.controls.clone(),
            matched: Some(MatchedAlias {
                alias_id: m.alias_id,
                consume_disposable: false,
            }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `ResolvedControls` carrying just the two rate caps.
    fn caps(per_hour: Option<i64>, per_day: Option<i64>) -> ResolvedControls {
        ResolvedControls {
            spam_threshold_override: None,
            rate_limit_per_hour: per_hour,
            rate_limit_per_day: per_day,
        }
    }

    #[test]
    fn an_uncapped_alias_is_never_over_quota_and_owes_no_count() {
        let c = caps(None, None);
        assert!(
            !c.has_rate_cap(),
            "no cap set ⇒ the resolver skips the query"
        );
        // Even an absurd hit history passes: `None` = unlimited.
        assert!(!rate_cap_exceeded(&c, i64::MAX, i64::MAX));
    }

    #[test]
    fn the_cap_admits_exactly_cap_messages_per_window() {
        let c = caps(Some(3), None);
        assert!(c.has_rate_cap());
        // Hits 0,1,2 are the three the window admits…
        for hits in 0..3 {
            assert!(!rate_cap_exceeded(&c, hits, 0), "hits={hits} must pass");
        }
        // …and the 4th message finds the window full.
        assert!(rate_cap_exceeded(&c, 3, 0));
        assert!(rate_cap_exceeded(&c, 4, 0));
    }

    #[test]
    fn a_zero_cap_blocks_every_message() {
        // `0` is a valid stored value — the doors refuse only a NEGATIVE
        // (§ Per-alias rate-cap) — and it reads as the tempfail "pause",
        // distinct from `disabled`'s permanent 550.
        assert!(rate_cap_exceeded(&caps(Some(0), None), 0, 0));
        assert!(rate_cap_exceeded(&caps(None, Some(0)), 0, 0));
    }

    #[test]
    fn either_window_alone_can_reject() {
        // Daily cap blown, hourly fine (a slow trickle that has run all day).
        assert!(rate_cap_exceeded(&caps(Some(100), Some(10)), 1, 10));
        // Hourly cap blown, daily fine (a burst on a generous daily budget).
        assert!(rate_cap_exceeded(&caps(Some(5), Some(1000)), 5, 5));
        // Under both ⇒ deliver.
        assert!(!rate_cap_exceeded(&caps(Some(5), Some(1000)), 4, 900));
    }

    #[test]
    fn the_windows_are_one_hour_and_24_hours() {
        assert_eq!(RATE_CAP_HOUR_WINDOW_MS, 3_600_000);
        assert_eq!(RATE_CAP_DAY_WINDOW_MS, 86_400_000);
        // The reply is a TEMPfail: legitimate senders retry (§ :401).
        assert!((400..500).contains(&RATE_CAP_REJECT_CODE));
    }

    #[test]
    fn accepts_plain_and_dotted_and_tagged_local_parts() {
        for ok in ["bob", "bob.smith", "b_smith", "bob-corp", "a.b-c_d", "x"] {
            assert!(
                validate_exact_local_part(ok, DEFAULT_RESERVED_LOCAL_PARTS).is_ok(),
                "expected {ok:?} to validate"
            );
        }
    }

    #[test]
    fn rejects_empty() {
        assert_eq!(
            validate_exact_local_part("", DEFAULT_RESERVED_LOCAL_PARTS),
            Err(AliasValidationError::Empty)
        );
    }

    #[test]
    fn rejects_overlong() {
        let long = "a".repeat(MAX_LOCAL_PART_LEN + 1);
        assert_eq!(
            validate_exact_local_part(&long, DEFAULT_RESERVED_LOCAL_PARTS),
            Err(AliasValidationError::TooLong(MAX_LOCAL_PART_LEN + 1))
        );
        // Exactly the limit is allowed.
        let at_limit = "a".repeat(MAX_LOCAL_PART_LEN);
        assert!(validate_exact_local_part(&at_limit, DEFAULT_RESERVED_LOCAL_PARTS).is_ok());
    }

    #[test]
    fn rejects_bad_characters() {
        // '+' (sub-addressing is resolver-only, never a stored pattern),
        // '@' (domain separator), whitespace, and non-ASCII.
        for (bad, ch) in [
            ("bob+work", '+'),
            ("bob@x", '@'),
            ("bob smith", ' '),
            ("bobü", 'ü'),
        ] {
            assert_eq!(
                validate_exact_local_part(bad, DEFAULT_RESERVED_LOCAL_PARTS),
                Err(AliasValidationError::InvalidChar(ch)),
                "input {bad:?}"
            );
        }
    }

    #[test]
    fn rejects_reserved_case_insensitively() {
        for reserved in DEFAULT_RESERVED_LOCAL_PARTS {
            assert_eq!(
                validate_exact_local_part(reserved, DEFAULT_RESERVED_LOCAL_PARTS),
                Err(AliasValidationError::Reserved((*reserved).to_string()))
            );
            let upper = reserved.to_ascii_uppercase();
            assert!(matches!(
                validate_exact_local_part(&upper, DEFAULT_RESERVED_LOCAL_PARTS),
                Err(AliasValidationError::Reserved(_))
            ));
        }
        assert!(is_reserved_local_part(
            "Postmaster",
            DEFAULT_RESERVED_LOCAL_PARTS
        ));
        assert!(!is_reserved_local_part("bob", DEFAULT_RESERVED_LOCAL_PARTS));
    }

    #[test]
    fn classify_role_address_splits_reserved_set_by_destination() {
        use RoleAddressRoute::*;
        // RFC 2142/5321 admin role addresses → the admin's mailbox.
        for la in ["postmaster", "abuse", "noc", "security"] {
            assert_eq!(
                classify_role_address(la, DEFAULT_RESERVED_LOCAL_PARTS),
                AdminMailbox,
                "{la} should route to the admin mailbox"
            );
        }
        // Report-processor role addresses → their nest-side processors.
        assert_eq!(
            classify_role_address("tlsrpt", DEFAULT_RESERVED_LOCAL_PARTS),
            TlsrptProcessor
        );
        assert_eq!(
            classify_role_address("dmarc-report", DEFAULT_RESERVED_LOCAL_PARTS),
            DmarcReportProcessor
        );
        // Case-insensitive, like is_reserved_local_part.
        assert_eq!(
            classify_role_address("Postmaster", DEFAULT_RESERVED_LOCAL_PARTS),
            AdminMailbox
        );
        // Non-reserved → resolve normally (no role-address routing).
        assert_eq!(
            classify_role_address("bob", DEFAULT_RESERVED_LOCAL_PARTS),
            NotReserved
        );
    }

    // ── reserved-set split: `unsubscribe@` is create-reserved but NOT a
    //    role address (`mail-mass-mailing.md` § Reserved local-part) ──────

    #[test]
    fn is_creation_reserved_matches_unsubscribe_family_only() {
        for yes in [
            "unsubscribe",
            "UNSUBSCRIBE",
            "Unsubscribe",
            "unsubscribe-weekly",
        ] {
            assert!(is_creation_reserved_local_part(yes), "{yes:?} reserved");
        }
        // Not the family: a different word that merely shares the prefix, or a
        // non-`-` separator, is a claimable local-part.
        for no in ["unsubscribed", "unsubscribe.x", "unsub", "subscribe", "bob"] {
            assert!(!is_creation_reserved_local_part(no), "{no:?} claimable");
        }
    }

    #[test]
    fn unsubscribe_is_create_reserved_uncircumventably_but_not_a_role_address() {
        // Create-reserved even against an EMPTY admin reserved list — the
        // reservation is independent of `mail.inbound.reserved_local_parts`.
        assert!(matches!(
            validate_exact_local_part("unsubscribe", &[]),
            Err(AliasValidationError::Reserved(_))
        ));
        assert!(matches!(
            validate_exact_local_part("unsubscribe-weekly", &[]),
            Err(AliasValidationError::Reserved(_))
        ));
        // The split: it is NOT a role address, so an inbound `unsubscribe@`
        // never routes to the admin mailbox (it routes to the mailto handler
        // / 550 at the resolver, ahead of role classification).
        assert_eq!(
            classify_role_address("unsubscribe", DEFAULT_RESERVED_LOCAL_PARTS),
            RoleAddressRoute::NotReserved
        );
        assert!(!is_reserved_local_part(
            "unsubscribe",
            DEFAULT_RESERVED_LOCAL_PARTS
        ));
    }

    #[test]
    fn wildcard_cannot_capture_the_unsubscribe_family() {
        assert!(matches!(
            validate_wildcard_prefix("unsubscribe-", &[]),
            Err(AliasValidationError::ReservedInWildcard(_))
        ));
        // A wildcard that doesn't overlap the family still validates.
        assert!(validate_wildcard_prefix("news-", &[]).is_ok());
    }

    // ── A2.2 — wildcard-prefix create-time validation ────────────────

    #[test]
    fn validate_wildcard_prefix_accepts_well_formed() {
        for ok in ["bob-", "bo-", "bob-news-", "a.b_c-"] {
            assert!(
                validate_wildcard_prefix(ok, DEFAULT_RESERVED_LOCAL_PARTS).is_ok(),
                "expected {ok:?} to validate"
            );
        }
    }

    #[test]
    fn validate_wildcard_prefix_requires_trailing_dash() {
        assert_eq!(
            validate_wildcard_prefix("bob", DEFAULT_RESERVED_LOCAL_PARTS),
            Err(AliasValidationError::WildcardMissingDash)
        );
        // A bare '*' is rejected by the char-class before the dash check —
        // the client must strip the '*' and submit the literal prefix.
        assert_eq!(
            validate_wildcard_prefix("bob-*", DEFAULT_RESERVED_LOCAL_PARTS),
            Err(AliasValidationError::InvalidChar('*'))
        );
    }

    #[test]
    fn validate_wildcard_prefix_enforces_min_lead() {
        // One char before the '-' is too greedy.
        assert_eq!(
            validate_wildcard_prefix("b-", DEFAULT_RESERVED_LOCAL_PARTS),
            Err(AliasValidationError::WildcardPrefixTooShort)
        );
        // The lone '-' has zero lead chars.
        assert_eq!(
            validate_wildcard_prefix("-", DEFAULT_RESERVED_LOCAL_PARTS),
            Err(AliasValidationError::WildcardPrefixTooShort)
        );
    }

    #[test]
    fn validate_wildcard_prefix_refuses_reserved_glob() {
        // `dmarc-*` would match the reserved `dmarc-report`.
        assert_eq!(
            validate_wildcard_prefix("dmarc-", DEFAULT_RESERVED_LOCAL_PARTS),
            Err(AliasValidationError::ReservedInWildcard("dmarc-".into()))
        );
        // Case-insensitive.
        assert!(matches!(
            validate_wildcard_prefix("DMARC-", DEFAULT_RESERVED_LOCAL_PARTS),
            Err(AliasValidationError::ReservedInWildcard(_))
        ));
        // `bob-*` matches no reserved name (none start with `bob-`).
        assert!(validate_wildcard_prefix("bob-", DEFAULT_RESERVED_LOCAL_PARTS).is_ok());
    }

    // ── A2.2 — sub-address split (RFC 5233) ──────────────────────────

    #[test]
    fn split_subaddress_none_when_no_plus() {
        assert_eq!(split_subaddress("bob"), SubaddressSplit::None);
        assert_eq!(split_subaddress("bob.smith"), SubaddressSplit::None);
    }

    #[test]
    fn split_subaddress_valid_splits_on_first_plus() {
        assert_eq!(
            split_subaddress("bob+work"),
            SubaddressSplit::Valid {
                base: "bob".into(),
                suffix: "work".into()
            }
        );
    }

    #[test]
    fn split_subaddress_invalid_suffix() {
        // Empty suffix, a second '+', and an out-of-class char all reject.
        assert_eq!(split_subaddress("bob+"), SubaddressSplit::Invalid);
        assert_eq!(split_subaddress("bob+a+b"), SubaddressSplit::Invalid);
        assert_eq!(split_subaddress("bob+wörk"), SubaddressSplit::Invalid);
        let long = format!("bob+{}", "a".repeat(MAX_LOCAL_PART_LEN + 1));
        assert_eq!(split_subaddress(&long), SubaddressSplit::Invalid);
    }

    // ── A2.2 — wildcard longest-prefix matching ──────────────────────

    fn wc(prefix: &str, actor: u8) -> WildcardCandidate {
        WildcardCandidate {
            alias_id: [actor; 16],
            actor_id: [actor; 32],
            prefix: prefix.into(),
            disabled: false,
            controls: ResolvedControls::default(),
        }
    }

    #[test]
    fn match_wildcard_longest_prefix_wins() {
        let cands = [wc("bob-", 1), wc("bob-news-", 2)];
        let (winner, suffix) = match_wildcard("bob-news-weekly", &cands).unwrap();
        assert_eq!(winner.actor_id, [2; 32]);
        assert_eq!(suffix, "weekly");
        // A shorter match still routes (to the shorter prefix's owner).
        let (winner, suffix) = match_wildcard("bob-amazon", &cands).unwrap();
        assert_eq!(winner.actor_id, [1; 32]);
        assert_eq!(suffix, "amazon");
    }

    #[test]
    fn match_wildcard_requires_nonempty_suffix() {
        let cands = [wc("bob-", 1)];
        // Exactly the prefix (empty remainder) does not match.
        assert!(match_wildcard("bob-", &cands).is_none());
        // No prefix relationship.
        assert!(match_wildcard("alice-x", &cands).is_none());
    }

    // ── A2.2 — the fixed-order resolver ──────────────────────────────

    fn exact(actor: u8, disabled: bool) -> ExactCandidate {
        ExactCandidate {
            alias_id: [actor; 16],
            actor_id: [actor; 32],
            disabled,
            controls: ResolvedControls::default(),
        }
    }

    fn base_input<'a>(
        local_part: &'a str,
        wildcards: &'a [WildcardCandidate],
    ) -> ResolverInput<'a> {
        ResolverInput {
            local_part,
            domain: "example.test",
            subaddressing_enabled: true,
            wildcard_prefix_enabled: true,
            catch_all_actor: None,
            exact: None,
            forwarder: None,
            subaddress_base: None,
            disposable: None,
            wildcards,
            role_address_target: None,
            list_address: false,
        }
    }

    fn fwd(dest: &str, admin: u8, disabled: bool) -> ForwarderCandidate {
        ForwarderCandidate {
            alias_id: [admin; 16],
            forward_target: dest.into(),
            forwarder_actor_id: [admin; 32],
            disabled,
        }
    }

    #[test]
    fn resolve_exact_wins_with_no_header() {
        let mut input = base_input("bob", &[]);
        input.exact = Some(ExactCandidate {
            alias_id: [7; 16],
            actor_id: [7; 32],
            disabled: false,
            controls: ResolvedControls {
                spam_threshold_override: Some(8),
                ..Default::default()
            },
        });
        match resolve_recipient(&input) {
            RecipientResolution::Resolved {
                actor_id,
                stamped_headers,
                controls,
                matched,
            } => {
                assert_eq!(actor_id, [7; 32]);
                assert!(stamped_headers.is_empty(), "exact stamps no header");
                assert_eq!(controls.spam_threshold_override, Some(8));
                assert_eq!(
                    matched,
                    Some(MatchedAlias {
                        alias_id: [7; 16],
                        consume_disposable: false,
                    }),
                    "exact matches its row but consumes no disposable"
                );
            }
            other => panic!("expected Resolved, got {other:?}"),
        }
    }

    #[test]
    fn resolve_subaddress_stamps_suffix() {
        let mut input = base_input("bob+work", &[]);
        input.subaddress_base = Some(exact(7, false));
        match resolve_recipient(&input) {
            RecipientResolution::Resolved {
                actor_id,
                stamped_headers,
                matched,
                ..
            } => {
                assert_eq!(actor_id, [7; 32]);
                assert_eq!(
                    stamped_headers,
                    vec![(HEADER_ADDRESS_SUFFIX.to_string(), "work".to_string())]
                );
                // +suffix routes through the base exact row — that row gets the hit.
                assert_eq!(
                    matched,
                    Some(MatchedAlias {
                        alias_id: [7; 16],
                        consume_disposable: false,
                    })
                );
            }
            other => panic!("expected Resolved, got {other:?}"),
        }
    }

    #[test]
    fn resolve_subaddress_disabled_when_flag_off() {
        // Flag off → '+' address falls through to no-match (not invalid).
        let mut input = base_input("bob+work", &[]);
        input.subaddressing_enabled = false;
        input.subaddress_base = Some(exact(7, false));
        assert!(matches!(
            resolve_recipient(&input),
            RecipientResolution::Reject { smtp_code: 550, .. }
        ));
    }

    #[test]
    fn resolve_invalid_subaddress_rejected() {
        let input = base_input("bob+", &[]);
        match resolve_recipient(&input) {
            RecipientResolution::Reject { smtp_code, reason } => {
                assert_eq!(smtp_code, 550);
                assert!(reason.contains("Invalid sub-address"), "{reason}");
            }
            other => panic!("expected Reject, got {other:?}"),
        }
    }

    #[test]
    fn resolve_wildcard_after_exact_and_suffix_miss() {
        let cands = [wc("bob-", 3)];
        let input = base_input("bob-amazon", &cands);
        match resolve_recipient(&input) {
            RecipientResolution::Resolved {
                actor_id,
                stamped_headers,
                matched,
                ..
            } => {
                assert_eq!(actor_id, [3; 32]);
                assert_eq!(
                    stamped_headers,
                    vec![(HEADER_WILDCARD_SUFFIX.to_string(), "amazon".to_string())]
                );
                assert_eq!(
                    matched,
                    Some(MatchedAlias {
                        alias_id: [3; 16],
                        consume_disposable: false,
                    })
                );
            }
            other => panic!("expected Resolved, got {other:?}"),
        }
    }

    #[test]
    fn resolve_exact_beats_wildcard() {
        // A registered exact alias takes precedence over a pattern match
        // (`mail-aliases.md:119`) — no wildcard-suffix stamp.
        let cands = [wc("bob-", 3)];
        let mut input = base_input("bob-corp", &cands);
        input.exact = Some(exact(9, false));
        match resolve_recipient(&input) {
            RecipientResolution::Resolved {
                actor_id,
                stamped_headers,
                ..
            } => {
                assert_eq!(actor_id, [9; 32]);
                assert!(stamped_headers.is_empty());
            }
            other => panic!("expected Resolved, got {other:?}"),
        }
    }

    // ── AF (mail-forwarding § AF) — forwarder step 2 ────────────────

    #[test]
    fn resolve_forwarder_yields_forward_no_header() {
        // An admin forwarder on the exact key forwards to its external
        // destination, attributed to the managing admin, with no stamp.
        let mut input = base_input("info", &[]);
        input.forwarder = Some(fwd("oldaccount@example.com", 4, false));
        match resolve_recipient(&input) {
            RecipientResolution::Forward {
                forward_target,
                forwarder_actor_id,
            } => {
                assert_eq!(forward_target, "oldaccount@example.com");
                assert_eq!(forwarder_actor_id, [4; 32]);
            }
            other => panic!("expected Forward, got {other:?}"),
        }
    }

    #[test]
    fn resolve_exact_beats_forwarder() {
        // Exact (step 1) wins the shared exact-key tier — a registered local
        // mailbox is checked before the forwarder table (`mail-aliases.md:134`).
        let mut input = base_input("info", &[]);
        input.exact = Some(exact(9, false));
        input.forwarder = Some(fwd("oldaccount@example.com", 4, false));
        match resolve_recipient(&input) {
            RecipientResolution::Resolved { actor_id, .. } => assert_eq!(actor_id, [9; 32]),
            other => panic!("expected exact Resolved, got {other:?}"),
        }
    }

    #[test]
    fn resolve_forwarder_beats_subaddress_and_wildcard() {
        // Forwarder is step 2 — it wins over a `+suffix` re-match (step 3) and
        // over a wildcard that would also catch the local-part (step 5).
        let cands = [wc("inf-", 3)];
        let mut input = base_input("info", &cands);
        input.subaddress_base = Some(exact(7, false));
        input.forwarder = Some(fwd("team@offsite.example", 4, false));
        match resolve_recipient(&input) {
            RecipientResolution::Forward { forward_target, .. } => {
                assert_eq!(forward_target, "team@offsite.example");
            }
            other => panic!("expected Forward, got {other:?}"),
        }
    }

    #[test]
    fn resolve_disabled_forwarder_rejected() {
        let mut input = base_input("info", &[]);
        input.forwarder = Some(fwd("oldaccount@example.com", 4, true));
        match resolve_recipient(&input) {
            RecipientResolution::Reject { smtp_code, reason } => {
                assert_eq!(smtp_code, 550);
                assert!(reason.contains("disabled"), "{reason}");
            }
            other => panic!("expected Reject, got {other:?}"),
        }
    }

    #[test]
    fn resolve_disabled_alias_rejected() {
        let mut input = base_input("bob", &[]);
        input.exact = Some(exact(7, true));
        match resolve_recipient(&input) {
            RecipientResolution::Reject { smtp_code, reason } => {
                assert_eq!(smtp_code, 550);
                assert!(reason.contains("disabled"), "{reason}");
            }
            other => panic!("expected Reject, got {other:?}"),
        }
    }

    #[test]
    fn resolve_catch_all_when_set_else_user_unknown() {
        // No match, catch-all set → routes + stamps the catch-all header.
        let mut input = base_input("typo", &[]);
        input.catch_all_actor = Some([5; 32]);
        match resolve_recipient(&input) {
            RecipientResolution::Resolved {
                actor_id,
                stamped_headers,
                matched,
                ..
            } => {
                assert_eq!(actor_id, [5; 32]);
                // Both the catch-all flag AND the domain-tagging header (valued
                // with the RCPT domain the catch-all fired on) are stamped.
                assert_eq!(
                    stamped_headers,
                    vec![
                        (HEADER_CATCHALL.to_string(), "true".to_string()),
                        (
                            HEADER_CATCHALL_DOMAIN.to_string(),
                            "example.test".to_string()
                        ),
                    ]
                );
                // Catch-all has no `account_aliases` row → logs no hit.
                assert_eq!(matched, None);
            }
            other => panic!("expected Resolved, got {other:?}"),
        }
        // No match, no catch-all → 550 user unknown.
        let input = base_input("typo", &[]);
        match resolve_recipient(&input) {
            RecipientResolution::Reject { smtp_code, reason } => {
                assert_eq!(smtp_code, 550);
                assert!(reason.contains("User unknown"), "{reason}");
            }
            other => panic!("expected Reject, got {other:?}"),
        }
    }

    #[test]
    fn resolve_wildcard_beats_catch_all() {
        let cands = [wc("bob-", 3)];
        let mut input = base_input("bob-x", &cands);
        input.catch_all_actor = Some([5; 32]);
        match resolve_recipient(&input) {
            RecipientResolution::Resolved { actor_id, .. } => assert_eq!(actor_id, [3; 32]),
            other => panic!("expected wildcard Resolved, got {other:?}"),
        }
    }

    #[test]
    fn resolve_list_address_refuses_ahead_of_wildcard_and_catch_all() {
        // A list only sends (`mail-mass-mailing.md` § Pattern): inbound to its
        // posting address refuses with the goal's bare reason text (the bridge
        // prepends `550 5.1.1`) — even when a wildcard would match and a
        // catch-all actor is set, neither of which may swallow list mail.
        let cands = [wc("ne", 3)];
        let mut input = base_input("news", &cands);
        input.list_address = true;
        input.catch_all_actor = Some([5; 32]);
        match resolve_recipient(&input) {
            RecipientResolution::Reject { smtp_code, reason } => {
                assert_eq!(smtp_code, 550);
                assert_eq!(reason, "List submissions not accepted at this address");
                assert_eq!(reason, LIST_SUBMISSIONS_REFUSED);
            }
            other => panic!("expected list Reject, got {other:?}"),
        }
    }

    #[test]
    fn resolve_role_address_routes_to_admin_on_full_miss() {
        // No alias matched; the handler pre-resolved the deployment admin for a
        // reserved local-part → never-reject route to the admin mailbox.
        let mut input = base_input("postmaster", &[]);
        input.role_address_target = Some([9; 32]);
        match resolve_recipient(&input) {
            RecipientResolution::RoleAddress { target_actor_id } => {
                assert_eq!(target_actor_id, [9; 32]);
            }
            other => panic!("expected RoleAddress, got {other:?}"),
        }
    }

    #[test]
    fn resolve_role_address_beats_catch_all() {
        // Role-address mail must reach the admin, not a non-admin catch-all
        // (smtp-server.md § abuse@/postmaster@ routing): role wins over step 7.
        let mut input = base_input("abuse", &[]);
        input.role_address_target = Some([9; 32]);
        input.catch_all_actor = Some([5; 32]);
        match resolve_recipient(&input) {
            RecipientResolution::RoleAddress { target_actor_id } => {
                assert_eq!(target_actor_id, [9; 32], "role beats catch-all");
            }
            other => panic!("expected RoleAddress, got {other:?}"),
        }
    }

    #[test]
    fn resolve_exact_beats_role_address() {
        // An explicit `postmaster@` mailbox the admin registered wins over the
        // RFC default route (exact is step 1).
        let mut input = base_input("postmaster", &[]);
        input.exact = Some(exact(7, false));
        input.role_address_target = Some([9; 32]);
        match resolve_recipient(&input) {
            RecipientResolution::Resolved { actor_id, .. } => assert_eq!(actor_id, [7; 32]),
            other => panic!("expected exact Resolved, got {other:?}"),
        }
    }

    // ── A2.3 — disposable decode + aliveness ─────────────────────────

    #[test]
    fn is_base32_token_accepts_only_6_base32_chars() {
        for ok in ["abcdef", "ABCDEF", "a2b3c4", "zzzzz7", "AbCdEf"] {
            assert!(is_base32_token(ok), "expected {ok:?} to be a token");
        }
        // Wrong length, base32-excluded digits (0/1/8/9), and out-of-class.
        for bad in ["abcde", "abcdefg", "abcd01", "abcde8", "abc-ef", "ab cde"] {
            assert!(!is_base32_token(bad), "expected {bad:?} to be rejected");
        }
    }

    #[test]
    fn split_disposable_extracts_lowercased_token() {
        assert_eq!(split_disposable("bob-temp-abcdef"), Some("abcdef".into()));
        // Case-insensitive token; the handle may itself contain `-temp-`.
        assert_eq!(split_disposable("bob-temp-AbCdEf"), Some("abcdef".into()));
        assert_eq!(
            split_disposable("bob-temp-temp-a2b3c4"),
            Some("a2b3c4".into())
        );
    }

    #[test]
    fn split_disposable_none_for_non_disposable_shapes() {
        for miss in [
            "bob-amazon",       // no -temp- infix
            "temp-abcdef",      // empty handle (the strip leaves "")
            "-temp-abcdef",     // empty handle
            "bob-temp-abcde",   // token too short
            "bob-temp-abcdefg", // token too long
            "bob-temp-abcd01",  // base32-excluded digit
            "bob-temp-ab cde",  // out-of-class
        ] {
            assert_eq!(split_disposable(miss), None, "input {miss:?}");
        }
        // Any non-ASCII local-part bails (the byte-split guard) — never
        // matches, never panics, whether the non-ASCII is in the handle or
        // the token region.
        assert_eq!(split_disposable("böb-temp-abcdef"), None);
        assert_eq!(split_disposable("bob-temp-abcdeö"), None);
    }

    #[test]
    fn disposable_local_part_round_trips_through_split() {
        let lp = disposable_local_part("bob", "a2b3c4");
        assert_eq!(lp, "bob-temp-a2b3c4");
        assert_eq!(split_disposable(&lp), Some("a2b3c4".into()));
    }

    #[test]
    fn disposable_alive_honors_ttl_and_uses() {
        // TTL: alive at/under the bound, dead past it; None = no TTL bound.
        assert!(disposable_alive(Some(100), Some(1), 100));
        assert!(disposable_alive(Some(100), Some(1), 99));
        assert!(!disposable_alive(Some(100), Some(1), 101));
        assert!(disposable_alive(None, Some(1), i64::MAX));
        // Uses: >0 alive, 0 exhausted; None = unlimited (the `uses=0` mint).
        assert!(!disposable_alive(Some(100), Some(0), 50));
        assert!(disposable_alive(Some(100), None, 50));
        // Unlimited uses still bounded by TTL.
        assert!(!disposable_alive(Some(100), None, 101));
    }

    // ── A2.3 — disposable resolver step (order, consume, expiry) ─────

    fn disp(actor: u8, alive: bool, disabled: bool) -> DisposableCandidate {
        DisposableCandidate {
            alias_id: [actor; 16],
            actor_id: [actor; 32],
            token: "a2b3c4".into(),
            disabled,
            alive,
            controls: ResolvedControls {
                rate_limit_per_day: Some(DISPOSABLE_RATE_LIMIT_PER_DAY_DEFAULT),
                ..Default::default()
            },
        }
    }

    #[test]
    fn resolve_live_disposable_stamps_token_and_signals_consume() {
        let mut input = base_input("bob-temp-a2b3c4", &[]);
        input.disposable = Some(disp(8, true, false));
        match resolve_recipient(&input) {
            RecipientResolution::Resolved {
                actor_id,
                stamped_headers,
                controls,
                matched,
            } => {
                assert_eq!(actor_id, [8; 32]);
                assert_eq!(
                    stamped_headers,
                    vec![(HEADER_DISPOSABLE.to_string(), "a2b3c4".to_string())]
                );
                assert_eq!(
                    controls.rate_limit_per_day,
                    Some(DISPOSABLE_RATE_LIMIT_PER_DAY_DEFAULT)
                );
                // The one matched-row kind that also owes the atomic decrement.
                assert_eq!(
                    matched,
                    Some(MatchedAlias {
                        alias_id: [8; 16],
                        consume_disposable: true,
                    })
                );
            }
            other => panic!("expected Resolved, got {other:?}"),
        }
    }

    #[test]
    fn resolve_dead_disposable_rejects_expired() {
        let mut input = base_input("bob-temp-a2b3c4", &[]);
        input.disposable = Some(disp(8, false, false));
        match resolve_recipient(&input) {
            RecipientResolution::Reject { smtp_code, reason } => {
                assert_eq!(smtp_code, 550);
                assert!(reason.contains("expired"), "{reason}");
            }
            other => panic!("expected Reject, got {other:?}"),
        }
    }

    #[test]
    fn resolve_disabled_disposable_rejects_disabled() {
        let mut input = base_input("bob-temp-a2b3c4", &[]);
        input.disposable = Some(disp(8, true, true));
        match resolve_recipient(&input) {
            RecipientResolution::Reject { smtp_code, reason } => {
                assert_eq!(smtp_code, 550);
                assert!(reason.contains("disabled"), "{reason}");
            }
            other => panic!("expected Reject, got {other:?}"),
        }
    }

    #[test]
    fn resolve_exact_beats_disposable() {
        // If an exact alias `bob-temp-a2b3c4` exists, it wins (step 1) and no
        // disposable use is consumed — the handler must not even build the
        // candidate when exact matched, but the matcher's order guarantees it.
        let mut input = base_input("bob-temp-a2b3c4", &[]);
        input.exact = Some(exact(1, false));
        input.disposable = Some(disp(8, true, false));
        match resolve_recipient(&input) {
            RecipientResolution::Resolved {
                actor_id,
                stamped_headers,
                matched,
                ..
            } => {
                assert_eq!(actor_id, [1; 32]);
                assert!(stamped_headers.is_empty());
                // Exact wins (step 1): matches its own row, no disposable consume.
                assert_eq!(
                    matched,
                    Some(MatchedAlias {
                        alias_id: [1; 16],
                        consume_disposable: false,
                    })
                );
            }
            other => panic!("expected exact Resolved, got {other:?}"),
        }
    }

    #[test]
    fn resolve_disposable_beats_wildcard() {
        // A live disposable wins over a wildcard that would also match the
        // `<handle>-...` shape (disposable is step 3, wildcard step 4).
        let cands = [wc("bob-", 3)];
        let mut input = base_input("bob-temp-a2b3c4", &cands);
        input.disposable = Some(disp(8, true, false));
        match resolve_recipient(&input) {
            RecipientResolution::Resolved {
                actor_id, matched, ..
            } => {
                assert_eq!(actor_id, [8; 32]);
                assert_eq!(
                    matched,
                    Some(MatchedAlias {
                        alias_id: [8; 16],
                        consume_disposable: true,
                    })
                );
            }
            other => panic!("expected disposable Resolved, got {other:?}"),
        }
    }

    #[test]
    fn resolve_unmatched_disposable_token_falls_through_to_wildcard() {
        // No disposable row for the token → the handler builds no candidate →
        // a `bob-*` wildcard catches `bob-temp-a2b3c4` with the full suffix.
        let cands = [wc("bob-", 3)];
        let input = base_input("bob-temp-a2b3c4", &cands);
        match resolve_recipient(&input) {
            RecipientResolution::Resolved {
                actor_id,
                stamped_headers,
                matched,
                ..
            } => {
                assert_eq!(actor_id, [3; 32]);
                assert_eq!(
                    stamped_headers,
                    vec![(
                        HEADER_WILDCARD_SUFFIX.to_string(),
                        "temp-a2b3c4".to_string()
                    )]
                );
                // Wildcard matches its own row, no disposable consume.
                assert_eq!(
                    matched,
                    Some(MatchedAlias {
                        alias_id: [3; 16],
                        consume_disposable: false,
                    })
                );
            }
            other => panic!("expected wildcard Resolved, got {other:?}"),
        }
    }

    // ── The delivery-time spam-threshold fold (`mail-aliases.md:153`) ──

    #[test]
    fn spam_threshold_fold_prefers_the_alias_tier_then_the_account_tier() {
        // Alias wins over both lower tiers.
        assert_eq!(resolve_delivery_spam_threshold(Some(2), Some(7), 5), 2);
        // No alias override ⇒ the per-account tier.
        assert_eq!(resolve_delivery_spam_threshold(None, Some(7), 5), 7);
        // Neither user tier ⇒ the admin default.
        assert_eq!(resolve_delivery_spam_threshold(None, None, 5), 5);
    }

    #[test]
    fn spam_threshold_fold_treats_zero_as_a_chosen_value_not_as_unset() {
        // A user who sets 0 has DISABLED auto-Junk for that alias/account —
        // that must outrank the admin default, not fall through to it. This is
        // the whole reason the tiers are `Option<u32>` rather than a sentinel.
        assert_eq!(resolve_delivery_spam_threshold(Some(0), Some(7), 5), 0);
        assert_eq!(resolve_delivery_spam_threshold(None, Some(0), 5), 0);
        // And the converse: an admin-disabled deployment is overridable upward
        // by a user tier (`mail-spam.md:29` — the user's safety valve).
        assert_eq!(resolve_delivery_spam_threshold(None, Some(4), 0), 4);
    }

    #[test]
    fn prepended_stamps_are_readable_by_the_reader_that_consumes_them() {
        // The two halves of the nest-side delivery path must agree: what the
        // stamper writes is what the reader finds, with no CRLF or ordering
        // assumption left to chance.
        let raw = b"From: s@e.test\r\nSubject: hi\r\n\r\nbody\r\n";
        let stamps = vec![
            (HEADER_ADDRESS_SUFFIX.to_string(), "work".to_string()),
            (HEADER_SPAM_THRESHOLD.to_string(), "4".to_string()),
        ];
        let out = prepend_stamped_headers(raw, &stamps);
        assert_eq!(read_spam_threshold_stamp(&out), Some(4));
        assert!(out.starts_with(b"X-Fauna-Address-Suffix: work\r\n"));
        assert!(
            out.ends_with(raw),
            "the original message is copied verbatim"
        );
        // No stamps ⇒ byte-identical, so a system-generated sender pays nothing.
        assert_eq!(prepend_stamped_headers(raw, &[]), raw.to_vec());
    }

    #[test]
    fn a_fired_allow_leaves_one_zero_threshold_stamp_first_in_the_copy() {
        // The resolver's RCPT-time stamp (a real threshold) prepended ahead of
        // the sender's headers, plus a body line that merely LOOKS like a stamp.
        let raw = b"X-Fauna-Address-Suffix: work\r\n\
                    X-Fauna-Spam-Threshold: 5\r\n\
                    From: sender@example.test\r\n\
                    x-fauna-spam-threshold: 9\r\n\
                    \x20continued\r\n\
                    Subject: hi\r\n\
                    \r\n\
                    X-Fauna-Spam-Threshold: 7\r\n";
        let out = stamp_filter_allow(raw);
        // Every scorer reads the first match, so the override must BE the first
        // (and, in the header section, the only) threshold line.
        assert_eq!(
            read_spam_threshold_stamp(&out),
            Some(FILTER_ALLOW_SPAM_THRESHOLD)
        );
        assert!(out.starts_with(b"X-Fauna-Spam-Threshold: 0\r\n"));
        let header_end = crate::header_walk::find_body_offset(&out).unwrap();
        let headers = std::str::from_utf8(&out[..header_end]).unwrap();
        assert_eq!(
            headers
                .to_ascii_lowercase()
                .matches("x-fauna-spam-threshold")
                .count(),
            1
        );
        // Everything else survives verbatim, the body included.
        assert_eq!(
            &out[b"X-Fauna-Spam-Threshold: 0\r\n".len()..],
            &b"X-Fauna-Address-Suffix: work\r\n\
               From: sender@example.test\r\n\
               Subject: hi\r\n\
               \r\n\
               X-Fauna-Spam-Threshold: 7\r\n"[..]
        );
        // An unstamped copy (no resolver stamp) gains the override too.
        let bare = b"From: s@e.test\r\n\r\nbody\r\n";
        let out = stamp_filter_allow(bare);
        assert_eq!(read_spam_threshold_stamp(&out), Some(0));
        assert!(out.ends_with(bare));
    }

    #[test]
    fn spam_threshold_stamp_round_trips_through_a_delivered_message() {
        // Exactly the shape the MTA builds: the resolver's stamps prepended
        // (CRLF) ahead of the sender's own headers, then the body.
        let raw = b"X-Fauna-Address-Suffix: work\r\n\
                    X-Fauna-Spam-Threshold: 2\r\n\
                    From: sender@example.test\r\n\
                    Subject: hi\r\n\
                    \r\n\
                    body\r\n";
        assert_eq!(read_spam_threshold_stamp(raw), Some(2));
        // Zero is a real stamped value, not "absent".
        let raw_zero = b"X-Fauna-Spam-Threshold: 0\r\nFrom: s@e.test\r\n\r\nbody\r\n";
        assert_eq!(read_spam_threshold_stamp(raw_zero), Some(0));
        // Field name matched case-insensitively (RFC 5322 §2.2.3).
        let raw_case = b"x-FAUNA-spam-THRESHOLD:  7 \r\nFrom: s@e.test\r\n\r\nbody\r\n";
        assert_eq!(read_spam_threshold_stamp(raw_case), Some(7));
    }

    #[test]
    fn spam_threshold_stamp_reads_none_rather_than_zero_when_absent_or_junk() {
        // No stamp — mail delivered before this shipped. The caller must fall
        // back to its session policy; reading `0` here would silently disable
        // Junk routing for every pre-existing message.
        let none = b"From: sender@example.test\r\nSubject: hi\r\n\r\nbody\r\n";
        assert_eq!(read_spam_threshold_stamp(none), None);
        // Malformed values fail safe the same way.
        for raw in [
            &b"X-Fauna-Spam-Threshold: not-a-number\r\n\r\nbody\r\n"[..],
            &b"X-Fauna-Spam-Threshold: -3\r\n\r\nbody\r\n"[..],
            &b"X-Fauna-Spam-Threshold:\r\n\r\nbody\r\n"[..],
        ] {
            assert_eq!(read_spam_threshold_stamp(raw), None, "raw: {raw:?}");
        }
        // A merely similar field name is not the stamp (substring-safe).
        let similar = b"X-Fauna-Spam-Threshold-Note: 4\r\n\r\nbody\r\n";
        assert_eq!(read_spam_threshold_stamp(similar), None);
        // A body line that looks like the header is not read: the scan stops
        // at the header/body boundary.
        let in_body = b"From: s@e.test\r\n\r\nX-Fauna-Spam-Threshold: 1\r\n";
        assert_eq!(read_spam_threshold_stamp(in_body), None);
    }

    #[test]
    fn the_stamp_name_is_inside_the_namespace_the_inbound_forgery_strip_covers() {
        // The spoof-safety of the whole channel rests on this: a sender-supplied
        // copy is removed by `strip_fauna_headers` before the genuine stamp is
        // prepended, and that strip matches on the `X-Fauna-` prefix. If the
        // constant were ever renamed out of the namespace, forged thresholds
        // would ride straight through.
        assert!(HEADER_SPAM_THRESHOLD.starts_with("X-Fauna-"));
        let forged = b"X-Fauna-Spam-Threshold: 15\r\nFrom: s@e.test\r\n\r\nbody\r\n";
        let stripped = crate::received_header::strip_fauna_headers(forged);
        assert_eq!(read_spam_threshold_stamp(&stripped), None);
    }
}
