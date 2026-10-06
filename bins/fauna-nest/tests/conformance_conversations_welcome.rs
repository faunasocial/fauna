//! Integration round-trip for `fauna.conversations.welcome.deliver` —
//! same-nest MLS Welcome delivery. Mirrors
//! `conformance_conversations_channel.rs` / `_keypackage.rs`; reaches
//! `CacheDb` directly through `state.db.{push_inbox, list_inbox_all,
//! list_actor_channels}` via the handler.
//!
//! Authority for the wire types: `libs/fauna-protocol/src/conversations.rs`.
//! Authority for the slice + per-kind replay semantics: tracked
//! internally (§ T3 + § Per-kind replay semantics).

mod common;
use common::dispatch;

use std::sync::Arc;

use bytes::Bytes;

use fauna_nest::{
    bridge_method_allowlist::{CallerClass, is_permitted},
    conversations_handlers,
    db::CacheDb,
    routes::AppState,
    rpc_router::RpcRouter,
};
use fauna_protocol::{
    conversations::{WelcomeDeliverReply, WelcomeDeliverRequest, WelcomeKind},
    decode_strict as decode, encode_canonical,
    inbox::InboxEnvelope,
};

async fn router_with_db_only() -> (RpcRouter, Arc<AppState>) {
    let db = Arc::new(CacheDb::open_in_memory().unwrap());
    let state = Arc::new(AppState::for_test(db));
    let mut b = RpcRouter::builder();
    conversations_handlers::register_conversations_handlers(&mut b);
    (b.build(), state)
}

fn deliver_payload(
    recipient_hex: &str,
    channel_id_hex: &str,
    welcome_bytes: Vec<u8>,
    kind: WelcomeKind,
) -> Bytes {
    let req = WelcomeDeliverRequest {
        recipient_actor_id: recipient_hex.into(),
        channel_id: channel_id_hex.into(),
        welcome_bytes,
        kind,
        nest_url: None,
        extra: std::collections::BTreeMap::new(),
    };
    Bytes::from(encode_canonical(&req).unwrap().to_vec())
}

// ── fauna.conversations.welcome.deliver — DM kind ──────────────

#[tokio::test]
async fn deliver_dm_pushes_inbox_row_and_returns_id() {
    let (router, state) = router_with_db_only().await;
    let sender = [11u8; 32];
    let recipient = [22u8; 32];
    let channel_id_hex = "ab".repeat(32);
    let welcome = vec![0xaa, 0xbb, 0xcc, 0xdd];

    // The mode gate acts on NEW parties (direct-messages.md § Reach policy);
    // this test's subject is delivery mechanics, so arrange an accepted
    // contact and let it flow.
    state
        .db
        .upsert_contact(&recipient, &sender, "accepted")
        .await
        .unwrap();

    let reply_bytes = dispatch(
        &router,
        state.clone(),
        sender,
        "fauna.conversations.welcome.deliver",
        deliver_payload(
            &hex::encode(recipient),
            &channel_id_hex,
            welcome.clone(),
            WelcomeKind::Dm,
        ),
    )
    .await
    .expect("deliver ok");
    let reply: WelcomeDeliverReply = decode(&reply_bytes).unwrap();
    assert!(reply.inbox_id > 0, "inbox row id assigned");

    // The recipient now has exactly one inbox row carrying the canonical
    // DAG-CBOR Welcome envelope (layer 1): the same-nest path wraps the raw
    // Welcome with its channel metadata (id / type) so the durable drain
    // backstop recovers the routing after a missed push. The raw Welcome bytes
    // ride inside the envelope.
    let rows = state.db.list_inbox_all(&recipient).await.unwrap();
    assert_eq!(rows.len(), 1, "single inbox row");
    let (id, payload, _blob_hash, _created_at, _delivered) = &rows[0];
    assert_eq!(*id, reply.inbox_id);
    let w = InboxEnvelope::from_canonical_bytes(payload)
        .expect("inbox envelope decodes")
        .decode_welcome()
        .expect("welcome inbox decodes");
    assert_eq!(
        w.welcome_bytes, welcome,
        "raw Welcome bytes ride inside the envelope"
    );
    assert_eq!(w.channel_type.as_deref(), Some("dm"));
}

