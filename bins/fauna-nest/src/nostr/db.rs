//! Nostr-specific database operations.
//!
//! Tables are created when the `nostr` feature is enabled and `init_db` is called.

use anyhow::Result;
use rusqlite::{Connection, OptionalExtension};

pub const CREATE_TABLES_SQL: &str = "
    CREATE TABLE IF NOT EXISTS nostr_accounts (
        actor_id          TEXT PRIMARY KEY,
        nostr_pubkey      TEXT NOT NULL UNIQUE,
        signing_mode      TEXT NOT NULL,
        encrypted_privkey BLOB,
        nip46_bunker_url  TEXT,
        relay_list        TEXT,
        expose_content    INTEGER NOT NULL DEFAULT 0,
        auto_publish      INTEGER NOT NULL DEFAULT 0,
        publish_replies   INTEGER NOT NULL DEFAULT 1,
        publish_reactions INTEGER NOT NULL DEFAULT 0,
        inbound_to_feed   INTEGER NOT NULL DEFAULT 1,
        created_at        INTEGER NOT NULL,
        updated_at        INTEGER NOT NULL
    );

    CREATE TABLE IF NOT EXISTS nostr_follows (
        id           INTEGER PRIMARY KEY,
        actor_id     TEXT NOT NULL,
        nostr_pubkey TEXT NOT NULL,
        petname      TEXT,
        relay_hints  TEXT,
        created_at   INTEGER NOT NULL,
        UNIQUE(actor_id, nostr_pubkey)
    );

    -- `replace_key` (nullable) is `store::replace_key`'s `kind:pubkey[:d]`
    -- coordinate for an INBOUND swept row of a (parameterized-)replaceable
    -- kind, and NULL otherwise. It is what makes a NIP-09 `a` tag resolvable
    -- on the sweep plane: four of the kinds the inbound subscription asks for
    -- (34550, 30402, 30311, 30009) are addressable, and their authors delete
    -- them by coordinate, not by event id.
    CREATE TABLE IF NOT EXISTS nostr_event_map (
        fauna_post_id  TEXT NOT NULL,
        nostr_event_id TEXT NOT NULL,
        nostr_pubkey   TEXT NOT NULL,
        direction      TEXT NOT NULL,
        published_at   INTEGER NOT NULL,
        replace_key    TEXT,
        PRIMARY KEY (fauna_post_id, nostr_event_id)
    );
    CREATE INDEX IF NOT EXISTS idx_nostr_event_id ON nostr_event_map(nostr_event_id);

    CREATE TABLE IF NOT EXISTS nostr_relay_state (
        relay_url    TEXT NOT NULL,
        nostr_pubkey TEXT NOT NULL,
        last_seen    INTEGER NOT NULL,
        PRIMARY KEY (relay_url, nostr_pubkey)
    );

    CREATE TABLE IF NOT EXISTS nostr_zaps (
        zap_event_id    TEXT PRIMARY KEY,
        target_event_id TEXT,
        target_pubkey   TEXT NOT NULL,
        sender_pubkey   TEXT,
        amount_msats    INTEGER,
        created_at      INTEGER NOT NULL,
        -- Arrival time on THIS box; `created_at` above is the receipt's own,
        -- sender-controlled claim.
        inserted_at     INTEGER,
        -- The tier this receipt BOUGHT, when it met that tier's asking price
        -- (monetization.md § The asking price / § Per-post pay-to-unlock).
        -- NULL = an ordinary tip, which is every row's default and the
        -- ratified outcome for an under-threshold or unpriced target.
        --
        -- Recorded here so the two consequence classes are distinguishable AT
        -- REST: the split is decided once at ingest, and the tip surface then
        -- excludes purchases by reading this column rather than by re-deciding
        -- anything. A purchase is not a tip — counting it as one would inflate
        -- a post's tip total with a sale.
        purchased_tier  TEXT
    );
    CREATE INDEX IF NOT EXISTS idx_nostr_zaps_target ON nostr_zaps(target_event_id);

    -- NIP-57 zap-receipt trust roots. One row per signer pubkey an actor has
    -- designated as allowed to speak for their money -- in practice their
    -- LNURL/wallet provider's `nostrPubkey`. A zap receipt is signed by the
    -- *recipient's* provider, not the sender, and is plain signed JSON anyone
    -- may mint, so its own signature proves nothing; this table is what turns
    -- it into a claim worth believing (`docs/goal/behavior/monetization.md`
    -- § Zap receipts -- the trust model).
    --
    -- The empty set is the out-of-the-box default and it is load-bearing: a
    -- payee who has designated nobody believes nobody, so no zap is ever
    -- silently believed. `signer_pubkey` rests lowercase-normalized (the
    -- writers normalize) so UNIQUE actually dedupes -- the verdict compares
    -- case-insensitively, and a table that stored both cases would let one
    -- signer occupy two rows.
    CREATE TABLE IF NOT EXISTS nostr_zap_signers (
        id            INTEGER PRIMARY KEY,
        actor_id      TEXT NOT NULL,
        signer_pubkey TEXT NOT NULL,
        label         TEXT NOT NULL DEFAULT '',
        created_at    INTEGER NOT NULL,
        UNIQUE(actor_id, signer_pubkey)
    );
    CREATE INDEX IF NOT EXISTS idx_nostr_zap_signers_actor ON nostr_zap_signers(actor_id);

    CREATE TABLE IF NOT EXISTS nostr_badges (
        id              INTEGER PRIMARY KEY,
        badge_id        TEXT NOT NULL,
        badge_name      TEXT,
        badge_image     TEXT,
        awardee_pubkey  TEXT NOT NULL,
        created_at      INTEGER NOT NULL,
        UNIQUE(badge_id, awardee_pubkey)
    );
    CREATE INDEX IF NOT EXISTS idx_nostr_badges_awardee ON nostr_badges(awardee_pubkey);

    -- Relay event store (NIP-01). Every event the relay persists: native events
    -- written via EVENT by a local account, plus the signed translations of
    -- exposed Fauna posts, materialized once at the outbound-sync signing
    -- position (`crate::nostr::store`). `raw_json` is the full signed wire event
    -- (id/pubkey/sig all live inside it — the columns are the NIP-01 filter
    -- index). `replace_key` is NULL for regular/ephemeral kinds and
    -- `kind:pubkey[:dtag]` for (parameterized-)replaceable kinds, so a
    -- replacement is a single keyed lookup. `derived=1` marks a row
    -- materialized from a Fauna post — recreatable (re-derived from the post on
    -- re-expose), so deleting it when `expose_content` toggles off is an allowed
    -- no-data-loss deletion.
    -- `origin` is the Phase-2 proxy-delegation provenance column (`nostr.md`
    -- § The bridging gate → Phase 2, R4 (account-data-plane.md § The ratified decisions)): `'ingest'` = the row arrived via local
    -- relay/materialization/sweep (the default); `'federation'` = it arrived over a nostr
    -- federation leg. Federation legs select only `origin='ingest'` rows, so a
    -- federation-arrived row is never re-exported (killing echo + loops).
    CREATE TABLE IF NOT EXISTS nostr_events (
        id           TEXT PRIMARY KEY,
        pubkey       TEXT NOT NULL,
        kind         INTEGER NOT NULL,
        created_at   INTEGER NOT NULL,
        raw_json     TEXT NOT NULL,
        replace_key  TEXT,
        derived      INTEGER NOT NULL DEFAULT 0,
        stored_at    INTEGER NOT NULL,
        expiration   INTEGER,
        origin       TEXT NOT NULL DEFAULT 'ingest'
    );
    CREATE INDEX IF NOT EXISTS idx_nostr_events_author ON nostr_events(pubkey, kind, created_at DESC);
    CREATE INDEX IF NOT EXISTS idx_nostr_events_created ON nostr_events(created_at DESC);
    CREATE UNIQUE INDEX IF NOT EXISTS idx_nostr_events_replace
        ON nostr_events(replace_key) WHERE replace_key IS NOT NULL;

    -- Single-letter indexed tags (`e`/`p`/`d`/…) for REQ tag filters. One row
    -- per (event, tag-name, value); removed with its event.
    CREATE TABLE IF NOT EXISTS nostr_event_tags (
        event_id  TEXT NOT NULL,
        name      TEXT NOT NULL,
        value     TEXT NOT NULL,
        PRIMARY KEY (event_id, name, value)
    );
    CREATE INDEX IF NOT EXISTS idx_nostr_event_tags_nv ON nostr_event_tags(name, value);

    -- NIP-50 search over the public event store (`nostr.md` § The relay event
    -- store — placement ratified 2026-07-15). Relay-internal, and a DIFFERENT
    -- table from both the per-user tantivy index (the nest holds no index key)
    -- and `content_fts` (the Fauna Search page's corpus). The latter is now
    -- fed by this store too, under the uniform per-bridge search policy — but
    -- as a separate, policy-gated, `bridge.nostr`-typed row set with its own
    -- corpus, keying and lifecycle (see the `nostr_events_bridge_search_ad`
    -- trigger below); this table keeps serving the Nostr protocol surface and
    -- never feeds `fauna.search.query`.
    -- Plain unicode61, deliberately NOT content_fts's porter stemmer: the
    -- live-broadcast path approximates this verdict with literal token
    -- containment (fauna_bridge_nostr::nip50), and stemming would widen the
    -- store-read verdict away from it.
    -- rowid-linked to nostr_events.rowid and maintained by the two triggers
    -- below, so every store write/delete path — NIP-01 replacement, NIP-09
    -- deletion, NIP-40 sweep, expose-toggle-off, and any future path — stays
    -- in lockstep structurally; nothing 'runs an indexer'.
    -- Kind-1059 gift wraps are excluded at index time: the search plane is
    -- structurally incapable of touching DM ciphertext (the serving gates
    -- still apply unchanged on top).
    CREATE VIRTUAL TABLE IF NOT EXISTS nostr_event_fts USING fts5(
        content,
        tokenize='unicode61'
    );
    CREATE TRIGGER IF NOT EXISTS nostr_events_fts_ai
    AFTER INSERT ON nostr_events
    WHEN new.kind != 1059
    BEGIN
        INSERT INTO nostr_event_fts (rowid, content)
        VALUES (new.rowid, coalesce(json_extract(new.raw_json, '$.content'), ''));
    END;
    CREATE TRIGGER IF NOT EXISTS nostr_events_fts_ad
    AFTER DELETE ON nostr_events
    BEGIN
        DELETE FROM nostr_event_fts WHERE rowid = old.rowid;
    END;

    -- The Fauna Search corpus (`content_fts`) removal arm — the delete half of
    -- the uniform per-bridge search policy (`content-index.md` § Bridge content
    -- in the Search corpus: 'indexing rides each bridge's nest-transit point,
    -- in lockstep — nobody runs an indexer'). Writing is Rust-side (the key is
    -- blake3(content_type:natural_id), which SQL cannot compute); removal is a
    -- trigger for the same reason `nostr_event_fts` uses one — NIP-01
    -- replacement, NIP-09 deletion, NIP-40 expiry sweep, the expose-toggle-off
    -- bulk delete, and any future path all delete `nostr_events` rows without
    -- passing through one common Rust funnel, so only a trigger keeps the
    -- corpus in lockstep by construction. `bridge_index_map` is what makes the
    -- blake3 key reachable from SQL.
    CREATE TRIGGER IF NOT EXISTS nostr_events_bridge_search_ad
    AFTER DELETE ON nostr_events
    BEGIN
        DELETE FROM content_fts WHERE rowid IN (
            SELECT m.fts_rowid FROM content_fts_map m
            JOIN bridge_index_map b ON b.content_id = m.content_id
            WHERE b.content_type = 'bridge.nostr' AND b.natural_id = old.id);
        DELETE FROM content_fts_map WHERE content_id IN (
            SELECT content_id FROM bridge_index_map
            WHERE content_type = 'bridge.nostr' AND natural_id = old.id);
        DELETE FROM bridge_index_map
            WHERE content_type = 'bridge.nostr' AND natural_id = old.id;
    END;

    -- The SWEEP plane's twin of the trigger above. Nostr rests foreign content
    -- on the nest by two different routes, and they touch different tables:
    -- the relay store writes `nostr_events` (trigger above), while the sync
    -- worker's inbound sweep translates a followed author's event into a Fauna
    -- post and records it here (`direction = 'inbound'`). Both index the corpus
    -- under the SAME key — `bridge.nostr` : the event id — so an event seen by
    -- both routes indexes once; each therefore needs its own removal arm keyed
    -- on the same natural id, or one plane's teardown leaves the row resting.
    --
    -- A trigger for the same reason its twin is one: every sweep-plane removal
    -- path (NIP-09 kind-5, the NIP-40 expiry sweep, and any future one) ends in
    -- deleting the map row, so hanging the corpus removal off that delete keeps
    -- the two in lockstep by construction rather than by remembering to call
    -- something. Scoped to `inbound`: an OUTBOUND row is a materialization of a
    -- local user's own Fauna post, which never enters the bridge corpus (its
    -- body is already in `content_fts` as the post itself — the
    -- no-double-surfacing rule), so retiring one must not evict a corpus row
    -- that a genuinely foreign event of the same id put there.
    CREATE TRIGGER IF NOT EXISTS nostr_event_map_bridge_search_ad
    AFTER DELETE ON nostr_event_map
    WHEN old.direction = 'inbound'
    BEGIN
        DELETE FROM content_fts WHERE rowid IN (
            SELECT m.fts_rowid FROM content_fts_map m
            JOIN bridge_index_map b ON b.content_id = m.content_id
            WHERE b.content_type = 'bridge.nostr' AND b.natural_id = old.nostr_event_id);
        DELETE FROM content_fts_map WHERE content_id IN (
            SELECT content_id FROM bridge_index_map
            WHERE content_type = 'bridge.nostr' AND natural_id = old.nostr_event_id);
        DELETE FROM bridge_index_map
            WHERE content_type = 'bridge.nostr' AND natural_id = old.nostr_event_id;
    END;

    -- NIP-46 bunker (`nostr.md` § The nest as the user's NIP-46 signer).
    -- Per-account signer keypair: minted on first invite, encrypted at rest
    -- under its own key_crypto context. Deliberately not the user's keypair —
    -- the bunker:// connect string must not leak the user's identity, and the
    -- signer pubkey keys request routing + rate limiting.
    CREATE TABLE IF NOT EXISTS nostr_bunker_signers (
        actor_id          TEXT PRIMARY KEY,
        signer_pubkey     TEXT NOT NULL UNIQUE,
        encrypted_privkey BLOB NOT NULL,
        created_at        INTEGER NOT NULL
    );

    -- Per-app connections: the nest-enforced roster (deliberately NOT
    -- capability_grants — the nest is the enforcement point; § Why a
    -- nest-enforced roster). status: pending (invite secret outstanding,
    -- secret_hash set, app_pubkey NULL) / active (app_pubkey pinned,
    -- secret_hash cleared) / revoked (tombstone). expires_at is the invite
    -- TTL while pending and the sliding idle expiry once active.
    CREATE TABLE IF NOT EXISTS nostr_bunker_apps (
        id           INTEGER PRIMARY KEY,
        actor_id     TEXT NOT NULL,
        app_pubkey   TEXT,
        label        TEXT NOT NULL DEFAULT '',
        secret_hash  BLOB,
        status       TEXT NOT NULL,
        created_at   INTEGER NOT NULL,
        last_used_at INTEGER,
        use_count    INTEGER NOT NULL DEFAULT 0,
        expires_at   INTEGER NOT NULL
    );
    CREATE INDEX IF NOT EXISTS idx_nostr_bunker_apps_actor ON nostr_bunker_apps(actor_id);

    -- The oracle's bunker clients (TP11 — `key-material-hierarchy.md`
    -- § Audience: deployment infrastructure → *The oracle*; `ui/nostr.md`
    -- § The nest as the user's NIP-46 signer → *A principal as a bunker
    -- client*). One row per (account, third-party principal): the NIP-46
    -- client key the principal bound with `fauna.nostr.bunker.bind`. The row
    -- is NOT the authority — every request re-resolves the principal row and
    -- the owner's live `identity.op` grant (`nostr/oracle.rs`); it carries
    -- the audit counters and the rate ceiling's fixed window
    -- (`fauna_core::identity_op::RateWindow`). Deleted with the principal
    -- (`revoke_third_party_principal`), the link (`unlink_account`) and at
    -- succession (burned — `db/actor_tables.rs`).
    CREATE TABLE IF NOT EXISTS nostr_oracle_clients (
        id            INTEGER PRIMARY KEY,
        actor_id      TEXT NOT NULL,
        principal_id  BLOB NOT NULL,
        client_pubkey TEXT NOT NULL,
        created_at    INTEGER NOT NULL,
        last_used_at  INTEGER,
        use_count     INTEGER NOT NULL DEFAULT 0,
        window_start  INTEGER NOT NULL DEFAULT 0,
        window_count  INTEGER NOT NULL DEFAULT 0,
        UNIQUE (actor_id, principal_id)
    );
    CREATE INDEX IF NOT EXISTS idx_nostr_oracle_clients_pubkey
        ON nostr_oracle_clients(actor_id, client_pubkey);

    -- The oracle's per-operation record (the audit split,
    -- `key-material-hierarchy.md` § *The oracle*): one row per key operation
    -- the custodian performed for a principal — the class and its detail
    -- (the event kind for `nostr.sign_event`, the method for `nostr.nip44`).
    -- Bounded to the newest `oracle::OPS_KEPT_PER_CLIENT` per client; the
    -- grant lifecycle itself is the client-signed `GrantEvent` log.
    CREATE TABLE IF NOT EXISTS nostr_oracle_ops (
        id        INTEGER PRIMARY KEY,
        actor_id  TEXT NOT NULL,
        client_id INTEGER NOT NULL,
        at        INTEGER NOT NULL,
        class     TEXT NOT NULL,
        detail    TEXT NOT NULL
    );
    CREATE INDEX IF NOT EXISTS idx_nostr_oracle_ops_client
        ON nostr_oracle_ops(client_id, id);

    -- Phase-2 proxy-delegation federation cursors (`nostr.md` § The bridging
    -- gate → Phase 2, R5). Head-side only: the head owns and persists the
    -- compound `(stored_at, id)` lexicographic cursor per (actor, peer public
    -- box), advanced only after a successful push reply / pull ingest — so an
    -- at-least-once leg + event-id dedup nets an exactly-once effect, crash-safe
    -- (worst case is a re-pull the store dedups). Ephemeral-recoverable: a lost
    -- cursor re-pulls from the start and dedups. Zeroed columns (`0`/`''`) mean
    -- nothing has bridged yet -- lexicographically before every real row.
    CREATE TABLE IF NOT EXISTS nostr_federation_cursors (
        actor_id       TEXT NOT NULL,
        peer_nest_id   TEXT NOT NULL,
        push_stored_at INTEGER NOT NULL DEFAULT 0,
        push_id        TEXT NOT NULL DEFAULT '',
        pull_stored_at INTEGER NOT NULL DEFAULT 0,
        pull_id        TEXT NOT NULL DEFAULT '',
        PRIMARY KEY (actor_id, peer_nest_id)
    );
