//! Integration round-trip for `fauna.email.filters.*` (Layer-3 mail-
//! server adjacent user-facing surface). Mirrors the
//! `conformance_bridges_ui.rs` § feeds.* harness — the email-filter
//! handlers don't need a BridgeProvider registry because they reach
//! `CacheDb` directly through `state.db.{list,create,get,update,
//! delete}_email_filter`.
//!
//! Authority for the wire types: `libs/fauna-protocol/src/email.rs`.
//! Authority for the slice + namespace decision: tracked internally
//! (§ T6).

mod common;
use common::dispatch;

use std::sync::Arc;

use bytes::Bytes;

use fauna_nest::{db::CacheDb, email_handlers, routes::AppState, rpc_router::RpcRouter};
use fauna_protocol::{
    decode_strict as decode,
    email::{
        CreateEmailFilterReply, CreateEmailFilterRequest, DeleteEmailFilterReply,
        DeleteEmailFilterRequest, EmailFilterAction, EmailFilterRule, GetEmailFilterReply,
        GetEmailFilterRequest, ListEmailFiltersReply, ListEmailFiltersRequest,
        UpdateEmailFilterReply, UpdateEmailFilterRequest,
    },
    encode_canonical,
};

async fn router_with_db_only() -> (RpcRouter, Arc<AppState>) {
    let db = Arc::new(CacheDb::open_in_memory().unwrap());
    let state = Arc::new(AppState::for_test(db));
    let mut b = RpcRouter::builder();
    email_handlers::register_email_handlers(&mut b);
    (b.build(), state)
}

fn create_payload(name: &str, action: EmailFilterAction) -> Bytes {
    let req = CreateEmailFilterRequest {
        extra: Default::default(),
        name: name.into(),
        rules: vec![EmailFilterRule::SenderDomain {
            domain: "example.com".into(),
        }],
        combination: "all".into(),
        action,
        priority: 0,
        continue_on_match: false,
    };
    Bytes::from(encode_canonical(&req).unwrap().to_vec())
}

// ── fauna.email.filters.list ───────────────────────────────────

#[tokio::test]
async fn list_empty_when_no_filters() {
    let (router, state) = router_with_db_only().await;
    let payload = Bytes::from(
        encode_canonical(&ListEmailFiltersRequest {})
            .unwrap()
            .to_vec(),
    );
    let reply_bytes = dispatch(
        &router,
        state,
        [1u8; 32],
        "fauna.email.filters.list",
        payload,
    )
    .await
    .expect("list ok");
    let reply: ListEmailFiltersReply = decode(&reply_bytes).unwrap();
    assert!(reply.filters.is_empty());
}

#[tokio::test]
async fn list_isolates_per_actor() {
    let (router, state) = router_with_db_only().await;
    let actor_a = [2u8; 32];
    let actor_b = [3u8; 32];

    for actor in [actor_a, actor_b] {
        let name = format!("filter for actor {}", actor[0]);
        let payload = create_payload(
            &name,
            EmailFilterAction::AddLabel {
                label: format!("L{}", actor[0]),
            },
        );
        dispatch(
            &router,
            state.clone(),
            actor,
            "fauna.email.filters.create",
            payload,
        )
        .await
        .unwrap();
    }

    let payload = Bytes::from(
        encode_canonical(&ListEmailFiltersRequest {})
            .unwrap()
            .to_vec(),
    );
    let reply_bytes = dispatch(&router, state, actor_a, "fauna.email.filters.list", payload)
        .await
        .unwrap();
    let reply: ListEmailFiltersReply = decode(&reply_bytes).unwrap();
    assert_eq!(reply.filters.len(), 1, "only own row visible");
    assert_eq!(reply.filters[0].name, "filter for actor 2");
}

// ── fauna.email.filters.create ─────────────────────────────────