#[tokio::test]
async fn deliver_dm_auto_registers_recipient_on_channel() {
    let (router, state) = router_with_db_only().await;
    let sender = [33u8; 32];
    let recipient = [44u8; 32];
    let channel_id_hex = "cd".repeat(32);
    let channel_id = [0xcd_u8; 32];

    // Before delivery — recipient has no channels.
    let before = state.db.list_actor_channels(&recipient).await.unwrap();
    assert!(before.is_empty());

    // The mode gate acts on NEW parties (direct-messages.md § Reach policy);
    // this test's subject is delivery mechanics, so arrange an accepted
    // contact and let it flow.
    state
        .db
        .upsert_contact(&recipient, &sender, "accepted")
        .await
        .unwrap();

    dispatch(
        &router,
        state.clone(),
        sender,
        "fauna.conversations.welcome.deliver",
        deliver_payload(
            &hex::encode(recipient),
            &channel_id_hex,
            vec![0x01],
            WelcomeKind::Dm,
        ),
    )
    .await
    .expect("deliver ok");

    let after = state.db.list_actor_channels(&recipient).await.unwrap();
    assert_eq!(
        after,
        vec![channel_id],
        "recipient auto-registered on welcome"
    );
}

// ── fauna.conversations.welcome.deliver — Group kind ───────────

#[tokio::test]
async fn deliver_group_kind_round_trips() {
    let (router, state) = router_with_db_only().await;
    let sender = [55u8; 32];
    let recipient = [66u8; 32];
    let channel_id_hex = "ef".repeat(32);
    let group_id_hex = "12".repeat(32);
    let welcome = vec![0x77, 0x88, 0x99];

    // The mode gate acts on NEW parties (direct-messages.md § Reach policy);
    // this test's subject is delivery mechanics, so arrange an accepted
    // contact and let it flow.
    state
        .db
        .upsert_contact(&recipient, &sender, "accepted")
        .await
        .unwrap();

    let reply_bytes = dispatch(
        &router,
        state.clone(),
        sender,
        "fauna.conversations.welcome.deliver",
        deliver_payload(
            &hex::encode(recipient),
            &channel_id_hex,
            welcome.clone(),
            WelcomeKind::Group {
                group_id: group_id_hex.clone(),
            },
        ),
    )
    .await
    .expect("deliver group ok");
    let reply: WelcomeDeliverReply = decode(&reply_bytes).unwrap();
    assert!(reply.inbox_id > 0);

    // Group-kind welcomes store the canonical envelope carrying channel_type
    // ("group") + the group id alongside the raw Welcome bytes, so the durable
    // drain backstop recovers the routing metadata after a missed push.
    let rows = state.db.list_inbox_all(&recipient).await.unwrap();
    assert_eq!(rows.len(), 1);
    let w = InboxEnvelope::from_canonical_bytes(&rows[0].1)
        .expect("inbox envelope decodes")
        .decode_welcome()
        .expect("welcome inbox decodes");
    assert_eq!(w.welcome_bytes, welcome);
    assert_eq!(w.channel_type.as_deref(), Some("group"));
    assert_eq!(w.group_id.as_deref(), Some(group_id_hex.as_str()));
}

// ── fauna.conversations.welcome.deliver — Folder kind ─────────

#[tokio::test]
async fn deliver_folder_kind_persists_folder_channel_type() {
    let (router, state) = router_with_db_only().await;
    let sender = [0x5a_u8; 32];
    let recipient = [0xa5_u8; 32];
    let channel_id_hex = "f1".repeat(32);
    let channel_id = [0xf1_u8; 32];
    let group_id_hex = "9c".repeat(32);
    let welcome = vec![0xf1, 0x1e, 0x5e, 0x70];

    // The production shape: `folders.share` claims the channel to the owner
    // before the client delivers the Welcome. That claim row — not the
    // `Folder` label — is what exempts a share from the recipient's inbox
    // mode, so a faithful fixture writes it.
    state
        .db
        .claim_folder_channel(&sender, &channel_id)
        .await
        .unwrap();

    let reply_bytes = dispatch(
        &router,
        state.clone(),
        sender,
        "fauna.conversations.welcome.deliver",
        deliver_payload(
            &hex::encode(recipient),
            &channel_id_hex,
            welcome.clone(),
            WelcomeKind::Folder {
                group_id: group_id_hex.clone(),
            },
        ),
    )
    .await
    .expect("deliver folder ok");
    let reply: WelcomeDeliverReply = decode(&reply_bytes).unwrap();
    assert!(reply.inbox_id > 0);

    // The persisted inbox envelope carries channel_type="folder" (+ the group
    // id). This is the metadata the durable drain backstop routes on, so the
    // recipient maps it to `WelcomeChannelKind::Folder` (away from the chat rail)
    // even after a missed push — the whole point of the Folder kind vs. Group.
    let rows = state.db.list_inbox_all(&recipient).await.unwrap();
    assert_eq!(rows.len(), 1);
    let env = InboxEnvelope::from_canonical_bytes(&rows[0].1).expect("inbox envelope decodes");
    let w = env.decode_welcome().expect("welcome inbox decodes");
    assert_eq!(w.channel_type.as_deref(), Some("folder"));
    assert_eq!(w.group_id.as_deref(), Some(group_id_hex.as_str()));
    assert_eq!(w.welcome_bytes, welcome);

    // The sharer is nest-stamped from the authenticated caller (the sender),
    // so the recipient contact gate reads the sharer's contact-status without a
    // roster round-trip (folders.md § Sharing). Unspoofable: it is the caller
    // the dispatcher authenticated, not a client-supplied field.
    assert_eq!(w.shared_by.as_deref(), Some(hex::encode(sender).as_str()));

    // The sender here has no `users`-table handle, so the nest leaves
    // `shared_by_handle` absent and the recipient falls back to the shortened
    // `shared_by` hex — the resolve must never invent a handle or fail delivery.
    assert!(
        w.shared_by_handle.is_none(),
        "no handle set → shared_by_handle stays None (hex fallback)"
    );

    // And the recipient is auto-registered on the derived channel (like any
    // welcome), so a later content-key / snapshot fetch resolves.
    let channels = state.db.list_actor_channels(&recipient).await.unwrap();
    assert_eq!(channels, vec![channel_id]);
}

