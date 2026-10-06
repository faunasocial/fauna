//! A message thread is a directory of `message_<n>.json` members, and the
//! archive chooses how many (`archive-import.md` § Parser contract rule 9).
//! Reading one must hold one member's JSON at a time, never the whole
//! directory's: this probe counts the heap the thread streams hold at peak and
//! fails when that peak grows with the MEMBER COUNT.
//!
//! It is its own test binary because it installs a counting global allocator,
//! and it is one `#[test]` so no sibling test thread's allocations land in the
//! count. The numbers are allocation sizes, not timings, so the verdict does
//! not depend on machine load.

use std::alloc::{GlobalAlloc, Layout, System};
use std::io::{Cursor, Write};
use std::sync::atomic::{AtomicUsize, Ordering::Relaxed};

use fauna_archive::facebook::FacebookParser;
use fauna_archive::model::{Category, Entity};
use fauna_archive::parser::ArchiveParser;
use fauna_archive::reader::ArchiveReader;
use fauna_archive::source::VecSource;
use zip::CompressionMethod;
use zip::write::SimpleFileOptions;

struct Counting;

static LIVE: AtomicUsize = AtomicUsize::new(0);
static PEAK: AtomicUsize = AtomicUsize::new(0);

fn grew(by: usize) {
    let now = LIVE.fetch_add(by, Relaxed) + by;
    PEAK.fetch_max(now, Relaxed);
}

// SAFETY: every call forwards to `System` unchanged; the counters only observe.
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let ptr = unsafe { System.alloc(layout) };
        if !ptr.is_null() {
            grew(layout.size());
        }
        ptr
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        unsafe { System.dealloc(ptr, layout) };
        LIVE.fetch_sub(layout.size(), Relaxed);
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        let moved = unsafe { System.realloc(ptr, layout, new_size) };
        if !moved.is_null() {
            if new_size >= layout.size() {
                grew(new_size - layout.size());
            } else {
                LIVE.fetch_sub(layout.size() - new_size, Relaxed);
            }
        }
        moved
    }
}

#[global_allocator]
static COUNTING: Counting = Counting;

/// The most bytes live at once while `f` runs, above what was live before it.
fn peak_during(f: impl FnOnce()) -> usize {
    let base = LIVE.load(Relaxed);
    PEAK.store(base, Relaxed);
    f();
    PEAK.load(Relaxed) - base
}

/// Bytes of JSON each member carries that no model field keeps — the part of
/// a member only its parsed `serde_json::Value` holds.
const PADDING: usize = 2 * 1024 * 1024;

/// One thread directory of `members` files, each one message plus
/// [`PADDING`] bytes of an unknown field (rule 2: ignored, but parsed).
/// Deflated, so the zip itself stays small.
fn thread_archive(members: usize) -> VecSource {
    let padding = "x".repeat(PADDING);
    let mut w = zip::ZipWriter::new(Cursor::new(Vec::new()));
    let deflated = SimpleFileOptions::default().compression_method(CompressionMethod::Deflated);
    w.start_file(
        "personal_information/profile_information/profile_information.json",
        deflated,
    )
    .unwrap();
    w.write_all(br#"{ "profile_v2": { "name": { "full_name": "Test Owner" } } }"#)
        .unwrap();
    for n in 1..=members {
        w.start_file(
            format!("your_facebook_activity/messages/inbox/longchat_1/message_{n}.json"),
            deflated,
        )
        .unwrap();
        let member = format!(
            r#"{{ "participants": [ {{ "name": "Friend One" }}, {{ "name": "Test Owner" }} ],
                "messages": [ {{ "sender_name": "Friend One", "timestamp_ms": {n}000, "content": "m{n}" }} ],
                "title": "Friend One", "thread_path": "inbox/longchat_1", "padding": "{padding}" }}"#
        );
        w.write_all(member.as_bytes()).unwrap();
    }
    VecSource(w.finish().unwrap().into_inner())
}

/// Peak heap while one category's stream is drained, and what it yielded —
/// counted, not collected, so the measurement is the stream's own hold.
fn drain(src: &VecSource, category: Category) -> (usize, usize, usize) {
    let mut reader = ArchiveReader::open(src).unwrap();
    let (mut ok, mut err) = (0, 0);
    let peak = peak_during(|| {
        for item in FacebookParser.stream(&mut reader, category) {
            match item {
                Ok(Entity::Thread(_) | Entity::Message(_)) => ok += 1,
                Ok(other) => panic!("a thread stream yielded {other:?}"),
                Err(e) => {
                    eprintln!("{e:?}");
                    err += 1;
                }
            }
        }
    });
    (peak, ok, err)
}

#[test]
fn a_thread_holds_one_members_json_whatever_its_member_count() {
    const FEW: usize = 2;
    const MANY: usize = 16;
    let few = thread_archive(FEW);
    let many = thread_archive(MANY);

    for category in [Category::Threads, Category::Messages] {
        let (peak_few, ok_few, err_few) = drain(&few, category.clone());
        let (peak_many, ok_many, err_many) = drain(&many, category.clone());
        assert_eq!(
            (err_few, err_many),
            (0, 0),
            "{category:?}: the probe's archive is valid"
        );
        let expected = |members: usize| match category {
            Category::Threads => 1,
            _ => members,
        };
        assert_eq!(
            (ok_few, ok_many),
            (expected(FEW), expected(MANY)),
            "{category:?}: every member's message is read"
        );

        // 14 more members may add their 14 small messages, but not their
        // JSON: each is PADDING bytes a held `Value` would keep. Half of ONE
        // member's padding is the tolerance, so a directory that keeps even
        // one extra member's JSON alive fails, and the old whole-directory
        // hold misses by 14 × PADDING.
        assert!(
            peak_many < peak_few + PADDING / 2,
            "{category:?}: reading a thread of {MANY} members peaked at {peak_many} B, \
             {FEW} members at {peak_few} B — the hold grows with the member count \
             (each member is {PADDING} B of JSON), so the directory's members are \
             held together (rule 9)"
        );
    }
}
