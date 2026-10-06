use std::io::Read;
use std::sync::Arc;

use fauna_core::identity::ActorKeypair;
use fauna_nest::backup::service::BackupService;
use fauna_nest::db::CacheDb;
use fauna_nest::db::actor_tables::{ACTOR_TABLES, ActorKey, ActorTable, Export};
use fauna_nest::token_store::TokenStore;

async fn start_server() -> (String, Arc<fauna_nest::routes::AppState>) {
    let db = Arc::new(CacheDb::open_in_memory().unwrap());
    let dir = tempfile::tempdir().unwrap();
    let dir_path = dir.path().to_path_buf();
    std::mem::forget(dir);
    let backup_svc =
        Arc::new(BackupService::new(db.clone(), None, false, dir_path.clone(), None).unwrap());
    let tokens = Arc::new(TokenStore::new());
    let state = Arc::new(fauna_nest::routes::AppState {
        backup_service: Some(backup_svc.clone()),
        auth: fauna_nest::state::AuthState {
            token_store: tokens.clone(),
            ..Default::default()
        },
        ..fauna_nest::routes::AppState::for_test(db.clone())
    });

    let app = fauna_nest::build_router(state.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.ok();
    });

    (format!("http://{addr}"), state)
}

#[tokio::test]
async fn export_requires_auth() {
    let (base, _state) = start_server().await;
    let client = reqwest::Client::new();

    let resp = client
        .get(format!("{base}/api/v1/export"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 401);
}

#[tokio::test]
async fn export_returns_valid_zip() {
    let (base, state) = start_server().await;
    let kp = ActorKeypair::generate();
    let actor_id = kp.actor_id();
    let actor_hex = hex::encode(actor_id.0);

    // Create user and get bearer token
    state
        .db
        .create_user(&actor_id.0, "free", "test-export")
        .await
        .unwrap();
    let token = state.auth.token_store.insert(actor_id, 3600).await;

    // Add a confirmed contact
    let peer = ActorKeypair::generate();
    state
        .db
        .upsert_contact(&actor_id.0, &peer.actor_id().0, "confirmed")
        .await
        .unwrap();

    // Add an inbox message
    state
        .db
        .push_inbox(&actor_id.0, b"test inbox payload", None)
        .await
        .unwrap();

    // GET /api/v1/export with bearer token
    let client = reqwest::Client::new();
    let resp = client
        .get(format!("{base}/api/v1/export"))
        .header("authorization", format!("Bearer {token}"))
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status(), 200);
    assert_eq!(
        resp.headers()
            .get("content-type")
            .unwrap()
            .to_str()
            .unwrap(),
        "application/zip"
    );

    let zip_bytes = resp.bytes().await.unwrap();
    let cursor = std::io::Cursor::new(zip_bytes.as_ref());
    let mut archive = zip::ZipArchive::new(cursor).expect("response should be a valid zip");

    // Verify expected files exist
    let file_names: Vec<String> = (0..archive.len())
        .map(|i| archive.by_index(i).unwrap().name().to_string())
        .collect();

    assert!(
        file_names.contains(&"export/manifest.json".to_string()),
        "zip should contain export/manifest.json, got: {file_names:?}"
    );
    assert!(
        file_names.contains(&"export/profile.json".to_string()),
        "zip should contain export/profile.json, got: {file_names:?}"
    );
    assert!(
        file_names.contains(&"export/contacts.json".to_string()),
        "zip should contain export/contacts.json, got: {file_names:?}"
    );

    // Verify manifest.json content
    {
        let mut file = archive.by_name("export/manifest.json").unwrap();
        let mut s = String::new();
        file.read_to_string(&mut s).unwrap();
        let manifest: serde_json::Value = serde_json::from_str(&s).unwrap();
        assert_eq!(manifest["format"], 1);
        assert_eq!(manifest["actor_id"], actor_hex);
    }

    // Verify contacts.json content
    {
        let mut file = archive.by_name("export/contacts.json").unwrap();
        let mut s = String::new();
        file.read_to_string(&mut s).unwrap();
        let contacts: serde_json::Value = serde_json::from_str(&s).unwrap();
        let arr = contacts.as_array().expect("contacts should be an array");
        assert_eq!(arr.len(), 1);
        assert_eq!(arr[0]["status"], "confirmed");
    }

    // Verify at least one inbox entry exists
    let inbox_entries: Vec<&String> = file_names
        .iter()
        .filter(|name| name.starts_with("export/inbox/"))
        .collect();
    assert!(
        !inbox_entries.is_empty(),
        "zip should contain at least one export/inbox/ entry"
    );
}

/// A live bearer whose actor has no `users` row (never registered, or deleted
/// since the mint) has no standing, so the one bearer validator refuses it at
/// the door — `403`, the same answer this route gives a suspended or locked-out
/// account's surviving bearer — before the handler's own lookup could say
/// `404`.
#[tokio::test]
async fn export_by_a_bearer_whose_user_is_gone_is_refused() {
    let (base, state) = start_server().await;

    // Generate a keypair and insert a token, but do NOT create the user
    let kp = ActorKeypair::generate();
    let actor_id = kp.actor_id();
    let token = state.auth.token_store.insert(actor_id, 3600).await;

    let client = reqwest::Client::new();
    let resp = client
        .get(format!("{base}/api/v1/export"))
        .header("authorization", format!("Bearer {token}"))
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status(), 403);
}

// ---------------------------------------------------------------------------
// The export confidentiality axis (`ACTOR_TABLES`' fourth axis, ratified
// 2026-08-15) — `account-data-plane.md` § Nest-side requirements item 1.
// ---------------------------------------------------------------------------

/// Whether this build's schema actually has `table`.
///
/// The registry includes tables that exist only under the `nostr`/`bluesky`/
/// `activitypub` features, none of which is in `bins/fauna-nest`'s default
/// feature set — so "declared in the registry" and "present on this nest" are
/// different questions, and both tests below have to ask the second one.
async fn table_exists(db: &CacheDb, table: &str) -> bool {
    let conn = db.conn().await;
    conn.query_row(
        "SELECT 1 FROM sqlite_master WHERE type='table' AND name=?1",
        rusqlite::params![table],
        |_| Ok(true),
    )
    .ok()
    .unwrap_or(false)
}

/// Seeds exactly one row into `entry`'s table, with the actor column bound in
/// the entry's own [`ActorKey`] spelling and `secret` written into every TEXT
/// and BLOB column. Returns `false` when this build's schema has no such table.
///
/// Foreign keys are disabled for the seed: these tests assert what the *export*
/// does with a row, and a referential-integrity refusal would silently reduce
/// the walk's coverage to whichever tables happen to have no parents.
async fn seed_secret_row(db: &CacheDb, entry: &ActorTable, actor: &[u8; 32], secret: &str) -> bool {
    if !table_exists(db, entry.table).await {
        return false;
    }
    let conn = db.conn().await;
    conn.execute_batch("PRAGMA foreign_keys = OFF;").unwrap();

    let cols: Vec<(String, String)> = {
        let mut stmt = conn
            .prepare(&format!("PRAGMA table_info({})", entry.table))
            .unwrap();
        let rows = stmt
            .query_map([], |r| Ok((r.get::<_, String>(1)?, r.get::<_, String>(2)?)))
            .unwrap();
        rows.map(|r| r.unwrap()).collect()
    };
    assert!(
        !cols.is_empty(),
        "{} exists but reports no columns",
        entry.table
    );

    let mut names: Vec<String> = Vec::new();
    let mut values: Vec<Box<dyn rusqlite::ToSql>> = Vec::new();
    for (name, decl) in &cols {
        names.push(name.clone());
        if name == entry.column {
            match entry.key {
                ActorKey::Blob => values.push(Box::new(actor.to_vec())),
                ActorKey::Hex => values.push(Box::new(hex::encode(actor))),
            }
            continue;
        }
        let d = decl.to_ascii_uppercase();
        if d.contains("BLOB") {
            values.push(Box::new(secret.as_bytes().to_vec()));
        } else if d.contains("CHAR") || d.contains("TEXT") || d.contains("CLOB") || d.is_empty() {
            values.push(Box::new(secret.to_string()));
        } else {
            values.push(Box::new(0_i64));
        }
    }
    let placeholders: Vec<String> = (1..=names.len()).map(|i| format!("?{i}")).collect();
    let sql = format!(
        "INSERT OR REPLACE INTO {} ({}) VALUES ({})",
        entry.table,
        names.join(", "),
        placeholders.join(", ")
    );
    let params: Vec<&dyn rusqlite::ToSql> = values.iter().map(|v| v.as_ref()).collect();
    conn.execute(&sql, params.as_slice())
        .unwrap_or_else(|e| panic!("seed {}: {e}", entry.table));
    true
}

/// Every archive entry's decompressed bytes, keyed by path.
fn read_archive(zip_bytes: &[u8]) -> Vec<(String, Vec<u8>)> {
    let cursor = std::io::Cursor::new(zip_bytes);
    let mut archive = zip::ZipArchive::new(cursor).expect("response should be a valid zip");
    let mut out = Vec::new();
    for i in 0..archive.len() {
        let mut f = archive.by_index(i).unwrap();
        let name = f.name().to_string();
        let mut buf = Vec::new();
        f.read_to_end(&mut buf).unwrap();
        out.push((name, buf));
    }
    out
}