#[tokio::test]
async fn deliver_folder_kind_stamps_sharer_handle() {
    // The nest resolves the authenticated sharer's handle into `shared_by_handle`
    // on a same-nest folder welcome, so the recipient renders "Shared by
    // ‹handle›" without a roster round-trip (folders.md § Sharing). Resolved
    // from the SAME authenticated caller the dispatcher gated — unspoofable, and
    // same-nest only (the sharer is a local `users` row).
    let (router, state) = router_with_db_only().await;
    let sender = [0x5a_u8; 32];
    let recipient = [0xa5_u8; 32];
    let channel_id_hex = "f2".repeat(32);
    let group_id_hex = "9c".repeat(32);
    let welcome = vec![0xf1, 0x1e, 0x5e, 0x70];

    // Register the sharer with a handle in the local users table.
    state
        .db
        .create_user(&sender, "free", "alice")
        .await
        .unwrap();
    state.db.set_handle(&sender, "alice").await.unwrap();
    // …and claim the channel to them, as `folders.share` does before the
    // client's Welcome.
    state
        .db
        .claim_folder_channel(&sender, &[0xf2_u8; 32])
        .await
        .unwrap();

    dispatch(
        &router,
        state.clone(),
        sender,
        "fauna.conversations.welcome.deliver",
        deliver_payload(
            &hex::encode(recipient),
            &channel_id_hex,
            welcome,
            WelcomeKind::Folder {
                group_id: group_id_hex,
            },
        ),
    )
    .await
    .expect("deliver folder ok");

    let rows = state.db.list_inbox_all(&recipient).await.unwrap();
    let w = InboxEnvelope::from_canonical_bytes(&rows[0].1)
        .expect("inbox envelope decodes")
        .decode_welcome()
        .expect("welcome inbox decodes");
    assert_eq!(w.shared_by.as_deref(), Some(hex::encode(sender).as_str()));
    assert_eq!(
        w.shared_by_handle.as_deref(),
        Some("alice"),
        "the sharer's handle is nest-resolved into shared_by_handle"
    );
}

