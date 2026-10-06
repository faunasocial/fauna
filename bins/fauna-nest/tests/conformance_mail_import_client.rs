//! **Mailbox import — source → shared client → nest** (tier_3) — real-wire,
//! *client-driven*. The end-to-end proof that the shared-Rust IMAP client and
//! the nest's `fauna.bridges.import_message` handler actually compose.
//!
//! Production data flow asserted end-to-end (real TCP + real rustls to the
//! source, real WS-RPC to the nest, real SQLite, real HPKE seal):
//!
//!   the user's client opens a real IMAP session to a foreign source server
//!   (`NativeImapConnector` → `ImapSession::connect` → `LOGIN` → `EXAMINE` →
//!   `enumerate_uids` → pipelined `UID FETCH … BODY.PEEK[]`) → for each fetched
//!   message it derives the two wire fields through the *same* shared functions
//!   `FfiMailImportClient` calls (`mail_dedup_keys_from_slice` +
//!   `envelope::sender_domain` — `mailbox-migration.md:128`, the key's whole
//!   value is byte-for-byte agreement across all three producers) → pushes the
//!   raw RFC 5322 bytes into the nest over an authenticated `NestClient`
//!   (`fauna.bridges.start_import_session` then `import_message`, User-class and
//!   caller-scoped) → the nest seals body + index hint at ingest to the actor's
//!   registered seal key and records the dedup key → the record rests **sealed**
//!   (openable only by the owner's secret), and a **re-import of the same source
//!   message is skipped as a duplicate**.
//!
//! What this catches that nothing else does:
//! - Over `libs/fauna-mail/tests/imap_client_native.rs` (client ↔ source: a real
//!   TLS handshake and a real FETCH, but the bytes stop there) and
//!   `tests/e2e-unified/tests/api/test_import_push_api.py` (client ↔ nest: the
//!   real `import_message` wire, but the message is *synthesized in Python* — no
//!   IMAP client anywhere): the two halves were each proven alone and **nothing
//!   proved them composed**. Every field nest validates — `body_size ==
//!   body.len()`, no `\Recent`, non-empty `dedup_key`, the `(source_uid,
//!   source_uid_validity)` resume cursor — is produced *here* by the real client
//!   from real source bytes rather than hand-written to match. A drift between
//!   what the client emits and what the handler accepts fails here and passes
//!   both halves.
//! - Over the in-process lib pin `import_seals_body_and_index_hint_at_rest`
//!   (`bridge_import_handlers.rs`), which seeds a *dummy* pubkey (`[9u8; 32]`)
//!   and can therefore only assert the record *looks* sealed: this seeds a **real
//!   X25519 keypair** and **opens the sealed record with the owner's secret**,
//!   asserting the recovered plaintext is byte-for-byte the RFC 5322 the source
//!   server served. That is the difference between "something encrypted landed"
//!   and "the user's mail landed, intact and readable by them alone".
//! - The **dedup loop closing across the wire** — the key the client derives from
//!   the source bytes is the key nest indexes at import, and the key nest checks
//!   on the next one. Two assertions carry it, and they are not the same claim:
//!   (a) a second pass over the same source mailbox comes back `Skipped { reason:
//!   "dedup" }`, so the client's derivation is stable across independent IMAP
//!   sessions and survives the wire intact; (b) `already_delivered_mail_is_not_
//!   re_imported` proves the shape the user actually feels — a message they
//!   ALREADY have from normal delivery (its dedup row written by the MDA/MTA
//!   producer path, `insert_dedup_key`) is skipped on its **first** import. An
//!   import must not duplicate the mailbox it is merging into (§ Dedup: a fresh
//!   import "dedups against *all* of the actor's mail, not just previously-
//!   imported mail").
//!
//!   What this deliberately does **not** claim: that the Rust derivation is
//!   byte-identical to the Go MDA/MTA's. On a fresh nest an import populates the
//!   index itself, so (a) alone would dedup against itself even with a wrong key.
//!   Rust↔Go agreement is pinned where it belongs — the golden vector asserted on
//!   both sides (`libs/fauna-mail/src/dedup_key.rs`) — not here.
//!
//! Both dedup-key shapes ride the corpus deliberately: `MSG_WITH_ID` exercises
//! the normalized-`Message-ID` key, `MSG_NO_ID` the canonical-envelope SHA-256
//! fallback (§ Key format). The `\Recent` flag on the source message is load
//! bearing too — nest's `validate_item` rejects the whole message over it, so
//! the import only succeeds because the *client* strips it (`imap_client.rs`
//! `FetchedMessage::flags`).
//!
//! Harness: the source server is lifted from `libs/fauna-mail/tests/
//! imap_client_native.rs` (`mint_cert` / `tls_acceptor` / the `serve_imap` line
//! loop), generalized from one hard-coded message to a corpus. The nest half
//! mirrors `conformance_capability_trust_client.rs` (`start_trust_nest` +
//! `connected_client`) — an in-process `axum::serve` over a real socket, which
//! is what these tests mean by "real nest".
//!
//! The recipient seal key is a **fixture precondition** (the e2e testing-rules
//! fixture-setup carve-out: arranging the world, not the action under test). In
//! production the client's "enable mail" step provisions it; the import handler
//! fails closed without one (`import_fails_closed_without_a_registered_seal_key`
//! pins that half).

