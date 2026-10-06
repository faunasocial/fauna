//! ATProto PDS F1 auth-core storage: app-credential verifier rows, the
//! session registry (rotate-on-use refresh with reuse detection), and the
//! per-account external-apps kill-switch. Tables land in
//! `MIGRATIONS_ATPROTO_PDS` (`atproto-pds-full.md` § Nest state schema).
//! The nest stores only PHC verifier strings — never credential secrets —
//! and never runs Argon2id itself (that cost lives bridge-side).

use anyhow::{Context, Result, anyhow};
use fauna_protocol::atproto::IntegrationLevel;
use fauna_protocol::atproto_pds::ConsentSetInfo;
use rusqlite::OptionalExtension;

use super::{CacheDb, blob_col_to_array, now_epoch_millis};

/// Maximum bytes of a PHC verifier string. Production strings are ~100
/// bytes; the cap only guards against a hostile client bloating the table.
pub const MAX_VERIFIER_LEN: usize = 1024;
/// Maximum bytes of a credential label / client note.
pub const MAX_LABEL_LEN: usize = 256;
/// Session/family ids are token jtis — 16-byte random values Go-side; cap
/// generously.
pub const MAX_SESSION_ID_LEN: usize = 64;
/// Maximum bytes of the sealed session-secret blob — same ceiling as the
/// DKIM/identity blobs (production blobs are ~250 bytes).
pub const MAX_ATPROTO_SESSION_SECRET_BLOB_BYTES: usize = 16 * 1024;

/// Maximum bytes of a stored consent-request **identity** string (`client_id`,
/// `client_name`). Generous — a `client_id` is a URL — and it exists only so a
/// hostile or broken bridge cannot bloat rows the user's apps must render.
///
/// The joined `scopes` string has its own, wider ceiling
/// ([`MAX_CONSENT_SCOPES_LEN`]): once permission sets expand into it, it stopped
/// being "a handful of grammar terms" and became the thing the bridge's own
/// grant cap is measured in.
pub const MAX_CONSENT_FIELD_LEN: usize = 4096;

/// Maximum bytes of the stored, space-joined `scopes` string.
///
/// **This is not an independent number — it is the bridge's
/// `permission_set::MAX_EXPANDED_SCOPE_BYTES_PER_GRANT`, which is measured as
/// exactly this rendered form.** A narrower ceiling here does not make the
/// system safer, it makes the bridge's published cap a lie: `/oauth/par`
/// accepts a grant inside its own limit and hands the client a `request_uri`,
/// and the ceremony then dies at `/oauth/authorize` on this refusal — after the
/// client believes it holds a live request, and with nothing on the user's side
/// to see. The two are one number, and the drift assertion below is what keeps
/// them one.
pub const MAX_CONSENT_SCOPES_LEN: usize = 8192;

/// Bytes this ceiling allows the JSON encoding itself, on top of the text it
/// carries: the quotes and commas around every member, each set's field names
/// and braces, and the outer brackets. Measured at ~3.5 KB for the largest
/// payload PAR can pass; the allowance is generous because guessing *low* here
/// is the whole bug below.
pub const MAX_CONSENT_SETS_JSON_ENCODING_ALLOWANCE: usize = 8192;

/// Maximum bytes of the JSON-encoded `sets` payload.
///
/// **Like [`MAX_CONSENT_SCOPES_LEN`], this is not an independent number** — it
/// is `permission_set::MAX_SET_PAYLOAD_BYTES_PER_GRANT` (93,672, written out
/// rather than imported and tied by the assertion below, which runs on every
/// build) plus the encoding allowance above. It gets its own ceiling
/// rather than riding the scopes one because it is bounded by different things:
/// it repeats every expanded member under its own set entry, and adds each
/// set's NSID plus the two strings the set's *author* wrote.
///
/// It was 40,960 until 2026-08-03, and that number was a guess dressed as
/// arithmetic — narrower than what PAR accepts on **both** axes it was meant to
/// cover. A 60 KB `title` cleared PAR because nothing bounded a set
/// author's prose before the megabyte proof fetch; and with no hostile input at
/// all, 8 sets in one namespace with overlapping expansions encode to ~95 KB,
/// because the bridge's byte cap measures the *deduped union* while this
/// payload repeats each member per set. Either way the client got a
/// `request_uri` and the ceremony then died here, at `/oauth/authorize`.
///
/// Both halves are now closed at their own boundary: the author's strings are
/// truncated where they are read, and the payload total is measured at PAR by
/// `permission_set::check_sets_payload`. **The tie to this ceiling is a test,
/// not a `const` assertion** — see
/// `the_sets_ceiling_admits_the_largest_payload_par_can_accept`. The scopes
/// ceiling guards a string both sides render identically, so a compile-time
/// `>=` is exact there; this one guards `serde_json`'s encoding of a wire type,
/// so a new field on [`ConsentSetInfo`] moves the number and arithmetic in a
/// doc comment would not notice. The assertion below still pins the part that
/// *is* arithmetic.
pub const MAX_CONSENT_SETS_LEN: usize = 93_672 + MAX_CONSENT_SETS_JSON_ENCODING_ALLOWANCE;

/// The ceiling on a consent request's stored kind manifest: the JWS is one
/// member of the client's metadata document, which resolution refuses past
/// [`crate::oauth_as_client::CLIENT_METADATA_MAX_BYTES`], so every manifest a
/// start can hand this table fits under it by construction.
pub const MAX_CONSENT_MANIFEST_LEN: usize = crate::oauth_as_client::CLIENT_METADATA_MAX_BYTES;

/// The tie. `fauna-bridge-atproto` is an unconditional dependency of this
/// binary (only its heavy `client` sub-feature is optional) — so the number
/// above is written out rather than imported, but this assertion now runs on
/// every build: raising one ceiling without the other is a compile error
/// rather than a mid-ceremony 502 nobody reproduces.
const _: () = assert!(
    MAX_CONSENT_SCOPES_LEN
        >= fauna_bridge_atproto::permission_set::MAX_EXPANDED_SCOPE_BYTES_PER_GRANT as usize,
    "the stored scopes ceiling must not be narrower than the grant expansion cap PAR enforces"
);

/// The same tie for the set payload — the ceiling a review found untied. This
/// covers the *text*; the encoding on top of it is covered by
/// `the_sets_ceiling_admits_the_largest_payload_par_can_accept`, which is the
/// half a `const` cannot see.
const _: () = assert!(
    MAX_CONSENT_SETS_LEN
        >= fauna_bridge_atproto::permission_set::MAX_SET_PAYLOAD_BYTES_PER_GRANT
            + MAX_CONSENT_SETS_JSON_ENCODING_ALLOWANCE,
    "the stored sets ceiling must not be narrower than the set payload PAR enforces"
);

/// How many live consent requests one bucket may hold — an account's own, or
/// the unassigned pool, each counted separately. Overflow evicts
/// nearest-to-expiry rather than refusing; see
/// [`CacheDb::open_atproto_consent_request`] for why that direction.
pub const MAX_PENDING_CONSENTS_PER_BUCKET: usize = 8;

/// Bytes of entropy in a consent id. It is a bearer-free handle — the bridge
/// polls with it and the user's app answers with it, and both are already
/// authenticated — but it is minted at 256 bits anyway, matching every other
/// nest-minted opaque id, because there is no reason for it to be the one that
/// is not.
const CONSENT_ID_LEN: usize = 32;

/// Characters in the binding code, and the display grouping — `ABC-DEF`.
///
/// **6 characters over the 32-symbol alphabet is 30 bits, and that is enough
/// here for a reason that is not entropy.** The code is never typed and never
/// compared programmatically: a human reads it off their browser and matches it
/// against the card in their app. What it defends against is a *phished consent
/// push* — an attacker starting a flow in the victim's name and hoping the
/// victim approves it — and the defence is that the attacker cannot make their
/// flow display the victim's code, because this nest mints it and refuses to
/// reuse a live one. Guessing does not help either: a collision would have to
/// be found against a code that lives for minutes, behind the PAR endpoint's
/// per-IP limit and the per-bucket ceiling above. Length is therefore chosen for
/// **glance-comparison comfort**, which is the property that actually fails in
/// the field, and shortening it further would start to cost that.
///
/// ⚠ This is emphatically **not** [`fauna_core::claim_code`]'s 8-char/40-bit
/// constant, whose length is a user decision tied to the *claim throttle*. The
/// two share only [`fauna_core::human_code`]'s alphabet — see that module's doc
/// for why sharing the generator but not the length is the whole point.
const CONSENT_CODE_LEN: usize = 6;
const CONSENT_CODE_GROUP: usize = 3;

/// Characters in a typed-code start's user code, and its grouping — `ABCD-EFGH`.
///
/// **Longer than the binding code because this one IS typed and IS compared
/// programmatically**: the account that types it claims the row it names, so a
/// guess that lands claims someone else's device. 8 characters over the
/// 32-symbol alphabet is 40 bits, against at most
/// [`MAX_PENDING_TYPED_CODE_CONSENTS`] live codes behind an authenticated,
/// self-scoped kind, for ten minutes — RFC 8628 §6.1's own recommendation is a
/// 34-bit code for the same threat. Its own constant, not the binding code's
/// and not the claim code's, for the reason [`CONSENT_CODE_LEN`] gives.
const TYPED_CODE_LEN: usize = 8;
const TYPED_CODE_GROUP: usize = 4;

/// How many live typed-code rows the unassigned typed pool may hold.
///
/// Wider than [`MAX_PENDING_CONSENTS_PER_BUCKET`] because that ceiling bounds
/// a list every account renders, and this pool is rendered to nobody: its
/// ceiling bounds only memory and the odds of a guessed code, and a device
/// flood evicting a real user's code mid-typing is the cost a small ceiling
/// would add. Its own pool, so a flood of either start cannot evict the other.
pub const MAX_PENDING_TYPED_CODE_CONSENTS: usize = 64;

/// How many times minting retries on a collision with a live code before giving
/// up loudly. Bounded on purpose: exhausting it is a real failure, and the
/// alternative — emitting a duplicate — is the one outcome the code exists to
/// prevent.
const CONSENT_CODE_MINT_ATTEMPTS: usize = 8;

/// The one column list every consent-request read selects, in
/// [`ConsentRequestRow::from_row`]'s order — four readers once spelled it
/// apart, and a column added to one and not the others reads as shifted.
const CONSENT_REQUEST_COLUMNS: &str = "consent_id, actor_id, code, client_id, client_name, scopes,
     created_at, expires_at, resolved_at, approved, sets, consent_start,
     holder_x25519, writer_ed25519, fauna_manifest";

/// One pending (or resolved) OAuth consent request as stored.
///
/// `logo_uri` is deliberately absent — the resolved client carries one and F4
/// rules that nothing here fetches it; carrying a value to an app that must
/// never load it is the dark-capability shape. See § F4 detail's ceremony
/// bullet.
// Not `Eq`: the wire type behind `sets` carries an additive-extension map whose
// values are `Value`, and `Value` is only ever `PartialEq`.
#[derive(Debug, Clone, PartialEq)]
pub struct ConsentRequestRow {
    pub consent_id: Vec<u8>,
    /// `None` while the request is unassigned — a PAR with no `login_hint`.
    /// Set to the answering actor when the request is resolved.
    pub actor_id: Option<Vec<u8>>,
    /// The binding code, in display form (`ABC-DEF`).
    pub code: String,
    pub client_id: String,
    pub client_name: Option<String>,
    /// The PAR's validated scope set, space-joined — the same spelling the
    /// OAuth `scope` parameter uses, so nothing re-encodes it.
    pub scopes: String,
    /// The permission sets behind those scopes, frozen at PAR. Empty both for a
    /// request that named none and for a row written before PS-b existed —
    /// deliberately the same value, because they mean the same thing to the
    /// only consumer: this card has no set grouping to show.
    pub sets: Vec<ConsentSetInfo>,
    pub created_at: i64,
    pub expires_at: i64,
    pub resolved_at: Option<i64>,
    pub approved: Option<bool>,
    /// Which consent start opened the row.
    pub start: ConsentStartKind,
    /// The keys the ceremony attested and the client document's kind manifest
    /// (`third-party-kinds.md` § The record doors: the card reads them, so the
    /// row says what will be wrapped). Carried for the card only — the
    /// principal row takes its keys from the code the same DPoP key redeems.
    pub binding: ConsentBinding,
}

/// What a consent card needs beyond the scope list to say what a records
/// grant will wrap, and to whom (`third-party-kinds.md` § The record doors):
/// the ceremony's attested keys and the document's verified manifest JWS.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ConsentBinding {
    pub attested: crate::db::third_party_principals::AttestedKeys,
    pub fauna_manifest: Option<String>,
}

/// Which of `authorization-server.md` § Consent's starts opened a consent row.
/// All of them render as the one card; they differ only in who may see the row
/// before it is answered, how it is found, and which bucket its ceiling counts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConsentStartKind {
    /// `/oauth/authorize` — the browser start. Stored as NULL, which is also
    /// what every row written before the column existed reads as.
    Browser,
    /// `/oauth/device_authorization` — RFC 8628's typed code. Opened
    /// unassigned and **listed to nobody**: the row's `code` is the user code
    /// the device displays, and the account that types it claims the row
    /// ([`CacheDb::claim_typed_consent_code`]). Listing it to every account the
    /// way an unhinted browser row is would let anyone approve a device they
    /// never saw, which is what typing the code exists to prove.
    TypedCode,
    /// `/oauth/bc-authorize` — CIBA's quiet push. Opened only for an account
    /// the `login_hint` resolved to, never unassigned, and one live row per
    /// (client, account): a repeat replaces.
    Push,
    /// `fauna.oauth.consent.open_handoff` — the same-device handoff. Opened
    /// only **assigned to the account that opened the route**, never
    /// unassigned, in a bucket of its own so a flood through the route evicts
    /// none of the account's browser or push rows.
    Handoff,
    /// `fauna.plugins.install` — an admin's install card (`third-party.md`
    /// § The runner contract → *The install-approval leg*). Opened only
    /// **assigned to the calling admin**, in a bucket of its own, and
    /// answered through the same `resolve_consent`; its approval mints a
    /// hosted plugin, never a client's grant.
    Install,
}

impl ConsentStartKind {
    const TYPED_CODE: &'static str = "typed_code";
    const PUSH: &'static str = "push";
    const HANDOFF: &'static str = "handoff";
    const INSTALL: &'static str = "install";

    fn column(self) -> Option<&'static str> {
        match self {
            ConsentStartKind::Browser => None,
            ConsentStartKind::TypedCode => Some(Self::TYPED_CODE),
            ConsentStartKind::Push => Some(Self::PUSH),
            ConsentStartKind::Handoff => Some(Self::HANDOFF),
            ConsentStartKind::Install => Some(Self::INSTALL),
        }
    }

    /// An unknown value reads as the browser start: it is the one start with
    /// no visibility rule narrower than its bucket, and a value only a newer
    /// binary writes must not make a row disappear from its owner's list.
    fn from_column(value: Option<&str>) -> Self {
        match value {
            Some(Self::TYPED_CODE) => ConsentStartKind::TypedCode,
            Some(Self::PUSH) => ConsentStartKind::Push,
            Some(Self::HANDOFF) => ConsentStartKind::Handoff,
            Some(Self::INSTALL) => ConsentStartKind::Install,
            _ => ConsentStartKind::Browser,
        }
    }

    /// The code's length and display grouping. A typed code is what RFC 8628
    /// §6.1 calls the user code, and it differs from the glance-read binding
    /// code for the reason [`TYPED_CODE_LEN`] states.
    fn code_shape(self) -> (usize, usize) {
        match self {
            ConsentStartKind::TypedCode => (TYPED_CODE_LEN, TYPED_CODE_GROUP),
            ConsentStartKind::Browser
            | ConsentStartKind::Push
            | ConsentStartKind::Handoff
            | ConsentStartKind::Install => (CONSENT_CODE_LEN, CONSENT_CODE_GROUP),
        }
    }

    /// How many live rows one bucket of this start may hold.
    fn bucket_ceiling(self) -> usize {
        match self {
            ConsentStartKind::TypedCode => MAX_PENDING_TYPED_CODE_CONSENTS,
            ConsentStartKind::Browser
            | ConsentStartKind::Push
            | ConsentStartKind::Handoff
            | ConsentStartKind::Install => MAX_PENDING_CONSENTS_PER_BUCKET,
        }
    }
}

impl ConsentRequestRow {
    /// Column order: `consent_id, actor_id, code, client_id, client_name,
    /// scopes, created_at, expires_at, resolved_at, approved, sets,
    /// consent_start`.
    ///
    /// `sets` is last because it was added last: appending keeps every existing
    /// index stable, and the reader below is the only thing that has to know.
    fn from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Self> {
        Ok(ConsentRequestRow {
            consent_id: row.get(0)?,
            actor_id: row.get(1)?,
            code: row.get(2)?,
            client_id: row.get(3)?,
            client_name: row.get(4)?,
            scopes: row.get(5)?,
            created_at: row.get(6)?,
            expires_at: row.get(7)?,
            resolved_at: row.get(8)?,
            approved: row.get::<_, Option<i64>>(9)?.map(|v| v != 0),
            // A row this nest wrote itself, so a decode failure is corruption,
            // not input — and the honest answer to corruption on a *display*
            // grouping is to show the flat scope list rather than to fail the
            // whole card. The scopes column, which is what the grant is
            // actually made of, is unaffected either way.
            sets: row
                .get::<_, Option<String>>(10)?
                .and_then(|json| serde_json::from_str(&json).ok())
                .unwrap_or_default(),
            start: ConsentStartKind::from_column(row.get::<_, Option<String>>(11)?.as_deref()),
            binding: ConsentBinding {
                attested: crate::db::third_party_principals::AttestedKeys {
                    holder_x25519: key_column(row.get(12)?),
                    writer_ed25519: key_column(row.get(13)?),
                },
                fauna_manifest: row.get(14)?,
            },
        })
    }
}

/// A stored attested key: 32 bytes or absent. A row this nest wrote itself, so
/// a wrong length is corruption — answered as "attested nothing", which the
/// card renders as the conservative case (no grant can be wrapped, or
/// read-only), never as a key it would wrap to.
fn key_column(blob: Option<Vec<u8>>) -> Option<[u8; 32]> {
    blob.and_then(|b| b.try_into().ok())
}

/// One app-credential row as stored (verifier included — only the
/// bridge-facing fetch path reads it; the client list projection drops it).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppCredentialRow {
    pub credential_id: String,
    pub label: String,
    pub verifier: String,
    pub dm_allowed: bool,
    pub created_at: i64,
    pub last_used_at: Option<i64>,
}

/// Mark the OAuth grant whose `grant_id` is this session family's id revoked,
/// inside the caller's transaction (F4 slice 8a).
///
/// A free function rather than a method because every caller already holds the
/// connection: revocation of a family and revocation of its grant must land or
/// fail together, and a method would have to take the lock a second time.
///
/// A no-op for an app-credential session (no grant row) and for an
/// already-revoked grant (`revoked_at IS NULL` guards it), so the first
/// revocation's timestamp is the one that stands.
pub(super) fn revoke_grant_row(
    tx: &rusqlite::Transaction<'_>,
    actor_id: &[u8; 32],
    grant_id: &[u8],
    now: i64,
) -> Result<()> {
    tx.execute(
        "UPDATE atproto_oauth_grants SET revoked_at = ?3
          WHERE actor_id = ?1 AND grant_id = ?2 AND revoked_at IS NULL",
        rusqlite::params![&actor_id[..], grant_id, now],
    )
    .context("revoke atproto oauth grant row")?;
    Ok(())
}

/// Mark one session family revoked — [`revoke_grant_row`]'s twin, and the other
/// half of the pair every revocation writes together.
///
/// Extracted so the per-session door ([`CacheDb::revoke_atproto_session`]) and
/// the deployment-wide sweep ([`CacheDb::end_nest_minted_oauth_grants`]) cannot
/// come to disagree about what revoking a family *is*. They are two selections
/// over one act, not two acts, and F4 slice 8a's refusal of a second
/// `revoke_grant` spelling is the same rule one table over.
///
/// Returns whether a live row was revoked: `revoked_at IS NULL` guards it, so
/// the first revocation's timestamp is the one that stands.
pub(super) fn revoke_session_row(
    tx: &rusqlite::Transaction<'_>,
    actor_id: &[u8; 32],
    session_id: &[u8],
    now: i64,
) -> Result<bool> {
    let n = tx
        .execute(
            "UPDATE atproto_sessions SET revoked_at = ?3
              WHERE actor_id = ?1 AND session_id = ?2 AND revoked_at IS NULL",
            rusqlite::params![&actor_id[..], session_id, now],
        )
        .context("revoke atproto session")?;
    Ok(n > 0)
}

/// What a deployment-wide grant ending ended — the count an admin reads, and
/// the actors whose open connected-apps lists are now stale.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct EndedOAuthGrants {
    /// How many live grant rows the sweep ended. This is the number of rows
    /// that **disappeared from a connected-apps list**, which is why the sweep
    /// scopes itself to the same live view `list_atproto_oauth_grants` serves.
    pub ended: u64,
    /// Each actor that lost at least one grant, once — the nudge fan-out set.
    pub actors: Vec<[u8; 32]>,
}

/// End every live grant carrying the nest AS's issuer mark, inside the
/// caller's transaction.
///
/// **Scoped to the LIVE view `list_atproto_oauth_grants` serves** — not
/// revoked, not past `expires_at` — because the count this returns is exactly
/// "how many connected-apps rows disappeared". A grant already ended is not
/// this sweep's to claim, and an expired one is already invisible to the user,
/// so counting it would inflate the number an admin reads about a compromise
/// response in the one direction that makes it sound bigger than it was.
///
/// **One transaction for the whole sweep, and one ENDING rather than N.** The
/// alternative — read the set, then call the per-grant door in a loop — leaves
/// a sweep that ended half the grants and then failed, with the count and the
/// surface disagreeing and nothing saying where the boundary fell; it also
/// fires one connected-apps nudge per grant, where the nudge names an *actor*.
/// The two row writes are [`revoke_session_row`] and [`revoke_grant_row`] — the
/// same pair the per-session door writes, shared rather than respelled, so a
/// bulk ending can never drift from what revoking a family means everywhere
/// else.
///
/// Ordered so the count is stable across calls and a test can name a row.
pub(crate) fn end_live_oauth_grants(
    tx: &rusqlite::Transaction<'_>,
    now: i64,
) -> Result<EndedOAuthGrants> {
    let mut stmt = tx
        .prepare(
            "SELECT actor_id, grant_id
               FROM atproto_oauth_grants
              WHERE revoked_at IS NULL
                AND (expires_at IS NULL OR expires_at > ?1)
                AND issuer = ?2
              ORDER BY actor_id, grant_id",
        )
        .context("prepare oauth grant sweep")?;
    let targets: Vec<(Vec<u8>, Vec<u8>)> = stmt
        .query_map(rusqlite::params![now, OAUTH_GRANT_ISSUER_NEST], |r| {
            Ok((r.get::<_, Vec<u8>>(0)?, r.get::<_, Vec<u8>>(1)?))
        })
        .context("query oauth grants to end")?
        .collect::<std::result::Result<_, _>>()
        .context("collect oauth grants to end")?;
    drop(stmt);
    let mut ended: u64 = 0;
    let mut actors: Vec<[u8; 32]> = Vec::new();
    for (actor_bytes, grant_id) in targets {
        // A row whose actor_id is not 32 bytes cannot be addressed by the
        // nudge, and this nest wrote the row itself, so a mis-sized id is
        // corruption rather than input. Erroring rather than skipping: silently
        // dropping it would under-count the ending, and the whole transaction
        // rolls back rather than leaving a partial ending nobody can describe.
        let actor: [u8; 32] = actor_bytes
            .try_into()
            .map_err(|_| anyhow!("atproto_oauth_grants row has a malformed actor_id"))?;
        revoke_session_row(tx, &actor, &grant_id, now)?;
        revoke_grant_row(tx, &actor, &grant_id, now)?;
        ended += 1;
        if !actors.contains(&actor) {
            actors.push(actor);
        }
    }
    Ok(EndedOAuthGrants { ended, actors })
}