/// Path-sealing S5c-2: the same-nest `WelcomeInbox` carries the claimed
/// set's name-seal + salt pair, resolved from the same `claimed_fs` row
/// `set_name` itself resolves from — no extra query. Uniform stamp, same
/// reasoning as `set_name`.
#[tokio::test]
async fn deliver_folder_kind_stamps_the_set_name_seal_and_salt_pair() {
    let (router, state) = router_with_db_only().await;
    let owner = [0x5a_u8; 32];
    let recipient = [0xa5_u8; 32];
    let group_id = vec![0x88u8; 24];
    let channel_id = fauna_mls::types::ChannelId::from_group_id(&group_id).0;
    let channel_id_hex = hex::encode(channel_id);
    let welcome = vec![0xf1, 0x1e, 0x5e, 0x70];

    state
        .db
        .create_folder("shared-notes", &owner)
        .await
        .unwrap();
    state
        .db
        .set_folder_mls_group("shared-notes", &owner, Some(&group_id))
        .await
        .unwrap();
    state
        .db
        .claim_folder_channel(&owner, &channel_id)
        .await
        .unwrap();

    // Only a keyed writer stamps the seal; stamp it directly, as the
    // engine's bind/serve catch-up pass would (mirrors
    // `conformance_media_list.rs`'s `bind_shared`-based tests).
    let sealed = vec![0xEDu8; 48];
    state
        .db
        .update_folder_for_user(
            "shared-notes",
            &owner,
            fauna_nest::db::FolderUpdate {
                name_sealed: Some(&sealed),
                ..Default::default()
            },
        )
        .await
        .unwrap();

    dispatch(
        &router,
        state.clone(),
        owner,
        "fauna.conversations.welcome.deliver",
        deliver_payload(
            &hex::encode(recipient),
            &channel_id_hex,
            welcome,
            WelcomeKind::Folder {
                group_id: hex::encode(&group_id),
            },
        ),
    )
    .await
    .expect("deliver folder ok");

    let rows = state.db.list_inbox_all(&recipient).await.unwrap();
    let w = InboxEnvelope::from_canonical_bytes(&rows[0].1)
        .expect("inbox envelope decodes")
        .decode_welcome()
        .expect("welcome inbox decodes");
    assert_eq!(
        w.set_name_sealed.as_deref().map(|b| b.to_vec()),
        Some(sealed),
        "the claimed set's seal rides the same-nest envelope verbatim"
    );
    assert_eq!(
        w.set_name_hash.as_deref().map(|b| b.to_vec()),
        Some(fauna_core::path_crypto::set_name_hash("shared-notes").to_vec()),
        "and the salt that opens it"
    );
    // The sealed set's row rests no name, so neither does the resting
    // envelope: the pair alone, never an empty or plaintext `set_name`.
    assert_eq!(w.set_name, None);
    assert!(
        !rows[0].1.windows(12).any(|w| w == b"shared-notes"),
        "the inbox row rests no plaintext set name"
    );
}

#[tokio::test]
async fn deliver_dm_welcome_carries_no_sharer_stamp() {
    // Only folder welcomes are contact-gated on the recipient side, so only
    // they carry a `shared_by` stamp. A DM welcome (the recipient already shares
    // the DM with the sender) must NOT stamp one — no behavior change vs. Slice A.
    let (router, state) = router_with_db_only().await;
    let sender = [0x33_u8; 32];
    let recipient = [0x44_u8; 32];
    let channel_id_hex = "cd".repeat(32);
    let welcome = vec![0x01, 0x02];

    // The mode gate acts on NEW parties (direct-messages.md § Reach policy);
    // this test's subject is delivery mechanics, so arrange an accepted
    // contact and let it flow.
    state
        .db
        .upsert_contact(&recipient, &sender, "accepted")
        .await
        .unwrap();

    dispatch(
        &router,
        state.clone(),
        sender,
        "fauna.conversations.welcome.deliver",
        deliver_payload(
            &hex::encode(recipient),
            &channel_id_hex,
            welcome,
            WelcomeKind::Dm,
        ),
    )
    .await
    .expect("deliver dm ok");

    let rows = state.db.list_inbox_all(&recipient).await.unwrap();
    let w = InboxEnvelope::from_canonical_bytes(&rows[0].1)
        .expect("inbox envelope decodes")
        .decode_welcome()
        .expect("welcome inbox decodes");
    assert_eq!(w.channel_type.as_deref(), Some("dm"));
    assert!(w.shared_by.is_none(), "DM welcomes carry no sharer stamp");
    assert!(
        w.shared_by_handle.is_none(),
        "no sharer stamp ⇒ no handle to resolve"
    );
}

// ── replay / idempotency ───────────────────────────────────────