";

// ── Row types ───────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct NostrAccount {
    pub actor_id: String,
    pub nostr_pubkey: String,
    pub signing_mode: String,
    pub encrypted_privkey: Option<Vec<u8>>,
    pub nip46_bunker_url: Option<String>,
    pub relay_list: Option<String>,
    pub expose_content: bool,
    pub auto_publish: bool,
    pub publish_replies: bool,
    pub publish_reactions: bool,
    pub inbound_to_feed: bool,
    pub created_at: i64,
    pub updated_at: i64,
}

#[derive(Debug, Clone)]
pub struct NostrFollow {
    pub id: i64,
    pub actor_id: String,
    pub nostr_pubkey: String,
    pub petname: Option<String>,
    pub relay_hints: Option<String>,
    pub created_at: i64,
}

#[derive(Debug, Clone)]
pub struct EventMapEntry {
    pub fauna_post_id: String,
    pub nostr_event_id: String,
    pub nostr_pubkey: String,
    pub direction: String,
    pub published_at: i64,
}

/// Settings updatable via `fauna.bridges.set_settings` (bridge_id "nostr").
/// Deserialized from the wire by `NostrProvider::update_settings`.
#[derive(Debug, Clone, Default, serde::Deserialize)]
pub struct NostrSettings {
    pub relay_list: Option<String>,
    pub expose_content: Option<bool>,
    pub auto_publish: Option<bool>,
    pub publish_replies: Option<bool>,
    pub publish_reactions: Option<bool>,
    pub inbound_to_feed: Option<bool>,
}

