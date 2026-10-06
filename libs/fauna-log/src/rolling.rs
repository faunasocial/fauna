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

use std::fs::{File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};

/// The file-name prefix every log file carries (`fauna.log.<date>…`).
pub const FILE_PREFIX: &str = "fauna.log";
/// A single file rolls over to the next continuation once it reaches this size.
pub const MAX_FILE_BYTES: u64 = 10 * 1024 * 1024;
/// The whole directory's `fauna.log.*` files, live file included, stay under this.
pub const MAX_TOTAL_BYTES: u64 = 50 * 1024 * 1024;

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
        Self::open_with(
            dir,
            Box::new(|| fauna_core::data::Timestamp::now_millis() as i64),
            MAX_FILE_BYTES,
            MAX_TOTAL_BYTES,
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicI64, Ordering};

    // 2026-09-30T00:00:00Z.
    const DAY0: i64 = 20_726 * MILLIS_PER_DAY;

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