mod common;
use common::connected_client;

use std::sync::Arc;

use fauna_client::NestClient;
use fauna_core::identity::ActorKeypair;
use fauna_mail::dedup_key::mail_dedup_keys_from_slice;
use fauna_mail::envelope::sender_domain;
use fauna_mail::imap_client::test_fixtures::{
    FixtureMessage, TestCert, mint_cert, serve_imap, tls_acceptor,
};
use fauna_mail::imap_client::{
    FetchOutcome, ImapSession, NativeImapConnector, TlsMode, TokioClock,
};
use fauna_mls::wrapped_blob::is_sealed_mail_record;
use fauna_nest::db::CacheDb;
use fauna_nest::nest_identity::NestIdentity;
use fauna_nest::routes::{AppState, RegistrationConfig};
use fauna_nest::state::AuthState;
use fauna_nest::token_store::TokenStore;
use fauna_protocol::bridge_routing::{
    ImportMessageBatchReply, ImportMessageBatchRequest, ImportMessageItem, ImportMessageOutcome,
    ImportMessageReply, ImportMessageRequest, StartImportSessionReply, StartImportSessionRequest,
};
use tokio::io::AsyncWriteExt;
use tokio::net::TcpListener;

/// The importing user's identity seed.
const USER_SEED: [u8; 32] = [31u8; 32];

/// The source mailbox's UIDVALIDITY — lands in nest as the resume cursor's
/// other half.
const UID_VALIDITY: u32 = 42;

/// `01-Jan-2020 00:00:00 +0000` as epoch seconds — what the client's
/// `INTERNALDATE` parse must yield, and what nest stores as the message
/// timestamp.
const INTERNALDATE_EPOCH: i64 = 1_577_836_800;

/// `\Recent` on the first corpus message is deliberate: nest rejects it, so a
/// successful import proves the client stripped it.
///
/// Carries a `Message-ID` → dedup key is the normalized message-id.
const MSG_WITH_ID: &[u8] = b"From: alice@example.com\r\n\
To: bob@fauna.test\r\n\
Subject: the first one\r\n\
Message-ID: <a1@example.com>\r\n\
\r\n\
first body\r\n";

/// No `Message-ID` → dedup key falls back to the canonical-envelope SHA-256.
const MSG_NO_ID: &[u8] = b"From: carol@other.example\r\n\
To: bob@fauna.test\r\n\
Subject: the second one\r\n\
Date: Wed, 01 Jan 2020 00:00:00 +0000\r\n\
\r\n\
second body\r\n";

/// Carries forged reserved delivery stamps beside the one `X-Fauna-*` header
/// every door keeps. The client ships these bytes unmodified; the nest is the
/// door that must remove the forged pair before it seals (`smtp-server.md`
/// § Architectural rules → *The `X-Fauna-*` namespace*), so what rests is
/// `strip_fauna_headers` of this, not this.
const MSG_FORGED_STAMPS: &[u8] = b"X-Fauna-Spam-Threshold: 0\r\n\
x-fauna-address-suffix: forged\r\n\
\tcontinued\r\n\
X-Fauna-Forwarded-By: actor=peer; t=1; rule=forward-all\r\n\
X-Not-Fauna: keepme\r\n\
From: mallory@hostile.example\r\n\
To: bob@fauna.test\r\n\
Subject: the third one\r\n\
Message-ID: <a3@hostile.example>\r\n\
\r\n\
third body\r\n";