// ── Account CRUD (synchronous, takes &Connection) ───────────────

/// Is at least one user's `nsec` deposited on this box
/// (`nostr_accounts.encrypted_privkey`)? This is the box-level half of the
/// Nostr bridging gate (`crate::nostr::nostr_bridging_available`): the
/// deposit is the explicit per-user trust act that licenses the box to act
/// as that user's Nostr agent (`docs/goal/ui/nostr.md` § The bridging gate).
/// Per-user enforcement is structural — signing, gift-wrap unwrap, and DM
/// send all require the account's own deposited key.
pub fn any_nsec_deposited(conn: &Connection) -> Result<bool> {
    let deposited: bool = conn
        .prepare("SELECT EXISTS(SELECT 1 FROM nostr_accounts WHERE encrypted_privkey IS NOT NULL)")?
        .query_row([], |row| row.get::<_, i32>(0))
        .map(|c| c > 0)?;
    Ok(deposited)
}

/// Why [`link_account`] refused. Callers map `PubkeyHeld` to a typed refusal
/// (the `CredentialWriteError` pattern in `db/atproto_pds.rs`).
///
/// The write is never an upsert: `nostr_accounts` is UNIQUE on
/// `nostr_pubkey`, so a REPLACE naming a pubkey another actor holds deletes
/// that actor's row — deposited nsec included, which for a `generated` key is
/// its only copy. Several link routes carry an unproven pubkey (`nip07`,
/// `remote`, the `nostr_push` provision), so one pubkey maps to one actor and
/// the writer, not each caller, enforces it.
#[derive(Debug)]
pub enum LinkAccountError {
    /// Another actor on this box already holds this pubkey.
    PubkeyHeld,
    /// This actor already has an account row (callers check first; this is
    /// the race arm).
    ActorLinked,
    Other(anyhow::Error),
}

impl std::fmt::Display for LinkAccountError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::PubkeyHeld => {
                f.write_str("nostr pubkey is linked to another account on this box")
            }
            Self::ActorLinked => f.write_str("actor already has a linked nostr account"),
            Self::Other(e) => write!(f, "{e:#}"),
        }
    }
}

impl std::error::Error for LinkAccountError {}

fn map_link_write_err(e: rusqlite::Error) -> LinkAccountError {
    if let rusqlite::Error::SqliteFailure(f, _) = &e {
        match f.extended_code {
            rusqlite::ffi::SQLITE_CONSTRAINT_UNIQUE => return LinkAccountError::PubkeyHeld,
            rusqlite::ffi::SQLITE_CONSTRAINT_PRIMARYKEY => return LinkAccountError::ActorLinked,
            _ => {}
        }
    }
    LinkAccountError::Other(anyhow::Error::from(e).context("link nostr account"))
}

pub fn link_account(
    conn: &Connection,
    actor_id: &str,
    pubkey: &str,
    mode: &str,
    encrypted_privkey: Option<&[u8]>,
    bunker_url: Option<&str>,
    relay_list: Option<&str>,
) -> std::result::Result<(), LinkAccountError> {
    let now = crate::db::now_epoch_secs();
    conn.execute(
        "INSERT INTO nostr_accounts
         (actor_id, nostr_pubkey, signing_mode, encrypted_privkey,
          nip46_bunker_url, relay_list, created_at, updated_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?7)",
        rusqlite::params![
            actor_id,
            pubkey,
            mode,
            encrypted_privkey,
            bunker_url,
            relay_list,
            now
        ],
    )
    .map_err(map_link_write_err)?;
    Ok(())
}

pub fn unlink_account(conn: &Connection, actor_id: &str) -> Result<()> {
    conn.execute("DELETE FROM nostr_accounts WHERE actor_id = ?1", [actor_id])?;
    // Key-less cascade (`nostr.md` § signer): unlinking revokes every bunker
    // connection; the signer keypair is nest-minted and re-minted on a future
    // first invite (recreatable — no user data), so its row is dropped.
    conn.execute(
        "DELETE FROM nostr_bunker_signers WHERE actor_id = ?1",
        [actor_id],
    )?;
    conn.execute(
        "UPDATE nostr_bunker_apps SET status = 'revoked', secret_hash = NULL WHERE actor_id = ?1",
        [actor_id],
    )?;
    // The oracle's bound clients go with the deposited key they spoke to; a
    // principal re-binds after a future link (its grant still decides).
    conn.execute(
        "DELETE FROM nostr_oracle_ops WHERE actor_id = ?1",
        [actor_id],
    )?;
    conn.execute(
        "DELETE FROM nostr_oracle_clients WHERE actor_id = ?1",
        [actor_id],
    )?;
    Ok(())
}

pub fn get_account(conn: &Connection, actor_id: &str) -> Result<Option<NostrAccount>> {
    let mut stmt = conn.prepare(
        "SELECT actor_id, nostr_pubkey, signing_mode, encrypted_privkey,
                nip46_bunker_url, relay_list, expose_content, auto_publish,
                publish_replies, publish_reactions, inbound_to_feed,
                created_at, updated_at
         FROM nostr_accounts WHERE actor_id = ?1",
    )?;
    let row = stmt.query_row([actor_id], row_to_account);
    match row {
        Ok(acct) => Ok(Some(acct)),
        Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
        Err(e) => Err(e.into()),
    }
}

pub fn get_account_by_pubkey(conn: &Connection, pubkey: &str) -> Result<Option<NostrAccount>> {
    let mut stmt = conn.prepare(
        "SELECT actor_id, nostr_pubkey, signing_mode, encrypted_privkey,
                nip46_bunker_url, relay_list, expose_content, auto_publish,
                publish_replies, publish_reactions, inbound_to_feed,
                created_at, updated_at
         FROM nostr_accounts WHERE nostr_pubkey = ?1",
    )?;
    let row = stmt.query_row([pubkey], row_to_account);
    match row {
        Ok(acct) => Ok(Some(acct)),
        Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
        Err(e) => Err(e.into()),
    }
}

/// Every registered bunker signer pubkey on this box (across all custodial
/// accounts). The head's NIP-46 proxy subscription (spec R10) uses this as the
/// `#p` filter it subscribes with on each paired public serving box's relay —
/// so a bunker request the app deposited at the public box (p-tagging the
/// signer) reaches the head that hosts the signer's key. Empty → nothing to
/// proxy (no invites minted anywhere).
pub fn list_bunker_signer_pubkeys(conn: &Connection) -> Result<Vec<String>> {
    let mut stmt = conn.prepare("SELECT signer_pubkey FROM nostr_bunker_signers")?;
    let rows = stmt
        .query_map([], |r| r.get::<_, String>(0))?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    Ok(rows)
}

