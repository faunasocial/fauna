//! Permission sets (`include:<NSID>?aud=…`) — the pure expansion half.
//!
//! A client may name a **Lexicon permission set** instead of listing granular
//! scopes: `include:com.example.calendar.appPerms?aud=did:web:svc.example`.
//! This authorization server must then resolve the set's published document and
//! expand it into member scopes. Resolution is a caller-directed
//! DNS + DID + HTTPS plane; expansion is grammar and document interpretation.
//! `atproto-pds-full.md` § F4 detail's *Permission sets* bullet (ruled
//! 2026-08-03) splits them along exactly that line, and this module is the
//! pure half: **Go only ever fetches; expansion is the pure module's.**
//!
//! # What lives here, and what deliberately does not
//!
//! Here: the `include:` grammar, NSID syntax validation, document parsing, the
//! same-NSID-hierarchy constraint, the invalid-member ignore rules,
//! `inheritAud` application, and the caps. The output is **ordinary granular
//! scopes** — `repo:…` and `rpc:…?aud=…` strings indistinguishable from ones a
//! client requested directly, which is what lets [`crate::authz`] bind them
//! with no new arm and no second opinion about scope meaning.
//!
//! Not here, each for a stated reason:
//!
//! - **No network.** Not a fetch, not a cache, not a DNS lookup. The NSID is
//!   syntax-validated *before* any I/O so Go fetches only what this parser
//!   emitted, and the module stays wasm-clean and fixture-testable.
//! - **No control-character stripping of `title`/`details`.** Those strings are
//!   attacker-authored and the fence is real, but it lives at **machine
//!   composition**, in the same pass that fences `client_name` (§ F4 detail's
//!   card bullet). Stripping here too would put the
//!   property in two places, which is how the two come to disagree.
//! - **No D8 change.** Members are ordinary granular scopes, so
//!   [`crate::authz::authorize`] judges them through the matrix it already has;
//!   `no_granular_scope_reaches` sits above every granular arm, so a set may
//!   declare what it likes and `SessionLifecycle`, `AccountMutation` and
//!   `Migration` stay unreachable by construction.
//!
//! # Expand exactly the bytes that were verified
//!
//! [`expand_permission_set`] takes the record's **dag-cbor bytes**, not a
//! re-encoded JSON view of them. § F4 detail requires the fetched record to be
//! authenticated (MST inclusion proof plus commit signature) before it may
//! feed an authorization decision; handing this module the verified block
//! verbatim is what makes "what was verified" and "what was expanded" the same
//! object. A JSON round-trip in between would reopen the gap the verification
//! exists to close. This is the same decoder [`crate::record_refs`] walks
//! external writes with, from the same always-on pure core.
//!
//! # Depth is 1, and it is not a recursion bound
//!
//! A set holds permissions, never other sets (§ Ecosystem reality's 2026-08-03
//! addendum item 1), so an include-shaped member is simply an **invalid
//! member** and is ignored like any other. There is no recursion here to
//! bound, no depth counter, and no way for a hostile authority to make one:
//! the only thing that reads a document is a caller that already holds the
//! NSID it asked for.

use std::collections::HashSet;

use ipld_core::ipld::Ipld;
use serde::{Deserialize, Serialize};

use crate::outbound::truncate_for_lexicon;

// ── Caps ─────────────────────────────────────────────────────────────────────
//
// Four constants, all hard-coded (§ Product invariants: a value no human
// chooses is a Rust constant, never a config knob).

/// The most `include:` scopes one authorization request may carry
/// (`atproto-pds-full.md:331`).
///
/// Observed practice is 1–3; eight is generous for a real client and small
/// enough that a cold-cache PAR costs a bounded number of resolution chains.
/// Beyond it the whole request refuses `invalid_scope` — enforced by PS-b at
/// PAR, where the request's scope list is known.
pub const MAX_INCLUDES_PER_REQUEST: u32 = 8;

/// The most members one set's document may declare.
///
/// The spec motivates "dozens or even hundreds of individual permissions" and
/// specifies no limit of its own, so this is ours: high enough that no honest
/// set meets it, low enough to bound the work a single hostile document can
/// ask for. Over it the set **refuses** rather than truncating — a truncated
/// expansion would silently narrow a grant, and a grant that quietly means
/// less than the card said is the dishonesty this whole design avoids.
pub const MAX_MEMBERS_PER_SET: u32 = 256;

/// The most expanded scopes one grant may carry, across every include.
///
/// The access token carries the frozen expansion (`atproto-pds-full.md:329`
/// consequence 2), so this is a token-size bound. It is refused at PAR, which
/// is what makes an oversized request fail at the developer's desk rather than
/// mid-session.
pub const MAX_EXPANDED_SCOPES_PER_GRANT: u32 = 128;

/// The most expanded-scope **bytes** one grant may carry, across every include.
///
/// A count cap alone is not a cap: scope strings vary by an order of magnitude
/// (`repo:com.example.event` is 22 bytes, an `rpc:` scope naming a long method
/// and a `did:web` audience is over 80), so 128 of the long kind is ~10 KB and
/// 128 of the short kind is under 3. **A cap is a pair** — the lesson the
/// projection's own lexicon string caps taught the expensive way, where a
/// grapheme cap shipped without its byte twin published records no PDS on the
/// network would accept (§ Implementation status today, 2026-08-03).
///
/// 8 KiB is sized against the *ecosystem's* header budget rather than our own:
/// this bridge's listener takes Go's 1 MiB default, but a deployment behind an
/// ordinary reverse proxy meets an 8 KiB per-header limit long before that, and
/// a grant that works only when nothing sits in front of the box is not a grant
/// we should mint.
///
/// The round-trip through our own listener is pinned (2026-08-03) by
/// `TestMaxSizeGrantTokenClearsTheProductionListener` (`cmd/fauna-atproto-bridge/pdsmux_test.go`),
/// which asks THIS function where the boundary is rather than transcribing the
/// number: a grant at the cap renders 8167 bytes of scope into an 11,585-byte
/// `Authorization` header, served with room. Changing the constant re-derives
/// the fixture automatically; it is the listener, not this value, that the pin
/// is watching.
pub const MAX_EXPANDED_SCOPE_BYTES_PER_GRANT: u32 = 8192;

/// The most **graphemes**, then bytes, of a set's `title` that reach a card.
///
/// `title`/`details` are the one input on this path the scope grammar never
/// looks at: they are copied off a third party's published record, and until
/// 2026-08-03 the only thing bounding them was the 1 MiB proof fetch — so a
/// 60 KB title cleared PAR and then killed the ceremony at the nest's storage
/// ceiling (`atproto-pds-full.md:329` requires the opposite order).
///
/// They are **truncated, not refused**, and that is the opposite call from
/// [`MAX_MEMBERS_PER_SET`] one screen up — deliberately. A truncated *member*
/// list silently narrows a grant, which is the dishonesty this design exists to
/// avoid; `title`/`details` are **descriptive, not grant-bearing**, so cutting
/// them narrows no grant and hides no capability, while refusing on their
/// account would let a publisher's long prose break a user's sign-in.
///
/// The numbers are the ecosystem's own caps for the same class of text —
/// `app.bsky.actor.profile`'s `displayName` (64/640) and `description`
/// (256/2560). They are a **pair** because a cap is a pair: the grapheme half
/// alone let 300 family-emoji clusters render 7500 bytes past a 3000-byte
/// ceiling, the bug § Implementation status today records for 2026-08-03, and
/// this text is attacker-authored, where a one-axis cut is not an accident but
/// an invitation.
///
/// ⚠ **Load-bearing beyond this cap: `byte_cap >= 6 * grapheme_cap` must hold
/// for both pairs, or the nest's JSON allowance stops covering the payload.**
/// These strings cross to the nest **raw** — control characters included, since
/// the strip fence is at render — and `serde_json` encodes a control character
/// as `\u00XX`, 6 bytes from 1. Every escapable byte is ASCII, hence one byte
/// *and* one grapheme, so a field's largest encoding is
/// `max(byte_cap, 6 * grapheme_cap)`. At 10× (both pairs today) the byte cap
/// dominates and escaping yields nothing past what `check_sets_payload` already
/// counted; `MAX_CONSENT_SETS_JSON_ENCODING_ALLOWANCE` therefore budgets only
/// JSON *structure*. **Tightening a byte cap toward its grapheme cap reads as
/// more conservative and inverts this** — at 64/64 and 256/256 the escape gain
/// is 1,600 bytes per set, 12,800 across `MAX_INCLUDES_PER_REQUEST`, against
/// ~4,700 bytes of headroom, and the ceremony dies at `/oauth/authorize` after
/// the client holds a `request_uri` — which is the same problem again. Change these and
/// the allowance must be re-derived with it.
pub const MAX_SET_TITLE_GRAPHEMES: usize = 64;
/// Byte half of [`MAX_SET_TITLE_GRAPHEMES`] — see there.
pub const MAX_SET_TITLE_BYTES: usize = 640;
/// Grapheme cap on a set's `details` — see [`MAX_SET_TITLE_GRAPHEMES`].
pub const MAX_SET_DETAILS_GRAPHEMES: usize = 256;
/// Byte half of [`MAX_SET_DETAILS_GRAPHEMES`] — see there.
pub const MAX_SET_DETAILS_BYTES: usize = 2560;

