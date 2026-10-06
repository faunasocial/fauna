//! Integration test: parse an email and verify it delivers to the inbox.
//!
//! Run with: cargo test -p fauna-nest --features email -- email_inbound

mod tests {
    use fauna_nest::db::CacheDb;

    #[tokio::test]
    async fn inbound_email_delivers_to_inbox() {
        let db = CacheDb::open_in_memory().unwrap();

        // Create a user with a handle
        let actor_id = [42u8; 32];
        db.create_user(&actor_id, "free", "alice").await.unwrap();
        db.set_handle(&actor_id, "alice").await.unwrap();

        // Raw email to parse
        let raw_email = b"From: bob@example.net\r\n\
            To: alice@fauna.example\r\n\
            Subject: Hello from email\r\n\
            Message-ID: <test123@example.net>\r\n\
            \r\n\
            Hi Alice, this is a test email.\r\n";

        // Parse it (same logic as inbound.rs)
        let parsed = mail_parser::MessageParser::default()
            .parse(raw_email.as_slice())
            .unwrap();

        let from = parsed
            .from()
            .unwrap()
            .first()
            .unwrap()
            .address()
            .unwrap()
            .to_string();
        let subject = parsed.subject().unwrap().to_string();
        let message_id = parsed.message_id().unwrap().to_string();
        let body = parsed.body_text(0).unwrap().to_string();

        assert_eq!(from, "bob@example.net");
        assert_eq!(subject, "Hello from email");
        assert_eq!(message_id, "test123@example.net");
        assert!(body.contains("Hi Alice"));

        // Build payload (same format as inbound.rs deliver_inbound)
        let payload = format!(
            "source=email\nfrom={from}\nsubject={subject}\nmessage_id={message_id}\n\n{body}"
        );

        // Resolve handle and deliver
        let resolved = db.resolve_handle("alice").await.unwrap().unwrap();
        assert_eq!(resolved, actor_id);
        db.push_inbox(&resolved, payload.as_bytes(), None)
            .await
            .unwrap();

        // Verify it's in the inbox
        let msgs = db.poll_inbox(&actor_id).await.unwrap();
        assert_eq!(msgs.len(), 1);
        let payload_str = String::from_utf8(msgs[0].1.clone()).unwrap();
        assert!(payload_str.contains("source=email"));
        assert!(payload_str.contains("bob@example.net"));
        assert!(payload_str.contains("Hello from email"));
        assert!(payload_str.contains("Hi Alice"));
    }

    #[tokio::test]
    async fn inbound_email_unknown_recipient() {
        let db = CacheDb::open_in_memory().unwrap();

        // No users registered — handle should not resolve
        assert!(db.resolve_handle("nobody").await.unwrap().is_none());
    }
}