#[tokio::test]
async fn create_then_list_round_trips_all_action_variants() {
    // Catches divergence between the wire enum + the on-disk string
    // shape (`action_to_string` / `action_from_string`). Each variant
    // gets its own row, then a single list read decodes them all and
    // we assert the action variant survives.
    let (router, state) = router_with_db_only().await;
    let actor = [10u8; 32];

    let actions = [
        EmailFilterAction::Allow,
        EmailFilterAction::Discard,
        EmailFilterAction::Reject {
            reason: "blocked".into(),
        },
        EmailFilterAction::FileInto {
            mailbox: "Archive".into(),
        },
        EmailFilterAction::Forward {
            address: "bob@example.com".into(),
            redirect: false,
        },
        // The redirect copy mode rides the additive `forward_redirect`
        // column beside the `forward:<address>` string — both forms must
        // survive the create → list round trip.
        EmailFilterAction::Forward {
            address: "bob-redirect@example.com".into(),
            redirect: true,
        },
        EmailFilterAction::AutoReply {
            subject: "Out of office".into(),
            body: "Back\nMonday.".into(),
            interval_hours: 12,
        },
        EmailFilterAction::AddLabel {
            label: "important".into(),
        },
    ];
    for (i, action) in actions.iter().cloned().enumerate() {
        let payload = create_payload(&format!("f{i}"), action);
        let reply_bytes = dispatch(
            &router,
            state.clone(),
            actor,
            "fauna.email.filters.create",
            payload,
        )
        .await
        .expect("create ok");
        let r: CreateEmailFilterReply = decode(&reply_bytes).unwrap();
        assert!(r.id > 0);
    }

    let payload = Bytes::from(
        encode_canonical(&ListEmailFiltersRequest {})
            .unwrap()
            .to_vec(),
    );
    let reply_bytes = dispatch(&router, state, actor, "fauna.email.filters.list", payload)
        .await
        .unwrap();
    let reply: ListEmailFiltersReply = decode(&reply_bytes).unwrap();
    assert_eq!(reply.filters.len(), actions.len());
    // The DB orders by priority ASC then id ASC; all rows share
    // priority=0, so insert order is preserved through id.
    for (i, action) in actions.iter().enumerate() {
        assert_eq!(&reply.filters[i].action, action, "row {i} action mismatch");
        assert_eq!(reply.filters[i].name, format!("f{i}"));
        assert_eq!(reply.filters[i].combination, "all");
    }
}

#[tokio::test]
async fn create_rejects_empty_name() {
    let (router, state) = router_with_db_only().await;
    let req = CreateEmailFilterRequest {
        extra: Default::default(),
        name: "".into(),
        rules: Vec::new(),
        combination: "all".into(),
        action: EmailFilterAction::Discard,
        priority: 0,
        continue_on_match: false,
    };
    let payload = Bytes::from(encode_canonical(&req).unwrap().to_vec());
    let err = dispatch(
        &router,
        state,
        [20u8; 32],
        "fauna.email.filters.create",
        payload,
    )
    .await
    .expect_err("empty name rejected");
    assert_eq!(err.code, "fauna.email.invalid_params");
}

#[tokio::test]
async fn create_rejects_unknown_combination() {
    let (router, state) = router_with_db_only().await;
    let req = CreateEmailFilterRequest {
        extra: Default::default(),
        name: "x".into(),
        rules: Vec::new(),
        // Neither "all" nor "any" — the HTTP twin rejects this too.
        combination: "either".into(),
        action: EmailFilterAction::Discard,
        priority: 0,
        continue_on_match: false,
    };
    let payload = Bytes::from(encode_canonical(&req).unwrap().to_vec());
    let err = dispatch(
        &router,
        state,
        [21u8; 32],
        "fauna.email.filters.create",
        payload,
    )
    .await
    .expect_err("bad combination rejected");
    assert_eq!(err.code, "fauna.email.invalid_params");
}

// ── fauna.email.filters.get ────────────────────────────────────