/// One OAuth grant-registry row (live view) — the connected-apps row.
///
/// `grant_id` doubles as the session-family id, which is what lets the
/// connected-apps surface revoke a grant through the session kind it already
/// calls (F4 slice 8a: one operation, one spelling).
// Not `Eq`: `ConsentSetInfo` carries an additive-extension map of `Value`,
// which is only ever `PartialEq`.
#[derive(Debug, Clone, PartialEq)]
pub struct AtprotoOauthGrantRow {
    pub grant_id: Vec<u8>,
    pub client_id: String,
    pub client_name: Option<String>,
    pub scopes: String,
    /// The permission sets those scopes were expanded from, frozen at the
    /// ceremony. Empty both for a grant that named none and for one recorded
    /// before PS-b.
    pub sets: Vec<ConsentSetInfo>,
    pub dpop_jkt: Option<String>,
    pub created_at: i64,
    pub last_used_at: Option<i64>,
    pub expires_at: Option<i64>,
    /// The grant itself was never revoked, but its paired session (same id,
    /// `revoke_grant_row`'s doc) is dead — a bulk teardown (step-down,
    /// `delete_presence`) killed every session while deliberately leaving
    /// grant rows at rest (`ui/atproto.md`'s downward matrix: "kept, listed,
    /// individually revocable"). Derived at read time from the paired
    /// session, never stored, so it self-heals the moment a fresh session
    /// replaces it — no separate "un-suspend" step to forget.
    pub suspended: bool,
}

/// One session-registry row (live view).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AtprotoSessionRow {
    pub session_id: Vec<u8>,
    pub plane: String,
    pub credential_id: Option<String>,
    pub client_note: Option<String>,
    pub created_at: i64,
    pub last_refreshed_at: Option<i64>,
    pub expires_at: i64,
}

/// One native-records journal row (D1). `record` is the record's canonical
/// dag-cbor bytes verbatim; `deleted_at` is `Some` for a tombstone (kept for
/// re-derivability — never `DELETE`d).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NativeRecordRow {
    pub collection: String,
    pub rkey: String,
    pub cid: String,
    pub record: Vec<u8>,
    pub created_at: i64,
    pub deleted_at: Option<i64>,
}

impl NativeRecordRow {
    /// Column order: `collection, rkey, cid, record, created_at, deleted_at`.
    fn from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Self> {
        Ok(Self {
            collection: row.get(0)?,
            rkey: row.get(1)?,
            cid: row.get(2)?,
            record: row.get(3)?,
            created_at: row.get(4)?,
            deleted_at: row.get(5)?,
        })
    }
}

/// One delegated authoring sub-key row (D10). `k_secret_wrapped` is K's
/// Ed25519 secret at rest, wrapped under the nest-internal KEK (see
/// `crate::atproto_authoring_key`); `cert` is the client-uploaded
/// identity-signed `DeviceAuthorization` embed-as-bytes, `None` between mint
/// (first `fetch_authoring_key`) and `provision_authoring_delegation`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuthoringKeyRow {
    pub k_pub: Vec<u8>,
    pub k_secret_wrapped: Vec<u8>,
    pub cert: Option<Vec<u8>>,
    pub created_at: i64,
    pub cert_updated_at: Option<i64>,
    /// Epoch-millis of the last external-app write that actually APPLIED under
    /// this delegation — **advisory only** (D10 § Audit). Nest-maintained, so it
    /// is not forensics: a compromised nest can under-report it, and a refused
    /// batch never stamps it. `None` = not used yet, which is also what a
    /// delegation provisioned before this column existed honestly reads.
    pub last_used_at: Option<i64>,
}

impl AuthoringKeyRow {
    /// Column order: `k_pub, k_secret_wrapped, cert, created_at, cert_updated_at,
    /// last_used_at`.
    fn from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Self> {
        Ok(Self {
            k_pub: row.get(0)?,
            k_secret_wrapped: row.get(1)?,
            cert: row.get(2)?,
            created_at: row.get(3)?,
            cert_updated_at: row.get(4)?,
            last_used_at: row.get(5)?,
        })
    }
}

/// The account's authoring sub-key row on `conn`, if one has been minted.
pub(crate) fn authoring_key(
    conn: &rusqlite::Connection,
    actor_id: &[u8; 32],
) -> Result<Option<AuthoringKeyRow>> {
    conn.query_row(
        "SELECT k_pub, k_secret_wrapped, cert, created_at, cert_updated_at,
                last_used_at
           FROM atproto_authoring_keys
          WHERE actor_id = ?1",
        rusqlite::params![&actor_id[..]],
        AuthoringKeyRow::from_row,
    )
    .optional()
    .context("read atproto authoring key")
}

/// Insert a freshly-minted sub-key on `conn`, **first-write-wins**: a concurrent
/// mint that already inserted the row is left untouched (`ON CONFLICT DO
/// NOTHING`), so the caller re-reads to learn the winning `k_pub`. Returns
/// `true` iff this call inserted the row (the loser gets `false`).
///
/// Takes a connection rather than the pool so the mint
/// (`atproto_authoring_key::mint_or_fetch`) reads the deployment seed, seals and
/// inserts under one hold of the database guard.
pub(crate) fn insert_authoring_key_if_absent(
    conn: &rusqlite::Connection,
    actor_id: &[u8; 32],
    k_pub: &[u8; 32],
    k_secret_wrapped: &[u8],
) -> Result<bool> {
    let inserted = conn
        .execute(
            "INSERT INTO atproto_authoring_keys
                 (actor_id, k_pub, k_secret_wrapped, cert, created_at, cert_updated_at)
             VALUES (?1, ?2, ?3, NULL, ?4, NULL)
             ON CONFLICT(actor_id) DO NOTHING",
            rusqlite::params![
                &actor_id[..],
                &k_pub[..],
                k_secret_wrapped,
                now_epoch_millis()
            ],
        )
        .context("insert atproto authoring key")?;
    Ok(inserted > 0)
}

/// One `atproto_blobs` row (F2.4): the sha256-CID ↔ Fauna media mapping a
/// `com.atproto.repo.uploadBlob` leaves behind (`atproto-pds-full.md` § Nest
/// state schema).
///
/// `media_ref` is the blake3 `ContentHash` the nest's byte route answered — the
/// name a Fauna post's media carries — while `cid` is the ATProto blob CID
/// (CIDv1/raw/sha2-256) the external app's record references. The two
/// content-address spaces cannot share a name, and this row is the only place
/// they are tied together.
///
/// `referenced_at` is `None` until a *record* references the blob. That is what
/// makes an unreferenced row transient upload state rather than user data: it is
/// recreatable by re-upload, hence GC-able past the reference window (`:191`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BlobRow {
    pub cid: String,
    pub media_ref: [u8; 32],
    pub created_at: i64,
    pub referenced_at: Option<i64>,
}

impl BlobRow {
    /// Column order: `cid, media_ref, created_at, referenced_at`.
    fn from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Self> {
        let media_ref: [u8; 32] = blob_col_to_array(row.get(1)?, 1, "media_ref")?;
        Ok(Self {
            cid: row.get(0)?,
            media_ref,
            created_at: row.get(2)?,
            referenced_at: row.get(3)?,
        })
    }
}

/// Why an app-credential provision write failed. The handler maps
/// `Duplicate` → `fauna.bridges.atproto.credential_exists` and `Other` →
/// the generic malformed/internal path (the `ListWriteError` pattern in
/// `mail_lists.rs`).
///
/// `Duplicate` exists because the write must **never** be an upsert: two
/// devices deriving the same `credential_id` from the same label is a real
/// race (a device's collision-avoid can only see credentials it has already
/// refreshed), and replacing the row would retire the other device's app
/// password with no signal on either side.
#[derive(Debug)]
pub enum CredentialWriteError {
    /// `(actor_id, credential_id)` already exists.
    Duplicate,
    Other(anyhow::Error),
}

impl From<anyhow::Error> for CredentialWriteError {
    fn from(e: anyhow::Error) -> Self {
        CredentialWriteError::Other(e)
    }
}

/// Outcome of a rotate-on-use refresh attempt (F1 detail).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RefreshOutcome {
    /// Presented jti was current — rotated to the new jti.
    Rotated,
    /// Presented jti was superseded — replay: the family is now revoked.
    ReuseDetected,
    /// No live session (unknown, revoked, or expired).
    Invalid,
}

/// The `atproto_oauth_grants.issuer` value the **nest-hosted** authorization
/// server writes. The column is `NOT NULL`, so every grant row carries a mark.
///
/// A string rather than a bool because exactly one issuer exists, and a
/// `nest_issued INTEGER` column would be a flag whose false arm names no
/// server.
pub const OAUTH_GRANT_ISSUER_NEST: &str = "nest";

impl CacheDb {
    // ── App credentials ─────────────────────────────────────────

    pub async fn put_atproto_app_credential(
        &self,
        actor_id: &[u8; 32],
        credential_id: &str,
        label: &str,
        verifier: &str,
        dm_allowed: bool,
    ) -> std::result::Result<(), CredentialWriteError> {
        if verifier.len() > MAX_VERIFIER_LEN {
            return Err(anyhow!(
                "atproto verifier too large: {} bytes (max {MAX_VERIFIER_LEN})",
                verifier.len()
            )
            .into());
        }
        if label.len() > MAX_LABEL_LEN {
            return Err(anyhow!(
                "atproto credential label too large: {} bytes (max {MAX_LABEL_LEN})",
                label.len()
            )
            .into());
        }
        let actor = *actor_id;
        let now = now_epoch_millis();
        let conn = self.conn.lock().await;
        // Plain INSERT, never OR REPLACE: the PK collision is a real
        // two-device race and must surface, not silently retire the other
        // device's credential. See [`CredentialWriteError::Duplicate`].
        conn.execute(
            "INSERT INTO atproto_app_credentials
                (actor_id, credential_id, label, verifier, dm_allowed, created_at, last_used_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, NULL)",
            rusqlite::params![
                &actor[..],
                credential_id,
                label,
                verifier,
                dm_allowed as i64,
                now
            ],
        )
        .map_err(map_credential_write_err)?;
        Ok(())
    }

    pub async fn list_atproto_app_credentials(
        &self,
        actor_id: &[u8; 32],
    ) -> Result<Vec<AppCredentialRow>> {
        let actor = *actor_id;
        let conn = self.conn.lock().await;
        let mut stmt = conn
            .prepare(
                "SELECT credential_id, label, verifier, dm_allowed, created_at, last_used_at
                   FROM atproto_app_credentials
                  WHERE actor_id = ?1
                  ORDER BY created_at ASC",
            )
            .context("prepare list atproto credentials")?;
        let rows = stmt
            .query_map(rusqlite::params![&actor[..]], |row| {
                Ok(AppCredentialRow {
                    credential_id: row.get(0)?,
                    label: row.get(1)?,
                    verifier: row.get(2)?,
                    dm_allowed: row.get::<_, i64>(3)? != 0,
                    created_at: row.get(4)?,
                    last_used_at: row.get(5)?,
                })
            })
            .context("query atproto credentials")?
            .collect::<std::result::Result<Vec<_>, _>>()
            .context("collect atproto credentials")?;
        Ok(rows)
    }

    /// Delete a credential and revoke every session minted from it, in one
    /// transaction. Returns `(credential_existed, sessions_revoked)`.
    pub async fn revoke_atproto_app_credential(
        &self,
        actor_id: &[u8; 32],
        credential_id: &str,
    ) -> Result<(bool, u32)> {
        let actor = *actor_id;
        let now = now_epoch_millis();
        let mut conn = self.conn.lock().await;
        let tx = conn.transaction().context("begin credential revoke tx")?;
        let deleted = tx
            .execute(
                "DELETE FROM atproto_app_credentials
                  WHERE actor_id = ?1 AND credential_id = ?2",
                rusqlite::params![&actor[..], credential_id],
            )
            .context("delete atproto credential")?;
        let sessions = tx
            .execute(
                "UPDATE atproto_sessions SET revoked_at = ?3
                  WHERE actor_id = ?1 AND credential_id = ?2 AND revoked_at IS NULL",
                rusqlite::params![&actor[..], credential_id, now],
            )
            .context("revoke sessions of credential")?;
        tx.commit().context("commit credential revoke tx")?;
        Ok((deleted > 0, sessions as u32))
    }

    /// Stamp `last_used_at` on the credential that just minted a session.
    pub async fn touch_atproto_credential_last_used(
        &self,
        actor_id: &[u8; 32],
        credential_id: &str,
    ) -> Result<()> {
        let actor = *actor_id;
        let now = now_epoch_millis();
        let conn = self.conn.lock().await;
        conn.execute(
            "UPDATE atproto_app_credentials SET last_used_at = ?3
              WHERE actor_id = ?1 AND credential_id = ?2",
            rusqlite::params![&actor[..], credential_id, now],
        )
        .context("touch atproto credential last_used")?;
        Ok(())
    }

    // ── Session registry ────────────────────────────────────────

