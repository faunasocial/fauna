//! `data.conversation_threads` — the e2e state-serialization contract for the
//! unified conversations page (`tests/e2e-unified/actions/conversations.py::
//! list_threads`). Shared home for what tui's `conversations::state_json` and
//! linux's `conversations::state::build_conversation_threads_state` used to
//! each hand-roll identically (priority #2 — same concept, same code).
//! Every app serializes through it now: tui and linux in-process, web through
//! `WasmConversationsManager::conversation_threads_json`, and windows, macOS, iOS
//! and android by re-parsing the UniFFI `conversation_threads_json` string
//! (windows: `AppDataSnapshot.GetConversationsThreadsForState`).
//!
//! Each entry shape:
//! ```text
//! { thread_id, label, snippet, rail, flavor,
//!   unread_count, participant_count, message_count,
//!   message_subject_lines: [str], channel_id_hex: str | null,
//!   participant_actor_ids: [str | null], participant_displays: [str] }
//! ```

use serde_json::{Value, json};

use crate::backends::fauna_mls::FaunaMlsBackend;
use crate::manager::ConversationsManager;

/// Build the top-level `mls_folded_commits` state value — `{channel_hex: count}`
/// — from the session's live [`FaunaMlsBackend`]. The shared derivation behind
/// `fauna_e2e_agent::MLS_FOLDED_COMMITS_KEY`, so every app's leg is a getter read
/// (priority #2) rather than its own bookkeeping.
///
/// Pre-session callers (no real conversations session yet) publish `{}` rather
/// than `null`: an app that HAS the leg but has not yet built a session is at a
/// legitimate zero for every channel, whereas `null` is reserved for an app that
/// does not publish the key at all — which the consumer refuses loudly
/// (`helpers/waiting.py::mls_folded_commits`, convention 11). Collapsing the two
/// would turn an unbuilt app leg into a silent "no fold-ins yet" and hang the
/// barrier instead of naming the gap.
pub fn mls_folded_commits_json(backend: &FaunaMlsBackend) -> Value {
    let mut map = serde_json::Map::new();
    for (channel_hex, count) in backend.folded_commits() {
        map.insert(channel_hex, json!(count));
    }
    Value::Object(map)
}

/// [`mls_folded_commits_json`] over an app's `Option<Arc<ConversationsSession>>`
/// — the whole native leg, so tui and linux each spend one line rather than
/// re-deriving the "no session yet" arm. `None` ⇒ `{}` (a legitimate zero for
/// every channel), never `null`; see [`mls_folded_commits_json`] for why that
/// distinction is load-bearing.
pub fn mls_folded_commits_json_for_session(
    session: Option<&std::sync::Arc<crate::session::ConversationsSession>>,
) -> Value {
    match session {
        Some(s) => mls_folded_commits_json(&s.backend()),
        None => Value::Object(serde_json::Map::new()),
    }
}

/// Build the top-level `conv_receive_cycles` state value —
/// `{"started": N, "completed": M, "exit": null | "closed" | "retired" |
/// "panicked"}` — from the session's live counters. The shared derivation behind
/// `fauna_e2e_agent::CONV_RECEIVE_CYCLES_KEY`, so every app's leg is a getter
/// read (priority #2) rather than its own bookkeeping.
///
/// `exit` is [`ReceiveLoopExit::as_wire_word`](crate::session::ReceiveLoopExit::as_wire_word)
/// once the loop has left, `null` while it runs (and before any session). A
/// plain atomic read, so legal on the state path (convention 11's corollary).
///
/// Pre-session callers publish `{"started": 0, "completed": 0, "exit": null}`
/// rather than `null`, for the reason [`mls_folded_commits_json`] gives in full:
/// an app that HAS the leg but no session yet is at a legitimate zero, whereas
/// `null` is reserved for an app that does not publish the key — which the
/// consumer refuses loudly rather than waiting out its budget.
pub fn conv_receive_cycles_json(
    session: Option<&std::sync::Arc<crate::session::ConversationsSession>>,
) -> Value {
    let (started, completed, exit) = match session {
        Some(s) => {
            let cycles = s.receive_cycles();
            (cycles.started(), cycles.completed(), cycles.exit())
        }
        None => (0, 0, None),
    };
    json!({
        "started": started,
        "completed": completed,
        "exit": exit.map(crate::session::ReceiveLoopExit::as_wire_word),
    })
}