pub fn update_settings(conn: &Connection, actor_id: &str, settings: &NostrSettings) -> Result<()> {
    let now = crate::db::now_epoch_secs();
    if let Some(ref relays) = settings.relay_list {
        conn.execute(
            "UPDATE nostr_accounts SET relay_list = ?1, updated_at = ?2 WHERE actor_id = ?3",
            rusqlite::params![relays, now, actor_id],
        )?;
    }
    if let Some(v) = settings.expose_content {
        conn.execute(
            "UPDATE nostr_accounts SET expose_content = ?1, updated_at = ?2 WHERE actor_id = ?3",
            rusqlite::params![v as i32, now, actor_id],
        )?;
    }
    if let Some(v) = settings.auto_publish {
        conn.execute(
            "UPDATE nostr_accounts SET auto_publish = ?1, updated_at = ?2 WHERE actor_id = ?3",
            rusqlite::params![v as i32, now, actor_id],
        )?;
    }
    if let Some(v) = settings.publish_replies {
        conn.execute(
            "UPDATE nostr_accounts SET publish_replies = ?1, updated_at = ?2 WHERE actor_id = ?3",
            rusqlite::params![v as i32, now, actor_id],
        )?;
    }
    if let Some(v) = settings.publish_reactions {
        conn.execute(
            "UPDATE nostr_accounts SET publish_reactions = ?1, updated_at = ?2 WHERE actor_id = ?3",
            rusqlite::params![v as i32, now, actor_id],
        )?;
    }
    if let Some(v) = settings.inbound_to_feed {
        conn.execute(
            "UPDATE nostr_accounts SET inbound_to_feed = ?1, updated_at = ?2 WHERE actor_id = ?3",
            rusqlite::params![v as i32, now, actor_id],
        )?;
    }
    Ok(())
}

// ── Follow CRUD ─────────────────────────────────────────────────

pub fn add_follow(
    conn: &Connection,
    actor_id: &str,
    nostr_pubkey: &str,
    petname: Option<&str>,
    relay_hints: Option<&str>,
) -> Result<()> {
    let now = crate::db::now_epoch_secs();
    conn.execute(
        "INSERT OR REPLACE INTO nostr_follows
         (actor_id, nostr_pubkey, petname, relay_hints, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5)",
        rusqlite::params![actor_id, nostr_pubkey, petname, relay_hints, now],
    )?;
    Ok(())
}

pub fn remove_follow(conn: &Connection, actor_id: &str, nostr_pubkey: &str) -> Result<()> {
    conn.execute(
        "DELETE FROM nostr_follows WHERE actor_id = ?1 AND nostr_pubkey = ?2",
        rusqlite::params![actor_id, nostr_pubkey],
    )?;
    Ok(())
}

/// Is `nostr_pubkey` followed by an account this nest actually sweeps for?
///
/// **The ingest-side authorization for the inbound sweep plane.** The
/// subscription puts `authors: <follow list>` in the `Filter` it sends to the
/// relay, but that filter is enforced by the *relay* — an untrusted party, on a
/// URL the follow's `relay_hints` chose (user-supplied through `add_follow`
/// today; a future NIP-65 ingestion would make it network-supplied — the dial
/// itself is guarded either way, `relays::relay_dial_policy`). A hostile or
/// buggy relay returns whatever it likes, so membership must be re-checked here, on the
/// receiving side, before a stranger's event can rest in `content` or reach the
/// user's Search corpus.
///
/// The account predicate is deliberately **the same one
/// [`NostrSyncWorker::refresh_inbound_subscriptions`](super::sync_worker) selects
/// on** (`inbound_to_feed = 1 AND encrypted_privkey IS NOT NULL`): the question
/// is not "did anyone ever follow this pubkey" but "did an account that is
/// currently sweeping ask for it". A follow belonging to an account that has
/// since switched `inbound_to_feed` off, or whose nsec was withdrawn, is not a
/// licence — the subscription would no longer name that pubkey either.
///
/// Matching is **exact equality**, not the prefix (`starts_with`) semantics
/// `Filter::matches` uses for wire filters — a prefix is the right rule for
/// serving a REQ and the wrong one for an authorization gate.
///
/// Fail-closed: a query error answers `false` (refuse), never `true`.
pub fn is_followed_by_sweeping_account(conn: &Connection, nostr_pubkey: &str) -> bool {
    conn.query_row(
        "SELECT EXISTS(
             SELECT 1 FROM nostr_follows f
             JOIN nostr_accounts a ON a.actor_id = f.actor_id
             WHERE f.nostr_pubkey = ?1
               AND a.inbound_to_feed = 1
               AND a.encrypted_privkey IS NOT NULL
         )",
        [nostr_pubkey],
        |row| row.get::<_, i64>(0),
    )
    .map(|n| n == 1)
    .unwrap_or(false)
}

pub fn list_follows(conn: &Connection, actor_id: &str) -> Result<Vec<NostrFollow>> {
    let mut stmt = conn.prepare(
        "SELECT id, actor_id, nostr_pubkey, petname, relay_hints, created_at
         FROM nostr_follows WHERE actor_id = ?1 ORDER BY created_at DESC",
    )?;
    let rows = stmt
        .query_map([actor_id], |row| {
            Ok(NostrFollow {
                id: row.get(0)?,
                actor_id: row.get(1)?,
                nostr_pubkey: row.get(2)?,
                petname: row.get(3)?,
                relay_hints: row.get(4)?,
                created_at: row.get(5)?,
            })
        })?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows)
}

// ── Event map ───────────────────────────────────────────────────

pub fn insert_event_map(
    conn: &Connection,
    fauna_post_id: &str,
    nostr_event_id: &str,
    nostr_pubkey: &str,
    direction: &str,
) -> Result<()> {
    insert_event_map_with_replace_key(
        conn,
        fauna_post_id,
        nostr_event_id,
        nostr_pubkey,
        direction,
        None,
    )
}

/// [`insert_event_map`], recording the row's `replace_key` coordinate too — the
/// inbound sweep's form, because a NIP-09 `a` tag names a coordinate and not an
/// event id (`store::replace_key`; `None` for a regular kind, which is most of
/// them).
pub fn insert_event_map_with_replace_key(
    conn: &Connection,
    fauna_post_id: &str,
    nostr_event_id: &str,
    nostr_pubkey: &str,
    direction: &str,
    replace_key: Option<&str>,
) -> Result<()> {
    let now = crate::db::now_epoch_secs();
    conn.execute(
        "INSERT OR IGNORE INTO nostr_event_map
         (fauna_post_id, nostr_event_id, nostr_pubkey, direction, published_at, replace_key)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        rusqlite::params![
            fauna_post_id,
            nostr_event_id,
            nostr_pubkey,
            direction,
            now,
            replace_key
        ],
    )?;
    Ok(())
}

/// Every derived Nostr event id mapped for a Fauna post (the outbound
/// materializations a kind-5 deletion must name — `feed.md` § Post deletion,
/// propagation). `fauna_post_id` is the lowercase-hex 32-byte content digest,
/// the same key `materialize_account` inserts.
pub fn list_event_ids_by_fauna_id(conn: &Connection, fauna_post_id: &str) -> Result<Vec<String>> {
    let mut stmt = conn.prepare(
        "SELECT nostr_event_id FROM nostr_event_map
          WHERE fauna_post_id = ?1 AND direction = 'outbound'",
    )?;
    let rows = stmt.query_map([fauna_post_id], |r| r.get::<_, String>(0))?;
    rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
}

/// Drop a deleted Fauna post's outbound map rows — after the kind-5 has
/// removed the derived events, a surviving map row would only make a delete
/// retry re-publish and (were the post ever recreated byte-identically) block
/// re-materialization against a row that no longer describes a stored event.
pub fn delete_event_map_by_fauna_id(conn: &Connection, fauna_post_id: &str) -> Result<usize> {
    let n = conn.execute(
        "DELETE FROM nostr_event_map WHERE fauna_post_id = ?1 AND direction = 'outbound'",
        [fauna_post_id],
    )?;
    Ok(n)
}

/// Resolve a NIP-09 `e` tag against the **sweep plane**, author-scoped: the
/// swept Fauna post id for `nostr_event_id`, but only when that row is an
/// `inbound` translation authored by `nostr_pubkey`.
///
/// The two clauses are one guard doing two jobs. `nostr_pubkey` is NIP-09's own
/// rule — a relay ignores an `e` tag naming another author's event — and
/// `direction = 'inbound'` is what stops a *destructive* remote verb from ever
/// reaching a local user's own content: a local post's materialization is
/// mapped `outbound`, so it is not addressable here at all.
/// `fauna.posts.delete`, with its three author checks, stays the only verb that
/// may destroy a local post. Both clauses are mutation-pinned by
/// `tests/conformance_nostr_inbound_lifecycle.rs`.
pub fn resolve_inbound_by_event_id(
    conn: &Connection,
    nostr_event_id: &str,
    nostr_pubkey: &str,
) -> Result<Option<String>> {
    conn.query_row(
        "SELECT fauna_post_id FROM nostr_event_map
          WHERE nostr_event_id = ?1 AND nostr_pubkey = ?2 AND direction = 'inbound'",
        rusqlite::params![nostr_event_id, nostr_pubkey],
        |r| r.get::<_, String>(0),
    )
    .optional()
    .map_err(Into::into)
}

