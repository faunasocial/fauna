//! `messages/**/message_N.json` → one `ArchiveThread` per directory and its
//! `ArchiveMessage`s oldest-first. Facebook writes a thread newest-first and
//! splits long threads across numbered files, so every file of a thread is
//! read before anything is yielded — one file at a time, never all of their
//! JSON at once (`archive-import.md` § Parser contract rule 9).

use std::collections::VecDeque;

use serde_json::Value;

use crate::error::EntityError;
use crate::facebook::json::{
    actor, arr, check_identity, entity_error, index_dated, media_ref, millis_field, parse_member,
    str_field,
};
use crate::facebook::layout::ThreadFiles;
use crate::facebook::posts::hash_media;
use crate::model::{
    ArchiveMediaRef, ArchiveMessage, ArchiveSummary, ArchiveThread, Category, Entity, EntityKind,
    ExternalActorRef, ExternalId, MessageReaction, Platform, Timestamp, media_instants,
};
use crate::reader::ArchiveReader;

const MEDIA_LISTS: &[&str] = &["photos", "videos", "audio_files", "gifs", "files"];

/// Index: every message record counts (bad ones included) and dates the range.
pub fn index_messages(value: &Value, summary: &mut ArchiveSummary) {
    index_dated(
        arr(value, "messages").iter(),
        &mut summary.counts.messages,
        &mut summary.date_range,
        |m| millis_field(m, "timestamp_ms"),
    );
}

fn resolve(owner: &ExternalActorRef, name: &str) -> ExternalActorRef {
    let a = actor(name);
    if a.name_key == owner.name_key {
        owner.clone()
    } else {
        a
    }
}

/// The platform's thread ID: the last segment of `thread_path`, else of the
/// directory.
fn thread_id(first: Option<&Value>, dir: &str) -> (String, String) {
    let path = first
        .and_then(|v| str_field(v, "thread_path"))
        .unwrap_or_else(|| dir.split("messages/").last().unwrap_or(dir).to_string());
    let id = path
        .trim_end_matches('/')
        .rsplit('/')
        .next()
        .unwrap_or(&path)
        .to_string();
    (id, path)
}

struct ParsedThread {
    /// `None` when the thread was refused; the reason is in `errors`.
    thread: Option<ArchiveThread>,
    messages: Vec<ArchiveMessage>,
    errors: Vec<EntityError>,
}

/// A thread refused whole: what it had already skipped, then the refusal.
fn refused(mut errors: Vec<EntityError>, refusal: EntityError) -> ParsedThread {
    errors.push(refusal);
    ParsedThread {
        thread: None,
        messages: Vec::new(),
        errors,
    }
}

/// What a thread's first readable file says about the thread as a whole.
/// Later files repeat it; the first one wins.
struct ThreadHead {
    external_id: ExternalId,
    path: String,
    title: Option<String>,
    participants: Vec<ExternalActorRef>,
}

/// `Err` when the thread's ID outgrows [`MAX_IDENTITY_BYTES`](crate::facebook::json::MAX_IDENTITY_BYTES): every one of
/// its messages carries a copy, so the thread is refused whole.
fn thread_head(
    first: Option<&Value>,
    dir: &str,
    owner: &ExternalActorRef,
) -> Result<ThreadHead, String> {
    let (id, path) = thread_id(first, dir);
    check_identity("thread ID", &id)?;
    let participants = first
        .map(|v| {
            arr(v, "participants")
                .iter()
                .filter_map(|p| str_field(p, "name"))
                .map(|n| resolve(owner, &n))
                .collect()
        })
        .unwrap_or_default();
    Ok(ThreadHead {
        external_id: ExternalId::native(Platform::Facebook, EntityKind::Thread, &id),
        path,
        title: first.and_then(|v| str_field(v, "title")),
        participants,
    })
}

fn parse_message(
    reader: &mut ArchiveReader<'_>,
    owner: &ExternalActorRef,
    thread: &ExternalId,
    member: &str,
    position: u64,
    m: &Value,
    hash: bool,
) -> Result<ArchiveMessage, EntityError> {
    let sender = str_field(m, "sender_name").ok_or_else(|| {
        entity_error(
            Category::Messages,
            member,
            position,
            "message has no sender_name",
        )
    })?;
    let created_at = millis_field(m, "timestamp_ms").ok_or_else(|| {
        entity_error(
            Category::Messages,
            member,
            position,
            "message has no timestamp_ms",
        )
    })?;
    let text = str_field(m, "content");
    let mut media: Vec<ArchiveMediaRef> = MEDIA_LISTS
        .iter()
        .flat_map(|key| arr(m, key).iter().filter_map(media_ref))
        .collect();
    if hash {
        hash_media(reader, &mut media);
    }
    let reactions = arr(m, "reactions")
        .iter()
        .filter_map(|r| {
            let emoji = str_field(r, "reaction")?;
            let who = str_field(r, "actor")?;
            Some(MessageReaction {
                actor: resolve(owner, &who),
                emoji,
            })
        })
        .collect();
    let external_id = ExternalId::derive(
        Platform::Facebook,
        EntityKind::Message,
        created_at,
        text.as_deref().unwrap_or(""),
        &media_instants(&media),
    );
    Ok(ArchiveMessage {
        external_id,
        thread: thread.clone(),
        sender: resolve(owner, &sender),
        created_at,
        text,
        media,
        reactions,
    })
}