    pub async fn insert_atproto_session(
        &self,
        actor_id: &[u8; 32],
        session_id: &[u8],
        plane: &str,
        credential_id: Option<&str>,
        client_note: Option<&str>,
        expires_at: i64,
    ) -> Result<()> {
        if session_id.is_empty() || session_id.len() > MAX_SESSION_ID_LEN {
            return Err(anyhow!(
                "atproto session_id length {} out of range (1..={MAX_SESSION_ID_LEN})",
                session_id.len()
            ));
        }
        if let Some(note) = client_note
            && note.len() > MAX_LABEL_LEN
        {
            return Err(anyhow!(
                "atproto client_note too large: {} bytes (max {MAX_LABEL_LEN})",
                note.len()
            ));
        }
        let actor = *actor_id;
        let now = now_epoch_millis();
        let conn = self.conn.lock().await;
        conn.execute(
            "INSERT OR REPLACE INTO atproto_sessions
                (actor_id, session_id, plane, credential_id, client_note,
                 created_at, last_refreshed_at, expires_at, revoked_at, current_refresh_jti)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, NULL, ?7, NULL, NULL)",
            rusqlite::params![
                &actor[..],
                session_id,
                plane,
                credential_id,
                client_note,
                now,
                expires_at
            ],
        )
        .context("insert atproto session")?;
        Ok(())
    }

    /// Live (unrevoked, unexpired) sessions for the account.
    pub async fn list_atproto_sessions(
        &self,
        actor_id: &[u8; 32],
    ) -> Result<Vec<AtprotoSessionRow>> {
        let actor = *actor_id;
        let now = now_epoch_millis();
        let conn = self.conn.lock().await;
        let mut stmt = conn
            .prepare(
                "SELECT session_id, plane, credential_id, client_note,
                        created_at, last_refreshed_at, expires_at
                   FROM atproto_sessions
                  WHERE actor_id = ?1 AND revoked_at IS NULL AND expires_at > ?2
                  ORDER BY created_at ASC",
            )
            .context("prepare list atproto sessions")?;
        let rows = stmt
            .query_map(rusqlite::params![&actor[..], now], |row| {
                Ok(AtprotoSessionRow {
                    session_id: row.get(0)?,
                    plane: row.get(1)?,
                    credential_id: row.get(2)?,
                    client_note: row.get(3)?,
                    created_at: row.get(4)?,
                    last_refreshed_at: row.get(5)?,
                    expires_at: row.get(6)?,
                })
            })
            .context("query atproto sessions")?
            .collect::<std::result::Result<Vec<_>, _>>()
            .context("collect atproto sessions")?;
        Ok(rows)
    }

    /// Revoke one session, **cascading to its OAuth grant row when it has
    /// one** (F4 slice 8a).
    ///
    /// Returns whether a live session row was revoked.
    ///
    /// The cascade is the other half of [`Self::record_atproto_oauth_grant`]'s
    /// one-transaction write, and it exists for the same reason: neither row is
    /// meaningful alone. A revoked family beside a live grant row would render
    /// on the connected-apps surface as a working connection nothing can be
    /// done with — a capability the user is shown but cannot revoke, which is
    /// precisely what `principles.md`'s *audited from the user's app* forbids.
    ///
    /// **One function, three callers** (this kind, the bridge's `end_session`
    /// behind `/oauth/revoke`, and the reuse family-kill in
    /// [`Self::refresh_atproto_session`]), which is why F4 slice 8a deliberately
    /// added **no** `revoke_grant` kind: the grant id *is* the session id, so a
    /// second spelling of this operation would be two implementations of one
    /// rule, free to drift.
    ///
    /// The grant `UPDATE` is a harmless no-op for an app-credential session,
    /// which has no grant row — keeping the plane out of the SQL is what stops
    /// this becoming two branches that must agree.
    pub async fn revoke_atproto_session(
        &self,
        actor_id: &[u8; 32],
        session_id: &[u8],
    ) -> Result<bool> {
        let actor = *actor_id;
        let now = now_epoch_millis();
        let mut conn = self.conn.lock().await;
        let tx = conn.transaction().context("begin revoke atproto session")?;
        let revoked = revoke_session_row(&tx, &actor, session_id, now)?;
        revoke_grant_row(&tx, &actor, session_id, now)?;
        tx.commit().context("commit revoke atproto session")?;
        Ok(revoked)
    }

    /// End **every live grant the nest-hosted authorization server minted** —
    /// the deployment-wide arm of [`Self::revoke_atproto_session`], for the
    /// forced session-secret rotation (`authorization-server.md` § The issuer
    /// → *The forced session-secret arm ends the grants it kills*).
    ///
    /// **Why a cross-actor set at all.** The secret that rotation re-mints is
    /// the nest's own refresh-token MAC key (§ The issuer → *Two HS256 secrets,
    /// not one*), so the rotation kills exactly the families the nest MACed.
    /// Ending grants by actor — the shape every other revocation here uses —
    /// cannot express that set.
    ///
    /// **Positively selected on the nest's mark.** The column is `NOT NULL`
    /// and the nest AS is the only issuer, so today the selection equals every
    /// live grant; it stays positive so the ending names what it kills rather
    /// than what it spares.
    ///
    /// The transaction, its scope and its count are [`end_live_oauth_grants`]'s.
    pub async fn end_nest_minted_oauth_grants(&self) -> Result<EndedOAuthGrants> {
        let now = now_epoch_millis();
        let mut conn = self.conn.lock().await;
        let tx = conn
            .transaction()
            .context("begin end nest-minted oauth grants")?;
        let ended = end_live_oauth_grants(&tx, now)?;
        tx.commit().context("commit end nest-minted oauth grants")?;
        Ok(ended)
    }

    /// Rotate-on-use refresh with reuse detection — the one-row check
    /// (F1 detail): the presented jti must equal the stored current jti
    /// (`current_refresh_jti`, or the immutable `session_id` before the
    /// first rotation). A superseded jti is a replay and kills the family.
    pub async fn refresh_atproto_session(
        &self,
        actor_id: &[u8; 32],
        session_id: &[u8],
        presented_jti: &[u8],
        new_jti: &[u8],
        new_expires_at: i64,
    ) -> Result<RefreshOutcome> {
        if new_jti.is_empty() || new_jti.len() > MAX_SESSION_ID_LEN {
            return Err(anyhow!(
                "atproto new_jti length {} out of range (1..={MAX_SESSION_ID_LEN})",
                new_jti.len()
            ));
        }
        let actor = *actor_id;
        let now = now_epoch_millis();
        let mut conn = self.conn.lock().await;
        let row: Option<(Option<Vec<u8>>, Option<i64>, i64)> = conn
            .query_row(
                "SELECT current_refresh_jti, revoked_at, expires_at
                   FROM atproto_sessions
                  WHERE actor_id = ?1 AND session_id = ?2",
                rusqlite::params![&actor[..], session_id],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .optional()
            .context("read atproto session for refresh")?;
        let Some((current_jti, revoked_at, expires_at)) = row else {
            return Ok(RefreshOutcome::Invalid);
        };
        if revoked_at.is_some() || expires_at <= now {
            return Ok(RefreshOutcome::Invalid);
        }
        let current: &[u8] = current_jti.as_deref().unwrap_or(session_id);
        let tx = conn.transaction().context("begin atproto refresh")?;
        let outcome = if presented_jti == current {
            tx.execute(
                "UPDATE atproto_sessions
                    SET current_refresh_jti = ?3, last_refreshed_at = ?4, expires_at = ?5
                  WHERE actor_id = ?1 AND session_id = ?2",
                rusqlite::params![&actor[..], session_id, new_jti, now, new_expires_at],
            )
            .context("rotate atproto session jti")?;
            // The grant's `last_used_at` (D10 § Audit's *complementary and
            // advisory* half). A successful rotation is the one moment the
            // NEST observes an OAuth grant in use — access-token calls are
            // verified bridge-side and never reach here — so this is the
            // honest maximum a nest-maintained column can claim, and the
            // reason the goal doc calls it advisory rather than forensics.
            // It rides the rotation's own transaction: a stamp that could
            // outlive a failed rotation would report a use that did not
            // happen.
            tx.execute(
                "UPDATE atproto_oauth_grants SET last_used_at = ?3
                  WHERE actor_id = ?1 AND grant_id = ?2",
                rusqlite::params![&actor[..], session_id, now],
            )
            .context("stamp atproto grant last_used_at")?;
            RefreshOutcome::Rotated
        } else if current == new_jti {
            // The SAME rotation, asked for twice — a retry, not a reuse.
            //
            // `forbid_replay = false` on this kind asserts the handler is
            // naturally idempotent (`transport.md` § Idempotency and
            // reconnect-with-resume): a caller that resends the identical
            // `(presented_jti, new_jti)` pair after a dropped reply must land
            // here, not on the family-kill below, or a network blip would log
            // the user out of a connected app AND record a false
            // token-theft signal.
            //
            // ⚠ Not a currently reachable path — verified, not assumed. `refresh_session`'s only
            // live caller, `bins/fauna-bridges/internal/atprotopds`'s
            // `server.go` (via `TokenMinter::MintRotation`), mints a **fresh
            // random `new_jti` on every handler invocation** — an HTTP-level
            // client retry therefore never resubmits the same pair, and the Go
            // bridge's own `wsrpc.Client.Call` is single-shot besides (fresh
            // idempotency key per call, no reissue). This arm exists to keep
            // the kind's `forbid_replay = false` contract honest for any
            // future caller that *does* preserve `new_jti` across a retry —
            // e.g. a Rust client via `request_auto_retry`, which the Go
            // bridge neither uses nor resembles — not because today's caller
            // exercises it.
            //
            // Safe regardless of who calls it: the discriminator is the
            // caller's OWN minted value, so reaching here requires presenting
            // the exact pair that already succeeded, and a stolen refresh
            // token cannot — a thief's call carries a `new_jti` that is not
            // `current` and still family-kills. Nothing is written: the row
            // already holds this rotation's result, and re-stamping
            // `last_refreshed_at`/`last_used_at` would report a second use
            // that did not happen (the same honesty rule the rotation arm's
            // own stamp comment states).
            RefreshOutcome::Rotated
        } else {
            tx.execute(
                "UPDATE atproto_sessions SET revoked_at = ?3
                  WHERE actor_id = ?1 AND session_id = ?2",
                rusqlite::params![&actor[..], session_id, now],
            )
            .context("family-kill atproto session on jti reuse")?;
            // The family-kill cascades for the same reason an explicit revoke
            // does: a dead family beside a live grant row is a connection the
            // user is shown and cannot act on. Reuse detection is exactly when
            // that row most needs to stop reading as live.
            revoke_grant_row(&tx, &actor, session_id, now)?;
            RefreshOutcome::ReuseDetected
        };
        tx.commit().context("commit atproto refresh")?;
        Ok(outcome)
    }

    // ── OAuth grant registry (F4 slice 7) ───────────────────────

    /// Not-yet-revoked, not-yet-expired OAuth grants for the account — the
    /// connected-apps registry read (F4 slice 8a). "Not revoked" is not the
    /// same fact as "usable right now": a bulk teardown (step-down,
    /// `delete_presence`) kills the paired session while deliberately
    /// leaving the grant row at rest, so a returned row also carries
    /// `suspended`, derived from that same session's liveness.
    ///
    /// The revoked/expired half of the predicate is the same one
    /// [`Self::list_atproto_sessions`] applies, because the two render one
    /// surface: a grant whose row this hides while its session row still
    /// listed would show the user a connection with no identity attached.
    /// `expires_at IS NULL` is live — a grant whose refresh horizon is
    /// open-ended (the confidential-client case) has no deadline to compare
    /// against, which is a different fact from having a past one.
    pub async fn list_atproto_oauth_grants(
        &self,
        actor_id: &[u8; 32],
    ) -> Result<Vec<AtprotoOauthGrantRow>> {
        let actor = *actor_id;
        let now = now_epoch_millis();
        let conn = self.conn.lock().await;
        let mut stmt = conn
            .prepare(
                "SELECT g.grant_id, g.client_id, g.client_name, g.scopes, g.dpop_jkt,
                        g.created_at, g.last_used_at, g.expires_at, g.sets,
                        EXISTS (
                            SELECT 1 FROM atproto_sessions s
                             WHERE s.actor_id = g.actor_id
                               AND s.session_id = g.grant_id
                               AND s.revoked_at IS NOT NULL
                        ) AS suspended
                   FROM atproto_oauth_grants g
                  WHERE g.actor_id = ?1
                    AND g.revoked_at IS NULL
                    AND (g.expires_at IS NULL OR g.expires_at > ?2)
                  ORDER BY g.created_at ASC",
            )
            .context("prepare list atproto oauth grants")?;
        let rows = stmt
            .query_map(rusqlite::params![&actor[..], now], |row| {
                Ok(AtprotoOauthGrantRow {
                    grant_id: row.get(0)?,
                    client_id: row.get(1)?,
                    client_name: row.get(2)?,
                    scopes: row.get(3)?,
                    dpop_jkt: row.get(4)?,
                    created_at: row.get(5)?,
                    last_used_at: row.get(6)?,
                    expires_at: row.get(7)?,
                    // A row this nest wrote itself, so a decode failure is
                    // corruption rather than input — and the honest answer for
                    // a *provenance* grouping is to fall back to the flat scope
                    // list, never to hide a live connection from the user.
                    sets: row
                        .get::<_, Option<String>>(8)?
                        .and_then(|json| serde_json::from_str(&json).ok())
                        .unwrap_or_default(),
                    suspended: row.get(9)?,
                })
            })
            .context("query atproto oauth grants")?
            .collect::<std::result::Result<Vec<_>, _>>()
            .context("collect atproto oauth grants")?;
        Ok(rows)
    }

    /// Record an OAuth grant **and** its initial refresh-token session family,
    /// in one transaction.
    ///
    /// The two rows are written together because neither is meaningful alone:
    /// a grant row with no session is a connected-apps entry the user can see
    /// but whose tokens do not exist, and a session row with no grant is a
    /// live refresh family the connected-apps surface cannot show or revoke —
    /// the second being the worse half, since [`../principles.md`]'s
    /// *audited from the user's app* is exactly what it breaks. One
    /// transaction makes both half-states unrepresentable instead of leaving a
    /// crash window between two writes.
    ///
    /// `grant_id` doubles as the session family id (the initial refresh
    /// `jti`), so one identifier joins the grant, its session, and every
    /// rotation — which is what lets the revoke slice cascade with no join
    /// table.
    ///
    /// **Idempotent on `(actor_id, grant_id)`, and the arm that makes it so is
    /// not `OR REPLACE`.** A `grant_id` is fresh 128-bit entropy per exchange
    /// (`oauth_as_token::JTI_BYTES`), so a second call carrying one is the
    /// *same* exchange asked twice — the reply was lost between this commit and
    /// the caller. Three shapes, one read then at most one write:
    ///
    /// - **No row** → both `INSERT`s run, as before.
    /// - **A row whose authorization fields match** (`client_id`, `scopes`,
    ///   `dpop_jkt`, `sets`) → **nothing is written** and the call reports the
    ///   same `Ok` the first one did. Writing here is what would be unsafe:
    ///   `atproto_sessions.current_refresh_jti` is NULL only until the first
    ///   rotation, and `refresh_atproto_session` reads NULL as *"the initial
    ///   jti (= `grant_id`) is current"* — so re-landing this row after a
    ///   rotation would make a **spent refresh token valid again** and disarm
    ///   the reuse family-kill. `revoked_at` is the same story one step on: a
    ///   replay must not resurrect a grant the user has revoked. This is
    ///   `refresh_atproto_session`'s same-rotation arm, one table over, and
    ///   safe for the same reason — the discriminator is the caller's own
    ///   minted id plus the authorization it already recorded under it.
    /// - **A row whose authorization fields differ** → still an error. That is
    ///   a genuine collision or a caller bug (a *different* authorization filed
    ///   under a colliding id), and it must surface rather than be swallowed —
    ///   the property the plain `INSERT` was reaching for and the one an
    ///   unconditional `ON CONFLICT DO NOTHING` would throw away.
    ///
    /// The timestamps are deliberately **not** discriminators: a retry re-reads
    /// the clock, so comparing `created_at`/`expires_at` would report a
    /// conflict for every honest replay.
    ///
    /// Neither row ever takes `insert_atproto_session`'s `OR REPLACE` footgun.
    /// This is what `fauna.bridges.atproto.record_grant`'s
    /// `forbid_replay = false` asserts (`transport.md` § Idempotency and
    /// reconnect-with-resume: the flag is a claim that the handler is naturally
    /// idempotent), and until 2026-09-09 the claim was false — the bare
    /// `INSERT`s hit `PRIMARY KEY (actor_id, grant_id)` and rolled the
    /// transaction back.
    #[allow(clippy::too_many_arguments)]
    pub async fn record_atproto_oauth_grant(
        &self,
        actor_id: &[u8; 32],
        grant_id: &[u8],
        client_id: &str,
        client_name: Option<&str>,
        scopes: &str,
        sets: &[ConsentSetInfo],
        dpop_jkt: &str,
        session_expires_at: i64,
        grant_expires_at: Option<i64>,
        issuer: &str,
        principal: &super::third_party_principals::PrincipalAttestation,
    ) -> Result<()> {
        if grant_id.is_empty() || grant_id.len() > MAX_SESSION_ID_LEN {
            return Err(anyhow!(
                "atproto grant_id length {} out of range (1..={MAX_SESSION_ID_LEN})",
                grant_id.len()
            ));
        }
        for (what, value) in [
            ("client_id", Some(client_id)),
            ("client_name", client_name),
            ("dpop_jkt", Some(dpop_jkt)),
        ] {
            if let Some(v) = value
                && v.len() > MAX_CONSENT_FIELD_LEN
            {
                return Err(anyhow!(
                    "atproto grant {what} too large: {} bytes (max {MAX_CONSENT_FIELD_LEN})",
                    v.len()
                ));
            }
        }
        // `scopes` rides the ceremony's one scope ceiling, not the identity
        // one — the same string the consent row stored, so a narrower cap here
        // would refuse a grant *the user has already approved*, at
        // `/oauth/token`, with the card long gone. See `MAX_CONSENT_SCOPES_LEN`.
        if scopes.len() > MAX_CONSENT_SCOPES_LEN {
            return Err(anyhow!(
                "atproto grant scopes too large: {} bytes (max {MAX_CONSENT_SCOPES_LEN})",
                scopes.len()
            ));
        }
        let sets_json = match sets {
            [] => None,
            sets => {
                let json = serde_json::to_string(sets).context("encode grant sets")?;
                if json.len() > MAX_CONSENT_SETS_LEN {
                    return Err(anyhow!(
                        "atproto grant sets too large: {} bytes (max {MAX_CONSENT_SETS_LEN})",
                        json.len()
                    ));
                }
                Some(json)
            }
        };
        let actor = *actor_id;
        let now = now_epoch_millis();
        let mut conn = self.conn.lock().await;
        // The replay discriminator, read before the transaction opens: is this
        // the same exchange asked twice, or a different authorization under a
        // colliding id? See the doc comment for why the answer is "write
        // nothing" rather than "re-land the rows".
        // `dpop_jkt` and `sets` are nullable columns, so both read as `Option`.
        //
        // `issuer` joins the discriminator rather than riding along silently:
        // the same grant id recorded by the OTHER authorization server is a
        // different authorization, not the same exchange asked twice, and the
        // two issuers hold different refresh secrets — so treating it as a
        // replay would return `Ok(())` while leaving the row pointing at the
        // wrong issuer, which is precisely the row a forced rotation then fails
        // to end.
        type PriorGrant = (String, String, Option<String>, Option<String>, String);
        let existing: Option<PriorGrant> = conn
            .query_row(
                "SELECT client_id, scopes, dpop_jkt, sets, issuer
                   FROM atproto_oauth_grants
                  WHERE actor_id = ?1 AND grant_id = ?2",
                rusqlite::params![&actor[..], grant_id],
                |row| {
                    Ok((
                        row.get(0)?,
                        row.get(1)?,
                        row.get(2)?,
                        row.get(3)?,
                        row.get(4)?,
                    ))
                },
            )
            .optional()
            .context("read atproto oauth grant for replay check")?;
        if let Some((prior_client_id, prior_scopes, prior_dpop_jkt, prior_sets, prior_issuer)) =
            existing
        {
            if prior_client_id == client_id
                && prior_scopes == scopes
                && prior_dpop_jkt.as_deref() == Some(dpop_jkt)
                && prior_sets == sets_json
                && prior_issuer == issuer
            {
                return Ok(());
            }
            return Err(anyhow!(
                "atproto grant_id collision: a different authorization is already \
                 recorded under this grant_id"
            ));
        }
        let tx = conn.transaction().context("begin record grant")?;
        tx.execute(
            "INSERT INTO atproto_oauth_grants
                (actor_id, grant_id, client_id, client_name, scopes, dpop_jkt,
                 created_at, last_used_at, expires_at, revoked_at, sets, issuer)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, NULL, ?8, NULL, ?9, ?10)",
            rusqlite::params![
                &actor[..],
                grant_id,
                client_id,
                client_name,
                scopes,
                dpop_jkt,
                now,
                grant_expires_at,
                &sets_json,
                issuer
            ],
        )
        .context("insert atproto oauth grant")?;
        // The session row's `plane` is `"oauth"` and its `credential_id` is
        // NULL — an OAuth session is minted from a grant, not from an app
        // credential, and `list_sessions` renders the plane so the two are
        // distinguishable in the user's app.
        tx.execute(
            "INSERT INTO atproto_sessions
                (actor_id, session_id, plane, credential_id, client_note,
                 created_at, last_refreshed_at, expires_at, revoked_at, current_refresh_jti)
             VALUES (?1, ?2, 'oauth', NULL, ?3, ?4, NULL, ?5, NULL, NULL)",
            rusqlite::params![&actor[..], grant_id, client_name, now, session_expires_at],
        )
        .context("insert atproto oauth session")?;
        // The consent's third row: the principal this grant belongs to
        // (`third-party.md` § The principal model — minted by the consent
        // act). In THIS transaction so a grant never lands without its
        // principal, and a refused holder key rolls the grant back with it.
        super::third_party_principals::upsert_principal_in_tx(
            &tx,
            &actor,
            client_id,
            client_name,
            scopes,
            principal,
            now,
        )?;
        tx.commit().context("commit record grant")?;
        Ok(())
    }

    // ── External-apps kill-switch ───────────────────────────────

    /// Default ON: no row means enabled (`atproto_account_settings` rows
    /// exist only once a user touches the toggle).
    pub async fn get_atproto_external_apps_enabled(&self, actor_id: &[u8; 32]) -> Result<bool> {
        let actor = *actor_id;
        let conn = self.conn.lock().await;
        let enabled: Option<i64> = conn
            .query_row(
                "SELECT external_apps_enabled FROM atproto_account_settings
                  WHERE actor_id = ?1",
                rusqlite::params![&actor[..]],
                |row| row.get(0),
            )
            .optional()
            .context("read atproto external_apps_enabled")?;
        Ok(enabled.map(|v| v != 0).unwrap_or(true))
    }

    pub async fn set_atproto_external_apps_enabled(
        &self,
        actor_id: &[u8; 32],
        enabled: bool,
    ) -> Result<()> {
        let actor = *actor_id;
        let now = now_epoch_millis();
        let conn = self.conn.lock().await;
        conn.execute(
            "INSERT INTO atproto_account_settings (actor_id, external_apps_enabled, updated_at)
             VALUES (?1, ?2, ?3)
             ON CONFLICT(actor_id) DO UPDATE
                SET external_apps_enabled = excluded.external_apps_enabled,
                    updated_at = excluded.updated_at",
            rusqlite::params![&actor[..], enabled as i64, now],
        )
        .context("set atproto external_apps_enabled")?;
        Ok(())
    }

    // ── Preferences (app.bsky.actor.{get,put}Preferences, F3 phase 4) ──

    /// Read the account's opaque preferences payload, or `None` when it has
    /// never stored preferences. The nest never interprets the blob (D2).
    pub async fn get_atproto_preferences(&self, actor_id: &[u8; 32]) -> Result<Option<Vec<u8>>> {
        let actor = *actor_id;
        let conn = self.conn.lock().await;
        let prefs: Option<Vec<u8>> = conn
            .query_row(
                "SELECT preferences FROM atproto_preferences WHERE actor_id = ?1",
                rusqlite::params![&actor[..]],
                |row| row.get(0),
            )
            .optional()
            .context("read atproto preferences")?;
        Ok(prefs)
    }

    /// Overwrite the account's single preferences row (upsert). The caller
    /// enforces the size cap (`PREFERENCES_MAX_BYTES`) before reaching here.
    pub async fn set_atproto_preferences(
        &self,
        actor_id: &[u8; 32],
        preferences: &[u8],
    ) -> Result<()> {
        let actor = *actor_id;
        let now = now_epoch_millis();
        let conn = self.conn.lock().await;
        conn.execute(
            "INSERT INTO atproto_preferences (actor_id, preferences, updated_at)
             VALUES (?1, ?2, ?3)
             ON CONFLICT(actor_id) DO UPDATE
                SET preferences = excluded.preferences,
                    updated_at = excluded.updated_at",
            rusqlite::params![&actor[..], preferences, now],
        )
        .context("set atproto preferences")?;
        Ok(())
    }

    // ── Native-records journal (D1; F2 unmappable-lexicon path) ──

    /// Read one journal row (live or tombstoned) by its `(collection, rkey)`
    /// primary key, or `None` when the row was never written. The caller uses
    /// this for op semantics: `createRecord` refuses when a *live* row exists;
    /// `deleteRecord`/`putRecord` route on the same lookup. `deleted_at` is
    /// `Some` for a tombstoned row (the row is kept, never `DELETE`d — the
    /// repo's re-derivability requires the tombstone; `migrations.rs`
    /// § atproto_native_records).
    pub async fn get_atproto_native_record(
        &self,
        actor_id: &[u8; 32],
        collection: &str,
        rkey: &str,
    ) -> Result<Option<NativeRecordRow>> {
        let actor = *actor_id;
        let collection = collection.to_string();
        let rkey = rkey.to_string();
        let conn = self.conn.lock().await;
        let row = conn
            .query_row(
                "SELECT collection, rkey, cid, record, created_at, deleted_at
                   FROM atproto_native_records
                  WHERE actor_id = ?1 AND collection = ?2 AND rkey = ?3",
                rusqlite::params![&actor[..], collection, rkey],
                NativeRecordRow::from_row,
            )
            .optional()
            .context("read atproto native record")?;
        Ok(row)
    }

    /// Write a live journal row — insert, or revive-and-replace a row that
    /// already exists at `(collection, rkey)` (new `cid`/`record`, `deleted_at`
    /// cleared). This is the storage primitive; the caller enforces the
    /// per-op policy (a `createRecord` reads first and refuses a live
    /// collision; a `putRecord`/journal-update writes through). `created_at`
    /// is stamped on first write and refreshed on a revive so a re-created
    /// rkey sorts by its new instant.
    pub async fn put_atproto_native_record(
        &self,
        actor_id: &[u8; 32],
        collection: &str,
        rkey: &str,
        cid: &str,
        record: &[u8],
    ) -> Result<()> {
        let actor = *actor_id;
        let now = now_epoch_millis();
        let conn = self.conn.lock().await;
        conn.execute(
            "INSERT INTO atproto_native_records
                 (actor_id, collection, rkey, cid, record, created_at, deleted_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, NULL)
             ON CONFLICT(actor_id, collection, rkey) DO UPDATE
                SET cid = excluded.cid,
                    record = excluded.record,
                    created_at = excluded.created_at,
                    deleted_at = NULL",
            rusqlite::params![&actor[..], collection, rkey, cid, record, now],
        )
        .context("put atproto native record")?;
        Ok(())
    }

    /// Tombstone a live journal row (`deleted_at = now`), keeping the row for
    /// re-derivability. Returns `true` when a live row was tombstoned, `false`
    /// when there was no live row at `(collection, rkey)` — the caller maps
    /// `false` to `deleteRecord`'s "record not found" (a delete of an
    /// already-absent record is a no-op in ATProto, not an error).
    pub async fn tombstone_atproto_native_record(
        &self,
        actor_id: &[u8; 32],
        collection: &str,
        rkey: &str,
    ) -> Result<bool> {
        let actor = *actor_id;
        let now = now_epoch_millis();
        let conn = self.conn.lock().await;
        let changed = conn
            .execute(
                "UPDATE atproto_native_records
                    SET deleted_at = ?4
                  WHERE actor_id = ?1 AND collection = ?2 AND rkey = ?3
                    AND deleted_at IS NULL",
                rusqlite::params![&actor[..], collection, rkey, now],
            )
            .context("tombstone atproto native record")?;
        Ok(changed > 0)
    }

    /// List the account's **live** journal rows for one collection, ordered by
    /// `rkey` (the repo materializes its MST from projection ∪ this journal,
    /// so tombstoned rows are excluded here). Keyset pagination is deferred to
    /// F2's `listRecords` slice; the collection cardinality is small today.
    pub async fn list_atproto_native_records(
        &self,
        actor_id: &[u8; 32],
        collection: &str,
    ) -> Result<Vec<NativeRecordRow>> {
        let actor = *actor_id;
        let collection = collection.to_string();
        let conn = self.conn.lock().await;
        let mut stmt = conn
            .prepare(
                "SELECT collection, rkey, cid, record, created_at, deleted_at
                   FROM atproto_native_records
                  WHERE actor_id = ?1 AND collection = ?2 AND deleted_at IS NULL
                  ORDER BY rkey ASC",
            )
            .context("prepare list atproto native records")?;
        let rows = stmt
            .query_map(
                rusqlite::params![&actor[..], collection],
                NativeRecordRow::from_row,
            )
            .context("query atproto native records")?
            .collect::<rusqlite::Result<Vec<_>>>()
            .context("collect atproto native records")?;
        Ok(rows)
    }

    // ── Blobs (F2.4 — the `uploadBlob` sha256-CID ↔ Fauna media map) ─────────

    /// Record that `cid`'s bytes now live in the Fauna media path under
    /// `media_ref`, for `actor_id`.
    ///
    /// An **upsert**, deliberately: the ATProto blob CID is the sha256 of the
    /// bytes, so an app that retries an upload whose reply it never saw sends
    /// the identical CID for identical bytes, and the retry must converge rather
    /// than collide on the primary key. That convergence is what lets the
    /// upload leg fail forward at every step (see the `uploadBlob` handler's
    /// write ordering). The upsert cannot lose information: with the CID fixed,
    /// the bytes are fixed, so `media_ref` is fixed too.
    ///
    /// `referenced_at` is left alone — a re-upload is not a reference, and
    /// clobbering an existing stamp would make a referenced blob look
    /// collectable.
    pub async fn upsert_atproto_blob(
        &self,
        actor_id: &[u8; 32],
        cid: &str,
        media_ref: &[u8; 32],
    ) -> Result<()> {
        let actor = *actor_id;
        let media_ref = *media_ref;
        let cid = cid.to_string();
        let now = now_epoch_millis();
        let conn = self.conn.lock().await;
        conn.execute(
            "INSERT INTO atproto_blobs (actor_id, cid, media_ref, created_at, referenced_at)
             VALUES (?1, ?2, ?3, ?4, NULL)
             ON CONFLICT(actor_id, cid) DO UPDATE
                SET media_ref = excluded.media_ref",
            rusqlite::params![&actor[..], cid, &media_ref[..], now],
        )
        .context("upsert atproto blob")?;
        Ok(())
    }

    /// Resolve one uploaded blob by the CID a record references. `None` means
    /// this account never uploaded those bytes here — which is the ordinary case
    /// for a record referencing a blob hosted on another PDS.
    pub async fn get_atproto_blob(
        &self,
        actor_id: &[u8; 32],
        cid: &str,
    ) -> Result<Option<BlobRow>> {
        let actor = *actor_id;
        let cid = cid.to_string();
        let conn = self.conn.lock().await;
        let row = conn
            .query_row(
                "SELECT cid, media_ref, created_at, referenced_at
                   FROM atproto_blobs
                  WHERE actor_id = ?1 AND cid = ?2",
                rusqlite::params![&actor[..], cid],
                BlobRow::from_row,
            )
            .optional()
            .context("read atproto blob")?;
        Ok(row)
    }

    /// Every blob this account has uploaded, oldest first. A per-account read
    /// for account-scoped callers; the per-account cardinality is small.
    ///
    /// **Not** the GC sweep's input, despite what this comment used to claim: a
    /// periodic sweep does not know the actor set, and the two queries it does
    /// need are cross-actor and do their filtering in SQL —
    /// [`Self::all_atproto_blob_media_refs`] (reachability) and
    /// [`Self::delete_unreferenced_atproto_blobs`] (the window).
    pub async fn list_atproto_blobs(&self, actor_id: &[u8; 32]) -> Result<Vec<BlobRow>> {
        let actor = *actor_id;
        let conn = self.conn.lock().await;
        let mut stmt = conn
            .prepare(
                "SELECT cid, media_ref, created_at, referenced_at
                   FROM atproto_blobs
                  WHERE actor_id = ?1
                  ORDER BY created_at ASC, cid ASC",
            )
            .context("prepare list atproto blobs")?;
        let rows = stmt
            .query_map(rusqlite::params![&actor[..]], BlobRow::from_row)
            .context("query atproto blobs")?
            .collect::<rusqlite::Result<Vec<_>>>()
            .context("collect atproto blobs")?;
        Ok(rows)
    }

    /// Mark an uploaded blob as referenced by a record (F2.4 slice 2).
    ///
    /// **First reference wins** (`COALESCE`): the stamp is a high-water mark —
    /// "a record has named these bytes" — not a last-used clock, so a
    /// re-reference never rewrites history and the stamp is idempotent. A CID
    /// this account never uploaded updates zero rows, which is deliberate: the
    /// caller stamps every ref a record carries, and rows that do not exist
    /// (a profile's projection-stored picture) are simply not this ledger's.
    ///
    /// Once stamped, the row is user data to the F2.4 GC — deleting a
    /// referenced blob breaks a record the network already carries — so the
    /// write path stamps BEFORE it applies: an over-stamp on a batch that then
    /// fails internally costs one blob's collectability, while the reverse
    /// order would let a crash between apply and stamp turn referenced media
    /// into GC bait (the no-data-loss direction).
    pub async fn stamp_atproto_blob_referenced(
        &self,
        actor_id: &[u8; 32],
        cid: &str,
    ) -> Result<()> {
        let actor = *actor_id;
        let cid = cid.to_string();
        let now = now_epoch_millis();
        let conn = self.conn.lock().await;
        conn.execute(
            "UPDATE atproto_blobs
                SET referenced_at = COALESCE(referenced_at, ?3)
              WHERE actor_id = ?1 AND cid = ?2",
            rusqlite::params![&actor[..], cid, now],
        )
        .context("stamp atproto blob referenced")?;
        Ok(())
    }

    /// Every `media_ref` this ledger names, across all accounts — the input to
    /// the box-wide GC's reachability oracle (F2.4 slice 4).
    ///
    /// **A row's existence IS the reference.** The `uploadBlob` leg writes the
    /// bytes, a `blob_metadata` row and a row here, and records no
    /// `sync_changes` row, so before this query existed the oracle could not see
    /// these bytes at all and swept an external app's live media on the first
    /// cycle past grace.
    /// Deliberately unfiltered: the window belongs to
    /// [`Self::delete_unreferenced_atproto_blobs`], so the oracle never does
    /// cutoff arithmetic and the two cannot drift into a gap that deletes
    /// referenced media. Bytes become collectable when the ROW goes, never
    /// before.
    ///
    /// Duplicates are possible and harmless — two accounts uploading identical
    /// bytes get two rows and one content address. That sharing is also why the
    /// sweep must never delete media bytes itself: one account's expired upload
    /// is another's live post.
    pub async fn all_atproto_blob_media_refs(&self) -> Result<Vec<Vec<u8>>> {
        let conn = self.conn.lock().await;
        let mut stmt = conn
            .prepare("SELECT media_ref FROM atproto_blobs")
            .context("prepare all atproto blob media refs")?;
        let rows = stmt
            .query_map([], |row| row.get::<_, Vec<u8>>(0))
            .context("query atproto blob media refs")?
            .collect::<rusqlite::Result<Vec<_>>>()
            .context("collect atproto blob media refs")?;
        Ok(rows)
    }

    /// Delete every unreferenced blob row older than `cutoff_millis`, across all
    /// accounts, returning how many went (F2.4 slice 4's sweep).
    ///
    /// `atproto-pds-full.md` § Nest state schema sanctions exactly this row set:
    /// an unreferenced row past the reference window is transient upload state,
    /// recreatable by re-upload, hence deletable under the no-data-loss rule.
    ///
    /// **Deletes the ROW only — never the media bytes.** Once the row is gone
    /// the bytes fall out of [`Self::all_atproto_blob_media_refs`] and the
    /// box-wide GC reclaims them under its own reachability walk, which is the
    /// one oracle that can also see the *other* references those content-
    /// addressed bytes may have. That split is what makes the sweep crash-safe
    /// with nothing to order: there is a single write, and the reclaim it
    /// enables is a separate idempotent pass.
    ///
    /// **`referenced_at IS NULL` is evaluated inside the DELETE**, not by the
    /// caller over a listed row set — the stamp and this statement then
    /// serialize on the one connection, so a row stamped by a concurrent write
    /// can never be taken by a sweep that read it as unreferenced a moment ago.
    /// `created_at`/`referenced_at` are epoch **milliseconds**
    /// ([`now_epoch_millis`]), so `cutoff_millis` must be too.
    pub async fn delete_unreferenced_atproto_blobs(&self, cutoff_millis: i64) -> Result<u64> {
        let conn = self.conn.lock().await;
        let deleted = conn
            .execute(
                "DELETE FROM atproto_blobs
                  WHERE referenced_at IS NULL AND created_at < ?1",
                rusqlite::params![cutoff_millis],
            )
            .context("sweep unreferenced atproto blobs")?;
        Ok(deleted as u64)
    }

    // ── Authoring keys (D10 delegated authoring) ────────────────

    /// Read the account's authoring sub-key row, if one has been minted.
    pub async fn get_atproto_authoring_key(
        &self,
        actor_id: &[u8; 32],
    ) -> Result<Option<AuthoringKeyRow>> {
        let conn = self.conn.lock().await;
        authoring_key(&conn, actor_id)
    }

    /// Store the client-uploaded delegation cert onto an existing sub-key row.
    /// Returns `false` when no row exists (the client must fetch/mint K first).
    pub async fn set_atproto_authoring_key_cert(
        &self,
        actor_id: &[u8; 32],
        cert: &[u8],
    ) -> Result<bool> {
        let actor = *actor_id;
        let now = now_epoch_millis();
        let conn = self.conn.lock().await;
        let changed = conn
            .execute(
                "UPDATE atproto_authoring_keys
                    SET cert = ?2, cert_updated_at = ?3
                  WHERE actor_id = ?1",
                rusqlite::params![&actor[..], cert, now],
            )
            .context("set atproto authoring key cert")?;
        Ok(changed > 0)
    }

    /// Destroy the account's authoring sub-key (D10 § Revocation): the row —
    /// secret, pubkey, cert — is DELETEd. Recreatable by re-enabling a hosted
    /// level (a fresh K is re-minted, a fresh cert re-provisioned; already
    /// published posts stay verifiable from their embedded cert). Idempotent:
    /// a delete of an absent row is a no-op. Returns `true` iff a row went.
    pub async fn delete_atproto_authoring_key(&self, actor_id: &[u8; 32]) -> Result<bool> {
        let actor = *actor_id;
        let conn = self.conn.lock().await;
        let changed = conn
            .execute(
                "DELETE FROM atproto_authoring_keys WHERE actor_id = ?1",
                rusqlite::params![&actor[..]],
            )
            .context("delete atproto authoring key")?;
        Ok(changed > 0)
    }

    /// Stamp the delegation row's **advisory** `last_used_at` (D10 § Audit).
    ///
    /// Called only after an external-app batch has actually APPLIED — "last
    /// used" honestly means an external app *authored*, and a refused batch
    /// authored nothing. Best-effort by design: a delegation deleted mid-write
    /// updates zero rows and that is fine (`Ok(false)`), because this value is
    /// advisory and must never be able to fail a write that already succeeded.
    /// Callers therefore log rather than propagate.
    pub async fn touch_atproto_authoring_key_last_used(
        &self,
        actor_id: &[u8; 32],
        at_millis: i64,
    ) -> Result<bool> {
        let actor = *actor_id;
        let conn = self.conn.lock().await;
        let changed = conn
            .execute(
                "UPDATE atproto_authoring_keys SET last_used_at = ?2 WHERE actor_id = ?1",
                rusqlite::params![&actor[..], at_millis],
            )
            .context("stamp atproto authoring key last_used_at")?;
        Ok(changed > 0)
    }

    // ── Integration level (S4-A) ────────────────────────────────

    /// The user's stored Bluesky integration depth (`ui/atproto.md` § State &
    /// data shape). An absent row — and any value this binary does not
    /// recognize — reads as the default OFF: a *newer* nest may have written a
    /// level this binary predates, and reporting an unknown depth as "no
    /// integration" is the safe read (it never over-states what is published;
    /// only the transition kind writes, and it writes what the user confirmed).
    pub async fn get_atproto_integration_level(
        &self,
        actor_id: &[u8; 32],
    ) -> Result<IntegrationLevel> {
        let actor = *actor_id;
        let conn = self.conn.lock().await;
        let stored: Option<String> = conn
            .query_row(
                "SELECT integration_level FROM atproto_account_settings
                  WHERE actor_id = ?1",
                rusqlite::params![&actor[..]],
                |row| row.get(0),
            )
            .optional()
            .context("read atproto integration_level")?;
        Ok(stored
            .as_deref()
            .and_then(IntegrationLevel::from_wire)
            .unwrap_or_default())
    }

    /// Persist the level. Written LAST in a transition, so a crash mid-way
    /// leaves the OLD level in force against partially-applied — and
    /// individually idempotent — effects: the client's retry replays the same
    /// plan and converges. See `set_integration_level_handler`.
    pub async fn set_atproto_integration_level(
        &self,
        actor_id: &[u8; 32],
        level: IntegrationLevel,
    ) -> Result<()> {
        let actor = *actor_id;
        let now = now_epoch_millis();
        let conn = self.conn.lock().await;
        conn.execute(
            "INSERT INTO atproto_account_settings (actor_id, integration_level, updated_at)
             VALUES (?1, ?2, ?3)
             ON CONFLICT(actor_id) DO UPDATE
                SET integration_level = excluded.integration_level,
                    updated_at = excluded.updated_at",
            rusqlite::params![&actor[..], level.as_str(), now],
        )
        .context("set atproto integration_level")?;
        Ok(())
    }

    /// Revoke every live session for one actor — the login-plane suspension a
    /// step-down off the top rung (or `delete_presence`'s teardown) performs.
    /// Credential and grant rows are deliberately KEPT (listed, individually
    /// revocable) so stepping back up restores usability —
    /// [`Self::list_atproto_oauth_grants`] derives each grant's `suspended`
    /// flag from its paired session going dead here, so no companion write
    /// against the grant table is needed. Returns how many sessions were
    /// revoked.
    pub async fn revoke_all_atproto_sessions(&self, actor_id: &[u8; 32]) -> Result<usize> {
        let actor = *actor_id;
        let now = now_epoch_millis();
        let conn = self.conn.lock().await;
        let n = conn
            .execute(
                "UPDATE atproto_sessions SET revoked_at = ?2
                  WHERE actor_id = ?1 AND revoked_at IS NULL",
                rusqlite::params![&actor[..], now],
            )
            .context("revoke all atproto sessions")?;
        Ok(n)
    }

    /// Read the bridge-wide sealed HS256 session-secret blob, if provisioned.
    pub async fn get_atproto_session_secret_blob(
        &self,
        bridge_role: &str,
        bridge_id: &str,
    ) -> Result<Option<Vec<u8>>> {
        let (role, id) = (bridge_role.to_string(), bridge_id.to_string());
        let conn = self.conn.lock().await;
        conn.query_row(
            "SELECT blob FROM atproto_session_secret_blobs
              WHERE bridge_role = ?1 AND bridge_id = ?2",
            rusqlite::params![role, id],
            |row| row.get::<_, Vec<u8>>(0),
        )
        .optional()
        .context("get atproto session secret blob")
    }

    /// Store a freshly-minted sealed session-secret blob **iff none exists**,
    /// returning the canonical stored blob either way. First-write-wins under
    /// one connection lock, so a double provision-on-read race can never
    /// rotate an already-served secret (which would silently invalidate every
    /// token minted under it).
    pub async fn put_atproto_session_secret_blob_if_absent(
        &self,
        bridge_role: &str,
        bridge_id: &str,
        blob: &[u8],
    ) -> Result<Vec<u8>> {
        if blob.len() > MAX_ATPROTO_SESSION_SECRET_BLOB_BYTES {
            return Err(anyhow!(
                "atproto session secret blob too large: {} bytes (max {})",
                blob.len(),
                MAX_ATPROTO_SESSION_SECRET_BLOB_BYTES
            ));
        }
        let (role, id) = (bridge_role.to_string(), bridge_id.to_string());
        let blob = blob.to_vec();
        let now = now_epoch_millis();
        let conn = self.conn.lock().await;
        conn.execute(
            "INSERT OR IGNORE INTO atproto_session_secret_blobs
                 (bridge_role, bridge_id, blob, created_at)
             VALUES (?1, ?2, ?3, ?4)",
            rusqlite::params![role, id, blob, now],
        )
        .context("put atproto session secret blob")?;
        conn.query_row(
            "SELECT blob FROM atproto_session_secret_blobs
              WHERE bridge_role = ?1 AND bridge_id = ?2",
            rusqlite::params![role, id],
            |row| row.get::<_, Vec<u8>>(0),
        )
        .context("read back atproto session secret blob")
    }

    // ── OAuth consent requests (F4 slice 6a, D3 rung 2) ──────────────────────

    /// Open a pending consent request and return the row as stored.
    ///
    /// `actor_id` is `None` for a PAR that carried no `login_hint` — the row is
    /// then unassigned and any user of this nest may claim it by resolving it
    /// (§ F4 detail's ceremony: "no hint → … the pending consent is visible
    /// in-app").
    ///
    /// Three things happen on the one connection, in this order, and the order
    /// is why none of them needs a lock: expired rows are swept, the bucket's
    /// ceiling is enforced by evicting **nearest-to-expiry**, and the row is
    /// inserted under a code proven unique among the live rows. Eviction is
    /// nearest-to-expiry — i.e. oldest, since every row shares one TTL —
    /// because a flood's victim is mid-flow in the *newest* request; refusing
    /// instead would let an attacker deny a user the ability to consent at all,
    /// and evicting the newest would let them push the user's real request out.
    ///
    /// The browser start's spelling of [`Self::open_atproto_consent_request_for`].
    pub async fn open_atproto_consent_request(
        &self,
        actor_id: Option<[u8; 32]>,
        client_id: &str,
        client_name: Option<&str>,
        scopes: &str,
        sets: &[ConsentSetInfo],
        ttl_millis: i64,
    ) -> Result<ConsentRequestRow> {
        self.open_atproto_consent_request_for(
            ConsentStartKind::Browser,
            actor_id,
            client_id,
            client_name,
            scopes,
            sets,
            &ConsentBinding::default(),
            ttl_millis,
        )
        .await
    }

    /// Open a pending consent request for one of the consent starts — the one
    /// implementation every start's row goes through (see
    /// [`Self::open_atproto_consent_request`] for the ordering and the
    /// eviction direction, which hold for every start).
    ///
    /// **A bucket is (account-or-unassigned, start)**: a flood through one start
    /// cannot evict another start's rows, for the reason an account's rows and
    /// the unassigned pool are already separate — a quiet push nobody asked for
    /// must not be able to push out the browser request the user is mid-way
    /// through.
    ///
    /// **A quiet push replaces**: any live, unanswered push row for the same
    /// (client, account) is deleted in the same pass before the new one is
    /// inserted (`authorization-server.md` § Consent rule (b)). The poll behind
    /// a replaced row then reads it as expired, which is the honest answer to a
    /// client that superseded its own request.
    #[allow(clippy::too_many_arguments)]
    #[allow(clippy::too_many_arguments)] // the request's facts, flat, as every start holds them
    pub async fn open_atproto_consent_request_for(
        &self,
        start: ConsentStartKind,
        actor_id: Option<[u8; 32]>,
        client_id: &str,
        client_name: Option<&str>,
        scopes: &str,
        sets: &[ConsentSetInfo],
        binding: &ConsentBinding,
        ttl_millis: i64,
    ) -> Result<ConsentRequestRow> {
        if matches!(
            start,
            ConsentStartKind::Push | ConsentStartKind::Handoff | ConsentStartKind::Install
        ) && actor_id.is_none()
        {
            // Structural, not a policy the caller has to remember: an
            // unassigned push row would be listed to every account, which is
            // the unsolicited request rule (a) exists to keep out of sight;
            // and a handoff or install row is the opening user's by definition.
            return Err(anyhow!(
                "a quiet-push, handoff or install consent request must name an account"
            ));
        }
        if client_id.len() > MAX_CONSENT_FIELD_LEN
            || client_name.is_some_and(|n| n.len() > MAX_CONSENT_FIELD_LEN)
        {
            return Err(anyhow!(
                "consent request identity field over the {MAX_CONSENT_FIELD_LEN}-byte cap"
            ));
        }
        // Its own, wider ceiling — see `MAX_CONSENT_SCOPES_LEN` for why this is
        // the bridge's grant cap rather than a second opinion about it.
        if scopes.len() > MAX_CONSENT_SCOPES_LEN {
            return Err(anyhow!(
                "consent request scopes over the {MAX_CONSENT_SCOPES_LEN}-byte cap"
            ));
        }
        // The set payload carries the members a second time *plus* two strings
        // the set's own author wrote, so it needs its own ceiling rather than
        // riding the scopes one — see `MAX_CONSENT_SETS_LEN`.
        let sets_json = match sets {
            [] => None,
            sets => {
                let json = serde_json::to_string(sets).context("encode consent request sets")?;
                if json.len() > MAX_CONSENT_SETS_LEN {
                    return Err(anyhow!(
                        "consent request sets over the {MAX_CONSENT_SETS_LEN}-byte cap"
                    ));
                }
                Some(json)
            }
        };
        // The manifest rides its own ceiling: it was verified at resolution,
        // whose document cap bounds it, but this table must not trust a
        // caller's input to be bounded by a cap it cannot see.
        if binding
            .fauna_manifest
            .as_ref()
            .is_some_and(|jws| jws.len() > MAX_CONSENT_MANIFEST_LEN)
        {
            return Err(anyhow!(
                "consent request manifest over the {MAX_CONSENT_MANIFEST_LEN}-byte cap"
            ));
        }
        let (client_id, scopes) = (client_id.to_string(), scopes.to_string());
        let client_name = client_name.map(str::to_string);
        let now = now_epoch_millis();
        let expires_at = now.saturating_add(ttl_millis);
        let conn = self.conn.lock().await;

        // Sweep first: an expired row is not visible to anything, so keeping it
        // would let dead rows consume a live bucket's ceiling.
        sweep_expired_consents(&conn, now)?;

        // Rule (b): a quiet push replaces the same client's live, unanswered
        // push to the same account. Before the ceiling is counted, so a client
        // repeating itself never evicts anything but its own earlier request.
        if start == ConsentStartKind::Push {
            conn.execute(
                "DELETE FROM atproto_consent_requests
                  WHERE actor_id = ?1 AND client_id = ?2
                    AND consent_start = ?3 AND resolved_at IS NULL",
                rusqlite::params![
                    actor_id.as_ref().map(|a| &a[..]),
                    &client_id,
                    ConsentStartKind::PUSH
                ],
            )
            .context("replace the client's earlier quiet push")?;
        }

        // The ceiling is per bucket, and a bucket is (account-or-unassigned,
        // start) — `IS` rather than `=` so the unassigned pool and the browser
        // start (both NULL) are buckets of their own rather than matching
        // nothing.
        let start_column = start.column();
        let actor_column = actor_id.as_ref().map(|a| a.to_vec());
        let over: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM atproto_consent_requests
                  WHERE actor_id IS ?1 AND consent_start IS ?2",
                rusqlite::params![&actor_column, start_column],
                |row| row.get(0),
            )
            .context("count pending consent requests")?;
        for _ in 0..(over - start.bucket_ceiling() as i64 + 1).max(0) {
            let evicted = conn
                .execute(
                    "DELETE FROM atproto_consent_requests WHERE consent_id = (
                         SELECT consent_id FROM atproto_consent_requests
                          WHERE actor_id IS ?1 AND consent_start IS ?2
                          ORDER BY expires_at ASC LIMIT 1)",
                    rusqlite::params![&actor_column, start_column],
                )
                .context("evict oldest pending consent request")?;
            if evicted == 0 {
                break;
            }
        }

        let (code_len, code_group) = start.code_shape();
        let code = mint_unique_consent_code(
            || fauna_core::human_code::generate(code_len, code_group),
            |candidate| {
                let taken: i64 = conn
                    .query_row(
                        "SELECT COUNT(*) FROM atproto_consent_requests WHERE code = ?1",
                        rusqlite::params![candidate],
                        |row| row.get(0),
                    )
                    .context("check consent code uniqueness")?;
                Ok(taken != 0)
            },
        )?;

        let mut consent_id = [0u8; CONSENT_ID_LEN];
        getrandom::fill(&mut consent_id).context("consent id entropy")?;
        conn.execute(
            "INSERT INTO atproto_consent_requests
                (consent_id, actor_id, code, client_id, client_name, scopes,
                 created_at, expires_at, resolved_at, approved, sets, consent_start,
                 holder_x25519, writer_ed25519, fauna_manifest)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, NULL, NULL, ?9, ?10, ?11, ?12, ?13)",
            rusqlite::params![
                &consent_id[..],
                &actor_column,
                &code,
                &client_id,
                &client_name,
                &scopes,
                now,
                expires_at,
                &sets_json,
                start_column,
                binding.attested.holder_x25519.as_ref().map(|k| &k[..]),
                binding.attested.writer_ed25519.as_ref().map(|k| &k[..]),
                &binding.fauna_manifest,
            ],
        )
        .context("insert consent request")?;

        Ok(ConsentRequestRow {
            consent_id: consent_id.to_vec(),
            actor_id: actor_column,
            code,
            client_id,
            client_name,
            scopes,
            sets: sets.to_vec(),
            created_at: now,
            expires_at,
            resolved_at: None,
            approved: None,
            start,
            binding: binding.clone(),
        })
    }

    /// Read one consent request by id, expired rows included.
    ///
    /// Expiry is **not** filtered here on purpose: the bridge's poll needs to
    /// tell "expired" from "never existed" to answer its browser honestly, and
    /// every caller that must not act on a stale row checks `expires_at`
    /// itself. The pending *list* below does filter — an app must never render
    /// a card the user can no longer usefully approve.
    pub async fn get_atproto_consent_request(
        &self,
        consent_id: &[u8],
    ) -> Result<Option<ConsentRequestRow>> {
        let id = consent_id.to_vec();
        let conn = self.conn.lock().await;
        conn.query_row(
            &format!(
                "SELECT {CONSENT_REQUEST_COLUMNS}
                   FROM atproto_consent_requests WHERE consent_id = ?1"
            ),
            rusqlite::params![id],
            ConsentRequestRow::from_row,
        )
        .optional()
        .context("get consent request")
    }

    /// Every live, unresolved consent request this actor may answer: their own,
    /// plus the unassigned browser ones (§ F4 detail — a request with no
    /// `login_hint` is claimed by whoever approves it). An unassigned
    /// **typed-code** row is not listed: it is found only by typing its code
    /// ([`Self::claim_typed_consent_code`]), which is the whole of what that
    /// start proves.
    pub async fn list_pending_atproto_consent_requests(
        &self,
        actor_id: &[u8; 32],
    ) -> Result<Vec<ConsentRequestRow>> {
        let actor = *actor_id;
        let now = now_epoch_millis();
        let conn = self.conn.lock().await;
        let mut stmt = conn
            .prepare(&format!(
                "SELECT {CONSENT_REQUEST_COLUMNS}
                   FROM atproto_consent_requests
                  WHERE resolved_at IS NULL
                    AND expires_at > ?2
                    AND (actor_id = ?1 OR (actor_id IS NULL AND consent_start IS NULL))
                  ORDER BY created_at ASC"
            ))
            .context("prepare list pending consent requests")?;
        let rows = stmt
            .query_map(
                rusqlite::params![&actor[..], now],
                ConsentRequestRow::from_row,
            )
            .context("query pending consent requests")?
            .collect::<std::result::Result<Vec<_>, _>>()
            .context("collect pending consent requests")?;
        Ok(rows)
    }

    /// Answer a pending consent request as `actor_id`, returning the row as it
    /// now stands. `None` means there was no live, unresolved row this actor
    /// could answer.
    ///
    /// The `WHERE` clause is the authorization, not a pre-check followed by an
    /// update: a row already belonging to another account is not matched, an
    /// already-resolved row is not matched (so an approval cannot be flipped or
    /// replayed into a second grant), and an expired one is not matched. That
    /// is why there is no read-then-write window to lose.
    ///
    /// An unassigned row is **bound** to the answering actor by this same
    /// statement — for a decline as much as an approval, because the row then
    /// honestly records who answered it. An unassigned typed-code row is not
    /// matched: it must be claimed by its code first, so knowing a consent id is
    /// never a way around typing the code.
    pub async fn resolve_atproto_consent_request(
        &self,
        consent_id: &[u8],
        actor_id: &[u8; 32],
        approved: bool,
    ) -> Result<Option<ConsentRequestRow>> {
        self.resolve_atproto_consent_request_choosing(consent_id, actor_id, approved, None)
            .await
    }

    /// [`Self::resolve_atproto_consent_request`], approving with the folder the
    /// card chose (`authorization-server.md` § Scope grammar → *The folder
    /// plane's qualifier is the user's*): every bare folder-plane scope on the
    /// row is rewritten to its qualified string IN THE SAME STATEMENT that
    /// records the approval, so the read-back the token is minted from
    /// carries exactly the qualified strings and nothing is re-resolved
    /// later. The rewrite is compare-and-set on the scopes it was computed
    /// from. `None` — nothing resolved — also when the row carries no bare
    /// folder scope for the choice to qualify. Whose folder it is, is the
    /// caller's check.
    pub async fn resolve_atproto_consent_request_choosing(
        &self,
        consent_id: &[u8],
        actor_id: &[u8; 32],
        approved: bool,
        folder: Option<i64>,
    ) -> Result<Option<ConsentRequestRow>> {
        let id = consent_id.to_vec();
        let actor = *actor_id;
        let now = now_epoch_millis();
        let conn = self.conn.lock().await;
        let (stored, qualified) = match folder {
            None => (None, None),
            Some(folder) => {
                let stored: Option<String> = conn
                    .query_row(
                        "SELECT scopes FROM atproto_consent_requests WHERE consent_id = ?1",
                        rusqlite::params![id],
                        |row| row.get(0),
                    )
                    .optional()
                    .context("read consent scopes to qualify")?;
                let Some(stored) = stored else {
                    return Ok(None);
                };
                let scopes: Vec<&str> = stored.split_whitespace().collect();
                let Some(qualified) =
                    fauna_bridge_atproto::fauna_scope::qualify_bare_scopes(&scopes, folder)
                else {
                    return Ok(None);
                };
                (Some(stored), Some(qualified.join(" ")))
            }
        };
        let changed = conn
            .execute(
                "UPDATE atproto_consent_requests
                    SET resolved_at = ?3, approved = ?4, actor_id = ?2,
                        scopes = COALESCE(?5, scopes)
                  WHERE consent_id = ?1
                    AND resolved_at IS NULL
                    AND expires_at > ?3
                    AND (actor_id = ?2 OR (actor_id IS NULL AND consent_start IS NULL))
                    AND (?6 IS NULL OR scopes = ?6)",
                rusqlite::params![id, &actor[..], now, approved as i64, qualified, stored],
            )
            .context("resolve consent request")?;
        if changed == 0 {
            return Ok(None);
        }
        conn.query_row(
            &format!(
                "SELECT {CONSENT_REQUEST_COLUMNS}
                   FROM atproto_consent_requests WHERE consent_id = ?1"
            ),
            rusqlite::params![consent_id.to_vec()],
            ConsentRequestRow::from_row,
        )
        .optional()
        .context("read back resolved consent request")
    }

    /// Claim the live typed-code row whose user code `typed` names, binding it
    /// to `actor_id`, and return it — the one door to a typed-code start's row
    /// (`authorization-server.md` § Consent: the user opens Fauna and types the
    /// code the device shows).
    ///
    /// `typed` is compared in [`fauna_core::human_code::normalize`]d form, so
    /// display hyphens, spaces and lower-case typing all wash out. A row the
    /// same account already claimed answers again (a user typing twice is one
    /// claim, not an error); a row another account claimed, an answered row
    /// and an expired one are unmatched — `None`, one answer for all of them,
    /// so the reply says nothing about codes the caller does not hold.
    ///
    /// As with resolution, the `WHERE` clause is the authorization: there is
    /// no read-then-write window in which two accounts could both claim.
    pub async fn claim_typed_consent_code(
        &self,
        typed: &str,
        actor_id: &[u8; 32],
    ) -> Result<Option<ConsentRequestRow>> {
        let normalized = fauna_core::human_code::normalize(typed);
        if normalized.len() != TYPED_CODE_LEN {
            return Ok(None);
        }
        let actor = *actor_id;
        let now = now_epoch_millis();
        let conn = self.conn.lock().await;
        // The stored code is in display form; only the generator's own
        // separator is stripped, which is exactly what `normalize` leaves out
        // of an alphabet with no lower case.
        let id: Option<Vec<u8>> = conn
            .query_row(
                "SELECT consent_id FROM atproto_consent_requests
                  WHERE REPLACE(code, '-', '') = ?1
                    AND consent_start = ?2
                    AND resolved_at IS NULL
                    AND expires_at > ?3
                    AND (actor_id IS NULL OR actor_id = ?4)",
                rusqlite::params![normalized, ConsentStartKind::TYPED_CODE, now, &actor[..]],
                |row| row.get(0),
            )
            .optional()
            .context("find typed consent code")?;
        let Some(id) = id else {
            return Ok(None);
        };
        let claimed = conn
            .execute(
                "UPDATE atproto_consent_requests SET actor_id = ?2
                  WHERE consent_id = ?1 AND (actor_id IS NULL OR actor_id = ?2)",
                rusqlite::params![&id, &actor[..]],
            )
            .context("claim typed consent code")?;
        if claimed == 0 {
            return Ok(None);
        }
        conn.query_row(
            &format!(
                "SELECT {CONSENT_REQUEST_COLUMNS}
                   FROM atproto_consent_requests WHERE consent_id = ?1"
            ),
            rusqlite::params![&id],
            ConsentRequestRow::from_row,
        )
        .optional()
        .context("read back claimed consent request")
    }

    /// Whether `actor_id` has ever granted `client_id` — and, when `dpop_jkt`
    /// is given, granted it to **that key**. Any grant row counts, revoked or
    /// expired included: the question is "has this user approved this client
    /// before", which a later sign-out does not un-answer.
    ///
    /// The key form is how a **public** client's prior approval is read
    /// (`authorization-server.md` § Consent rule (a)): nothing authenticates a
    /// public client, so a `client_id` alone is a name anyone may use, and the
    /// installation the user actually approved is the DPoP key its grant was
    /// bound to.
    pub async fn atproto_oauth_client_approved_before(
        &self,
        actor_id: &[u8; 32],
        client_id: &str,
        dpop_jkt: Option<&str>,
    ) -> Result<bool> {
        let actor = *actor_id;
        let conn = self.conn.lock().await;
        let found: Option<i64> = conn
            .query_row(
                "SELECT 1 FROM atproto_oauth_grants
                  WHERE actor_id = ?1 AND client_id = ?2
                    AND (?3 IS NULL OR dpop_jkt = ?3)
                  LIMIT 1",
                rusqlite::params![&actor[..], client_id, dpop_jkt],
                |row| row.get(0),
            )
            .optional()
            .context("read prior approval")?;
        Ok(found.is_some())
    }

    /// Set or clear `actor_id`'s block on `client_id` (`authorization-server.md`
    /// § Consent rule (c)). Idempotent both ways; answers whether the client is
    /// now blocked.
    pub async fn set_oauth_client_block(
        &self,
        actor_id: &[u8; 32],
        client_id: &str,
        blocked: bool,
    ) -> Result<bool> {
        if client_id.is_empty() || client_id.len() > MAX_CONSENT_FIELD_LEN {
            return Err(anyhow!(
                "a blocked client_id must be non-empty and under the {MAX_CONSENT_FIELD_LEN}-byte cap"
            ));
        }
        let actor = *actor_id;
        let conn = self.conn.lock().await;
        if blocked {
            conn.execute(
                "INSERT OR IGNORE INTO oauth_client_blocks (actor_id, client_id, created_at)
                 VALUES (?1, ?2, ?3)",
                rusqlite::params![&actor[..], client_id, now_epoch_millis()],
            )
            .context("block oauth client")?;
        } else {
            conn.execute(
                "DELETE FROM oauth_client_blocks WHERE actor_id = ?1 AND client_id = ?2",
                rusqlite::params![&actor[..], client_id],
            )
            .context("unblock oauth client")?;
        }
        Ok(blocked)
    }

    /// Whether `actor_id` has blocked `client_id`.
    pub async fn oauth_client_blocked(&self, actor_id: &[u8; 32], client_id: &str) -> Result<bool> {
        let actor = *actor_id;
        let conn = self.conn.lock().await;
        let found: Option<i64> = conn
            .query_row(
                "SELECT 1 FROM oauth_client_blocks WHERE actor_id = ?1 AND client_id = ?2",
                rusqlite::params![&actor[..], client_id],
                |row| row.get(0),
            )
            .optional()
            .context("read oauth client block")?;
        Ok(found.is_some())
    }

    /// Every client `actor_id` has blocked, oldest first — the read half the
    /// user needs to see and lift a block from their own app.
    pub async fn list_oauth_client_blocks(
        &self,
        actor_id: &[u8; 32],
    ) -> Result<Vec<(String, i64)>> {
        let actor = *actor_id;
        let conn = self.conn.lock().await;
        let mut stmt = conn
            .prepare(
                "SELECT client_id, created_at FROM oauth_client_blocks
                  WHERE actor_id = ?1 ORDER BY created_at ASC, client_id ASC",
            )
            .context("prepare list oauth client blocks")?;
        let rows = stmt
            .query_map(rusqlite::params![&actor[..]], |row| {
                Ok((row.get(0)?, row.get(1)?))
            })
            .context("query oauth client blocks")?
            .collect::<std::result::Result<Vec<_>, _>>()
            .context("collect oauth client blocks")?;
        Ok(rows)
    }

    /// Drop every consent request whose expiry has passed. Called
    /// opportunistically by [`Self::open_atproto_consent_request`]; exposed so a
    /// nest that never opens another one still converges.
    ///
    /// Not user data: a consent request is a minutes-long question, and the
    /// answer to a swept one is to retry the flow (§ Nest state schema's row).
    pub async fn sweep_expired_atproto_consent_requests(&self) -> Result<usize> {
        let now = now_epoch_millis();
        let conn = self.conn.lock().await;
        sweep_expired_consents(&conn, now)
    }
}

