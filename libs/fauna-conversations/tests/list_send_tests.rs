//! Sending to one of the account's own mailing lists from the conversations
//! compose (`docs/goal/behavior/mail-mass-mailing.md` § Composing a list
//! message): the send goes out through the list fan-out, never as plain mail
//! to the list address, and the compose carries the list-send view.

use async_trait::async_trait;
use fauna_conversations::backend::OutboundMailSink;
use fauna_conversations::backends::smtp::SmtpBackend;
use fauna_conversations::list_send::{
    ListSendProgress, OwnMailList, OwnMailLists, QUOTA_APPROACHING_KEY, SEND_PROGRESS_KEY,
    SEND_WARNING_KEY,
};
use fauna_conversations::*;
use std::sync::{Arc, Mutex};

const LIST_ID_HEX: &str = "a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1";

#[derive(Default)]
struct ListSink {
    plain: Mutex<Vec<Vec<String>>>,
    to_list: Mutex<Vec<(String, Vec<u8>)>>,
    /// Recipients already used today, before any send here.
    used_before: u64,
}

impl ListSink {
    fn sent_to_list(&self) -> u64 {
        self.to_list.lock().unwrap().len() as u64
    }
}

#[async_trait]
impl OutboundMailSink for ListSink {
    async fn submit(&self, recipients: Vec<String>, _raw: Vec<u8>) -> Result<(), String> {
        self.plain.lock().unwrap().push(recipients);
        Ok(())
    }

    async fn own_lists(&self) -> Result<OwnMailLists, String> {
        Ok(OwnMailLists {
            lists: vec![OwnMailList {
                list_id_hex: LIST_ID_HEX.into(),
                address: "news@localhost".into(),
                name: "Weekly news".into(),
                member_count: 3,
            }],
            recipients_today: self.used_before + 3 * self.sent_to_list(),
            recipients_per_day: 100,
        })
    }

    async fn submit_to_list(&self, list_id_hex: &str, raw: Vec<u8>) -> Result<(), String> {
        self.to_list
            .lock()
            .unwrap()
            .push((list_id_hex.to_string(), raw));
        Ok(())
    }

    async fn latest_list_send(
        &self,
        _list_id_hex: &str,
    ) -> Result<Option<ListSendProgress>, String> {
        Ok((self.sent_to_list() > 0).then_some(ListSendProgress {
            recipient_count: 3,
            delivered_count: 3,
        }))
    }
}

async fn compose_to(m: &ConversationsManager, address: &str) {
    m.start_new_conversation();
    m.set_new_thread_recipient_input(address.into());
    m.resolve_recipient().await;
    assert!(m.accept_current_recipient_chip(), "chip should be accepted");
}

#[tokio::test]
async fn a_compose_to_an_own_list_warns_then_sends_through_the_list_fan_out() {
    let sink = Arc::new(ListSink::default());
    let m = ConversationsManager::new();
    m.register_backend(Arc::new(SmtpBackend::new(sink.clone(), "alice@localhost")));

    compose_to(&m, "news@localhost").await;
    m.refresh_list_send(None).await;
    let compose = m.snapshot().new_thread_compose.expect("composer open");
    let view = compose
        .list_send
        .expect("a list recipient shows the warning");
    assert_eq!(view.send_warning.key, SEND_WARNING_KEY);
    assert_eq!(view.send_warning.args["count"], "3");
    assert_eq!(view.send_warning.args["used"], "0");
    assert_eq!(view.send_warning.args["limit"], "100");
    assert_eq!(view.progress, None, "nothing sent yet");

    m.set_new_thread_subject(Some("Issue 1".into()));
    m.set_new_thread_body("Hello, readers".into());
    let id = m
        .send_new_thread()
        .await
        .expect("send ok")
        .expect("a thread id");

    assert!(
        sink.plain.lock().unwrap().is_empty(),
        "a list is never sent plain mail"
    );
    let to_list = sink.to_list.lock().unwrap().clone();
    assert_eq!(to_list.len(), 1, "one list send, not one per member");
    assert_eq!(to_list[0].0, LIST_ID_HEX);
    let raw = String::from_utf8(to_list[0].1.clone()).unwrap();
    assert!(raw.contains("From: alice@localhost\r\n"), "{raw:?}");
    assert!(raw.contains("Subject: Issue 1\r\n"));

    let detail = m.thread_detail(id).expect("thread exists");
    assert_eq!(
        detail.messages.len(),
        1,
        "one Sent entry for the whole send"
    );
    let view = detail
        .compose
        .list_send
        .expect("the thread's compose keeps the list view");
    let progress = view.progress.expect("the send shows as one progress");
    assert_eq!(progress.key, SEND_PROGRESS_KEY);
    assert_eq!(progress.args["delivered"], "3");
    assert_eq!(view.send_warning.args["used"], "3", "the meter moved");
}

#[tokio::test]
async fn close_to_the_daily_cap_the_compose_warns_how_many_are_left() {
    let sink = Arc::new(ListSink {
        used_before: 92,
        ..Default::default()
    });
    let m = ConversationsManager::new();
    m.register_backend(Arc::new(SmtpBackend::new(sink.clone(), "alice@localhost")));

    compose_to(&m, "news@localhost").await;
    m.refresh_list_send(None).await;
    let view = m
        .snapshot()
        .new_thread_compose
        .and_then(|c| c.list_send)
        .expect("list view");
    let warning = view.quota_warning.expect("8 left of 100");
    assert_eq!(warning.key, QUOTA_APPROACHING_KEY);
    assert_eq!(warning.args["remaining"], "8");
}

#[tokio::test]
async fn a_compose_to_anyone_else_has_no_list_view_and_sends_plain_mail() {
    let sink = Arc::new(ListSink::default());
    let m = ConversationsManager::new();
    m.register_backend(Arc::new(SmtpBackend::new(sink.clone(), "alice@localhost")));

    compose_to(&m, "bob@external.test").await;
    m.refresh_list_send(None).await;
    assert!(
        m.snapshot()
            .new_thread_compose
            .expect("composer open")
            .list_send
            .is_none()
    );
    m.set_new_thread_body("hi".into());
    m.send_new_thread().await.expect("send ok");
    assert_eq!(sink.plain.lock().unwrap().len(), 1);
    assert_eq!(sink.sent_to_list(), 0);
}