/// The most **rendered bytes** the whole set payload of one grant may carry:
/// every set's NSID, title, details and repeated member list.
///
/// This is the second half of the same limit, and it needs no hostile input at all.
/// [`MAX_EXPANDED_SCOPE_BYTES_PER_GRANT`] measures the **deduped union** — the
/// list the token carries — while the set payload repeats each member under its
/// own set entry. The hierarchy constraint bounds a set's *namespace*, not the
/// overlap between sets, so a family of related sets in one namespace legally
/// carries up to [`MAX_INCLUDES_PER_REQUEST`] copies of the union.
///
/// So it is **derived, not chosen**: the worst case the existing caps already
/// admit. Sizing it below that would be a new refusal invented on this path,
/// and sizing it above would leave the nest's ceiling guessing again. It is
/// measured at PAR by [`check_sets_payload`], which is what turns an
/// arithmetic drift here into an honest `invalid_scope` at the developer's
/// desk instead of a 502 the user meets mid-ceremony.
pub const MAX_SET_PAYLOAD_BYTES_PER_GRANT: usize = MAX_INCLUDES_PER_REQUEST as usize
    * (MAX_NSID_LEN
        + MAX_SET_TITLE_BYTES
        + MAX_SET_DETAILS_BYTES
        + MAX_EXPANDED_SCOPE_BYTES_PER_GRANT as usize);

/// The longest an NSID may be (the Lexicon spec's own bound).
const MAX_NSID_LEN: usize = 317;

/// The longest one dot-separated NSID segment may be (a DNS label).
const MAX_NSID_SEGMENT_LEN: usize = 63;

/// The scope prefix this module owns.
const INCLUDE_PREFIX: &str = "include:";

// ── The `include:` grammar ───────────────────────────────────────────────────

/// A well-formed `include:` scope, split into the set it names and the audience
/// the invocation supplies to `inheritAud` members.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Record))]
pub struct ParsedInclude {
    /// The permission set's NSID, syntax-valid by construction — this is the
    /// only string PS-b may build a resolution chain from.
    pub nsid: String,
    /// The `?aud=` parameter, percent-decoded; `None` when the invocation
    /// supplied none. Members declaring `inheritAud` take this value.
    pub aud: Option<String>,
}

/// What a scope string turned out to be.
///
/// Three outcomes rather than `Option<Result<…>>` because the caller must tell
/// "not my business" from "yours and malformed": a scope that is not an
/// include falls through to the ordinary grammar, while a malformed include
/// **refuses the whole authorization request** (`atproto-pds-full.md:331`).
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Enum))]
pub enum IncludeScope {
    /// Not an `include:` scope. Hand it to the ordinary scope grammar.
    NotAnInclude,
    /// A well-formed include, ready to resolve.
    Parsed { include: ParsedInclude },
    /// An include this server will not act on. `reason` is the diagnostic that
    /// rides the `invalid_scope` refusal to the client developer.
    Refused { reason: String },
}

/// Parse an `include:<NSID>[?aud=<audience>]` scope.
///
/// **Every refusal here happens before any I/O**, which is the point: Go
/// fetches only what this parser emitted, so no caller-controlled string
/// reaches DNS or an HTTPS request without having been through
/// [`nsid_syntax_error`] first.
///
/// Unknown query parameters **refuse** rather than being ignored — the
/// loopback-client rule (`oauth_client::synthesize_loopback_client`), for the
/// same reason: an unrecognized parameter means the client believes it
/// configured something this server did not read, and a permission scope is
/// the last place to let that pass quietly.
#[cfg_attr(feature = "uniffi", uniffi::export)]
pub fn parse_include_scope(scope: String) -> IncludeScope {
    let Some(rest) = scope.strip_prefix(INCLUDE_PREFIX) else {
        return IncludeScope::NotAnInclude;
    };

    let (nsid, query) = match rest.split_once('?') {
        Some((nsid, query)) => (nsid, Some(query)),
        None => (rest, None),
    };

    if let Some(err) = nsid_syntax_error(nsid) {
        return IncludeScope::Refused {
            reason: format!("permission set `{nsid}` is not a valid NSID: {err}"),
        };
    }

    let mut aud: Option<String> = None;
    for pair in query.unwrap_or("").split('&').filter(|p| !p.is_empty()) {
        let (key, raw) = pair.split_once('=').unwrap_or((pair, ""));
        match key {
            "aud" => {
                if aud.is_some() {
                    return IncludeScope::Refused {
                        reason: format!(
                            "permission set `{nsid}` names more than one `aud` parameter"
                        ),
                    };
                }
                let value = crate::authz::percent_decode(raw);
                if let Some(err) = audience_syntax_error(&value) {
                    return IncludeScope::Refused {
                        reason: format!("permission set `{nsid}` has an invalid `aud`: {err}"),
                    };
                }
                aud = Some(value);
            }
            other => {
                return IncludeScope::Refused {
                    reason: format!(
                        "permission set `{nsid}` names unsupported parameter `{other}`"
                    ),
                };
            }
        }
    }

    IncludeScope::Parsed {
        include: ParsedInclude {
            nsid: nsid.to_string(),
            aud,
        },
    }
}

// ── Expansion ────────────────────────────────────────────────────────────────

/// Why one member of a set contributed nothing.
///
/// Every variant is carried out as **data** rather than dropped, because the
/// set's author is a third party who cannot see our logs and the client
/// developer cannot see the document: a set that expands to less than its
/// author intended must be diagnosable from both ends
/// (`atproto-pds-full.md:333` — "every ignore surfaced as data for logs and
/// diagnostics, never silently").
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Enum))]
pub enum IgnoreReason {
    /// The member was not a map at all.
    NotAnObject,
    /// No readable resource kind — see [`member_kind`] for the two shapes read.
    UnknownResourceKind,
    /// A member naming another permission set. Depth is 1 by construction, so
    /// this is an invalid member, not a recursion to follow.
    NestedInclude,
    /// A member declaring both `inheritAud` and its own `aud`. The spec's own
    /// words: "is invalid (and should be ignored)".
    InheritAudWithOwnAud,
    /// A member declaring `inheritAud` where the invocation supplied no `?aud=`
    /// to inherit. There is nothing to render it into.
    InheritAudWithoutInvocationAud,
    /// An `rpc` member with no audience from either source.
    RpcMemberWithoutAudience,
    /// The member named a resource outside the set's own NSID namespace — a
    /// sibling group or a parent. This is the constraint that confines a
    /// hostile or compromised authority to widening only within its own
    /// namespace.
    OutsideSetNamespace,
    /// The member named a wildcard resource. A wildcard necessarily reaches
    /// outside the set's namespace, so no set can express one — called out
    /// separately from [`Self::MalformedResourceName`] because a set author who
    /// tries it deserves to be told why rather than "that is not an NSID".
    WildcardResource,
    /// The member's resource name was not a syntactically valid NSID.
    MalformedResourceName,
    /// The member declared a readable kind but named no resource at all.
    NoResourceNamed,
    /// The member expanded to a syntactically fine scope that this server's
    /// authorization matrix grants nothing under.
    ///
    /// Recorded by the PAR step rather than by [`expand_permission_set`] — this
    /// module knows the `include:` grammar, not what D8 will do with a granular
    /// scope — but it belongs in the same list for the same reason every other
    /// variant does: a member that reaches neither the token nor the card must
    /// be diagnosable, and the alternative is a set whose card is quietly
    /// shorter than its document with nothing saying why.
    ///
    /// ⚠ Its [`IgnoredMember::index`] is the scope's position in
    /// [`ExpandedSet::members`], **not** in the document's `permissions` array:
    /// expansion deduplicates, so the two stop coinciding, and the coordinate a
    /// post-expansion refusal can honestly name is the expansion's. The scope
    /// string itself rides `detail`, which is the diagnostic that matters.
    NotGrantable,
    /// A `repo` member declaring an `action` (or `actions`) restriction. The
    /// scope grammar this server evaluates has no action-qualified `repo:` form
    /// yet, and the bare `repo:<collection>` the expander would otherwise emit
    /// means create, edit *and* delete — so carrying the member would widen it
    /// past what its author declared and past what the consent card could say
    /// honestly. Fail closed, like any other parameter the expander does not
    /// read; widening later (a qualified scope, with `describe_scope` and the
    /// matrix learning it in the same change) is additive.
    ActionRestricted,
}

/// One member that contributed nothing, and why.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Record))]
pub struct IgnoredMember {
    /// The member's zero-based position in the document's `permissions` array,
    /// so a diagnostic can point at it without quoting attacker-authored text.
    pub index: u32,
    /// The member's declared resource kind, or the empty string when that is
    /// itself what could not be read.
    pub kind: String,
    /// Why it was ignored.
    pub reason: IgnoreReason,
    /// The offending value, when naming one helps and quoting it is bounded
    /// (a resource name, never free-form document text). Empty otherwise.
    pub detail: String,
}