/// Nothing but forged delivery stamps and no header/body separator: every line
/// is a header, every header is a reserved stamp, so the nest's strip leaves
/// ZERO bytes. The body is non-empty as sent — it passes the pre-strip
/// empty-body refusal — which is exactly why the nest must re-check after the
/// strip: an empty stored body fails every later export of the account
/// (`mail-export.md` § UX shape step 2 treats one as a caller bug).
const MSG_STAMPS_ONLY: &[u8] = b"X-Fauna-Spam-Threshold: 0\r\n\
X-Fauna-Address-Suffix: forged\r\n";

/// A hostile source's mailbox: the stamp-only message beside an ordinary one,
/// so one import call proves the refusal is per-message.
const HOSTILE_CORPUS: &[FixtureMessage] = &[
    FixtureMessage {
        uid: 3,
        flags: "\\Seen",
        body: MSG_STAMPS_ONLY,
    },
    FixtureMessage {
        uid: 5,
        flags: "\\Seen",
        body: MSG_NO_ID,
    },
];

const CORPUS: &[FixtureMessage] = &[
    FixtureMessage {
        uid: 7,
        flags: "\\Seen \\Recent",
        body: MSG_WITH_ID,
    },
    FixtureMessage {
        uid: 9,
        flags: "\\Flagged",
        body: MSG_NO_ID,
    },
    FixtureMessage {
        uid: 11,
        flags: "\\Seen",
        body: MSG_FORGED_STAMPS,
    },
];

// ─────────────────────────── the source IMAP server ───────────────────────────
//
// `TestCert`/`mint_cert`/`tls_acceptor`/`serve_imap`/`FixtureMessage` are
// fauna-mail's shared `imap_client::test_fixtures` (its `tls-test-fixtures`
// feature, enabled above) — not a local copy. Nest's own `rcgen = "0.13"`
// prod-dep (used elsewhere for `web_content::cert`) pins a different major
// version than the shared fixture's `rcgen = "0.14"`; the fixture's
// `TestCert { cert_pem, key_pem }` return shape abstracts that away, so this
// file never names `rcgen` directly.

/// Spawn the implicit-TLS source server over `corpus`; returns its port.
/// Serves one session per accept, so the re-import phase can reconnect.
async fn spawn_source(cert: &TestCert, corpus: &'static [FixtureMessage]) -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let acceptor = tls_acceptor(cert);

    tokio::spawn(async move {
        loop {
            let Ok((tcp, _)) = listener.accept().await else {
                return;
            };
            let acceptor = acceptor.clone();
            tokio::spawn(async move {
                let Ok(mut tls) = acceptor.accept(tcp).await else {
                    return;
                };
                if tls.write_all(b"* OK IMAP4rev1 ready\r\n").await.is_err() {
                    return;
                }
                serve_imap(&mut tls, UID_VALIDITY, corpus).await;
            });
        }
    });

    port
}

// ─────────────────────────────── the nest ────────────────────────────────────

/// Spin a real in-process nest serving the surface an import drives: auth
/// bootstrap + discovery + the `fauna.bridges.*` import kinds.
async fn start_import_nest() -> (String, Arc<AppState>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let authority = format!("127.0.0.1:{}", addr.port());

    let db = Arc::new(CacheDb::open_in_memory().unwrap());

    let state = Arc::new(AppState {
        nest_identity: Arc::new(NestIdentity::generate()),
        rpc_router: Arc::new({
            let mut b = fauna_nest::rpc_router::RpcRouter::builder();
            fauna_nest::auth_handlers::register_auth_handlers(&mut b);
            fauna_nest::discovery_handlers::register_discovery_handlers(&mut b);
            fauna_nest::bridge_import_handlers::register_bridge_import_handlers(&mut b);
            b.build()
        }),
        auth: AuthState {
            token_store: Arc::new(TokenStore::new()),
            registration: RegistrationConfig {
                handle_domain: Some(authority.clone()),
                ..Default::default()
            },
            ..Default::default()
        },
        enforce_tier_quotas: Arc::new(tokio::sync::RwLock::new(true)),
        ..AppState::for_test(db)
    });

    let app = fauna_nest::build_router(state.clone());
    tokio::spawn(async move {
        axum::serve(listener, app).await.ok();
    });
    (format!("http://{authority}"), state)
}