/// Drop every expired consent request — and, first, the capability grants an
/// approved one left wrapped to a holder no principal row names
/// (`third-party-kinds.md` § The record doors: the approving device mints the
/// consent-time grant before it resolves, because the principal row is minted
/// at `/oauth/token` with no client in the loop; a ceremony approved and then
/// never redeemed would otherwise leave keys deposited for nobody). The same
/// owner-scoped deletion `fauna.principals.revoke` performs for a deleted row,
/// selected by the expired request's attested holder; a holder some row of
/// the account names — the ceremony was redeemed, or an earlier one was — is
/// that row's, and kept. Answers how many requests were dropped.
fn sweep_expired_consents(conn: &rusqlite::Connection, now: i64) -> Result<usize> {
    let ended = conn
        .execute(
            "DELETE FROM capability_grants
              WHERE EXISTS (
                    SELECT 1 FROM atproto_consent_requests c
                     WHERE c.expires_at <= ?1 AND c.approved = 1
                       AND c.actor_id = capability_grants.owner_actor_id
                       AND c.holder_x25519 = capability_grants.holder_pubkey)
                AND NOT EXISTS (
                    SELECT 1 FROM third_party_principals p
                     WHERE p.actor_id = capability_grants.owner_actor_id
                       AND p.holder_x25519 = capability_grants.holder_pubkey)",
            rusqlite::params![now],
        )
        .context("end unredeemed consent grants")?;
    if ended > 0 {
        tracing::info!(
            grants = ended,
            "consent: ended capability grants an expired, unredeemed approval left behind"
        );
    }
    conn.execute(
        "DELETE FROM atproto_consent_requests WHERE expires_at <= ?1",
        rusqlite::params![now],
    )
    .context("sweep expired consent requests")
}