/// A resolved permission set, expanded.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Record))]
pub struct ExpandedSet {
    /// The set's NSID — the identity the card renders **verbatim**.
    pub nsid: String,
    /// The set's human title, **raw**. Attacker-authored: the control-strip
    /// fence is machine composition's, not this module's.
    pub title: Option<String>,
    /// The set's human description, **raw**. Same fence, same reason.
    pub details: Option<String>,
    /// The expansion: ordinary granular scopes, deduplicated, in document
    /// order.
    pub members: Vec<String>,
    /// Every member that contributed nothing, with its reason.
    pub ignored: Vec<IgnoredMember>,
}

/// The outcome of expanding one set.
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Enum))]
pub enum Expansion {
    /// The set expanded. It may still have expanded to **nothing** — an empty
    /// `members` with the reasons in `ignored`. That is not a refusal here:
    /// PS-b refuses it at PAR the way a dead scalar scope is refused
    /// (`atproto-pds-full.md:330`), and it needs the ignore reasons to say why.
    Expanded { set: ExpandedSet },
    /// The document could not be used at all.
    Refused { reason: String },
}

/// Expand a resolved permission set into ordinary granular scopes.
///
/// `record_dag_cbor` is the **verified** `com.atproto.lexicon.schema` record —
/// see the module docs on why the bytes cross verbatim.
///
/// # The document `id` must match the NSID that was asked for
///
/// A permission set lives on whatever PDS its authority's DID designates,
/// which is usually a large multi-tenant host — the party a prior ruling says to
/// verify rather than trust. The MST proof and commit signature prove *that
/// host published this record*; they do not prove it is the record for the NSID
/// the client named. Checking `id` is what closes the substitution: a host that
/// serves `com.evil.wide` under the rkey `com.example.narrow` gets a failed
/// resolution, not a silently wider grant.
#[cfg_attr(feature = "uniffi", uniffi::export)]
pub fn expand_permission_set(include: ParsedInclude, record_dag_cbor: Vec<u8>) -> Expansion {
    let Ok(record) = serde_ipld_dagcbor::from_slice::<Ipld>(&record_dag_cbor) else {
        return refused(&include.nsid, "the record is not decodable dag-cbor");
    };

    match string_at(field(&record, "id")) {
        Some(id) if id == include.nsid => {}
        Some(other) => {
            // Bounded quoting: `other` is attacker-authored, so it is rendered
            // through the same NSID validator before it can reach a log line.
            let shown = if nsid_syntax_error(other).is_none() {
                other
            } else {
                "<malformed>"
            };
            return refused(
                &include.nsid,
                &format!("the record published under this NSID declares `id` `{shown}`"),
            );
        }
        None => return refused(&include.nsid, "the record declares no `id`"),
    }

    // The primary definition. The rkey IS the full NSID, so `main` is the
    // definition the NSID names; a document whose set lives under another key
    // is not the one that was asked for.
    let Some(def) = field(&record, "defs").and_then(|defs| field(defs, "main")) else {
        return refused(&include.nsid, "the record declares no `defs.main`");
    };

    let members_ipld = match field(def, "permissions") {
        Some(Ipld::List(list)) => list.as_slice(),
        Some(_) => return refused(&include.nsid, "`permissions` is not a list"),
        // A set declaring no permissions is well-formed and grants nothing.
        // PS-b refuses it at PAR; here it is an honest empty expansion.
        None => &[],
    };

    if members_ipld.len() > MAX_MEMBERS_PER_SET as usize {
        return refused(
            &include.nsid,
            &format!(
                "declares {} members, over the {MAX_MEMBERS_PER_SET}-member limit",
                members_ipld.len()
            ),
        );
    }

    let group = namespace_group(&include.nsid);
    let mut members: Vec<String> = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();
    let mut ignored: Vec<IgnoredMember> = Vec::new();

    for (index, member) in members_ipld.iter().enumerate() {
        expand_member(
            index as u32,
            member,
            &group,
            include.aud.as_deref(),
            &mut members,
            &mut seen,
            &mut ignored,
        );
    }

    Expansion::Expanded {
        set: ExpandedSet {
            nsid: include.nsid,
            // Bounded where they are read. These two strings are the only
            // input on this path the scope grammar never inspects, and the
            // record they come from is a third party's — see
            // `MAX_SET_TITLE_GRAPHEMES` for why they truncate rather than
            // refuse, and why the cap is a grapheme/byte pair.
            title: string_at(field(def, "title"))
                .map(|t| truncate_for_lexicon(t, MAX_SET_TITLE_GRAPHEMES, MAX_SET_TITLE_BYTES)),
            details: string_at(field(def, "details"))
                .map(|d| truncate_for_lexicon(d, MAX_SET_DETAILS_GRAPHEMES, MAX_SET_DETAILS_BYTES)),
            members,
            ignored,
        },
    }
}

/// Expand one member, appending either scopes or an [`IgnoredMember`].
fn expand_member(
    index: u32,
    member: &Ipld,
    group: &str,
    invocation_aud: Option<&str>,
    members: &mut Vec<String>,
    seen: &mut HashSet<String>,
    ignored: &mut Vec<IgnoredMember>,
) {
    if !matches!(member, Ipld::Map(_)) {
        ignored.push(ignore(index, "", IgnoreReason::NotAnObject, ""));
        return;
    }

    let Some(kind) = member_kind(member) else {
        ignored.push(ignore(index, "", IgnoreReason::UnknownResourceKind, ""));
        return;
    };

    match kind {
        "repo" => {
            // Presence alone refuses, whatever the value's shape: a malformed or
            // empty restriction is still one this expander cannot honour.
            if field(member, "action").is_some() || field(member, "actions").is_some() {
                ignored.push(ignore(index, kind, IgnoreReason::ActionRestricted, ""));
                return;
            }
            let names = resource_names(member, &["collection", "collections"]);
            if names.is_empty() {
                ignored.push(ignore(index, kind, IgnoreReason::NoResourceNamed, ""));
                return;
            }
            for name in names {
                match resource_error(name, group) {
                    Some(reason) => ignored.push(ignore(index, kind, reason, name)),
                    None => push_scope(members, seen, format!("repo:{name}")),
                }
            }
        }
        "rpc" => {
            let inherit = matches!(field(member, "inheritAud"), Some(Ipld::Bool(true)));
            let own_aud = string_at(field(member, "aud")).filter(|a| !a.is_empty());

            let aud = match (inherit, own_aud) {
                // The spec's explicit invalid-member rule.
                (true, Some(_)) => {
                    ignored.push(ignore(index, kind, IgnoreReason::InheritAudWithOwnAud, ""));
                    return;
                }
                (true, None) => match invocation_aud {
                    Some(aud) => aud,
                    None => {
                        ignored.push(ignore(
                            index,
                            kind,
                            IgnoreReason::InheritAudWithoutInvocationAud,
                            "",
                        ));
                        return;
                    }
                },
                (false, Some(aud)) => aud,
                (false, None) => {
                    ignored.push(ignore(
                        index,
                        kind,
                        IgnoreReason::RpcMemberWithoutAudience,
                        "",
                    ));
                    return;
                }
            };

            let names = resource_names(member, &["lxm", "lxms"]);
            if names.is_empty() {
                ignored.push(ignore(index, kind, IgnoreReason::NoResourceNamed, ""));
                return;
            }
            let encoded_aud = percent_encode_audience(aud);
            for name in names {
                match resource_error(name, group) {
                    Some(reason) => ignored.push(ignore(index, kind, reason, name)),
                    None => push_scope(members, seen, format!("rpc:{name}?aud={encoded_aud}")),
                }
            }
        }
        // Depth is 1 by construction — a set holds permissions, not sets.
        "include" | "permission-set" | "permissionSet" => {
            ignored.push(ignore(index, kind, IgnoreReason::NestedInclude, ""));
        }
        // Everything else, `blob` included. `blob` is deliberately here rather
        // than implemented: its resource is a MIME pattern, which carries no
        // NSID for the hierarchy constraint to bind, so a `blob` member is a
        // permission a set author could not be confined within their own
        // namespace. Fail-closed until the spec says otherwise — widening later
        // is additive, and wrongly granting is not.
        other => {
            ignored.push(ignore(index, other, IgnoreReason::UnknownResourceKind, ""));
        }
    }
}

/// The member's resource kind, read from either plausible discriminator
/// placement.
///
/// The design pass's sources (§ Ecosystem reality's 2026-08-03 addendum)
/// establish that members are permission objects of kinds `repo` and `rpc`, but
/// do not transcribe which field names them: the proposal shape puts the kind
/// in `resource` with `type: "permission"`, while a plain-lexicon reading puts
/// it in `type`. Reading both is deliberate, and it is safe rather than sloppy
/// — whichever field carried the kind, the member is still confined by the
/// hierarchy constraint to the set's own namespace and still judged
/// independently by D8, so a mis-parse can only ever produce scopes that were
/// already going to be gated. PS-b confirms the shape against a real published
/// set and narrows this (the first run's `ignored` reasons say which).
fn member_kind(member: &Ipld) -> Option<&str> {
    if let Some(resource) = string_at(field(member, "resource")).filter(|r| !r.is_empty()) {
        return Some(resource);
    }
    match string_at(field(member, "type")).filter(|t| !t.is_empty()) {
        // `type: "permission"` is the wrapper, not the kind — a document using
        // that shape without a `resource` names nothing.
        Some("permission") | None => None,
        Some(other) => Some(other),
    }
}

