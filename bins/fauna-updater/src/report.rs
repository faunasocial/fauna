//! Writing the report into the nest container (`update-channel.md` § What the
//! updater reports → *The file*): per container, the ledger (read back from
//! the container once, so it continues across an updater restart and a
//! hand-off), the transitions that decide when an event is due, and the one
//! `PUT /containers/{id}/archive` that writes the file whole — a one-entry
//! ustar archive this module writes itself, so no dependency is added.

use std::collections::HashMap;

use fauna_provisioning::update_channel::report::{REPORT_DIR, REPORT_FILE, REPORT_PATH, Report};
use tokio::time::Instant;

use crate::catalogue::{CheckFailure, Event};
use crate::engine::Engine;

/// The tar block.
const BLOCK: usize = 512;

/// A one-entry ustar archive: `name` as a root-owned `0644` regular file
/// holding `data`, stamped `mtime_s`.
pub fn ustar(name: &str, data: &[u8], mtime_s: u64) -> Vec<u8> {
    let mut header = [0u8; BLOCK];
    let name = name.as_bytes();
    header[..name.len().min(100)].copy_from_slice(&name[..name.len().min(100)]);
    octal(&mut header[100..108], 0o644);
    octal(&mut header[108..116], 0);
    octal(&mut header[116..124], 0);
    octal(&mut header[124..136], data.len() as u64);
    octal(&mut header[136..148], mtime_s);
    header[156] = b'0';
    header[257..263].copy_from_slice(b"ustar\0");
    header[263..265].copy_from_slice(b"00");
    // The checksum is the byte sum with its own field read as spaces.
    header[148..156].fill(b' ');
    let sum: u32 = header.iter().map(|&b| u32::from(b)).sum();
    let field = format!("{sum:06o}\0 ");
    header[148..156].copy_from_slice(field.as_bytes());

    let padded = data.len().div_ceil(BLOCK) * BLOCK;
    let mut out = Vec::with_capacity(BLOCK + padded + 2 * BLOCK);
    out.extend_from_slice(&header);
    out.extend_from_slice(data);
    out.resize(BLOCK + padded + 2 * BLOCK, 0);
    out
}

/// Write `value` as a zero-padded, NUL-terminated octal field.
fn octal(field: &mut [u8], value: u64) {
    let digits = field.len() - 1;
    let text = format!("{value:0digits$o}");
    field[..digits].copy_from_slice(&text.as_bytes()[text.len() - digits..]);
    field[digits] = 0;
}

fn parse_octal(field: &[u8]) -> Option<usize> {
    let text = std::str::from_utf8(field).ok()?;
    let text = text.trim_matches(|c: char| c == '\0' || c == ' ');
    usize::from_str_radix(text, 8).ok()
}

/// The first regular file's bytes in a tar archive, as the engine's archive
/// read returns one — skipping the extended-header entries an engine may put
/// before it.
pub fn first_file(archive: &[u8]) -> Option<&[u8]> {
    let mut at = 0;
    while at + BLOCK <= archive.len() {
        let header = &archive[at..at + BLOCK];
        if header.iter().all(|&b| b == 0) {
            return None;
        }
        let size = parse_octal(&header[124..136])?;
        let data = at + BLOCK;
        if matches!(header[156], b'0' | 0) {
            return archive.get(data..data + size);
        }
        at = data + size.div_ceil(BLOCK) * BLOCK;
    }
    None
}

/// The report state of one container.
#[derive(Default)]
struct Target {
    /// Failed registry checks since the last success, and when they began.
    registry_failing: Option<(u32, Instant)>,
    nest_unreachable_since: Option<Instant>,
    /// The (running, head) pair a hold was last reported for.
    held: Option<(String, String)>,
    pinned: Option<String>,
}

/// Every container's report state, keyed by container name — the state a
/// transition is decided on — with the ledger keyed by id, since a re-created
/// container starts a fresh one.
#[derive(Default)]
pub struct Reporter {
    by_name: HashMap<String, Target>,
    ledgers: HashMap<String, Report>,
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as u64)
}

impl Reporter {
    fn target(&mut self, name: &str) -> &mut Target {
        self.by_name.entry(name.to_string()).or_default()
    }