// ──────────────────────────── the client half ────────────────────────────────

/// Drive the **real shared-Rust IMAP client** against the source server
/// serving `corpus` and return one `ImportMessageItem` per fetched message.
///
/// This is the production derivation, not a test-local one: the two computed
/// wire fields go through the *same* `fauna_mail` functions `FfiMailImportClient`
/// calls (`libs/fauna-ffi/src/mail_import.rs:126-127`), which is precisely why a
/// Kotlin/Swift shell is not allowed to compute them itself.
async fn fetch_from_source(
    cert: &TestCert,
    port: u16,
    corpus: &[FixtureMessage],
) -> Vec<ImportMessageItem> {
    let connector = NativeImapConnector::new(TlsMode::Implicit)
        .with_extra_root_pem(&cert.cert_pem)
        .unwrap();
    let transport = connector.connect("localhost", port).await.unwrap();

    let mut session = ImapSession::connect(transport).await.unwrap();
    session.login("alice", "hunter2").await.unwrap();

    let status = session.examine("INBOX", None).await.unwrap();
    assert_eq!(status.uid_validity, UID_VALIDITY);

    let uids: Vec<u32> = session
        .enumerate_uids(1)
        .await
        .unwrap()
        .iter()
        .map(|u| u.uid)
        .collect();
    assert_eq!(
        uids,
        corpus.iter().map(|m| m.uid).collect::<Vec<_>>(),
        "the client must enumerate every source UID"
    );

    let clock = TokioClock::new();
    let mut fetched = Vec::new();
    session
        .fetch_messages(&uids, &clock, |o| match o {
            FetchOutcome::Fetched(m) => fetched.push(*m),
            FetchOutcome::Failed { uid, reason } => {
                panic!("source fetch failed for uid {uid}: {reason}")
            }
        })
        .await
        .unwrap();
    session.logout().await.unwrap();

    fetched.sort_by_key(|m| m.uid);
    fetched
        .into_iter()
        .map(|m| {
            // The derived fields — same functions the FFI facade calls.
            let keys = mail_dedup_keys_from_slice(&m.body);
            ImportMessageItem {
                mailbox: m.mailbox,
                flags: m.flags,
                timestamp: m.internal_date_epoch,
                body_size: m.body.len() as u32,
                dedup_key: keys.dedup_key,
                envelope_key: keys.envelope_key,
                sender_domain: sender_domain(&m.body),
                source_uid: m.uid,
                source_uid_validity: m.uid_validity,
                body: m.body,
                staged_body: None,
            }
        })
        .collect()
}

/// Push one message over the real WS-RPC import kind.
async fn import_one(
    nest: &NestClient,
    session_id: &str,
    message: ImportMessageItem,
) -> ImportMessageReply {
    nest.request(
        "fauna.bridges.import_message",
        ImportMessageRequest {
            session_id: session_id.to_string(),
            message,
            skip_dedup: false,
        },
    )
    .await
    .expect("import_message RPC")
}

/// The sealed source label, minted exactly as the app mints it: the owner root
/// derived from this user's own signing secret, convergent under
/// `import_source_hash`. Deriving it here rather than passing a stand-in blob is
/// what makes the round-trip assertion below meaningful -- a fixture the test
/// invented would prove only that the column accepts bytes.
fn source_seal(source: &str) -> Vec<u8> {
    let key = fauna_core::crypto::BackupKey::derive(&USER_SEED);
    let root = fauna_core::path_crypto::LabelRoot::owner_of(&key);
    fauna_core::label_custody::seal_import_source(&root, source).expect("seal the import source")
}

async fn start_session(nest: &NestClient, source: &str) -> String {
    let reply: StartImportSessionReply = nest
        .request(
            "fauna.bridges.start_import_session",
            StartImportSessionRequest {
                source_descriptor: source.to_string(),
                total_count: CORPUS.len() as u64,
                scope: vec!["INBOX".to_string()],
                source_sealed: Some(serde_bytes::ByteBuf::from(source_seal(source))),
                ..Default::default()
            },
        )
        .await
        .expect("start_import_session RPC");
    reply.session_id
}

