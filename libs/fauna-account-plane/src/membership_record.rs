//! **The membership record** — a seedless engine holder reads the membership
//! answer of the signed-in app beside it.
//!
//! Owner: `docs/goal/architecture/account-runtime-roles.md` § Multi-instance
//! concurrency → *The membership record*. On a desktop the engine holder is
//! the sync agent, which links no MLS engine, so its passes have no membership
//! answer of their own: without this record it walks its own-actor scopes
//! alone and concludes no departure ([`crate::departure`], refusal 1). The app
//! beside it — the seed-leg holder — holds the answer, so it writes it here,
//! into the store both processes open, and the agent reads it at the scope
//! derivation of every pass.
//!
//! One device-local `store_meta` record beside the subscription marker and
//! the departed list ([`crate::departure`]): the wall-clock stamp of the
//! write, then the channel set — newline-framed like the marker, each channel
//! as 64 lowercase hex. An empty set is an affirmative answer ("this app is in
//! no channel"); an absent record is no answer.
//!
//! * **Who writes:** the seed-leg holder, in every pass it runs, the stamp
//!   refreshed whether the set changed or not — the stamp is its heartbeat. A
//!   `None` answer from its engine, and a clean teardown, clear the record.
//!   Every host writes it the same way, a phone or a tab included, where
//!   nobody reads it.
//! * **Who reads:** a runtime wired to read it
//!   ([`crate::account_driver::MembershipSource::Record`] — the seedless
//!   agent), at every pass, never held over. A record older than
//!   [`lease_for`] the reader's backstop interval is no answer: a crashed
//!   app's last snapshot is trusted for at most two of its refresh intervals.
//!   A stamp *ahead* of the reader's clock (the wall clock stepped back) is
//!   trusted for the same span and no longer.
//!
//! Local, for the subscription marker's reason: each machine concludes from
//! its own membership state.

use std::collections::BTreeSet;
use std::time::Duration;

use fauna_account_store::backend::StoreBackend;
use fauna_account_store::store::AccountStore;

/// `store_meta` key holding the record (module docs).
const META_MEMBERSHIP_RECORD: &str = "membership_record";

/// The lease a reader whose backstop interval is `backstop` grants a record:
/// twice that interval (module docs, *Who reads*).
#[must_use]
pub fn lease_for(backstop: Duration) -> Duration {
    backstop.saturating_mul(2)
}

/// One stored record: when it was written and the channel set it names.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MembershipRecord {
    /// Wall-clock milliseconds of the write.
    pub stamp_ms: u64,
    /// The joined channels, sorted and deduplicated.
    pub channels: BTreeSet<[u8; 32]>,
}

impl MembershipRecord {
    fn encode(&self) -> Vec<u8> {
        let mut out = self.stamp_ms.to_string();
        for channel in &self.channels {
            out.push('\n');
            out.push_str(&hex::encode(channel));
        }
        out.into_bytes()
    }

    /// `None` for a record of any other shape — read as no answer, never as an
    /// empty set: a malformed record vouches for nothing.
    fn decode(raw: &[u8]) -> Option<Self> {
        let text = std::str::from_utf8(raw).ok()?;
        let mut lines = text.split('\n');
        let stamp_ms = lines.next()?.parse().ok()?;
        let mut channels = BTreeSet::new();
        for line in lines {
            if !fauna_core::hex32::is_lowercase_hex64(line) {
                return None;
            }
            let mut channel = [0u8; 32];
            hex::decode_to_slice(line, &mut channel).ok()?;
            channels.insert(channel);
        }
        Some(Self { stamp_ms, channels })
    }

    /// Is this record within `lease` of `now_ms`, on either side?
    fn fresh_at(&self, now_ms: u64, lease: Duration) -> bool {
        let lease_ms = u64::try_from(lease.as_millis()).unwrap_or(u64::MAX);
        now_ms.abs_diff(self.stamp_ms) <= lease_ms
    }
}

/// The record as stored, whatever its age; `None` when absent or malformed.
pub async fn read<B: StoreBackend>(
    store: &AccountStore<B>,
) -> anyhow::Result<Option<MembershipRecord>> {
    Ok(store
        .backend()
        .meta_get(META_MEMBERSHIP_RECORD)
        .await?
        .and_then(|raw| MembershipRecord::decode(&raw)))
}

/// The reader's answer at `now_ms`: the channel set of a record within
/// `lease`, else `None` (absent, malformed or stale — *cannot tell*).
pub async fn read_fresh<B: StoreBackend>(
    store: &AccountStore<B>,
    now_ms: u64,
    lease: Duration,
) -> anyhow::Result<Option<Vec<[u8; 32]>>> {
    Ok(read(store)
        .await?
        .filter(|record| record.fresh_at(now_ms, lease))
        .map(|record| record.channels.into_iter().collect()))
}