#[tokio::test]
async fn get_returns_filter_for_owner() {
    let (router, state) = router_with_db_only().await;
    let actor = [30u8; 32];

    let payload = create_payload(
        "my-filter",
        EmailFilterAction::FileInto {
            mailbox: "Reading".into(),
        },
    );
    let reply_bytes = dispatch(
        &router,
        state.clone(),
        actor,
        "fauna.email.filters.create",
        payload,
    )
    .await
    .unwrap();
    let CreateEmailFilterReply { id, .. } = decode(&reply_bytes).unwrap();

    let payload = Bytes::from(
        encode_canonical(&GetEmailFilterRequest {
            extra: Default::default(),
            id,
        })
        .unwrap()
        .to_vec(),
    );
    let reply_bytes = dispatch(&router, state, actor, "fauna.email.filters.get", payload)
        .await
        .expect("get ok");
    let reply: GetEmailFilterReply = decode(&reply_bytes).unwrap();
    assert_eq!(reply.filter.id, id);
    assert_eq!(reply.filter.name, "my-filter");
    assert_eq!(
        reply.filter.action,
        EmailFilterAction::FileInto {
            mailbox: "Reading".into()
        },
    );
    assert_eq!(reply.filter.rules.len(), 1);
    assert!(reply.filter.created_at > 0);
}

#[tokio::test]
async fn get_unknown_id_returns_not_found() {
    let (router, state) = router_with_db_only().await;
    let payload = Bytes::from(
        encode_canonical(&GetEmailFilterRequest {
            extra: Default::default(),
            id: 9999,
        })
        .unwrap()
        .to_vec(),
    );
    let err = dispatch(
        &router,
        state,
        [31u8; 32],
        "fauna.email.filters.get",
        payload,
    )
    .await
    .expect_err("unknown id rejected");
    assert_eq!(err.code, "fauna.email.not_found");
}

#[tokio::test]
async fn get_other_actors_row_returns_not_found() {
    // Per-actor isolation: actor A can't read actor B's filter row.
    // The wrong-actor path collapses to the same not_found shape as
    // the unknown-id path.
    let (router, state) = router_with_db_only().await;
    let owner = [40u8; 32];
    let intruder = [41u8; 32];

    let payload = create_payload("private", EmailFilterAction::Discard);
    let reply_bytes = dispatch(
        &router,
        state.clone(),
        owner,
        "fauna.email.filters.create",
        payload,
    )
    .await
    .unwrap();
    let CreateEmailFilterReply { id, .. } = decode(&reply_bytes).unwrap();

    let payload = Bytes::from(
        encode_canonical(&GetEmailFilterRequest {
            extra: Default::default(),
            id,
        })
        .unwrap()
        .to_vec(),
    );
    let err = dispatch(
        &router,
        state.clone(),
        intruder,
        "fauna.email.filters.get",
        payload,
    )
    .await
    .expect_err("intruder rejected");
    assert_eq!(err.code, "fauna.email.not_found");

    // The owner can still read their own row.
    let payload = Bytes::from(
        encode_canonical(&GetEmailFilterRequest {
            extra: Default::default(),
            id,
        })
        .unwrap()
        .to_vec(),
    );
    let reply_bytes = dispatch(&router, state, owner, "fauna.email.filters.get", payload)
        .await
        .expect("owner can still read");
    let reply: GetEmailFilterReply = decode(&reply_bytes).unwrap();
    assert_eq!(reply.filter.id, id);
}

// ── fauna.email.filters.update ─────────────────────────────────