/// Build the top-level `share_endpoints_counts` state value —
/// `{"seen": N, "no_sink": N, "captured": N, "uncaptured": N}` — from the
/// session's live [`FaunaMlsBackend`] tally. The shared derivation behind
/// `fauna_e2e_agent::SHARE_ENDPOINTS_COUNTS_KEY`; the counting itself is
/// `FaunaMlsBackend::share_endpoints_counts`, which existed from the ingest
/// seam's first day and was read by **tier_1 tests only** — so at tier_3 the
/// share plane's three ingest exits were mutually indistinguishable and a
/// step-3 failure could only ever say *"no dial row appeared"*.
///
/// The four numbers separate what that sentence cannot: `seen == 0` is an
/// advertisement that never crossed or never decrypted; `no_sink > 0` is a
/// plane inert on this seat; `uncaptured > 0` is an advertisement refused at
/// the bind or unwritable at the account plane; and `captured > 0` with no
/// transfer moves the hunt downstream of ingest entirely.
///
/// A pure field read of four atomics — no lock, no I/O — so convention 11's
/// no-blocking-I/O corollary holds on the ack path.
///
/// Pre-session callers publish the four zeros rather than `null`, for the
/// reason [`mls_folded_commits_json`] gives in full: an app that HAS the leg
/// but no session yet is at a legitimate zero, whereas `null` is reserved for
/// an app that does not publish the key — which the consumer refuses loudly
/// rather than reading a silent zero and spending its whole budget.
pub fn share_endpoints_counts_json(
    session: Option<&std::sync::Arc<crate::session::ConversationsSession>>,
) -> Value {
    let counts = match session {
        Some(s) => s.backend().share_endpoints_counts(),
        None => Default::default(),
    };
    json!({
        "seen": counts.seen,
        "no_sink": counts.no_sink,
        "captured": counts.captured,
        "uncaptured": counts.uncaptured,
    })
}

/// Build `data.conversation_sort` — the list's active [`crate::SortOrder`] in
/// its serde spelling (`"LatestActivity"` / `"OldestFirst"` / `"Unread"`, the
/// names `setSort` already speaks). The rows alone cannot say which order is
/// active — with no thread unread, the unread order and latest-activity render
/// identically — so a driver asserting what a `conversation-sort` tap produced
/// reads the order here and the rows off the page.
pub fn conversation_sort_json(manager: &ConversationsManager) -> Value {
    serde_json::to_value(manager.sort_order()).unwrap_or(Value::Null)
}

/// Build `data.selected_thread_id` — the thread the page has selected, `null`
/// when none (apple publishes the same key off its snapshot). A click on a
/// `conversation-item` that opened no detail pane leaves no header to read,
/// so without this a row click that selected nothing and a selection whose
/// pane failed to render read identically.
pub fn selected_thread_id_json(manager: &ConversationsManager) -> Value {
    manager
        .selected_thread_id()
        .map(|t| Value::String(t.0))
        .unwrap_or(Value::Null)
}