// ───────────────────────────────── the test ──────────────────────────────────

#[tokio::test]
async fn imports_from_a_real_imap_source_into_a_real_nest_sealed_and_dedup_indexed() {
    let cert = mint_cert();
    let source_port = spawn_source(&cert, CORPUS).await;
    let (base, state) = start_import_nest().await;

    // ── Fixture precondition: a registered user with a real recipient seal key.
    // In production "enable mail" provisions this; the handler fails closed
    // without it. The secret half never leaves this test — it is the owner's,
    // and it is what lets us prove below that the sealed record is *theirs*.
    let user = ActorKeypair::from_secret(USER_SEED);
    let user_id = user.actor_id().0;
    state
        .db
        .create_user(&user_id, "free", "importer")
        .await
        .unwrap();
    let owner_msek = [0x6fu8; 32];
    common::seed_recipient_seal_key(&state.db, &user_id, &owner_msek).await;

    // ── The client half: a real IMAP session over a real TLS socket.
    let items = fetch_from_source(&cert, source_port, CORPUS).await;
    assert_eq!(items.len(), CORPUS.len());

    // The client stripped `\Recent` (nest would reject the message over it) and
    // kept the real source flag.
    assert!(
        !items[0].flags.iter().any(|f| f == "\\Recent"),
        "the client must strip \\Recent — nest's validate_item rejects it"
    );
    assert!(items[0].flags.contains(&"\\Seen".to_string()));

    // Both dedup-key shapes are exercised, and both derivations ran.
    assert_eq!(items[0].sender_domain, "example.com");
    assert_eq!(items[1].sender_domain, "other.example");
    assert!(!items[0].dedup_key.is_empty());
    assert!(!items[1].dedup_key.is_empty());
    assert_ne!(
        items[0].dedup_key, items[1].dedup_key,
        "distinct messages must not collide on the dedup key"
    );

    // ── The nest half: push each message over the real authenticated WS-RPC.
    let nest = connected_client(&base, ActorKeypair::from_secret(USER_SEED)).await;
    let sid = start_session(&nest, "generic:imap.example.com:alice").await;

    let mut message_ids = Vec::new();
    for (i, item) in items.iter().enumerate() {
        // The client sends the source bytes unmodified — pinned here, since the
        // strip below is the NEST's duty and a client-side strip would leave
        // that door untested.
        assert_eq!(
            item.body, CORPUS[i].body,
            "the client must ship the source bytes unmodified"
        );
        // What rests is the source bytes minus every forged `X-Fauna-*` stamp
        // (a no-op on the first two; the third loses its forged pair and keeps
        // `X-Fauna-Forwarded-By` and the `X-Not-Fauna` look-alike).
        let expected_body = fauna_mail::received_header::strip_fauna_headers(CORPUS[i].body);
        if i == 2 {
            assert_ne!(
                expected_body, CORPUS[i].body,
                "the fixture must carry a forged stamp"
            );
            let text = String::from_utf8_lossy(&expected_body);
            assert!(!text.contains("X-Fauna-Spam-Threshold") && !text.contains("forged"));
            assert!(
                text.contains("X-Fauna-Forwarded-By: actor=peer")
                    && text.contains("X-Not-Fauna: keepme")
            );
        }
        let reply = import_one(&nest, &sid, item.clone()).await;
        let ImportMessageOutcome::Imported {
            message_id,
            uid,
            uid_validity,
        } = reply.outcome
        else {
            panic!(
                "expected Imported for source uid {}, got {:?}",
                item.source_uid, reply.outcome
            );
        };
        // These are the **destination** placement's coordinates — imports share
        // APPEND's insert path, so nest assigns its own mailbox UID. The
        // *source* cursor is asserted below, off the session row.
        assert!(uid >= 1 && uid_validity >= 1, "a real nest-side placement");
        assert_eq!(item.timestamp, INTERNALDATE_EPOCH, "INTERNALDATE survived");
        message_ids.push((message_id, expected_body, item.dedup_key.clone()));
    }
    assert_eq!(message_ids.len(), CORPUS.len());

    // The resume cursor: nest folded the *source* (uid, uid_validity) of the
    // last processed message, so a resumed import never refetches it.
    let row = state
        .db
        .get_import_session(&user_id, &sid)
        .await
        .unwrap()
        .expect("the import session row");
    assert_eq!(row.imported_count, CORPUS.len() as u64);
    assert_eq!(
        row.cursors.get("INBOX"),
        Some(&(CORPUS[CORPUS.len() - 1].uid, UID_VALIDITY)),
        "the source resume cursor must round-trip into the session row"
    );

    // ── The session's SOURCE LABEL rests sealed, and opens to the descriptor
    // under the owner's own custody.
    //
    // The lib-level pins can only show that the column accepts and returns
    // bytes; this one drives the real `start_import_session` RPC over the wire
    // and then opens what rests with the key derived from this user's signing
    // secret. That is the assertion whose absence let `source_sealed` ship with
    // no production writer at all: the pre-existing at-rest coverage sealed its
    // own fixtures, so it observed the schema and never the writer.
    {
        use fauna_core::path_crypto::SealedLabelRender;
        let sealed = row.source_sealed.as_deref().expect(
            "the descriptor must rest SEALED -- it is the user's external mailbox              identity (provider + host + username) and otherwise sits in plaintext              for the row's 30-day life",
        );
        let keys = fauna_core::file_download::FileDownloadKeys::owner(
            fauna_core::crypto::BackupKey::derive(&USER_SEED),
        );
        // Rendered the way the resume screen renders it, and deliberately with
        // an EMPTY plaintext: that is the post-boot-scrub state, so this proves
        // the label is migrated rather than destroyed.
        assert_eq!(
            fauna_core::label_custody::render_import_source(
                &keys,
                Some(sealed),
                "",
                row.source_hash.as_deref(),
            ),
            SealedLabelRender::Sealed("generic:imap.example.com:alice".to_string()),
            "a scrubbed session must still render its source from the seal"
        );
    }

    // ── At rest: sealed, and openable by the owner alone — byte-for-byte the
    // RFC 5322 the source server served.
    for (message_id, expected_body, dedup_key) in &message_ids {
        let (env, _floor) = fauna_nest::segments::mail::read_record_with_floor(
            &state.mail_segments,
            &state.db,
            &user_id,
            message_id,
        )
        .await
        .unwrap()
        .expect("the imported record must be readable at rest");

        assert!(
            is_sealed_mail_record(&env.encrypted_body),
            "imported body must rest sealed (encryption-at-rest.md S1 uniform seal at ingest)"
        );
        assert!(
            is_sealed_mail_record(&env.encrypted_index_hint),
            "imported index hint must rest sealed too"
        );

        // The proof the lib pin cannot make: open it with the owner's keys.
        let opened = common::open_recipient_record(&env.encrypted_body, &owner_msek);
        assert_eq!(
            &opened, expected_body,
            "the plaintext recovered from the sealed record must be byte-for-byte \
             the RFC 5322 the source server served, minus any forged X-Fauna-* stamp"
        );

        // The dedup key the *client* derived is the key *nest* stored.
        assert!(
            state.db.has_dedup_key(&user_id, dedup_key).await.unwrap(),
            "nest must have indexed the client-derived dedup key"
        );
    }

    // ── Re-import the same source mailbox: every message is a duplicate.
    //
    // This is the assertion that proves the client's dedup derivation agrees
    // byte-for-byte with the key nest already holds — the whole point of
    // § Dedup. A drift between the two derivations shows up *only* here: it
    // would silently double-store the user's entire mailbox on a resumed or
    // repeated import, and every other test in the tree would still pass.
    let items_again = fetch_from_source(&cert, source_port, CORPUS).await;
    let sid2 = start_session(&nest, "generic:imap.example.com:alice-second-pass").await;

    for item in &items_again {
        let reply = import_one(&nest, &sid2, item.clone()).await;
        match reply.outcome {
            ImportMessageOutcome::Skipped { reason } => assert_eq!(
                reason, "dedup",
                "a re-imported message must be skipped as a duplicate"
            ),
            other => panic!(
                "re-import of uid {} must be skipped, got {other:?}",
                item.source_uid
            ),
        }
    }
}