#[tokio::test]
async fn deliver_twice_creates_two_inbox_rows() {
    // forbid_replay=true at the kind metadata level prevents the
    // NestClient::request_auto_retry path from re-issuing on
    // disconnect. The handler itself is not idempotent at the DB
    // layer: two explicit deliveries produce two inbox rows (the MLS
    // engine on the recipient would reject the second as
    // already-processed, but that's a client-side guard, not a server
    // one). This test pins the server behavior so a future "make
    // welcome.deliver idempotent" change has to be explicit.
    //
    // Regression guard: the two deliveries are back-to-back and so land
    // in the same millisecond, which used to collide the derived inbox
    // `content_id` and fail the second on the `idx_links_unique_delivery`
    // index ("storage error"). The per-insertion nonce in
    // `CacheDb::inbox_content_id` makes each row's id unique regardless of
    // timing; this test flaked (passed only when the loop straddled a ms
    // boundary) before that fix.
    let (router, state) = router_with_db_only().await;
    let sender = [77u8; 32];
    let recipient = [88u8; 32];
    let channel_id_hex = "ab".repeat(32);
    let welcome = vec![0xab_u8, 0xcd, 0xef];

    // The mode gate acts on NEW parties (direct-messages.md § Reach policy);
    // this test's subject is delivery mechanics, so arrange an accepted
    // contact and let it flow.
    state
        .db
        .upsert_contact(&recipient, &sender, "accepted")
        .await
        .unwrap();

    for _ in 0..2 {
        dispatch(
            &router,
            state.clone(),
            sender,
            "fauna.conversations.welcome.deliver",
            deliver_payload(
                &hex::encode(recipient),
                &channel_id_hex,
                welcome.clone(),
                WelcomeKind::Dm,
            ),
        )
        .await
        .expect("deliver ok");
    }
    let rows = state.db.list_inbox_all(&recipient).await.unwrap();
    assert_eq!(rows.len(), 2, "two explicit deliveries → two inbox rows");
}

// ── inbox mode on the Welcome plane (direct-messages.md § Reach policy) ──
//
// The recipient's inbox mode is the unsupervised arm of the DM reach policy:
// the floor's `Proceed` hands the arrival to "the caller's own routing", and
// for a conversation-shaped Welcome (Dm / Group) that routing is
// `dm_initiation_mode_verdict`. Folder and Scheduling kinds are deliberately
// NOT mode-gated (each has its own ratified recipient-side story) — pinned
// below so a regression in either direction is loud.

/// The refusal every mode-gated arm answers with: deliberately the SAME code
/// the supervised floor uses, so a sender cannot distinguish `contacts_only`
/// from supervision (family-safety.md § Don't do these) — and deployed
/// clients already render it.
const FORBIDDEN: &str = "fauna.conversations.forbidden";

#[tokio::test]
async fn deliver_dm_from_a_stranger_is_refused_under_the_default_mode() {
    // No contact edge, no mode row: the stored default is `allow_knock`, whose
    // FAQ-ratified meaning is "the recipient must accept before DMs flow". The
    // sender's path is the contact request (`fauna.inbox.send`).
    let (router, state) = router_with_db_only().await;
    let sender = [0xA1u8; 32];
    let recipient = [0xA2u8; 32];

    let err = dispatch(
        &router,
        state,
        sender,
        "fauna.conversations.welcome.deliver",
        deliver_payload(
            &hex::encode(recipient),
            &"ab".repeat(32),
            vec![0x01],
            WelcomeKind::Dm,
        ),
    )
    .await
    .expect_err("a stranger's DM Welcome must not deliver under allow_knock");
    assert_eq!(err.code, FORBIDDEN);
}

#[tokio::test]
async fn deliver_group_from_a_stranger_is_refused_under_the_default_mode() {
    // A group Welcome is gated identically — otherwise the mode is evaded by
    // minting a 3-member group (the one-extra-Welcome evasion the anti-spam
    // section records).
    let (router, state) = router_with_db_only().await;
    let sender = [0xA3u8; 32];
    let recipient = [0xA4u8; 32];

    let err = dispatch(
        &router,
        state,
        sender,
        "fauna.conversations.welcome.deliver",
        deliver_payload(
            &hex::encode(recipient),
            &"cd".repeat(32),
            vec![0x02],
            WelcomeKind::Group {
                group_id: "ef".repeat(32),
            },
        ),
    )
    .await
    .expect_err("a stranger's group Welcome must not deliver under allow_knock");
    assert_eq!(err.code, FORBIDDEN);
}

#[tokio::test]
async fn deliver_dm_from_a_stranger_flows_on_open_and_is_refused_on_stricter_modes() {
    let (router, state) = router_with_db_only().await;
    let channel = "ab".repeat(32);
    for (i, (mode, delivers)) in [
        ("open", true),
        ("allow_knock", false),
        ("contacts_only", false),
        ("closed", false),
    ]
    .into_iter()
    .enumerate()
    {
        let sender = [0xB0u8 + i as u8; 32];
        let recipient = [0xC0u8 + i as u8; 32];
        state.db.set_inbox_mode(&recipient, mode).await.unwrap();
        let got = dispatch(
            &router,
            state.clone(),
            sender,
            "fauna.conversations.welcome.deliver",
            deliver_payload(
                &hex::encode(recipient),
                &channel,
                vec![0x03],
                WelcomeKind::Dm,
            ),
        )
        .await;
        match (delivers, got) {
            (true, Ok(_)) => {}
            (false, Err(e)) => assert_eq!(e.code, FORBIDDEN, "mode {mode}"),
            (true, Err(e)) => panic!("mode {mode} must deliver, got {e:?}"),
            (false, Ok(_)) => panic!("mode {mode} must refuse a stranger"),
        }
    }
}

