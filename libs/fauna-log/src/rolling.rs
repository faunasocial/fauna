//! The on-disk log file: daily-named, size-capped, and bounded in total.
//!
//! A long-running process on a user's machine (the per-user sync agent under
//! launchd, the Windows nest service, a desktop app left open for weeks) must
//! never grow a log the user has to find and truncate by hand — a sync agent
//! stuck retrying a rejected credential once filled a single file to 6.6 GB. So
//! the file sink is bounded by construction, with constants, not a setting
//! (nobody chooses a log budget — `observability.md` § Persistence & privacy):
//!
//! * the day's file is `fauna.log.<YYYY-MM-DD>` (UTC) — the name every reader
//!   (the e2e drivers, the admin-gate helper, a human) already globs for;
//! * once it reaches [`MAX_FILE_BYTES`] the day continues in
//!   `fauna.log.<YYYY-MM-DD>.<NNN>`, so a plain lexicographic sort of the
//!   directory is oldest→newest and the last name is always the live file;
//! * every time a new file opens, the oldest `fauna.log.*` files are deleted
//!   until the others fit in [`MAX_TOTAL_BYTES`] − [`MAX_FILE_BYTES`], so the
//!   directory never holds more than [`MAX_TOTAL_BYTES`] (plus one line of
//!   overshoot on the live file).
//!
//! **Test-capable builds only:** `FAUNA_E2E_LOG_TOTAL_CAP_BYTES` LOWERS the
//! total cap (the per-file cap follows at the same one-fifth ratio), so a tier_3
//! journey can watch the bound hold on a real app's log directory without
//! writing 50 MiB. The same gate and posture as `fauna-client-region`'s
//! `FAUNA_E2E_REGION_REGISTRY` seed (`e2e-automation-surface-gating.md`,
//! convention 15): compiled out of release artifacts, so a shipped app never
//! even names the variable — and it can only shrink the caps, never raise them.
//! A process whose launch environment arrives only after its logging is up (the
//! android app: `FaunaApp.onCreate` installs the subscriber before the launch
//! intent's extras are re-exported as process env) calls
//! [`reload_e2e_caps_for_test`] once the variable is set, and the open writer
//! lowers its caps at its next write.