/// [`resolve_inbound_by_event_id`]'s coordinate twin, for a NIP-09 `a` tag —
/// same author scoping, keyed on the `kind:pubkey[:d]` coordinate the sweep
/// recorded. Returns `(fauna_post_id, nostr_event_id)`: the caller needs the
/// event id too, because that is what the map row is deleted by.
///
/// The coordinate already embeds the author pubkey, so the explicit
/// `nostr_pubkey` clause is redundant *for a well-formed coordinate* — it is
/// kept because the coordinate is externally supplied text and the guard must
/// not depend on parsing it correctly.
pub fn resolve_inbound_by_replace_key(
    conn: &Connection,
    replace_key: &str,
    nostr_pubkey: &str,
) -> Result<Option<(String, String)>> {
    conn.query_row(
        "SELECT fauna_post_id, nostr_event_id FROM nostr_event_map
          WHERE replace_key = ?1 AND nostr_pubkey = ?2 AND direction = 'inbound'",
        rusqlite::params![replace_key, nostr_pubkey],
        |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)),
    )
    .optional()
    .map_err(Into::into)
}

/// The nostr event id a swept Fauna post came from, if it is an `inbound` row —
/// the reverse of [`resolve_inbound_by_event_id`], for paths that start from
/// the content side (the NIP-40 expiry sweep, which finds its work in
/// `content.expires_at`).
pub fn inbound_event_id_for_post(conn: &Connection, fauna_post_id: &str) -> Result<Option<String>> {
    conn.query_row(
        "SELECT nostr_event_id FROM nostr_event_map
          WHERE fauna_post_id = ?1 AND direction = 'inbound'",
        [fauna_post_id],
        |r| r.get::<_, String>(0),
    )
    .optional()
    .map_err(Into::into)
}

/// Retire one swept row's map entry. This is the write that fires
/// `nostr_event_map_bridge_search_ad`, so it is how every sweep-plane removal
/// path takes the bridge Search corpus row with it — never call a corpus
/// removal separately.
pub fn delete_inbound_event_map(
    conn: &Connection,
    fauna_post_id: &str,
    nostr_event_id: &str,
) -> Result<usize> {
    let n = conn.execute(
        "DELETE FROM nostr_event_map
          WHERE fauna_post_id = ?1 AND nostr_event_id = ?2 AND direction = 'inbound'",
        rusqlite::params![fauna_post_id, nostr_event_id],
    )?;
    Ok(n)
}

pub fn get_event_by_nostr_id(
    conn: &Connection,
    nostr_event_id: &str,
) -> Result<Option<EventMapEntry>> {
    let mut stmt = conn.prepare(
        "SELECT fauna_post_id, nostr_event_id, nostr_pubkey, direction, published_at
         FROM nostr_event_map WHERE nostr_event_id = ?1",
    )?;
    let row = stmt.query_row([nostr_event_id], row_to_event_map);
    match row {
        Ok(e) => Ok(Some(e)),
        Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
        Err(e) => Err(e.into()),
    }
}

pub fn get_event_by_fauna_id(
    conn: &Connection,
    fauna_post_id: &str,
) -> Result<Option<EventMapEntry>> {
    let mut stmt = conn.prepare(
        "SELECT fauna_post_id, nostr_event_id, nostr_pubkey, direction, published_at
         FROM nostr_event_map WHERE fauna_post_id = ?1",
    )?;
    let row = stmt.query_row([fauna_post_id], row_to_event_map);
    match row {
        Ok(e) => Ok(Some(e)),
        Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
        Err(e) => Err(e.into()),
    }
}

// ── Relay state ─────────────────────────────────────────────────

pub fn upsert_relay_state(
    conn: &Connection,
    relay_url: &str,
    nostr_pubkey: &str,
    last_seen: i64,
) -> Result<()> {
    conn.execute(
        "INSERT OR REPLACE INTO nostr_relay_state (relay_url, nostr_pubkey, last_seen)
         VALUES (?1, ?2, ?3)",
        rusqlite::params![relay_url, nostr_pubkey, last_seen],
    )?;
    Ok(())
}

pub fn get_relay_state(
    conn: &Connection,
    relay_url: &str,
    nostr_pubkey: &str,
) -> Result<Option<i64>> {
    let mut stmt = conn.prepare(
        "SELECT last_seen FROM nostr_relay_state WHERE relay_url = ?1 AND nostr_pubkey = ?2",
    )?;
    let row = stmt.query_row(rusqlite::params![relay_url, nostr_pubkey], |row| {
        row.get::<_, i64>(0)
    });
    match row {
        Ok(ts) => Ok(Some(ts)),
        Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
        Err(e) => Err(e.into()),
    }
}

// The relay's historical serving now runs off the persistent event store
// (`crate::nostr::store`), not a per-REQ translate-on-read over `content`. The
// former `query_exposed_content` / `list_exposed_pubkeys` / `ExposedContent`
// helpers were removed with that path: `query_exposed_content` joined
// `n.actor_id = hex(c.author)` (SQLite's uppercase `hex()`) against the
// lowercase-hex `actor_id` convention, so it was case-broken and returned
// nothing. Materialization uses `lower(hex(...))` in `store::materialize_account`.

// ── Zap CRUD ────────────────────────────────────────────────────

pub fn insert_zap(
    conn: &Connection,
    zap_event_id: &str,
    target_event_id: Option<&str>,
    target_pubkey: &str,
    sender_pubkey: Option<&str>,
    amount_msats: Option<i64>,
    created_at: i64,
    purchased_tier: Option<&str>,
) -> Result<()> {
    // `created_at` is the receipt's own claim; `inserted_at` is this box's
    // arrival fact, stamped here so no caller can forge it from the wire.
    let now = crate::db::now_epoch_secs();
    conn.execute(
        "INSERT OR IGNORE INTO nostr_zaps
         (zap_event_id, target_event_id, target_pubkey, sender_pubkey, amount_msats, created_at, inserted_at, purchased_tier)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
        rusqlite::params![
            zap_event_id,
            target_event_id,
            target_pubkey,
            sender_pubkey,
            amount_msats,
            created_at,
            now,
            purchased_tier
        ],
    )?;
    Ok(())
}

/// Returns `(total_msats, zap_count)` for a given target event ID.
/// If no zaps exist for the event, returns `(0, 0)`.
pub fn get_zap_total(conn: &Connection, target_event_id: &str) -> Result<(i64, i64)> {
    let row = conn.query_row(
        "SELECT COALESCE(SUM(amount_msats), 0), COUNT(*)
         FROM nostr_zaps
         WHERE target_event_id = ?1",
        [target_event_id],
        |row| Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?)),
    )?;
    Ok(row)
}

/// Is this receipt already recorded? — the **replay question** the
/// `zaps.receipt.ingest` gate asks before it spends anything.
///
/// `nostr_zaps.zap_event_id` is the mechanism's own idempotency key (its
/// `INSERT OR IGNORE` keys on it), so a receipt already in the table is a
/// redelivery, not a new operation. Without this check the gate would be an
/// **amplifier**: a kind-9735 from a designated signer is public, replayable
/// JSON, so anyone holding a copy could resend it to the relay endpoint until
/// the payee's zap quota was spent and their real zaps started bouncing.
/// Spending once per distinct receipt is what keeps the bound a bound on
/// *zapping* rather than on *being resent to*.
pub fn has_zap(conn: &Connection, zap_event_id: &str) -> Result<bool> {
    let found: i64 = conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM nostr_zaps WHERE zap_event_id = ?1)",
        [zap_event_id],
        |row| row.get(0),
    )?;
    Ok(found != 0)
}

/// Has this payee already been zapped by this sender? — the **newness question**
/// the `zaps.receipt.ingest` gate asks (`dynamic-features.md` § The quota
/// grammar's third refinement: *"the caller resolves the operation's newness
/// delta against the feature's own mechanism … 'not currently in the records ⇒
/// new'"*).
///
/// Deliberately a *predicate*, never a count. The counterparty dimension's
/// bound is summed from the usage buckets, and this row set exists only to
/// decide whether the current receipt introduces someone — reading a
/// **count** out of here would make the dimension refundable by anything that
/// prunes or rewrites zap rows, which is the exact shape that refinement
/// refuses.
///
/// Both keys are Nostr pubkeys, because that is the vocabulary the receipt and
/// this table share; the payee's Fauna actor id is resolved separately by the
/// caller.
pub fn has_zap_from_sender(
    conn: &Connection,
    target_pubkey: &str,
    sender_pubkey: &str,
) -> Result<bool> {
    let found: i64 = conn.query_row(
        "SELECT EXISTS(
             SELECT 1 FROM nostr_zaps
             WHERE target_pubkey = ?1 AND sender_pubkey = ?2
         )",
        [target_pubkey, sender_pubkey],
        |row| row.get(0),
    )?;
    Ok(found != 0)
}