/// Write the writer's affirmative answer `channels`, stamped `stamp_ms`.
/// Returns whether the channel set differs from the one the store held (an
/// absent or malformed record counts as different) — the seed pass's cue to
/// ask the engine holder for its pass.
pub async fn write<B: StoreBackend>(
    store: &AccountStore<B>,
    channels: &[[u8; 32]],
    stamp_ms: u64,
) -> anyhow::Result<bool> {
    let record = MembershipRecord {
        stamp_ms,
        channels: channels.iter().copied().collect(),
    };
    let changed = read(store)
        .await?
        .is_none_or(|held| held.channels != record.channels);
    store
        .backend()
        .meta_put(META_MEMBERSHIP_RECORD, &record.encode())
        .await?;
    Ok(changed)
}

/// Remove the record: the writer's engine cannot tell now, or it is tearing
/// down — either way it vouches for nothing.
pub async fn clear<B: StoreBackend>(store: &AccountStore<B>) -> anyhow::Result<()> {
    store.backend().meta_delete(META_MEMBERSHIP_RECORD).await
}

/// What the writer does with one membership answer `answer` from its engine
/// at `stamp_ms`: write an affirmative one, clear on `None`. Returns whether
/// a written set changed ([`write`]); a clear reports `false`.
pub async fn follow<B: StoreBackend>(
    store: &AccountStore<B>,
    answer: Option<&[[u8; 32]]>,
    stamp_ms: u64,
) -> anyhow::Result<bool> {
    match answer {
        Some(channels) => write(store, channels, stamp_ms).await,
        None => clear(store).await.map(|()| false),
    }
}

#[cfg(test)]
mod tests {
    use fauna_account_store::sqlite::SqliteBackend;
    use fauna_account_store::types::WriterId;

    use super::*;

    const LEASE: Duration = Duration::from_secs(600);
    const NOW: u64 = 10_000_000;

    async fn store() -> AccountStore<SqliteBackend> {
        AccountStore::open(
            SqliteBackend::open_in_memory().unwrap(),
            "0a0a",
            WriterId([7; 32]),
        )
        .await
        .unwrap()
    }

    #[tokio::test]
    async fn a_fresh_record_answers_with_its_channels() {
        let s = store().await;
        write(&s, &[[2; 32], [1; 32]], NOW - 1_000).await.unwrap();
        assert_eq!(
            read_fresh(&s, NOW, LEASE).await.unwrap(),
            Some(vec![[1; 32], [2; 32]])
        );
    }

    #[tokio::test]
    async fn a_stale_record_is_no_answer() {
        let s = store().await;
        let lease_ms = LEASE.as_millis() as u64;
        write(&s, &[[1; 32]], NOW - lease_ms - 1).await.unwrap();
        assert_eq!(read_fresh(&s, NOW, LEASE).await.unwrap(), None);
        // A stamp ahead of the reader's clock is trusted for the lease alone.
        write(&s, &[[1; 32]], NOW + lease_ms + 1).await.unwrap();
        assert_eq!(read_fresh(&s, NOW, LEASE).await.unwrap(), None);
        write(&s, &[[1; 32]], NOW + 1_000).await.unwrap();
        assert!(read_fresh(&s, NOW, LEASE).await.unwrap().is_some());
    }

    #[tokio::test]
    async fn an_absent_record_is_no_answer() {
        let s = store().await;
        assert_eq!(read_fresh(&s, NOW, LEASE).await.unwrap(), None);
        write(&s, &[[1; 32]], NOW).await.unwrap();
        clear(&s).await.unwrap();
        assert_eq!(read_fresh(&s, NOW, LEASE).await.unwrap(), None);
    }

    #[tokio::test]
    async fn an_empty_set_is_an_affirmative_answer() {
        let s = store().await;
        write(&s, &[], NOW).await.unwrap();
        assert_eq!(read_fresh(&s, NOW, LEASE).await.unwrap(), Some(vec![]));
    }

    #[tokio::test]
    async fn a_malformed_record_is_no_answer() {
        let s = store().await;
        s.backend()
            .meta_put(META_MEMBERSHIP_RECORD, b"123\nnot-hex")
            .await
            .unwrap();
        assert_eq!(read_fresh(&s, 123, LEASE).await.unwrap(), None);
    }

    #[tokio::test]
    async fn write_reports_a_changed_set_and_follow_clears_on_none() {
        let s = store().await;
        assert!(
            write(&s, &[[1; 32]], NOW).await.unwrap(),
            "absent → changed"
        );
        assert!(
            !write(&s, &[[1; 32]], NOW + 5).await.unwrap(),
            "same set, new stamp → unchanged"
        );
        assert_eq!(read(&s).await.unwrap().unwrap().stamp_ms, NOW + 5);
        assert!(
            follow(&s, Some(&[]), NOW + 6).await.unwrap(),
            "left → changed"
        );
        assert!(!follow(&s, None, NOW + 7).await.unwrap());
        assert_eq!(read(&s).await.unwrap(), None, "a None answer clears");
    }
}