use std::fs::{File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
#[cfg(any(test, debug_assertions, feature = "e2e-agent"))]
use std::sync::atomic::{AtomicU64, Ordering};

/// The file-name prefix every log file carries (`fauna.log.<date>…`).
pub const FILE_PREFIX: &str = "fauna.log";
/// A single file rolls over to the next continuation once it reaches this size.
pub const MAX_FILE_BYTES: u64 = 10 * 1024 * 1024;
/// The whole directory's `fauna.log.*` files, live file included, stay under this.
pub const MAX_TOTAL_BYTES: u64 = 50 * 1024 * 1024;

/// The environment variable carrying the e2e total-cap override.
#[cfg(any(test, debug_assertions, feature = "e2e-agent"))]
pub const E2E_TOTAL_CAP_ENV: &str = "FAUNA_E2E_LOG_TOTAL_CAP_BYTES";

/// `(max_file_bytes, max_total_bytes)` for an e2e total-cap override value:
/// `None` (the production constants apply) for an absent, unparsable or zero
/// value, or one that would not lower the cap; otherwise the lowered total and a
/// per-file cap of one fifth of it — the production ratio.
#[cfg(any(test, debug_assertions, feature = "e2e-agent"))]
fn e2e_caps(value: Option<&str>) -> Option<(u64, u64)> {
    let total: u64 = value?.trim().parse().ok()?;
    (total > 0 && total < MAX_TOTAL_BYTES).then(|| ((total / 5).max(1), total))
}

/// The caps [`CappedRollingFile::open`] applies: the constants, or — in a
/// test-capable build — the lowered e2e override.
fn caps() -> (u64, u64) {
    #[cfg(any(test, debug_assertions, feature = "e2e-agent"))]
    if let Some(caps) = e2e_caps(std::env::var(E2E_TOTAL_CAP_ENV).ok().as_deref()) {
        return caps;
    }
    (MAX_FILE_BYTES, MAX_TOTAL_BYTES)
}

/// The lowered total cap [`reload_e2e_caps_for_test`] published for the writer
/// already open (0: none). Read at every write, so it takes effect without
/// reopening the file layer.
#[cfg(any(test, debug_assertions, feature = "e2e-agent"))]
static E2E_TOTAL_CAP_OVERRIDE: AtomicU64 = AtomicU64::new(0);

/// Re-read [`E2E_TOTAL_CAP_ENV`] now and lower the caps of the writer already
/// open — for a process whose launch environment reaches it after
/// [`crate::init`] ran (the android app re-exports its launch intent's extras as
/// process env in `MainActivity`, after `FaunaApp` installed logging). Answers
/// the lowered total in force, or `None` when the variable asks for no
/// lowering. Like the variable itself it only ever lowers the caps, and it is
/// compiled out of release artifacts (convention 15).
#[cfg(any(test, debug_assertions, feature = "e2e-agent"))]
pub fn reload_e2e_caps_for_test() -> Option<u64> {
    let total = e2e_caps(std::env::var(E2E_TOTAL_CAP_ENV).ok().as_deref()).map(|(_, t)| t);
    E2E_TOTAL_CAP_OVERRIDE.store(total.unwrap_or(0), Ordering::Relaxed);
    total
}

const MILLIS_PER_DAY: i64 = 86_400_000;

/// The size-capped rolling writer [`crate::init`] hands to the non-blocking
/// file layer.
pub struct CappedRollingFile {
    dir: PathBuf,
    now_millis: Box<dyn Fn() -> i64 + Send>,
    max_file_bytes: u64,
    max_total_bytes: u64,
    day: i64,
    seq: u32,
    file: Option<File>,
    written: u64,
}

impl CappedRollingFile {
    /// Open (creating `dir` if needed) the live file for today, resuming the
    /// newest one a previous run left behind. Fails — so the caller can degrade
    /// file logging away — when the directory or the file cannot be written.
    pub fn open(dir: &Path) -> io::Result<Self> {
        let (max_file_bytes, max_total_bytes) = caps();
        Self::open_with(
            dir,
            Box::new(|| fauna_core::data::Timestamp::now_millis() as i64),
            max_file_bytes,
            max_total_bytes,
        )
    }

    fn open_with(
        dir: &Path,
        now_millis: Box<dyn Fn() -> i64 + Send>,
        max_file_bytes: u64,
        max_total_bytes: u64,
    ) -> io::Result<Self> {
        std::fs::create_dir_all(dir)?;
        let day = day_of(now_millis());
        let seq = newest_seq_for_day(dir, day);
        let mut this = Self {
            dir: dir.to_path_buf(),
            now_millis,
            max_file_bytes,
            max_total_bytes,
            day,
            seq,
            file: None,
            written: 0,
        };
        this.open_current()?;
        if this.written >= this.max_file_bytes {
            this.seq += 1;
            this.open_current()?;
        }
        Ok(this)
    }

    /// Lower this writer's caps to a published e2e `total` (0: none) — the
    /// per-file cap at the production one-fifth ratio, exactly as
    /// [`e2e_caps`] derives it at open. Never raises them.
    #[cfg(any(test, debug_assertions, feature = "e2e-agent"))]
    fn lower_caps_to(&mut self, total: u64) {
        if let Some((max_file, max_total)) = e2e_caps(Some(&total.to_string()))
            && max_total < self.max_total_bytes
        {
            self.max_file_bytes = max_file;
            self.max_total_bytes = max_total;
        }
    }

    fn current_path(&self) -> PathBuf {
        self.dir.join(file_name(self.day, self.seq))
    }

    fn open_current(&mut self) -> io::Result<()> {
        self.file = None;
        let path = self.current_path();
        let file = OpenOptions::new().create(true).append(true).open(&path)?;
        self.written = file.metadata().map(|m| m.len()).unwrap_or(0);
        self.file = Some(file);
        self.prune();
        Ok(())
    }

    /// Delete the oldest non-live `fauna.log.*` files until they fit beside a
    /// full live file. Best effort: a file another process holds or already
    /// removed is skipped, never an error.
    fn prune(&self) {
        let live = file_name(self.day, self.seq);
        let Ok(entries) = std::fs::read_dir(&self.dir) else {
            return;
        };
        let mut others: Vec<(String, u64)> = entries
            .filter_map(Result::ok)
            .filter_map(|e| {
                let name = e.file_name().into_string().ok()?;
                let is_log = name.starts_with(FILE_PREFIX)
                    && name[FILE_PREFIX.len()..].starts_with('.')
                    && name != live;
                let meta = e.metadata().ok()?;
                (is_log && meta.is_file()).then_some((name, meta.len()))
            })
            .collect();
        others.sort();
        let budget = self.max_total_bytes.saturating_sub(self.max_file_bytes);
        let mut total: u64 = others.iter().map(|(_, len)| len).sum();
        for (name, len) in others {
            if total <= budget {
                break;
            }
            if std::fs::remove_file(self.dir.join(&name)).is_ok() {
                total -= len;
            }
        }
    }
}

impl Write for CappedRollingFile {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        #[cfg(any(test, debug_assertions, feature = "e2e-agent"))]
        self.lower_caps_to(E2E_TOTAL_CAP_OVERRIDE.load(Ordering::Relaxed));
        let today = day_of((self.now_millis)());
        if today != self.day {
            self.day = today;
            self.seq = 0;
            self.open_current()?;
        } else if self.written >= self.max_file_bytes || self.file.is_none() {
            if self.written >= self.max_file_bytes {
                self.seq += 1;
            }
            self.open_current()?;
        }
        let file = self.file.as_mut().expect("open_current installs the file");
        file.write_all(buf)?;
        self.written += buf.len() as u64;
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        match self.file.as_mut() {
            Some(file) => file.flush(),
            None => Ok(()),
        }
    }
}

