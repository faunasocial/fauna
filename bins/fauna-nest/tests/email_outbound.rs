//! Integration test: per-recipient outbound mail queue lifecycle.
//!
//! Run with: cargo test -p fauna-nest --features email -- email_outbound

mod tests {
    use fauna_nest::db::CacheDb;
    use fauna_nest::db::outbound::{InboundVerdictsSnapshot, NewOutbound, OutboundStatus};

    fn empty_verdicts() -> InboundVerdictsSnapshot {
        InboundVerdictsSnapshot {
            spf: String::new(),
            dmarc: String::new(),
            dmarc_policy: String::new(),
        }
    }

    #[tokio::test]
    async fn outbound_mail_queue_roundtrip() {
        let db = CacheDb::open_in_memory().unwrap();

        let raw = b"From: alice@fauna.example\r\n\
            To: bob@example.com\r\n\
            Subject: Reply\r\n\
            Message-ID: <m1@fauna.example>\r\n\
            \r\n\
            Thanks Bob!\r\n";

        let ids = db
            .enqueue_outbound(NewOutbound {
                original_msgid: "<m1@fauna.example>",
                original_sender: "alice@fauna.example",
                recipients: &["bob@example.com"],
                raw_message: raw,
                inbound_verdicts: empty_verdicts(),
                is_forwarded: false,
                forward_actor_id: None,
                forward_rule_id: None,
                forward_copy_mode: None,
                submit_actor_id: None,
            })
            .await
            .unwrap();
        assert_eq!(ids.len(), 1);

        let due = db.fetch_due_outbound(i64::MAX, 10).await.unwrap();
        assert_eq!(due.len(), 1);
        assert_eq!(due[0].id, ids[0]);
        assert_eq!(due[0].recipient, "bob@example.com");

        db.mark_outbound_sent(ids[0]).await.unwrap();

        let due = db.fetch_due_outbound(i64::MAX, 10).await.unwrap();
        assert!(due.is_empty());
    }

    #[tokio::test]
    async fn outbound_permfail_drops_row_from_due_queue() {
        let db = CacheDb::open_in_memory().unwrap();

        let ids = db
            .enqueue_outbound(NewOutbound {
                original_msgid: "<m2@example>",
                original_sender: "alice@example.com",
                recipients: &["fail@example.com"],
                raw_message: b"msg",
                inbound_verdicts: empty_verdicts(),
                is_forwarded: false,
                forward_actor_id: None,
                forward_rule_id: None,
                forward_copy_mode: None,
                submit_actor_id: None,
            })
            .await
            .unwrap();

        db.mark_outbound_permfail(ids[0], "550 nope", "5.1.1")
            .await
            .unwrap();

        let due = db.fetch_due_outbound(i64::MAX, 10).await.unwrap();
        assert!(due.is_empty(), "permfail rows are not redrained");

        let all = db.fetch_all_outbound_for_test().await.unwrap();
        assert_eq!(all.len(), 1);
        assert!(matches!(all[0].status, OutboundStatus::PermFail));
        assert_eq!(all[0].last_enhanced.as_deref(), Some("5.1.1"));
    }
}