#[tokio::test]
async fn deliver_dm_flows_for_an_accepted_contact_under_every_mode() {
    // A mode acts on NEW parties only — an accepted/confirmed contact keeps
    // flowing even under `closed` (the same rule the floor applies).
    let (router, state) = router_with_db_only().await;
    for (i, mode) in ["open", "allow_knock", "contacts_only", "closed"]
        .into_iter()
        .enumerate()
    {
        let sender = [0xD0u8 + i as u8; 32];
        let recipient = [0xE0u8 + i as u8; 32];
        state.db.set_inbox_mode(&recipient, mode).await.unwrap();
        state
            .db
            .upsert_contact(&recipient, &sender, "accepted")
            .await
            .unwrap();
        dispatch(
            &router,
            state.clone(),
            sender,
            "fauna.conversations.welcome.deliver",
            deliver_payload(
                &hex::encode(recipient),
                &"ab".repeat(32),
                vec![0x04],
                WelcomeKind::Dm,
            ),
        )
        .await
        .unwrap_or_else(|e| panic!("accepted contact must flow under {mode}: {e:?}"));
    }
}

#[tokio::test]
async fn deliver_folder_share_by_the_channels_claimant_is_not_mode_gated() {
    // The recipient-side pending-share gate stages a stranger's share
    // (folders.md § Sharing), so the mode gate must not pre-empt it. What
    // makes a delivery a share is the channel's folder claim — nest state,
    // written by `folders.share` before the client's Welcome — never the
    // `Folder` label on the wire.
    let (router, state) = router_with_db_only().await;
    let sharer = [0xF0u8; 32];
    let recipient = [0xF8u8; 32];
    let channel = [0x0Au8; 32];

    state
        .db
        .claim_folder_channel(&sharer, &channel)
        .await
        .unwrap();
    // Even under the strictest mode: a genuine share routes past it.
    state.db.set_inbox_mode(&recipient, "closed").await.unwrap();

    dispatch(
        &router,
        state.clone(),
        sharer,
        "fauna.conversations.welcome.deliver",
        deliver_payload(
            &hex::encode(recipient),
            &hex::encode(channel),
            vec![0x05],
            WelcomeKind::Folder {
                group_id: "aa".repeat(32),
            },
        ),
    )
    .await
    .expect("the channel's claimant shares past `closed`");
}

/// The exemptions are NEST STATE, so a label alone buys nothing: a
/// same-nest stranger who tags its own Welcome `Scheduling` (the CalDAV
/// gateway's kind) or `Folder` on a channel of its choosing is gated exactly
/// like a `Dm`. `direct-messages.md` § Reach policy → § Scope.
#[tokio::test]
async fn a_mode_exempt_label_buys_no_exemption_on_the_user_rail() {
    let (router, state) = router_with_db_only().await;
    for (i, kind) in [
        WelcomeKind::Scheduling,
        WelcomeKind::Folder {
            group_id: "aa".repeat(32),
        },
    ]
    .into_iter()
    .enumerate()
    {
        let stranger = [0x10u8 + i as u8; 32];
        let recipient = [0x18u8 + i as u8; 32];
        // A channel of the STRANGER's choosing — this nest holds no folder
        // claim on it, so nest state exempts nothing.
        let channel = [0x20u8 + i as u8; 32];
        state.db.set_inbox_mode(&recipient, "closed").await.unwrap();

        let err = match dispatch(
            &router,
            state.clone(),
            stranger,
            "fauna.conversations.welcome.deliver",
            deliver_payload(
                &hex::encode(recipient),
                &hex::encode(channel),
                vec![0x05],
                kind.clone(),
            ),
        )
        .await
        {
            Ok(_) => panic!("{kind:?} from a stranger must not skip `closed`"),
            Err(e) => e,
        };
        assert_eq!(err.code, FORBIDDEN, "{kind:?}");
        assert!(
            state
                .db
                .list_actor_channels(&recipient)
                .await
                .unwrap()
                .is_empty(),
            "{kind:?}: a refused Welcome must buy no roster seat"
        );
    }
}

