//! Test-only conversations-plane fakes shared by the `commit_gate` and
//! `gate_impl` unit tests: an in-memory nest channel log enforcing the
//! device-owned-epoch commit gate ([`FakeConvNest`]), its [`ConversationsRpc`]
//! seam wrapper ([`ConvNest`]), and the no-IO [`block_on`] executor. Kept in
//! one place so the gate contract the two suites model cannot drift apart.
//!
//! **Two nests, when a test asks for them.** A [`FakeConvNest`] is one nest, but
//! it can be handed **peers** ([`FakeConvNest::register_peer_nest`]) — other
//! `FakeConvNest`s reachable at a home URL — and then it models the federation
//! relay: `channel_send_remote` / a `channel_fetch` carrying a `home_nest_url`
//! land on the *peer's* log, not this one. Without that registration the relay
//! arm still fails loud, which is what a single-nest test wants. This is what
//! lets a test express a **foreign-homed** channel at all: while the fake could
//! only model one nest, no test in this crate could tell a commit that routed
//! correctly from one that blackholed into the member's own log
//! .

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use fauna_conversations::backend::{ConvRpcError, ConversationsRpc};
use fauna_core::identity::ActorId;
use fauna_mls::types::ChannelId;

use crate::commit_gate::GatedCommitSend;
// Re-exported so the sibling modules importing `block_on` from here (`use
// crate::test_conv::{.., block_on}`) keep working unchanged.
pub(crate) use fauna_client_testkit::block_on;
use fauna_mls::types::ChannelEnvelope;

/// Lowercase-hex of an actor id — the key `bootstrap_group` fetches key
/// packages under.
pub(crate) fn actor_hex(actor: &ActorId) -> String {
    actor.to_hex()
}

/// Ordered record of cross-plane side effects (`"provider-put"`,
/// `"commit-send"`), so a test can assert the design-§3 crash-safety sequence
/// rather than just its end state.
pub(crate) type EventLog = Arc<Mutex<Vec<&'static str>>>;

/// The fake conversations log: `channel_hex → [(seq, envelope)]`.
type ConvLog = Mutex<HashMap<String, Vec<(i64, Vec<u8>)>>>;

/// In-memory conversations log enforcing the device-owned-epoch commit gate:
/// `channel_send` with `Some(expect)` rejects `StaleCommit` when a `Commit`
/// landed after `expect`; every accepted `Commit` bumps the per-channel commit
/// high-water. Records a `"commit-send"` event on each accepted `Commit` send.
#[derive(Default)]
pub(crate) struct FakeConvNest {
    // channel_hex -> Vec<(seq, envelope)>
    log: ConvLog,
    // channel_hex -> highest seq that carried a Commit envelope
    commit_high: Mutex<HashMap<String, i64>>,
    next_seq: Mutex<i64>,
    pub(crate) events: EventLog,
    // actor_hex -> published key packages, served FIFO by `keypackage_fetch`
    // (the nest's one-time pool). Register via `register_key_package` so a test
    // can drive the REAL `bootstrap_group` (create_group → bind → Welcome).
    key_packages: Mutex<HashMap<String, Vec<Vec<u8>>>>,
    // (recipient_hex, channel_hex) of every accepted `welcome_deliver`.
    welcomes: Mutex<Vec<(String, String)>>,
    // home_nest_url -> the nest that homes channels at that URL. Empty for the
    // single-nest default; populated by `register_peer_nest` to model federation.
    peers: Mutex<HashMap<String, Arc<FakeConvNest>>>,
    // channel_hex -> that channel's home nest URL, the test twin of
    // `FaunaMlsBackend::channel_home`. Absent = same-nest (the common case).
    channel_home: Mutex<HashMap<String, String>>,
    // When set, the NEXT `channel_fetch` serves a non-empty page that does NOT
    // advance past the requested `after` (echoes a record AT the cursor) — a
    // server-contract violation that the inbound walks' no-progress guard must
    // classify as an INCOMPLETE walk, not a clean completion.
    stall_next_fetch: Mutex<bool>,
    // When set, every `Commit` send is refused with a terminal `Rejected` —
    // the shape a member's takeover meets when the nest refuses its Commit
    // (`federation.md` § Cross-nest shared
    // folders + channel append; current nests admit rostered members'
    // commits, but a terminal refusal stays a live shape, and any terminal
    // rejection must unstage). Application sends are
    // deliberately NOT refused: that asymmetry is the point — the app send is
    // accepted while the Commit in front of it is not.
    refuse_commits: Mutex<bool>,
}