/// Mail the user **already has from normal delivery** is not duplicated by an
/// import — the promise a user actually feels when they merge an old mailbox
/// into a Fauna account that has been receiving mail for weeks.
///
/// The precondition is the *producer* path, not the import path: the MDA/MTA
/// record a delivered message's dedup key through `insert_dedup_key` (nest
/// cannot compute it — it holds only ciphertext), exactly as this fixture does.
/// The import then meets that row on the message's **first** import and skips
/// it. This is the half `imports_from_a_real_imap_source_…`'s re-import pass
/// cannot reach: there, the import populated the index itself.
#[tokio::test]
async fn already_delivered_mail_is_not_re_imported() {
    let cert = mint_cert();
    let source_port = spawn_source(&cert, CORPUS).await;
    let (base, state) = start_import_nest().await;

    let user = ActorKeypair::from_secret(USER_SEED);
    let user_id = user.actor_id().0;
    state
        .db
        .create_user(&user_id, "free", "importer")
        .await
        .unwrap();
    let owner_msek = [0x6fu8; 32];
    common::seed_recipient_seal_key(&state.db, &user_id, &owner_msek).await;

    // The first corpus message was already delivered by the MTA: its dedup row
    // exists, written by the producer path. The second was not.
    let already_have = mail_dedup_keys_from_slice(MSG_WITH_ID);
    state
        .db
        .insert_dedup_key(
            &user_id,
            &already_have.dedup_key,
            &already_have.envelope_key,
            "mail://delivered-earlier",
        )
        .await
        .unwrap();

    let items = fetch_from_source(&cert, source_port, CORPUS).await;
    let nest = connected_client(&base, ActorKeypair::from_secret(USER_SEED)).await;
    let sid = start_session(&nest, "generic:imap.example.com:alice").await;

    // The already-delivered one is skipped on its FIRST import…
    let reply = import_one(&nest, &sid, items[0].clone()).await;
    assert!(
        matches!(
            &reply.outcome,
            ImportMessageOutcome::Skipped { reason } if reason == "dedup"
        ),
        "mail the user already has must not be imported twice, got {:?}",
        reply.outcome
    );

    // …while the message they genuinely lack still lands.
    let reply = import_one(&nest, &sid, items[1].clone()).await;
    assert!(
        matches!(&reply.outcome, ImportMessageOutcome::Imported { .. }),
        "a message the user does NOT have must still import, got {:?}",
        reply.outcome
    );
    assert_eq!(reply.imported_count, 1);
    assert_eq!(reply.skipped_count, 1);
}