/// The two-step the probe walked, for BOTH mode-exempt kinds: step
/// 1 labels a Welcome `Scheduling` (or `Folder`) to take the roster seat its
/// delivery registers, and step 2 sends a `Dm` on that same channel, which the
/// seat makes read as in-band traffic — `closed` defeated in two calls. Step 1
/// is refused now, so step 2 is still an initiation.
///
/// Both arms are WALKED rather than composed from the single-step refusal
/// [`a_mode_exempt_label_buys_no_exemption_on_the_user_rail`] proves: step 2's verdict turns on the seat step 1 would have bought, so only
/// the pair run end to end shows the chain is cut.
#[tokio::test]
async fn the_seat_then_dm_two_step_is_refused_under_closed() {
    let (router, state) = router_with_db_only().await;
    for (i, kind) in [
        WelcomeKind::Scheduling,
        WelcomeKind::Folder {
            group_id: "aa".repeat(32),
        },
    ]
    .into_iter()
    .enumerate()
    {
        let offset = (i as u8) * 0x10;
        let stranger = [0x21u8 + offset; 32];
        let recipient = [0x22u8 + offset; 32];
        // A channel of the STRANGER's choosing — this nest holds no folder
        // claim on it, so nest state exempts neither label.
        let channel = [0x23u8 + offset; 32];
        let channel_hex = hex::encode(channel);
        state.db.set_inbox_mode(&recipient, "closed").await.unwrap();

        let err = dispatch(
            &router,
            state.clone(),
            stranger,
            "fauna.conversations.welcome.deliver",
            deliver_payload(
                &hex::encode(recipient),
                &channel_hex,
                vec![0x06],
                kind.clone(),
            ),
        )
        .await
        .expect_err("step 1: a mode-exempt LABEL must not seat the recipient");
        assert_eq!(err.code, FORBIDDEN, "{kind:?} step 1");
        assert!(
            !state
                .db
                .is_actor_in_channel(&recipient, &channel)
                .await
                .unwrap(),
            "{kind:?}: step 1 bought no roster seat — the fact step 2 turns on"
        );

        let err = dispatch(
            &router,
            state.clone(),
            stranger,
            "fauna.conversations.welcome.deliver",
            deliver_payload(
                &hex::encode(recipient),
                &channel_hex,
                vec![0x07],
                WelcomeKind::Dm,
            ),
        )
        .await
        .expect_err("step 2: with no seat bought, the DM is still an initiation");
        assert_eq!(err.code, FORBIDDEN, "{kind:?} step 2");

        assert!(
            state
                .db
                .list_inbox_all(&recipient)
                .await
                .unwrap()
                .is_empty(),
            "{kind:?}: a `closed` recipient took no inbox row from either step"
        );
    }
}

/// The chain's surviving arm. A stranger's GENUINE folder share
/// does reach a `closed` recipient (§ Scope's ratified exemption) and does
/// seat them on the claimed channel — so the second step has to be denied by
/// the PLANE rather than the reach gate: every Welcome onto a folder-claimed
/// channel is a folder delivery, whatever label it carries, and no chat thread
/// is ever born from that seat.
#[tokio::test]
async fn a_folder_seat_cannot_be_relabelled_into_a_chat_thread() {
    let (router, state) = router_with_db_only().await;
    let sharer = [0x31u8; 32];
    let recipient = [0x32u8; 32];
    let channel = [0x33u8; 32];
    let channel_hex = hex::encode(channel);

    state.db.set_inbox_mode(&recipient, "closed").await.unwrap();
    state
        .db
        .claim_folder_channel(&sharer, &channel)
        .await
        .unwrap();

    dispatch(
        &router,
        state.clone(),
        sharer,
        "fauna.conversations.welcome.deliver",
        deliver_payload(
            &hex::encode(recipient),
            &channel_hex,
            vec![0x08],
            WelcomeKind::Folder {
                group_id: "aa".repeat(32),
            },
        ),
    )
    .await
    .expect("the share itself flows — § Scope's ratified exemption");
    assert!(
        state
            .db
            .is_actor_in_channel(&recipient, &channel)
            .await
            .unwrap(),
        "the share seats the recipient on the claimed channel"
    );

    // Now the relabel. It is in-band on the claimant's own channel, so it is
    // not refused — but it lands on the folder plane the claim defines.
    dispatch(
        &router,
        state.clone(),
        sharer,
        "fauna.conversations.welcome.deliver",
        deliver_payload(
            &hex::encode(recipient),
            &channel_hex,
            vec![0x09],
            WelcomeKind::Dm,
        ),
    )
    .await
    .expect("in-band on the claimed channel");

    let rows = state.db.list_inbox_all(&recipient).await.unwrap();
    assert_eq!(rows.len(), 2, "both welcomes landed");
    for (i, row) in rows.iter().enumerate() {
        let w = InboxEnvelope::from_canonical_bytes(&row.1)
            .expect("inbox envelope decodes")
            .decode_welcome()
            .expect("welcome inbox decodes");
        assert_eq!(
            w.channel_type.as_deref(),
            Some("folder"),
            "row {i}: a folder-claimed channel's Welcome is a folder Welcome, \
             so `closed` never sees a chat thread"
        );
    }
}