impl FakeConvNest {
    /// Build with a caller-shared event log (the mls-plane fake pushes its
    /// `"provider-put"` events into the same one).
    pub(crate) fn with_events(events: EventLog) -> Self {
        Self {
            events,
            ..Default::default()
        }
    }

    /// Refuse every subsequent `Commit` send with a terminal rejection — the
    /// claimant-gate refusal seen from a non-claimant member (a live shape; any
    /// terminal rejection must
    /// unstage).
    pub(crate) fn refuse_commits(&self) {
        *self.refuse_commits.lock().unwrap() = true;
    }

    fn alloc_seq(&self) -> i64 {
        let mut n = self.next_seq.lock().unwrap();
        *n += 1;
        *n
    }

    fn is_commit(env: &[u8]) -> bool {
        matches!(
            ChannelEnvelope::from_bytes(env),
            Ok(ChannelEnvelope::Commit(_))
        )
    }

    /// Append an out-of-band record (models another device/member's commit).
    pub(crate) fn inject(&self, hex: &str, envelope: Vec<u8>) -> i64 {
        let seq = self.alloc_seq();
        if Self::is_commit(&envelope) {
            self.commit_high
                .lock()
                .unwrap()
                .insert(hex.to_string(), seq);
        }
        self.log
            .lock()
            .unwrap()
            .entry(hex.to_string())
            .or_default()
            .push((seq, envelope));
        seq
    }

    pub(crate) fn fetch_after(&self, hex: &str, after: i64) -> Vec<(i64, Vec<u8>)> {
        self.log
            .lock()
            .unwrap()
            .get(hex)
            .map(|v| v.iter().filter(|(s, _)| *s > after).cloned().collect())
            .unwrap_or_default()
    }

    /// Publish a key package for `actor` (the `keypackage_upload` analogue), so
    /// a test can drive the real group bootstrap against this fake nest.
    pub(crate) fn register_key_package(&self, actor: &ActorId, kp_bytes: Vec<u8>) {
        self.key_packages
            .lock()
            .unwrap()
            .entry(actor_hex(actor))
            .or_default()
            .push(kp_bytes);
    }

    /// The `(recipient_hex, channel_hex)` pairs delivered so far.
    #[allow(dead_code)]
    pub(crate) fn delivered_welcomes(&self) -> Vec<(String, String)> {
        self.welcomes.lock().unwrap().clone()
    }

    /// Make `nest` the home of every channel addressed at `home_nest_url`, so a
    /// relayed send/fetch through this nest lands on `nest`'s log. Un-registered
    /// URLs keep failing loud — a mis-routed send must never look like success.
    pub(crate) fn register_peer_nest(&self, home_nest_url: &str, nest: Arc<FakeConvNest>) {
        self.peers
            .lock()
            .unwrap()
            .insert(home_nest_url.to_string(), nest);
    }

    /// The nest homing `home_nest_url`, or `None` if none was registered.
    fn peer(&self, home_nest_url: &str) -> Option<Arc<FakeConvNest>> {
        self.peers.lock().unwrap().get(home_nest_url).cloned()
    }

    /// How many `Commit` envelopes this nest's log holds for `channel` — the
    /// observable a routing test needs: WHICH nest a gated commit landed on, not
    /// merely that the send returned `Ok`.
    pub(crate) fn commits_on(&self, channel: &ChannelId) -> usize {
        self.fetch_after(&channel.to_string(), 0)
            .iter()
            .filter(|(_, env)| FakeConvNest::is_commit(env))
            .count()
    }

    /// Every record this nest's log holds for `channel`, commits and
    /// applications alike.
    pub(crate) fn records_on(&self, channel: &ChannelId) -> usize {
        self.fetch_after(&channel.to_string(), 0).len()
    }

