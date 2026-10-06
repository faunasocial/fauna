//! Path and bucket helpers for the `__mail/<actor_id_hex>/` reserved
//! folder.

use std::path::{Path, PathBuf};

/// Root directory for one actor's mail segments.
/// `<data_dir>/__mail/<actor_id_hex>/`
///
/// Matches `SegmentManager::scope_dir(actor_id)` (kind = "mail"):
/// `<data_dir>/__mail/<hex>/` — no `segments/` subdirectory.
pub fn mail_segments_root(data_dir: &Path, actor_id: &[u8; 32]) -> PathBuf {
    let hex = actor_id_hex(actor_id);
    data_dir.join("__mail").join(hex)
}

/// Manifest file for one actor's mail segments.
/// `<data_dir>/__mail/<actor_id_hex>/manifest.mail`
pub fn mail_manifest_path(data_dir: &Path, actor_id: &[u8; 32]) -> PathBuf {
    mail_segments_root(data_dir, actor_id).join("manifest.mail")
}

fn actor_id_hex(actor_id: &[u8; 32]) -> String {
    let mut s = String::with_capacity(64);
    for b in actor_id {
        use std::fmt::Write;
        write!(&mut s, "{:02x}", b).expect("write hex");
    }
    s
}

/// Calendar-month bucket key for a `received_at` timestamp.
/// Returns `"YYYY-MM"` (UTC).
///
/// `received_at_secs` is epoch seconds. Callers with epoch
/// milliseconds (the nest's `now_epoch_millis()` shape) divide by
/// 1000 first.
///
/// The civil-from-days algorithm itself lives in
/// `fauna_segment_store::bucket_for` (Plan 7 lift — conv is the second
/// consumer, so the rule is shared rather than copied). Under the
/// `nest-segments` feature we delegate to it directly; without that feature
/// (e.g. the wasm content-index build) the segment-store crate isn't a
/// dependency, so the byte-identical [`bucket_for_pure`] below serves. The
/// two are pinned to the same output by `bucket_matches_segment_store`.
#[cfg(feature = "nest-segments")]
pub fn bucket_for(received_at_secs: i64) -> String {
    fauna_segment_store::bucket_for(received_at_secs)
}

/// Bucket key for builds without the `nest-segments` feature — the local
/// copy, since `fauna-segment-store` is not a dependency there.
#[cfg(not(feature = "nest-segments"))]
pub fn bucket_for(received_at_secs: i64) -> String {
    bucket_for_pure(received_at_secs)
}

/// The pure (no-`fauna-segment-store`) copy of the bucket rule, byte-identical
/// to `fauna_segment_store::bucket_for`.
///
/// It is a **separate function from [`bucket_for`] on purpose.** Under
/// `nest-segments` the public `bucket_for` *is* the segment-store one, so a
/// guard written against `bucket_for` compares that function to itself and can
/// never see this copy drift — which is exactly what
/// `bucket_matches_segment_store` did until 2026-08-22. The `cfg(test)` arm
/// keeps this copy compiled under `nest-segments` as well, the one
/// configuration where both implementations exist at once, so the guard can
/// compare them for real.
#[cfg(any(not(feature = "nest-segments"), test))]
fn bucket_for_pure(received_at_secs: i64) -> String {
    // Civil-from-days algorithm (Howard Hinnant, public domain). UTC.
    let secs = received_at_secs.max(0) as u64;
    let days_total = (secs / 86_400) as i64;
    let z = days_total + 719_468;
    let era = z.div_euclid(146_097);
    let doe = (z - era * 146_097) as u64; // 0..=146096
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y_civ = y + if m <= 2 { 1 } else { 0 };
    format!("{:04}-{:02}", y_civ, m)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn segments_root_uses_lowercase_hex() {
        let actor = [0xabu8; 32];
        let path = mail_segments_root(Path::new("/data"), &actor);
        let s = path.to_string_lossy();
        assert!(s.ends_with(&"ab".repeat(32)), "got {s}");
        assert!(s.contains("/__mail/"), "got {s}");
        // Must NOT include an extra segments/ subdirectory — path matches
        // SegmentManager::scope_dir(actor_id) with kind="mail".
        assert!(
            !s.contains("/segments/__mail/"),
            "unexpected segments/ prefix: {s}"
        );
    }

    #[test]
    fn manifest_path_under_root() {
        let actor = [0u8; 32];
        let p = mail_manifest_path(Path::new("/data"), &actor);
        assert_eq!(
            p.file_name().and_then(|s| s.to_str()),
            Some("manifest.mail")
        );
    }

    #[test]
    fn bucket_known_dates() {
        // 2026-05-14 12:00 UTC = 1778760000
        assert_eq!(bucket_for(1_778_760_000), "2026-05");
        // 2025-12-31 23:59 UTC = 1767225540
        assert_eq!(bucket_for(1_767_225_540), "2025-12");
        // Epoch.
        assert_eq!(bucket_for(0), "1970-01");
    }

    #[test]
    fn bucket_handles_negative_clamp() {
        assert_eq!(bucket_for(-1), "1970-01");
    }

    /// The copy-vs-owner guard. Mail keeps [`bucket_for_pure`] for builds
    /// without `fauna-segment-store`, and a bucket key that disagrees with the
    /// nest's files a mail record under the wrong `YYYY-MM` segment directory —
    /// so the copy must never drift from the owner.
    ///
    /// It compares `bucket_for_pure` against `fauna_segment_store::bucket_for`
    /// directly, NOT the public `bucket_for`: under `nest-segments` the public
    /// one *is* the segment-store function, so the older form of this test
    /// asserted `fauna_segment_store::bucket_for == fauna_segment_store::bucket_for`
    /// and could not fail however far the copy drifted.
    ///
    /// The spread is exhaustive rather than sampled: both edges of every
    /// calendar day from the epoch to 2100, which covers every month and every
    /// leap-year boundary the two implementations could disagree about, plus
    /// the pre-epoch clamp.
    #[cfg(feature = "nest-segments")]
    #[test]
    fn bucket_matches_segment_store() {
        let check = |secs: i64| {
            assert_eq!(
                bucket_for_pure(secs),
                fauna_segment_store::bucket_for(secs),
                "mail's bucket_for_pure diverged from fauna_segment_store at {secs}"
            );
        };
        for secs in [-1i64, -86_400, -1_000_000_000] {
            check(secs);
        }
        // 1970-01-01 .. 2100-01-01, first and last second of each day.
        for day in 0i64..47_482 {
            check(day * 86_400);
            check(day * 86_400 + 86_399);
        }
    }
}
