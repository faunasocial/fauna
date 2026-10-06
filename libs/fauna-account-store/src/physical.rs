//! The at-rest spellings every physical arm shares — the store-meta keys and
//! value encodings, and the segment area's file naming — so the arms cannot
//! drift in *how* a fact rests where [`crate::backend::StoreBackend`] fixes
//! *what* rests. One home per spelling (the SQLite, memory and IndexedDB arms
//! all read it from here).

use anyhow::{Context, Result};

use crate::segments::SegmentKey;

/// `store_meta` key: scopes whose departure committed its row deletions but
/// whose segment files may still be in the file area (a
/// [`drop_scope`](crate::backend::StoreBackend::drop_scope) interrupted
/// between its commit and its sweep). Newline-separated scope strings; absent
/// when nothing is owed.
pub(crate) const META_SCOPE_DROPS_PENDING: &str = "scope_drops_pending";

/// The `store_meta` key holding `scope`'s serve-order watermark
/// ([`crate::backend::StoreBackend::nest_watermark`]). A scope string is its
/// own escaping: the key space is per-scope by construction, like the
/// frontier's rows.
pub(crate) fn nest_watermark_key(scope: &str) -> String {
    format!("nest_watermark/{scope}")
}

/// The `store_meta` key holding the replica id `scope`'s watermark was banked
/// against ([`crate::store::AccountStore::nest_watermark_replica`];
/// `account-sync-plane.md` § The bind leg, ruling 2). Absent = banked against
/// a nest that named none. Meaningless without the watermark beside it, so a
/// scope drop may leave it behind: the next raise re-keys it.
pub(crate) fn nest_watermark_replica_key(scope: &str) -> String {
    format!("nest_watermark_replica/{scope}")
}

/// The `store_meta` key holding `scope`'s **listed** fact
/// ([`crate::store::AccountStore::listed`]): present once this replica has
/// listed the scope from its bound nest. Replica-local and never synced, so
/// it goes with a wiped store and a re-created one starts without it.
pub(crate) fn listed_key(scope: &str) -> String {
    format!("listed/{scope}")
}

/// The `store_meta` key holding `scope`'s **unkeyed** set
/// ([`crate::store::AccountStore::unkeyed`]): the generations this replica's
/// listings left rows unopened under for want of the key, each with its
/// answered-empty bit. Replica-local and never synced, like the listed fact.
pub(crate) fn unkeyed_key(scope: &str) -> String {
    format!("unkeyed/{scope}")
}

/// The `store_meta` key holding the **let-go** set
/// ([`crate::store::AccountStore::let_go`]): the dead generations the user let
/// go on this replica, whose escrow wraps the secondary leg still owes every
/// linked holder. Replica-local and never synced, like the unkeyed set.
pub(crate) const LET_GO_KEY: &str = "let_go";

/// The `store_meta` key holding `writer`'s **parked** rows on `scope`
/// ([`crate::store::AccountStore::parked`]): the `writer_seq`s of the own
/// rows the bound nest refused for room, which the publish owes by name
/// (`account-replica-posture.md` § The store device principal, refinement 11
/// → *A row refused for room is parked*). Per writer, so a rotation's
/// successor never reads its predecessor's coordinates as its own.
/// Replica-local and never synced; it goes with a wiped store.
pub(crate) fn parked_key(scope: &str, writer: &crate::types::WriterId) -> String {
    format!("parked/{scope}/{}", writer.to_hex())
}

/// `store_meta` key of the settled replica
/// ([`crate::store::AccountStore::settled_replica`]) — the plane's opaque
/// record of the nest this store last completed a pass against.
pub(crate) const SETTLED_REPLICA_KEY: &str = "bind_settled_replica";

/// An ASCII-decimal `store_meta` value — the encoding every rising-only meta
/// write rests in.
pub(crate) fn ascii_u64(raw: &[u8]) -> Result<u64> {
    std::str::from_utf8(raw)
        .context("not UTF-8")?
        .parse()
        .context("not an unsigned decimal")
}

/// A stored rising-only value as the rising-only compare reads it — SQL's
/// `CAST(value AS INTEGER)`: the leading decimal digits, `0` when there are
/// none. The SQLite arm compares in SQL; the other arms call this, so a value
/// none of them wrote (hand damage) compares alike everywhere.
pub(crate) fn rising_meta_integer(raw: &[u8]) -> u64 {
    raw.iter()
        .take_while(|b| b.is_ascii_digit())
        .fold(0u64, |acc, b| {
            acc.saturating_mul(10).saturating_add(u64::from(b - b'0'))
        })
}