/// A stranger who knows a Message-ID in the mailbox the user has not imported
/// yet cannot make the real message skip (`mailbox-migration.md` § The envelope
/// key confirms a Message-ID hit). The stranger's delivery is recorded through
/// the MTA producer path first — same Message-ID, other content — and the real
/// message, fetched from a real source and keyed by the real client derivation,
/// still imports on its first attempt.
#[tokio::test]
async fn a_strangers_reused_message_id_does_not_pre_empt_the_real_message() {
    let cert = mint_cert();
    let source_port = spawn_source(&cert, CORPUS).await;
    let (base, state) = start_import_nest().await;

    let user = ActorKeypair::from_secret(USER_SEED);
    let user_id = user.actor_id().0;
    state
        .db
        .create_user(&user_id, "free", "importer")
        .await
        .unwrap();
    let owner_msek = [0x6fu8; 32];
    common::seed_recipient_seal_key(&state.db, &user_id, &owner_msek).await;

    // The stranger reuses `<a1@example.com>` with their own headers and body.
    const FORGED: &[u8] = b"From: stranger@evil.test\r\n\
To: bob@fauna.test\r\n\
Subject: the first one\r\n\
Message-ID: <a1@example.com>\r\n\
\r\n\
forged body\r\n";
    let forged = mail_dedup_keys_from_slice(FORGED);
    assert_eq!(
        forged.dedup_key,
        mail_dedup_keys_from_slice(MSG_WITH_ID).dedup_key,
        "the attack premise: the forged copy shares the real message's lookup key"
    );
    state
        .db
        .insert_dedup_key(
            &user_id,
            &forged.dedup_key,
            &forged.envelope_key,
            "mail://planted-by-a-stranger",
        )
        .await
        .unwrap();

    let items = fetch_from_source(&cert, source_port, CORPUS).await;
    let nest = connected_client(&base, ActorKeypair::from_secret(USER_SEED)).await;
    let sid = start_session(&nest, "generic:imap.example.com:alice").await;

    let reply = import_one(&nest, &sid, items[0].clone()).await;
    assert!(
        matches!(&reply.outcome, ImportMessageOutcome::Imported { .. }),
        "a stranger's reused Message-ID must not pre-empt the real message, got {:?}",
        reply.outcome
    );

    // First writer wins and a row never updates (§ The envelope key confirms
    // a Message-ID hit), so the planted row still disagrees with the real
    // message: a re-import stores it again. That is the rule's chosen failure
    // direction — a double-store, never a lost message.
    let reply = import_one(&nest, &sid, items[0].clone()).await;
    assert!(
        matches!(&reply.outcome, ImportMessageOutcome::Imported { .. }),
        "first writer wins: the planted row keeps disagreeing with the real \
         message, so a re-import stores it again (a double-store, never a lost \
         message), got {:?}",
        reply.outcome
    );
}