    /// Record `channel` as homed on `home_nest_url` — the test twin of
    /// `FaunaMlsBackend::record_channel_home`, which production fills from a
    /// cross-nest Welcome's `nest_url`.
    ///
    /// This is the only writer of `channel_home`, so leaving it unrecorded
    /// makes `channel_home_url` always `None` and the relay arm of
    /// [`ConvNest::send_gated_commit`] — the fake's OWN cross-nest routing —
    /// unreachable, which is exactly the gap
    /// `a_recorded_home_relays_the_gated_commit_to_the_peer_nest` below
    /// exists to close (the one other cross-nest test,
    /// `a_foreign_homed_gated_takeover_lands_on_the_home_nest`, routes
    /// through the *production* `FaunaMlsBackend` map instead and never
    /// touches this path).
    pub(crate) fn record_channel_home(&self, channel: &ChannelId, home_nest_url: &str) {
        self.channel_home
            .lock()
            .unwrap()
            .insert(channel.to_string(), home_nest_url.to_string());
    }

    /// This channel's recorded home nest URL, `None` for a same-nest channel.
    fn channel_home_url(&self, channel: &ChannelId) -> Option<String> {
        self.channel_home
            .lock()
            .unwrap()
            .get(&channel.to_string())
            .cloned()
    }

    /// Append `envelope` to `channel_id_hex` under the device-owned-epoch gate —
    /// the body both `channel_send` and the relayed `channel_send_remote` run, so
    /// the two doors cannot drift in what they accept.
    fn append_gated(
        &self,
        channel_id_hex: String,
        envelope: Vec<u8>,
        expect_no_commit_since: Option<i64>,
    ) -> Result<i64, ConvRpcError> {
        let is_commit = FakeConvNest::is_commit(&envelope);
        if is_commit && *self.refuse_commits.lock().unwrap() {
            return Err(ConvRpcError::Rejected {
                message: "only the folder channel's claimant may append a Commit envelope".into(),
            });
        }
        if let Some(expect) = expect_no_commit_since {
            let high = self
                .commit_high
                .lock()
                .unwrap()
                .get(&channel_id_hex)
                .copied()
                .unwrap_or(0);
            if high > expect {
                return Err(ConvRpcError::StaleCommit {
                    latest_commit_seq: Some(high),
                });
            }
        }
        let seq = self.alloc_seq();
        if is_commit {
            self.commit_high
                .lock()
                .unwrap()
                .insert(channel_id_hex.clone(), seq);
            self.events.lock().unwrap().push("commit-send");
        }
        self.log
            .lock()
            .unwrap()
            .entry(channel_id_hex)
            .or_default()
            .push((seq, envelope));
        Ok(seq)
    }

    /// Arm a one-shot **misbehaving** fetch: the next `channel_fetch` serves a
    /// non-empty page that does not advance past the requested `after`, modelling
    /// a nest that violates the "every served seq exceeds `after`" contract (a
    /// truncated / stuck page). Drives the inbound walks' no-progress guard.
    pub(crate) fn serve_a_non_advancing_page_once(&self) {
        *self.stall_next_fetch.lock().unwrap() = true;
    }
}

/// The [`ConversationsRpc`] seam over [`FakeConvNest`]. Only `channel_send`
/// (the gate) and `channel_fetch` are exercised; the rest of the trait is
/// stubbed — the rebase loop and the inbound driver touch nothing else.
/// `ConversationsRpc` is `#[async_trait]`-desugared; tests compile native-only,
/// so the native (boxed-`Send`-future) arm suffices.
pub(crate) struct ConvNest(pub(crate) Arc<FakeConvNest>);