/// One believed zap on a Fauna post, as the mechanism-independent tip surface
/// reads it (`docs/goal/behavior/monetization.md` § Tips).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PostTipRow {
    /// The tipper's Nostr pubkey as the receipt reported it, when it did.
    pub sender_pubkey: Option<String>,
    /// Hex actor id, when the sender's pubkey belongs to an account linked on
    /// **this** box. `None` for a tipper from outside — the tip still counts
    /// and still displays, it just displays unattributed.
    pub sender_actor_id: Option<String>,
    /// Millisatoshis, when the receipt carried a parseable `bolt11` amount.
    pub amount_msats: Option<i64>,
    /// Arrival on this box, seconds since epoch.
    pub received_at: i64,
}

/// Every believed zap targeting the Nostr events this box published for
/// `fauna_post_id` — the Fauna-addressed tip read.
///
/// **Why the join rather than [`get_zap_total`]'s direct key.** `nostr_zaps`
/// is keyed by *Nostr* event id, which is the mechanism's own identifier; a
/// Fauna app holding a `PostSummary` has only the Fauna post id. Resolving
/// through `nostr_event_map` here is what makes the tip surface addressable
/// the way every other post-scoped read is, and it is why nothing downstream
/// of this function needs to know NIP-57 exists.
///
/// Only `direction = 'outbound'` rows are joined: those are posts *this* box
/// published on behalf of a local author, which is the only case where a zap
/// naming the event can be a tip on a Fauna post of ours. Inbound rows map
/// foreign events we mirrored, whose zaps are not ours to total.
///
/// Every row here has already passed the ingest trust gate (`zap_ingest`), so
/// this read applies no further judgement — that is the ratified *at ingest,
/// never at read* discipline (§ Zap receipts — the trust model), and adding a
/// filter here would be the first step back toward re-judging at every
/// reader.
///
/// **Purchases are excluded** (`purchased_tier IS NULL`). A receipt that met
/// its target tier's asking price is a *sale*, not appreciation
/// (`monetization.md` § The asking price — under-threshold "stays a tip", so
/// a met one does not), and summing it into a post's tip total would report a
/// sale as a gift. This is not a filter that re-judges anything: the class was
/// decided once at ingest and written to the row, so this clause only reads
/// it — the *at ingest, never at read* discipline is intact.
///
/// Ordering is newest-first by arrival, with the zap event id as the
/// tiebreak so a page boundary cannot straddle two same-second rows
/// inconsistently.
pub fn list_tips_for_fauna_post(conn: &Connection, fauna_post_id: &str) -> Result<Vec<PostTipRow>> {
    let mut stmt = conn.prepare(
        // `COALESCE(inserted_at, created_at)`: `inserted_at` is the box's own
        // observation and the column every post-gate row carries; the
        // fallback covers only the nullable column's type, since the pre-gate
        // rows that lacked it were purged wholesale.
        "SELECT z.sender_pubkey,
                a.actor_id,
                z.amount_msats,
                COALESCE(z.inserted_at, z.created_at) AS received_at
           FROM nostr_zaps z
           JOIN nostr_event_map m
             ON m.nostr_event_id = z.target_event_id
            AND m.direction = 'outbound'
      LEFT JOIN nostr_accounts a
             ON a.nostr_pubkey = z.sender_pubkey
          WHERE m.fauna_post_id = ?1
            AND z.purchased_tier IS NULL
       ORDER BY received_at DESC, z.zap_event_id ASC",
    )?;
    let rows = stmt.query_map([fauna_post_id], |row| {
        Ok(PostTipRow {
            sender_pubkey: row.get(0)?,
            sender_actor_id: row.get(1)?,
            amount_msats: row.get(2)?,
            received_at: row.get(3)?,
        })
    })?;
    rows.collect::<rusqlite::Result<Vec<_>>>()
        .map_err(Into::into)
}

// ── Zap-signer designations (NIP-57 trust roots) ─────────────────

/// One designated zap signer (`nostr-zap-signer-item`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ZapSigner {
    pub id: i64,
    pub signer_pubkey: String,
    pub label: String,
    pub created_at: i64,
}

/// A nostr pubkey as it rests: 64 lowercase hex characters.
///
/// Applied at every write into the trust root. Two reasons, both
/// load-bearing: an unnormalized table would let one signer occupy two rows
/// (`UNIQUE` is case-sensitive while the verdict's comparison is not), and a
/// malformed designation is silently dead weight the payee believes they
/// made — it can never match a real `event.pubkey`, so the failure would
/// present as "my zaps are ignored" with a correct-looking roster.
pub fn normalize_signer_pubkey(pubkey: &str) -> Result<String> {
    let trimmed = pubkey.trim();
    if trimmed.len() != 64 || !trimmed.chars().all(|c| c.is_ascii_hexdigit()) {
        anyhow::bail!("signer pubkey must be 64 hex characters, got {:?}", trimmed);
    }
    Ok(trimmed.to_ascii_lowercase())
}

/// Designate a signer for `actor_id`. Idempotent: re-adding a designated
/// signer refreshes its label rather than erroring or duplicating.
pub fn add_zap_signer(
    conn: &Connection,
    actor_id: &str,
    signer_pubkey: &str,
    label: &str,
) -> Result<ZapSigner> {
    let normalized = normalize_signer_pubkey(signer_pubkey)?;
    let now = crate::db::now_epoch_secs();
    conn.execute(
        "INSERT INTO nostr_zap_signers (actor_id, signer_pubkey, label, created_at)
         VALUES (?1, ?2, ?3, ?4)
         ON CONFLICT(actor_id, signer_pubkey) DO UPDATE SET label = excluded.label",
        rusqlite::params![actor_id, normalized, label, now],
    )?;
    let row = conn.query_row(
        "SELECT id, signer_pubkey, label, created_at
         FROM nostr_zap_signers WHERE actor_id = ?1 AND signer_pubkey = ?2",
        rusqlite::params![actor_id, normalized],
        |row| {
            Ok(ZapSigner {
                id: row.get(0)?,
                signer_pubkey: row.get(1)?,
                label: row.get(2)?,
                created_at: row.get(3)?,
            })
        },
    )?;
    Ok(row)
}

/// Undesignate a signer. Returns whether a row was removed, so the caller
/// can report an unknown handle honestly instead of a silent success.
///
/// A malformed pubkey is *not* an error here: it simply matches nothing.
/// Refusing it would make a roster that somehow holds a malformed row
/// unremovable — the recoverability rule (`nest/common.md` § Client-state
/// recoverability) applies to the removal path even though the add path
/// refuses to create such a row.
pub fn remove_zap_signer(conn: &Connection, actor_id: &str, signer_pubkey: &str) -> Result<bool> {
    let normalized = normalize_signer_pubkey(signer_pubkey)
        .unwrap_or_else(|_| signer_pubkey.trim().to_ascii_lowercase());
    let removed = conn.execute(
        "DELETE FROM nostr_zap_signers WHERE actor_id = ?1 AND signer_pubkey = ?2",
        rusqlite::params![actor_id, normalized],
    )?;
    Ok(removed > 0)
}

/// The caller's designated signers, newest first.
pub fn list_zap_signers(conn: &Connection, actor_id: &str) -> Result<Vec<ZapSigner>> {
    let mut stmt = conn.prepare(
        "SELECT id, signer_pubkey, label, created_at
         FROM nostr_zap_signers WHERE actor_id = ?1
         ORDER BY created_at DESC, id DESC",
    )?;
    let rows = stmt
        .query_map([actor_id], |row| {
            Ok(ZapSigner {
                id: row.get(0)?,
                signer_pubkey: row.get(1)?,
                label: row.get(2)?,
                created_at: row.get(3)?,
            })
        })?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows)
}

/// The signers a **payee pubkey** has designated — the ingest-side read, used
/// by both zap ingress points.
///
/// Keyed by pubkey rather than actor because that is what a receipt names:
/// the join to `nostr_accounts` is the step that decides whose trust root
/// applies. A pubkey belonging to no local account resolves to the empty
/// set, which is the same answer as "designated nobody" and fails closed by
/// construction — there is no arm in which an unknown payee is believed.
pub fn trusted_zap_signers_for_pubkey(
    conn: &Connection,
    payee_pubkey: &str,
) -> Result<Vec<String>> {
    let normalized = payee_pubkey.trim().to_ascii_lowercase();
    let mut stmt = conn.prepare(
        "SELECT s.signer_pubkey
         FROM nostr_zap_signers s
         JOIN nostr_accounts a ON a.actor_id = s.actor_id
         WHERE LOWER(a.nostr_pubkey) = ?1",
    )?;
    let rows = stmt
        .query_map([normalized], |row| row.get::<_, String>(0))?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows)
}

// ── Badge CRUD ──────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct Badge {
    pub badge_id: String,
    pub badge_name: Option<String>,
    pub badge_image: Option<String>,
    pub created_at: i64,
}

pub fn insert_badge(
    conn: &Connection,
    badge_id: &str,
    badge_name: Option<&str>,
    badge_image: Option<&str>,
    awardee_pubkey: &str,
    created_at: i64,
) -> Result<()> {
    conn.execute(
        "INSERT OR IGNORE INTO nostr_badges
         (badge_id, badge_name, badge_image, awardee_pubkey, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5)",
        rusqlite::params![
            badge_id,
            badge_name,
            badge_image,
            awardee_pubkey,
            created_at
        ],
    )?;
    Ok(())
}