/// The resource names a member declares, from a string or a list of strings.
///
/// Both spellings are read for the same reason [`member_kind`] reads two
/// fields; a non-string entry in the list is skipped here and shows up as an
/// empty result (hence [`IgnoreReason::NoResourceNamed`]) when it was the only
/// one.
fn resource_names<'a>(member: &'a Ipld, keys: &[&str]) -> Vec<&'a str> {
    for key in keys {
        match field(member, key) {
            Some(Ipld::String(s)) if !s.is_empty() => return vec![s.as_str()],
            Some(Ipld::List(list)) => {
                let names: Vec<&str> = list
                    .iter()
                    .filter_map(|v| string_at(Some(v)))
                    .filter(|s| !s.is_empty())
                    .collect();
                if !names.is_empty() {
                    return names;
                }
            }
            _ => {}
        }
    }
    Vec::new()
}

/// Why this resource name may not be expanded, or `None` when it may.
fn resource_error(name: &str, group: &str) -> Option<IgnoreReason> {
    if name.contains('*') {
        return Some(IgnoreReason::WildcardResource);
    }
    if nsid_syntax_error(name).is_some() {
        return Some(IgnoreReason::MalformedResourceName);
    }
    if !within_namespace(name, group) {
        return Some(IgnoreReason::OutsideSetNamespace);
    }
    None
}

// ── The hierarchy constraint ─────────────────────────────────────────────────

/// The set's namespace group: every segment but the last.
///
/// `com.example.calendar.appPerms` → `com.example.calendar`.
fn namespace_group(set_nsid: &str) -> String {
    match set_nsid.rsplit_once('.') {
        Some((group, _name)) => group.to_string(),
        // Unreachable for a syntax-valid NSID (three segments minimum), and
        // an empty group would match nothing, which is the safe direction.
        None => String::new(),
    }
}

/// Is this resource under the set's own namespace — same group or deeper?
///
/// The spec's words (§ Ecosystem reality addendum item 2): permissions may
/// reference resources "under the same NSID namespace as the set itself", the
/// same group and its children "recursively deep", never "sibling groups" or
/// parents. So the test is a **segment-boundary** prefix match, which is what
/// keeps `com.example.calendarium.event` out of `com.example.calendar`'s
/// namespace — a plain `starts_with` would have let a sibling group in through
/// nothing more than a longer name.
fn within_namespace(resource: &str, group: &str) -> bool {
    if group.is_empty() {
        return false;
    }
    resource
        .strip_prefix(group)
        .and_then(|rest| rest.strip_prefix('.'))
        .is_some_and(|rest| !rest.is_empty())
}

// ── NSID syntax ──────────────────────────────────────────────────────────────

/// Why this string is not a usable NSID, or `None` when it is.
///
/// This is a **fetch-safety gate first and a spec oracle second**, and the two
/// pull in opposite directions on the margins. What it must guarantee is that
/// nothing caller-controlled reaches a DNS query or an HTTPS path with a
/// character that could mean something there: hence the closed character class
/// (ASCII alphanumerics and `-`, dot-separated), the DNS label lengths, and the
/// overall bound.
///
/// Within that safe class it is deliberately **permissive** — notably the name
/// segment may carry digits. The asymmetry is why: refusing a valid NSID is a
/// functionality gap a user meets as "this permission set cannot be granted",
/// discovered at F5 if at all; accepting one the registry does not consider
/// valid costs a resolution that fails, which is self-correcting and lands as
/// the ordinary unresolvable-set refusal.
fn nsid_syntax_error(nsid: &str) -> Option<String> {
    if nsid.is_empty() {
        return Some("empty".to_string());
    }
    if nsid.len() > MAX_NSID_LEN {
        return Some(format!("longer than {MAX_NSID_LEN} characters"));
    }
    let segments: Vec<&str> = nsid.split('.').collect();
    if segments.len() < 3 {
        return Some("fewer than three dot-separated segments".to_string());
    }
    for segment in &segments {
        if segment.is_empty() {
            return Some("has an empty segment".to_string());
        }
        if segment.len() > MAX_NSID_SEGMENT_LEN {
            return Some(format!(
                "has a segment over {MAX_NSID_SEGMENT_LEN} characters"
            ));
        }
        if !segment
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-')
        {
            return Some("has a segment outside [a-zA-Z0-9-]".to_string());
        }
        if segment.starts_with('-') || segment.ends_with('-') {
            return Some("has a segment starting or ending with `-`".to_string());
        }
    }
    // The first segment is the authority's TLD once reversed, and a TLD may not
    // begin with a digit. The last segment is the name, which must begin with a
    // letter so it is unambiguous as a record key.
    if !segments[0].starts_with(|c: char| c.is_ascii_alphabetic()) {
        return Some("starts with a non-letter".to_string());
    }
    if !segments[segments.len() - 1].starts_with(|c: char| c.is_ascii_alphabetic()) {
        return Some("has a name segment starting with a non-letter".to_string());
    }
    None
}

/// Why this `?aud=` value is unusable, or `None` when it is.
///
/// Deliberately a *safety* check and not a DID grammar: the audience flows into
/// an `rpc:…?aud=…` scope that [`crate::authz`] pattern-matches and
/// [`crate::authz::describe_scope`] renders, so what matters here is that it
/// cannot carry control characters into a token, a log line or a consent card,
/// nor whitespace into a space-separated scope list. An audience that is
/// well-formed but names nothing this server proxies to simply grants nothing —
/// D8's answer, not this module's.
fn audience_syntax_error(aud: &str) -> Option<String> {
    if aud.is_empty() {
        return Some("empty".to_string());
    }
    if aud.chars().any(|c| c.is_control() || c.is_whitespace()) {
        return Some("contains whitespace or control characters".to_string());
    }
    None
}

// ── Grant-level caps ─────────────────────────────────────────────────────────

/// Whether one grant's whole expansion fits the caps.
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Enum))]
pub enum GrantExpansion {
    /// Within every cap.
    WithinCaps,
    /// Over one of them; `reason` rides the PAR `invalid_scope` refusal.
    Refused { reason: String },
}

/// The fan-out cap, as one owner with two callers.
///
/// [`check_grant_expansion`] applies it after expansion; `oauth_par`'s plan step
/// applies it **before any I/O**, which is where it actually protects something
/// — a request naming five hundred sets must cost zero resolution chains, not
/// five hundred followed by a refusal. Two spellings of the same number would
/// eventually disagree about which request is too wide.
pub(crate) fn include_count_refusal(include_count: u32) -> Option<String> {
    (include_count > MAX_INCLUDES_PER_REQUEST).then(|| {
        format!(
            "names {include_count} permission sets, over the \
             {MAX_INCLUDES_PER_REQUEST}-set limit for one request"
        )
    })
}

/// Check one authorization request's include count and total expansion against
/// the caps.
///
/// One face rather than three constants for PS-b to re-implement at PAR: the
/// caps and the sentence explaining a refusal belong to the same owner as the
/// grammar they bound. `expanded` is every scope the grant would carry —
/// expansion members and directly-requested scopes alike, since the token
/// carries one list and the byte budget does not care which is which.
#[cfg_attr(feature = "uniffi", uniffi::export)]
pub fn check_grant_expansion(include_count: u32, expanded: Vec<String>) -> GrantExpansion {
    if let Some(reason) = include_count_refusal(include_count) {
        return GrantExpansion::Refused { reason };
    }
    if expanded.len() > MAX_EXPANDED_SCOPES_PER_GRANT as usize {
        return GrantExpansion::Refused {
            reason: format!(
                "expands to {} scopes, over the {MAX_EXPANDED_SCOPES_PER_GRANT}-scope limit",
                expanded.len()
            ),
        };
    }
    // The rendered form: scopes reach the token space-separated, so the
    // separators are part of what has to fit.
    let bytes = expanded.iter().map(|s| s.len()).sum::<usize>() + expanded.len().saturating_sub(1);
    if bytes > MAX_EXPANDED_SCOPE_BYTES_PER_GRANT as usize {
        return GrantExpansion::Refused {
            reason: format!(
                "expands to {bytes} bytes of scope, over the \
                 {MAX_EXPANDED_SCOPE_BYTES_PER_GRANT}-byte limit"
            ),
        };
    }
    GrantExpansion::WithinCaps
}

/// Check the **set payload** of one grant against
/// [`MAX_SET_PAYLOAD_BYTES_PER_GRANT`].
///
/// [`check_grant_expansion`] one function up measures the deduped union — what
/// the *token* carries. This measures what the *card and the grant row* carry,
/// which is a different and larger thing: every member appears again under its
/// own set, plus the set's NSID and its author's two strings. Nothing else on
/// this path looks at that total, and until 2026-08-03 nothing did: the nest's
/// storage ceiling was the first component to measure it, three binaries later
/// and long after the client had been handed a `request_uri`.
///
/// The bound is derived from the caps that already exist, so a request the rest
/// of this module accepts cannot exceed it. That is deliberate — this is the
/// runtime half of an arithmetic claim, and its job is to turn a future drift
/// in that arithmetic into an honest `invalid_scope` at the developer's desk
/// rather than a 502 a user meets mid-ceremony.
///
/// `ignored` is not counted, because it does not cross the wire: it is
/// diagnostics about members that granted nothing, and attacker-authored.
pub fn check_sets_payload(sets: &[ExpandedSet]) -> GrantExpansion {
    let bytes: usize = sets.iter().map(rendered_set_bytes).sum();
    if bytes > MAX_SET_PAYLOAD_BYTES_PER_GRANT {
        return GrantExpansion::Refused {
            reason: format!(
                "carries {bytes} bytes of permission-set detail, over the \
                 {MAX_SET_PAYLOAD_BYTES_PER_GRANT}-byte limit"
            ),
        };
    }
    GrantExpansion::WithinCaps
}