/// Every arm rests its integers within a signed 64-bit (SQLite's `INTEGER`)
/// and refuses a `u64` above it rather than wrapping — a refusal callers
/// observe (the nest watermark's `u64::MAX` case), so every arm makes it.
pub(crate) fn fits_i64(v: u64, what: &str) -> Result<()> {
    i64::try_from(v).with_context(|| format!("{what} exceeds i64"))?;
    Ok(())
}

/// The entry-and-row writes' version check
/// ([`crate::backend::StoreBackend::state_put_with_row`]): has the entry left
/// the version the caller read? `stored` is the entry's version as the write's
/// own transaction reads it, `None` when no entry is stored; the write is due
/// exactly one above it.
pub(crate) fn entry_moved(stored: Option<u64>, writing: u64) -> bool {
    stored.unwrap_or(0).checked_add(1) != Some(writing)
}

/// The actor-scope floor: an actor id as the one lowercase 64-hex spelling
/// every per-actor store location is keyed by — a native state dir
/// (`crate::db::actor_state_dir`) or a web store name (`crate::root` on
/// wasm32). Junk is refused rather than minted into a stray location.
pub(crate) fn normalize_actor_hex(actor_id_hex: &str) -> Result<String> {
    let hex = actor_id_hex.trim().to_ascii_lowercase();
    if !fauna_core::hex32::is_hex64(&hex) {
        anyhow::bail!("actor id must be 64 hex chars, got {:?}", actor_id_hex);
    }
    Ok(hex)
}

/// The pending-drop mark's scopes. A scope string is
/// `content:<kind>:<64 hex>` (`ContentScope`'s Display) — it can hold no
/// newline, so the framing needs no escaping and no encoder.
pub(crate) fn parse_pending_drops(raw: &[u8]) -> Vec<String> {
    String::from_utf8_lossy(raw)
        .lines()
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
        .collect()
}

/// The `.dat`/`.meta` file stem for one adopted segment, identical on every
/// arm with a file area: `<scope>-<kind>-seg-<id:08>`, each tag folded by
/// [`sanitize_component`]. The scope's sanitized form followed by `-` is an
/// unambiguous prefix (a scope string ends in a fixed-width hex id), which is
/// what lets a departure sweep a scope's files by name after its rows are
/// gone.
pub(crate) fn segment_stem(key: &SegmentKey) -> String {
    format!(
        "{}-{}-seg-{:08}",
        sanitize_component(&key.scope),
        sanitize_component(&key.kind),
        key.segment_id
    )
}

/// The filename prefix of every staged (not yet adopted) segment file
/// ([`crate::backend::SegmentStaging`]), in the same area as the adopted ones
/// so adoption is a same-directory rename. No [`segment_stem`] can start with
/// it: [`sanitize_component`] writes `%` only before two hex digits, and `s`
/// is not one — so neither a departure's sweep nor an adoption can name a
/// staging file, and the open-time staging sweep can name nothing else.
pub(crate) const STAGING_PREFIX: &str = "%staging-";

/// A fresh staging stem, `%staging-<unique>` — `unique` distinct among every
/// slot any live process on this store holds (the caller supplies it: a
/// process id and a per-process counter natively, a random draw on the web).
pub(crate) fn staging_stem(unique: &str) -> String {
    format!("{STAGING_PREFIX}{unique}")
}

/// The filename prefix every segment file of `scope` starts with — the
/// departure sweep's key ([`segment_stem`]).
pub(crate) fn scope_file_prefix(scope: &str) -> String {
    format!("{}-", sanitize_component(scope))
}

/// Fold a scope or kind tag into one safe filename component.
///
/// Percent-encoding rather than a character class mapped to `_`: scopes carry
/// `:` (`content:__post`) and could carry `/`, and a lossy fold would let
/// `content:x` and `content/x` name the same file — two scopes silently
/// overwriting each other's segments. This is injective, so they cannot.
pub(crate) fn sanitize_component(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for byte in s.as_bytes() {
        let c = *byte as char;
        if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
            out.push(c);
        } else {
            out.push_str(&format!("%{byte:02x}"));
        }
    }
    out
}