fn day_of(millis: i64) -> i64 {
    millis.div_euclid(MILLIS_PER_DAY)
}

fn date_string(day: i64) -> String {
    let (y, m, d) = fauna_core::caltime::civil_from_days(day);
    format!("{y:04}-{m:02}-{d:02}")
}

fn file_name(day: i64, seq: u32) -> String {
    match seq {
        0 => format!("{FILE_PREFIX}.{}", date_string(day)),
        n => format!("{FILE_PREFIX}.{}.{n:03}", date_string(day)),
    }
}

/// The highest continuation number a previous run left for `day` (0 when none).
fn newest_seq_for_day(dir: &Path, day: i64) -> u32 {
    let base = format!("{FILE_PREFIX}.{}", date_string(day));
    let Ok(entries) = std::fs::read_dir(dir) else {
        return 0;
    };
    entries
        .filter_map(Result::ok)
        .filter_map(|e| e.file_name().into_string().ok())
        .filter_map(|name| {
            let rest = name.strip_prefix(&base)?;
            if rest.is_empty() {
                Some(0)
            } else {
                rest.strip_prefix('.')?.parse::<u32>().ok()
            }
        })
        .max()
        .unwrap_or(0)
}

/// The largest burst [`emit_burst_for_test`] writes, in lines.
#[cfg(any(test, debug_assertions, feature = "e2e-agent"))]
pub const E2E_BURST_MAX_LINES: u64 = 100_000;
/// The largest padding [`emit_burst_for_test`] puts on one line, in bytes.
#[cfg(any(test, debug_assertions, feature = "e2e-agent"))]
pub const E2E_BURST_MAX_PAD: u64 = 4_096;