/// The bytes one expanded set contributes to the payload the nest stores.
fn rendered_set_bytes(set: &ExpandedSet) -> usize {
    set.nsid.len()
        + set.title.as_deref().map_or(0, str::len)
        + set.details.as_deref().map_or(0, str::len)
        + set.members.iter().map(String::len).sum::<usize>()
}

// ── Small helpers ────────────────────────────────────────────────────────────

/// Percent-encode an audience for the `rpc:<lxm>?aud=<aud>` grammar.
///
/// The unreserved set keeps a DID readable (`did:web:api.bsky.app`); everything
/// else is encoded, which in practice means the `#` of a service fragment
/// becoming `%23` — exactly the form `authz::GRANTABLE_SCOPES`' exemplar
/// carries, and the form `authz`'s own `percent_decode` reverses.
fn percent_encode_audience(aud: &str) -> String {
    let mut out = String::with_capacity(aud.len());
    for byte in aud.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'.' | b'_' | b'~' | b':') {
            out.push(byte as char);
        } else {
            out.push_str(&format!("%{byte:02X}"));
        }
    }
    out
}

/// Append a scope, skipping duplicates so a document that names the same
/// resource twice does not pay for it twice in the token budget.
///
/// `seen` is a hash set beside the ordered list: a `Vec::contains` scan is
/// quadratic over one member naming tens of thousands of resources, and that
/// runs at PAR before the grant-level caps apply.
fn push_scope(members: &mut Vec<String>, seen: &mut HashSet<String>, scope: String) {
    if seen.insert(scope.clone()) {
        members.push(scope);
    }
}

fn ignore(index: u32, kind: &str, reason: IgnoreReason, detail: &str) -> IgnoredMember {
    IgnoredMember {
        index,
        kind: kind.to_string(),
        reason,
        detail: detail.to_string(),
    }
}

fn refused(nsid: &str, why: &str) -> Expansion {
    Expansion::Refused {
        reason: format!("permission set `{nsid}` could not be expanded: {why}"),
    }
}

/// Read a map field, or `None` when the value is any other IPLD kind.
fn field<'a>(value: &'a Ipld, key: &str) -> Option<&'a Ipld> {
    match value {
        Ipld::Map(map) => map.get(key),
        _ => None,
    }
}