pub fn list_badges_for_pubkey(conn: &Connection, pubkey: &str) -> Result<Vec<Badge>> {
    let mut stmt = conn.prepare(
        "SELECT badge_id, badge_name, badge_image, created_at
         FROM nostr_badges WHERE awardee_pubkey = ?1 ORDER BY created_at DESC",
    )?;
    let rows = stmt
        .query_map([pubkey], |row| {
            Ok(Badge {
                badge_id: row.get(0)?,
                badge_name: row.get(1)?,
                badge_image: row.get(2)?,
                created_at: row.get(3)?,
            })
        })?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows)
}

// ── Phase-2 federation cursors (head-side) ──────────────────────
//
// The head's per-(actor, peer public box) push/pull cursors over the compound
// `(stored_at, id)` order (`nostr.md` § The bridging gate → Phase 2, R5).
// Head-side only — the keyless public box runs no worker and holds no row here.

/// The head's persisted federation cursors for one (actor, peer public box).
/// Each pair is the compound `(stored_at, id)` position the corresponding leg
/// has bridged **through** (strictly-after semantics on the next fetch). Absent
/// columns default to `(0, "")` — before every real row.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FederationCursors {
    pub push_stored_at: i64,
    pub push_id: String,
    pub pull_stored_at: i64,
    pub pull_id: String,
}

/// Read the head's push+pull cursors for `(actor_id, peer_nest_id)`; `None`
/// when no leg has run yet (the caller starts from `(0, "")`).
pub fn get_federation_cursors(
    conn: &Connection,
    actor_id: &str,
    peer_nest_id: &str,
) -> Result<Option<FederationCursors>> {
    let mut stmt = conn.prepare(
        "SELECT push_stored_at, push_id, pull_stored_at, pull_id
         FROM nostr_federation_cursors WHERE actor_id = ?1 AND peer_nest_id = ?2",
    )?;
    let row = stmt.query_row(rusqlite::params![actor_id, peer_nest_id], |r| {
        Ok(FederationCursors {
            push_stored_at: r.get(0)?,
            push_id: r.get(1)?,
            pull_stored_at: r.get(2)?,
            pull_id: r.get(3)?,
        })
    });
    match row {
        Ok(c) => Ok(Some(c)),
        Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
        Err(e) => Err(e.into()),
    }
}

/// Advance (upsert) the **push** cursor for `(actor_id, peer_nest_id)`, leaving
/// the pull cursor untouched. Called only after a successful push reply.
pub fn set_federation_push_cursor(
    conn: &Connection,
    actor_id: &str,
    peer_nest_id: &str,
    stored_at: i64,
    id: &str,
) -> Result<()> {
    conn.execute(
        "INSERT INTO nostr_federation_cursors
             (actor_id, peer_nest_id, push_stored_at, push_id)
         VALUES (?1, ?2, ?3, ?4)
         ON CONFLICT(actor_id, peer_nest_id)
             DO UPDATE SET push_stored_at = ?3, push_id = ?4",
        rusqlite::params![actor_id, peer_nest_id, stored_at, id],
    )?;
    Ok(())
}

/// Advance (upsert) the **pull** cursor for `(actor_id, peer_nest_id)`, leaving
/// the push cursor untouched. Called only after a successful pull ingest.
pub fn set_federation_pull_cursor(
    conn: &Connection,
    actor_id: &str,
    peer_nest_id: &str,
    stored_at: i64,
    id: &str,
) -> Result<()> {
    conn.execute(
        "INSERT INTO nostr_federation_cursors
             (actor_id, peer_nest_id, pull_stored_at, pull_id)
         VALUES (?1, ?2, ?3, ?4)
         ON CONFLICT(actor_id, peer_nest_id)
             DO UPDATE SET pull_stored_at = ?3, pull_id = ?4",
        rusqlite::params![actor_id, peer_nest_id, stored_at, id],
    )?;
    Ok(())
}

// ── Row mappers ─────────────────────────────────────────────────

fn row_to_account(row: &rusqlite::Row<'_>) -> rusqlite::Result<NostrAccount> {
    Ok(NostrAccount {
        actor_id: row.get(0)?,
        nostr_pubkey: row.get(1)?,
        signing_mode: row.get(2)?,
        encrypted_privkey: row.get(3)?,
        nip46_bunker_url: row.get(4)?,
        relay_list: row.get(5)?,
        expose_content: row.get::<_, i32>(6)? != 0,
        auto_publish: row.get::<_, i32>(7)? != 0,
        publish_replies: row.get::<_, i32>(8)? != 0,
        publish_reactions: row.get::<_, i32>(9)? != 0,
        inbound_to_feed: row.get::<_, i32>(10)? != 0,
        created_at: row.get(11)?,
        updated_at: row.get(12)?,
    })
}