async fn export_zip(base: &str, token: &str) -> Vec<u8> {
    let client = reqwest::Client::new();
    let resp = client
        .get(format!("{base}/api/v1/export"))
        .header("authorization", format!("Bearer {token}"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    resp.bytes().await.unwrap().to_vec()
}

/// Rule 4: *"while any table is `Unreviewed`, the export manifest says so
/// (additive JSON fields on `manifest.json`, format stays 1: a partiality flag
/// + the unreviewed table names), and withheld tables are likewise declared by
/// name and reason class"* — `account-data-plane.md` § Nest-side requirements
/// → *(4) The export declares its own partiality*.
///
/// The lists are scoped to tables this build's schema actually has: the
/// declaration's purpose is telling the owner **what this nest holds but does
/// not export**, and a nest built without the bridge features holds none of
/// their tables at all.
#[tokio::test]
async fn export_declares_its_own_partiality() {
    let (base, state) = start_server().await;
    let kp = ActorKeypair::generate();
    let actor_id = kp.actor_id();
    state
        .db
        .create_user(&actor_id.0, "free", "coverage")
        .await
        .unwrap();
    let token = state.auth.token_store.insert(actor_id, 3600).await;

    let zip_bytes = export_zip(&base, &token).await;
    let entries = read_archive(&zip_bytes);
    let (_, manifest_bytes) = entries
        .iter()
        .find(|(n, _)| n == "export/manifest.json")
        .expect("manifest.json");
    let manifest: serde_json::Value = serde_json::from_slice(manifest_bytes).unwrap();

    // Additive — the format number does not move.
    assert_eq!(manifest["format"], 1);

    let coverage = &manifest["coverage"];
    assert!(
        coverage.is_object(),
        "manifest must carry a coverage declaration, got: {manifest}"
    );
    // ⚠ **Flipped deliberately when the last verdict
    // landed and the backlog reached ZERO.** This asserted `partial == true`
    // for as long as the backlog existed; the assertion could not simply be
    // deleted, because the flag is rule 4's own declaration and its FALSE case
    // is now the one that ships to every user.
    //
    // **What `partial: false` means, and what it does NOT.**
    // `ActorExportSet::partial()` is defined as "any table this nest holds is
    // still `Export::Unreviewed`" — so false says *no open judgments remain*,
    // NOT "everything you have is in this archive". `withheld_tables` below
    // goes on naming, by reason class, every plane the nest holds and does not
    // export, and that list is what tells the owner where the gaps are.
    // Reading this flag as a completeness claim would be wrong in a way the
    // archive itself contradicts one field further down.
    assert_eq!(
        coverage["partial"], false,
        "every registry table now carries a ruled export verdict, \
         so no OPEN JUDGMENT remains and the backlog flag is false. It is not a completeness \
         claim — `withheld_tables` still declares what the nest holds and does not export"
    );

    // The declared backlog is exactly this nest's Unreviewed set — empty today,
    // and the assertion is kept rather than dropped because it is what would
    // notice a table rejoining the backlog (which `Export` being a required
    // field makes a compile error, but a `Unreviewed` verdict written by hand
    // would still pass compilation).
    let mut expected_unreviewed: Vec<&str> = Vec::new();
    for e in ACTOR_TABLES {
        if matches!(e.export, Export::Unreviewed) && table_exists(&state.db, e.table).await {
            expected_unreviewed.push(e.table);
        }
    }
    expected_unreviewed.sort_unstable();
    expected_unreviewed.dedup();
    let declared: Vec<String> = coverage["unreviewed_tables"]
        .as_array()
        .expect("unreviewed_tables array")
        .iter()
        .map(|v| v.as_str().unwrap().to_string())
        .collect();
    assert_eq!(
        declared, expected_unreviewed,
        "the manifest must declare exactly the current backlog"
    );
    assert!(
        declared.is_empty(),
        "the backlog reached zero; a non-empty declaration here means a \
         table rejoined it, which the ratchet is supposed to refuse: {declared:?}"
    );

    // Withheld tables: declared by name AND reason class.
    //
    // ⚠ All THREE reason classes are asserted, not just `secret`. Rule 4 says
    // withheld tables are declared "by name and reason class", and the point of
    // the class is telling the owner *why* a plane is absent — "a key we must
    // not hand you" and "a cache you already have" are different answers. Only
    // `secret` had a live example until the axis's first `WithheldDerived`
    // and `WithheldOperational` verdicts landed, so until then
    // two thirds of the declaration travelled untested through the real
    // endpoint.
    let withheld = coverage["withheld_tables"]
        .as_array()
        .expect("withheld_tables array");
    let class_of = |table: &str| -> String {
        withheld
            .iter()
            .find(|w| w["table"] == table)
            .unwrap_or_else(|| panic!("{table} is a withheld verdict and must be declared by name"))
            ["reason_class"]
            .as_str()
            .expect("reason_class string")
            .to_string()
    };
    assert_eq!(class_of("recovery_escrow"), "secret");
    assert_eq!(class_of("spam_model_holder_copies"), "derived");
    assert_eq!(class_of("foreign_recovery_heads"), "operational");

    // Every withheld declaration carries a class — an empty or missing one
    // would satisfy the three lookups above while telling the owner nothing.
    for w in withheld {
        let table = w["table"].as_str().expect("withheld table name");
        let class = w["reason_class"].as_str().unwrap_or("");
        assert!(
            matches!(class, "secret" | "derived" | "operational"),
            "{table} is declared withheld with reason_class {class:?}, which is not one \
             of the three the vocabulary defines"
        );
    }
}

/// The tables whose rows are credential, key, or escrow material — named here
/// rather than read off the registry's own verdicts.
///
/// **The separation is the whole point, and it was found by mutation, not by
/// design.** The first version of the belt below asked `ACTOR_TABLES` which
/// tables were `Export::WithheldSecret` and checked exactly those, so flipping
/// `recovery_escrow` to `Export::Verbatim` — the leak the belt exists to catch —
/// removed the table from the belt's own worklist and the test stayed green
/// while the escrow blob rode out in the archive. A roster kept deliberately
/// apart from the thing it audits is the same shape row 129 landed for the
/// mail-enable coverage floor.
///
/// Adding a `WithheldSecret` verdict without adding it here is caught by
/// `the_secret_roster_covers_every_withheld_secret_verdict` below; the reverse
/// — demoting one of these — is caught by the belt itself.
const SECRET_BEARING_TABLES: &[&str] = &[
    "atproto_app_credentials",
    "atproto_authoring_keys",
    "atproto_identity_key_blobs",
    "atproto_sessions",
    "bridge_mls_snapshot_blobs",
    "bridge_webdav_keys_blobs",
    "bridge_wrapped_mls_blobs",
    "bridge_wrapped_submission_tokens",
    "current_key_blobs",
    "eviction_tokens",
    "generation_escrow_wraps",
    "namespace_entries",
    "nest_backup_keys",
    "nostr_accounts",
    "nostr_bunker_signers",
    "recovery_escrow",
];

/// The roster is a superset check in one direction: every table the registry
/// rules `WithheldSecret` must be named above, so a new secret-bearing table
/// cannot join the registry and quietly sit outside the belt's worklist.
#[test]
fn the_secret_roster_covers_every_withheld_secret_verdict() {
    let missing: Vec<&str> = ACTOR_TABLES
        .iter()
        .filter(|e| matches!(e.export, Export::WithheldSecret(_)))
        .map(|e| e.table)
        .filter(|t| !SECRET_BEARING_TABLES.contains(t))
        .collect();
    assert!(
        missing.is_empty(),
        "these tables are Export::WithheldSecret but are not on the belt's \
         roster, so nothing seeds them and nothing would notice them leaking: \
         {missing:?}"
    );
}

/// The runtime belt behind the declaration: a secret-bearing table's rows never
/// reach the archive, whatever the registry currently says about them.
///
/// `WithheldSecret` exists because the endpoint accepts an **eviction export
/// token** — the weakest credential it takes — so an export must never mint a
/// new resting place for a secret (`actor_tables.rs:238-242`).
#[tokio::test]
async fn export_never_carries_a_secret_bearing_table() {
    let (base, state) = start_server().await;
    let kp = ActorKeypair::generate();
    let actor_id = kp.actor_id();
    state
        .db
        .create_user(&actor_id.0, "free", "withheld")
        .await
        .unwrap();
    let token = state.auth.token_store.insert(actor_id, 3600).await;

    const SENTINEL: &str = "SEEDED-SECRET-must-never-be-exported";
    let mut seeded: Vec<&str> = Vec::new();
    for table in SECRET_BEARING_TABLES {
        let entry = ACTOR_TABLES
            .iter()
            .find(|e| &e.table == table)
            .unwrap_or_else(|| panic!("{table} is on the secret roster but not in ACTOR_TABLES"));
        if seed_secret_row(&state.db, entry, &actor_id.0, SENTINEL).await {
            seeded.push(table);
        }
    }
    // ⚠ PER-TABLE COVERAGE, not a non-emptiness check (shape,
    // applied here when the roster grew from 5 to 16).
    // The original bar was `!seeded.is_empty()`, which any ONE seedable table
    // satisfies — so every table added afterwards could sit on the roster
    // unseeded and unaudited while the test stayed green, and the roster would
    // read as coverage it did not have.
    //
    // A *count* floor does not close that: an unseedable addition leaves the
    // seeded count unchanged and passes just as quietly. So the skipped set is
    // declared BY NAME instead, and every roster table not named here must
    // have been seeded. Adding a table nothing can seed then reds immediately,
    // which is the whole point of the roster.
    //
    // These two live in `nostr/db.rs` behind the `nostr` feature, absent from
    // `bins/fauna-nest`'s default `["store-safe", "payments", "zaps"]`. The
    // atproto tables are NOT in this class despite a long-standing comment
    // here saying three of the original five were: they are created
    // unconditionally in `db/migrations.rs`, so they are seeded and audited.
    const UNSEEDABLE_IN_DEFAULT_BUILD: &[&str] = &["nostr_accounts", "nostr_bunker_signers"];
    let mut skipped: Vec<&str> = SECRET_BEARING_TABLES
        .iter()
        .copied()
        .filter(|t| !seeded.contains(t))
        .collect();
    skipped.sort_unstable();
    assert_eq!(
        skipped,
        UNSEEDABLE_IN_DEFAULT_BUILD,
        "the set of roster tables this build cannot seed has changed. Nothing \
         seeds these, so nothing here would notice them leaking: either a \
         migration moved a table behind a feature (name it above, deliberately) \
         or a feature-gated table became unconditional (drop it from the list). \
         Seeded {} of {}",
        seeded.len(),
        SECRET_BEARING_TABLES.len(),
    );

    let zip_bytes = export_zip(&base, &token).await;
    let entries = read_archive(&zip_bytes);

    for table in &seeded {
        let path = format!("export/tables/{table}.ndjson");
        assert!(
            !entries.iter().any(|(n, _)| n == &path),
            "{path} is in the archive, but {table} holds credential/key/escrow material"
        );
    }

    // Belt: the seeded bytes appear nowhere, in any encoding the export uses.
    let hex_sentinel = hex::encode(SENTINEL.as_bytes());
    for (name, body) in &entries {
        let haystack = String::from_utf8_lossy(body);
        assert!(
            !haystack.contains(SENTINEL),
            "{name} carries the seeded secret verbatim"
        );
        assert!(
            !haystack.contains(&hex_sentinel),
            "{name} carries the seeded secret hex-encoded"
        );
    }
}

/// A `Verbatim` verdict on a **real registry table** puts that actor's rows in
/// the archive, every column of them.
///
/// The unit test `a_verbatim_verdict_emits_every_column` proves the mechanism
/// against a synthetic registry slice, which was all the skeleton pass could do
/// — it left zero `Verbatim` verdicts, so any test against the real registry
/// would have passed vacuously. A later pass landed the first real ones,
/// and this is the first assertion that a shipped `Verbatim` verdict actually
/// reaches a user's archive rather than merely being declared.
///
/// `revoked_device_grants` is the case pinned: a public renewal key and the
/// clock, which is the owner's own device-revocation history.
#[tokio::test]
async fn a_verbatim_verdict_emits_the_actors_rows() {
    let (base, state) = start_server().await;
    let kp = ActorKeypair::generate();
    let actor_id = kp.actor_id();
    state
        .db
        .create_user(&actor_id.0, "free", "verbatim")
        .await
        .unwrap();
    let token = state.auth.token_store.insert(actor_id, 3600).await;

    const DEVICE_KEY: &str = "revoked-device-renewal-pubkey";
    {
        let conn = state.db.conn().await;
        conn.execute(
            "INSERT INTO revoked_device_grants (actor_id, auth_device_key, revoked_at)
             VALUES (?1, ?2, ?3)",
            rusqlite::params![
                actor_id.0.to_vec(),
                DEVICE_KEY.as_bytes().to_vec(),
                1_755_000_123_i64
            ],
        )
        .unwrap();
    }

    let entries = read_archive(&export_zip(&base, &token).await);
    let (_, body) = entries
        .iter()
        .find(|(n, _)| n == "export/tables/revoked_device_grants.ndjson")
        .expect(
            "revoked_device_grants is Export::Verbatim with a seeded row, so the \
             registry-driven walk must emit it",
        );
    let row: serde_json::Value =
        serde_json::from_slice(body.split(|b| *b == b'\n').next().unwrap()).unwrap();

    // Every column, in the at-rest form the walk declares (BLOB as lowercase hex).
    assert_eq!(
        row.get("auth_device_key").and_then(|v| v.as_str()),
        Some(hex::encode(DEVICE_KEY.as_bytes()).as_str())
    );
    assert_eq!(
        row.get("revoked_at").and_then(|v| v.as_i64()),
        Some(1_755_000_123)
    );
    assert_eq!(
        row.get("actor_id").and_then(|v| v.as_str()),
        Some(hex::encode(actor_id.0).as_str()),
        "Verbatim means every column, the actor column included"
    );

    // And the table is no longer declared as backlog or withheld.
    let (_, manifest_bytes) = entries
        .iter()
        .find(|(n, _)| n == "export/manifest.json")
        .expect("manifest.json");
    let manifest: serde_json::Value = serde_json::from_slice(manifest_bytes).unwrap();
    let coverage = &manifest["coverage"];
    for list in ["unreviewed_tables", "withheld_tables"] {
        let declared = coverage[list].as_array().expect("coverage list");
        assert!(
            !declared
                .iter()
                .any(|v| v == "revoked_device_grants" || v["table"] == "revoked_device_grants"),
            "a table the export EMITS must not also be declared in coverage.{list}"
        );
    }
}

/// The shaped domain's blind spot, closed — a knock the user has ALREADY
/// RECEIVED, with its contents, reaches the archive.
///
/// **Why this case and not a synthetic one.** The finding here is
/// that a hand-written shaped domain LOOKS like coverage: `export/knocks/` is
/// right there in the archive, so a reader concludes knocks are exported. It
/// is built from `poll_knocks`, which selects `WHERE delivered = 0` and does
/// not select `payload` at all (`db/contacts.rs:300`) — so before this fix,
/// every knock the user had already received was absent from their
/// export, and the ones present arrived without their contents. Nothing
/// failed; the archive was simply, silently short.
///
/// So the assertion is deliberately double-ended: the row must appear in the
/// registry-driven `tables/knocks.ndjson`, **and** the shaped `export/knocks/`
/// directory must still not carry it. The second half is what keeps this test
/// honest — if a later session "fixes" the gap by widening `poll_knocks`
/// instead, the delivered knock would appear in both places and this test
/// would go green for a reason it was not written for, which is why it pins
/// the shaped domain's own filter as the thing that did NOT change.
#[tokio::test]
async fn a_delivered_knock_and_its_payload_reach_the_archive() {
    let (base, state) = start_server().await;
    let kp = ActorKeypair::generate();
    let actor_id = kp.actor_id();
    state
        .db
        .create_user(&actor_id.0, "free", "knocked")
        .await
        .unwrap();
    let token = state.auth.token_store.insert(actor_id, 3600).await;

    const PAYLOAD: &str = "sealed-knock-body-the-shaped-domain-never-selected";
    const SUMMARY: &str = "a knock already delivered";
    let sender = [7u8; 32];
    {
        let conn = state.db.conn().await;
        conn.execute(
            "INSERT INTO knocks
               (actor_id, sender_id, sender_node, summary, payload, created_at, delivered)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, 1)",
            rusqlite::params![
                actor_id.0.to_vec(),
                sender.to_vec(),
                b"https://sender.example".to_vec(),
                SUMMARY,
                PAYLOAD.as_bytes().to_vec(),
                1_755_000_456_i64,
            ],
        )
        .unwrap();
    }

    let entries = read_archive(&export_zip(&base, &token).await);

    // The shaped domain still does not carry it — `poll_knocks` is unchanged.
    assert!(
        !entries.iter().any(|(n, _)| n.starts_with("export/knocks/")),
        "the shaped knocks domain reads `delivered = 0`, so a delivered knock \
         must still be absent from export/knocks/ — if this fires, the domain's \
         filter changed and this test is no longer measuring what it was \
         written to measure"
    );

    // The registry-driven leg does.
    let (_, body) = entries
        .iter()
        .find(|(n, _)| n == "export/tables/knocks.ndjson")
        .expect(
            "knocks is Export::Verbatim with a seeded row, so \
             the registry-driven walk must emit it",
        );
    let row: serde_json::Value =
        serde_json::from_slice(body.split(|b| *b == b'\n').next().unwrap()).unwrap();

    assert_eq!(
        row.get("payload").and_then(|v| v.as_str()),
        Some(hex::encode(PAYLOAD.as_bytes()).as_str()),
        "the knock's contents are the half `poll_knocks` never selected; \
         Verbatim carries them in at-rest form (BLOB as lowercase hex)"
    );
    assert_eq!(row.get("delivered").and_then(|v| v.as_i64()), Some(1));
    assert_eq!(row.get("summary").and_then(|v| v.as_str()), Some(SUMMARY));
    assert_eq!(
        row.get("sender_id").and_then(|v| v.as_str()),
        Some(hex::encode(sender).as_str())
    );

    // And knocks is no longer declared as backlog or withheld.
    let (_, manifest_bytes) = entries
        .iter()
        .find(|(n, _)| n == "export/manifest.json")
        .expect("manifest.json");
    let manifest: serde_json::Value = serde_json::from_slice(manifest_bytes).unwrap();
    let coverage = &manifest["coverage"];
    for list in ["unreviewed_tables", "withheld_tables"] {
        let declared = coverage[list].as_array().expect("coverage list");
        assert!(
            !declared
                .iter()
                .any(|v| v == "knocks" || v["table"] == "knocks"),
            "a table the export EMITS must not also be declared in coverage.{list}"
        );
    }
}

/// A `Redacted` verdict drops its named column **from the real archive**, and
/// the rest of the row still rides.
///
/// The registry-side guard (`every_redacted_omit_names_a_real_column`) proves
/// the `omit` names exist; the unit test `a_redacted_verdict_omits_exactly_its_
/// named_columns` proves the mechanism on a synthetic table. Neither watches a
/// real verdict travel the real endpoint, which is the direction that matters
/// now that row 138 emits: `read_actor_rows` matches `omit` against the live
/// `stmt.column_names()`, so this is the assertion that a specific shipped
/// verdict withholds a specific shipped column.
///
/// `capability_grants` is the case worth pinning: its `blob` is a `GrantBlob`
/// whose `wrapped_keys` open the granted scopes, while the rest of the row is
/// the grant ledger `principles.md` § The user always controls their data
/// requires the owner be able to audit. Both halves of that verdict are
/// asserted — the key is gone, the ledger is there — because a redaction that
/// quietly took the whole row with it would satisfy a leak-only check.
#[tokio::test]
async fn a_redacted_verdict_drops_its_column_and_keeps_the_row() {
    let (base, state) = start_server().await;
    let kp = ActorKeypair::generate();
    let actor_id = kp.actor_id();
    state
        .db
        .create_user(&actor_id.0, "free", "redacted")
        .await
        .unwrap();
    let token = state.auth.token_store.insert(actor_id, 3600).await;

    const GRANT_BLOB: &str = "SEALED-GRANTBLOB-must-never-be-exported";
    const HOLDER: &str = "HOLDER-PUBKEY-must-survive-the-redaction";
    {
        let conn = state.db.conn().await;
        conn.execute(
            "INSERT INTO capability_grants
                 (owner_actor_id, grant_id, holder_pubkey, blob, epoch_end, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            rusqlite::params![
                actor_id.0.to_vec(),
                b"grant-id-0001".to_vec(),
                HOLDER.as_bytes().to_vec(),
                GRANT_BLOB.as_bytes().to_vec(),
                4_102_444_800_i64,
                1_755_000_000_i64,
            ],
        )
        .unwrap();
    }

    let entries = read_archive(&export_zip(&base, &token).await);
    let (_, body) = entries
        .iter()
        .find(|(n, _)| n == "export/tables/capability_grants.ndjson")
        .expect(
            "capability_grants is Export::Redacted with a seeded row, so the walk must emit it \
             — an absent file means the redaction took the whole table, not one column",
        );
    let row: serde_json::Value =
        serde_json::from_slice(body.split(|b| *b == b'\n').next().unwrap()).unwrap();

    assert!(
        row.get("blob").is_none(),
        "capability_grants.blob rode out in the archive: {row}"
    );
    assert_eq!(
        row.get("holder_pubkey").and_then(|v| v.as_str()),
        Some(hex::encode(HOLDER.as_bytes()).as_str()),
        "the grant ledger must survive the redaction — omitting `blob` must not empty the row"
    );
    assert!(
        row.get("epoch_end").is_some() && row.get("created_at").is_some(),
        "the grant's window is the audit the ledger exists for: {row}"
    );

    // And the sealed bytes appear in no archive entry, under either encoding.
    let hex_blob = hex::encode(GRANT_BLOB.as_bytes());
    for (name, body) in &entries {
        let haystack = String::from_utf8_lossy(body);
        assert!(
            !haystack.contains(GRANT_BLOB) && !haystack.contains(&hex_blob),
            "{name} carries the redacted GrantBlob"
        );
    }
}

/// The accountability plane: an actor's own CONDUCT records ride, the hash
/// chain they sit in stays home (`audit_log` +
/// `pending_actions`, the two nest-wide hash-chained tables).
///
/// **The invariant that decides both verdicts is authorship**: every
/// production writer keys these rows to the party who PERFORMED the act and
/// fills `action`/`target`/`detail`/`payload` from that party's own request
/// (`audit_on_conn` call sites; `create_pending_action` call sites), so the
/// `WHERE actor_id = me` slice hands the exporter only records of their own
/// conduct, with content they supplied — safe under the eviction token by
/// construction, because there is nothing in the slice the exporter did not
/// already possess when they acted. The chain columns are the one exception:
/// `prev_hash`/`entry_hash`/`chain_hash` are computed by the nest OVER THE
/// WHOLE LOG, verify nothing inside a slice, and their only marginal
/// information is a confirmation oracle on third parties' adjacent entries —
/// so they are the redaction.
///
/// Both directions are asserted per table: presence (a later session flipping
/// either verdict to `Withheld*` reds here — the guard-set asymmetry means no
/// belt would), redaction (a demotion to `Verbatim` reds here), and the slice
/// boundary (a foreign actor's rows appear in no archive entry).
#[tokio::test]
async fn the_actors_own_conduct_rides_while_the_hash_chain_stays_home() {
    let (base, state) = start_server().await;
    let kp = ActorKeypair::generate();
    let actor_id = kp.actor_id();
    state
        .db
        .create_user(&actor_id.0, "free", "conduct")
        .await
        .unwrap();
    let token = state.auth.token_store.insert(actor_id, 3600).await;
    let foreign: [u8; 32] = [0x5A; 32];

    const MY_DETAIL: &str = "my-audit-detail-rides";
    const FOREIGN_DETAIL: &str = "foreign-audit-detail-must-not-ride";
    const MY_PAYLOAD: &str = "my-pending-payload-rides";
    const FOREIGN_PAYLOAD: &str = "foreign-pending-payload-must-not-ride";
    const MY_IP: &str = "203.0.113.7";

    state
        .db
        .audit(
            Some(&actor_id.0),
            "pairing.add",
            Some("device-alpha"),
            Some(MY_DETAIL),
        )
        .await
        .unwrap();
    state
        .db
        .audit(
            Some(&foreign),
            "user.suspended",
            Some("some-target"),
            Some(FOREIGN_DETAIL),
        )
        .await
        .unwrap();

    use fauna_nest::pending_actions::ActionType;
    let mine = state
        .db
        .create_pending_action(
            &ActionType::SnapshotDelete,
            &actor_id.0,
            Some("snapshot-42"),
            Some(MY_PAYLOAD),
            Some(MY_IP),
        )
        .await
        .unwrap();
    // Cancelled by its creator: fills `cancelled_by`/`cancelled_at`, the two
    // columns the live own-row surface (`fauna.pending_actions.get`) does not
    // serve — the export is deliberately the wider view of the actor's own row.
    state
        .db
        .cancel_pending_action(mine, &actor_id.0)
        .await
        .unwrap();
    state
        .db
        .create_pending_action(
            &ActionType::AccountDelete,
            &foreign,
            None,
            Some(FOREIGN_PAYLOAD),
            None,
        )
        .await
        .unwrap();

    let entries = read_archive(&export_zip(&base, &token).await);

    // --- audit_log: my conduct rides, the chain does not, the neighbour never.
    let (_, body) = entries
        .iter()
        .find(|(n, _)| n == "export/tables/audit_log.ndjson")
        .expect(
            "audit_log is a ruled emitting verdict with a seeded row for this actor — an absent \
             file means the verdict was demoted to Withheld, which no belt watches (the guard-set \
             asymmetry): this assertion is the hand-written pin",
        );
    let rows: Vec<serde_json::Value> = body
        .split(|b| *b == b'\n')
        .filter(|l| !l.is_empty())
        .map(|l| serde_json::from_slice(l).unwrap())
        .collect();
    let my_row = rows
        .iter()
        .find(|r| r.get("action").and_then(|v| v.as_str()) == Some("pairing.add"))
        .expect("the actor's own audit row must ride");
    assert_eq!(
        my_row.get("detail").and_then(|v| v.as_str()),
        Some(MY_DETAIL)
    );
    assert_eq!(
        my_row.get("target").and_then(|v| v.as_str()),
        Some("device-alpha")
    );
    for row in &rows {
        for chain_col in ["prev_hash", "entry_hash"] {
            assert!(
                row.get(chain_col).is_none(),
                "audit_log.{chain_col} rode out: it is computed over the whole log, verifies \
                 nothing in a slice, and confirms third parties' adjacent entries — {row}"
            );
        }
        assert_ne!(
            row.get("action").and_then(|v| v.as_str()),
            Some("user.suspended"),
            "a foreign actor's audit row crossed the slice boundary"
        );
    }

    // --- pending_actions: same shape, plus the lifecycle columns ride.
    let (_, body) = entries
        .iter()
        .find(|(n, _)| n == "export/tables/pending_actions.ndjson")
        .expect(
            "pending_actions is a ruled emitting verdict with a seeded row for this actor — an \
             absent file means the verdict was demoted to Withheld (see audit_log's pin above)",
        );
    let rows: Vec<serde_json::Value> = body
        .split(|b| *b == b'\n')
        .filter(|l| !l.is_empty())
        .map(|l| serde_json::from_slice(l).unwrap())
        .collect();
    let my_row = rows
        .iter()
        .find(|r| r.get("action_type").and_then(|v| v.as_str()) == Some("snapshot.delete"))
        .expect("the actor's own pending action must ride");
    assert_eq!(
        my_row.get("payload").and_then(|v| v.as_str()),
        Some(MY_PAYLOAD)
    );
    assert_eq!(
        my_row.get("ip_address").and_then(|v| v.as_str()),
        Some(MY_IP),
        "the request's own address is the actor_last_ip precedent — personal data cuts toward \
         exporting"
    );
    assert_eq!(
        my_row.get("cancelled_by").and_then(|v| v.as_str()),
        Some(hex::encode(actor_id.0).as_str()),
        "who cancelled the actor's scheduled action is accountability data whose primary \
         audience is the row's owner"
    );
    assert!(my_row.get("cancelled_at").is_some());
    for row in &rows {
        assert!(
            row.get("chain_hash").is_none(),
            "pending_actions.chain_hash rode out — same chain-column class as audit_log's: {row}"
        );
        assert_ne!(
            row.get("action_type").and_then(|v| v.as_str()),
            Some("account.delete"),
            "a foreign actor's pending action crossed the slice boundary"
        );
    }

    // --- and the foreign rows' content appears in NO archive entry.
    for (name, body) in &entries {
        let haystack = String::from_utf8_lossy(body);
        assert!(
            !haystack.contains(FOREIGN_DETAIL) && !haystack.contains(FOREIGN_PAYLOAD),
            "{name} carries a foreign actor's conduct record"
        );
    }
}

/// A sealed collection rides in its at-rest form; its tombstone log does not.
///
/// **The two halves are asserted together because the ruling separated them,
/// and each half is a different first.**
///
/// The sealed calendar columns are the axis's first use of the sealed-columns clause
/// (`account-data-plane.md` § Nest-side requirements item 1: *"Sealed columns
/// ride `Verbatim`/`Redacted` in their at-rest form"*). Every `Verbatim`
/// verdict that shipped before this ruling carried plaintext or a public key,
/// so nothing had ever watched ciphertext the nest cannot open travel out to
/// its owner — and the failure mode is invisible from the registry side: a
/// verdict reads identically whether the emission carries the sealed column or
/// silently drops it.
///
/// The tombstone log is the other half. The **succession** axis rules
/// `bridge_caldav_expunged` inseparable from the two tables above it (one sync
/// clock a MUA compares across all three); this axis withholds it, because an
/// archive is a snapshot with no clock to catch up against. That divergence is
/// deliberate and this test is what pins it: a later session "restoring
/// consistency" by promoting the tombstones to `Verbatim` — the natural reading
/// of the succession comment — reds here.
#[tokio::test]
async fn a_sealed_collection_rides_while_its_tombstone_log_does_not() {
    let (base, state) = start_server().await;
    let kp = ActorKeypair::generate();
    let actor_id = kp.actor_id();
    state
        .db
        .create_user(&actor_id.0, "free", "sealed")
        .await
        .unwrap();
    let token = state.auth.token_store.insert(actor_id, 3600).await;

    // Ciphertext the nest holds and cannot open — what the owner's own read key
    // would unseal into a calendar name and a VEVENT's search tokens. (The
    // sealed VEVENT itself rests in the `__calendar` segment, not the row; the
    // export carries it through the segment plane.)
    const SEALED_CALENDAR_METADATA: &[u8] = b"sealed-calendar-name-colour-timezone";
    const SEALED_INDEX_HINT: &[u8] = b"SEALED-HINT-dentist-tuesday-0900";
    const CALENDAR_ID: &[u8] = b"calendar-id-0001";
    {
        let conn = state.db.conn().await;
        conn.execute(
            "INSERT INTO bridge_caldav_calendars
                 (actor_id, calendar_id, encrypted_metadata, ctag, highestmodseq, created_at)
             VALUES (?1, ?2, ?3, 7, 9, ?4)",
            rusqlite::params![
                actor_id.0.to_vec(),
                CALENDAR_ID.to_vec(),
                SEALED_CALENDAR_METADATA.to_vec(),
                1_755_000_000_i64
            ],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO bridge_caldav_events
                 (actor_id, calendar_id, event_id, uid_hash,
                  encrypted_index_hint, etag, modseq, ciphertext_size, internal_date, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, '0000000000000009', 9, ?6, ?7, ?7)",
            rusqlite::params![
                actor_id.0.to_vec(),
                CALENDAR_ID.to_vec(),
                b"event-id-0001".to_vec(),
                b"uid-hash-0001".to_vec(),
                SEALED_INDEX_HINT.to_vec(),
                34_i64,
                1_755_000_001_i64,
            ],
        )
        .unwrap();
        // A deletion the owner performed: the tombstone the withheld verdict
        // covers. Seeded so the assertion below is about a verdict, not about
        // an empty table (the walk skips a table with zero rows either way,
        // which would make an absence-only check pass vacuously).
        conn.execute(
            "INSERT INTO bridge_caldav_expunged
                 (actor_id, calendar_id, event_id, uid_hash, modseq, expunged_at)
             VALUES (?1, ?2, ?3, ?4, 8, ?5)",
            rusqlite::params![
                actor_id.0.to_vec(),
                CALENDAR_ID.to_vec(),
                b"event-id-0000".to_vec(),
                b"uid-hash-0000".to_vec(),
                1_755_000_002_i64,
            ],
        )
        .unwrap();
    }

    let entries = read_archive(&export_zip(&base, &token).await);

    // -- the sealed calendar rides, as ciphertext, hex-encoded --
    let (_, calendars) = entries
        .iter()
        .find(|(n, _)| n == "export/tables/bridge_caldav_calendars.ndjson")
        .expect("bridge_caldav_calendars is Verbatim with a seeded row");
    let calendar: serde_json::Value =
        serde_json::from_slice(calendars.split(|b| *b == b'\n').next().unwrap()).unwrap();
    assert_eq!(
        calendar.get("encrypted_metadata").and_then(|v| v.as_str()),
        Some(hex::encode(SEALED_CALENDAR_METADATA).as_str()),
        "the sealed calendar metadata must ride in at-rest form — the nest does not \
         unseal for the export, and it does not withhold the owner's own sealed rows either"
    );
    assert_eq!(
        calendar.get("ctag").and_then(|v| v.as_i64()),
        Some(7),
        "Verbatim is every column: the plaintext floor rides beside the sealed one"
    );

    let (_, events) = entries
        .iter()
        .find(|(n, _)| n == "export/tables/bridge_caldav_events.ndjson")
        .expect("bridge_caldav_events is Verbatim with a seeded row");
    let event: serde_json::Value =
        serde_json::from_slice(events.split(|b| *b == b'\n').next().unwrap()).unwrap();
    assert_eq!(
        event.get("encrypted_index_hint").and_then(|v| v.as_str()),
        Some(hex::encode(SEALED_INDEX_HINT).as_str()),
        "the sealed lookup hint over the VEVENT is the owner's own calendar content and \
         must reach their archive in at-rest form"
    );

    // -- the tombstone log does not ride, and says so by name and class --
    assert!(
        !entries
            .iter()
            .any(|(n, _)| n == "export/tables/bridge_caldav_expunged.ndjson"),
        "bridge_caldav_expunged is WithheldOperational — a seeded row must not produce an \
         archive entry"
    );

    let (_, manifest_bytes) = entries
        .iter()
        .find(|(n, _)| n == "export/manifest.json")
        .expect("manifest.json");
    let manifest: serde_json::Value = serde_json::from_slice(manifest_bytes).unwrap();
    let withheld = manifest["coverage"]["withheld_tables"]
        .as_array()
        .expect("withheld_tables array");
    let declared = withheld
        .iter()
        .find(|w| w["table"] == "bridge_caldav_expunged")
        .expect(
            "a withheld table must be declared by name — an archive that simply lacks the \
             rows tells the owner nothing about why",
        );
    assert_eq!(
        declared["reason_class"], "operational",
        "the class is the answer to *why*: this is the nest's own sync bookkeeping, not a \
         key it must not hand over"
    );
    assert!(
        !manifest["coverage"]["unreviewed_tables"]
            .as_array()
            .expect("unreviewed_tables array")
            .iter()
            .any(|v| v == "bridge_caldav_calendars"
                || v == "bridge_caldav_events"
                || v == "bridge_caldav_expunged"),
        "a ruled table must leave the backlog declaration"
    );
}

/// The backup topology RIDES — the plane's `WithheldDerived` reflex, pinned.
///
/// The ruling made the five backup/custody tables `Verbatim`, and the
/// one mistake that plane invites is a word: the **succession** axis rules
/// `backup_destinations` a `Burn` because it is *"a PROJECTION of
/// the `fauna.state.backup` destination rows, which are the authority"*, and `projection`
/// reads like `WithheldDerived` on this axis. It is not, and the discriminator
/// is a simple test — *is the input in the archive?* The authority is the
/// client-sealed `fauna.state.backup` plane entries, whose content the export never carries,
/// so an owner handed a `Derived` verdict here could not rebuild their backup
/// topology from anything in the zip; they would simply lose it.
///
/// ⚠ **This test exists because a mutation did NOT red.** Demoting the verdict
/// to `WithheldDerived` leaves all 39 registry guards green: the backlog
/// ratchet counts only `Unreviewed`, and nothing anywhere checks *which*
/// withholding class a table gets. That asymmetry is deliberate in the ruling
/// ("a wrong Withheld is the status quo") and it is exactly why the direction
/// this axis does not belt — the archive silently coming up SHORT — needs its
/// pins written by hand, one per plane whose verdict a later reader might
/// plausibly talk themselves out of.
#[tokio::test]
async fn the_backup_topology_rides_rather_than_reading_as_derived() {
    let (base, state) = start_server().await;
    let kp = ActorKeypair::generate();
    let actor_id = kp.actor_id();
    state
        .db
        .create_user(&actor_id.0, "free", "backed-up")
        .await
        .unwrap();
    let token = state.auth.token_store.insert(actor_id, 3600).await;

    const DEST_URL: &str = "https://custodian.example/nest";
    {
        let conn = state.db.conn().await;
        conn.execute(
            "INSERT INTO backup_destinations
               (owner_actor_id, destination_id, nest_url, nest_id, added_at)
             VALUES (?1, 'dest-1', ?2, ?3, ?4)",
            rusqlite::params![
                actor_id.0.to_vec(),
                DEST_URL,
                vec![9u8; 32],
                1_755_000_789_i64
            ],
        )
        .unwrap();
    }

    let entries = read_archive(&export_zip(&base, &token).await);
    let (_, body) = entries
        .iter()
        .find(|(n, _)| n == "export/tables/backup_destinations.ndjson")
        .expect(
            "backup_destinations is Export::Verbatim with a \
             seeded row, so the registry-driven walk must emit it. If this is \
             missing, check whether the verdict was demoted to a Withheld* class \
             on the \"it is only a projection\" reading this ruling refutes",
        );
    let row: serde_json::Value =
        serde_json::from_slice(body.split(|b| *b == b'\n').next().unwrap()).unwrap();
    assert_eq!(row.get("nest_url").and_then(|v| v.as_str()), Some(DEST_URL));
}

/// The creator's subscriber roster RIDES — the two-party verdict a later
/// reader is most likely to talk themselves out of.
///
/// `subscribers` names a second person (`subscriber_id`, the paying reader),
/// which is the shape that once got a reported-party table (`sender_reputation`,
/// since removed) withheld — so the standing temptation is to "fix" this one
/// the same way. It was ruled
/// the other direction because the product already answers the
/// disclosure question: `monetization.md` § Pillar 1 renders the roster to the
/// creator in their own Tiers tab, "with per-row remove". Withholding it would
/// take the commercial relationship the creator OWNS out of their own records
/// while the nest keeps showing it to them on screen.
///
/// ⚠ Written for the same reason as the backup-topology pin, and for the
/// sharper version of that lesson this ruling turned up: demoting a `Verbatim`
/// to any `Withheld*` class reds **nothing** in the 39 registry guards, and so
/// does the reverse. The belts on this axis catch *named-secret* leaks, via
/// column-name fragments — they cannot see a wrong-OWNER decision in either
/// direction. Those get hand-written pins or they get nothing.
#[tokio::test]
async fn the_creators_subscriber_roster_rides() {
    let (base, state) = start_server().await;
    let kp = ActorKeypair::generate();
    let actor_id = kp.actor_id();
    state
        .db
        .create_user(&actor_id.0, "free", "creator")
        .await
        .unwrap();
    let token = state.auth.token_store.insert(actor_id, 3600).await;

    let subscriber = [3u8; 32];
    {
        let conn = state.db.conn().await;
        conn.execute_batch("PRAGMA foreign_keys = OFF;").unwrap();
        conn.execute(
            "INSERT INTO subscribers
                 (author_id, subscriber_id, tier_name, approved_at)
             VALUES (?1, ?2, 'gold', ?3)",
            rusqlite::params![actor_id.0.to_vec(), subscriber.to_vec(), 1_755_000_333_i64],
        )
        .unwrap();
    }

    let entries = read_archive(&export_zip(&base, &token).await);
    let (_, body) = entries
        .iter()
        .find(|(n, _)| n == "export/tables/subscribers.ndjson")
        .expect(
            "subscribers is Export::Verbatim with a seeded \
             row, so the walk must emit it. If this is missing, check whether the \
             verdict was withheld on the \"it names a third party\" reading -- the \
             creator already works this roster by name in their own Tiers tab",
        );
    let row: serde_json::Value =
        serde_json::from_slice(body.split(|b| *b == b'\n').next().unwrap()).unwrap();
    assert_eq!(
        row.get("subscriber_id").and_then(|v| v.as_str()),
        Some(hex::encode(subscriber).as_str()),
        "the paying reader's id is the substance of the roster; a redaction here \
         would leave the creator a row that names nobody"
    );
    assert_eq!(row.get("tier_name").and_then(|v| v.as_str()), Some("gold"));
}

/// A foreign feed's contributor rows never reach the archive — the plane's
/// only NOT-the-exporter's-data table, asserted on the bytes.
///
/// `feed_contributors` is a catch, and it is a catch because
/// the name and the actor column both point the wrong way: `author_id` reads as
/// "the author whose feed this is", and `upsert_contributor` (db/feeds.rs:436)
/// makes it the author a DISCOVERY feed *found* — the feed's own owner being
/// `feeds.owner`, a different column in a different table. An actor-scoped read
/// therefore returns rows describing the exporter's presence inside **other
/// users' feeds**: which feed picked them up, how often, how recently, by what
/// route. It is the `foreign_recovery_heads` shape, except that one was
/// unreachable by construction and this one matches real rows.
///
/// Seeded here from a SECOND actor's feed, which is the only arrangement that
/// can tell the two readings apart: under the correct verdict the exporter's
/// archive is silent about it, and under a `Verbatim` "the author's own
/// contributions" reading it would carry another user's feed id and crawl
/// counters. Asserted on the bytes rather than the verdict, the
/// `the_mail_spools_never_reach_the_archive` shape — and for the same reason:
/// no registry guard notices a withholding class being flipped,
/// so the pin has to watch the archive itself.
#[tokio::test]
async fn a_foreign_feeds_contributor_rows_never_reach_the_archive() {
    let (base, state) = start_server().await;
    let kp = ActorKeypair::generate();
    let actor_id = kp.actor_id();
    state
        .db
        .create_user(&actor_id.0, "free", "discovered")
        .await
        .unwrap();
    let token = state.auth.token_store.insert(actor_id, 3600).await;

    // Somebody ELSE's discovery feed, which happens to have discovered us.
    const OTHER_FEED: &str = "other-users-discovery-feed-id";
    const OTHER_NEST: &str = "https://someone-elses-nest.example";
    {
        let conn = state.db.conn().await;
        conn.execute_batch("PRAGMA foreign_keys = OFF;").unwrap();
        conn.execute(
            "INSERT INTO feed_contributors
                 (feed_id, nest_url, author_id, hit_count, last_seen, poll_priority,
                  discovered_via, created_at)
             VALUES (?1, ?2, ?3, 42, ?4, 3, 'referral', ?4)",
            rusqlite::params![
                OTHER_FEED,
                OTHER_NEST,
                actor_id.0.to_vec(),
                1_755_000_222_i64
            ],
        )
        .unwrap();
    }

    let entries = read_archive(&export_zip(&base, &token).await);

    assert!(
        !entries
            .iter()
            .any(|(n, _)| n == "export/tables/feed_contributors.ndjson"),
        "feed_contributors is WithheldOperational — a seeded row must not produce \
         an archive entry. If this fires, check whether the verdict was read as \
         \"the author's own feed contributions\"; `author_id` is the DISCOVERED \
         contributor, and the row belongs to a feed this actor does not own"
    );

    for needle in [OTHER_FEED, OTHER_NEST] {
        for (name, bytes) in &entries {
            let haystack = String::from_utf8_lossy(bytes);
            assert!(
                !haystack.contains(needle),
                "{name} carries another user's discovery-feed identity ({needle}) — the \
                 exporter is the SUBJECT of that row, not its owner"
            );
        }
    }

    // Declared by name and class, so the owner is told the plane exists.
    let (_, manifest_bytes) = entries
        .iter()
        .find(|(n, _)| n == "export/manifest.json")
        .expect("manifest.json");
    let manifest: serde_json::Value = serde_json::from_slice(manifest_bytes).unwrap();
    let withheld = manifest["coverage"]["withheld_tables"]
        .as_array()
        .expect("withheld_tables array");
    let declared = withheld
        .iter()
        .find(|w| w["table"] == "feed_contributors")
        .expect("feed_contributors must be declared withheld by name");
    assert_eq!(declared["reason_class"], "operational");
}

/// The sync plane, graded in one test because the plane's
/// whole point is a SPLIT: four tables ride and one is withheld, and the
/// withheld one is the twin that reads exactly like the others.
///
/// **Why the withheld half needs a byte pin at all.** The ruling established
/// that no registry guard notices a withholding class being flipped — the
/// backlog ratchet counts `Unreviewed`, and the `Redacted` column guard only
/// checks spellings — so a later session that reads
/// `channel_foreign_members.actor_id` as "this actor's cross-nest memberships"
/// can promote it to `Verbatim` and nothing reds. This watches the archive
/// itself.
///
/// **Why the seed is synthetic, and why that is legitimate here.** No producer
/// can write a LOCAL actor into `channel_foreign_members`: the sole writer is
/// `welcome_deliver`'s federation-relay branch, whose recipient is homed on the
/// peer nest by construction. The succession ruling forbids a pin that
/// seeds a local row *there*, and rightly — a `Move` pin would manufacture the
/// observable it claims. The direction of the claim is what makes this one
/// different: it asserts a WITHHOLDING, so passing it means "the walk honours
/// the verdict", never "production can reach this state". Under the wrong
/// verdict this row is precisely what would emit.
#[tokio::test]
async fn the_sync_plane_rides_except_the_foreign_membership_row() {
    let (base, state) = start_server().await;
    let kp = ActorKeypair::generate();
    let actor_id = kp.actor_id();
    state
        .db
        .create_user(&actor_id.0, "free", "syncplane")
        .await
        .unwrap();
    let token = state.auth.token_store.insert(actor_id, 3600).await;

    // The peer nest that homes the foreign member — the identity a wrong
    // verdict would put in this actor's archive.
    const PEER_NEST_URL: &str = "https://peer-that-homes-the-foreign-member.example";
    let peer_nest_id = [0xF0u8; 32];
    let channel = [0xC1u8; 32];

    {
        let conn = state.db.conn().await;
        conn.execute_batch("PRAGMA foreign_keys = OFF;").unwrap();
        let actor = actor_id.0.to_vec();

        conn.execute(
            "INSERT INTO actor_channels (actor_id, channel_id, created_at) VALUES (?1, ?2, ?3)",
            rusqlite::params![actor.clone(), channel.to_vec(), 1_755_000_300_i64],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO folder_member_access
                 (channel_id, actor_id, access, byte_cap, bytes_used, updated_at)
             VALUES (?1, ?2, 'writer', 4096, 512, ?3)",
            rusqlite::params![channel.to_vec(), actor.clone(), 1_755_000_301_i64],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO folder_channel_claims (channel_id, claimed_by, claimed_at)
             VALUES (?1, ?2, ?3)",
            rusqlite::params![channel.to_vec(), actor.clone(), 1_755_000_302_i64],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO sync_changes
                 (actor_id, path_hash, manifest_hash, size_bytes, change_type, created_at)
             VALUES (?1, ?2, ?3, 17, 'upsert', ?4)",
            rusqlite::params![
                actor.clone(),
                vec![0xA1u8; 32],
                vec![0xA2u8; 32],
                1_755_000_303_i64
            ],
        )
        .unwrap();

        // The row no production writer can create — see the doc comment.
        conn.execute(
            "INSERT INTO channel_foreign_members
                 (channel_id, actor_id, home_nest_id, created_at, nest_url)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            rusqlite::params![
                channel.to_vec(),
                actor,
                peer_nest_id.to_vec(),
                1_755_000_304_i64,
                PEER_NEST_URL
            ],
        )
        .unwrap();
    }

    let entries = read_archive(&export_zip(&base, &token).await);
    let name_of = |t: &str| format!("export/tables/{t}.ndjson");

    // The four that ride. Asserted individually so a partial regression names
    // the table it lost rather than the count.
    for table in [
        "sync_changes",
        "actor_channels",
        "folder_member_access",
        "folder_channel_claims",
    ] {
        assert!(
            entries.iter().any(|(n, _)| n == &name_of(table)),
            "{table} is Verbatim — a seeded row must produce \
             `export/tables/{table}.ndjson`. The archive must not be quieter than the \
             doors this actor can already call"
        );
    }

    // The one that does not.
    assert!(
        !entries
            .iter()
            .any(|(n, _)| n == &name_of("channel_foreign_members")),
        "channel_foreign_members is WithheldOperational — a seeded row must not produce \
         an archive entry. If this fires, check whether the verdict was read as \"this \
         actor's cross-nest memberships\"; `actor_id` there names an actor homed on \
         ANOTHER nest, and the row is the channel's relay-authorization record"
    );

    // ...and not as bytes anywhere, under either encoding: the peer's identity is
    // what a wrong verdict would actually disclose.
    let peer_hex = hex::encode(peer_nest_id);
    for (name, bytes) in &entries {
        let haystack = String::from_utf8_lossy(bytes);
        assert!(
            !haystack.contains(PEER_NEST_URL) && !haystack.contains(&peer_hex),
            "{name} carries the peer nest that homes a foreign channel member — that is \
             the channel's federation-relay bookkeeping, not this actor's data"
        );
    }

    // Declared by name and class, so the owner is told the plane exists.
    let (_, manifest_bytes) = entries
        .iter()
        .find(|(n, _)| n == "export/manifest.json")
        .expect("manifest.json");
    let manifest: serde_json::Value = serde_json::from_slice(manifest_bytes).unwrap();
    let withheld = manifest["coverage"]["withheld_tables"]
        .as_array()
        .expect("withheld_tables array");
    let declared = withheld
        .iter()
        .find(|w| w["table"] == "channel_foreign_members")
        .expect("channel_foreign_members must be declared withheld by name");
    assert_eq!(declared["reason_class"], "operational");

    // And none of the five is still declared an open judgment.
    let unreviewed = manifest["coverage"]["unreviewed_tables"]
        .as_array()
        .expect("unreviewed_tables array");
    for table in [
        "sync_changes",
        "actor_channels",
        "folder_member_access",
        "folder_channel_claims",
        "channel_foreign_members",
    ] {
        assert!(
            !unreviewed.iter().any(|t| t == table),
            "{table} was ruled — it must not still be declared Unreviewed"
        );
    }
}

/// The identity/lifecycle plane, all five `Verbatim`.
///
/// **The reversal this guards is a misreading, not a disagreement.** Two of
/// the five carry a succession `Stay` reasoned from *compromise* —
/// `actor_mls_pubkeys` and `actor_index_pubkeys` stay behind because their keys
/// seal future inbound mail and a seed thief can derive the private halves.
/// That is a question about who keeps *receiving*, and it reads exactly like a
/// withholding argument to someone skimming for one. The bytes stored here are
/// the PUBLISHED halves: the MTA bridge fetches both by RPC
/// (`fauna.bridges.fetch_recipient_{mls,index}_pubkey`), and `mlkem_ek` is an
/// ML-KEM *encapsulation* key — the public one, the same naming trap already
/// cleared one plane over. Demoting either to a `Withheld*` reds nothing in the
/// registry (the guard-set asymmetry already established), so the archive
/// itself has to watch.
///
/// Asserted on the emitted bytes for the two key columns specifically, because
/// a demotion that kept the table but dropped those columns would still pass an
/// entry-name check.
#[tokio::test]
async fn the_identity_plane_rides_including_the_published_key_halves() {
    let (base, state) = start_server().await;
    let kp = ActorKeypair::generate();
    let actor_id = kp.actor_id();
    state
        .db
        .create_user(&actor_id.0, "free", "identityplane")
        .await
        .unwrap();
    let token = state.auth.token_store.insert(actor_id, 3600).await;

    // Distinctive so the byte assertions below cannot pass by accident.
    let mls_pubkey = vec![0xA6u8; 32];
    let mlkem_ek = vec![0x9Eu8; 32];
    let index_pubkey = vec![0x7Du8; 32];

    {
        let conn = state.db.conn().await;
        conn.execute_batch("PRAGMA foreign_keys = OFF;").unwrap();
        let actor = actor_id.0.to_vec();

        conn.execute(
            "INSERT INTO actor_successions
                 (old_actor_id, new_actor_id, statement, seq, succeeded_at)
             VALUES (?1, ?2, ?3, 4, ?4)",
            rusqlite::params![
                actor.clone(),
                vec![0xB1u8; 32],
                vec![0xB2u8; 48],
                1_755_000_400_i64
            ],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO handle_cooldowns (handle, old_actor_id, released_at)
             VALUES ('released-handle', ?1, ?2)",
            rusqlite::params![actor.clone(), 1_755_000_401_i64],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO admin_actor_ids (actor_id, added_at, role)
             VALUES (?1, ?2, 'superadmin')",
            rusqlite::params![actor.clone(), 1_755_000_402_i64],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO actor_mls_pubkeys (actor_id, mls_pubkey, updated_at, mlkem_ek)
             VALUES (?1, ?2, ?3, ?4)",
            rusqlite::params![
                actor.clone(),
                mls_pubkey.clone(),
                1_755_000_403_i64,
                mlkem_ek.clone()
            ],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO actor_index_pubkeys (actor_id, index_pubkey, updated_at)
             VALUES (?1, ?2, ?3)",
            rusqlite::params![actor.clone(), index_pubkey.clone(), 1_755_000_404_i64],
        )
        .unwrap();
    }

    let entries = read_archive(&export_zip(&base, &token).await);
    let name_of = |t: &str| format!("export/tables/{t}.ndjson");

    for table in [
        "actor_successions",
        "handle_cooldowns",
        "admin_actor_ids",
        "actor_mls_pubkeys",
        "actor_index_pubkeys",
    ] {
        assert!(
            entries.iter().any(|(n, _)| n == &name_of(table)),
            "{table} is Verbatim — a seeded row must produce \
             `export/tables/{table}.ndjson`. If this fires on one of the pubkey tables, \
             check whether a succession `Stay` reasoned from COMPROMISE was read as a \
             disclosure argument: these columns are the PUBLISHED halves the MTA bridge \
             fetches by RPC"
        );
    }

    // The two published key halves, on the bytes: a demotion that kept the table
    // and dropped these columns would pass the entry-name check above.
    for (col, bytes) in [
        ("actor_mls_pubkeys.mls_pubkey", &mls_pubkey),
        ("actor_mls_pubkeys.mlkem_ek", &mlkem_ek),
        ("actor_index_pubkeys.index_pubkey", &index_pubkey),
    ] {
        let needle = hex::encode(bytes);
        assert!(
            entries
                .iter()
                .any(|(_, b)| String::from_utf8_lossy(b).contains(&needle)),
            "{col} is the owner's own PUBLISHED key half and must ride — it is served to \
             the MTA bridge on request, so an archive that omits it is quieter than an \
             RPC any bridge can call"
        );
    }

    // None of the five is still an open judgment.
    let (_, manifest_bytes) = entries
        .iter()
        .find(|(n, _)| n == "export/manifest.json")
        .expect("manifest.json");
    let manifest: serde_json::Value = serde_json::from_slice(manifest_bytes).unwrap();
    let unreviewed = manifest["coverage"]["unreviewed_tables"]
        .as_array()
        .expect("unreviewed_tables array");
    for table in [
        "actor_successions",
        "handle_cooldowns",
        "admin_actor_ids",
        "actor_mls_pubkeys",
        "actor_index_pubkeys",
    ] {
        assert!(
            !unreviewed.iter().any(|t| t == table),
            "{table} was ruled — it must not still be declared Unreviewed"
        );
    }
}

/// The residual set, whose whole shape is a three-way
/// split, so one test grades all three.
///
/// **The `push_subscriptions` half is the one that matters and it is asserted
/// on bytes.** The row rides (device, transport, when) while the triple
/// `(endpoint, key_p256dh, key_auth)` is dropped, because together they are a
/// sendable Web Push credential and — in the succession entry's own words — the
/// dispatcher authenticates nobody and no kind this nest has can list or revoke
/// a rogue endpoint. An archive copy is therefore an *unrevokable* delivery
/// capability reachable with an eviction token. A `Redacted` verdict that
/// misspells a column omits nothing and says it did
/// (`every_redacted_omission_names_a_real_column` catches the misspelling; this
/// catches the omission failing for any other reason).
///
/// The two withheld tables are vacuous for different reasons and both are
/// seeded synthetically to prove the walk honours the verdict anyway:
/// `invite_requests` cannot coexist with a `users` row (the export endpoint
/// refuses first), and `labelers.publisher_actor` is an artifact keypair rather
/// than an account.
#[tokio::test]
async fn the_residual_plane_splits_three_ways() {
    let (base, state) = start_server().await;
    let kp = ActorKeypair::generate();
    let actor_id = kp.actor_id();
    state
        .db
        .create_user(&actor_id.0, "free", "residual")
        .await
        .unwrap();
    let token = state.auth.token_store.insert(actor_id, 3600).await;

    const PUSH_ENDPOINT: &str = "https://push.example/unrevokable-delivery-capability";
    const PUSH_AUTH: &str = "push-auth-shared-secret";
    const PUSH_P256DH: &str = "push-p256dh-public-half";
    const APPLICANT_MESSAGE: &str = "please-let-me-in-admissions-message";
    const LABELER_WASM: &str = "published-labeler-artifact-bytes";

    {
        let conn = state.db.conn().await;
        conn.execute_batch("PRAGMA foreign_keys = OFF;").unwrap();
        let actor = actor_id.0.to_vec();

        conn.execute(
            "INSERT INTO notifications
                 (actor_id, notif_type, source, sender_id, summary, created_at)
             VALUES (?1, 'mention', 'fauna', ?2, 'someone mentioned you', ?3)",
            rusqlite::params![actor.clone(), vec![0xC5u8; 32], 1_755_000_500_i64],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO push_subscriptions
                 (actor_id, device_id, transport, endpoint, key_p256dh, key_auth, created_at)
             VALUES (?1, 'device-a', 'webpush', ?2, ?3, ?4, ?5)",
            rusqlite::params![
                actor.clone(),
                PUSH_ENDPOINT,
                PUSH_P256DH,
                PUSH_AUTH,
                1_755_000_501_i64
            ],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO labeler_subscriptions
                 (owner_actor, labeler_id, subscribed_ver, created_at)
             VALUES (?1, ?2, 3, ?3)",
            rusqlite::params![actor.clone(), vec![0xC6u8; 32], 1_755_000_502_i64],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO nest_pairings
                 (actor_id, private_nest_id, capabilities, created_at, nest_url)
             VALUES (?1, ?2, 'nostr_serving', ?3, 'https://paired.example')",
            rusqlite::params![actor.clone(), vec![0xC7u8; 32], 1_755_000_503_i64],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO import_sessions
                 (session_id, actor_id, source_descriptor, state, started_at,
                  last_progress_at, expires_at)
             VALUES ('sess-1', ?1, 'gmail:imap.gmail.com:someone', 'completed', ?2, ?2, ?3)",
            rusqlite::params![actor.clone(), 1_755_000_504_i64, 1_755_900_000_i64],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO obligation_action_records
                 (content_type, content_id, author_hex, obligation_id, rule_index, category,
                  confidence, action_taken, timestamp, signature)
             VALUES ('post', 'content-1', ?1, ?2, 0, 'illegal', 1.0, 7, ?3, x'')",
            rusqlite::params![hex::encode(actor_id.0), vec![0xC8u8; 32], 1_755_000_505_i64],
        )
        .unwrap();

        // The two withheld tables, seeded synthetically — neither state is
        // reachable in production (see the doc comment).
        conn.execute(
            "INSERT INTO invite_requests (actor_id, handle, message, status, created_at)
             VALUES (?1, 'wanted-handle', ?2, 'pending', ?3)",
            rusqlite::params![actor.clone(), APPLICANT_MESSAGE, 1_755_000_506_i64],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO labelers
                 (labeler_id, version, publisher_actor, content_kind, factor, wasm_hash,
                  wasm_size, metadata_blob, wasm_bytes, updated_at)
             VALUES (?1, 1, ?2, 'post', 'quality', ?3, 32, x'00', ?4, ?5)",
            rusqlite::params![
                vec![0xC9u8; 32],
                actor,
                vec![0xCAu8; 32],
                LABELER_WASM.as_bytes().to_vec(),
                1_755_000_507_i64
            ],
        )
        .unwrap();
    }

    let entries = read_archive(&export_zip(&base, &token).await);
    let name_of = |t: &str| format!("export/tables/{t}.ndjson");

    // (a) The five that ride whole.
    for table in [
        "notifications",
        "labeler_subscriptions",
        "nest_pairings",
        "import_sessions",
        "obligation_action_records",
    ] {
        assert!(
            entries.iter().any(|(n, _)| n == &name_of(table)),
            "{table} is Verbatim — a seeded row must produce \
             `export/tables/{table}.ndjson`"
        );
    }

    // (b) The two that do not ride at all.
    for table in ["invite_requests", "labelers"] {
        assert!(
            !entries.iter().any(|(n, _)| n == &name_of(table)),
            "{table} is WithheldOperational — a seeded row must not produce an archive \
             entry, whether or not production can create that row"
        );
    }
    for needle in [APPLICANT_MESSAGE, LABELER_WASM] {
        for (name, bytes) in &entries {
            assert!(
                !String::from_utf8_lossy(bytes).contains(needle),
                "{name} carries a withheld residual-plane row's contents"
            );
        }
    }

    // (c) The one that rides in part — the split asserted on the bytes.
    let (_, push_rows) = entries
        .iter()
        .find(|(n, _)| n == &name_of("push_subscriptions"))
        .expect("push_subscriptions is Redacted, not withheld — the registration itself rides");
    let push = String::from_utf8_lossy(push_rows);
    assert!(
        push.contains("device-a") && push.contains("webpush"),
        "the push REGISTRATION rides: which device, what transport, since when"
    );
    for (col, secret) in [
        ("endpoint", PUSH_ENDPOINT),
        ("key_auth", PUSH_AUTH),
        ("key_p256dh", PUSH_P256DH),
    ] {
        for (name, bytes) in &entries {
            let haystack = String::from_utf8_lossy(bytes);
            assert!(
                !haystack.contains(secret) && !haystack.contains(&hex::encode(secret)),
                "{name} carries push_subscriptions.{col} — the endpoint and its two keys are \
                 together a sendable Web Push credential, the dispatcher authenticates nobody, \
                 and NO kind this nest has can revoke a rogue endpoint. In an archive an \
                 eviction token can fetch, that is an unrevokable delivery capability"
            );
        }
    }

    // None of the eight is still an open judgment.
    let (_, manifest_bytes) = entries
        .iter()
        .find(|(n, _)| n == "export/manifest.json")
        .expect("manifest.json");
    let manifest: serde_json::Value = serde_json::from_slice(manifest_bytes).unwrap();
    let unreviewed = manifest["coverage"]["unreviewed_tables"]
        .as_array()
        .expect("unreviewed_tables array");
    for table in [
        "notifications",
        "push_subscriptions",
        "invite_requests",
        "labelers",
        "labeler_subscriptions",
        "nest_pairings",
        "import_sessions",
        "obligation_action_records",
    ] {
        assert!(
            !unreviewed.iter().any(|t| t == table),
            "{table} was ruled — it must not still be declared Unreviewed"
        );
    }
}

/// The mail spools never reach the archive — asserted on the BYTES, not on the
/// verdict.
///
/// **Why the bytes.** `forward_queue.raw_message` and
/// `outbound_mail_queue.raw_message` are full RFC822 messages in *plaintext* at
/// rest, while the owner's durable copy of the same mail rests sealed in their
/// mailbox. So the cost of a wrong verdict here is not "a table the owner
/// cannot see" but "an unsealed copy of mail that is sealed everywhere else,
/// inside an archive an **eviction export token** can fetch". The ruling made
/// both `WithheldOperational` on the class argument — a spool is delivery
/// bookkeeping — and this test is what makes the ruling hold under a later
/// session that disagrees: promote either verdict to `Verbatim` and the
/// plaintext body appears in an archive entry, which reds here, whatever the
/// declaration then says. It is the `export_never_carries_a_secret_bearing_table`
/// shape (a check kept deliberately apart from the registry it audits), applied
/// to the one non-secret plane whose export would still weaken mail at rest.
#[tokio::test]
async fn the_mail_spools_never_reach_the_archive() {
    let (base, state) = start_server().await;
    let kp = ActorKeypair::generate();
    let actor_id = kp.actor_id();
    state
        .db
        .create_user(&actor_id.0, "free", "spool")
        .await
        .unwrap();
    let token = state.auth.token_store.insert(actor_id, 3600).await;

    const PARKED_BODY: &str = "Subject: parked-forward-plaintext-body\r\n\r\nsealed nowhere";
    const QUEUED_BODY: &str = "Subject: queued-outbound-plaintext-body\r\n\r\nsealed nowhere";
    {
        let conn = state.db.conn().await;
        conn.execute(
            "INSERT INTO forward_queue
                 (actor_id, queued_at, source_message_id, original_sender,
                  destination_address, rule_id_or_forward_all, raw_message)
             VALUES (?1, ?2, 'msg-0001', 'someone@example.org', 'tap@example.net',
                     'forward-all', ?3)",
            rusqlite::params![
                actor_id.0.to_vec(),
                1_755_000_000_i64,
                PARKED_BODY.as_bytes().to_vec()
            ],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO outbound_mail_queue
                 (original_msgid, original_sender, recipient, raw_message, next_attempt_at,
                  is_forwarded, forward_actor_id, created_at)
             VALUES ('msg-0002', 'me@example.org', 'tap@example.net', ?1, ?2, 1, ?3, ?2)",
            rusqlite::params![
                QUEUED_BODY.as_bytes().to_vec(),
                1_755_000_001_i64,
                actor_id.0.to_vec()
            ],
        )
        .unwrap();
    }

    let entries = read_archive(&export_zip(&base, &token).await);

    // Not as an entry...
    for table in ["forward_queue", "outbound_mail_queue"] {
        assert!(
            !entries
                .iter()
                .any(|(n, _)| n == &format!("export/tables/{table}.ndjson")),
            "{table} is WithheldOperational — a seeded row must not produce an archive entry"
        );
    }

    // ...and not as bytes, under either encoding, anywhere in the archive.
    for body in [PARKED_BODY, QUEUED_BODY] {
        let hex_body = hex::encode(body.as_bytes());
        for (name, bytes) in &entries {
            let haystack = String::from_utf8_lossy(bytes);
            assert!(
                !haystack.contains(body) && !haystack.contains(&hex_body),
                "{name} carries a spooled plaintext mail body — the owner's durable copy of \
                 this message rests sealed, so the archive must not be the place it rests \
                 unsealed"
            );
        }
    }

    // Both are declared, by name and class: the owner is told the plane exists
    // and why it is absent, which is the half of rule 4 an omission cannot do.
    let (_, manifest_bytes) = entries
        .iter()
        .find(|(n, _)| n == "export/manifest.json")
        .expect("manifest.json");
    let manifest: serde_json::Value = serde_json::from_slice(manifest_bytes).unwrap();
    let withheld = manifest["coverage"]["withheld_tables"]
        .as_array()
        .expect("withheld_tables array");
    for table in ["forward_queue", "outbound_mail_queue"] {
        let declared = withheld
            .iter()
            .find(|w| w["table"] == table)
            .unwrap_or_else(|| panic!("{table} must be declared withheld by name"));
        assert_eq!(declared["reason_class"], "operational");
    }
}

/// The report plane never reaches an archive.
///
/// `content_reports` is keyed on `reporter`, so an actor-keyed read would
/// return only the exporter's OWN flags — and it is withheld anyway, because
/// its owner doc allows no second read surface: `report-sharing.md` § One gate
/// function routes every read through `exposed_report_count`, and § Report
/// capture rules that below k nothing is readable anywhere, "not in any
/// export". Asserting on the archive's entries and the manifest's declaration,
/// rather than on the registry verdict alone, is what makes that survive a
/// later disagreement. (The plane's reported-party table, `sender_reputation`,
/// left with the federation reputation leg.)
#[tokio::test]
async fn the_report_plane_never_reaches_an_archive() {
    let (base, state) = start_server().await;
    let kp = ActorKeypair::generate();
    let actor_id = kp.actor_id();
    state
        .db
        .create_user(&actor_id.0, "free", "reporter")
        .await
        .unwrap();
    let token = state.auth.token_store.insert(actor_id, 3600).await;

    // The exporter's own flag on the k-anonymity table.
    const CONTENT_HASH: [u8; 32] = [0xAB; 32];
    {
        let conn = state.db.conn().await;
        conn.execute(
            "INSERT INTO content_reports
                 (content_hash, factor, reporter, content_kind, created_at)
             VALUES (?1, 'spam', ?2, 'post', ?3)",
            rusqlite::params![
                CONTENT_HASH.to_vec(),
                actor_id.0.to_vec(),
                1_755_000_001_i64
            ],
        )
        .unwrap();
    }

    let entries = read_archive(&export_zip(&base, &token).await);

    assert!(
        !entries
            .iter()
            .any(|(n, _)| n == "export/tables/content_reports.ndjson"),
        "content_reports is withheld — a seeded row must not produce an archive entry"
    );
    let hex_hash = hex::encode(CONTENT_HASH);
    for (name, bytes) in &entries {
        assert!(
            !String::from_utf8_lossy(bytes).contains(&hex_hash),
            "{name} carries a reported item's hash — `report-sharing.md` § One gate \
             function: there is no read surface but the gate"
        );
    }

    let (_, manifest_bytes) = entries
        .iter()
        .find(|(n, _)| n == "export/manifest.json")
        .expect("manifest.json");
    let manifest: serde_json::Value = serde_json::from_slice(manifest_bytes).unwrap();
    let withheld = manifest["coverage"]["withheld_tables"]
        .as_array()
        .expect("withheld_tables array");
    let declared = withheld
        .iter()
        .find(|w| w["table"] == "content_reports")
        .expect("content_reports must be declared withheld by name");
    assert_eq!(declared["reason_class"], "operational");
}

/// The ATProto PDS plane: the repo and its surrounding
/// state ride, the consent CEREMONY does not.
///
/// **Why this is hand-written rather than left to the registry guards.** The
/// ruling measured the axis's guard set as asymmetric — a wrong `Withheld*` class
/// reds nothing, because withholding is the resting state every table had before
/// this axis existed — and a later pass sharpened it to the strong form: a
/// wrong-OWNER verdict reds nothing in *either* direction. So
/// `atproto_consent_requests` being `WithheldOperational` is a claim no
/// generated check can falsify, and the six `Verbatim` verdicts beside it are
/// the ones that would leak if a later session flipped them wrongly. Both
/// directions are asserted here on the BYTES.
///
/// **The ceremony/authority split is the ruling under test.** The durable record
/// of what the owner agreed to is an `atproto_oauth_grants` row, which is
/// `Verbatim` so the connected-apps audit `principles.md` § The user always
/// controls their data requires reaches the archive. The consent request is only
/// the minutes-long question that minted it, swept by expiry however it was
/// answered. Promote it and the approval card's contents — including the binding
/// `code` — appear in an archive an eviction export token can fetch; that reds
/// here whatever the declaration then says.
///
/// ⚠ `rkey` rides deliberately: it is the ATProto record key, a path segment,
/// and the assertion below is what makes its `CLEARED_EXPORTING_COLUMNS` entry
/// a statement about this export rather than about the fragment list.
#[tokio::test]
async fn the_atproto_repo_rides_while_the_consent_ceremony_stays_home() {
    let (base, state) = start_server().await;
    let kp = ActorKeypair::generate();
    let actor_id = kp.actor_id();
    state
        .db
        .create_user(&actor_id.0, "free", "pdsowner")
        .await
        .unwrap();
    let token = state.auth.token_store.insert(actor_id, 3600).await;

    const DID: &str = "did:plc:owners-own-hosted-identity";
    const RETIRED_DID: &str = "did:plc:the-only-surviving-record-of-this-one";
    const RKEY: &str = "3jzfcijpj2z2a";
    const RECORD_TEXT: &str = "the-owners-own-published-post-bytes";
    const PREFS: &str = "the-owners-opaque-preferences-payload";
    const BLOB_CID: &str = "bafkreiOWNERS-own-uploaded-media";
    const CONSENT_CODE: &str = "the-binding-code-both-surfaces-show";
    const CONSENT_CLIENT: &str = "https://an-external-app.example/client-metadata.json";
    const GRANT_CLIENT: &str = "https://the-app-they-actually-approved.example";

    {
        let conn = state.db.conn().await;
        conn.execute_batch("PRAGMA foreign_keys = OFF;").unwrap();
        let actor = actor_id.0.to_vec();

        conn.execute(
            "INSERT INTO atproto_identities
                 (actor_id, method, status, did, user_rotation_pub, signing_pub,
                  bridge_rotation_pub, created_at, updated_at)
             VALUES (?1, 'plc', 'active', ?2, 'zROTATION-PUB', 'zSIGNING-PUB',
                     'zBRIDGE-ROTATION-PUB', ?3, ?3)",
            rusqlite::params![actor.clone(), DID, 1_755_100_000_i64],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO atproto_account_settings
                 (actor_id, external_apps_enabled, integration_level, updated_at)
             VALUES (?1, 1, 'hosted_full', ?2)",
            rusqlite::params![actor.clone(), 1_755_100_001_i64],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO atproto_native_records
                 (actor_id, collection, rkey, cid, record, created_at)
             VALUES (?1, 'app.bsky.feed.post', ?2, 'bafyreiRECORD', ?3, ?4)",
            rusqlite::params![
                actor.clone(),
                RKEY,
                RECORD_TEXT.as_bytes().to_vec(),
                1_755_100_002_i64
            ],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO atproto_preferences (actor_id, preferences, updated_at)
             VALUES (?1, ?2, ?3)",
            rusqlite::params![actor.clone(), PREFS.as_bytes().to_vec(), 1_755_100_003_i64],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO atproto_blobs (actor_id, cid, media_ref, created_at, referenced_at)
             VALUES (?1, ?2, ?3, ?4, ?4)",
            rusqlite::params![actor.clone(), BLOB_CID, vec![0xB1u8; 32], 1_755_100_004_i64],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO atproto_retired_identities
                 (actor_id, method, did, user_rotation_pub, created_at, retired_at)
             VALUES (?1, 'plc', ?2, 'zOLD-ROTATION-PUB', ?3, ?4)",
            rusqlite::params![
                actor.clone(),
                RETIRED_DID,
                1_755_000_000_i64,
                1_755_100_005_i64
            ],
        )
        .unwrap();

        // The authority the owner really granted — Verbatim, and the reason the
        // ceremony beside it can stay home without losing the audit.
        conn.execute(
            "INSERT INTO atproto_oauth_grants
                 (actor_id, grant_id, client_id, scopes, created_at, issuer)
             VALUES (?1, ?2, ?3, 'atproto transition:generic', ?4, 'nest')",
            rusqlite::params![
                actor.clone(),
                vec![0xA7u8; 32],
                GRANT_CLIENT,
                1_755_100_006_i64
            ],
        )
        .unwrap();
        // The ceremony itself, seeded RESOLVED so the row is as export-worthy
        // as this table ever gets — a pending one would be the easy case.
        conn.execute(
            "INSERT INTO atproto_consent_requests
                 (consent_id, actor_id, code, client_id, client_name, scopes,
                  created_at, expires_at, resolved_at, approved)
             VALUES (?1, ?2, ?3, ?4, 'An External App', 'atproto', ?5, ?6, ?5, 1)",
            rusqlite::params![
                vec![0xC0u8; 32],
                actor,
                CONSENT_CODE,
                CONSENT_CLIENT,
                1_755_100_007_i64,
                1_755_900_000_i64
            ],
        )
        .unwrap();
    }

    let entries = read_archive(&export_zip(&base, &token).await);
    let name_of = |t: &str| format!("export/tables/{t}.ndjson");

    // (a) The six that ride.
    for table in [
        "atproto_identities",
        "atproto_account_settings",
        "atproto_native_records",
        "atproto_preferences",
        "atproto_blobs",
        "atproto_retired_identities",
    ] {
        assert!(
            entries.iter().any(|(n, _)| n == &name_of(table)),
            "{table} is Verbatim — a seeded row must produce \
             `export/tables/{table}.ndjson`"
        );
    }

    // The content, not just the filename: an empty ndjson would satisfy the
    // check above while exporting nothing the owner asked for.
    let rows_of = |t: &str| {
        let (_, bytes) = entries
            .iter()
            .find(|(n, _)| n == &name_of(t))
            .unwrap_or_else(|| panic!("{t} entry"));
        String::from_utf8_lossy(bytes).into_owned()
    };
    assert!(
        rows_of("atproto_native_records").contains(RKEY),
        "`rkey` is the ATProto record key — a path segment, not key material. It is cleared \
         in CLEARED_EXPORTING_COLUMNS and must actually ride, or the clearance describes \
         nothing"
    );
    assert!(
        rows_of("atproto_identities").contains(DID)
            && rows_of("atproto_identities").contains("zSIGNING-PUB"),
        "the hosted DID and the PUBLISHED key halves ride — they are what the PLC directory \
         serves to anyone who resolves the DID (the rule: a succession reason about \
         COMPROMISE says nothing about DISCLOSURE)"
    );
    assert!(
        rows_of("atproto_retired_identities").contains(RETIRED_DID),
        "a retired DID resolves nowhere, so this row is the only surviving record that the \
         owner ever published that identity — user-irrecoverable by its own schema comment"
    );
    assert!(
        rows_of("atproto_account_settings").contains("hosted_full"),
        "the integration level is the owner's own consent posture, not an authority"
    );

    // (b) The ceremony does not ride at all — asserted on the bytes, across
    //     every entry, so a future `Verbatim` cannot slip it in elsewhere.
    assert!(
        !entries
            .iter()
            .any(|(n, _)| n == &name_of("atproto_consent_requests")),
        "atproto_consent_requests is WithheldOperational — the ceremony, whose durable \
         outcome is the atproto_oauth_grants row beside it"
    );
    for needle in [CONSENT_CODE, CONSENT_CLIENT] {
        for (name, bytes) in &entries {
            assert!(
                !String::from_utf8_lossy(bytes).contains(needle),
                "{name} carries a consent-request column. The request is a minutes-long \
                 question swept by expiry however it was answered; what the owner durably \
                 agreed to is the grant row, which rides. An archive an eviction token can \
                 fetch must not also carry the binding code the approval card showed"
            );
        }
    }

    // (c) The audit the withholding relies on is really there — otherwise (b)
    //     would be dropping the connected-apps record `principles.md` requires,
    //     not merely the ceremony that minted it.
    assert!(
        rows_of("atproto_oauth_grants").contains(GRANT_CLIENT),
        "the standing authority rides: withholding the ceremony is only sound because the \
         grant it produced is Verbatim"
    );

    // None of the seven is still an open judgment.
    let (_, manifest_bytes) = entries
        .iter()
        .find(|(n, _)| n == "export/manifest.json")
        .expect("manifest.json");
    let manifest: serde_json::Value = serde_json::from_slice(manifest_bytes).unwrap();
    let unreviewed = manifest["coverage"]["unreviewed_tables"]
        .as_array()
        .expect("unreviewed_tables array");
    for table in [
        "atproto_identities",
        "atproto_account_settings",
        "atproto_native_records",
        "atproto_preferences",
        "atproto_blobs",
        "atproto_retired_identities",
        "atproto_consent_requests",
    ] {
        assert!(
            !unreviewed.iter().any(|t| t == table),
            "{table} was ruled — it must not still be declared Unreviewed"
        );
    }
    let withheld = manifest["coverage"]["withheld_tables"]
        .as_array()
        .expect("withheld_tables array");
    let declared = withheld
        .iter()
        .find(|w| w["table"] == "atproto_consent_requests")
        .expect("atproto_consent_requests must be declared withheld by name");
    assert_eq!(declared["reason_class"], "operational");
}

/// The LAST verdict: the segment mirror rides, and the
/// conversation records the per-actor walk cannot see stay out of *its* reach.
///
/// ⚠ **Updated.** The conv records are no longer absent from the
/// archive outright — they ride the membership-resolved `conversations` domain
/// (`the_owners_conv_records_ride_the_membership_resolved_domain` below). What
/// this test pins is the walk's own boundary plus the access rule: this
/// exporter is deliberately *not* a member of the seeded channel.
///
/// **Why this is hand-written.** Same asymmetry as every demotable verdict on
/// this axis: nothing generated reds if
/// `segment_records` is quietly demoted to a `Withheld*` class, because
/// withholding is the resting state every table had before the axis existed.
/// The `Unreviewed` direction *is* covered — the exact-count ratchet and the
/// manifest's backlog declaration both red — so this test exists for the other
/// one.
///
/// **It also pins the fact that makes `Verbatim` safe.** `scope_id` carries two
/// kinds of identity since the Plan 6 T4 rename: `mail` scopes to the recipient
/// actor, `conv` to the **channel**. Both are seeded here through the
/// production writers (`records_db::insert_mail` / `insert_conv`), and the
/// assertion is that the walk returns the first and cannot reach the second —
/// which is the safety property: no shared scope is ever walked as if it were
/// the actor's. If a later session "fixes" anything by keying a registry entry
/// on a channel column, this test is what should stop them — that shape stayed
/// forbidden when row 157 closed the gap the other way.
#[tokio::test]
async fn the_segment_mirror_rides_while_the_channel_scope_stays_out_of_reach() {
    use fauna_nest::segments::records_db;

    let (base, state) = start_server().await;
    let kp = ActorKeypair::generate();
    let actor_id = kp.actor_id();
    state
        .db
        .create_user(&actor_id.0, "free", "segmentowner")
        .await
        .unwrap();
    let token = state.auth.token_store.insert(actor_id, 3600).await;

    const SENDER_DOM: &str = "their-correspondents-domain.example";
    const OWN_BUCKET: &str = "the-owners-own-mail-bucket";
    const CONV_BUCKET: &str = "a-channel-scoped-conv-bucket";
    let mail_cid = fauna_cbor::Cid::from_digest_dag_cbor([0xA1u8; 32]);
    let conv_cid = fauna_cbor::Cid::from_digest_dag_cbor([0xC2u8; 32]);
    // A channel id is not an actor id — that is the whole point of the second
    // seeding, so it must not be the exporter's own bytes.
    let channel_id = [0xEEu8; 32];

    {
        let conn = state.db.conn().await;
        records_db::insert_mail(
            &conn,
            &actor_id.0,
            1,
            &mail_cid,
            OWN_BUCKET,
            1_755_300_000_000,
            SENDER_DOM,
            "ham",
            false,
            1,
            None,
            0,
            1_755_300_000,
        )
        .expect("seed the owner's own mail record");
        records_db::insert_conv(
            &conn,
            &channel_id,
            2,
            &conv_cid,
            CONV_BUCKET,
            1_755_300_001,
            1,
        )
        .expect("seed a channel-scoped conv record");
    }

    let entries = read_archive(&export_zip(&base, &token).await);
    let name = "export/tables/segment_records.ndjson";
    let (_, bytes) = entries
        .iter()
        .find(|(n, _)| n == name)
        .expect("segment_records is Verbatim — a seeded row must emit");
    let rows = String::from_utf8_lossy(bytes).into_owned();

    // (a) The mirror rides: the owner's own placement, by content address.
    assert!(
        rows.contains(&hex::encode(mail_cid.as_bytes())) && rows.contains(OWN_BUCKET),
        "the owner's own mail placement rides -- `record_cid` is a content ADDRESS into the \
         segment store (the `content.blob_hash` class), not key material, and the bucket is \
         where their own record sits"
    );
    assert!(
        rows.contains(SENDER_DOM),
        "the mail floor rides: `sender_dom` is the correspondent domain, the `contacts.peer_id` \
         class that has always ridden"
    );

    // (b) The channel scope is out of reach of THIS walk — the safety property.
    //     A per-actor walk must never return a scope shared with other
    //     participants, and this half is unchanged by the close.
    assert!(
        !rows.contains(&hex::encode(conv_cid.as_bytes())) && !rows.contains(CONV_BUCKET),
        "a `conv` record keys on the CHANNEL, not the actor, so a `scope_id = ?actor` walk must \
         not reach it. If this fires, either a registry entry was keyed on a channel column (the \
         family plane's forbidden shape) or the conv writer started scoping to an actor -- both \
         change who a per-actor export can see"
    );

    // (b2) **And membership is the access rule**. This actor was never
    //      registered on `channel_id`, so no surface in the archive may carry
    //      that record — not the walk, and not the conversations domain either.
    //      ⚠ This used to be an unconditional sweep ("no export surface may
    //      reach a shared scope"), which was true only while nothing resolved
    //      membership at all. Row 157 built the domain that does, so the claim
    //      is now the sharper one: reach is bounded by membership, and the
    //      positive half is pinned in
    //      `the_owners_conv_records_ride_the_membership_resolved_domain` below.
    for (n, b) in &entries {
        assert!(
            !String::from_utf8_lossy(b).contains(CONV_BUCKET),
            "{n} carries a channel-scoped conv record for an actor who is not a member of that \
             channel. The conversations domain's reach is `actor_channels` membership; if this \
             fires, that resolution was widened or dropped"
        );
    }

    // (c) The milestone itself: no table is an open judgment any more, and the
    //     flag that says so must not be mistaken for a completeness claim.
    let (_, manifest_bytes) = entries
        .iter()
        .find(|(n, _)| n == "export/manifest.json")
        .expect("manifest.json");
    let manifest: serde_json::Value = serde_json::from_slice(manifest_bytes).unwrap();
    assert!(
        manifest["coverage"]["unreviewed_tables"]
            .as_array()
            .expect("unreviewed_tables array")
            .is_empty()
            && manifest["coverage"]["partial"] == false,
        "the ruling drained the backlog: no table is Unreviewed, so the partiality flag is false \
         -- which declares that no OPEN JUDGMENT remains, never that the archive is complete. \
         The segment BYTES are still absent for every kind; the conv \
         RECORDS stopped being absent when row 157 built the membership-resolved domain"
    );
    assert!(
        !manifest["coverage"]["withheld_tables"]
            .as_array()
            .expect("withheld_tables array")
            .is_empty(),
        "the withheld declaration is what carries the honest remainder once `partial` is false; \
         an empty list here would mean the archive claims to hold everything"
    );
}

/// **Row 157** — the owner's conversation records ride the membership-resolved
/// `conversations` domain, all the way into the archive.
///
/// **Why this is hand-written, and why the DB-level tests are not enough.** The
/// unit tests in `db::actor_tables` pin the reader (`gather_conv_records`): the
/// kind filter, the membership access rule, the no-double-emit property. None of
/// them observes the *zip entry*, so the whole domain could be built, ruled and
/// unit-tested while `write_export_zip` never wrote it — a capability exported
/// nowhere, which is exactly the dark-entry-point class. This test drives the
/// real HTTP endpoint and reads the archive, so the last link is witnessed.
///
/// It is the positive half of
/// `the_segment_mirror_rides_while_the_channel_scope_stays_out_of_reach`'s (b2):
/// there the exporter is not a member and nothing reaches them; here they are,
/// and the record does — through a door keyed on `actor_channels`, never on a
/// channel column in `ACTOR_TABLES`.
#[tokio::test]
async fn the_owners_conv_records_ride_the_membership_resolved_domain() {
    use fauna_nest::segments::records_db;

    let (base, state) = start_server().await;
    let kp = ActorKeypair::generate();
    let actor_id = kp.actor_id();
    state
        .db
        .create_user(&actor_id.0, "free", "convowner")
        .await
        .unwrap();
    let token = state.auth.token_store.insert(actor_id, 3600).await;

    const MEMBER_BUCKET: &str = "a-bucket-on-a-channel-they-are-on";
    const STRANGER_BUCKET: &str = "a-bucket-on-a-channel-they-are-not-on";
    let member_cid = fauna_cbor::Cid::from_digest_dag_cbor([0x11u8; 32]);
    let stranger_cid = fauna_cbor::Cid::from_digest_dag_cbor([0x22u8; 32]);
    // Neither channel id is the exporter's own bytes — a channel scope is not an
    // actor scope, which is the fact the whole domain exists to bridge.
    let mine = [0xA7u8; 32];
    let theirs = [0xB8u8; 32];

    // Membership in exactly one of the two channels.
    state
        .db
        .register_actor_channel(&actor_id.0, &mine)
        .await
        .unwrap();

    {
        let conn = state.db.conn().await;
        records_db::insert_conv(
            &conn,
            &mine,
            1,
            &member_cid,
            MEMBER_BUCKET,
            1_755_300_010,
            1,
        )
        .expect("seed a conv record on a channel the exporter is on");
        records_db::insert_conv(
            &conn,
            &theirs,
            2,
            &stranger_cid,
            STRANGER_BUCKET,
            1_755_300_011,
            1,
        )
        .expect("seed a conv record on a channel the exporter is NOT on");
    }

    let entries = read_archive(&export_zip(&base, &token).await);
    let name = "export/conversations/records.ndjson";
    let (_, bytes) = entries.iter().find(|(n, _)| n == name).unwrap_or_else(|| {
        panic!(
            "the conversations domain must write {name}; the archive holds: {:?}",
            entries.iter().map(|(n, _)| n).collect::<Vec<_>>()
        )
    });
    let rows = String::from_utf8_lossy(bytes).into_owned();

    // (a) The record on their own channel rides, by content address and bucket,
    //     and the row names the CHANNEL it is scoped to so the archive is
    //     self-describing about which key produced it.
    assert!(
        rows.contains(&hex::encode(member_cid.as_bytes())) && rows.contains(MEMBER_BUCKET),
        "the exporter's conv placement rides the membership door: {rows}"
    );
    assert!(
        rows.contains(&hex::encode(mine)),
        "each row carries its own `scope_id` -- the channel -- so a reader can tell these rows \
         from the actor-keyed ones in tables/segment_records.ndjson: {rows}"
    );

    // (b) Membership BOUNDS the reach: the other channel's record is in the same
    //     table, one `SELECT` away, and no surface in the archive may carry it.
    //     This is the property a channel-column registry entry would have
    //     destroyed while still reading as if "the rows are the actor's".
    for (n, b) in &entries {
        assert!(
            !String::from_utf8_lossy(b).contains(STRANGER_BUCKET),
            "{n} carries a conv record from a channel this exporter is not a member of. The \
             domain resolves `actor_channels` first, and that resolution IS the access rule"
        );
    }

    // (c) No double emission: the actor-keyed walk and the membership door read
    //     one table on two keys, and the walk must not have reached the conv row.
    if let Some((_, walked)) = entries
        .iter()
        .find(|(n, _)| n == "export/tables/segment_records.ndjson")
    {
        assert!(
            !String::from_utf8_lossy(walked).contains(MEMBER_BUCKET),
            "the conv record must reach the archive through exactly ONE door. Finding it in the \
             registry walk's file too means someone keyed an entry on a channel column"
        );
    }
}

/// The eleven feature-gated bridge tables: the owner's
/// LINKS and the things they did through them ride, their bridge CREDENTIALS
/// and the pollers' CURSORS do not.
///
/// **Why this is hand-written, and why it is the most load-bearing pin on the
/// axis so far.** The ruling measured the guard set as asymmetric (a wrong
/// `Withheld*` class reds nothing) and it gave it the strong form (a
/// wrong-OWNER verdict reds nothing in *either* direction) — so
/// `nostr_federation_cursors` being `WithheldOperational` is a claim no
/// generated check can falsify. On top of that, this whole plane
/// is invisible to a bare `cargo test`: all three bridges are opt-in, so every
/// registry guard skips these tables as absent and says so only in its
/// `skipped_gated` list. Without this test the plane's verdicts would rest on
/// reading alone.
///
/// **The seeding runs each bridge's real `init_db`** rather than re-spelling
/// `CREATE TABLE`, so the schema under test is production's, `ALTER`s included
/// (`bluesky_accounts.write_through`, `ap_post_map.remote_actor_uri`) — the
/// same faithfulness restored to the registry guards'
/// own seeding, bought by construction for enrollment (a schema guard pin
/// elsewhere reds if either ALTER goes missing) AND, since, by an explicit value-level assertion each below — a pin's prose
/// states the property, only its fixtures state the coverage.
///
/// ⚠ **The two `Redacted` verdicts are seeded with NON-EMPTY credential
/// bytes on purpose.** In production `upsert_linked_account` writes
/// `bluesky_accounts`' three token columns EMPTY, so a `Verbatim` verdict would
/// pass a test that seeded them the way production does — and would start
/// leaking the day a writer changed. Filling them here is what makes the
/// redaction a property of the verdict rather than of the current writer.
#[cfg(all(feature = "nostr", feature = "bluesky", feature = "activitypub"))]
#[tokio::test]
async fn the_bridge_links_ride_while_their_credentials_and_cursors_stay_home() {
    let (base, state) = start_server().await;
    let kp = ActorKeypair::generate();
    let actor_id = kp.actor_id();
    state
        .db
        .create_user(&actor_id.0, "free", "bridgeowner")
        .await
        .unwrap();
    let token = state.auth.token_store.insert(actor_id, 3600).await;

    // Production's own schema path — never a re-spelling of CREATE TABLE.
    fauna_nest::activitypub::init_db(&state.db).await.unwrap();
    fauna_nest::bluesky::init_db(&state.db).await.unwrap();
    fauna_nest::nostr::init_db(&state.db).await.unwrap();

    // Secrets: must appear in NO archive entry.
    const AP_PRIVKEY: &[u8] = b"the-rsa-key-that-signs-as-this-actor";
    const BSKY_ACCESS: &[u8] = b"a-live-bluesky-access-token";
    const BSKY_REFRESH: &[u8] = b"a-live-bluesky-refresh-token";
    const BSKY_DPOP: &[u8] = b"a-live-dpop-binding-key";
    // Withheld-table payloads: must likewise appear nowhere.
    const PEER_NEST: &str = "the-federation-peer-box-watermark";
    // Owner data + cleared columns: must ride.
    const AP_USERNAME: &str = "the-owners-own-ap-handle";
    const AP_PUBKEY_PEM: &str = "-----BEGIN PUBLIC KEY-----the-published-half";
    const AP_FOLLOW_URI: &str = "https://remote.example/users/someone-they-follow";
    const AP_URL: &str = "https://this.example/ap/objects/their-own-post";
    // `ap_post_map.remote_actor_uri` is NULL on every row a real exporter's
    // `actor_id = ?` match returns in production (actor_tables.rs's own
    // ruling: it is populated only on synthetic-author inbound rows, which
    // belong to actors that can never authenticate to export at all) — seeded
    // here anyway, on the same row, purely to witness that the EXPORT CODE
    // carries this ALTER'd column's byte value through when present; not a
    // claim this combination occurs on a real box.
    const AP_POST_REMOTE_ACTOR_URI: &str = "https://remote.example/users/who-they-crossposted-to";
    const BSKY_HANDLE: &str = "theirhandle.bsky.social";
    const BSKY_FEED: &str = "at://did:plc:x/app.bsky.feed.generator/their-saved-feed";
    const BSKY_RECORD_URI: &str = "at://did:plc:x/app.bsky.feed.like/their-own-like";
    const DM_SEALED: &[u8] = b"the-sealed-dm-body-only-their-client-opens";
    const DM_PEER: &str = "npubTHE-correspondent-they-talked-to";
    const FOLLOW_PETNAME: &str = "the-petname-they-chose-themselves";

    let hex = hex::encode(actor_id.0);
    {
        let conn = state.db.conn().await;
        conn.execute_batch("PRAGMA foreign_keys = OFF;").unwrap();

        conn.execute(
            "INSERT INTO ap_accounts
                 (actor_id, username, actor_url, encrypted_privkey, public_key_pem,
                  created_at, updated_at)
             VALUES (?1, ?2, 'https://this.example/ap/users/x', ?3, ?4, ?5, ?5)",
            rusqlite::params![
                hex.clone(),
                AP_USERNAME,
                AP_PRIVKEY.to_vec(),
                AP_PUBKEY_PEM,
                1_755_200_000_i64
            ],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO ap_follows
                 (local_actor_id, remote_actor_uri, direction, created_at)
             VALUES (?1, ?2, 'outbound', ?3)",
            rusqlite::params![hex.clone(), AP_FOLLOW_URI, 1_755_200_001_i64],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO ap_post_map
                 (fauna_post_id, ap_url, actor_id, created_at, remote_actor_uri)
             VALUES ('their-post', ?1, ?2, ?3, ?4)",
            rusqlite::params![
                AP_URL,
                hex.clone(),
                1_755_200_002_i64,
                AP_POST_REMOTE_ACTOR_URI
            ],
        )
        .unwrap();

        conn.execute(
            "INSERT INTO bluesky_accounts
                 (actor_id, bluesky_did, bluesky_handle, access_token, refresh_token,
                  dpop_key, token_expires, created_at, updated_at, write_through)
             VALUES (?1, 'did:plc:theirs', ?2, ?3, ?4, ?5, ?6, ?7, ?7, 1)",
            rusqlite::params![
                hex.clone(),
                BSKY_HANDLE,
                BSKY_ACCESS.to_vec(),
                BSKY_REFRESH.to_vec(),
                BSKY_DPOP.to_vec(),
                1_755_900_000_i64,
                1_755_200_003_i64
            ],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO bluesky_interactions
                 (actor_id, fauna_post_id, interaction_type, record_uri, created_at)
             VALUES (?1, 'p1', 'like', ?2, ?3)",
            rusqlite::params![hex.clone(), BSKY_RECORD_URI, 1_755_200_004_i64],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO bluesky_saved_feeds
                 (actor_id, feed_uri, display_name, saved_at)
             VALUES (?1, ?2, 'A Feed They Saved', ?3)",
            rusqlite::params![hex.clone(), BSKY_FEED, 1_755_200_005_i64],
        )
        .unwrap();

        // The Nostr DM leg's rows live in the bridged-conversation family
        // since schema 118: a room per peer, the sealed body on its row.
        conn.execute(
            "INSERT INTO bridge_conversation_rooms
                 (room_id, actor_id, bridge_principal_id, bridge_id, far_room_id,
                  participants, capabilities, bridge_x25519, created_at, last_at)
             VALUES (x'D1', ?1, x'51', 'nostr', ?2, ?3, '{}', x'00', ?4, ?4)",
            rusqlite::params![
                &actor_id.0[..],
                DM_PEER,
                format!("[\"{DM_PEER}\"]"),
                1_755_200_006_i64
            ],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO bridge_conversation_messages
                 (room_id, actor_id, direction, sender, sealed_content, created_at,
                  received_at)
             VALUES (x'D1', ?1, 'in', ?2, ?3, ?4, ?4)",
            rusqlite::params![
                &actor_id.0[..],
                DM_PEER,
                DM_SEALED.to_vec(),
                1_755_200_006_i64
            ],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO nostr_federation_cursors
                 (actor_id, peer_nest_id, push_id, pull_id)
             VALUES (?1, ?2, 'push-cursor', 'pull-cursor')",
            rusqlite::params![hex.clone(), PEER_NEST],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO nostr_follows (actor_id, nostr_pubkey, petname, created_at)
             VALUES (?1, 'npubTHEIR-CONTACT', ?2, ?3)",
            rusqlite::params![hex.clone(), FOLLOW_PETNAME, 1_755_200_007_i64],
        )
        .unwrap();
    }

    let entries = read_archive(&export_zip(&base, &token).await);
    let name_of = |t: &str| format!("export/tables/{t}.ndjson");
    let rows_of = |t: &str| {
        let (_, bytes) = entries
            .iter()
            .find(|(n, _)| n == &name_of(t))
            .unwrap_or_else(|| panic!("{t} entry"));
        String::from_utf8_lossy(bytes).into_owned()
    };

    // (a) The exporting tables each produce an entry.
    for table in [
        "ap_accounts",
        "ap_follows",
        "ap_post_map",
        "bluesky_accounts",
        "bluesky_interactions",
        "bluesky_saved_feeds",
        "bridge_conversation_messages",
        "bridge_conversation_rooms",
        "nostr_follows",
    ] {
        assert!(
            entries.iter().any(|(n, _)| n == &name_of(table)),
            "{table} exports here — a seeded row must produce \
             `export/tables/{table}.ndjson`"
        );
    }

    // (b) The owner's own things really ride — the filename alone would be
    //     satisfied by an empty ndjson.
    assert!(
        rows_of("ap_accounts").contains(AP_USERNAME)
            && rows_of("ap_accounts").contains(AP_PUBKEY_PEM),
        "the owner's AP identity rides, and `public_key_pem` with it: the AP actor endpoint \
         already serves that half to the whole fediverse unauthenticated, which is what its \
         CLEARED_EXPORTING_COLUMNS entry claims — so it must actually appear here"
    );
    assert!(
        rows_of("ap_follows").contains(AP_FOLLOW_URI),
        "the owner's follow graph rides; `remote_actor_uri` is the counterparty class that has \
         always ridden, and an AP following collection is public by protocol"
    );
    assert!(
        rows_of("ap_post_map").contains(AP_URL),
        "the witness that this post reached the fediverse, and under what URL"
    );
    assert!(
        rows_of("ap_post_map").contains(AP_POST_REMOTE_ACTOR_URI),
        "`ap_post_map.remote_actor_uri` (the ALTER-added column) must ride verbatim under its \
         Export::Verbatim ruling whenever a row carries a value — the schema-level guard only \
         proves the column has SOME ruling, this proves the export code actually carries its bytes"
    );
    assert!(
        rows_of("bluesky_accounts").contains(BSKY_HANDLE)
            && rows_of("bluesky_accounts").contains("1755900000"),
        "the link itself rides — the handle they typed, and `token_expires`, an expiry integer \
         cleared because it matched the word `token` inside a column holding a time"
    );
    assert!(
        rows_of("bluesky_accounts").contains("\"write_through\":1"),
        "`write_through` is their cross-post setting, per its own Export::Redacted ruling's \
         omit list (which does NOT name write_through) — it must ride as the value seeded (1), \
         not just be absent from the redaction"
    );
    assert!(
        rows_of("bluesky_saved_feeds").contains(BSKY_FEED)
            && rows_of("bluesky_interactions").contains(BSKY_RECORD_URI),
        "their curation, their likes — things they did, recorded against themselves"
    );
    assert!(
        rows_of("bridge_conversation_messages").contains(&hex::encode(DM_SEALED))
            && rows_of("bridge_conversation_rooms").contains(DM_PEER),
        "the DM body rides in at-rest form (BLOBs are hex-encoded into the ndjson) — it is \
         sealed to the exporting actor's OWN recipient key, so withholding it would carry the \
         fact of a DM plane and none of the mail. The room's far id is the correspondent"
    );
    assert!(
        rows_of("nostr_follows").contains(FOLLOW_PETNAME),
        "the petname is the column that settles nostr_follows: a name the owner chose, stored \
         nowhere else and recreatable by nobody"
    );

    // (c) The credentials stay home — asserted across EVERY entry, so a future
    //     verdict elsewhere cannot smuggle them in. Seeded non-empty on
    //     purpose: production writes bluesky's three empty, so this is the
    //     direction the `Redacted` verdicts exist for.
    for (label, needle) in [
        ("ap_accounts.encrypted_privkey", AP_PRIVKEY),
        ("bluesky_accounts.access_token", BSKY_ACCESS),
        ("bluesky_accounts.refresh_token", BSKY_REFRESH),
        ("bluesky_accounts.dpop_key", BSKY_DPOP),
    ] {
        let hex_needle = hex::encode(needle);
        for (name, bytes) in &entries {
            let body = String::from_utf8_lossy(bytes);
            assert!(
                !body.contains(&hex_needle),
                "{name} carries {label}. The export is retrievable with an eviction export \
                 token — the weakest credential the endpoint accepts — so it must never mint a \
                 new resting place for a key that opens something else. The AP privkey signs \
                 HTTP Signatures AS this actor; the three Bluesky columns are empty in \
                 production today and are redacted so that stays true if a writer changes"
            );
        }
    }

    // (d) The withheld table stays home, by entry and by bytes.
    let table = "nostr_federation_cursors";
    assert!(
        !entries.iter().any(|(n, _)| n == &name_of(table)),
        "{table} is WithheldOperational — poll/sync bookkeeping, not \
         the owner's data"
    );
    for (name, bytes) in &entries {
        assert!(
            !String::from_utf8_lossy(bytes).contains(PEER_NEST),
            "{name} carries a {table} column. Its verdict is a claim no generated guard can \
             falsify — the withheld direction reds nowhere else — so this assertion is the \
             only thing standing between the ruling and a silent flip"
        );
    }

    // (e) The declaration matches the emission: none of them is still an open
    //     judgment, and the withholding is named with its class.
    let (_, manifest_bytes) = entries
        .iter()
        .find(|(n, _)| n == "export/manifest.json")
        .expect("manifest.json");
    let manifest: serde_json::Value = serde_json::from_slice(manifest_bytes).unwrap();
    let unreviewed = manifest["coverage"]["unreviewed_tables"]
        .as_array()
        .expect("unreviewed_tables array");
    for table in [
        "ap_accounts",
        "ap_follows",
        "ap_post_map",
        "bluesky_accounts",
        "bluesky_interactions",
        "bluesky_saved_feeds",
        "nostr_federation_cursors",
        "nostr_follows",
    ] {
        assert!(
            !unreviewed.iter().any(|t| t == table),
            "{table} was ruled — it must not still be declared Unreviewed"
        );
    }
    let withheld = manifest["coverage"]["withheld_tables"]
        .as_array()
        .expect("withheld_tables array");
    let declared = withheld
        .iter()
        .find(|w| w["table"] == table)
        .unwrap_or_else(|| panic!("{table} must be declared withheld by name"));
    assert_eq!(
        declared["reason_class"], "operational",
        "{table} withholds as operational — knowing what the nest holds but does not \
         export is part of `principles.md` § The user always controls their data"
    );
}

// ---------------------------------------------------------------------------
// Row 156 — the export reaches the segment store behind `include_blobs`
// ---------------------------------------------------------------------------

/// Build a `MailFloorMetadata` for a direct `append_record` seed. Only
/// `received_at` matters here — the bucket derives from it.
fn mail_floor(received_at_ms: i64) -> fauna_mail::segments::MailFloorMetadata {
    fauna_mail::segments::MailFloorMetadata {
        format_version: fauna_mail::segments::MAIL_FLOOR_FORMAT_VERSION,
        received_at: received_at_ms,
        timestamp: received_at_ms / 1000,
        ciphertext_size: 0,
        sender_domain: "example.com".into(),
        spam_disposition: "accept".into(),
        is_own_submission: false,
        spf: "pass".into(),
        dkim: "pass".into(),
        dmarc: "pass".into(),
        dmarc_policy: "reject".into(),
        arc: "pass".into(),
        spam_score: 0,
        seq: 0,
        continuation_role: fauna_mail::segments::CONTINUATION_ROLE_NORMAL,
        ..Default::default()
    }
}

/// Wrap a byte literal as an at-rest sealed carrier for a direct
/// `append_record` call (S6.12b structural seal gate). These tests exercise
/// what the export CARRIES, not seal genuineness — and what it carries is
/// exactly the at-rest form, so a literal is the honest fixture.
fn at_rest(bytes: Vec<u8>) -> fauna_mls::wrapped_blob::SealedRecordBytes {
    fauna_mls::wrapped_blob::SealedRecordBytes::carried_at_rest_unchecked(bytes)
}

/// Seed one real mail record through the production choke point
/// (`segments::mail::append_record` — the path every ingest leg passes
/// through), then finalize so the pair is on disk.
async fn seed_one_mail_segment(state: &fauna_nest::routes::AppState, actor: &[u8; 32]) -> u32 {
    fauna_nest::segments::mail::append_record(
        &state.mail_segments,
        &state.db,
        actor,
        // No caller-supplied record id since the record-identity cutover — the
        // append derives it as the content hash of the envelope it encodes.
        &at_rest(b"the-sealed-rfc822-bytes-at-rest".to_vec()),
        &at_rest(b"the-sealed-index-hint".to_vec()),
        mail_floor(1_755_300_000_000),
    )
    .await
    .expect("seed a mail record through the production append path");
    state
        .mail_segments
        .finalize_open(actor)
        .await
        .expect("finalize the open segment");
    let manifest = state
        .mail_segments
        .load_manifest(actor)
        .await
        .expect("load manifest");
    assert_eq!(
        manifest.kind_manifest.live_segments.len(),
        1,
        "one live segment expected after one append + finalize"
    );
    manifest.kind_manifest.live_segments[0]
}

async fn export_zip_q(base: &str, token: &str, query: &str) -> Vec<u8> {
    let client = reqwest::Client::new();
    let resp = client
        .get(format!("{base}/api/v1/export{query}"))
        .header("authorization", format!("Bearer {token}"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    resp.bytes().await.unwrap().to_vec()
}

/// Row 156 — `include_blobs` reaches the SEGMENT STORE: the archive carries the
/// owner's segment file pairs verbatim in their at-rest form, and the manifest
/// declares the segment-store disposition either way.
///
/// The ruling this executes (`account-data-plane.md` § Nest-side requirements
/// item 1, the payload-stores paragraph):
///
/// - **One flag.** `include_blobs` means "the payload bytes my records address
///   ride too"; which store serves them (blob store vs segment store) is
///   nest-internal mechanics, not a user choice — no second knob.
/// - **At-rest form verbatim.** The `.dat`+`.meta` pair rides byte-identical to
///   the on-disk file: CARv2 framing plaintext, record payloads sealed by the
///   kind's inner seal (`message-segment-store.md` § At-rest vs transport).
///   The nest never unseals for export — it structurally cannot. The one
///   plane this does NOT describe is `post`, whose public bodies rest in
///   plaintext: there a pair holding a legally taken-down post is withheld
///   whole and declared (decision (6); the pin is
///   `a_taken_down_posts_body_never_rides_the_segment_pair_leg_either`
///   below). This test seeds a MAIL segment, so its byte-identical
///   assertions are the rule for every pair the ruling does not withhold.
/// - **The manifest tells the owner which archive they hold**, so
///   `coverage.partial == false` cannot be read as "you have everything":
///   `segment_store.included` says whether the bytes rode, and the
///   channel-scoped `conv` plane is declared out of a per-actor walk's reach rather than silently absent.
#[tokio::test]
async fn include_blobs_carries_the_segment_pair_at_rest_form_verbatim() {
    let (base, state) = start_server().await;
    let kp = ActorKeypair::generate();
    let actor_id = kp.actor_id();
    state
        .db
        .create_user(&actor_id.0, "free", "segbytes")
        .await
        .unwrap();
    let token = state.auth.token_store.insert(actor_id, 3600).await;

    let seg_id = seed_one_mail_segment(&state, &actor_id.0).await;

    // (a) Without the flag: an index-only archive, and the manifest says so.
    let entries = read_archive(&export_zip_q(&base, &token, "").await);
    assert!(
        !entries
            .iter()
            .any(|(n, _)| n.starts_with("export/segments/")),
        "without include_blobs the segment bytes stay home — the flag is the owner's one \
         payload decision"
    );
    let (_, manifest_bytes) = entries
        .iter()
        .find(|(n, _)| n == "export/manifest.json")
        .expect("manifest.json");
    let manifest: serde_json::Value = serde_json::from_slice(manifest_bytes).unwrap();
    assert_eq!(
        manifest["segment_store"]["included"], false,
        "an index-only archive must declare the segment bytes absent -- `coverage.partial` \
         being false declares no open judgment, never completeness"
    );

    // (b) With the flag: the pair rides, byte-identical to the on-disk files.
    let entries = read_archive(&export_zip_q(&base, &token, "?include_blobs=true").await);
    let dat_name = format!("export/segments/mail/seg-{seg_id:08}.dat");
    let meta_name = format!("export/segments/mail/seg-{seg_id:08}.meta");
    let (_, dat_bytes) = entries
        .iter()
        .find(|(n, _)| n == &dat_name)
        .expect("the segment .dat must ride under include_blobs");
    let (_, meta_bytes) = entries.iter().find(|(n, _)| n == &meta_name).expect(
        "the .meta sidecar must ride with its .dat -- record_order is what lets the \
         owner's own tooling re-admit the pair (account-data-plane.md bootstrap contract)",
    );
    let on_disk_dat =
        std::fs::read(state.mail_segments.segment_file_path(&actor_id.0, seg_id)).unwrap();
    let on_disk_meta =
        std::fs::read(state.mail_segments.segment_meta_path(&actor_id.0, seg_id)).unwrap();
    assert_eq!(
        dat_bytes, &on_disk_dat,
        "the .dat rides VERBATIM in its at-rest form -- no unseal, no re-seal, no re-framing"
    );
    assert_eq!(meta_bytes, &on_disk_meta, "the .meta rides verbatim too");
    assert!(
        dat_bytes
            .windows(b"the-sealed-rfc822-bytes-at-rest".len())
            .any(|w| w == b"the-sealed-rfc822-bytes-at-rest"),
        "the record payload the CIDs address is actually in the archive -- the gap row 156 \
         closes was an export naming every record while the bytes stayed home"
    );

    // (c) The manifest declares the disposition.
    let (_, manifest_bytes) = entries
        .iter()
        .find(|(n, _)| n == "export/manifest.json")
        .expect("manifest.json");
    let manifest: serde_json::Value = serde_json::from_slice(manifest_bytes).unwrap();
    assert_eq!(manifest["segment_store"]["included"], true);
    let kinds: Vec<String> = manifest["segment_store"]["kinds"]
        .as_array()
        .expect("kinds array")
        .iter()
        .map(|v| v.as_str().unwrap().to_string())
        .collect();
    assert_eq!(
        kinds,
        vec!["mail", "post", "calendar", "card"],
        "the four actor-scoped kinds are the walk's universe"
    );
    assert_eq!(
        manifest["segment_store"]["channel_scoped_not_included"]
            .as_array()
            .expect("channel_scoped_not_included array")
            .iter()
            .map(|v| v.as_str().unwrap())
            .collect::<Vec<_>>(),
        vec!["conv"],
        "the channel-scoped plane a per-actor walk cannot reach is declared by name, never silently absent"
    );
}

/// The ruling, executable: **the eviction export token pulls the same
/// archive the owner's session token pulls — segment bytes included.**
///
/// Why that is safe under the weakest credential the endpoint accepts
/// (`account-data-plane.md` § Nest-side requirements item 1): the archive's
/// payload contents are the AT-REST form — sealed record payloads whose keys
/// never rest on the nest — so the seal, not credential tiering, is the safety
/// mechanism; a token thief gains ciphertext addressed to keys they do not
/// hold, strictly less than the plaintext index that already rides. The
/// `WithheldSecret` boundary is content-class-based (live credentials ride for
/// NOBODY) and stays untouched. And the token's one legitimate holder is an
/// account under eviction — the person for whom this archive is the last copy
/// before scheduled deletion; an index-only archive there would be data
/// destruction with a receipt (`principles.md` § The user always controls
/// their data).
#[tokio::test]
async fn the_eviction_token_pulls_the_same_archive_the_owner_does() {
    let (base, state) = start_server().await;
    let kp = ActorKeypair::generate();
    let actor_id = kp.actor_id();
    state
        .db
        .create_user(&actor_id.0, "free", "evictee")
        .await
        .unwrap();
    let session_token = state.auth.token_store.insert(actor_id, 3600).await;

    let _seg_id = seed_one_mail_segment(&state, &actor_id.0).await;

    // The admin arm mints this on eviction-start (admin_ws_handlers) so the
    // evicted account can still pull its data after ordinary tokens die.
    state
        .db
        .create_eviction_token("an-eviction-export-token", &actor_id.0, 1 << 40)
        .await
        .unwrap();

    let owner_entries =
        read_archive(&export_zip_q(&base, &session_token, "?include_blobs=true").await);
    let eviction_entries =
        read_archive(&export_zip_q(&base, "an-eviction-export-token", "?include_blobs=true").await);

    let segment_names = |entries: &[(String, Vec<u8>)]| {
        let mut v: Vec<String> = entries
            .iter()
            .filter(|(n, _)| n.starts_with("export/segments/"))
            .map(|(n, _)| n.clone())
            .collect();
        v.sort();
        v
    };
    let owner_segments = segment_names(&owner_entries);
    assert!(
        !owner_segments.is_empty(),
        "precondition: the owner's archive carries the seeded segment pair"
    );
    assert_eq!(
        segment_names(&eviction_entries),
        owner_segments,
        "auth strength does not tier the archive: the eviction token's holder is the evictee \
         pulling their last copy, and every payload byte is sealed at rest -- withholding \
         here would be data destruction with a receipt"
    );
    for name in &owner_segments {
        let a = &owner_entries.iter().find(|(n, _)| n == name).unwrap().1;
        let b = &eviction_entries.iter().find(|(n, _)| n == name).unwrap().1;
        assert_eq!(
            a, b,
            "{name} must be byte-identical under either credential"
        );
    }
}

/// **Row 161** — the owner's conv BODIES ride a membership-resolved per-record
/// door behind `include_blobs`, excluded and withheld exactly the way the
/// serving door excludes and withholds.
///
/// The shape this executes (`account-data-plane.md` § Nest-side requirements
/// item 1, the Universe paragraph): conv segments are CHANNEL-scoped files, so
/// the per-actor byte walk structurally cannot reach them, and a whole-pair
/// export would carry records compaction has not yet dropped where the serving
/// door reads live records. So the bodies door is the records domain
/// one level down — resolve `actor_channels`, then read each channel's live
/// records' sealed payloads through `segments::conv::read_after_seq`, the SAME
/// primitive every conv serve surface flows through. Inheriting that primitive
/// is the point: tombstone exclusion and the legal-obligation relay-withhold
/// are enforced once, at the read gate, and the export cannot drift from the
/// serving door because it IS the serving door's read.
///
/// Like the pin above, this drives the real HTTP endpoint and reads
/// the archive: the gather and the zip writer are separate halves, so a
/// gather-level unit test could pass while the bodies never reached a zip
/// entry (the dark-entry-point class).
#[tokio::test]
async fn the_owners_conv_bodies_ride_the_membership_resolved_door() {
    use fauna_nest::segments::conv;

    let (base, state) = start_server().await;
    let kp = ActorKeypair::generate();
    let actor_id = kp.actor_id();
    state
        .db
        .create_user(&actor_id.0, "free", "convbodies")
        .await
        .unwrap();
    let token = state.auth.token_store.insert(actor_id, 3600).await;

    // Neither channel id is the exporter's own bytes — same fact as the
    // records-domain pin: a channel scope is not an actor scope.
    let mine = [0xA9u8; 32];
    let theirs = [0xBAu8; 32];
    state
        .db
        .register_actor_channel(&actor_id.0, &mine)
        .await
        .unwrap();

    // Seed through the production writer so a real segment file holds the
    // bodies (the mirror-only seeding of the records pin has no bytes to ride).
    let tombstoned_body = b"sealed-conv-body-tombstoned".to_vec();
    let live_body = b"sealed-conv-body-live".to_vec();
    let withheld_body = b"sealed-conv-body-taken-down".to_vec();
    let stranger_body = b"sealed-conv-body-of-a-channel-they-are-not-on".to_vec();
    for body in [&tombstoned_body, &live_body, &withheld_body] {
        conv::append(
            &state.conv_segments,
            &state.db,
            &mine,
            body,
            1_755_300_020_000,
        )
        .await
        .expect("seed a conv record on the exporter's channel");
    }
    conv::append(
        &state.conv_segments,
        &state.db,
        &theirs,
        &stranger_body,
        1_755_300_021_000,
    )
    .await
    .expect("seed a conv record on a channel the exporter is NOT on");

    // seq 1 dies the way the serving door sees death: a mirror tombstone.
    let n = conv::tombstone_up_to_seq(&state.db, &[mine], 1)
        .await
        .expect("tombstone seq 1");
    assert_eq!(n, 1, "precondition: exactly the first record is tombstoned");

    // seq 3 is withheld the way the serving door withholds: a legal takedown.
    let withheld_cid =
        fauna_mls::segments::derive_record_cid(&withheld_body).expect("derive conv record cid");
    let n = state
        .db
        .set_conv_legal_takedown(&withheld_cid, Some("EU-DSA-2024/777"))
        .await
        .expect("set takedown");
    assert_eq!(n, 1, "precondition: exactly the third record is taken down");

    // (a) Without the flag: an index-only archive, and the manifest says so.
    let entries = read_archive(&export_zip_q(&base, &token, "").await);
    assert!(
        !entries
            .iter()
            .any(|(n, _)| n.starts_with("export/conversations/bodies/")),
        "without include_blobs the conv bodies stay home — one flag governs payload bytes"
    );
    let (_, manifest_bytes) = entries
        .iter()
        .find(|(n, _)| n == "export/manifest.json")
        .expect("manifest.json");
    let manifest: serde_json::Value = serde_json::from_slice(manifest_bytes).unwrap();
    assert_eq!(
        manifest["conversations"]["bodies_included"], false,
        "an index-only archive must declare the conv bodies absent by name, never silently"
    );

    // (b) With the flag: the LIVE record's sealed payload rides, addressed the
    //     way conv records are addressed — channel + per-channel seq.
    let entries = read_archive(&export_zip_q(&base, &token, "?include_blobs=true").await);
    let live_name = format!(
        "export/conversations/bodies/{}/rec-{:012}",
        hex::encode(mine),
        2
    );
    let (_, body_bytes) = entries
        .iter()
        .find(|(n, _)| n == &live_name)
        .unwrap_or_else(|| {
            panic!(
                "the live conv body must ride at {live_name}; the archive holds: {:?}",
                entries.iter().map(|(n, _)| n).collect::<Vec<_>>()
            )
        });
    assert_eq!(
        body_bytes, &live_body,
        "the body rides VERBATIM in its at-rest sealed form -- the nest never unseals for export"
    );

    // (b1) The tombstoned record's body does NOT ride — excluded the same way
    //      the serving door excludes it (`tombstoned = 0` at the read gate).
    assert!(
        !entries
            .iter()
            .any(|(n, _)| n.ends_with("/rec-000000000001")),
        "a tombstoned record's body must not ride: the serving door does not serve it, and the \
         export inherits the serving door's read"
    );

    // (b2) The taken-down record's body does NOT ride, while its records row
    //      still carries the reference — the withhold is visible, never silent
    //      (moderation.md: a legal takedown is a visible tombstone, and
    //      transparency to the author is the mechanism's point).
    assert!(
        !entries
            .iter()
            .any(|(n, _)| n.ends_with("/rec-000000000003")),
        "a legally-withheld record's sealed bytes must never be read for export -- the relay- \
         withhold gate is enforced once, at the read primitive the export shares"
    );
    let (_, records_bytes) = entries
        .iter()
        .find(|(n, _)| n == "export/conversations/records.ndjson")
        .expect("the records domain rides beside the bodies");
    assert!(
        String::from_utf8_lossy(records_bytes).contains("EU-DSA-2024/777"),
        "the takedown reference rides in the records row, so the absent body is declared, \
         not silently under-delivered"
    );

    // (b3) Membership BOUNDS the reach: nothing anywhere in the archive names
    //      the stranger channel or carries its body bytes.
    let theirs_hex = hex::encode(theirs);
    for (n, b) in &entries {
        assert!(
            !n.contains(&theirs_hex),
            "{n} names a channel this exporter is not a member of"
        );
        assert!(
            !b.windows(stranger_body.len()).any(|w| w == stranger_body),
            "{n} carries conv body bytes from a channel this exporter is not a member of. The \
             bodies door resolves `actor_channels` first, and that resolution IS the access rule"
        );
    }

    // (c) The manifest declares the disposition: bodies rode through the
    //     membership door, while the channel-scoped segment PAIRS stay out of
    //     the per-actor walk — deliberately (the pair carries records
    //     compaction has not yet dropped, where this door reads live records).
    let (_, manifest_bytes) = entries
        .iter()
        .find(|(n, _)| n == "export/manifest.json")
        .expect("manifest.json");
    let manifest: serde_json::Value = serde_json::from_slice(manifest_bytes).unwrap();
    assert_eq!(manifest["conversations"]["bodies_included"], true);
    assert!(
        !entries
            .iter()
            .any(|(n, _)| n.starts_with("export/segments/conv/")),
        "the channel-scoped pairs themselves must NOT ride: the bodies door serves live \
         records, a whole pair would resurrect compaction-undropped ones"
    );
    assert_eq!(
        manifest["segment_store"]["channel_scoped_not_included"]
            .as_array()
            .expect("channel_scoped_not_included array")
            .iter()
            .map(|v| v.as_str().unwrap())
            .collect::<Vec<_>>(),
        vec!["conv"],
        "the pair-level absence stays declared by name even while the bodies ride the \
         membership door"
    );
}

/// **A taken-down post's body never rides its author's archive, and its entry
/// says so** (`moderation.md` § Legal takedown → *Posts*: the body is withheld
/// from every viewer, the author included).
///
/// The `posts` domain reads bodies through the flag-blind `load_post_body`, so
/// until 2026-09-10 the author's export carried the very bytes the takedown
/// withholds everywhere else. The entry is KEPT, the way `fauna.posts.list`
/// keeps its row: empty `data_hex` plus the `legal_takedown_ref` citation, so
/// the absence is declared rather than silently partial. Only the takedown arm
/// binds here — the archive is the author's, and quarantine is author-visible —
/// so a quarantined post's body must still ride. Drives the real endpoint and
/// reads the zip, like the conv-bodies pin above: the gather and the writer are
/// separate halves.
///
/// Scope: the archive WITHOUT `include_blobs` — the `posts` domain alone. The
/// segment-pair leg the flag adds has its own pin directly below.
#[tokio::test]
async fn a_taken_down_posts_body_never_rides_its_authors_archive() {
    use fauna_core::data::{Post, PostBody, Timestamp};

    let (base, state) = start_server().await;
    let kp = ActorKeypair::generate();
    let actor_id = kp.actor_id();
    state
        .db
        .create_user(&actor_id.0, "free", "withheldposts")
        .await
        .unwrap();
    let token = state.auth.token_store.insert(actor_id, 3600).await;

    // Seed three posts through the production writer (`store_post`: body in
    // the `__post` segment, projection with an empty payload).
    let mut seed = Vec::new();
    for (i, text) in [
        "live-post-body-5c1d",
        "withheld-post-body-7f3a",
        "quarantined-post-body-9e2b",
    ]
    .iter()
    .enumerate()
    {
        let post = Post {
            author: actor_id,
            created_at: Timestamp(1_755_300_000_000_000 + i as u64),
            body: PostBody::Text {
                content: text.to_string(),
                facets: vec![],
            },
            references: vec![],
            expires_at: None,
            gated: None,
            content_warning: None,
            origin: None,
        };
        let body = fauna_core::encoding::canonical_encode(&post).unwrap();
        let post_id: [u8; 32] = *blake3::hash(&body).as_bytes();
        fauna_nest::segments::post::store_post(
            &state.post_segments,
            &state.db,
            &post_id,
            &body,
            None,
        )
        .await
        .expect("seed a post through the production writer");
        seed.push((post_id, body, *text));
    }
    let (live_id, live_body, _) = &seed[0];
    let (taken_id, taken_body, taken_text) = &seed[1];
    let (quarantined_id, quarantined_body, _) = &seed[2];
    state
        .db
        .set_post_legal_takedown(taken_id, Some("EU-DSA-2024/931"))
        .await
        .unwrap();
    state
        .db
        .set_post_quarantined(quarantined_id, true)
        .await
        .unwrap();

    let entries = read_archive(&export_zip_q(&base, &token, "").await);
    let post_entry = |id: &[u8; 32]| -> serde_json::Value {
        let name = format!("export/posts/{}.json", hex::encode(id));
        let (_, bytes) = entries.iter().find(|(n, _)| n == &name).unwrap_or_else(|| {
            panic!(
                "{name} must be in the archive; it holds: {:?}",
                entries.iter().map(|(n, _)| n).collect::<Vec<_>>()
            )
        });
        serde_json::from_slice(bytes).unwrap()
    };

    // Control: a live post rides whole, and its entry carries no takedown field
    // (the new field is omitted, so every other entry keeps its old shape).
    let live = post_entry(live_id);
    assert_eq!(live["data_hex"], hex::encode(live_body));
    assert!(
        live.get("legal_takedown_ref").is_none(),
        "an untaken post's entry must not grow the field: {live}"
    );

    // The taken-down post: entry kept, body withheld, citation declared.
    let taken = post_entry(taken_id);
    assert_eq!(
        taken["data_hex"], "",
        "a taken-down post's body must not ride its author's archive: {taken}"
    );
    assert_eq!(
        taken["legal_takedown_ref"], "EU-DSA-2024/931",
        "the withheld entry declares why its body is absent"
    );
    let taken_hex = hex::encode(taken_body);
    for (n, b) in &entries {
        assert!(
            !b.windows(taken_text.len())
                .any(|w| w == taken_text.as_bytes()),
            "{n} carries the taken-down post's body text"
        );
        assert!(
            !String::from_utf8_lossy(b).contains(&taken_hex),
            "{n} carries the taken-down post's body as hex"
        );
    }

    // Quarantine does not bind the author's own archive.
    let quarantined = post_entry(quarantined_id);
    assert_eq!(
        quarantined["data_hex"],
        hex::encode(quarantined_body),
        "a quarantined post is author-visible, so its body still rides the author's archive"
    );
}

/// **A taken-down post's body never rides the segment-pair leg either** — the
/// archive every app actually requests (`include_blobs=true`), and the leg the
/// pin above deliberately left open.
///
/// The ruling this executes (`account-data-plane.md` § Nest-side requirements
/// item 1, *Payload stores*, decision (6)): the `post` plane's at-rest form is
/// NOT sealed — public bodies rest in plaintext — so the sealed-bytes premise
/// that lets every other pair ride verbatim under any credential does not
/// cover it, and a pair cannot be byte-identical AND withhold one record. So a
/// `post` pair holding a currently taken-down record is **withheld whole** and
/// **declared** in the manifest (`segment_store.withheld`, by kind, segment and
/// reason) — never rewritten, never silently absent. Every other pair, and the
/// same author's other posts through the `posts` domain, ride as before; the
/// pair rides again once the takedown is overturned (a takedown sets
/// `content_meta.legal_takedown_ref` and never segment-tombstones the record,
/// so compaction keeps it and the withhold lasts exactly as long as the flag).
///
/// Drives the real endpoint and reads the zip, like the pins above.
#[tokio::test]
async fn a_taken_down_posts_body_never_rides_the_segment_pair_leg_either() {
    use fauna_core::data::{Post, PostBody, Timestamp};

    let (base, state) = start_server().await;
    let kp = ActorKeypair::generate();
    let actor_id = kp.actor_id();
    state
        .db
        .create_user(&actor_id.0, "free", "withheldpairs")
        .await
        .unwrap();
    let token = state.auth.token_store.insert(actor_id, 3600).await;

    // Control plane: a mail pair, which must keep riding verbatim — the
    // withhold is per PAIR, never a plane-wide or archive-wide switch.
    let mail_seg_id = seed_one_mail_segment(&state, &actor_id.0).await;

    // Two posts through the production writer, sharing one `__post` segment:
    // one stays live, one is taken down.
    let mut seed = Vec::new();
    for (i, text) in ["live-post-body-2b8e", "withheld-post-body-4d1c"]
        .iter()
        .enumerate()
    {
        let post = Post {
            author: actor_id,
            created_at: Timestamp(1_755_300_000_000_000 + i as u64),
            body: PostBody::Text {
                content: text.to_string(),
                facets: vec![],
            },
            references: vec![],
            expires_at: None,
            gated: None,
            content_warning: None,
            origin: None,
        };
        let body = fauna_core::encoding::canonical_encode(&post).unwrap();
        let post_id: [u8; 32] = *blake3::hash(&body).as_bytes();
        fauna_nest::segments::post::store_post(
            &state.post_segments,
            &state.db,
            &post_id,
            &body,
            None,
        )
        .await
        .expect("seed a post through the production writer");
        seed.push((post_id, body, *text));
    }
    let (live_id, live_body, _) = &seed[0];
    let (taken_id, taken_body, taken_text) = &seed[1];
    state
        .db
        .set_post_legal_takedown(taken_id, Some("EU-DSA-2024/1083"))
        .await
        .unwrap();
    let (scope, post_seg_id) =
        fauna_nest::segments::post::lookup_scope_by_post_id(&state.db, taken_id)
            .await
            .unwrap()
            .expect("the seeded post has a segment record");
    assert_eq!(scope, actor_id.0, "a post's segment scope is its author");
    let post_dat = format!("export/segments/post/seg-{post_seg_id:08}.dat");
    let post_meta = format!("export/segments/post/seg-{post_seg_id:08}.meta");

    let entries = read_archive(&export_zip_q(&base, &token, "?include_blobs=true").await);
    let names: Vec<&str> = entries.iter().map(|(n, _)| n.as_str()).collect();

    // (a) The pair holding the taken-down record does not ride — neither half:
    //     a `.meta` without its `.dat` re-admits nothing, and the pair is the
    //     unit the ruling withholds.
    assert!(
        !names.contains(&post_dat.as_str()),
        "a `post` pair holding a taken-down record must be withheld whole — {post_dat} rode; \
         the archive holds: {names:?}"
    );
    assert!(
        !names.contains(&post_meta.as_str()),
        "the withheld pair's sidecar must stay home with its .dat — {post_meta} rode"
    );

    // (b) Nothing anywhere in the archive carries the compelled bytes — the
    //     invariant the pair leg used to break.
    let taken_hex = hex::encode(taken_body);
    for (n, b) in &entries {
        assert!(
            !b.windows(taken_text.len())
                .any(|w| w == taken_text.as_bytes()),
            "{n} carries the taken-down post's body text"
        );
        assert!(
            !String::from_utf8_lossy(b).contains(&taken_hex),
            "{n} carries the taken-down post's body as hex"
        );
    }

    // (c) The author still gets everything the takedown does not compel: the
    //     live post's body through the `posts` domain, and the taken-down one's
    //     entry with its citation — the absence is declared on both doors.
    let post_entry = |id: &[u8; 32]| -> serde_json::Value {
        let name = format!("export/posts/{}.json", hex::encode(id));
        let (_, bytes) = entries
            .iter()
            .find(|(n, _)| n == &name)
            .unwrap_or_else(|| panic!("{name} must be in the archive; it holds: {names:?}"));
        serde_json::from_slice(bytes).unwrap()
    };
    assert_eq!(
        post_entry(live_id)["data_hex"],
        hex::encode(live_body),
        "the withheld pair's OTHER posts still reach the owner through the posts domain"
    );
    assert_eq!(
        post_entry(taken_id)["legal_takedown_ref"],
        "EU-DSA-2024/1083"
    );

    // (d) The mail pair rides verbatim — the withhold is per pair.
    let mail_dat = format!("export/segments/mail/seg-{mail_seg_id:08}.dat");
    let (_, mail_dat_bytes) = entries
        .iter()
        .find(|(n, _)| n == &mail_dat)
        .expect("a mail pair is untouched by a post takedown");
    assert_eq!(
        mail_dat_bytes,
        &std::fs::read(
            state
                .mail_segments
                .segment_file_path(&actor_id.0, mail_seg_id)
        )
        .unwrap(),
        "every pair the ruling does not withhold still rides byte-identical"
    );

    // (e) The manifest declares the withheld pair by kind, segment and reason,
    //     while `post` stays among the kinds walked — the plane rode, one pair
    //     of it did not, and the owner can tell which and why.
    let manifest_of = |entries: &[(String, Vec<u8>)]| -> serde_json::Value {
        let (_, bytes) = entries
            .iter()
            .find(|(n, _)| n == "export/manifest.json")
            .expect("manifest.json");
        serde_json::from_slice(bytes).unwrap()
    };
    let manifest = manifest_of(&entries);
    assert_eq!(manifest["segment_store"]["included"], true);
    assert!(
        manifest["segment_store"]["kinds"]
            .as_array()
            .unwrap()
            .iter()
            .any(|k| k == "post"),
        "the post plane is still walked; one pair of it is withheld"
    );
    assert_eq!(
        manifest["segment_store"]["withheld"],
        serde_json::json!([{
            "kind": "post",
            "segment_id": post_seg_id,
            "reason": "legal_takedown",
        }]),
        "a withheld pair is declared, never silently absent: {}",
        manifest["segment_store"]
    );

    // (f) Overturned: the flag is the whole withhold. Clear it and the same
    //     pair rides again, byte-identical, and the declaration empties.
    state
        .db
        .set_post_legal_takedown(taken_id, None)
        .await
        .unwrap();
    let entries = read_archive(&export_zip_q(&base, &token, "?include_blobs=true").await);
    let (_, dat_bytes) = entries
        .iter()
        .find(|(n, _)| n == &post_dat)
        .expect("once the takedown is overturned the post pair rides again");
    assert_eq!(
        dat_bytes,
        &std::fs::read(
            state
                .post_segments
                .segment_file_path(&actor_id.0, post_seg_id)
        )
        .unwrap(),
        "the restored pair rides in its verbatim at-rest form"
    );
    assert_eq!(
        manifest_of(&entries)["segment_store"]["withheld"],
        serde_json::json!([]),
        "nothing withheld once nothing is taken down"
    );
}

// ── standing is re-checked at USE, not only at token mint ───────────────────
//
// `GET /api/v1/export` hands back the whole account as one zip, and it accepts
// an **eviction** token as well as a session bearer. Eviction tokens live in
// SQLite and `revoke_actor` never touches them, so neither a lockout nor a
// suspension reaches an already-minted one; a plain bearer likewise stays valid
// for its whole TTL. Checking standing only at mint therefore leaves a
// full-account exfil open across exactly the events whose whole purpose is to
// stop one. `api-layers.md` § the `/api/v1/export` row.

/// Register an actor with a bearer, ready to export.
async fn actor_with_bearer(
    state: &Arc<fauna_nest::routes::AppState>,
    handle: &str,
) -> ([u8; 32], String) {
    let kp = ActorKeypair::generate();
    let actor_id = kp.actor_id();
    state
        .db
        .create_user(&actor_id.0, "free", handle)
        .await
        .unwrap();
    let token = state.auth.token_store.insert(actor_id, 3600).await;
    (actor_id.0, token)
}

async fn export_status(base: &str, token: &str) -> u16 {
    reqwest::Client::new()
        .get(format!("{base}/api/v1/export"))
        .header("Authorization", format!("Bearer {token}"))
        .send()
        .await
        .unwrap()
        .status()
        .as_u16()
}

/// A **suspended** account may not pull the archive — suspension is an admin
/// verdict that has to bite at use, not merely stop the next mint.
#[tokio::test]
async fn export_refuses_a_suspended_account() {
    let (base, state) = start_server().await;
    let (actor_id, token) = actor_with_bearer(&state, "suspended-export").await;
    assert_eq!(
        export_status(&base, &token).await,
        200,
        "precondition: the export works before the suspension"
    );

    state
        .db
        .suspend_user_now(&actor_id, "suspended by admin", "other")
        .await
        .unwrap();

    assert_eq!(
        export_status(&base, &token).await,
        403,
        "a suspended account's surviving bearer must not pull the whole account"
    );
}

/// A **locked-out** account may not pull the archive. Emergency lockout is the
/// panic button — "stop everything happening on my account right now" — and an
/// archive download is the one thing it most needs to stop.
#[tokio::test]
async fn export_refuses_a_locked_out_account() {
    let (base, state) = start_server().await;
    let (actor_id, token) = actor_with_bearer(&state, "locked-export").await;
    assert_eq!(
        export_status(&base, &token).await,
        200,
        "precondition: the export works before the lockout"
    );

    let until = fauna_core::data::Timestamp::now_secs() + 3600;
    state
        .db
        .set_locked_until(&actor_id, Some(until))
        .await
        .unwrap();

    assert_eq!(
        export_status(&base, &token).await,
        403,
        "a locked-out account's surviving bearer must not pull the whole account"
    );
}

/// the whole-account download is never silent — every archive
/// export rings the owner as an `ArchiveExported` security notice (the generic
/// `security.notice` row, so it renders on all 7 apps with no per-app work).
/// Exercised on the bearer path; the fire site sits after both credential
/// branches, so the eviction-token path shares it by construction.
#[tokio::test]
async fn an_archive_download_rings_the_owner() {
    let (base, state) = start_server().await;
    let kp = ActorKeypair::generate();
    let actor_id = kp.actor_id();
    state
        .db
        .create_user(&actor_id.0, "free", "test-export-ring")
        .await
        .unwrap();
    let token = state.auth.token_store.insert(actor_id, 3600).await;

    let client = reqwest::Client::new();
    let resp = client
        .get(format!("{base}/api/v1/export"))
        .header("authorization", format!("Bearer {token}"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let _ = resp.bytes().await.unwrap();

    // The event is spawned off the response path — a positive wait, so a
    // named generous budget + deadline poll (convention 14), never a settle
    // sleep-then-assert.
    const RING_BUDGET_SECS: u64 = 30;
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(RING_BUDGET_SECS);
    loop {
        let rows = state
            .db
            .list_notifications(&actor_id.0, None, 10)
            .await
            .unwrap();
        if rows.iter().any(|r| {
            r.notif_type.as_wire() == "security.notice"
                && r.summary.contains("archive was downloaded")
        }) {
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "no ArchiveExported security notice within {RING_BUDGET_SECS}s; rows: {rows:?}"
        );
        tokio::time::sleep(std::time::Duration::from_millis(25)).await;
    }
}

/// An invite code is a bearer credential (`validate_invite_code` redeems it on
/// the string alone), and `invite_codes`' own verdict omits it. The admin's
/// `invite.create` / `invite.delete` audit rows ride that admin's export under
/// the `audit_log` verdict, so they must carry a fingerprint, never the code —
/// else a demoted admin's self-export, or an evicted admin's eviction export
/// token, recovers every unspent code they minted (`account-data-plane.md`
/// § Nest-side requirements: an export never mints a new resting place for a
/// secret). Driven through the real handlers, so the pin follows the writer.
#[tokio::test]
async fn an_admins_minted_invite_codes_never_ride_their_audit_rows() {
    use fauna_nest::rpc_router::RpcRouter;
    use fauna_protocol::admin::{
        AdminInviteCodeCreateReply, AdminInviteCodeCreateRequest, AdminInviteCodeDeleteRequest,
    };
    use fauna_protocol::{decode_strict, encode_canonical};

    let (base, state) = start_server().await;
    let kp = ActorKeypair::generate();
    let admin = kp.actor_id();
    state
        .db
        .create_user(&admin.0, "free", "minter")
        .await
        .unwrap();
    state.db.add_admin_actor(&admin.0).await.unwrap();
    let token = state.auth.token_store.insert(admin, 3600).await;

    let mut b = RpcRouter::builder();
    fauna_nest::admin_ws_handlers::register_admin_handlers(&mut b);
    let router = b.build();
    let call = |kind: &'static str, payload: Vec<u8>| {
        let meta = router.kind_meta(kind).expect("kind registered");
        (meta.handler)(state.clone(), admin.0, payload.into())
    };
    let create = |code: &str| AdminInviteCodeCreateRequest {
        code: code.into(),
        tier: "free".into(),
        uses: 5,
        ..Default::default()
    };

    // An admin-typed code left unspent, a nest-minted one, and a typed one the
    // admin deletes again (so `invite.delete` is exercised too).
    const TYPED: &str = "TypedLiveInviteCode7731";
    const DELETED: &str = "TypedDeletedInviteCode4410";
    let mut codes = vec![TYPED.to_string(), DELETED.to_string()];
    for code in [TYPED, "", DELETED] {
        let reply: AdminInviteCodeCreateReply = decode_strict(
            &call(
                "fauna.admin.invite_codes.create",
                encode_canonical(&create(code)).unwrap().to_vec(),
            )
            .await
            .expect("create ok"),
        )
        .unwrap();
        if code.is_empty() {
            codes.push(reply.code);
        }
    }
    call(
        "fauna.admin.invite_codes.delete",
        encode_canonical(&AdminInviteCodeDeleteRequest {
            code: DELETED.into(),
            extra: Default::default(),
        })
        .unwrap()
        .to_vec(),
    )
    .await
    .expect("delete ok");

    let entries = read_archive(&export_zip(&base, &token).await);
    for (name, bytes) in &entries {
        let text = String::from_utf8_lossy(bytes);
        for code in &codes {
            assert!(
                !text.contains(code.as_str()),
                "{name} carries the live invite code {code}"
            );
        }
    }

    // The admin's conduct still rides: four invite rows, each naming its code
    // by fingerprint, and the delete matching its create.
    let (_, audit) = entries
        .iter()
        .find(|(n, _)| n == "export/tables/audit_log.ndjson")
        .expect("audit_log rides the archive");
    let rows: Vec<serde_json::Value> = String::from_utf8_lossy(audit)
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .filter(|r: &serde_json::Value| {
            r["action"]
                .as_str()
                .is_some_and(|a| a.starts_with("invite."))
        })
        .collect();
    assert_eq!(rows.len(), 4, "three creates and one delete ride: {rows:?}");
    let target_of = |action: &str, nth: usize| {
        rows.iter()
            .filter(|r| r["action"] == action)
            .nth(nth)
            .and_then(|r| r["target"].as_str())
            .unwrap_or_default()
            .to_string()
    };
    assert!(rows.iter().all(|r| {
        r["target"]
            .as_str()
            .is_some_and(|t| t.starts_with("invite-fp:"))
    }));
    assert_eq!(
        target_of("invite.delete", 0),
        target_of("invite.create", 2),
        "the delete's fingerprint matches its create's"
    );
    assert_ne!(target_of("invite.create", 0), target_of("invite.create", 1));
}