    /// Append `event` to container `id`'s ledger and write the file whole.
    /// Best effort: a write that fails is logged, never an error of the pass.
    pub async fn emit<E: Engine>(&mut self, engine: &E, id: &str, event: Event) {
        let ledger = match self.ledgers.remove(id) {
            Some(ledger) => ledger,
            None => read_back(engine, id).await,
        };
        let mut ledger = ledger;
        ledger.push(now_ms(), event.level(), event.id(), event.message());
        let archive = ustar(REPORT_FILE, &ledger.to_bytes(), now_ms() / 1000);
        if let Err(e) = engine.put_archive(id, REPORT_DIR, archive).await {
            tracing::info!("report {}: not written: {e:#}", event.id());
        }
        self.ledgers.insert(id.to_string(), ledger);
    }

    /// Forget a container's ledger — it was removed.
    pub fn forget(&mut self, id: &str) {
        self.ledgers.remove(id);
    }

    /// The health read's outcome: an event on the transition, else none.
    pub fn nest_health(&mut self, name: &str, readable: bool, now: Instant) -> Option<Event> {
        let t = self.target(name);
        match (readable, t.nest_unreachable_since) {
            (false, None) => {
                t.nest_unreachable_since = Some(now);
                Some(Event::NestUnreachable)
            }
            (true, Some(since)) => {
                t.nest_unreachable_since = None;
                Some(Event::NestRecovered {
                    outage: now.duration_since(since),
                })
            }
            _ => None,
        }
    }

    /// A failed registry check: an event on the first failure after a success,
    /// or the first check ever.
    pub fn registry_failed(
        &mut self,
        name: &str,
        kind: CheckFailure,
        channel: fauna_provisioning::update_channel::UpdateChannel,
        now: Instant,
    ) -> Option<Event> {
        let t = self.target(name);
        match &mut t.registry_failing {
            Some((count, _)) => {
                *count += 1;
                None
            }
            None => {
                t.registry_failing = Some((1, now));
                Some(Event::RegistryFailed {
                    kind,
                    channel,
                    interval: channel.registry_check_interval(),
                })
            }
        }
    }

    /// A successful registry check: an event on the first after failures.
    pub fn registry_succeeded(&mut self, name: &str, now: Instant) -> Option<Event> {
        self.target(name)
            .registry_failing
            .take()
            .map(|(failures, since)| Event::RegistryRecovered {
                failures,
                outage: now.duration_since(since),
            })
    }

    /// A hold: an event once per (running, head) pair.
    pub fn hold(
        &mut self,
        name: &str,
        channel: fauna_provisioning::update_channel::UpdateChannel,
        running: String,
        head: String,
    ) -> Option<Event> {
        let t = self.target(name);
        t.pinned = None;
        let pair = (running.clone(), head.clone());
        (t.held.as_ref() != Some(&pair)).then(|| {
            t.held = Some(pair);
            Event::Hold {
                channel,
                running,
                head,
            }
        })
    }

    /// The decision no longer holds (Current or Apply): a later hold of the
    /// same pair is reported again.
    pub fn not_held(&mut self, name: &str) {
        let t = self.target(name);
        t.held = None;
        t.pinned = None;
    }

    /// Pinned: an event once per tag seen.
    pub fn pinned(&mut self, name: &str, tag: &str) -> Option<Event> {
        let t = self.target(name);
        t.held = None;
        (t.pinned.as_deref() != Some(tag)).then(|| {
            t.pinned = Some(tag.to_string());
            Event::Pinned {
                tag: tag.to_string(),
            }
        })
    }
}