/// Every app's `app_log_emit` agent command: write `count` `info` lines
/// through the installed subscriber, line `i` reading `<tag> #<i> <padding>`
/// (`bytes` of padding), so a tier_3 journey can push the on-device file past
/// its lowered cap ([`E2E_TOTAL_CAP_ENV`]) without a real runaway writer. One
/// body for every app (priority #2), so the line shape the journey searches
/// for cannot drift between them. Answers the count written, or a refusal —
/// an untagged burst would ack green and be unfindable in the file, so it is
/// refused loudly (convention 11), as is one over the bounds. Test-capable
/// builds only, behind the same gate as the cap override (convention 15).
#[cfg(any(test, debug_assertions, feature = "e2e-agent"))]
pub fn emit_burst_for_test(tag: &str, count: u64, bytes: u64) -> Result<u64, &'static str> {
    if tag.is_empty() {
        return Err("payload needs a non-empty `tag`");
    }
    if count > E2E_BURST_MAX_LINES || bytes > E2E_BURST_MAX_PAD {
        return Err("`count` is at most 100000 and `bytes` at most 4096");
    }
    let padding = "x".repeat(bytes as usize);
    for i in 0..count {
        tracing::info!(target: "fauna_log::e2e", "{tag} #{i} {padding}");
    }
    Ok(count)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicI64, Ordering};

    // 2026-09-30T00:00:00Z.
    const DAY0: i64 = 20_726 * MILLIS_PER_DAY;

    #[test]
    fn the_e2e_burst_writes_its_count_and_refuses_an_untagged_or_oversized_one() {
        assert_eq!(emit_burst_for_test("tag", 3, 10), Ok(3));
        assert!(emit_burst_for_test("", 3, 10).is_err());
        assert!(emit_burst_for_test("tag", E2E_BURST_MAX_LINES + 1, 10).is_err());
        assert!(emit_burst_for_test("tag", 3, E2E_BURST_MAX_PAD + 1).is_err());
    }

    struct Fixture {
        dir: PathBuf,
        clock: Arc<AtomicI64>,
    }

    impl Fixture {
        fn new(tag: &str) -> Self {
            let dir = std::env::temp_dir()
                .join(format!("fauna-log-rolling-{tag}-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&dir);
            Self {
                dir,
                clock: Arc::new(AtomicI64::new(DAY0 + 1_000)),
            }
        }

        fn open(&self, max_file: u64, max_total: u64) -> CappedRollingFile {
            let clock = Arc::clone(&self.clock);
            CappedRollingFile::open_with(
                &self.dir,
                Box::new(move || clock.load(Ordering::SeqCst)),
                max_file,
                max_total,
            )
            .expect("open")
        }

        fn names(&self) -> Vec<String> {
            let mut names: Vec<String> = std::fs::read_dir(&self.dir)
                .unwrap()
                .map(|e| e.unwrap().file_name().into_string().unwrap())
                .collect();
            names.sort();
            names
        }

        fn total_bytes(&self) -> u64 {
            std::fs::read_dir(&self.dir)
                .unwrap()
                .map(|e| e.unwrap().metadata().unwrap().len())
                .sum()
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.dir);
        }
    }

    #[test]
    fn the_day_file_carries_the_utc_date() {
        let fx = Fixture::new("date");
        let mut w = fx.open(1_000, 5_000);
        w.write_all(b"hello\n").unwrap();
        assert_eq!(fx.names(), ["fauna.log.2026-09-30"]);
    }

    #[test]
    fn a_runaway_writer_stays_under_the_total_cap_forever() {
        // The 401-loop shape: one line after another, for "days", never stopping.
        let fx = Fixture::new("runaway");
        let (max_file, max_total) = (1_000, 5_000);
        let mut w = fx.open(max_file, max_total);
        let line = [b'x'; 99];
        for i in 0..20_000u64 {
            if i % 2_000 == 0 {
                fx.clock.fetch_add(MILLIS_PER_DAY / 2, Ordering::SeqCst);
            }
            w.write_all(&line).unwrap();
            w.write_all(b"\n").unwrap();
            assert!(
                fx.total_bytes() <= max_total + 100,
                "after {i} lines the log dir holds {} bytes (cap {max_total}): {:?}",
                fx.total_bytes(),
                fx.names()
            );
        }
        // Still writing to today's newest file, and the newest file sorts last.
        let names = fx.names();
        let last = names.last().unwrap();
        assert!(last.starts_with("fauna.log.2026-10-05"), "{names:?}");
        assert_eq!(*last, file_name(w.day, w.seq));
    }

    #[test]
    fn a_full_day_file_continues_in_a_numbered_file_that_sorts_after_it() {
        let fx = Fixture::new("continue");
        let mut w = fx.open(10, 1_000);
        for _ in 0..3 {
            w.write_all(b"0123456789\n").unwrap();
        }
        assert_eq!(
            fx.names(),
            [
                "fauna.log.2026-09-30",
                "fauna.log.2026-09-30.001",
                "fauna.log.2026-09-30.002",
            ]
        );
    }

    #[test]
    fn a_restart_resumes_the_newest_file_and_never_reopens_a_full_one() {
        let fx = Fixture::new("resume");
        {
            let mut w = fx.open(10, 1_000);
            w.write_all(b"0123456789\n").unwrap();
            w.write_all(b"ab\n").unwrap();
        }
        // `.001` holds 3 bytes → resumed; the full day file is never appended to.
        let mut w = fx.open(10, 1_000);
        w.write_all(b"cd\n").unwrap();
        let resumed = std::fs::read(fx.dir.join("fauna.log.2026-09-30.001")).unwrap();
        assert_eq!(resumed, b"ab\ncd\n");

        // A restart onto a full newest file opens the next continuation.
        w.write_all(b"0123456789\n").unwrap();
        drop(w);
        let mut w = fx.open(10, 1_000);
        w.write_all(b"ef\n").unwrap();
        assert_eq!(fx.names().last().unwrap(), "fauna.log.2026-09-30.002");
    }

    #[test]
    fn a_writer_open_under_the_production_caps_takes_a_later_lowering() {
        // The android shape: logging is up before the launch environment
        // arrives, so the open writer must take the lowered cap at a write.
        let fx = Fixture::new("late-lower");
        let mut w = fx.open(10_000, 50_000);
        w.write_all(&[b'x'; 99]).unwrap();
        w.lower_caps_to(1_000);
        assert_eq!((w.max_file_bytes, w.max_total_bytes), (200, 1_000));
        // A later, higher (or absent) value never raises them back.
        w.lower_caps_to(5_000);
        w.lower_caps_to(0);
        assert_eq!((w.max_file_bytes, w.max_total_bytes), (200, 1_000));
        for _ in 0..200 {
            w.write_all(&[b'y'; 49]).unwrap();
            w.write_all(b"\n").unwrap();
        }
        assert!(
            fx.total_bytes() <= 1_000 + 50,
            "{} bytes: {:?}",
            fx.total_bytes(),
            fx.names()
        );
        assert!(fx.names().len() > 1, "{:?}", fx.names());
    }

    #[test]
    fn the_e2e_override_only_ever_lowers_the_caps() {
        assert_eq!(e2e_caps(Some("250000")), Some((50_000, 250_000)));
        assert_eq!(e2e_caps(Some(" 3 ")), Some((1, 3)));
        assert_eq!(e2e_caps(None), None);
        assert_eq!(e2e_caps(Some("")), None);
        assert_eq!(e2e_caps(Some("0")), None);
        assert_eq!(e2e_caps(Some("-5")), None);
        assert_eq!(e2e_caps(Some("lots")), None);
        // Never a way to RAISE the bound the constants set.
        assert_eq!(e2e_caps(Some(&MAX_TOTAL_BYTES.to_string())), None);
        assert_eq!(e2e_caps(Some(&u64::MAX.to_string())), None);
    }

    #[test]
    fn pruning_leaves_unrelated_files_alone() {
        let fx = Fixture::new("unrelated");
        std::fs::create_dir_all(&fx.dir).unwrap();
        std::fs::write(fx.dir.join("other.txt"), vec![0u8; 4_000]).unwrap();
        std::fs::write(fx.dir.join("fauna.log"), vec![0u8; 4_000]).unwrap();
        std::fs::write(fx.dir.join("fauna.log.2026-01-01"), vec![0u8; 4_000]).unwrap();
        let mut w = fx.open(1_000, 2_000);
        w.write_all(b"x\n").unwrap();
        assert_eq!(
            fx.names(),
            ["fauna.log", "fauna.log.2026-09-30", "other.txt"],
            "only `fauna.log.*` files are this sink's to prune"
        );
    }
}