#[tokio::test]
async fn update_changes_fields() {
    let (router, state) = router_with_db_only().await;
    let actor = [50u8; 32];

    let payload = create_payload("old-name", EmailFilterAction::Discard);
    let reply_bytes = dispatch(
        &router,
        state.clone(),
        actor,
        "fauna.email.filters.create",
        payload,
    )
    .await
    .unwrap();
    let CreateEmailFilterReply { id, .. } = decode(&reply_bytes).unwrap();

    let upd = UpdateEmailFilterRequest {
        extra: Default::default(),
        id,
        name: "new-name".into(),
        rules: vec![EmailFilterRule::SubjectContains {
            text: "newsletter".into(),
        }],
        combination: "any".into(),
        action: EmailFilterAction::AddLabel {
            label: "News".into(),
        },
        priority: 7,
        continue_on_match: false,
    };
    let payload = Bytes::from(encode_canonical(&upd).unwrap().to_vec());
    let reply_bytes = dispatch(
        &router,
        state.clone(),
        actor,
        "fauna.email.filters.update",
        payload,
    )
    .await
    .expect("update ok");
    let reply: UpdateEmailFilterReply = decode(&reply_bytes).unwrap();
    assert!(reply.ok);

    let payload = Bytes::from(
        encode_canonical(&GetEmailFilterRequest {
            extra: Default::default(),
            id,
        })
        .unwrap()
        .to_vec(),
    );
    let reply_bytes = dispatch(&router, state, actor, "fauna.email.filters.get", payload)
        .await
        .unwrap();
    let reply: GetEmailFilterReply = decode(&reply_bytes).unwrap();
    assert_eq!(reply.filter.name, "new-name");
    assert_eq!(reply.filter.combination, "any");
    assert_eq!(reply.filter.priority, 7);
    assert_eq!(
        reply.filter.action,
        EmailFilterAction::AddLabel {
            label: "News".into()
        },
    );
    assert_eq!(reply.filter.rules.len(), 1);
    match &reply.filter.rules[0] {
        EmailFilterRule::SubjectContains { text } => assert_eq!(text, "newsletter"),
        other => panic!("unexpected rule {other:?}"),
    }
}

#[tokio::test]
async fn update_unknown_id_returns_not_found() {
    let (router, state) = router_with_db_only().await;
    let upd = UpdateEmailFilterRequest {
        extra: Default::default(),
        id: 9999,
        name: "x".into(),
        rules: Vec::new(),
        combination: "all".into(),
        action: EmailFilterAction::Discard,
        priority: 0,
        continue_on_match: false,
    };
    let payload = Bytes::from(encode_canonical(&upd).unwrap().to_vec());
    let err = dispatch(
        &router,
        state,
        [51u8; 32],
        "fauna.email.filters.update",
        payload,
    )
    .await
    .expect_err("unknown id rejected");
    assert_eq!(err.code, "fauna.email.not_found");
}

#[tokio::test]
async fn update_other_actors_row_returns_not_found() {
    let (router, state) = router_with_db_only().await;
    let owner = [60u8; 32];
    let intruder = [61u8; 32];

    let payload = create_payload("private", EmailFilterAction::Discard);
    let reply_bytes = dispatch(
        &router,
        state.clone(),
        owner,
        "fauna.email.filters.create",
        payload,
    )
    .await
    .unwrap();
    let CreateEmailFilterReply { id, .. } = decode(&reply_bytes).unwrap();

    let upd = UpdateEmailFilterRequest {
        extra: Default::default(),
        id,
        name: "hijacked".into(),
        rules: Vec::new(),
        combination: "all".into(),
        action: EmailFilterAction::Allow,
        priority: 0,
        continue_on_match: false,
    };
    let payload = Bytes::from(encode_canonical(&upd).unwrap().to_vec());
    let err = dispatch(
        &router,
        state.clone(),
        intruder,
        "fauna.email.filters.update",
        payload,
    )
    .await
    .expect_err("intruder rejected");
    assert_eq!(err.code, "fauna.email.not_found");

    // The owner's row is unchanged.
    let payload = Bytes::from(
        encode_canonical(&GetEmailFilterRequest {
            extra: Default::default(),
            id,
        })
        .unwrap()
        .to_vec(),
    );
    let reply_bytes = dispatch(&router, state, owner, "fauna.email.filters.get", payload)
        .await
        .unwrap();
    let reply: GetEmailFilterReply = decode(&reply_bytes).unwrap();
    assert_eq!(reply.filter.name, "private");
}