/// [`ConvNest`] as the gate's send door: the same `send` vs `send_remote` pick
/// production makes in `FaunaMlsBackend::send_on_channel`, over this fake's own
/// channel-to-home map. Deliberately NOT a blanket impl over `ConversationsRpc`
/// — a bare rpc has no way to make the pick, and handing the gate one is exactly
/// the defect this seam exists to make unrepresentable.
///
/// This mirrors the production pick rather than sharing it (the backend lives a
/// crate boundary away by design — see [`crate::commit_gate::CommitCatchUp`]'s
/// cycle note). The production door itself is pinned end-to-end by
/// `gate_impl.rs::a_foreign_homed_gated_takeover_lands_on_the_home_nest`.
impl GatedCommitSend for ConvNest {
    async fn send_gated_commit(
        &self,
        channel: &ChannelId,
        envelope: Vec<u8>,
        expect_no_commit_since: i64,
    ) -> Result<i64, ConvRpcError> {
        match self.0.channel_home_url(channel) {
            Some(home_nest_url) => {
                self.channel_send_remote(
                    channel.to_string(),
                    home_nest_url,
                    envelope,
                    Some(expect_no_commit_since),
                    Vec::new(),
                )
                .await
            }
            None => {
                self.channel_send(
                    channel.to_string(),
                    envelope,
                    Some(expect_no_commit_since),
                    Vec::new(),
                )
                .await
            }
        }
    }
}

#[async_trait::async_trait]
impl ConversationsRpc for ConvNest {
    async fn channel_send_remote(
        &self,
        channel_id_hex: String,
        home_nest_url: String,
        envelope: Vec<u8>,
        expect_no_commit_since: Option<i64>,
        _attachment_refs: Vec<String>,
    ) -> Result<i64, ConvRpcError> {
        // The federation relay: this nest hands the envelope to the nest that
        // homes the channel, and the record lands THERE. With no peer registered
        // the fake models one nest, so a routed remote send means the caller
        // mis-picked — fail loud rather than quietly accepting it locally.
        match self.0.peer(&home_nest_url) {
            Some(home) => home.append_gated(channel_id_hex, envelope, expect_no_commit_since),
            None => Err(ConvRpcError::Rejected {
                message: format!(
                    "FakeConvNest has no federation relay to {home_nest_url} \
                     (unexpected send_remote)"
                ),
            }),
        }
    }

    async fn channel_send(
        &self,
        channel_id_hex: String,
        envelope: Vec<u8>,
        expect_no_commit_since: Option<i64>,
        _attachment_refs: Vec<String>,
    ) -> Result<i64, ConvRpcError> {
        self.0
            .append_gated(channel_id_hex, envelope, expect_no_commit_since)
    }

    async fn channel_fetch(
        &self,
        channel_id_hex: String,
        after: i64,
        _limit: i64,
        home_nest_url: Option<String>,
    ) -> Result<Vec<fauna_conversations::backend::FetchedRecord>, ConvRpcError> {
        use fauna_conversations::backend::FetchedRecord;
        let record = |(seq, envelope)| FetchedRecord {
            seq,
            envelope,
            ..Default::default()
        };
        // A foreign-homed drain reads the HOME nest's log, mirroring the read
        // side's relay — so a two-nest test's read and write see one log.
        if let Some(home) = home_nest_url.as_deref().and_then(|u| self.0.peer(u)) {
            return Ok(home
                .fetch_after(&channel_id_hex, after)
                .into_iter()
                .map(record)
                .collect());
        }
        if std::mem::take(&mut *self.0.stall_next_fetch.lock().unwrap()) {
            // A misbehaving nest: a non-empty page whose record sits AT `after`
            // (not past it), so the walk cannot advance — the exact server-
            // contract violation the no-progress guard exists for. The bytes are
            // deliberately undecodable so the record is skipped, isolating the
            // guard (the cursor does not move on any other arm either).
            return Ok(vec![record((
                after,
                b"\xff not a valid ChannelEnvelope".to_vec(),
            ))]);
        }
        Ok(self
            .0
            .fetch_after(&channel_id_hex, after)
            .into_iter()
            .map(record)
            .collect())
    }