/// Mint a binding code no live row already holds.
///
/// A code must be unique among **every live row**, not merely within one
/// bucket: an unassigned request is rendered on the same card list as an
/// account's own, so two cards showing one code is exactly the confusion the
/// code exists to prevent.
///
/// **Why this is its own function rather than a loop at the call site.** The
/// journey-level "no two live rows share a code" assertion cannot fail against a
/// deleted uniqueness check — at 30 bits over a table of a dozen rows a natural
/// collision is a one-in-ten-million event, so the pin would report green with
/// the mechanism gone (finding 61's class). Taking the generator as a parameter
/// makes the retry itself testable with a generator that *deliberately*
/// collides, which is a pin that can actually go red.
///
/// The budget is bounded on purpose: exhausting it is a real, loud failure,
/// while the alternative — emitting a duplicate — is the one outcome this
/// exists to prevent.
fn mint_unique_consent_code(
    mut mint: impl FnMut() -> String,
    mut taken: impl FnMut(&str) -> Result<bool>,
) -> Result<String> {
    for _ in 0..CONSENT_CODE_MINT_ATTEMPTS {
        let candidate = mint();
        if !taken(&candidate)? {
            return Ok(candidate);
        }
    }
    Err(anyhow!(
        "could not mint a unique consent code in {CONSENT_CODE_MINT_ATTEMPTS} attempts"
    ))
}

/// Map a rusqlite write error to [`CredentialWriteError`], recognizing the
/// `PRIMARY KEY(actor_id, credential_id)` violation as `Duplicate`.
fn map_credential_write_err(e: rusqlite::Error) -> CredentialWriteError {
    use rusqlite::ErrorCode;
    if let rusqlite::Error::SqliteFailure(f, _) = &e
        && f.code == ErrorCode::ConstraintViolation
    {
        return CredentialWriteError::Duplicate;
    }
    CredentialWriteError::Other(anyhow::Error::from(e).context("put atproto app credential"))
}

#[cfg(test)]
mod tests {
    use super::*;

    const ACTOR: [u8; 32] = [7u8; 32];