/// The ledger the container already holds, or a fresh one: a container without
/// the file, a torn read, an unknown schema all start afresh — whose `seq`
/// still numbers above the old one while the clock moves forward.
async fn read_back<E: Engine>(engine: &E, id: &str) -> Report {
    match engine.get_archive(id, REPORT_PATH).await {
        Ok(archive) => first_file(&archive)
            .and_then(Report::parse)
            .unwrap_or_default(),
        Err(_) => Report::default(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::fake::Fake;
    use fauna_provisioning::update_channel::UpdateChannel;
    use std::time::Duration;

    #[test]
    fn the_ustar_entry_is_one_header_the_data_and_two_end_blocks() {
        let data = br#"{"schema":1}"#;
        let tar = ustar("report.json", data, 0o17);
        assert_eq!(tar.len(), 4 * BLOCK);
        assert_eq!(&tar[..11], b"report.json");
        assert_eq!(&tar[100..108], b"0000644\0");
        assert_eq!(&tar[124..136], b"00000000014\0");
        assert_eq!(tar[156], b'0');
        assert_eq!(&tar[257..265], b"ustar\x0000");
        // The checksum: the header's byte sum with the field as spaces.
        let mut copy = tar[..BLOCK].to_vec();
        copy[148..156].fill(b' ');
        let sum: u32 = copy.iter().map(|&b| u32::from(b)).sum();
        assert_eq!(parse_octal(&tar[148..156]), Some(sum as usize));
        assert_eq!(&tar[BLOCK..BLOCK + data.len()], data);
        assert!(tar[BLOCK + data.len()..].iter().all(|&b| b == 0));
        assert_eq!(first_file(&tar), Some(&data[..]));
    }

    #[test]
    fn the_read_skips_an_extended_header_entry() {
        let pax = b"30 mtime=1700000000.123456789\n";
        let mut tar = ustar("PaxHeader", pax, 0);
        tar[156] = b'x';
        tar.truncate(2 * BLOCK);
        // Re-sum the header for the changed type flag.
        tar[148..156].fill(b' ');
        let sum: u32 = tar[..BLOCK].iter().map(|&b| u32::from(b)).sum();
        tar[148..156].copy_from_slice(format!("{sum:06o}\0 ").as_bytes());
        tar.extend(ustar("report.json", b"{}", 0));
        assert_eq!(first_file(&tar), Some(&b"{}"[..]));
        assert_eq!(first_file(&[0u8; 1024]), None);
    }

    #[tokio::test]
    async fn each_event_is_appended_to_the_read_back_ledger_and_written_whole() {
        let mut old = Report::default();
        old.push(5, "info", "decision.pinned", "old".into());
        let engine = Fake {
            archives: std::sync::Mutex::new(HashMap::from([(
                "nest".to_string(),
                ustar(REPORT_FILE, &old.to_bytes(), 0),
            )])),
            ..Default::default()
        };
        let mut reporter = Reporter::default();
        reporter.emit(&engine, "nest", Event::NestUnreachable).await;
        reporter
            .emit(&engine, "nest", Event::SelfUpdateCompleted)
            .await;
        let written = engine.report("nest").unwrap();
        assert_eq!(written.events.len(), 3);
        assert_eq!(written.events[0].message, "old");
        assert_eq!(written.events[1].event, "check.nest_unreachable");
        assert_eq!(written.events[2].event, "self_update.completed");
        assert!(written.events.windows(2).all(|w| w[0].seq < w[1].seq));
        assert_eq!(
            engine.calls(),
            [
                format!("put {REPORT_DIR} on nest"),
                format!("put {REPORT_DIR} on nest")
            ]
        );
        // A container with no report starts a fresh ledger.
        reporter
            .emit(&engine, "other", Event::SelfUpdateCompleted)
            .await;
        assert_eq!(engine.report("other").unwrap().events.len(), 1);
    }

    #[test]
    fn checks_report_transitions_never_ticks() {
        let mut r = Reporter::default();
        let t0 = Instant::now();
        let later = t0 + Duration::from_secs(120);
        // The first check ever failing is reported; the next failures are not.
        assert!(matches!(
            r.registry_failed("nest", CheckFailure::Dns, UpdateChannel::Dev, t0),
            Some(Event::RegistryFailed { .. })
        ));
        assert_eq!(
            r.registry_failed("nest", CheckFailure::Dns, UpdateChannel::Dev, t0),
            None
        );
        assert_eq!(
            r.registry_succeeded("nest", later),
            Some(Event::RegistryRecovered {
                failures: 2,
                outage: Duration::from_secs(120)
            })
        );
        assert_eq!(r.registry_succeeded("nest", later), None);

        assert_eq!(r.nest_health("nest", true, t0), None);
        assert_eq!(
            r.nest_health("nest", false, t0),
            Some(Event::NestUnreachable)
        );
        assert_eq!(r.nest_health("nest", false, later), None);
        assert_eq!(
            r.nest_health("nest", true, later),
            Some(Event::NestRecovered {
                outage: Duration::from_secs(120)
            })
        );
    }

    #[test]
    fn a_hold_is_reported_once_per_pair_and_a_pin_once_per_tag() {
        let mut r = Reporter::default();
        let hold = |r: &mut Reporter, head: &str| {
            r.hold("nest", UpdateChannel::Test, "0.2.0".into(), head.into())
        };
        assert!(hold(&mut r, "0.1.9").is_some());
        assert!(hold(&mut r, "0.1.9").is_none());
        assert!(hold(&mut r, "0.1.10").is_some());
        r.not_held("nest");
        assert!(hold(&mut r, "0.1.10").is_some());

        assert!(r.pinned("nest", "sha-abc").is_some());
        assert!(r.pinned("nest", "sha-abc").is_none());
        assert!(r.pinned("nest", "sha-def").is_some());
    }
}