    async fn keypackage_count(&self, _actor_id_hex: String) -> Result<u64, ConvRpcError> {
        unimplemented!("gate tests never call keypackage_count")
    }
    async fn actor_by_handle(
        &self,
        _handle: String,
    ) -> Result<Option<fauna_conversations::backend::ResolvedHandle>, ConvRpcError> {
        unimplemented!("gate tests never call actor_by_handle")
    }
    async fn actor_by_handle_remote(
        &self,
        _domain: String,
        _localpart: String,
    ) -> Result<Option<fauna_conversations::backend::ResolvedHandle>, ConvRpcError> {
        unimplemented!("gate tests never call actor_by_handle_remote")
    }
    async fn keypackage_fetch(
        &self,
        actor_id_hex: String,
        _peer_domain: Option<String>,
    ) -> Result<Option<Vec<u8>>, ConvRpcError> {
        // Serve (and consume) one published package, like the nest's one-time
        // pool; `None` for an actor that never published — the production
        // "no key package available" bootstrap failure.
        Ok(self
            .0
            .key_packages
            .lock()
            .unwrap()
            .get_mut(&actor_id_hex)
            .and_then(|pool| pool.pop()))
    }
    async fn keypackage_upload(
        &self,
        _packages: Vec<Vec<u8>>,
        _last_resort: bool,
    ) -> Result<u64, ConvRpcError> {
        unimplemented!("gate tests never call keypackage_upload")
    }
    async fn welcome_deliver(
        &self,
        recipient_actor_id_hex: String,
        channel_id_hex: String,
        _welcome_bytes: Vec<u8>,
        _kind: fauna_conversations::backend::WelcomeChannelKind,
        _peer_domain: Option<String>,
    ) -> Result<(), ConvRpcError> {
        // Record the delivery; nothing here consumes the Welcome (the tests
        // that need a joined peer join its engine directly).
        self.0
            .welcomes
            .lock()
            .unwrap()
            .push((recipient_actor_id_hex, channel_id_hex));
        Ok(())
    }
    async fn blob_put(
        &self,
        _channel_id_hex: String,
        _home_nest_url: Option<String>,
        _sealed_cid_hex: String,
        _bytes: Vec<u8>,
    ) -> Result<(), ConvRpcError> {
        unimplemented!("gate tests never call blob_put")
    }
    async fn blob_get(
        &self,
        _channel_id_hex: String,
        _home_nest_url: Option<String>,
        _sealed_cid_hex: String,
    ) -> Result<Option<Vec<u8>>, ConvRpcError> {
        unimplemented!("gate tests never call blob_get")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The fake's own send-door pick — the twin of
    /// `FaunaMlsBackend::send_on_channel` that `GatedCommitSend for ConvNest`
    /// implements: a channel with a recorded home relays to THAT nest, one
    /// without lands locally.
    ///
    /// Without this, [`FakeConvNest::record_channel_home`] has no caller, so
    /// the map stays empty in every suite and the `Some(home_nest_url)` arm is
    /// unreachable — the fake would model the routing decision while only ever
    /// taking one side of it. (The *production* door is pinned separately, by
    /// `gate_impl::a_foreign_homed_gated_takeover_lands_on_the_home_nest`;
    /// this pins the twin, which is what the other suites in this crate
    /// actually send through.)
    #[test]
    fn a_recorded_home_relays_the_gated_commit_to_the_peer_nest() {
        const HOME: &str = "https://home.example";

        let local = Arc::new(FakeConvNest::default());
        let home = Arc::new(FakeConvNest::default());
        local.register_peer_nest(HOME, Arc::clone(&home));

        let homed = ChannelId::from_hex(&"a1".repeat(32)).expect("channel id");
        let same_nest = ChannelId::from_hex(&"b2".repeat(32)).expect("channel id");
        local.record_channel_home(&homed, HOME);

        let nest = ConvNest(Arc::clone(&local));
        block_on(nest.send_gated_commit(&homed, b"foreign-homed".to_vec(), 0))
            .expect("the relay accepts the routed send");
        block_on(nest.send_gated_commit(&same_nest, b"same-nest".to_vec(), 0))
            .expect("the local append accepts the unrouted send");

        assert_eq!(
            home.records_on(&homed),
            1,
            "the foreign-homed channel's record lands on its HOME nest"
        );
        assert_eq!(
            local.records_on(&homed),
            0,
            "...and never blackholes into the sending nest's own log"
        );
        assert_eq!(
            local.records_on(&same_nest),
            1,
            "a channel with no recorded home still lands locally"
        );
        assert_eq!(home.records_on(&same_nest), 0);
    }
}