#[tokio::test]
async fn update_rejects_invalid_combination() {
    // Validation runs before the DB layer. Same shape as create.
    let (router, state) = router_with_db_only().await;
    let upd = UpdateEmailFilterRequest {
        extra: Default::default(),
        id: 1,
        name: "x".into(),
        rules: Vec::new(),
        combination: "neither".into(),
        action: EmailFilterAction::Discard,
        priority: 0,
        continue_on_match: false,
    };
    let payload = Bytes::from(encode_canonical(&upd).unwrap().to_vec());
    let err = dispatch(
        &router,
        state,
        [62u8; 32],
        "fauna.email.filters.update",
        payload,
    )
    .await
    .expect_err("bad combination rejected");
    assert_eq!(err.code, "fauna.email.invalid_params");
}

// ── FileInto placement targets ─────────────────────────────────

#[tokio::test]
async fn create_refuses_file_into_a_mailbox_inbound_mail_must_never_enter() {
    // `Sent` and `Drafts` hold only this account's own writing, and the
    // guardian's held mailbox holds only holds (`email-filters.md` § Email
    // filter rules). A rule filing into one is refused before it is stored.
    let (router, state) = router_with_db_only().await;
    let actor = [22u8; 32];
    for mailbox in [
        "Sent",
        "Drafts",
        fauna_nest::db::bridge_imap::GUARDIAN_HELD_MAILBOX,
    ] {
        let payload = create_payload(
            "divert",
            EmailFilterAction::FileInto {
                mailbox: mailbox.into(),
            },
        );
        let err = dispatch(
            &router,
            state.clone(),
            actor,
            "fauna.email.filters.create",
            payload,
        )
        .await
        .expect_err("a refused FileInto target is rejected");
        assert_eq!(
            err.code, "fauna.email.invalid_params",
            "FileInto {mailbox:?}"
        );
    }

    let payload = Bytes::from(
        encode_canonical(&ListEmailFiltersRequest {})
            .unwrap()
            .to_vec(),
    );
    let reply_bytes = dispatch(&router, state, actor, "fauna.email.filters.list", payload)
        .await
        .unwrap();
    let reply: ListEmailFiltersReply = decode(&reply_bytes).unwrap();
    assert!(reply.filters.is_empty(), "no refused rule was stored");
}

#[tokio::test]
async fn update_refuses_file_into_sent_and_leaves_the_row_unchanged() {
    // Update shares create's validation, so a stored rule cannot be
    // re-pointed at `Sent` either.
    let (router, state) = router_with_db_only().await;
    let actor = [23u8; 32];
    let reports = EmailFilterAction::FileInto {
        mailbox: "Reports".into(),
    };
    let reply_bytes = dispatch(
        &router,
        state.clone(),
        actor,
        "fauna.email.filters.create",
        create_payload("reports", reports.clone()),
    )
    .await
    .expect("FileInto a custom folder is accepted");
    let CreateEmailFilterReply { id, .. } = decode(&reply_bytes).unwrap();

    let upd = UpdateEmailFilterRequest {
        extra: Default::default(),
        id,
        name: "reports".into(),
        rules: Vec::new(),
        combination: "all".into(),
        action: EmailFilterAction::FileInto {
            mailbox: "Sent".into(),
        },
        priority: 0,
        continue_on_match: false,
    };
    let payload = Bytes::from(encode_canonical(&upd).unwrap().to_vec());
    let err = dispatch(
        &router,
        state.clone(),
        actor,
        "fauna.email.filters.update",
        payload,
    )
    .await
    .expect_err("re-pointing a rule at Sent is rejected");
    assert_eq!(err.code, "fauna.email.invalid_params");

    let payload = Bytes::from(
        encode_canonical(&GetEmailFilterRequest {
            extra: Default::default(),
            id,
        })
        .unwrap()
        .to_vec(),
    );
    let reply_bytes = dispatch(&router, state, actor, "fauna.email.filters.get", payload)
        .await
        .unwrap();
    let reply: GetEmailFilterReply = decode(&reply_bytes).unwrap();
    assert_eq!(reply.filter.action, reports);
}