/// Read a string value, or `None` when it is absent or any other IPLD kind.
fn string_at(value: Option<&Ipld>) -> Option<&str> {
    match value {
        Some(Ipld::String(s)) => Some(s.as_str()),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// A permission-set document, dag-cbor encoded the way the verified record
    /// arrives. Built from JSON so the fixtures read like the documents they
    /// model.
    fn document(value: serde_json::Value) -> Vec<u8> {
        let ipld: Ipld = serde_json::from_value(value).expect("fixture is valid IPLD");
        serde_ipld_dagcbor::to_vec(&ipld).expect("fixture encodes")
    }

    /// The exemplar set: `com.example.calendar.appPerms`, group
    /// `com.example.calendar`.
    fn calendar_set(permissions: serde_json::Value) -> Vec<u8> {
        document(json!({
            "lexicon": 1,
            "id": "com.example.calendar.appPerms",
            "defs": {
                "main": {
                    "type": "permission-set",
                    "title": "Calendar access",
                    "details": "Read and write your calendar events.",
                    "permissions": permissions,
                }
            }
        }))
    }

    fn include(aud: Option<&str>) -> ParsedInclude {
        ParsedInclude {
            nsid: "com.example.calendar.appPerms".to_string(),
            aud: aud.map(str::to_string),
        }
    }

    fn expanded(expansion: Expansion) -> ExpandedSet {
        match expansion {
            Expansion::Expanded { set } => set,
            Expansion::Refused { reason } => panic!("expected an expansion, got refusal: {reason}"),
        }
    }

    fn refusal(expansion: Expansion) -> String {
        match expansion {
            Expansion::Refused { reason } => reason,
            Expansion::Expanded { set } => panic!("expected a refusal, got {set:?}"),
        }
    }

    fn reasons(set: &ExpandedSet) -> Vec<IgnoreReason> {
        set.ignored.iter().map(|i| i.reason).collect()
    }

    // ── The grammar ──────────────────────────────────────────────────────────

    #[test]
    fn an_ordinary_scope_is_not_an_include() {
        assert_eq!(
            parse_include_scope("repo:com.example.calendar.event".to_string()),
            IncludeScope::NotAnInclude
        );
        assert_eq!(
            parse_include_scope("atproto".to_string()),
            IncludeScope::NotAnInclude
        );
    }

    #[test]
    fn a_bare_include_parses_without_an_audience() {
        assert_eq!(
            parse_include_scope("include:com.example.calendar.appPerms".to_string()),
            IncludeScope::Parsed {
                include: include(None)
            }
        );
    }

    #[test]
    fn an_include_carries_its_percent_decoded_audience() {
        assert_eq!(
            parse_include_scope(
                "include:com.example.calendar.appPerms?aud=did:web:api.bsky.app%23bsky_appview"
                    .to_string()
            ),
            IncludeScope::Parsed {
                include: ParsedInclude {
                    nsid: "com.example.calendar.appPerms".to_string(),
                    aud: Some("did:web:api.bsky.app#bsky_appview".to_string()),
                }
            }
        );
    }

    /// The loopback-client rule, in the one place a silent read would be worst.
    #[test]
    fn an_unknown_query_parameter_refuses_the_scope() {
        let verdict =
            parse_include_scope("include:com.example.calendar.appPerms?depth=2".to_string());
        let IncludeScope::Refused { reason } = verdict else {
            panic!("expected a refusal, got {verdict:?}");
        };
        assert!(reason.contains("depth"), "{reason}");
    }

    #[test]
    fn a_repeated_audience_refuses_the_scope() {
        assert!(matches!(
            parse_include_scope(
                "include:com.example.calendar.appPerms?aud=did:web:a&aud=did:web:b".to_string()
            ),
            IncludeScope::Refused { .. }
        ));
    }

    #[test]
    fn an_empty_or_control_bearing_audience_refuses_the_scope() {
        assert!(matches!(
            parse_include_scope("include:com.example.calendar.appPerms?aud=".to_string()),
            IncludeScope::Refused { .. }
        ));
        assert!(matches!(
            parse_include_scope(
                "include:com.example.calendar.appPerms?aud=did:web:a%09b".to_string()
            ),
            IncludeScope::Refused { .. }
        ));
    }

    /// Every one of these refuses **before** anything could be fetched, which
    /// is the property the whole parse-before-I/O split rests on.
    #[test]
    fn a_malformed_nsid_refuses_before_any_io() {
        for bad in [
            "",
            "com.example",                        // too few segments
            "com..appPerms",                      // empty segment
            "com.example.calendar.appPerms/../",  // path escape
            "com.example.calendar.app_Perms",     // outside the character class
            "com.example.calendar.-appPerms",     // leading hyphen
            "com.example.calendar.appPerms-",     // trailing hyphen
            "1com.example.appPerms",              // TLD starting with a digit
            "com.example.calendar.2fa",           // name starting with a digit
            "com.example.calendar.app Perms",     // whitespace
            "com.example.calendar.app\u{0}Perms", // NUL
        ] {
            assert!(
                matches!(
                    parse_include_scope(format!("include:{bad}")),
                    IncludeScope::Refused { .. }
                ),
                "expected `{bad}` to refuse"
            );
        }
    }

    #[test]
    fn an_nsid_at_the_length_bound_is_accepted_and_past_it_refuses() {
        let long_name = "a".repeat(MAX_NSID_SEGMENT_LEN);
        let ok = format!("com.example.{long_name}");
        assert!(matches!(
            parse_include_scope(format!("include:{ok}")),
            IncludeScope::Parsed { .. }
        ));
        let too_long = format!("com.example.a{}", "b".repeat(MAX_NSID_SEGMENT_LEN));
        assert!(matches!(
            parse_include_scope(format!("include:{too_long}")),
            IncludeScope::Refused { .. }
        ));
    }

    // ── Expansion, the happy path ────────────────────────────────────────────

    #[test]
    fn repo_and_rpc_members_expand_to_ordinary_granular_scopes() {
        let doc = calendar_set(json!([
            {"type": "permission", "resource": "repo",
             "collection": ["com.example.calendar.event", "com.example.calendar.rsvp"]},
            {"type": "permission", "resource": "rpc",
             "lxm": ["com.example.calendar.sync.push"], "aud": "did:web:svc.example"},
        ]));
        let set = expanded(expand_permission_set(include(None), doc));

        assert_eq!(
            set.members,
            vec![
                "repo:com.example.calendar.event",
                "repo:com.example.calendar.rsvp",
                "rpc:com.example.calendar.sync.push?aud=did:web:svc.example",
            ]
        );
        assert!(set.ignored.is_empty(), "{:?}", set.ignored);
        assert_eq!(set.nsid, "com.example.calendar.appPerms");
    }

    /// A member's own `aud` is attacker-authored and, unlike the invocation's,
    /// is never syntax-checked — the encoder is what makes that safe. Scopes
    /// travel space-separated and `?`/`&` delimit the grammar, so a member
    /// audience carrying any of them must not be able to forge a second scope
    /// or a second parameter out of one.
    #[test]
    fn a_hostile_member_audience_cannot_inject_a_separator_into_the_scope() {
        let doc = calendar_set(json!([
            {"resource": "rpc", "lxm": "com.example.calendar.sync.push",
             "aud": "did:web:a repo:com.evil.wide.everything"},
            {"resource": "rpc", "lxm": "com.example.calendar.sync.pull",
             "aud": "did:web:b&aud=did:web:c"},
            {"resource": "rpc", "lxm": "com.example.calendar.sync.probe",
             "aud": "did:web:d\u{a}\u{0}"},
        ]));
        let set = expanded(expand_permission_set(include(None), doc));

        assert_eq!(set.members.len(), 3, "{:?}", set.members);
        for scope in &set.members {
            assert!(
                !scope.contains(char::is_whitespace),
                "whitespace survived into {scope}"
            );
            assert!(
                !scope.chars().any(char::is_control),
                "a control character survived into {scope}"
            );
            // Exactly one `?aud=`, and nothing after it re-opens the grammar.
            let (_, aud) = scope.split_once("?aud=").expect("one aud clause");
            assert!(!aud.contains('&') && !aud.contains('?'), "{scope}");
        }
        // And the encoding is reversible: what D8 decodes is what the document
        // actually said, not a mangled version of it.
        let first = set.members[0].split_once("?aud=").unwrap().1;
        assert_eq!(
            crate::authz::percent_decode(first),
            "did:web:a repo:com.evil.wide.everything"
        );
    }

    /// The plain-lexicon discriminator placement, expanding identically.
    #[test]
    fn a_member_naming_its_kind_in_type_expands_the_same_way() {
        let doc = calendar_set(json!([
            {"type": "repo", "collection": "com.example.calendar.event"},
        ]));
        let set = expanded(expand_permission_set(include(None), doc));
        assert_eq!(set.members, vec!["repo:com.example.calendar.event"]);
    }

    #[test]
    fn inherit_aud_takes_the_audience_from_the_invocation() {
        let doc = calendar_set(json!([
            {"type": "permission", "resource": "rpc",
             "lxm": "com.example.calendar.sync.push", "inheritAud": true},
        ]));
        let set = expanded(expand_permission_set(
            include(Some("did:web:api.bsky.app#bsky_appview")),
            doc,
        ));
        assert_eq!(
            set.members,
            vec!["rpc:com.example.calendar.sync.push?aud=did:web:api.bsky.app%23bsky_appview"]
        );
    }

    /// The encoding this module emits is the encoding `authz` decodes — the two
    /// halves of one grammar, pinned together rather than separately.
    #[test]
    fn the_emitted_audience_round_trips_through_authzs_own_decoder() {
        let doc = calendar_set(json!([
            {"type": "permission", "resource": "rpc",
             "lxm": "com.example.calendar.sync.push", "inheritAud": true},
        ]));
        let set = expanded(expand_permission_set(
            include(Some("did:web:api.bsky.app#bsky_appview")),
            doc,
        ));
        let encoded = set.members[0]
            .split_once("?aud=")
            .expect("the scope carries an aud")
            .1;
        assert_eq!(
            crate::authz::percent_decode(encoded),
            "did:web:api.bsky.app#bsky_appview"
        );
    }

    #[test]
    fn title_and_details_cross_raw_because_the_strip_fence_is_downstream() {
        let doc = document(json!({
            "lexicon": 1,
            "id": "com.example.calendar.appPerms",
            "defs": {"main": {
                "title": "Calendar\u{7} access",
                "details": "Line one\u{1b}[31m",
                "permissions": [],
            }}
        }));
        let set = expanded(expand_permission_set(include(None), doc));
        assert_eq!(set.title.as_deref(), Some("Calendar\u{7} access"));
        assert_eq!(set.details.as_deref(), Some("Line one\u{1b}[31m"));
    }

    #[test]
    fn a_duplicate_resource_is_carried_once() {
        let doc = calendar_set(json!([
            {"resource": "repo", "collection": ["com.example.calendar.event"]},
            {"resource": "repo", "collection": ["com.example.calendar.event"]},
        ]));
        let set = expanded(expand_permission_set(include(None), doc));
        assert_eq!(set.members, vec!["repo:com.example.calendar.event"]);
    }

    // ── The hierarchy constraint ─────────────────────────────────────────────

    #[test]
    fn a_member_deeper_in_the_sets_own_namespace_expands() {
        let doc = calendar_set(json!([
            {"resource": "repo", "collection": "com.example.calendar.recurring.rule"},
        ]));
        let set = expanded(expand_permission_set(include(None), doc));
        assert_eq!(
            set.members,
            vec!["repo:com.example.calendar.recurring.rule"]
        );
    }

    #[test]
    fn a_sibling_group_member_is_ignored() {
        let doc = calendar_set(json!([
            {"resource": "repo", "collection": "com.example.contacts.card"},
        ]));
        let set = expanded(expand_permission_set(include(None), doc));
        assert!(set.members.is_empty(), "{:?}", set.members);
        assert_eq!(reasons(&set), vec![IgnoreReason::OutsideSetNamespace]);
        assert_eq!(set.ignored[0].detail, "com.example.contacts.card");
    }

    #[test]
    fn a_parent_namespace_member_is_ignored() {
        let doc = calendar_set(json!([
            {"resource": "repo", "collection": "com.example.thing"},
            {"resource": "rpc", "lxm": "com.atproto.server.createSession",
             "aud": "did:web:svc.example"},
        ]));
        let set = expanded(expand_permission_set(include(None), doc));
        assert!(set.members.is_empty(), "{:?}", set.members);
        assert_eq!(
            reasons(&set),
            vec![
                IgnoreReason::OutsideSetNamespace,
                IgnoreReason::OutsideSetNamespace
            ]
        );
    }

    /// A plain `starts_with` would have let this through: the sibling group
    /// `com.example.calendarium` shares a textual prefix with
    /// `com.example.calendar` and nothing else.
    #[test]
    fn a_longer_named_sibling_group_does_not_pass_the_prefix_test() {
        let doc = calendar_set(json!([
            {"resource": "repo", "collection": "com.example.calendarium.event"},
        ]));
        let set = expanded(expand_permission_set(include(None), doc));
        assert!(set.members.is_empty(), "{:?}", set.members);
        assert_eq!(reasons(&set), vec![IgnoreReason::OutsideSetNamespace]);
    }

    #[test]
    fn a_wildcard_resource_is_ignored_with_its_own_reason() {
        let doc = calendar_set(json!([
            {"resource": "repo", "collection": "*"},
            {"resource": "rpc", "lxm": "*", "aud": "did:web:svc.example"},
        ]));
        let set = expanded(expand_permission_set(include(None), doc));
        assert!(set.members.is_empty(), "{:?}", set.members);
        assert_eq!(
            reasons(&set),
            vec![
                IgnoreReason::WildcardResource,
                IgnoreReason::WildcardResource
            ]
        );
    }

    // ── The invalid-member ignore rules ──────────────────────────────────────

    #[test]
    fn a_member_declaring_both_inherit_aud_and_its_own_aud_is_ignored() {
        let doc = calendar_set(json!([
            {"resource": "rpc", "lxm": "com.example.calendar.sync.push",
             "inheritAud": true, "aud": "did:web:elsewhere.example"},
        ]));
        let set = expanded(expand_permission_set(
            include(Some("did:web:svc.example")),
            doc,
        ));
        assert!(set.members.is_empty(), "{:?}", set.members);
        assert_eq!(reasons(&set), vec![IgnoreReason::InheritAudWithOwnAud]);
    }

    #[test]
    fn inherit_aud_with_no_invocation_audience_is_ignored() {
        let doc = calendar_set(json!([
            {"resource": "rpc", "lxm": "com.example.calendar.sync.push", "inheritAud": true},
        ]));
        let set = expanded(expand_permission_set(include(None), doc));
        assert!(set.members.is_empty(), "{:?}", set.members);
        assert_eq!(
            reasons(&set),
            vec![IgnoreReason::InheritAudWithoutInvocationAud]
        );
    }

    #[test]
    fn an_rpc_member_with_no_audience_at_all_is_ignored() {
        let doc = calendar_set(json!([
            {"resource": "rpc", "lxm": "com.example.calendar.sync.push"},
        ]));
        let set = expanded(expand_permission_set(
            include(Some("did:web:svc.example")),
            doc,
        ));
        assert!(set.members.is_empty(), "{:?}", set.members);
        assert_eq!(reasons(&set), vec![IgnoreReason::RpcMemberWithoutAudience]);
    }

    /// Depth 1 by construction: an include-shaped member is invalid, not a
    /// recursion to follow.
    #[test]
    fn a_nested_include_member_is_ignored_not_followed() {
        let doc = calendar_set(json!([
            {"resource": "include", "nsid": "com.example.calendar.morePerms"},
            {"type": "permission-set", "nsid": "com.example.calendar.evenMore"},
        ]));
        let set = expanded(expand_permission_set(include(None), doc));
        assert!(set.members.is_empty(), "{:?}", set.members);
        assert_eq!(
            reasons(&set),
            vec![IgnoreReason::NestedInclude, IgnoreReason::NestedInclude]
        );
    }

    #[test]
    fn unknown_member_kinds_including_blob_are_ignored_with_the_kind_carried() {
        let doc = calendar_set(json!([
            {"resource": "blob", "accept": ["image/*"]},
            {"resource": "identity"},
            {"type": "permission"},
            "not an object",
        ]));
        let set = expanded(expand_permission_set(include(None), doc));
        assert!(set.members.is_empty(), "{:?}", set.members);
        assert_eq!(
            reasons(&set),
            vec![
                IgnoreReason::UnknownResourceKind,
                IgnoreReason::UnknownResourceKind,
                IgnoreReason::UnknownResourceKind,
                IgnoreReason::NotAnObject,
            ]
        );
        assert_eq!(set.ignored[0].kind, "blob");
        assert_eq!(set.ignored[1].kind, "identity");
    }

    #[test]
    fn a_member_naming_no_resource_is_ignored() {
        let doc = calendar_set(json!([
            {"resource": "repo"},
            {"resource": "repo", "collection": []},
            {"resource": "rpc", "aud": "did:web:svc.example"},
        ]));
        let set = expanded(expand_permission_set(include(None), doc));
        assert_eq!(
            reasons(&set),
            vec![
                IgnoreReason::NoResourceNamed,
                IgnoreReason::NoResourceNamed,
                IgnoreReason::NoResourceNamed
            ]
        );
    }

    /// A partly-hostile set still yields its honest members — the ignore rules
    /// narrow a set, they never poison it.
    #[test]
    fn honest_members_survive_alongside_ignored_ones_and_indices_point_at_them() {
        let doc = calendar_set(json!([
            {"resource": "repo", "collection": "com.example.contacts.card"},
            {"resource": "repo", "collection": "com.example.calendar.event"},
            {"resource": "rpc", "lxm": "com.example.calendar.sync.push", "inheritAud": true,
             "aud": "did:web:elsewhere.example"},
        ]));
        let set = expanded(expand_permission_set(
            include(Some("did:web:svc.example")),
            doc,
        ));
        assert_eq!(set.members, vec!["repo:com.example.calendar.event"]);
        assert_eq!(set.ignored.len(), 2);
        assert_eq!(set.ignored[0].index, 0);
        assert_eq!(set.ignored[1].index, 2);
    }

    // ── Document-level refusals ──────────────────────────────────────────────

    /// The substitution defense: an authenticated record is proof the host
    /// published it, never proof it is the set that was asked for.
    #[test]
    fn a_document_declaring_another_nsid_refuses() {
        let doc = document(json!({
            "lexicon": 1,
            "id": "com.evil.wide.appPerms",
            "defs": {"main": {"permissions": [
                {"resource": "repo", "collection": "com.evil.wide.everything"},
            ]}}
        }));
        let reason = refusal(expand_permission_set(include(None), doc));
        assert!(reason.contains("com.evil.wide.appPerms"), "{reason}");
    }

    /// The mismatching `id` is attacker-authored, so it is only quoted back
    /// once it has passed the same validator the requested NSID did.
    #[test]
    fn a_malformed_substituted_id_is_not_quoted_back_verbatim() {
        let doc = document(json!({
            "lexicon": 1,
            "id": "\u{1b}[31mnot an nsid",
            "defs": {"main": {"permissions": []}}
        }));
        let reason = refusal(expand_permission_set(include(None), doc));
        assert!(reason.contains("<malformed>"), "{reason}");
        assert!(!reason.contains('\u{1b}'), "{reason}");
    }

    #[test]
    fn a_document_with_no_id_or_no_main_def_refuses() {
        let no_id = document(json!({"defs": {"main": {"permissions": []}}}));
        assert!(refusal(expand_permission_set(include(None), no_id)).contains("no `id`"));

        let no_main = document(json!({
            "id": "com.example.calendar.appPerms",
            "defs": {"other": {"permissions": []}}
        }));
        assert!(refusal(expand_permission_set(include(None), no_main)).contains("defs.main"));
    }

    #[test]
    fn undecodable_bytes_refuse_rather_than_panicking() {
        let reason = refusal(expand_permission_set(include(None), vec![0xff, 0xff, 0xff]));
        assert!(reason.contains("dag-cbor"), "{reason}");
    }

    /// An empty expansion is a real, reportable outcome — PS-b refuses it at
    /// PAR, and it needs the reasons to say why.
    #[test]
    fn a_set_that_expands_to_nothing_still_expands_with_its_reasons() {
        let doc = calendar_set(json!([]));
        let set = expanded(expand_permission_set(include(None), doc));
        assert!(set.members.is_empty());
        assert!(set.ignored.is_empty());
    }

    // ── Action restriction (a member never widens past what it declares) ────

    /// The defect: `action: ["create"]` used to expand to a bare `repo:<nsid>`,
    /// which the consent card words as create, edit and delete.
    #[test]
    fn an_action_restricted_repo_member_is_refused_never_widened() {
        let doc = calendar_set(json!([
            {"resource": "repo", "collection": "com.example.calendar.event",
             "action": ["create"]},
            {"resource": "repo", "collection": "com.example.calendar.rsvp",
             "actions": "delete"},
            {"resource": "repo", "collection": "com.example.calendar.note",
             "action": []},
        ]));
        let set = expanded(expand_permission_set(include(None), doc));
        assert!(set.members.is_empty(), "{:?}", set.members);
        assert_eq!(
            reasons(&set),
            vec![
                IgnoreReason::ActionRestricted,
                IgnoreReason::ActionRestricted,
                IgnoreReason::ActionRestricted
            ]
        );
        assert_eq!(set.ignored[0].kind, "repo");
    }

    /// An unrestricted sibling still expands beside a restricted one.
    #[test]
    fn an_unrestricted_repo_member_still_expands_beside_a_restricted_one() {
        let doc = calendar_set(json!([
            {"resource": "repo", "collection": "com.example.calendar.event",
             "action": ["create"]},
            {"resource": "repo", "collection": "com.example.calendar.rsvp"},
        ]));
        let set = expanded(expand_permission_set(include(None), doc));
        assert_eq!(set.members, vec!["repo:com.example.calendar.rsvp"]);
        assert_eq!(reasons(&set), vec![IgnoreReason::ActionRestricted]);
    }

    /// Dedup is linear: one member naming tens of thousands of distinct
    /// collections (a 1 MiB hostile document) must not pay a quadratic
    /// `Vec::contains` per name, and repeats still collapse.
    #[test]
    fn a_hostile_large_member_expands_in_linear_time_and_dedups() {
        let mut names: Vec<String> = (0..40_000)
            .map(|i| format!("com.example.calendar.e{i}"))
            .collect();
        names.extend((0..1_000).map(|i| format!("com.example.calendar.e{i}")));
        let doc = calendar_set(json!([{"resource": "rpc", "aud": "did:web:svc.example",
            "lxms": names.clone()}, {"resource": "repo", "collections": names}]));
        let set = expanded(expand_permission_set(include(None), doc));
        assert_eq!(set.members.len(), 80_000);
    }

    // ── Caps ─────────────────────────────────────────────────────────────────

    #[test]
    fn a_set_at_the_member_cap_expands_and_past_it_refuses() {
        let at_cap: Vec<serde_json::Value> = (0..MAX_MEMBERS_PER_SET)
            .map(
                |i| json!({"resource": "repo", "collection": format!("com.example.calendar.e{i}")}),
            )
            .collect();
        let set = expanded(expand_permission_set(
            include(None),
            calendar_set(json!(at_cap)),
        ));
        assert_eq!(set.members.len(), MAX_MEMBERS_PER_SET as usize);

        let over_cap: Vec<serde_json::Value> = (0..MAX_MEMBERS_PER_SET + 1)
            .map(
                |i| json!({"resource": "repo", "collection": format!("com.example.calendar.e{i}")}),
            )
            .collect();
        let reason = refusal(expand_permission_set(
            include(None),
            calendar_set(json!(over_cap)),
        ));
        assert!(reason.contains("over the"), "{reason}");
    }

    /// Refuse, never truncate: a silently narrowed grant means less than the
    /// card said.
    #[test]
    fn an_oversized_set_refuses_rather_than_truncating() {
        let over_cap: Vec<serde_json::Value> = (0..MAX_MEMBERS_PER_SET + 1)
            .map(
                |i| json!({"resource": "repo", "collection": format!("com.example.calendar.e{i}")}),
            )
            .collect();
        assert!(matches!(
            expand_permission_set(include(None), calendar_set(json!(over_cap))),
            Expansion::Refused { .. }
        ));
    }

    #[test]
    fn the_include_fan_out_cap_refuses_the_request() {
        assert_eq!(
            check_grant_expansion(MAX_INCLUDES_PER_REQUEST, vec![]),
            GrantExpansion::WithinCaps
        );
        let GrantExpansion::Refused { reason } =
            check_grant_expansion(MAX_INCLUDES_PER_REQUEST + 1, vec![])
        else {
            panic!("expected a refusal over the include cap");
        };
        assert!(reason.contains("permission sets"), "{reason}");
    }

    #[test]
    fn the_grant_scope_count_cap_refuses_the_request() {
        let at_cap: Vec<String> = (0..MAX_EXPANDED_SCOPES_PER_GRANT)
            .map(|i| format!("repo:com.e.c.a{i}"))
            .collect();
        assert_eq!(check_grant_expansion(1, at_cap), GrantExpansion::WithinCaps);

        let over_cap: Vec<String> = (0..MAX_EXPANDED_SCOPES_PER_GRANT + 1)
            .map(|i| format!("repo:com.e.c.a{i}"))
            .collect();
        assert!(matches!(
            check_grant_expansion(1, over_cap),
            GrantExpansion::Refused { .. }
        ));
    }

    /// A cap is a pair. Few enough scopes to clear the count cap, long enough
    /// to blow the byte budget — the exact shape the count cap alone misses.
    #[test]
    fn the_byte_cap_catches_what_the_count_cap_cannot() {
        let long = format!("rpc:{}?aud=did:web:svc.example", "a".repeat(200));
        let few_but_long: Vec<String> = (0..64).map(|_| long.clone()).collect();
        assert!(few_but_long.len() < MAX_EXPANDED_SCOPES_PER_GRANT as usize);

        let GrantExpansion::Refused { reason } = check_grant_expansion(1, few_but_long) else {
            panic!("expected the byte cap to refuse");
        };
        assert!(reason.contains("bytes of scope"), "{reason}");
    }

    // ── The author's own strings, and the payload they ride in ───────────────

    /// A permission-set document with a chosen `title`/`details` — the two
    /// fields the scope grammar never inspects.
    fn calendar_set_described(title: &str, details: &str) -> Vec<u8> {
        document(json!({
            "lexicon": 1,
            "id": "com.example.calendar.appPerms",
            "defs": {"main": {
                "type": "permission-set",
                "title": title,
                "details": details,
                "permissions": [
                    {"resource": "repo", "collection": ["com.example.calendar.event"]},
                ],
            }},
        }))
    }

    /// The known limit: the one input here nothing upstream bounds. 60 KB of title used
    /// to clear PAR untouched and then kill the ceremony at the nest's storage
    /// ceiling.
    #[test]
    fn an_unbounded_title_is_truncated_where_it_is_read() {
        let set = expanded(expand_permission_set(
            include(None),
            calendar_set_described(&"T".repeat(60_000), &"D".repeat(60_000)),
        ));
        assert!(set.title.as_ref().unwrap().len() <= MAX_SET_TITLE_BYTES);
        assert!(set.details.as_ref().unwrap().len() <= MAX_SET_DETAILS_BYTES);
        // The grant itself is untouched — this cap is over description only.
        assert_eq!(set.members, vec!["repo:com.example.calendar.event"]);
    }

    /// The opposite call from [`MAX_MEMBERS_PER_SET`] two tests up, and the
    /// distinction is the point: a member list refuses because truncating it
    /// narrows a grant; these two strings truncate because they carry no
    /// capability, and refusing would let a publisher's prose break a sign-in.
    ///
    /// It asserts through `check_sets_payload`, not just the expander, because
    /// that is where a *lost* truncation would surface as a refusal — the
    /// expander alone never refuses on these strings either way, so stopping
    /// there would pin nothing. The size is taken **from the bound**: a
    /// description that would blow the payload cap on its own is the only kind
    /// whose fate distinguishes truncate from refuse.
    #[test]
    fn a_long_description_never_refuses_the_authorization_request() {
        let long = "D".repeat(MAX_SET_PAYLOAD_BYTES_PER_GRANT + 1);
        let set = expanded(expand_permission_set(
            include(None),
            calendar_set_described("ok", &long),
        ));
        assert_eq!(check_sets_payload(&[set]), GrantExpansion::WithinCaps);
    }

    /// A cap is a pair, on attacker-authored text most of all. Family-emoji
    /// clusters are 1 grapheme and 25 bytes each, so a grapheme-only cap admits
    /// 25× its byte budget — the bug the projection's lexicon caps taught the
    /// expensive way, on text a stranger writes.
    #[test]
    fn the_title_cap_holds_on_both_axes_and_splits_no_cluster() {
        let family = "\u{1F468}\u{200D}\u{1F469}\u{200D}\u{1F467}\u{200D}\u{1F466}";
        let set = expanded(expand_permission_set(
            include(None),
            calendar_set_described(&family.repeat(MAX_SET_TITLE_GRAPHEMES), "d"),
        ));
        let title = set.title.unwrap();
        assert!(
            title.len() <= MAX_SET_TITLE_BYTES,
            "{} bytes over the {MAX_SET_TITLE_BYTES}-byte half",
            title.len()
        );
        // Whole clusters only: a byte-indexed cut would leave a lone ZWJ tail.
        assert!(
            title.trim_end_matches(" [...]").ends_with(family)
                || title.trim_end_matches(" [...]").is_empty(),
            "a cluster was split: {title:?}"
        );
    }

    /// The half that needs no hostile input. `check_grant_expansion` measures
    /// the deduped union; the set payload repeats every member under its own
    /// set, and the hierarchy constraint bounds each set's *namespace*, not the
    /// overlap between sets.
    #[test]
    fn the_set_payload_cap_measures_what_the_scope_cap_deduplicates() {
        // Sized to sit just under the scope byte cap once its separators are
        // counted: 128 × 63 bytes + 127 separators = 8191.
        let members: Vec<String> = (0..MAX_EXPANDED_SCOPES_PER_GRANT)
            .map(|i| format!("repo:com.example.calendar.e{i:0>36}"))
            .collect();
        let one = ExpandedSet {
            nsid: "com.example.calendar.appPerms".to_string(),
            title: Some("T".repeat(MAX_SET_TITLE_BYTES)),
            details: Some("D".repeat(MAX_SET_DETAILS_BYTES)),
            members: members.clone(),
            ignored: vec![],
        };

        // The union these sets expand to clears the scope cap comfortably...
        assert_eq!(
            check_grant_expansion(MAX_INCLUDES_PER_REQUEST, members),
            GrantExpansion::WithinCaps
        );
        // ...and the fan-out cap's worth of them fits the payload bound, which
        // is derived from exactly this worst case.
        let family: Vec<ExpandedSet> = (0..MAX_INCLUDES_PER_REQUEST).map(|_| one.clone()).collect();
        assert_eq!(check_sets_payload(&family), GrantExpansion::WithinCaps);

        // One more set is past it, and refuses at PAR rather than at the nest.
        let mut over = family;
        over.push(one);
        let GrantExpansion::Refused { reason } = check_sets_payload(&over) else {
            panic!("expected the payload cap to refuse");
        };
        assert!(reason.contains("permission-set detail"), "{reason}");
    }

    // ── The seam with D8 ─────────────────────────────────────────────────────

    /// The whole point of emitting ordinary granular scopes: a set may declare
    /// what it likes, and the matrix binds every member with no new arm. This
    /// pins the composition rather than re-testing D8 — a set naming session
    /// lifecycle verbs in its own namespace still grants nothing over them.
    #[test]
    fn expanded_members_are_bound_by_the_matrix_exactly_as_direct_scopes_are() {
        let doc = calendar_set(json!([
            {"resource": "rpc", "lxm": "com.example.calendar.sync.push", "inheritAud": true},
        ]));
        let set = expanded(expand_permission_set(
            include(Some("did:web:svc.example")),
            doc,
        ));

        // The expansion reaches the matrix as an ordinary scope list...
        let verdict = crate::authz::authorize(crate::authz::AuthzInput {
            plane: crate::authz::PLANE_OAUTH.to_string(),
            scopes: set.members.clone(),
            external_apps_enabled: true,
            lxm: "com.atproto.server.refreshSession".to_string(),
            aud: Some("did:web:svc.example".to_string()),
            endpoint_class: crate::authz::ENDPOINT_CLASS_AUTHED.to_string(),
        });
        // ...and `no_granular_scope_reaches` denies session lifecycle whatever
        // a set declared.
        assert!(!verdict.is_allow(), "{verdict:?}");
    }

    /// A bare `include:` is not a grantable scalar scope and must keep
    /// refusing — the shipped fail-closed baseline this slice does not disturb.
    #[test]
    fn a_bare_include_scope_still_grants_nothing_at_par() {
        assert!(!crate::authz::scope_grants_something(
            "include:com.example.calendar.appPerms"
        ));
        assert!(!crate::authz::scope_grants_something(
            "include:com.example.calendar.appPerms?aud=did:web:svc.example"
        ));
    }
}