    #[tokio::test]
    async fn credential_round_trip_and_list() {
        let db = CacheDb::open_in_memory().unwrap();
        db.put_atproto_app_credential(&ACTOR, "ivory-1", "ivory", "$argon2id$fake", true)
            .await
            .unwrap();
        let rows = db.list_atproto_app_credentials(&ACTOR).await.unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].credential_id, "ivory-1");
        assert!(rows[0].dm_allowed);
        assert!(rows[0].last_used_at.is_none());

        db.touch_atproto_credential_last_used(&ACTOR, "ivory-1")
            .await
            .unwrap();
        let rows = db.list_atproto_app_credentials(&ACTOR).await.unwrap();
        assert!(rows[0].last_used_at.is_some());
    }

    /// The forced session-secret rotation's reach, at the storage layer: the
    /// sweep must end the nest AS's live grants and **only** those.
    ///
    /// The selection is positive on the nest's mark: re-minting the nest's HS256
    /// refresh secret kills exactly the families the nest MACed (§ The issuer →
    /// *Two HS256 secrets, not one*). Every grant row has carried a mark since
    /// schema 102, so what this pins is the live-view scope and the count.
    #[tokio::test]
    async fn only_live_nest_issued_grants_are_ended_by_a_forced_rotation() {
        let db = CacheDb::open_in_memory().unwrap();
        let other: [u8; 32] = [9u8; 32];

        let record = async |db: &CacheDb, actor: &[u8; 32], id: &[u8], issuer| {
            db.record_atproto_oauth_grant(
                actor,
                id,
                "https://app.example/client",
                Some("An App"),
                "atproto",
                &[],
                "jkt",
                9_999_999_999,
                None,
                issuer,
                &crate::db::third_party_principals::UNATTESTED_DEVICE,
            )
            .await
            .expect("record the grant")
        };

        // Two accounts, so the read is proven CROSS-actor: a per-actor shape
        // could not express the rotation's reach at all.
        record(&db, &ACTOR, b"nest-a", OAUTH_GRANT_ISSUER_NEST).await;
        record(&db, &other, b"nest-b", OAUTH_GRANT_ISSUER_NEST).await;

        // A grant already ended is not this rotation's to count: `grants_ended`
        // reports what the response DID, not how many rows carry the mark. So
        // end one up front and watch the sweep decline to claim it.
        assert!(
            db.revoke_atproto_session(&ACTOR, b"nest-a").await.unwrap(),
            "precondition: the grant was live"
        );

        let ended = db.end_nest_minted_oauth_grants().await.unwrap();
        assert_eq!(
            ended.ended, 1,
            "only the still-live nest-minted grant is ended and counted — the \
             already-revoked one is not this response's to claim"
        );
        assert_eq!(
            ended.actors,
            vec![other],
            "and only the account that actually lost a connection is nudged"
        );

        assert!(
            db.list_atproto_oauth_grants(&other)
                .await
                .unwrap()
                .is_empty(),
            "the other account's nest-minted grant stops being listed"
        );

        // Proven CROSS-actor: a second account's grant was reached by one call,
        // which a per-actor shape could not express at all.
        let again = db.end_nest_minted_oauth_grants().await.unwrap();
        assert_eq!(
            again,
            EndedOAuthGrants::default(),
            "a second rotation ends nothing — the sweep is idempotent"
        );
    }

    /// **The one-nudge-per-actor collapse, pinned where it is decided.** The
    /// nudge names an actor, not a session, so `end_nest_issued_oauth_sessions`
    /// pushes once per entry of `actors` — which makes the dedupe here the whole
    /// of the collapse. An account losing two grants must appear once, or a user
    /// with four connected apps is pushed four identical frames for one act.
    #[tokio::test]
    async fn the_sweep_names_each_affected_actor_once_however_many_grants_it_lost() {
        let db = CacheDb::open_in_memory().unwrap();
        let other: [u8; 32] = [9u8; 32];
        let record = async |actor: &[u8; 32], id: &[u8]| {
            db.record_atproto_oauth_grant(
                actor,
                id,
                "https://app.example/client",
                Some("An App"),
                "atproto",
                &[],
                "jkt",
                now_epoch_millis() + 1_000_000,
                None,
                OAUTH_GRANT_ISSUER_NEST,
                &crate::db::third_party_principals::UNATTESTED_DEVICE,
            )
            .await
            .expect("record the grant")
        };
        record(&ACTOR, b"a-1").await;
        record(&ACTOR, b"a-2").await;
        record(&other, b"b-1").await;

        let ended = db.end_nest_minted_oauth_grants().await.unwrap();
        assert_eq!(ended.ended, 3, "every grant is counted");
        let mut actors = ended.actors.clone();
        actors.sort();
        assert_eq!(
            actors,
            vec![ACTOR, other],
            "two actors, each named once — three grants must not become three \
             nudges carrying no extra information"
        );
    }

    /// The count means *rows that left the connected-apps surface*, so the sweep
    /// is scoped to the same live view `list_atproto_oauth_grants` serves — and
    /// `expires_at` is half of that predicate, not just `revoked_at`.
    ///
    /// An expired grant is already invisible to the user, so ending it changes
    /// nothing they can see while inflating the number an admin reads about a
    /// compromise response, in the one direction that makes the response sound
    /// bigger than it was.
    #[tokio::test]
    async fn the_sweep_counts_only_grants_a_user_could_still_see() {
        let db = CacheDb::open_in_memory().unwrap();
        let past = now_epoch_millis() - 1;
        let far = now_epoch_millis() + 1_000_000;
        let record = async |id: &[u8], expires| {
            db.record_atproto_oauth_grant(
                &ACTOR,
                id,
                "https://app.example/client",
                Some("An App"),
                "atproto",
                &[],
                "jkt",
                far,
                expires,
                OAUTH_GRANT_ISSUER_NEST,
                &crate::db::third_party_principals::UNATTESTED_DEVICE,
            )
            .await
            .expect("record the grant")
        };
        record(b"live", None).await;
        record(b"expired", Some(past)).await;
        assert_eq!(
            db.list_atproto_oauth_grants(&ACTOR).await.unwrap().len(),
            1,
            "precondition: only one of the two is on the user's surface"
        );

        let ended = db.end_nest_minted_oauth_grants().await.unwrap();
        assert_eq!(
            ended.ended, 1,
            "one connected-apps row disappeared, so the count is 1"
        );
        assert!(
            db.list_atproto_oauth_grants(&ACTOR)
                .await
                .unwrap()
                .is_empty()
        );
    }

    /// The mark joins the replay discriminator rather than riding along: the
    /// same grant id under ANOTHER issuer is a different authorization, and
    /// answering `Ok(())` to it would leave the row pointing at the wrong
    /// issuer — which is exactly the row a forced rotation then fails to end.
    /// The nest is the one issuer standing, so the other mark here is a
    /// stand-in value.
    #[tokio::test]
    async fn the_same_grant_id_from_the_other_issuer_is_a_collision_not_a_replay() {
        let db = CacheDb::open_in_memory().unwrap();
        let args = |issuer| (issuer,);
        let record = async |issuer| {
            db.record_atproto_oauth_grant(
                &ACTOR,
                b"same-id",
                "https://app.example/client",
                Some("An App"),
                "atproto",
                &[],
                "jkt",
                9_999_999_999,
                None,
                args(issuer).0,
                &crate::db::third_party_principals::UNATTESTED_DEVICE,
            )
            .await
        };

        record(OAUTH_GRANT_ISSUER_NEST)
            .await
            .expect("the first record lands");
        record(OAUTH_GRANT_ISSUER_NEST)
            .await
            .expect("the SAME exchange asked twice is still a replay — write nothing, answer Ok");
        let err = record("another-issuer")
            .await
            .expect_err("the same id under another issuer is a collision, not a replay");
        assert!(
            err.to_string().contains("collision"),
            "the refusal names the collision: {err}"
        );
        let ended = db.end_nest_minted_oauth_grants().await.unwrap();
        assert_eq!(
            (ended.ended, ended.actors.as_slice()),
            (1, [ACTOR].as_slice()),
            "and the stored row keeps its original issuer — a forced rotation \
             still reaches it"
        );
    }

    #[tokio::test]
    async fn verifier_size_cap_enforced() {
        let db = CacheDb::open_in_memory().unwrap();
        let huge = "x".repeat(MAX_VERIFIER_LEN + 1);
        assert!(
            db.put_atproto_app_credential(&ACTOR, "c", "l", &huge, false)
                .await
                .is_err()
        );
    }

    #[tokio::test]
    async fn duplicate_credential_id_is_refused_not_upserted() {
        let db = CacheDb::open_in_memory().unwrap();
        db.put_atproto_app_credential(&ACTOR, "ivory-1", "ivory", "$argon2id$first", true)
            .await
            .unwrap();
        let e = db
            .put_atproto_app_credential(&ACTOR, "ivory-1", "ivory", "$argon2id$second", false)
            .await
            .expect_err("a PK collision must not upsert");
        assert!(matches!(e, CredentialWriteError::Duplicate), "{e:?}");

        // The first row is untouched — verifier and dm_allowed both.
        let rows = db.list_atproto_app_credentials(&ACTOR).await.unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].verifier, "$argon2id$first");
        assert!(rows[0].dm_allowed);

        // A different id on the same actor still inserts.
        db.put_atproto_app_credential(&ACTOR, "ivory-2", "ivory", "$argon2id$other", false)
            .await
            .unwrap();
        assert_eq!(
            db.list_atproto_app_credentials(&ACTOR).await.unwrap().len(),
            2
        );
    }

    #[tokio::test]
    async fn credential_revoke_cascades_to_sessions() {
        let db = CacheDb::open_in_memory().unwrap();
        db.put_atproto_app_credential(&ACTOR, "cred-a", "a", "$v", false)
            .await
            .unwrap();
        let far = now_epoch_millis() + 1_000_000;
        db.insert_atproto_session(
            &ACTOR,
            b"sess-1",
            "app_credential",
            Some("cred-a"),
            None,
            far,
        )
        .await
        .unwrap();
        db.insert_atproto_session(
            &ACTOR,
            b"sess-2",
            "app_credential",
            Some("cred-a"),
            None,
            far,
        )
        .await
        .unwrap();
        db.insert_atproto_session(
            &ACTOR,
            b"sess-3",
            "app_credential",
            Some("cred-b"),
            None,
            far,
        )
        .await
        .unwrap();

        let (existed, revoked) = db
            .revoke_atproto_app_credential(&ACTOR, "cred-a")
            .await
            .unwrap();
        assert!(existed);
        assert_eq!(revoked, 2);
        let live = db.list_atproto_sessions(&ACTOR).await.unwrap();
        assert_eq!(live.len(), 1);
        assert_eq!(live[0].session_id, b"sess-3");

        // Idempotent second revoke.
        let (existed, revoked) = db
            .revoke_atproto_app_credential(&ACTOR, "cred-a")
            .await
            .unwrap();
        assert!(!existed);
        assert_eq!(revoked, 0);
    }

    #[tokio::test]
    async fn session_list_hides_revoked_and_expired() {
        let db = CacheDb::open_in_memory().unwrap();
        let far = now_epoch_millis() + 1_000_000;
        db.insert_atproto_session(&ACTOR, b"live", "app_credential", None, Some("Ivory"), far)
            .await
            .unwrap();
        db.insert_atproto_session(&ACTOR, b"expired", "app_credential", None, None, 1)
            .await
            .unwrap();
        db.insert_atproto_session(&ACTOR, b"revoked", "app_credential", None, None, far)
            .await
            .unwrap();
        assert!(db.revoke_atproto_session(&ACTOR, b"revoked").await.unwrap());

        let live = db.list_atproto_sessions(&ACTOR).await.unwrap();
        assert_eq!(live.len(), 1);
        assert_eq!(live[0].session_id, b"live");
        assert_eq!(live[0].client_note.as_deref(), Some("Ivory"));

        // Revoking an already-revoked or unknown session reports false.
        assert!(!db.revoke_atproto_session(&ACTOR, b"revoked").await.unwrap());
        assert!(!db.revoke_atproto_session(&ACTOR, b"nope").await.unwrap());
    }

    /// Helper: an OAuth grant and its family, as `/oauth/token` records it.
    async fn record_grant(db: &CacheDb, id: &[u8], expires: Option<i64>) {
        let far = now_epoch_millis() + 1_000_000;
        db.record_atproto_oauth_grant(
            &ACTOR,
            id,
            "https://app.example/client-metadata.json",
            Some("Example App"),
            "atproto repo:app.bsky.feed.post",
            &[],
            "jkt-abc",
            far,
            expires,
            OAUTH_GRANT_ISSUER_NEST,
            &crate::db::third_party_principals::UNATTESTED_DEVICE,
        )
        .await
        .unwrap();
    }

    /// A replayed `record_grant` is a no-op that reports success — the claim
    /// `fauna.bridges.atproto.record_grant`'s `forbid_replay = false` makes
    /// (`transport.md` § Idempotency and reconnect-with-resume). Until the
    /// discriminator arm landed this was a `PRIMARY KEY` violation: the caller
    /// whose reply was lost between commit and ack could never get it back.
    #[tokio::test]
    async fn a_replayed_grant_is_a_no_op_that_still_reports_success() {
        let db = CacheDb::open_in_memory().unwrap();
        record_grant(&db, b"grant-replayed", None).await;
        record_grant(&db, b"grant-replayed", None).await;

        let listed = db.list_atproto_oauth_grants(&ACTOR).await.unwrap();
        assert_eq!(
            listed
                .iter()
                .filter(|r| r.grant_id == b"grant-replayed")
                .count(),
            1,
            "one exchange, one connected-apps row — a replay adds nothing"
        );
        let sessions = db.list_atproto_sessions(&ACTOR).await.unwrap();
        assert_eq!(
            sessions
                .iter()
                .filter(|r| r.session_id == b"grant-replayed")
                .count(),
            1,
            "and one session family, not two"
        );
    }

    /// **The trap that rules out `INSERT OR REPLACE`.**
    /// `atproto_sessions.current_refresh_jti` is NULL only until the first
    /// rotation, and `refresh_atproto_session` reads NULL as *"the initial jti
    /// (= `grant_id`) is current"*. So a replay that re-landed the session row
    /// would hand a **spent** refresh token back its validity and disarm the
    /// reuse family-kill — the one mechanism the rotate-on-use registry exists
    /// for. The replay must write nothing.
    #[tokio::test]
    async fn a_replayed_grant_does_not_revive_a_rotated_away_refresh_token() {
        let db = CacheDb::open_in_memory().unwrap();
        let far = now_epoch_millis() + 1_000_000;
        record_grant(&db, b"grant-rotating", None).await;

        // The first refresh: the initial jti (the grant id) rotates to jti-2.
        assert_eq!(
            db.refresh_atproto_session(&ACTOR, b"grant-rotating", b"grant-rotating", b"jti-2", far)
                .await
                .unwrap(),
            RefreshOutcome::Rotated
        );

        // Now the lost-reply replay of the ORIGINAL record_grant arrives.
        record_grant(&db, b"grant-rotating", None).await;

        // The spent initial jti must still be spent.
        assert_eq!(
            db.refresh_atproto_session(&ACTOR, b"grant-rotating", b"grant-rotating", b"jti-3", far)
                .await
                .unwrap(),
            RefreshOutcome::ReuseDetected,
            "the replay must not reset current_refresh_jti to NULL"
        );
    }

    /// A replay must not resurrect a grant the user has revoked — the same
    /// rule one column over. Revocation is the user's act; a retry of the
    /// exchange that created the grant is not a licence to undo it.
    #[tokio::test]
    async fn a_replayed_grant_does_not_resurrect_a_revoked_one() {
        let db = CacheDb::open_in_memory().unwrap();
        record_grant(&db, b"grant-revoked-then-replayed", None).await;
        assert!(
            db.revoke_atproto_session(&ACTOR, b"grant-revoked-then-replayed")
                .await
                .unwrap()
        );

        record_grant(&db, b"grant-revoked-then-replayed", None).await;

        let sessions = db.list_atproto_sessions(&ACTOR).await.unwrap();
        assert!(
            !sessions
                .iter()
                .any(|r| r.session_id == b"grant-revoked-then-replayed"),
            "the revoked family stays dead"
        );
    }

    /// **The trap that rules out an unconditional `ON CONFLICT DO NOTHING`.**
    /// The discriminator is the authorization, not the id alone: a *different*
    /// client, scope set, or DPoP key filed under a colliding `grant_id` is a
    /// bug or a genuine collision, and it must surface rather than be silently
    /// swallowed — otherwise the caller is answered `ok` for a grant the
    /// connected-apps row does not describe.
    #[tokio::test]
    async fn a_different_authorization_under_a_colliding_grant_id_still_errors() {
        let db = CacheDb::open_in_memory().unwrap();
        let far = now_epoch_millis() + 1_000_000;
        record_grant(&db, b"grant-collision", None).await;

        assert!(
            db.record_atproto_oauth_grant(
                &ACTOR,
                b"grant-collision",
                // A different client, everything else as before.
                "https://other.example/client-metadata.json",
                Some("Example App"),
                "atproto repo:app.bsky.feed.post",
                &[],
                "jkt-abc",
                far,
                None,
                OAUTH_GRANT_ISSUER_NEST,
                &crate::db::third_party_principals::UNATTESTED_DEVICE,
            )
            .await
            .is_err(),
            "a different authorization under the same id is not a replay"
        );
    }

    /// **The same cap break as the consent row's, one step later — and this one
    /// costs the user more.** A permission-set grant at the bridge's own
    /// ceiling now survives `/oauth/authorize`; if it dies here it dies at
    /// `/oauth/token`, *after* the human read the card and approved it. So the
    /// grant registry's `scopes` ceiling is the same one number the ceremony
    /// uses throughout, and only the identity fields keep the narrow one.
    #[tokio::test]
    async fn a_max_size_grant_expansion_records_a_grant() {
        let db = CacheDb::open_in_memory().unwrap();
        let far = now_epoch_millis() + 1_000_000;
        let at_cap = "x".repeat(MAX_CONSENT_SCOPES_LEN);
        db.record_atproto_oauth_grant(
            &ACTOR,
            b"grant-at-cap",
            "https://app.example/c",
            None,
            &at_cap,
            &[],
            "jkt-abc",
            far,
            None,
            OAUTH_GRANT_ISSUER_NEST,
            &crate::db::third_party_principals::UNATTESTED_DEVICE,
        )
        .await
        .expect("a grant the user already approved must record");

        let over_cap = "x".repeat(MAX_CONSENT_SCOPES_LEN + 1);
        assert!(
            db.record_atproto_oauth_grant(
                &ACTOR,
                b"grant-over-cap",
                "https://app.example/c",
                None,
                &over_cap,
                &[],
                "jkt-abc",
                far,
                None,
                OAUTH_GRANT_ISSUER_NEST,
                &crate::db::third_party_principals::UNATTESTED_DEVICE,
            )
            .await
            .is_err(),
            "past the ceiling it is still refused"
        );
    }

    /// The grant's provenance survives storage, and is read back on the
    /// connected-apps path — the second half of the ceremony's one frozen
    /// expansion (`atproto-pds-full.md:330`, "carried through the consent row
    /// into the grant row").
    #[tokio::test]
    async fn a_grants_permission_sets_round_trip_to_the_connected_apps_read() {
        let db = CacheDb::open_in_memory().unwrap();
        let sets = vec![ConsentSetInfo {
            nsid: "com.example.calendar.appPerms".into(),
            title: Some("Calendar\nsync".into()),
            details: Some("Keeps your calendar in step.".into()),
            members: vec!["repo:com.example.event".into()],
            extra: Default::default(),
        }];
        db.record_atproto_oauth_grant(
            &ACTOR,
            b"grant-with-sets",
            "https://app.example/c",
            None,
            "atproto repo:com.example.event",
            &sets,
            "jkt-abc",
            now_epoch_millis() + 1_000_000,
            None,
            OAUTH_GRANT_ISSUER_NEST,
            &crate::db::third_party_principals::UNATTESTED_DEVICE,
        )
        .await
        .unwrap();

        let listed = db.list_atproto_oauth_grants(&ACTOR).await.unwrap();
        let row = listed
            .iter()
            .find(|r| r.grant_id == b"grant-with-sets")
            .expect("the grant is live");
        assert_eq!(row.sets, sets);
        assert_eq!(
            row.sets[0].title.as_deref(),
            Some("Calendar\nsync"),
            "raw — the fence is the shared machine's, not this layer's"
        );
    }

    /// A grant naming no set stores NULL and reads back empty, identical to
    /// every grant recorded before this column existed.
    #[tokio::test]
    async fn a_grant_with_no_sets_reads_back_like_a_pre_ps_b_grant() {
        let db = CacheDb::open_in_memory().unwrap();
        record_grant(&db, b"plain", None).await;
        let listed = db.list_atproto_oauth_grants(&ACTOR).await.unwrap();
        assert!(listed.iter().all(|r| r.sets.is_empty()));
    }

    /// The connected-apps read hides exactly what the session read hides —
    /// they render one surface, and a grant visible past its session (or the
    /// reverse) is a connection the user cannot act on.
    #[tokio::test]
    async fn grant_list_hides_revoked_and_expired_and_keeps_open_ended() {
        let db = CacheDb::open_in_memory().unwrap();
        let past = now_epoch_millis() - 1;
        let far = now_epoch_millis() + 1_000_000;
        record_grant(&db, b"live", Some(far)).await;
        // A confidential client's refresh horizon may be open-ended: NULL is
        // "no deadline", a different fact from a deadline in the past.
        record_grant(&db, b"open", None).await;
        record_grant(&db, b"expired", Some(past)).await;
        record_grant(&db, b"revoked", Some(far)).await;
        assert!(db.revoke_atproto_session(&ACTOR, b"revoked").await.unwrap());

        let live = db.list_atproto_oauth_grants(&ACTOR).await.unwrap();
        let ids: Vec<&[u8]> = live.iter().map(|g| g.grant_id.as_slice()).collect();
        assert_eq!(ids, vec![&b"live"[..], &b"open"[..]]);
        assert_eq!(
            live[0].client_id,
            "https://app.example/client-metadata.json"
        );
        assert_eq!(live[0].client_name.as_deref(), Some("Example App"));
        assert_eq!(live[0].scopes, "atproto repo:app.bsky.feed.post");
        assert!(live[0].last_used_at.is_none(), "never used yet");
        assert!(
            live[1].expires_at.is_none(),
            "open-ended horizon stays live"
        );
    }

    /// **Slice 8a's headline property.** Revoking the session revokes the
    /// grant, in one transaction — the other half of `record_grant`'s
    /// one-transaction write. A live grant row beside a dead family is a
    /// connection the connected-apps surface shows and cannot act on.
    #[tokio::test]
    async fn revoking_a_session_cascades_to_its_oauth_grant() {
        let db = CacheDb::open_in_memory().unwrap();
        let far = now_epoch_millis() + 1_000_000;
        record_grant(&db, b"fam", Some(far)).await;
        assert_eq!(db.list_atproto_oauth_grants(&ACTOR).await.unwrap().len(), 1);
        assert_eq!(db.list_atproto_sessions(&ACTOR).await.unwrap().len(), 1);

        assert!(db.revoke_atproto_session(&ACTOR, b"fam").await.unwrap());

        assert!(
            db.list_atproto_oauth_grants(&ACTOR)
                .await
                .unwrap()
                .is_empty(),
            "the grant row must die with its family"
        );
        assert!(db.list_atproto_sessions(&ACTOR).await.unwrap().is_empty());
        // Idempotent from both directions.
        assert!(!db.revoke_atproto_session(&ACTOR, b"fam").await.unwrap());
    }

    /// The cascade must not invent a grant for the app-credential plane, whose
    /// sessions have no grant row at all. Keeping the plane out of the SQL is
    /// what makes this a no-op rather than a branch that could disagree.
    #[tokio::test]
    async fn revoking_an_app_credential_session_touches_no_grant() {
        let db = CacheDb::open_in_memory().unwrap();
        let far = now_epoch_millis() + 1_000_000;
        db.insert_atproto_session(&ACTOR, b"app-fam", "app_credential", Some("c1"), None, far)
            .await
            .unwrap();
        record_grant(&db, b"oauth-fam", Some(far)).await;

        assert!(db.revoke_atproto_session(&ACTOR, b"app-fam").await.unwrap());

        let grants = db.list_atproto_oauth_grants(&ACTOR).await.unwrap();
        assert_eq!(grants.len(), 1, "the unrelated OAuth grant is untouched");
        assert_eq!(grants[0].grant_id, b"oauth-fam");
    }

    /// `ui/atproto.md`'s downward transition matrix: a step-down (or
    /// `delete_presence`'s login-plane teardown) SUSPENDS grants rather than
    /// revoking them — "credential/grant rows are kept, listed, individually
    /// revocable; stepping back up restores usability." `revoke_all_atproto_sessions`
    /// kills every session and deliberately leaves grant rows untouched at
    /// rest; the read side must still tell suspended apart from live, or the
    /// connected-apps surface shows a dead connection as though it worked.
    #[tokio::test]
    async fn revoking_all_sessions_suspends_but_does_not_hide_the_grant() {
        let db = CacheDb::open_in_memory().unwrap();
        let far = now_epoch_millis() + 1_000_000;
        record_grant(&db, b"fam", Some(far)).await;
        assert!(
            !db.list_atproto_oauth_grants(&ACTOR).await.unwrap()[0].suspended,
            "an ordinary live grant is not suspended"
        );

        assert_eq!(db.revoke_all_atproto_sessions(&ACTOR).await.unwrap(), 1);

        assert!(
            db.list_atproto_sessions(&ACTOR).await.unwrap().is_empty(),
            "the session itself is gone"
        );
        let listed = db.list_atproto_oauth_grants(&ACTOR).await.unwrap();
        assert_eq!(
            listed.len(),
            1,
            "the grant row must survive a step-down, not disappear"
        );
        assert!(
            listed[0].suspended,
            "its session is dead, so it must read as suspended"
        );

        // Idempotent, and re-suspending an already-suspended grant changes nothing.
        assert_eq!(db.revoke_all_atproto_sessions(&ACTOR).await.unwrap(), 0);
        assert!(db.list_atproto_oauth_grants(&ACTOR).await.unwrap()[0].suspended);
    }

    /// The other half of the derivation's promise, and the half nothing pinned:
    /// **suspension UN-suspends itself.**
    ///
    /// `suspended` is derived, never stored, precisely so that stepping back up
    /// needs no "un-suspend" write for anyone to forget — a claim this module
    /// states twice (`AtprotoOauthGrantRow::suspended`,
    /// `revoke_all_atproto_sessions`) and the connected-apps surface now renders
    /// off. Only the suspending direction was pinned, so the self-healing half
    /// was a load-bearing claim with nothing holding it.
    #[tokio::test]
    async fn a_fresh_session_un_suspends_its_grant_with_no_un_suspend_write() {
        let db = CacheDb::open_in_memory().unwrap();
        let far = now_epoch_millis() + 1_000_000;
        record_grant(&db, b"fam", Some(far)).await;
        assert_eq!(db.revoke_all_atproto_sessions(&ACTOR).await.unwrap(), 1);
        assert!(
            db.list_atproto_oauth_grants(&ACTOR).await.unwrap()[0].suspended,
            "precondition: the step-down suspended it"
        );

        // Stepping back up re-mints the session family under the SAME id — the
        // grant id IS the session id, the identity the whole F4 slice-8a join
        // rests on — so the revoked row is replaced rather than accumulated.
        db.insert_atproto_session(&ACTOR, b"fam", "oauth", None, Some("Example App"), far)
            .await
            .unwrap();

        let listed = db.list_atproto_oauth_grants(&ACTOR).await.unwrap();
        assert_eq!(listed.len(), 1, "still exactly one grant - nothing cloned");
        assert!(
            !listed[0].suspended,
            "usability is restored by the session alone: no grant-table write \
             happens here, and none may be required"
        );
        assert_eq!(
            db.list_atproto_sessions(&ACTOR).await.unwrap().len(),
            1,
            "and the replaced row did not leave a dead twin behind"
        );
    }

    /// D10 § Audit's advisory half: the nest stamps `last_used_at` at the one
    /// moment it observes a grant in use — a successful rotation. A reuse
    /// family-kill must NOT stamp it (nothing was legitimately used) and must
    /// cascade to the grant for the same reason an explicit revoke does.
    #[tokio::test]
    async fn rotation_stamps_grant_last_used_and_reuse_kills_the_grant() {
        let db = CacheDb::open_in_memory().unwrap();
        let far = now_epoch_millis() + 1_000_000;
        record_grant(&db, b"fam", Some(far)).await;
        assert!(
            db.list_atproto_oauth_grants(&ACTOR).await.unwrap()[0]
                .last_used_at
                .is_none()
        );

        assert_eq!(
            db.refresh_atproto_session(&ACTOR, b"fam", b"fam", b"jti-2", far + 10)
                .await
                .unwrap(),
            RefreshOutcome::Rotated
        );
        let used = db.list_atproto_oauth_grants(&ACTOR).await.unwrap()[0].last_used_at;
        assert!(used.is_some(), "a rotation is the grant being used");

        // Replaying the superseded jti kills the family — and the grant with it.
        assert_eq!(
            db.refresh_atproto_session(&ACTOR, b"fam", b"fam", b"jti-3", far + 20)
                .await
                .unwrap(),
            RefreshOutcome::ReuseDetected
        );
        assert!(
            db.list_atproto_oauth_grants(&ACTOR)
                .await
                .unwrap()
                .is_empty(),
            "reuse detection is exactly when the row must stop reading as live"
        );
    }

    #[tokio::test]
    async fn refresh_rotates_then_detects_reuse_and_kills_family() {
        let db = CacheDb::open_in_memory().unwrap();
        let far = now_epoch_millis() + 1_000_000;
        db.insert_atproto_session(&ACTOR, b"fam", "app_credential", None, None, far)
            .await
            .unwrap();

        // First refresh presents the initial jti (== session_id).
        let out = db
            .refresh_atproto_session(&ACTOR, b"fam", b"fam", b"jti-2", far + 10)
            .await
            .unwrap();
        assert_eq!(out, RefreshOutcome::Rotated);

        // Second rotation with the current jti.
        let out = db
            .refresh_atproto_session(&ACTOR, b"fam", b"jti-2", b"jti-3", far + 20)
            .await
            .unwrap();
        assert_eq!(out, RefreshOutcome::Rotated);

        // Replay of the superseded jti-2 → reuse detected, family killed.
        let out = db
            .refresh_atproto_session(&ACTOR, b"fam", b"jti-2", b"jti-4", far + 30)
            .await
            .unwrap();
        assert_eq!(out, RefreshOutcome::ReuseDetected);
        assert!(db.list_atproto_sessions(&ACTOR).await.unwrap().is_empty());

        // The whole family is dead: even the would-be-current jti-3 refuses.
        let out = db
            .refresh_atproto_session(&ACTOR, b"fam", b"jti-3", b"jti-5", far + 40)
            .await
            .unwrap();
        assert_eq!(out, RefreshOutcome::Invalid);
    }

    /// A dropped connection mid-refresh must not log the user out of a
    /// connected app — and must not write a token-theft signal that never
    /// happened.
    ///
    /// `fauna.bridges.atproto.refresh_session` is `forbid_replay = false`,
    /// which asserts this handler is naturally idempotent; the per-connection
    /// idempotency cache cannot help, because `request_auto_retry` re-issues on
    /// a fresh connection. So the retry arrives as the byte-identical
    /// `(presented_jti, new_jti)` pair — and before 2026-08-02 it fell into the
    /// reuse branch and family-killed a live session.
    ///
    /// The distinction that makes recognising it safe is asserted here too: a
    /// *stolen* token produces the same superseded `presented_jti` but a
    /// freshly-minted `new_jti`, so it must still be caught.
    #[tokio::test]
    async fn a_replayed_refresh_is_the_same_rotation_not_a_reuse() {
        let db = CacheDb::open_in_memory().unwrap();
        let far = now_epoch_millis() + 1_000_000;
        db.insert_atproto_session(&ACTOR, b"fam", "app_credential", None, None, far)
            .await
            .unwrap();

        let first = db
            .refresh_atproto_session(&ACTOR, b"fam", b"fam", b"jti-2", far + 10)
            .await
            .unwrap();
        assert_eq!(first, RefreshOutcome::Rotated);

        // The retry: same request, re-issued on a fresh connection.
        let retry = db
            .refresh_atproto_session(&ACTOR, b"fam", b"fam", b"jti-2", far + 10)
            .await
            .unwrap();
        assert_eq!(
            retry,
            RefreshOutcome::Rotated,
            "a re-issued refresh must report what actually happened — the \
             rotation it asked for is the one already in the row"
        );
        assert_eq!(
            db.list_atproto_sessions(&ACTOR).await.unwrap().len(),
            1,
            "and the session must survive: a network blip is not a theft"
        );

        // The session is genuinely usable afterwards — the retry left the row
        // in the state the first call put it in, not in some third state.
        let next = db
            .refresh_atproto_session(&ACTOR, b"fam", b"jti-2", b"jti-3", far + 20)
            .await
            .unwrap();
        assert_eq!(next, RefreshOutcome::Rotated);

        // …and the theft shape is untouched: the superseded jti-2 presented
        // with a DIFFERENT new jti is what a thief's call looks like, because
        // the bridge mints a fresh one for every refresh it attempts.
        let stolen = db
            .refresh_atproto_session(&ACTOR, b"fam", b"jti-2", b"jti-9", far + 30)
            .await
            .unwrap();
        assert_eq!(stolen, RefreshOutcome::ReuseDetected);
        assert!(db.list_atproto_sessions(&ACTOR).await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn refresh_unknown_or_expired_is_invalid() {
        let db = CacheDb::open_in_memory().unwrap();
        let out = db
            .refresh_atproto_session(&ACTOR, b"none", b"none", b"j2", 10)
            .await
            .unwrap();
        assert_eq!(out, RefreshOutcome::Invalid);

        db.insert_atproto_session(&ACTOR, b"old", "app_credential", None, None, 1)
            .await
            .unwrap();
        let out = db
            .refresh_atproto_session(&ACTOR, b"old", b"old", b"j2", 10)
            .await
            .unwrap();
        assert_eq!(out, RefreshOutcome::Invalid);
    }

    #[tokio::test]
    async fn external_apps_flag_defaults_on_and_flips() {
        let db = CacheDb::open_in_memory().unwrap();
        assert!(db.get_atproto_external_apps_enabled(&ACTOR).await.unwrap());
        db.set_atproto_external_apps_enabled(&ACTOR, false)
            .await
            .unwrap();
        assert!(!db.get_atproto_external_apps_enabled(&ACTOR).await.unwrap());
        db.set_atproto_external_apps_enabled(&ACTOR, true)
            .await
            .unwrap();
        assert!(db.get_atproto_external_apps_enabled(&ACTOR).await.unwrap());
    }

    #[tokio::test]
    async fn native_record_round_trip_and_per_collection_list() {
        let db = CacheDb::open_in_memory().unwrap();
        let coll = "app.bsky.graph.list";
        db.put_atproto_native_record(&ACTOR, coll, "3kb", "bafyb", b"cbor-b")
            .await
            .unwrap();
        db.put_atproto_native_record(&ACTOR, coll, "3ka", "bafya", b"cbor-a")
            .await
            .unwrap();
        // A different collection must not bleed into the list.
        db.put_atproto_native_record(
            &ACTOR,
            "app.bsky.feed.threadgate",
            "3ka",
            "bafyt",
            b"cbor-t",
        )
        .await
        .unwrap();

        let got = db
            .get_atproto_native_record(&ACTOR, coll, "3kb")
            .await
            .unwrap();
        let got = got.expect("row present");
        assert_eq!(got.cid, "bafyb");
        assert_eq!(got.record, b"cbor-b");
        assert!(got.deleted_at.is_none());

        // Live list is per-collection and ordered by rkey ascending.
        let rows = db.list_atproto_native_records(&ACTOR, coll).await.unwrap();
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].rkey, "3ka");
        assert_eq!(rows[1].rkey, "3kb");
    }

    #[tokio::test]
    async fn native_record_tombstone_hides_from_list_and_second_delete_is_noop() {
        let db = CacheDb::open_in_memory().unwrap();
        let coll = "app.bsky.graph.list";
        db.put_atproto_native_record(&ACTOR, coll, "3k1", "bafy1", b"one")
            .await
            .unwrap();

        // First delete tombstones a live row.
        assert!(
            db.tombstone_atproto_native_record(&ACTOR, coll, "3k1")
                .await
                .unwrap()
        );
        // The row is kept (re-derivability) but marked deleted, and hidden from the live list.
        let got = db
            .get_atproto_native_record(&ACTOR, coll, "3k1")
            .await
            .unwrap();
        assert!(got.expect("row kept").deleted_at.is_some());
        assert!(
            db.list_atproto_native_records(&ACTOR, coll)
                .await
                .unwrap()
                .is_empty()
        );

        // Deleting an already-tombstoned (or unknown) row is a no-op → false.
        assert!(
            !db.tombstone_atproto_native_record(&ACTOR, coll, "3k1")
                .await
                .unwrap()
        );
        assert!(
            !db.tombstone_atproto_native_record(&ACTOR, coll, "nope")
                .await
                .unwrap()
        );
    }

    #[tokio::test]
    async fn native_record_put_revives_tombstone_with_new_content() {
        let db = CacheDb::open_in_memory().unwrap();
        let coll = "app.bsky.graph.list";
        db.put_atproto_native_record(&ACTOR, coll, "3k1", "old-cid", b"old")
            .await
            .unwrap();
        assert!(
            db.tombstone_atproto_native_record(&ACTOR, coll, "3k1")
                .await
                .unwrap()
        );

        // Re-creating the same rkey revives the row with the new cid/record and clears the tombstone.
        db.put_atproto_native_record(&ACTOR, coll, "3k1", "new-cid", b"new")
            .await
            .unwrap();
        let got = db
            .get_atproto_native_record(&ACTOR, coll, "3k1")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(got.cid, "new-cid");
        assert_eq!(got.record, b"new");
        assert!(got.deleted_at.is_none());
        let rows = db.list_atproto_native_records(&ACTOR, coll).await.unwrap();
        assert_eq!(rows.len(), 1);
    }

    // ── atproto_blobs (F2.4 slice 1) ─────────────────────────────────────────

    const BLOB_CID: &str = "bafkreiaha4dqobyha4dqobyha4dqobyha4dqobyha4dqobyha4dqobyha4";

    #[tokio::test]
    async fn blob_round_trip_records_the_media_ref_unreferenced() {
        let db = CacheDb::open_in_memory().unwrap();
        let media_ref = [3u8; 32];
        db.upsert_atproto_blob(&ACTOR, BLOB_CID, &media_ref)
            .await
            .unwrap();

        let got = db
            .get_atproto_blob(&ACTOR, BLOB_CID)
            .await
            .unwrap()
            .expect("row present");
        assert_eq!(got.cid, BLOB_CID);
        assert_eq!(got.media_ref, media_ref);
        // `referenced_at` is stamped when a *record* references the blob — F2.4
        // slice 2's job. An upload alone leaves it NULL, which is exactly what
        // makes the row GC-able as transient upload state (`:191`).
        assert!(got.referenced_at.is_none());
        assert!(got.created_at > 0);
    }

    /// A re-upload of the same bytes must converge on one row rather than
    /// erroring on the `(actor_id, cid)` primary key: the ATProto blob CID is
    /// the sha256 of the bytes, so an app retrying a `uploadBlob` whose reply it
    /// never saw sends the identical CID. R5 (account-data-plane.md § The ratified decisions)'s retry-convergence rests on this.
    #[tokio::test]
    async fn re_uploading_the_same_blob_converges_on_one_row() {
        let db = CacheDb::open_in_memory().unwrap();
        db.upsert_atproto_blob(&ACTOR, BLOB_CID, &[3u8; 32])
            .await
            .unwrap();
        db.upsert_atproto_blob(&ACTOR, BLOB_CID, &[3u8; 32])
            .await
            .unwrap();
        assert_eq!(db.list_atproto_blobs(&ACTOR).await.unwrap().len(), 1);
    }

    /// The primary key is `(actor_id, cid)`, so the same bytes uploaded by two
    /// accounts are two rows. Anything else would let one account's GC or
    /// revocation reach into another's media.
    #[tokio::test]
    async fn the_same_bytes_under_two_accounts_are_two_rows() {
        let db = CacheDb::open_in_memory().unwrap();
        let other: [u8; 32] = [9u8; 32];
        db.upsert_atproto_blob(&ACTOR, BLOB_CID, &[3u8; 32])
            .await
            .unwrap();
        db.upsert_atproto_blob(&other, BLOB_CID, &[3u8; 32])
            .await
            .unwrap();
        assert_eq!(db.list_atproto_blobs(&ACTOR).await.unwrap().len(), 1);
        assert_eq!(db.list_atproto_blobs(&other).await.unwrap().len(), 1);
    }

    #[tokio::test]
    async fn an_unknown_blob_cid_reads_as_absent() {
        let db = CacheDb::open_in_memory().unwrap();
        assert!(
            db.get_atproto_blob(&ACTOR, BLOB_CID)
                .await
                .unwrap()
                .is_none()
        );
    }

    /// The stamp is a first-reference-wins high-water mark: a second stamp
    /// keeps the first instant, and a re-upload never clears it (the upsert
    /// leaves `referenced_at` alone by design).
    #[tokio::test]
    async fn the_reference_stamp_is_first_wins_and_survives_re_upload() {
        let db = CacheDb::open_in_memory().unwrap();
        db.upsert_atproto_blob(&ACTOR, BLOB_CID, &[3u8; 32])
            .await
            .unwrap();

        db.stamp_atproto_blob_referenced(&ACTOR, BLOB_CID)
            .await
            .unwrap();
        let first = db
            .get_atproto_blob(&ACTOR, BLOB_CID)
            .await
            .unwrap()
            .unwrap()
            .referenced_at
            .expect("stamped");

        db.stamp_atproto_blob_referenced(&ACTOR, BLOB_CID)
            .await
            .unwrap();
        db.upsert_atproto_blob(&ACTOR, BLOB_CID, &[3u8; 32])
            .await
            .unwrap();
        assert_eq!(
            db.get_atproto_blob(&ACTOR, BLOB_CID)
                .await
                .unwrap()
                .unwrap()
                .referenced_at,
            Some(first),
            "a re-stamp or re-upload must not move the first-reference mark"
        );
    }

    /// Stamping a CID this account never uploaded is a no-op, not an error —
    /// the write path stamps every ref a record carries, and refs outside the
    /// upload ledger (a profile's projection-stored picture) are not its rows.
    #[tokio::test]
    async fn stamping_an_unknown_cid_is_a_no_op() {
        let db = CacheDb::open_in_memory().unwrap();
        db.stamp_atproto_blob_referenced(&ACTOR, BLOB_CID)
            .await
            .unwrap();
        assert!(
            db.get_atproto_blob(&ACTOR, BLOB_CID)
                .await
                .unwrap()
                .is_none()
        );
    }

    // ── the reference-window sweep (F2.4 slice 4) ────────────────────────────

    /// An upload no record ever named, past the window, is collectable —
    /// `atproto-pds-full.md` § Nest state schema's sanctioned row set.
    #[tokio::test]
    async fn the_sweep_retires_an_unreferenced_row_past_the_window() {
        let db = CacheDb::open_in_memory().unwrap();
        db.upsert_atproto_blob(&ACTOR, BLOB_CID, &[3u8; 32])
            .await
            .unwrap();

        let swept = db
            .delete_unreferenced_atproto_blobs(now_epoch_millis() + 1)
            .await
            .unwrap();

        assert_eq!(swept, 1);
        assert!(
            db.get_atproto_blob(&ACTOR, BLOB_CID)
                .await
                .unwrap()
                .is_none()
        );
    }

    /// **The negative that matters.** A stamped row is user data — a record on
    /// the network names those bytes — so no cutoff, however distant, may take
    /// it. This is the one assertion standing between the sweep and deleting
    /// media a committed repo record still references.
    #[tokio::test]
    async fn the_sweep_never_takes_a_stamped_row_however_old() {
        let db = CacheDb::open_in_memory().unwrap();
        db.upsert_atproto_blob(&ACTOR, BLOB_CID, &[3u8; 32])
            .await
            .unwrap();
        db.stamp_atproto_blob_referenced(&ACTOR, BLOB_CID)
            .await
            .unwrap();

        // A cutoff far past any conceivable window — a century of sweeps.
        let swept = db
            .delete_unreferenced_atproto_blobs(now_epoch_millis() + 100 * 365 * 86_400_000)
            .await
            .unwrap();

        assert_eq!(swept, 0, "a referenced blob was swept");
        assert!(
            db.get_atproto_blob(&ACTOR, BLOB_CID)
                .await
                .unwrap()
                .is_some(),
            "the row a record references must outlive every sweep"
        );
    }

    /// Inside the window an unreferenced row stays: it is an upload whose
    /// record has not arrived yet, which is the ordinary two-step shape of
    /// `uploadBlob` followed by `createRecord`.
    #[tokio::test]
    async fn the_sweep_leaves_an_unreferenced_row_inside_the_window() {
        let db = CacheDb::open_in_memory().unwrap();
        db.upsert_atproto_blob(&ACTOR, BLOB_CID, &[3u8; 32])
            .await
            .unwrap();

        let cutoff =
            now_epoch_millis() - crate::atproto_blob_sweeper::ATPROTO_BLOB_REFERENCE_WINDOW_MILLIS;
        let swept = db.delete_unreferenced_atproto_blobs(cutoff).await.unwrap();

        assert_eq!(swept, 0);
        assert!(
            db.get_atproto_blob(&ACTOR, BLOB_CID)
                .await
                .unwrap()
                .is_some()
        );
    }

    /// The sweep is cross-actor — a periodic pass does not know the actor set,
    /// which is the whole reason `list_atproto_blobs` could never have fed it.
    #[tokio::test]
    async fn the_sweep_spans_accounts_in_one_statement() {
        let db = CacheDb::open_in_memory().unwrap();
        let other: [u8; 32] = [9u8; 32];
        db.upsert_atproto_blob(&ACTOR, BLOB_CID, &[3u8; 32])
            .await
            .unwrap();
        db.upsert_atproto_blob(&other, BLOB_CID, &[3u8; 32])
            .await
            .unwrap();

        let swept = db
            .delete_unreferenced_atproto_blobs(now_epoch_millis() + 1)
            .await
            .unwrap();

        assert_eq!(swept, 2);
    }

    /// The reachability feed is unfiltered by design: an unreferenced row still
    /// pins its bytes, because the window is the sweep's decision and the bytes
    /// only ever become collectable once the ROW is gone.
    #[tokio::test]
    async fn every_row_feeds_reachability_stamped_or_not() {
        let db = CacheDb::open_in_memory().unwrap();
        let other: [u8; 32] = [9u8; 32];
        db.upsert_atproto_blob(&ACTOR, BLOB_CID, &[3u8; 32])
            .await
            .unwrap();
        db.upsert_atproto_blob(&other, "bafkreiother", &[4u8; 32])
            .await
            .unwrap();
        db.stamp_atproto_blob_referenced(&other, "bafkreiother")
            .await
            .unwrap();

        let mut refs = db.all_atproto_blob_media_refs().await.unwrap();
        refs.sort();
        assert_eq!(refs, vec![vec![3u8; 32], vec![4u8; 32]]);

        // …and after the sweep takes the unreferenced one, only the live blob
        // is still pinned. This is the handoff: the row going is what makes the
        // bytes collectable to the box-wide GC, never the sweep itself.
        db.delete_unreferenced_atproto_blobs(now_epoch_millis() + 1)
            .await
            .unwrap();
        assert_eq!(
            db.all_atproto_blob_media_refs().await.unwrap(),
            vec![vec![4u8; 32]]
        );
    }

    // ── Consent requests (F4 slice 6a) ───────────────────────────────────────

    const TTL: i64 = 10 * 60 * 1000;

    async fn open(db: &CacheDb, actor: Option<[u8; 32]>) -> ConsentRequestRow {
        db.open_atproto_consent_request(
            actor,
            "https://app.example/client-metadata.json",
            Some("Example App"),
            "atproto repo:app.bsky.feed.post",
            &[],
            TTL,
        )
        .await
        .unwrap()
    }

    const CLIENT: &str = "https://app.example/client-metadata.json";

    async fn open_as(
        db: &CacheDb,
        start: ConsentStartKind,
        actor: Option<[u8; 32]>,
        client_id: &str,
    ) -> ConsentRequestRow {
        db.open_atproto_consent_request_for(
            start,
            actor,
            client_id,
            None,
            "atproto",
            &[],
            &ConsentBinding::default(),
            TTL,
        )
        .await
        .unwrap()
    }

    /// The typed code's whole visibility rule: an unclaimed typed-code row is
    /// on nobody's list and answerable by nobody — not even by an account that
    /// learned its consent id — until an account types its code, after which
    /// it is that account's ordinary pending row and nobody else's.
    #[tokio::test]
    async fn a_typed_code_row_is_hidden_until_typed_and_then_belongs_to_the_typist() {
        let db = CacheDb::open_in_memory().unwrap();
        let other = [9u8; 32];
        let row = open_as(&db, ConsentStartKind::TypedCode, None, CLIENT).await;
        assert_eq!(row.start, ConsentStartKind::TypedCode);
        assert_eq!(
            fauna_core::human_code::normalize(&row.code).len(),
            TYPED_CODE_LEN,
            "a typed code is the long form: {}",
            row.code
        );

        assert!(
            db.list_pending_atproto_consent_requests(&ACTOR)
                .await
                .unwrap()
                .is_empty()
        );
        assert!(
            db.resolve_atproto_consent_request(&row.consent_id, &ACTOR, true)
                .await
                .unwrap()
                .is_none(),
            "an unclaimed typed-code row must not be resolvable by id"
        );

        // Typed as a person types: lower case, the hyphen dropped, a space.
        let typed = format!(" {} ", row.code.replace('-', "").to_lowercase());
        let claimed = db.claim_typed_consent_code(&typed, &ACTOR).await.unwrap();
        assert_eq!(
            claimed.as_ref().map(|r| r.consent_id.clone()),
            Some(row.consent_id.clone())
        );
        assert_eq!(
            db.claim_typed_consent_code(&row.code, &ACTOR)
                .await
                .unwrap()
                .map(|r| r.consent_id),
            Some(row.consent_id.clone()),
            "the typist typing again is the same claim"
        );
        assert!(
            db.claim_typed_consent_code(&row.code, &other)
                .await
                .unwrap()
                .is_none(),
            "a code another account already claimed is a miss"
        );
        assert!(
            db.claim_typed_consent_code("WRONG-CODE", &ACTOR)
                .await
                .unwrap()
                .is_none()
        );

        let listed = db
            .list_pending_atproto_consent_requests(&ACTOR)
            .await
            .unwrap();
        assert_eq!(listed.len(), 1);
        assert!(
            db.list_pending_atproto_consent_requests(&other)
                .await
                .unwrap()
                .is_empty()
        );
        assert!(
            db.resolve_atproto_consent_request(&row.consent_id, &ACTOR, true)
                .await
                .unwrap()
                .is_some()
        );
    }

    /// Rule (b): a quiet push replaces the same client's live push to the same
    /// account — and only that. Another client's push, and a browser request
    /// from the same client, are untouched.
    #[tokio::test]
    async fn a_quiet_push_replaces_only_the_same_clients_earlier_push() {
        let db = CacheDb::open_in_memory().unwrap();
        let browser = open_as(&db, ConsentStartKind::Browser, Some(ACTOR), CLIENT).await;
        let first = open_as(&db, ConsentStartKind::Push, Some(ACTOR), CLIENT).await;
        let elsewhere = open_as(&db, ConsentStartKind::Push, Some(ACTOR), "http://localhost").await;
        let second = open_as(&db, ConsentStartKind::Push, Some(ACTOR), CLIENT).await;

        let ids: Vec<Vec<u8>> = db
            .list_pending_atproto_consent_requests(&ACTOR)
            .await
            .unwrap()
            .into_iter()
            .map(|r| r.consent_id)
            .collect();
        assert!(
            !ids.contains(&first.consent_id),
            "the repeat replaced the first push"
        );
        for kept in [&browser, &elsewhere, &second] {
            assert!(
                ids.contains(&kept.consent_id),
                "{:?} must survive",
                kept.start
            );
        }
        assert!(
            db.get_atproto_consent_request(&first.consent_id)
                .await
                .unwrap()
                .is_none()
        );
    }

    /// A push row always names an account — the storage layer refuses an
    /// unassigned one, which would be listed to every account.
    #[tokio::test]
    async fn a_quiet_push_row_cannot_be_unassigned() {
        let db = CacheDb::open_in_memory().unwrap();
        assert!(
            db.open_atproto_consent_request_for(
                ConsentStartKind::Push,
                None,
                CLIENT,
                None,
                "atproto",
                &[],
                &ConsentBinding::default(),
                TTL
            )
            .await
            .is_err()
        );
    }

    /// Buckets are per start: a flood of quiet pushes at the account's ceiling
    /// evicts only pushes, never the browser request the user is mid-way
    /// through.
    #[tokio::test]
    async fn a_push_flood_cannot_evict_a_browser_request() {
        let db = CacheDb::open_in_memory().unwrap();
        let browser = open_as(&db, ConsentStartKind::Browser, Some(ACTOR), CLIENT).await;
        for i in 0..(MAX_PENDING_CONSENTS_PER_BUCKET + 3) {
            open_as(
                &db,
                ConsentStartKind::Push,
                Some(ACTOR),
                &format!("https://c{i}.example/m.json"),
            )
            .await;
        }
        let listed = db
            .list_pending_atproto_consent_requests(&ACTOR)
            .await
            .unwrap();
        assert!(listed.iter().any(|r| r.consent_id == browser.consent_id));
        assert_eq!(
            listed
                .iter()
                .filter(|r| r.start == ConsentStartKind::Push)
                .count(),
            MAX_PENDING_CONSENTS_PER_BUCKET
        );
    }

    /// The handoff's bucket is its own (`authorization-server.md` § Consent →
    /// *How the same-device handoff is built*): a flood through the route
    /// evicts only handoff rows, never the account's browser or push rows; the
    /// row round-trips as `Handoff`, and it can never be unassigned.
    #[tokio::test]
    async fn a_handoff_flood_evicts_only_handoff_rows() {
        let db = CacheDb::open_in_memory().unwrap();
        let browser = open_as(&db, ConsentStartKind::Browser, Some(ACTOR), CLIENT).await;
        let push = open_as(&db, ConsentStartKind::Push, Some(ACTOR), CLIENT).await;
        for i in 0..(MAX_PENDING_CONSENTS_PER_BUCKET + 3) {
            let row = open_as(
                &db,
                ConsentStartKind::Handoff,
                Some(ACTOR),
                &format!("https://h{i}.example/m.json"),
            )
            .await;
            assert_eq!(row.start, ConsentStartKind::Handoff);
        }
        let listed = db
            .list_pending_atproto_consent_requests(&ACTOR)
            .await
            .unwrap();
        assert!(listed.iter().any(|r| r.consent_id == browser.consent_id));
        assert!(listed.iter().any(|r| r.consent_id == push.consent_id));
        assert_eq!(
            listed
                .iter()
                .filter(|r| r.start == ConsentStartKind::Handoff)
                .count(),
            MAX_PENDING_CONSENTS_PER_BUCKET
        );
        assert!(
            db.open_atproto_consent_request_for(
                ConsentStartKind::Handoff,
                None,
                CLIENT,
                None,
                "atproto",
                &[],
                &ConsentBinding::default(),
                TTL
            )
            .await
            .is_err(),
            "a handoff row is the opening account's by definition"
        );
    }

    /// Rule (a)'s reading: any grant row counts as a prior approval, and the
    /// key form counts only a grant bound to that key.
    #[tokio::test]
    async fn a_prior_approval_is_read_by_client_and_optionally_by_key() {
        let db = CacheDb::open_in_memory().unwrap();
        assert!(
            !db.atproto_oauth_client_approved_before(&ACTOR, CLIENT, None)
                .await
                .unwrap()
        );
        db.record_atproto_oauth_grant(
            &ACTOR,
            &[1u8; 16],
            CLIENT,
            None,
            "atproto",
            &[],
            "jkt-approved",
            now_epoch_millis() + 60_000,
            None,
            OAUTH_GRANT_ISSUER_NEST,
            &crate::db::third_party_principals::UNATTESTED_DEVICE,
        )
        .await
        .unwrap();
        assert!(
            db.atproto_oauth_client_approved_before(&ACTOR, CLIENT, None)
                .await
                .unwrap()
        );
        assert!(
            db.atproto_oauth_client_approved_before(&ACTOR, CLIENT, Some("jkt-approved"))
                .await
                .unwrap()
        );
        assert!(
            !db.atproto_oauth_client_approved_before(&ACTOR, CLIENT, Some("jkt-impostor"))
                .await
                .unwrap(),
            "a public client's name without its approved key is not a prior approval"
        );
        assert!(
            !db.atproto_oauth_client_approved_before(&[9u8; 32], CLIENT, None)
                .await
                .unwrap(),
            "another account's approval is not this one's"
        );
    }

    /// Rule (c)'s block: set, read, listed, lifted — idempotently, and per
    /// account.
    #[tokio::test]
    async fn a_client_block_is_per_account_and_idempotent_both_ways() {
        let db = CacheDb::open_in_memory().unwrap();
        for _ in 0..2 {
            assert!(
                db.set_oauth_client_block(&ACTOR, CLIENT, true)
                    .await
                    .unwrap()
            );
        }
        assert!(db.oauth_client_blocked(&ACTOR, CLIENT).await.unwrap());
        assert!(!db.oauth_client_blocked(&[9u8; 32], CLIENT).await.unwrap());
        let listed = db.list_oauth_client_blocks(&ACTOR).await.unwrap();
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].0, CLIENT);
        for _ in 0..2 {
            assert!(
                !db.set_oauth_client_block(&ACTOR, CLIENT, false)
                    .await
                    .unwrap()
            );
        }
        assert!(!db.oauth_client_blocked(&ACTOR, CLIENT).await.unwrap());
        assert!(db.set_oauth_client_block(&ACTOR, "", true).await.is_err());
    }

    /// The happy path, and the two facts the ceremony rests on: the row carries
    /// a code the caller did not choose, and it is pending until answered.
    #[tokio::test]
    async fn an_opened_request_is_pending_and_carries_a_minted_code() {
        let db = CacheDb::open_in_memory().unwrap();
        let row = open(&db, Some(ACTOR)).await;
        assert_eq!(row.consent_id.len(), CONSENT_ID_LEN);
        assert_eq!(
            fauna_core::human_code::normalize(&row.code).len(),
            CONSENT_CODE_LEN
        );
        assert!(
            row.code.contains('-'),
            "display form is grouped: {}",
            row.code
        );
        assert_eq!(row.resolved_at, None);
        assert_eq!(row.approved, None);
        assert_eq!(row.expires_at, row.created_at + TTL);

        let pending = db
            .list_pending_atproto_consent_requests(&ACTOR)
            .await
            .unwrap();
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].code, row.code);
    }

    /// ⚠ The uniqueness mechanism itself, driven by a generator that
    /// deliberately collides — the pin the journey-level check below **cannot**
    /// be (a natural collision at 30 bits is a one-in-ten-million event, so
    /// deleting the retry leaves that one green). Deleting the `taken` check
    /// reddens exactly this.
    #[test]
    fn a_colliding_code_is_reminted_until_it_is_free() {
        let taken_codes = ["AAA-AAA", "BBB-BBB"];
        let mut handed_out = 0usize;
        let candidates = ["AAA-AAA", "BBB-BBB", "CCC-CCC"];
        let code = mint_unique_consent_code(
            || {
                let c = candidates[handed_out.min(candidates.len() - 1)].to_string();
                handed_out += 1;
                c
            },
            |candidate| Ok(taken_codes.contains(&candidate)),
        )
        .expect("the third candidate is free");
        assert_eq!(code, "CCC-CCC");

        // …and a generator that can only ever collide fails LOUDLY rather than
        // handing back a duplicate.
        assert!(
            mint_unique_consent_code(|| "AAA-AAA".to_string(), |_| Ok(true)).is_err(),
            "exhausting the budget must be an error, never a duplicate code"
        );
    }

    /// The journey-level sanity check: filling both buckets produces distinct
    /// codes. Honest about what it is — a smoke test over the real generator,
    /// **not** the uniqueness pin (see
    /// [`a_colliding_code_is_reminted_until_it_is_free`], which is).
    #[tokio::test]
    async fn live_requests_never_share_a_code() {
        let db = CacheDb::open_in_memory().unwrap();
        let mut codes = std::collections::HashSet::new();
        // Fill both buckets to their ceiling — every row alive at once.
        for _ in 0..MAX_PENDING_CONSENTS_PER_BUCKET {
            assert!(codes.insert(open(&db, Some(ACTOR)).await.code));
            assert!(codes.insert(open(&db, None).await.code));
        }
        assert_eq!(codes.len(), MAX_PENDING_CONSENTS_PER_BUCKET * 2);
    }

    /// An unassigned request (no `login_hint`) is visible to an account that
    /// did not open it — that is § F4 detail's "the pending consent is visible
    /// in-app" — and resolving it BINDS it to whoever answered.
    #[tokio::test]
    async fn an_unassigned_request_is_claimed_by_whoever_answers_it() {
        let db = CacheDb::open_in_memory().unwrap();
        let row = open(&db, None).await;
        assert_eq!(row.actor_id, None);

        let other: [u8; 32] = [9u8; 32];
        assert_eq!(
            db.list_pending_atproto_consent_requests(&other)
                .await
                .unwrap()
                .len(),
            1,
            "an unassigned request is answerable by any account"
        );

        let resolved = db
            .resolve_atproto_consent_request(&row.consent_id, &other, true)
            .await
            .unwrap()
            .expect("claimable");
        assert_eq!(resolved.actor_id.as_deref(), Some(&other[..]));
        assert_eq!(resolved.approved, Some(true));
        assert!(resolved.resolved_at.is_some());
    }

    /// ⚠ Authorization lives in the UPDATE's `WHERE`, not in a pre-check: a
    /// request already belonging to one account is not answerable by another,
    /// and the row is left exactly as it was.
    #[tokio::test]
    async fn another_account_cannot_answer_an_assigned_request() {
        let db = CacheDb::open_in_memory().unwrap();
        let row = open(&db, Some(ACTOR)).await;
        let other: [u8; 32] = [9u8; 32];

        assert!(
            db.resolve_atproto_consent_request(&row.consent_id, &other, true)
                .await
                .unwrap()
                .is_none()
        );
        let after = db
            .get_atproto_consent_request(&row.consent_id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(after.resolved_at, None, "the row must be untouched");
        assert_eq!(after.actor_id.as_deref(), Some(&ACTOR[..]));

        // …and it is not even listed to them.
        assert!(
            db.list_pending_atproto_consent_requests(&other)
                .await
                .unwrap()
                .is_empty()
        );
    }

    /// ⚠ An answer is final. A second `resolve` — a retry, another device, or a
    /// replay — matches nothing, so an approval can never become a second grant
    /// and a decline can never be flipped into one.
    #[tokio::test]
    async fn an_answered_request_cannot_be_answered_again() {
        let db = CacheDb::open_in_memory().unwrap();
        let denied = open(&db, Some(ACTOR)).await;
        assert!(
            db.resolve_atproto_consent_request(&denied.consent_id, &ACTOR, false)
                .await
                .unwrap()
                .is_some()
        );
        assert!(
            db.resolve_atproto_consent_request(&denied.consent_id, &ACTOR, true)
                .await
                .unwrap()
                .is_none(),
            "a decline must not be flippable into an approval"
        );
        let after = db
            .get_atproto_consent_request(&denied.consent_id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(after.approved, Some(false));

        // Resolved rows leave the pending list, so no card lingers.
        assert!(
            db.list_pending_atproto_consent_requests(&ACTOR)
                .await
                .unwrap()
                .is_empty()
        );
    }

    /// Expiry is enforced on the answer as well as the list: a card whose
    /// window has passed cannot be approved into a grant.
    #[tokio::test]
    async fn an_expired_request_is_neither_listed_nor_answerable() {
        let db = CacheDb::open_in_memory().unwrap();
        // A TTL already in the past — the deterministic way to age a row
        // without a clock (testing.md convention 14: no sleeps).
        let row = db
            .open_atproto_consent_request(
                Some(ACTOR),
                "https://app.example/c",
                None,
                "atproto",
                &[],
                -1,
            )
            .await
            .unwrap();
        assert!(
            db.list_pending_atproto_consent_requests(&ACTOR)
                .await
                .unwrap()
                .is_empty()
        );
        assert!(
            db.resolve_atproto_consent_request(&row.consent_id, &ACTOR, true)
                .await
                .unwrap()
                .is_none()
        );
        // The bridge's own read still SEES it — that is deliberate, so the poll
        // can answer "expired" rather than confusing it with an unknown id.
        // (Both answer `expired` at the handler; the distinction stays here.)
        assert!(
            db.get_atproto_consent_request(&row.consent_id)
                .await
                .unwrap()
                .is_some()
        );
    }

    /// The flood bound. Overflow evicts nearest-to-expiry — i.e. the oldest,
    /// since one TTL is shared — so the request the user is actually mid-flow
    /// in (the newest) is the one that survives.
    #[tokio::test]
    async fn a_bucket_is_bounded_and_evicts_the_oldest_first() {
        let db = CacheDb::open_in_memory().unwrap();
        let mut opened = Vec::new();
        for _ in 0..MAX_PENDING_CONSENTS_PER_BUCKET + 3 {
            opened.push(open(&db, Some(ACTOR)).await);
        }
        let pending = db
            .list_pending_atproto_consent_requests(&ACTOR)
            .await
            .unwrap();
        assert_eq!(pending.len(), MAX_PENDING_CONSENTS_PER_BUCKET);
        let live: std::collections::HashSet<_> =
            pending.iter().map(|r| r.consent_id.clone()).collect();
        assert!(
            live.contains(&opened.last().unwrap().consent_id),
            "the newest request must survive a flood"
        );
        assert!(
            !live.contains(&opened[0].consent_id),
            "the oldest must be the one evicted"
        );
    }

    /// The two buckets are separate: an unassigned flood cannot evict an
    /// account's own pending card, and vice versa.
    #[tokio::test]
    async fn the_unassigned_pool_has_its_own_ceiling() {
        let db = CacheDb::open_in_memory().unwrap();
        let mine = open(&db, Some(ACTOR)).await;
        for _ in 0..MAX_PENDING_CONSENTS_PER_BUCKET + 3 {
            open(&db, None).await;
        }
        let pending = db
            .list_pending_atproto_consent_requests(&ACTOR)
            .await
            .unwrap();
        assert_eq!(pending.len(), MAX_PENDING_CONSENTS_PER_BUCKET + 1);
        assert!(pending.iter().any(|r| r.consent_id == mine.consent_id));
    }

    #[tokio::test]
    async fn oversized_fields_are_refused() {
        let db = CacheDb::open_in_memory().unwrap();
        let huge = "x".repeat(MAX_CONSENT_FIELD_LEN + 1);
        assert!(
            db.open_atproto_consent_request(Some(ACTOR), &huge, None, "atproto", &[], TTL)
                .await
                .is_err()
        );
        assert!(
            db.open_atproto_consent_request(
                Some(ACTOR),
                "https://a/b",
                Some(&huge),
                "atproto",
                &[],
                TTL
            )
            .await
            .is_err()
        );
        let huge_scopes = "x".repeat(MAX_CONSENT_SCOPES_LEN + 1);
        assert!(
            db.open_atproto_consent_request(
                Some(ACTOR),
                "https://a/b",
                None,
                &huge_scopes,
                &[],
                TTL
            )
            .await
            .is_err()
        );
    }

    /// A permission-set grant at the bridge's own ceiling must survive the
    /// ceremony. `MAX_EXPANDED_SCOPE_BYTES_PER_GRANT` is measured as exactly
    /// this string — the space-joined rendered form — so a narrower cap here
    /// makes the bridge's published ceiling a lie: PAR accepts, hands the
    /// client a `request_uri`, and `/oauth/authorize` then dies on the nest's
    /// refusal, mid-ceremony and after the client believes it has a live
    /// request. The two caps are one number.
    #[tokio::test]
    async fn a_max_size_grant_expansion_opens_a_consent_request() {
        let db = CacheDb::open_in_memory().unwrap();
        let at_cap = "x".repeat(MAX_CONSENT_SCOPES_LEN);
        db.open_atproto_consent_request(Some(ACTOR), "https://a/b", None, &at_cap, &[], TTL)
            .await
            .expect("a grant at the bridge's expansion ceiling must open");
        let over_cap = "x".repeat(MAX_CONSENT_SCOPES_LEN + 1);
        assert!(
            db.open_atproto_consent_request(Some(ACTOR), "https://a/b", None, &over_cap, &[], TTL)
                .await
                .is_err(),
            "past the ceiling it is still refused"
        );
    }

    /// The frozen expansion survives storage **whole** — including the raw,
    /// attacker-authored `title`/`details`, because the control-strip fence is
    /// the shared machine's composition pass and stripping here would give the
    /// six apps that have yet to paint this card a second, quieter place to
    /// disagree with tui about what the user saw.
    #[tokio::test]
    async fn the_frozen_expansion_round_trips_through_storage() {
        let db = CacheDb::open_in_memory().unwrap();
        let sets = vec![ConsentSetInfo {
            nsid: "com.example.calendar.appPerms".into(),
            title: Some("Calendar\nsync".into()),
            details: None,
            members: vec![
                "repo:com.example.calendar.event".into(),
                "rpc:com.example.calendar.sync?aud=did%3Aweb%3Acal.example".into(),
            ],
            extra: Default::default(),
        }];
        let opened = db
            .open_atproto_consent_request(
                Some(ACTOR),
                "https://a/b",
                None,
                "atproto repo:com.example.calendar.event",
                &sets,
                TTL,
            )
            .await
            .unwrap();
        assert_eq!(opened.sets, sets);

        let read_back = db
            .get_atproto_consent_request(&opened.consent_id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(read_back.sets, sets, "and it survives the read path");
        assert_eq!(
            read_back.sets[0].title.as_deref(),
            Some("Calendar\nsync"),
            "raw — the fence is the machine's, not this layer's"
        );

        let listed = db
            .list_pending_atproto_consent_requests(&ACTOR)
            .await
            .unwrap();
        assert_eq!(
            listed.iter().find(|r| r.consent_id == opened.consent_id),
            Some(&read_back),
            "and the list projection carries it too"
        );
    }

    /// A request that names no set stores NULL, not `"[]"` — and reads back
    /// as an empty set.
    #[tokio::test]
    async fn no_sets_stores_null_and_reads_back_empty() {
        let db = CacheDb::open_in_memory().unwrap();
        let opened = open(&db, Some(ACTOR)).await;
        assert!(opened.sets.is_empty());
        assert!(
            db.get_atproto_consent_request(&opened.consent_id)
                .await
                .unwrap()
                .unwrap()
                .sets
                .is_empty()
        );
    }

    /// The card's binding — the ceremony's two attested keys and the manifest
    /// JWS — is stored with the row and read back by every reader the card is
    /// rendered from (`third-party-kinds.md` § The record doors), and a row
    /// opened with none reads back as "attested nothing". Mutation: drop a
    /// column from [`CONSENT_REQUEST_COLUMNS`] or the insert → this reds.
    #[tokio::test]
    async fn the_consent_binding_round_trips_through_every_reader() {
        let db = CacheDb::open_in_memory().unwrap();
        let binding = ConsentBinding {
            attested: crate::db::third_party_principals::AttestedKeys {
                holder_x25519: Some([0x11; 32]),
                writer_ed25519: Some([0x22; 32]),
            },
            fauna_manifest: Some("eyJh.eyJi.c2ln".into()),
        };
        let opened = db
            .open_atproto_consent_request_for(
                ConsentStartKind::Browser,
                Some(ACTOR),
                "https://app.example/client-metadata.json",
                None,
                "fauna:records:rw:ext.app.example.*",
                &[],
                &binding,
                TTL,
            )
            .await
            .unwrap();
        assert_eq!(opened.binding, binding);
        let got = db
            .get_atproto_consent_request(&opened.consent_id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(got.binding, binding, "the get reader");
        let listed = db
            .list_pending_atproto_consent_requests(&ACTOR)
            .await
            .unwrap();
        assert_eq!(
            listed[0].binding, binding,
            "the pending list the card renders"
        );
        let resolved = db
            .resolve_atproto_consent_request(&opened.consent_id, &ACTOR, true)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(resolved.binding, binding, "the resolve read-back");

        let bare = open(&db, Some(ACTOR)).await;
        let bare = db
            .get_atproto_consent_request(&bare.consent_id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(bare.binding, ConsentBinding::default());
    }

    /// **The unredeemed-grant sweep** (`third-party-kinds.md` § The record
    /// doors): when an approved request expires, the capability grants its
    /// owner deposited to the ceremony's attested holder end — unless some
    /// principal row of the account names that holder (the ceremony was
    /// redeemed). A grant to any other holder, and a declined request's
    /// holder, are untouched. Mutation: drop the `capability_grants` DELETE
    /// from `sweep_expired_consents` → this reds; drop its `NOT EXISTS` → the
    /// redeemed holder's grant goes too and this reds.
    #[tokio::test]
    async fn an_expired_unredeemed_approval_ends_the_grant_it_left_behind() {
        let db = CacheDb::open_in_memory().unwrap();
        let (orphan, redeemed, unrelated) = ([0xA1; 32], [0xA2; 32], [0xA3; 32]);
        for holder in [orphan, redeemed] {
            let row = db
                .open_atproto_consent_request_for(
                    ConsentStartKind::Browser,
                    Some(ACTOR),
                    "https://app.example/client-metadata.json",
                    None,
                    "fauna:records:rw:ext.app.example.*",
                    &[],
                    &ConsentBinding {
                        attested: crate::db::third_party_principals::AttestedKeys {
                            holder_x25519: Some(holder),
                            writer_ed25519: None,
                        },
                        fauna_manifest: None,
                    },
                    TTL,
                )
                .await
                .unwrap();
            db.resolve_atproto_consent_request(&row.consent_id, &ACTOR, true)
                .await
                .unwrap()
                .unwrap();
        }
        {
            let conn = db.conn.lock().await;
            for (i, holder) in [orphan, redeemed, unrelated].iter().enumerate() {
                conn.execute(
                    "INSERT INTO capability_grants
                        (owner_actor_id, grant_id, holder_pubkey, blob, epoch_end, created_at)
                     VALUES (?1, ?2, ?3, x'00', 0, 0)",
                    rusqlite::params![&ACTOR[..], &[i as u8; 16][..], &holder[..]],
                )
                .unwrap();
            }
            conn.execute(
                "INSERT INTO third_party_principals
                    (actor_id, principal_id, client_id, holder_x25519, execution_form,
                     granted_scopes, created_at)
                 VALUES (?1, x'01', 'https://app.example/client-metadata.json', ?2,
                         'remote', '', 0)",
                rusqlite::params![&ACTOR[..], &redeemed[..]],
            )
            .unwrap();
            conn.execute("UPDATE atproto_consent_requests SET expires_at = 0", [])
                .unwrap();
        }
        assert_eq!(
            db.sweep_expired_atproto_consent_requests().await.unwrap(),
            2
        );
        let conn = db.conn.lock().await;
        let mut stmt = conn
            .prepare("SELECT holder_pubkey FROM capability_grants ORDER BY holder_pubkey")
            .unwrap();
        let left: Vec<Vec<u8>> = stmt
            .query_map([], |r| r.get(0))
            .unwrap()
            .collect::<std::result::Result<_, _>>()
            .unwrap();
        assert_eq!(left, vec![redeemed.to_vec(), unrelated.to_vec()]);
    }

    /// A manifest past the document cap is refused — the table bounds what it
    /// stores without trusting a caller to have resolved it first.
    #[tokio::test]
    async fn an_oversized_manifest_is_refused() {
        let db = CacheDb::open_in_memory().unwrap();
        let binding = ConsentBinding {
            fauna_manifest: Some("m".repeat(MAX_CONSENT_MANIFEST_LEN + 1)),
            ..Default::default()
        };
        assert!(
            db.open_atproto_consent_request_for(
                ConsentStartKind::Browser,
                Some(ACTOR),
                "https://a/b",
                None,
                "atproto",
                &[],
                &binding,
                TTL,
            )
            .await
            .is_err()
        );
    }

    /// The set payload has its own ceiling, and it is the *author's* strings
    /// that make it necessary: the scope grammar bounds members, nothing bounds
    /// a title.
    #[tokio::test]
    async fn an_oversized_set_payload_is_refused() {
        let db = CacheDb::open_in_memory().unwrap();
        let sets = vec![ConsentSetInfo {
            nsid: "com.example.appPerms".into(),
            title: Some("t".repeat(MAX_CONSENT_SETS_LEN)),
            details: None,
            members: vec!["repo:com.example.event".into()],
            extra: Default::default(),
        }];
        assert!(
            db.open_atproto_consent_request(
                Some(ACTOR),
                "https://a/b",
                None,
                "atproto",
                &sets,
                TTL
            )
            .await
            .is_err()
        );
    }

    /// **The composition half.** The test above proves the nest's
    /// ceiling exists; it says nothing about whether the ceiling is *reachable
    /// from a request PAR accepted*, and that gap is the whole finding. Here
    /// the hostile document goes through the **real bridge expander** and the
    /// **real grant caps PAR enforces**, and only then reaches the nest — so a
    /// bridge that accepts what the nest refuses reddens this and nothing else.
    ///
    /// The `ExpandedSet` → [`ConsentSetInfo`] mapping in the middle is Go's
    /// carry (`permset_wiring.go`), graded clean and modelled here
    /// field-for-field rather than built up from literals.
    ///
    /// `atproto-pds-full.md:329`: an oversized request must fail "at the
    /// developer's desk, never mid-session".
    #[tokio::test]
    async fn a_par_the_bridge_accepts_is_a_consent_row_the_nest_stores() {
        use fauna_bridge_atproto::permission_set::{
            Expansion, GrantExpansion, ParsedInclude, check_grant_expansion, expand_permission_set,
        };

        let db = CacheDb::open_in_memory().unwrap();

        // A published set whose author wrote more title than this nest will
        // ever store. Nothing upstream of the expander bounds it: the record
        // arrives through the 1 MiB proof fetch and the scope grammar never
        // looks at `title`.
        //
        // The size is taken **from the ceiling** rather than from the finding's
        // 60,000-byte probe. That probe was sized against the *old* ceiling of
        // 40,960, so transcribing it would leave this pin quietly toothless the
        // moment the ceiling moved past it — which is exactly what happened on
        // the first grading run of this fix.
        let doc: ipld_core::ipld::Ipld = serde_json::from_value(serde_json::json!({
            "lexicon": 1,
            "id": "com.example.calendar.appPerms",
            "defs": {"main": {
                "type": "permission-set",
                "title": "T".repeat(MAX_CONSENT_SETS_LEN + 1),
                "details": "Read and write your calendar events.",
                "permissions": [
                    {"type": "permission", "resource": "repo",
                     "collection": ["com.example.calendar.event"]},
                ],
            }},
        }))
        .expect("fixture is valid IPLD");
        let record = serde_ipld_dagcbor::to_vec(&doc).expect("fixture encodes");

        let Expansion::Expanded { set } = expand_permission_set(
            ParsedInclude {
                nsid: "com.example.calendar.appPerms".to_string(),
                aud: None,
            },
            record,
        ) else {
            panic!("the expander accepts this document — the finding is about what happens next");
        };
        assert!(
            matches!(
                check_grant_expansion(1, set.members.clone()),
                GrantExpansion::WithinCaps
            ),
            "PAR's own grant caps accept this request, which is what makes the nest's \
             refusal a mid-ceremony 502 rather than an honest `invalid_scope`"
        );

        let sets = vec![ConsentSetInfo {
            nsid: set.nsid,
            title: set.title,
            details: set.details,
            members: set.members,
            extra: Default::default(),
        }];
        db.open_atproto_consent_request(Some(ACTOR), "https://a/b", None, "atproto", &sets, TTL)
            .await
            .expect(
                "PAR accepted this request and handed the client a `request_uri`; \
                 the nest refusing it here is a 502 at `/oauth/authorize`",
            );
    }

    /// **The arithmetic half — the overlapping-sets case.** PAR's byte
    /// cap measures the **deduped union**, while `sets` repeats every member
    /// under its own entry, so the payload the nest stores is up to
    /// `MAX_INCLUDES_PER_REQUEST` times what PAR measured. No hostile input is
    /// needed for that — it is what a family of related sets in one namespace
    /// looks like.
    ///
    /// This builds the payload at the bridge's maxima and asserts the nest's
    /// ceiling admits it. It is deliberately an empirical check over the real
    /// `serde_json` encoding rather than a `const` assertion: the ceiling
    /// guards *encoded* bytes, so a new field on [`ConsentSetInfo`] moves the
    /// number, and arithmetic in a doc comment would not notice.
    #[test]
    fn the_sets_ceiling_admits_the_largest_payload_par_can_accept() {
        use fauna_bridge_atproto::permission_set::{
            MAX_EXPANDED_SCOPE_BYTES_PER_GRANT, MAX_EXPANDED_SCOPES_PER_GRANT,
            MAX_INCLUDES_PER_REQUEST, MAX_SET_DETAILS_BYTES, MAX_SET_TITLE_BYTES,
        };

        // The largest member list PAR can pass: the scope-count cap's worth of
        // scopes, padded to exactly the byte cap (the separators PAR counts do
        // not survive into JSON, so this is a byte over what it measures — the
        // conservative direction).
        let count = MAX_EXPANDED_SCOPES_PER_GRANT as usize;
        let each = MAX_EXPANDED_SCOPE_BYTES_PER_GRANT as usize / count;
        let members: Vec<String> = (0..count)
            .map(|i| format!("{i:0>width$}", width = each))
            .collect();

        // Every set in one namespace may declare every member — the hierarchy
        // constraint bounds the *namespace*, not the overlap.
        let sets: Vec<ConsentSetInfo> = (0..MAX_INCLUDES_PER_REQUEST)
            .map(|i| ConsentSetInfo {
                nsid: format!("com.example.calendar.set{i}"),
                title: Some("T".repeat(MAX_SET_TITLE_BYTES)),
                details: Some("D".repeat(MAX_SET_DETAILS_BYTES)),
                members: members.clone(),
                extra: Default::default(),
            })
            .collect();

        let encoded = serde_json::to_string(&sets).expect("encodes");
        assert!(
            encoded.len() <= MAX_CONSENT_SETS_LEN,
            "PAR can accept a request whose set payload encodes to {} bytes, and the nest \
             stores at most {MAX_CONSENT_SETS_LEN} — the gap is a 502 at `/oauth/authorize`",
            encoded.len(),
        );
    }

    #[tokio::test]
    async fn the_sweep_drops_only_expired_rows() {
        let db = CacheDb::open_in_memory().unwrap();
        let live = open(&db, Some(ACTOR)).await;
        db.open_atproto_consent_request(Some(ACTOR), "https://a/b", None, "atproto", &[], -1)
            .await
            .unwrap();
        assert_eq!(
            db.sweep_expired_atproto_consent_requests().await.unwrap(),
            1
        );
        assert!(
            db.get_atproto_consent_request(&live.consent_id)
                .await
                .unwrap()
                .is_some()
        );
    }
}