// ── fauna.email.filters.delete ─────────────────────────────────

#[tokio::test]
async fn delete_removes_row() {
    let (router, state) = router_with_db_only().await;
    let actor = [70u8; 32];

    let payload = create_payload("to-delete", EmailFilterAction::Discard);
    let reply_bytes = dispatch(
        &router,
        state.clone(),
        actor,
        "fauna.email.filters.create",
        payload,
    )
    .await
    .unwrap();
    let CreateEmailFilterReply { id, .. } = decode(&reply_bytes).unwrap();

    let payload = Bytes::from(
        encode_canonical(&DeleteEmailFilterRequest {
            extra: Default::default(),
            id,
        })
        .unwrap()
        .to_vec(),
    );
    let reply_bytes = dispatch(
        &router,
        state.clone(),
        actor,
        "fauna.email.filters.delete",
        payload,
    )
    .await
    .expect("delete ok");
    let reply: DeleteEmailFilterReply = decode(&reply_bytes).unwrap();
    assert!(reply.ok);

    let payload = Bytes::from(
        encode_canonical(&ListEmailFiltersRequest {})
            .unwrap()
            .to_vec(),
    );
    let reply_bytes = dispatch(&router, state, actor, "fauna.email.filters.list", payload)
        .await
        .unwrap();
    let reply: ListEmailFiltersReply = decode(&reply_bytes).unwrap();
    assert!(reply.filters.is_empty());
}

#[tokio::test]
async fn delete_unknown_id_returns_not_found() {
    let (router, state) = router_with_db_only().await;
    let payload = Bytes::from(
        encode_canonical(&DeleteEmailFilterRequest {
            extra: Default::default(),
            id: 9999,
        })
        .unwrap()
        .to_vec(),
    );
    let err = dispatch(
        &router,
        state,
        [71u8; 32],
        "fauna.email.filters.delete",
        payload,
    )
    .await
    .expect_err("unknown id rejected");
    assert_eq!(err.code, "fauna.email.not_found");
}

#[tokio::test]
async fn delete_other_actors_row_returns_not_found() {
    let (router, state) = router_with_db_only().await;
    let owner = [80u8; 32];
    let intruder = [81u8; 32];

    let payload = create_payload("private", EmailFilterAction::Discard);
    let reply_bytes = dispatch(
        &router,
        state.clone(),
        owner,
        "fauna.email.filters.create",
        payload,
    )
    .await
    .unwrap();
    let CreateEmailFilterReply { id, .. } = decode(&reply_bytes).unwrap();

    let payload = Bytes::from(
        encode_canonical(&DeleteEmailFilterRequest {
            extra: Default::default(),
            id,
        })
        .unwrap()
        .to_vec(),
    );
    let err = dispatch(
        &router,
        state.clone(),
        intruder,
        "fauna.email.filters.delete",
        payload,
    )
    .await
    .expect_err("intruder rejected");
    assert_eq!(err.code, "fauna.email.not_found");

    // Owner's row survives.
    let payload = Bytes::from(
        encode_canonical(&ListEmailFiltersRequest {})
            .unwrap()
            .to_vec(),
    );
    let reply_bytes = dispatch(&router, state, owner, "fauna.email.filters.list", payload)
        .await
        .unwrap();
    let reply: ListEmailFiltersReply = decode(&reply_bytes).unwrap();
    assert_eq!(reply.filters.len(), 1, "owner's row survived");
}