/// A message a hostile source serves as nothing but `X-Fauna-*` stamps is
/// refused **after** the nest strips them, as that message's own `Errored` —
/// never filed as an empty body — while its sibling in the same batch still
/// imports (`mailbox-migration.md`, the per-message refusals). Without the
/// post-strip check the stamp-only message rests with a zero-length body, and
/// every later export of the account fails on it (`mail-export.md` § UX shape
/// step 2).
#[tokio::test]
async fn a_body_the_stamp_strip_empties_is_refused_and_its_sibling_still_imports() {
    let cert = mint_cert();
    let source_port = spawn_source(&cert, HOSTILE_CORPUS).await;
    let (base, state) = start_import_nest().await;

    let user = ActorKeypair::from_secret(USER_SEED);
    let user_id = user.actor_id().0;
    state
        .db
        .create_user(&user_id, "free", "importer")
        .await
        .unwrap();
    let owner_msek = [0x6fu8; 32];
    common::seed_recipient_seal_key(&state.db, &user_id, &owner_msek).await;

    let items = fetch_from_source(&cert, source_port, HOSTILE_CORPUS).await;
    // The client ships the stamp-only bytes unmodified and non-empty; the
    // strip is the nest's, and it empties them.
    assert_eq!(items[0].body, MSG_STAMPS_ONLY);
    assert!(
        fauna_mail::received_header::strip_fauna_headers(&items[0].body).is_empty(),
        "the fixture must strip to zero bytes"
    );

    let nest = connected_client(&base, ActorKeypair::from_secret(USER_SEED)).await;
    let sid = start_session(&nest, "generic:imap.hostile.example:alice").await;
    let reply: ImportMessageBatchReply = nest
        .request(
            "fauna.bridges.import_message_batch",
            ImportMessageBatchRequest {
                session_id: sid,
                messages: items.clone(),
                skip_dedup: false,
                revised_total_count: None,
            },
        )
        .await
        .expect("import_message_batch RPC");

    assert!(
        matches!(
            &reply.outcomes[0],
            ImportMessageOutcome::Errored { reason } if reason.contains("X-Fauna-*")
        ),
        "a body the X-Fauna-* strip empties must be refused, got {:?}",
        reply.outcomes[0]
    );
    assert!(
        matches!(&reply.outcomes[1], ImportMessageOutcome::Imported { .. }),
        "the sibling in the same call must still import, got {:?}",
        reply.outcomes[1]
    );
    assert_eq!((reply.imported_count, reply.errored_count), (1, 1));

    // The store holds the sibling alone — no zero-length record was filed, and
    // the refused message left no dedup row that would skip a later retry.
    assert_eq!(
        state
            .db
            .count_bridge_imap_live_messages(&user_id, "INBOX")
            .await
            .unwrap(),
        1,
        "only the sibling may rest in the mailbox"
    );
    assert!(
        !state
            .db
            .has_dedup_key(&user_id, &items[0].dedup_key)
            .await
            .unwrap()
    );
}