fn row_to_event_map(row: &rusqlite::Row<'_>) -> rusqlite::Result<EventMapEntry> {
    Ok(EventMapEntry {
        fauna_post_id: row.get(0)?,
        nostr_event_id: row.get(1)?,
        nostr_pubkey: row.get(2)?,
        direction: row.get(3)?,
        published_at: row.get(4)?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_conn() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch("PRAGMA foreign_keys = ON;").unwrap();
        // The nest genesis first: the bridge-search triggers reach its
        // `content_fts` tables.
        crate::db::migrations::run_migrations(&conn).unwrap();
        crate::nostr::apply_schema(&conn).unwrap();
        conn
    }

    #[test]
    fn link_and_get_account() {
        let conn = test_conn();
        link_account(
            &conn,
            "actor1",
            "npub_hex",
            "generated",
            Some(b"secret"),
            None,
            None,
        )
        .unwrap();
        let acct = get_account(&conn, "actor1").unwrap().unwrap();
        assert_eq!(acct.nostr_pubkey, "npub_hex");
        assert_eq!(acct.signing_mode, "generated");
        assert!(acct.encrypted_privkey.is_some());
    }

    /// The table is UNIQUE on `nostr_pubkey`, so an upsert naming a
    /// pubkey another actor holds would DELETE that actor's row — deposited
    /// nsec included, possibly its only copy. The writer refuses instead.
    #[test]
    fn link_refuses_a_pubkey_another_actor_holds() {
        let conn = test_conn();
        link_account(
            &conn,
            "victim",
            "pk1",
            "generated",
            Some(b"sealed"),
            None,
            None,
        )
        .unwrap();

        let err = link_account(&conn, "attacker", "pk1", "nip07", None, None, None).unwrap_err();
        assert!(matches!(err, LinkAccountError::PubkeyHeld), "{err:?}");

        let victim = get_account(&conn, "victim")
            .unwrap()
            .expect("victim survives");
        assert_eq!(victim.encrypted_privkey.as_deref(), Some(&b"sealed"[..]));
        assert!(get_account(&conn, "attacker").unwrap().is_none());
    }

    /// A second link for an actor that already has a row is refused too —
    /// never a silent overwrite of its mode, key, or settings.
    #[test]
    fn link_refuses_an_actor_already_linked() {
        let conn = test_conn();
        link_account(
            &conn,
            "actor1",
            "pk1",
            "generated",
            Some(b"sealed"),
            None,
            None,
        )
        .unwrap();

        let err = link_account(&conn, "actor1", "pk2", "nip07", None, None, None).unwrap_err();
        assert!(matches!(err, LinkAccountError::ActorLinked), "{err:?}");
        let acct = get_account(&conn, "actor1").unwrap().unwrap();
        assert_eq!(acct.nostr_pubkey, "pk1");
    }

    #[test]
    fn unlink_account_removes() {
        let conn = test_conn();
        link_account(&conn, "actor1", "npub_hex", "generated", None, None, None).unwrap();
        unlink_account(&conn, "actor1").unwrap();
        assert!(get_account(&conn, "actor1").unwrap().is_none());
    }

    #[test]
    fn unlink_account_cascades_bunker_rows() {
        // nostr.md § signer: unlinking revokes every bunker connection (the
        // key-less cascade) and drops the signer keypair (nest-minted,
        // re-minted on a future first invite — recreatable, no user data).
        let conn = test_conn();
        link_account(&conn, "actor1", "npub_hex", "generated", None, None, None).unwrap();
        conn.execute(
            "INSERT INTO nostr_bunker_signers (actor_id, signer_pubkey, encrypted_privkey, created_at)
             VALUES ('actor1', 'spk1', x'00', 1)",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO nostr_bunker_apps
             (actor_id, app_pubkey, label, secret_hash, status, created_at, expires_at)
             VALUES ('actor1', 'apk1', 'my app', NULL, 'active', 1, 999999),
                    ('actor1', NULL, '', x'11', 'pending', 1, 999999)",
            [],
        )
        .unwrap();

        unlink_account(&conn, "actor1").unwrap();

        let signers: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM nostr_bunker_signers WHERE actor_id='actor1'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(signers, 0, "signer row must be dropped on unlink");
        let non_revoked: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM nostr_bunker_apps WHERE actor_id='actor1' AND status != 'revoked'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(non_revoked, 0, "every connection must be revoked on unlink");
        let live_secrets: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM nostr_bunker_apps WHERE actor_id='actor1' AND secret_hash IS NOT NULL",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(
            live_secrets, 0,
            "no outstanding invite secret survives unlink"
        );
    }

    #[test]
    fn get_by_pubkey() {
        let conn = test_conn();
        link_account(&conn, "actor1", "pk1", "imported", None, None, None).unwrap();
        let acct = get_account_by_pubkey(&conn, "pk1").unwrap().unwrap();
        assert_eq!(acct.actor_id, "actor1");
    }

    #[test]
    fn settings_update() {
        let conn = test_conn();
        link_account(&conn, "actor1", "pk1", "generated", None, None, None).unwrap();
        update_settings(
            &conn,
            "actor1",
            &NostrSettings {
                expose_content: Some(true),
                auto_publish: Some(true),
                ..Default::default()
            },
        )
        .unwrap();
        let acct = get_account(&conn, "actor1").unwrap().unwrap();
        assert!(acct.expose_content);
        assert!(acct.auto_publish);
    }

    #[test]
    fn follow_crud() {
        let conn = test_conn();
        add_follow(&conn, "actor1", "npub1", Some("alice"), None).unwrap();
        add_follow(&conn, "actor1", "npub2", None, Some("[\"wss://r1\"]")).unwrap();

        let follows = list_follows(&conn, "actor1").unwrap();
        assert_eq!(follows.len(), 2);

        remove_follow(&conn, "actor1", "npub1").unwrap();
        let follows = list_follows(&conn, "actor1").unwrap();
        assert_eq!(follows.len(), 1);
        assert_eq!(follows[0].nostr_pubkey, "npub2");
    }

    #[test]
    fn event_map_roundtrip() {
        let conn = test_conn();
        insert_event_map(&conn, "fauna1", "nostr1", "pk1", "outbound").unwrap();

        let by_nostr = get_event_by_nostr_id(&conn, "nostr1").unwrap().unwrap();
        assert_eq!(by_nostr.fauna_post_id, "fauna1");
        assert_eq!(by_nostr.direction, "outbound");

        let by_fauna = get_event_by_fauna_id(&conn, "fauna1").unwrap().unwrap();
        assert_eq!(by_fauna.nostr_event_id, "nostr1");

        assert!(get_event_by_nostr_id(&conn, "missing").unwrap().is_none());
    }

    #[test]
    fn relay_state_upsert() {
        let conn = test_conn();
        upsert_relay_state(&conn, "wss://relay1", "pk1", 1000).unwrap();
        assert_eq!(
            get_relay_state(&conn, "wss://relay1", "pk1").unwrap(),
            Some(1000)
        );

        upsert_relay_state(&conn, "wss://relay1", "pk1", 2000).unwrap();
        assert_eq!(
            get_relay_state(&conn, "wss://relay1", "pk1").unwrap(),
            Some(2000)
        );

        assert!(
            get_relay_state(&conn, "wss://missing", "pk1")
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn apply_schema_is_idempotent() {
        let conn = test_conn();
        crate::nostr::apply_schema(&conn).unwrap();
    }

    /// The mechanism that replaced the hand-written `ALTER` chain: every
    /// column the genesis block declares that a long-lived database lacks is
    /// added back by `apply_schema`'s additive reconcile. Drops every
    /// nullable or defaulted column SQLite lets go of (not a key, not
    /// indexed), across every table, then asserts the shape comes back whole.
    #[test]
    fn apply_schema_reconciles_every_droppable_column() {
        let conn = test_conn();
        let dropped = crate::bridge_schema::drop_additive_columns_and_reapply(
            &conn,
            CREATE_TABLES_SQL,
            crate::nostr::apply_schema,
        );
        assert!(
            dropped > 10,
            "the probe must actually drop columns ({dropped})"
        );
    }

    #[test]
    fn insert_zap_and_get_total() {
        let conn = test_conn();
        insert_zap(
            &conn,
            "zap1",
            Some("event1"),
            "pk_target",
            Some("pk_sender"),
            Some(1000),
            1_700_000_000,
            None,
        )
        .unwrap();
        let (total, count) = get_zap_total(&conn, "event1").unwrap();
        assert_eq!(total, 1000);
        assert_eq!(count, 1);
    }

    #[test]
    fn insert_multiple_zaps_aggregate() {
        let conn = test_conn();
        insert_zap(
            &conn,
            "zap1",
            Some("event1"),
            "pk_target",
            Some("pk_sender1"),
            Some(1000),
            1_700_000_000,
            None,
        )
        .unwrap();
        insert_zap(
            &conn,
            "zap2",
            Some("event1"),
            "pk_target",
            Some("pk_sender2"),
            Some(2500),
            1_700_000_001,
            None,
        )
        .unwrap();
        insert_zap(
            &conn,
            "zap3",
            Some("event1"),
            "pk_target",
            None,
            Some(500),
            1_700_000_002,
            None,
        )
        .unwrap();
        let (total, count) = get_zap_total(&conn, "event1").unwrap();
        assert_eq!(total, 4000);
        assert_eq!(count, 3);
    }

    #[test]
    fn get_zap_total_missing_event_returns_zero() {
        let conn = test_conn();
        let (total, count) = get_zap_total(&conn, "nonexistent_event").unwrap();
        assert_eq!(total, 0);
        assert_eq!(count, 0);
    }

    #[test]
    fn insert_zap_without_target_event() {
        let conn = test_conn();
        // Zap targeting a pubkey without a specific event (target_event_id = None)
        insert_zap(
            &conn,
            "zap1",
            None,
            "pk_target",
            Some("pk_sender"),
            Some(5000),
            1_700_000_000,
            None,
        )
        .unwrap();
        // This zap doesn't show up in event totals (no event ID)
        let (total, count) = get_zap_total(&conn, "event1").unwrap();
        assert_eq!(total, 0);
        assert_eq!(count, 0);
    }

    #[test]
    fn insert_zap_stamps_arrival_time() {
        // `created_at` is the receipt's own, sender-controlled claim;
        // `inserted_at` is this box's arrival fact, stamped at the DB layer.
        let conn = test_conn();
        insert_zap(
            &conn,
            "zap1",
            Some("event1"),
            "pk_target",
            None,
            Some(100),
            1_700_000_000,
            None,
        )
        .unwrap();
        let inserted_at: Option<i64> = conn
            .query_row(
                "SELECT inserted_at FROM nostr_zaps WHERE zap_event_id = 'zap1'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert!(
            inserted_at.is_some_and(|t| t > 1_700_000_000),
            "arrival time is stamped from the box's clock, not the receipt: {inserted_at:?}"
        );
    }

    #[test]
    fn insert_zap_duplicate_ignored() {
        let conn = test_conn();
        insert_zap(
            &conn,
            "zap1",
            Some("event1"),
            "pk_target",
            None,
            Some(1000),
            1_700_000_000,
            None,
        )
        .unwrap();
        // Inserting same event ID again should be silently ignored
        insert_zap(
            &conn,
            "zap1",
            Some("event1"),
            "pk_target",
            None,
            Some(9999),
            1_700_000_000,
            None,
        )
        .unwrap();
        let (total, count) = get_zap_total(&conn, "event1").unwrap();
        assert_eq!(total, 1000); // original amount, not 9999
        assert_eq!(count, 1);
    }

    #[test]
    fn insert_badge_and_list() {
        let conn = test_conn();
        insert_badge(
            &conn,
            "30009:pk1:top-contributor",
            Some("Top Contributor"),
            Some("https://example.com/badge.png"),
            "awardee1",
            1_700_000_000,
        )
        .unwrap();

        let badges = list_badges_for_pubkey(&conn, "awardee1").unwrap();
        assert_eq!(badges.len(), 1);
        assert_eq!(badges[0].badge_id, "30009:pk1:top-contributor");
        assert_eq!(badges[0].badge_name, Some("Top Contributor".into()));
        assert_eq!(
            badges[0].badge_image,
            Some("https://example.com/badge.png".into())
        );
        assert_eq!(badges[0].created_at, 1_700_000_000);
    }

    #[test]
    fn list_badges_empty_for_unknown_pubkey() {
        let conn = test_conn();
        let badges = list_badges_for_pubkey(&conn, "unknown_pk").unwrap();
        assert!(badges.is_empty());
    }

    #[test]
    fn insert_badge_duplicate_ignored() {
        let conn = test_conn();
        insert_badge(
            &conn,
            "30009:pk1:badge",
            Some("Badge"),
            None,
            "awardee1",
            1_700_000_000,
        )
        .unwrap();
        // Insert same badge_id + awardee_pubkey again — should be ignored
        insert_badge(
            &conn,
            "30009:pk1:badge",
            Some("Badge Updated"),
            Some("https://img.png"),
            "awardee1",
            1_700_000_001,
        )
        .unwrap();

        let badges = list_badges_for_pubkey(&conn, "awardee1").unwrap();
        assert_eq!(badges.len(), 1);
        // Original name retained (INSERT OR IGNORE)
        assert_eq!(badges[0].badge_name, Some("Badge".into()));
    }
}