/// Reads one thread directory, file by file.
///
/// The archive chooses how many files a directory holds, so each file's JSON
/// is dropped as soon as its messages are taken: a thread costs one file's
/// parse burst at a time, never the directory's (§ Parser contract rule 9).
/// The messages are kept only when `want_messages` — the Messages stream has
/// to sort them before yielding the first — and the Threads stream keeps just
/// their count and date range. Only a `want_messages` read hashes media.
///
/// A thread whose ID outgrows [`MAX_IDENTITY_BYTES`](crate::facebook::json::MAX_IDENTITY_BYTES) is refused before any
/// of its messages is built, since each would carry a copy.
fn parse_thread(
    reader: &mut ArchiveReader<'_>,
    owner: &ExternalActorRef,
    files: &ThreadFiles,
    want_messages: bool,
) -> ParsedThread {
    let mut errors = Vec::new();
    let mut head: Option<ThreadHead> = None;
    let mut messages = Vec::new();
    let mut message_count: u64 = 0;
    let mut span: Option<(Timestamp, Timestamp)> = None;
    for member in &files.files {
        let value = match reader
            .read_member(member)
            .map_err(|e| e.to_string())
            .and_then(|b| parse_member(&b))
        {
            Ok(value) => value,
            Err(e) => {
                errors.push(entity_error(Category::Threads, member, 0, e));
                continue;
            }
        };
        let thread = match head {
            Some(ref thread) => thread,
            None => match thread_head(Some(&value), &files.dir, owner) {
                Ok(fresh) => head.insert(fresh),
                Err(reason) => {
                    return refused(errors, entity_error(Category::Threads, member, 0, reason));
                }
            },
        };
        for (i, m) in arr(&value, "messages").iter().enumerate() {
            match parse_message(
                reader,
                owner,
                &thread.external_id,
                member,
                i as u64,
                m,
                want_messages,
            ) {
                Ok(msg) => {
                    message_count += 1;
                    let at = msg.created_at;
                    span = Some(span.map_or((at, at), |(lo, hi)| (lo.min(at), hi.max(at))));
                    if want_messages {
                        messages.push(msg);
                    }
                }
                Err(e) => errors.push(e),
            }
        }
    }
    // No file was readable: the directory names the thread.
    let head = match head {
        Some(head) => head,
        None => match thread_head(None, &files.dir, owner) {
            Ok(head) => head,
            Err(reason) => {
                return refused(
                    errors,
                    entity_error(Category::Threads, &files.dir, 0, reason),
                );
            }
        },
    };
    messages.sort_by_key(|m| m.created_at);
    ParsedThread {
        thread: Some(ArchiveThread {
            external_id: head.external_id,
            title: head.title,
            participants: head.participants,
            message_count,
            first_at: span.map(|(lo, _)| lo),
            last_at: span.map(|(_, hi)| hi),
            path: head.path,
        }),
        messages,
        errors,
    }
}

/// Yields threads (one per directory) or messages (oldest-first per
/// directory), directory by directory.
pub struct ThreadStream<'r, 's> {
    reader: &'r mut ArchiveReader<'s>,
    threads: VecDeque<ThreadFiles>,
    category: Category,
    owner: ExternalActorRef,
    pending: VecDeque<Result<Entity, EntityError>>,
}

impl<'r, 's> ThreadStream<'r, 's> {
    pub fn new(
        reader: &'r mut ArchiveReader<'s>,
        threads: Vec<ThreadFiles>,
        category: Category,
        owner: ExternalActorRef,
    ) -> Self {
        ThreadStream {
            reader,
            threads: threads.into(),
            category,
            owner,
            pending: VecDeque::new(),
        }
    }
}

impl Iterator for ThreadStream<'_, '_> {
    type Item = Result<Entity, EntityError>;

    fn next(&mut self) -> Option<Self::Item> {
        loop {
            if let Some(item) = self.pending.pop_front() {
                return Some(item);
            }
            let files = self.threads.pop_front()?;
            let want_messages = self.category == Category::Messages;
            let parsed = parse_thread(self.reader, &self.owner, &files, want_messages);
            // Member-level failures (category Threads) belong to both streams;
            // per-message failures (category Messages) only to the Messages stream.
            let (thread_errors, message_errors): (Vec<EntityError>, Vec<EntityError>) = parsed
                .errors
                .into_iter()
                .partition(|e| e.category == Category::Threads);
            self.pending.extend(thread_errors.into_iter().map(Err));
            if want_messages {
                self.pending.extend(message_errors.into_iter().map(Err));
                self.pending
                    .extend(parsed.messages.into_iter().map(|m| Ok(Entity::Message(m))));
            } else if let Some(thread) = parsed.thread {
                self.pending.push_back(Ok(Entity::Thread(thread)));
            }
        }
    }
}