#[tokio::test]
async fn deliver_dm_re_welcome_to_an_established_recipient_is_not_mode_gated() {
    // A recipient already on the channel is being re-Welcomed (idempotent
    // retry / re-add) — in-band traffic, which must keep flowing whatever the
    // mode says (initiation vs. in-band is decided from THIS nest's roster).
    let (router, state) = router_with_db_only().await;
    let sender = [0x71u8; 32];
    let recipient = [0x72u8; 32];
    let channel = "ab".repeat(32);
    // First deliver flows as an accepted contact…
    state
        .db
        .upsert_contact(&recipient, &sender, "accepted")
        .await
        .unwrap();
    dispatch(
        &router,
        state.clone(),
        sender,
        "fauna.conversations.welcome.deliver",
        deliver_payload(
            &hex::encode(recipient),
            &channel,
            vec![0x06],
            WelcomeKind::Dm,
        ),
    )
    .await
    .expect("first deliver flows (accepted contact)");
    // …then the contact expires/downgrades AND the mode closes; the re-Welcome
    // to the now-established recipient still flows.
    state.db.delete_contact(&recipient, &sender).await.unwrap();
    state.db.set_inbox_mode(&recipient, "closed").await.unwrap();
    dispatch(
        &router,
        state.clone(),
        sender,
        "fauna.conversations.welcome.deliver",
        deliver_payload(
            &hex::encode(recipient),
            &channel,
            vec![0x07],
            WelcomeKind::Dm,
        ),
    )
    .await
    .expect("re-Welcome to an on-channel recipient is in-band, not initiation");
}

// ── malformed inputs ──────────────────────────────────────────

#[tokio::test]
async fn deliver_rejects_malformed_recipient_actor_id() {
    let (router, state) = router_with_db_only().await;
    let sender = [91u8; 32];
    let channel_id_hex = "aa".repeat(32);

    let err = dispatch(
        &router,
        state,
        sender,
        "fauna.conversations.welcome.deliver",
        deliver_payload("not-hex", &channel_id_hex, vec![0x01], WelcomeKind::Dm),
    )
    .await
    .expect_err("malformed recipient_actor_id rejected");
    assert_eq!(err.code, "fauna.conversations.invalid_params");
}

#[tokio::test]
async fn deliver_rejects_malformed_channel_id() {
    let (router, state) = router_with_db_only().await;
    let sender = [92u8; 32];
    let recipient = [93u8; 32];

    let err = dispatch(
        &router,
        state,
        sender,
        "fauna.conversations.welcome.deliver",
        deliver_payload(
            &hex::encode(recipient),
            "not-hex",
            vec![0x01],
            WelcomeKind::Dm,
        ),
    )
    .await
    .expect_err("malformed channel_id rejected");
    assert_eq!(err.code, "fauna.conversations.invalid_params");
}

// ── caller-class gate ─────────────────────────────────────────

#[tokio::test]
async fn deliver_denies_bridges_at_allowlist_layer() {
    // Sanity-check the allowlist mapping — the bridges must be denied.
    // The handler-layer wire-up uses `require_permission`, which calls
    // `is_permitted`; testing the mapping here pins it alongside the
    // handler test so a regression in either lands as one diff to read.
    //
    // `User` is permitted, and `Admin` inherits it: the blanket
    // `Admin ⊇ User` rule (`bridge_method_allowlist::is_permitted`) grants an admin every User-class permission, because
    // the admin who claims the nest is also its first Fauna user and
    // sends DMs through the same client UI. Only the bridges are denied.
    // Mirrors the in-crate twin `conversations_welcome_deliver_bridges_denied`.
    for class in [CallerClass::User, CallerClass::Admin] {
        assert!(
            is_permitted(class, "fauna.conversations.welcome.deliver"),
            "welcome.deliver should be permitted for {class:?}"
        );
    }
    for class in [CallerClass::BridgeMta, CallerClass::BridgeMda] {
        assert!(
            !is_permitted(class, "fauna.conversations.welcome.deliver"),
            "welcome.deliver should be denied for {class:?}"
        );
    }
}