/// Build the `data.conversation_threads` array from a live manager's current
/// snapshot. Callers with an `Option<ConversationsManager>` (tui, pre-auth)
/// return an empty array themselves rather than calling in — this function
/// mirrors linux's always-present-manager shape exactly.
///
/// **Its cost is the thread list's, never the messages' bytes.** Every app's
/// state provider calls this on every publish — linux twenty times a second on
/// its GTK main thread — so each thread is read through
/// [`ConversationsManager::thread_state_facts`], which clones no message. It
/// used `thread_detail`, which clones every body and rendered document: one
/// ~3 MiB mail made each publish cost ~230 ms and starved everything beneath
/// DEFAULT priority on linux (`apps/linux.md` § Message Flow; the cost rule is
/// `e2e-conventions.md` convention 11's second corollary).
pub fn conversation_threads_json(manager: &ConversationsManager) -> Value {
    let snapshot = manager.snapshot();
    let mut rows = Vec::with_capacity(snapshot.threads.len());
    for t in &snapshot.threads {
        let facts = manager.thread_state_facts(&t.thread_id);
        let detail = facts.as_ref().map(|f| &f.detail);
        let (message_count, subject_lines) = match &facts {
            Some(f) => (f.message_count, f.subject_lines.clone()),
            None => (0, Vec::new()),
        };
        // Per-participant actor id (hex) for the Fauna rows, `null` for every
        // other rail — **index-parallel with `participant_displays`**, hence
        // with the `thread-member-chip[i]` those displays render.
        //
        // This is the only observable of an identity succession's participant
        // re-point (`identity-succession.md` § Propagation → *MLS groups*).
        // The re-point is deliberately invisible to everything else a driver
        // can read: it keeps the list position and keeps the handle (the nest
        // moved the handle to the successor inside the succession
        // transaction), so `label`, `participant_count` and the chip's own
        // text are all unchanged by a *successful* one — and equally unchanged
        // by one that never happened.
        //
        // A non-Fauna participant holds its slot as `null` rather than being
        // filtered out: dropping it would slide every later id onto the wrong
        // chip, which presents as the *wrong person* rather than as a missing
        // field.
        let participant_actor_ids: Vec<Value> = match detail {
            Some(d) => d
                .participants
                .iter()
                .map(|p| match p.person_actor_id() {
                    Some(actor_id) => Value::String(actor_id.to_hex()),
                    None => Value::Null,
                })
                .collect(),
            None => Vec::new(),
        };
        // The room's render facts (`conversation-rooms.md` § The room): the
        // class, the viewer's role, one role per participant — **index-parallel
        // with `participant_actor_ids`** and therefore with the chips — and the
        // policy the editor shows. `null` where the rail models no room, and
        // `null` roles on a policy-less room.
        let room = detail.and_then(|d| d.room.as_ref());
        let room_json = match room {
            Some(r) => json!({
                "class": format!("{:?}", r.class),
                "my_role": r.my_role.map(|x| format!("{x:?}")),
                "member_roles": r
                    .members
                    .iter()
                    .map(|m| m.role.map(|x| format!("{x:?}")))
                    .collect::<Vec<_>>(),
                "policy": r.policy.as_ref().map(|p| json!({
                    "version": p.version,
                    "name": p.name,
                    "join_rule": format!("{:?}", p.join_rule),
                    "history_policy": format!("{:?}", p.history_policy),
                })),
                // A community room's two standing facts a journey waits on
                // rather than sleeping past: whether this seat is still
                // waiting for its key-in, and whether the home nest reads the
                // room (`null` where there is no such read — an end-to-end
                // room, or an answer not read yet).
                "awaiting_key": r.awaiting_key,
                "nest_read": r.nest_read,
                // The room's VERIFIED labeler set (lowercase hex ids) — what a
                // journey waits on after Save, rather than sleeping past it;
                // `null` where there is none (`RoomSnapshot::labelers`).
                "labelers": r.labelers,
            }),
            None => Value::Null,
        };
        let capabilities_json = detail.map(|d| {
            json!({
                "can_invite": d.capabilities.can_invite,
                "can_remove_members": d.capabilities.can_remove_members,
                "can_set_policy": d.capabilities.can_set_policy,
                "can_appoint_admins": d.capabilities.can_appoint_admins,
                "can_transfer_ownership": d.capabilities.can_transfer_ownership,
                "can_leave_room": d.capabilities.can_leave_room,
                "supports_rename": d.capabilities.supports_rename,
                "encryption": format!("{:?}", d.capabilities.encryption),
            })
        });
        rows.push(json!({
            "thread_id": t.thread_id.0,
            "label": t.label,
            "snippet": t.snippet,
            "rail": format!("{:?}", t.rail),
            "flavor": format!("{:?}", t.flavor),
            "unread_count": t.unread_count,
            "participant_count": t.participant_count,
            "message_count": message_count,
            "message_subject_lines": subject_lines,
            "participant_actor_ids": participant_actor_ids,
            // What the chips SAY, index-parallel with the ids above — the
            // rendered `thread-member-chip[i]` text, straight off
            // `ThreadDetail::participant_displays` with no re-derivation, so a
            // driver reads exactly what the app paints.
            //
            // Without it a driver could see *that* a member was seated and
            // *who* they are, but never that the row was **blank** — which is
            // precisely how a member seated off an MLS roster rendered before
            // `TypedAddress::display` gained its elided-id fallback
            // (`value-formatting.md` § Account display label). A field read of
            // an already-computed column: no I/O on the ack path, as e2e
            // convention 11's corollary requires.
            "participant_displays": detail
                .map(|d| d.participant_displays.clone())
                .unwrap_or_default(),
            "room": room_json,
            "capabilities": capabilities_json,
            // The bound nest-channel id (hex) once a FaunaMls group has
            // bootstrapped, else null — lets a real-wire tier_3 test observe
            // the channel an MLS thread carries on the nest.
            "channel_id_hex": manager.channel_hex(&t.thread_id),
        }));
    }
    Value::Array(rows)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backends::mock::MockRailBackend;
    use crate::{
        BodyFormat, MessageBadges, MessageId, Rail, RailInboundMessage, ThreadId, TypedAddress,
    };
    use fauna_core::identity::ActorKeypair;
    use std::sync::Arc;

    fn inbound(sender_seed: u8, message_id: &str) -> RailInboundMessage {
        let actor_id = ActorKeypair::from_secret([sender_seed; 32]).actor_id();
        RailInboundMessage {
            rail: Rail::FaunaMls,
            sender: TypedAddress::Fauna {
                handle: format!("peer{sender_seed}@nest.test"),
                actor_id,
            },
            recipients: vec![],
            // One subject per sender: the thread key includes it, so a
            // sender's messages share one thread.
            subject: Some(format!("from peer {sender_seed}")),
            // A body worth not copying: the pin is about what the read clones,
            // and a large body is what made the clone cost anything.
            body: "x".repeat(64 * 1024),
            body_format: BodyFormat::PlainText,
            timestamp_ms: 1,
            message_id: MessageId(message_id.into()),
            in_reply_to: None,
            attachments: vec![],
            badges: MessageBadges::default(),
            legal_takedown_ref: None,
            plane_ref: None,
        }
    }

    /// **Serializing the thread list clones no message** — counted, not timed
    /// (`e2e-latency-independent-assertions.md`, convention 14).
    ///
    /// Every app's state provider calls this on every publish, linux twenty
    /// times a second on its GTK main thread. Through `thread_detail` each call
    /// cloned every message of every thread; with one ~3 MiB inbound mail in
    /// the mailbox a linux publish cost ~230 ms, the thread never went idle,
    /// and the barrier ack and every confirm dialog beneath DEFAULT priority
    /// starved (2026-09-21, `apps/linux.md` § Message Flow). The rows must
    /// still say exactly what the messages say.
    #[test]
    fn serializing_the_thread_list_clones_no_message() {
        let manager = ConversationsManager::new();
        manager.register_backend(Arc::new(MockRailBackend::new(Rail::FaunaMls)));
        for (seed, id) in [(21, "a1"), (21, "a2"), (22, "b1"), (23, "c1")] {
            manager.ingest_inbound(inbound(seed, id)).expect("ingest");
        }

        let before = crate::store::threads::full_clones();
        let rows = conversation_threads_json(&manager);
        let cloned = crate::store::threads::full_clones() - before;

        let rows = rows.as_array().expect("the contract is an array").clone();
        assert_eq!(rows.len(), 3, "three senders, three threads: {rows:?}");
        assert!(
            rows.iter().any(|r| r["message_count"] == json!(2)),
            "one thread holds two messages, so a count is actually read: {rows:?}"
        );
        assert_eq!(
            cloned,
            0,
            "serializing {} threads cloned {cloned} full thread details — every \
             message body and document, on every state publish",
            rows.len()
        );

        for row in &rows {
            let id = ThreadId(row["thread_id"].as_str().expect("thread id").to_string());
            let detail = manager.thread_detail(id).expect("the row's thread exists");
            let subjects: Vec<String> = detail
                .messages
                .iter()
                .map(|m| m.subject_line.clone().unwrap_or_default())
                .collect();
            assert_eq!(row["message_count"], json!(detail.messages.len()), "{row}");
            assert_eq!(row["message_subject_lines"], json!(subjects), "{row}");
            assert_eq!(
                row["participant_displays"],
                json!(detail.participant_displays),
                "{row}"
            );
        }
    }

    /// `data.selected_thread_id` is the manager's selection: `null` with none,
    /// the thread's id once one is selected.
    #[test]
    fn the_selected_thread_is_published_and_null_when_none() {
        let manager = ConversationsManager::new();
        manager.register_backend(Arc::new(MockRailBackend::new(Rail::FaunaMls)));
        manager.ingest_inbound(inbound(31, "s1")).expect("ingest");
        let tid = manager.snapshot().threads[0].thread_id.clone();

        assert_eq!(selected_thread_id_json(&manager), Value::Null);
        manager.select_thread(tid.clone());
        assert_eq!(selected_thread_id_json(&manager), json!(tid.0));
    }
}
