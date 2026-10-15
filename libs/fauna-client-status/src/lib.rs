//! The shared **Status-page snapshot** — the node, sync, MLS and build legs
//! every app renders under the canonical `status-*` IDs, and the text each
//! element paints (`docs/goal/ui/status.md` § State & data shape, ratified
//! 2026-09-27; the surface itself is § Layout & flow).
//!
//! # What this crate is
//!
//! An **aggregate** of four sources that already exist in shared Rust, never a
//! second definition of any of them:
//!
//! | leg | source | owner of the semantics |
//! |---|---|---|
//! | [`NodeLeg`] | `fauna.nest.info` (`domain`, `version`) — [`StatusClient::node`], this crate's only RPC | the nest's discovery kinds |
//! | [`SyncLeg`] | the sync-agent pipe's `SyncStatusInfo`, embedded verbatim behind the `agent` feature | `sync-agent.md` § Local agent health → *the sync-status projection* |
//! | [`MlsLeg`] | the bearer's own `fauna.conversations.keypackage.count` (the conversations client every face already carries) + `fauna_conversations::snapshot::secure_channel_count` | `direct-messages.md` § Key Package Management; `fauna-conversations` |
//! | [`BuildLeg`] | the compile-time `FAUNA_BUILD_COMMIT` stamp (`fauna-build-commit`) | `status.md` § Build |
//!
//! What it owns: the shape ([`StatusSnapshot`]), the node read, the build
//! stamp's semantics ([`BuildLeg::from_stamp`], [`BUILD_SHA_ABBREV`]) and the
//! **text projection** ([`StatusText`], [`render`]) — so the seven element
//! texts are computed once and an e2e witness reads the same bare value on
//! every app. An app fetches the legs on its own cadence and paints; it derives
//! nothing (`status.md` § Don't do these: "Don't render Status data per-app").
//!
//! # The one rule every leg follows
//!
//! **`None` is not rendered.** A leg that has not loaded — or does not apply
//! on this app (no local agent, no build stamp) — produces `None` in
//! [`StatusText`], and the app paints no element for it: no placeholder, no
//! zero. That is the un-hydrated-paint rule the quota and feature-limits
//! sections already follow, and it is what lets a witness wait for the real
//! value instead of racing a placeholder.
//!
//! Pattern: the same shape as [`fauna_client_features`](https://docs.rs/fauna-client-features)
//! — a thin `StatusClient<R: RpcRequester>`, pure derivations, wasm-clean by
//! default (the two native-only halves are opt-in features, see `Cargo.toml`).

use fauna_core::localized::LocalizedText;
use fauna_protocol::RpcRequester;
use fauna_protocol::discovery::NestInfoReply;
use serde::{Deserialize, Serialize};

/// How many hex characters of the build commit `status-build-sha` shows —
/// enough to resolve in any checkout, short enough for one line. The web app's
/// `.slice(0, 12)` is this constant.
pub const BUILD_SHA_ABBREV: usize = 12;

/// The i18n key of the `status-sync-pending` text: `{files}` is the pending
/// file count, `{bytes}` the localized byte size. The count leads the string
/// so a reader takes the first token.
pub const PENDING_SUMMARY_KEY: &str = "status.sync.pending_summary";

/// The i18n key `status-sync-last` shows while no pass has ever finished —
/// the shared "Never" every app's Sync section already uses.
pub const NEVER_KEY: &str = "common.never";

/// The nest this account is on, as the nest describes itself on
/// `fauna.nest.info` — `status-node-domain` and `status-node-version`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NodeLeg {
    /// The nest's handle domain (`NestInfoReply::domain`, `"unknown"` on a
    /// never-claimed box — rendered verbatim, the nest's own claim).
    pub domain: String,
    /// `CARGO_PKG_VERSION` of the running nest.
    pub version: String,
}

impl From<NestInfoReply> for NodeLeg {
    fn from(reply: NestInfoReply) -> Self {
        Self {
            domain: reply.domain,
            version: reply.version,
        }
    }
}

/// The borrowed twin, for a caller that keeps the reply for its other legs
/// ([`StatusClient::nest_info`]).
impl From<&NestInfoReply> for NodeLeg {
    fn from(reply: &NestInfoReply) -> Self {
        Self {
            domain: reply.domain.clone(),
            version: reply.version.clone(),
        }
    }
}

/// This device's local sync backlog — the sync-agent pipe's `SyncStatusInfo`
/// fields **verbatim** (`sync-agent.md` § Local agent health owns every
/// semantic: `files_pending`/`bytes_pending` are the badge-coherent fold of
/// Uploading/Downloading files in whole-file bytes; `last_sync` is when this
/// device was last known consistent with the nest, `None` before any pass).
/// Aggregate over every bound set. `status-sync-pending` + `status-sync-last`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct SyncLeg {
    pub files_pending: u64,
    pub bytes_pending: u64,
    /// Unix **seconds**, as the pipe carries it.
    pub last_sync: Option<u64>,
}

#[cfg(feature = "agent")]
impl From<&fauna_ipc::sync::SyncStatusInfo> for SyncLeg {
    fn from(info: &fauna_ipc::sync::SyncStatusInfo) -> Self {
        Self {
            files_pending: info.files_pending,
            bytes_pending: info.bytes_pending,
            last_sync: info.last_sync,
        }
    }
}

/// The read-only MLS display — `status-mls-key-packages` and
/// `status-mls-channels`. This surface owns no key-package mutation
/// (`status.md` § Encryption / MLS key packages).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct MlsLeg {
    /// The bearer's own published, unexpired key packages —
    /// `fauna.conversations.keypackage.count` for the bearer's actor id.
    pub key_packages: u64,
    /// Secure channels open — `fauna_conversations::snapshot::secure_channel_count`
    /// (one per thread on the MLS rail, each of which is one group).
    pub channels: u64,
}

/// The commit the running artifact was built from — `status-build-sha`.
/// Constructed only through [`BuildLeg::from_stamp`], so the `dev`/empty
/// sentinels can never reach a render as a commit.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct BuildLeg {
    commit: Option<String>,
}

impl BuildLeg {
    /// From the compile-time stamp — `option_env!("FAUNA_BUILD_COMMIT")` in
    /// the binary crate, stamped by `fauna-build-commit`'s `emit()`. `None`,
    /// the empty string and `dev` (the two values the environment uses to mean
    /// "nothing was passed") all read as *unknown*.
    pub fn from_stamp(stamp: Option<&str>) -> Self {
        let commit = stamp
            .map(str::trim)
            .filter(|s| !s.is_empty() && *s != "dev")
            .map(str::to_owned);
        Self { commit }
    }

    /// The full stamp, when known.
    pub fn commit(&self) -> Option<&str> {
        self.commit.as_deref()
    }

    /// The first [`BUILD_SHA_ABBREV`] characters — what the element shows. A
    /// stamp shorter than that (the `just docker-push-dev` short-8) shows
    /// whole; an unknown build shows nothing.
    pub fn abbreviated(&self) -> Option<String> {
        self.commit
            .as_deref()
            .map(|sha| sha.chars().take(BUILD_SHA_ABBREV).collect())
    }
}

/// The whole snapshot. Every `Option` means *not loaded, or not applicable on
/// this app*; [`render`] turns each into an absent element.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct StatusSnapshot {
    pub node: Option<NodeLeg>,
    pub sync: Option<SyncLeg>,
    pub mls: Option<MlsLeg>,
    pub build: BuildLeg,
}

/// The node leg's typed read, generic over the WS-RPC transport
/// (`R: RpcRequester`) — native call sites pass `Arc<NestClient>`, the wasm
/// SPA its `WsRpcClient`.
pub struct StatusClient<R: RpcRequester> {
    nest: R,
}

impl<R: RpcRequester> StatusClient<R> {
    pub fn new(nest: R) -> Self {
        Self { nest }
    }

    /// `fauna.nest.info` → [`NodeLeg`]. Pure read, no parameters.
    pub async fn node(&self) -> Result<NodeLeg, R::Error> {
        Ok(self.nest_info().await?.into())
    }

    /// The whole `fauna.nest.info` reply, for a surface load whose other legs
    /// want it too: the node leg (`NodeLeg::from(&reply)`) and the
    /// feature-limits legs (`FeaturesClient::rows_with` /
    /// `authored_surface_with` over `reply.capabilities`) share one read
    /// instead of drawing one each on the actor's pre-identity discovery
    /// budget (`anonymous_rate_limit::DISCOVERY_MAX_EVENTS`, 60 / 60 s).
    ///
    /// Read through [`fauna_protocol::read_nest_info`]: on a transport that
    /// keeps the per-connection memo (native `NestClient`, wasm `WsRpcClient`)
    /// the reply is read once per connection, whichever reader asks first.
    pub async fn nest_info(&self) -> Result<NestInfoReply, R::Error> {
        fauna_protocol::read_nest_info(&self.nest).await
    }
}

/// The seven `status-*` element texts. `None` = **do not render the element**
/// (the leg is not loaded or does not apply); `Some` is the bare value — the
/// human label beside it is the app's, from the shared `status.*` / `common.*`
/// strings, and never part of the element text.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize)]
pub struct StatusText {
    pub node_domain: Option<String>,
    pub node_version: Option<String>,
    pub sync_pending: Option<String>,
    pub sync_last: Option<String>,
    pub mls_key_packages: Option<String>,
    pub mls_channels: Option<String>,
    pub build_sha: Option<String>,
}

/// `status-sync-pending`'s text: the shared [`PENDING_SUMMARY_KEY`] over the
/// file count and `fauna_core::format::byte_size`, resolved against the
/// caller's own i18n lookup.
pub fn sync_pending_text<F, S>(leg: &SyncLeg, lookup: F) -> String
where
    F: Fn(&str) -> Option<S>,
    S: AsRef<str>,
{
    let bytes = fauna_core::format::byte_size(leg.bytes_pending).resolve(&lookup);
    LocalizedText::key_args(
        PENDING_SUMMARY_KEY,
        [("files", leg.files_pending.to_string()), ("bytes", bytes)],
    )
    .resolve(&lookup)
}

/// `status-sync-last`'s text: the shared "Never" while no pass has finished,
/// else the shared relative time of `last_sync` (`now_ms` injected, so the
/// mapping is pure and unit-pinned — the windows `LastSyncText` and linux
/// `last_sync_text` shape, now in one place).
#[cfg(feature = "local-clock")]
pub fn sync_last_text<F, S>(leg: &SyncLeg, now_ms: i64, lookup: F) -> String
where
    F: Fn(&str) -> Option<S>,
    S: AsRef<str>,
{
    match leg.last_sync {
        Some(secs) => {
            let then_ms = i64::try_from(secs)
                .unwrap_or(i64::MAX)
                .saturating_mul(1_000);
            fauna_core::format::relative_time_text(now_ms, then_ms, &lookup)
        }
        None => LocalizedText::key(NEVER_KEY).resolve(&lookup),
    }
}

/// The clock-free projection — every text except `status-sync-last` (the one
/// that reads "how long ago", so needs `now`), which stays `None`. What a wasm
/// caller takes: it has no OS clock behind `local-clock` and no local sync
/// agent, so its snapshot carries `sync: None` and nothing is missing.
pub fn render_static<F, S>(snapshot: &StatusSnapshot, lookup: F) -> StatusText
where
    F: Fn(&str) -> Option<S>,
    S: AsRef<str>,
{
    StatusText {
        node_domain: snapshot.node.as_ref().map(|n| n.domain.clone()),
        node_version: snapshot.node.as_ref().map(|n| n.version.clone()),
        sync_pending: snapshot
            .sync
            .as_ref()
            .map(|leg| sync_pending_text(leg, &lookup)),
        sync_last: None,
        mls_key_packages: snapshot.mls.map(|m| m.key_packages.to_string()),
        mls_channels: snapshot.mls.map(|m| m.channels.to_string()),
        build_sha: snapshot.build.abbreviated(),
    }
}

/// The whole projection in one call — what a native Rust app paints from:
/// [`render_static`] plus the relative `status-sync-last`.
#[cfg(feature = "local-clock")]
pub fn render<F, S>(snapshot: &StatusSnapshot, now_ms: i64, lookup: F) -> StatusText
where
    F: Fn(&str) -> Option<S>,
    S: AsRef<str>,
{
    StatusText {
        sync_last: snapshot
            .sync
            .as_ref()
            .map(|leg| sync_last_text(leg, now_ms, &lookup)),
        ..render_static(snapshot, &lookup)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fauna_client_testkit::{RecordingRequester, block_on};
    use fauna_i18n::strings::lookup;
    use fauna_protocol::discovery::NestInfoRequest;

    const FULL_SHA: &str = "4c1b9f52624c1b9f52624c1b9f52624c1b9f5262";

    fn reply(kind: &'static str) -> Vec<u8> {
        match kind {
            "fauna.nest.info" => fauna_protocol::encode_canonical(&NestInfoReply {
                domain: "nest.example".into(),
                version: "0.1.2".into(),
                ..Default::default()
            }),
            other => panic!("RecordingRequester: unhandled kind {other}"),
        }
        .expect("encode reply")
        .to_vec()
    }

    #[test]
    fn node_reads_fauna_nest_info_and_keeps_domain_and_version() {
        let nest = RecordingRequester::new(reply);
        let leg = block_on(StatusClient::new(&nest).node()).unwrap();
        assert_eq!(
            leg,
            NodeLeg {
                domain: "nest.example".into(),
                version: "0.1.2".into(),
            }
        );
        let (kind, payload) = nest.recorded();
        assert_eq!(kind, "fauna.nest.info");
        let sent: NestInfoRequest = fauna_protocol::decode_strict(&payload).unwrap();
        assert_eq!(sent, NestInfoRequest::default());
    }

    #[test]
    fn nest_info_is_one_read_the_node_leg_projects_from() {
        let nest = RecordingRequester::new(reply);
        let info = block_on(StatusClient::new(&nest).nest_info()).unwrap();
        assert_eq!(nest.kinds(), vec!["fauna.nest.info"]);
        assert_eq!(
            NodeLeg::from(&info),
            NodeLeg {
                domain: "nest.example".into(),
                version: "0.1.2".into(),
            }
        );
    }

    #[test]
    fn the_unstamped_sentinels_are_an_unknown_build() {
        for stamp in [None, Some(""), Some("dev"), Some(" dev ")] {
            let leg = BuildLeg::from_stamp(stamp);
            assert_eq!(leg.commit(), None, "{stamp:?}");
            assert_eq!(leg.abbreviated(), None, "{stamp:?}");
        }
    }

    #[test]
    fn a_full_commit_abbreviates_to_twelve_and_a_short_one_shows_whole() {
        let full = BuildLeg::from_stamp(Some(FULL_SHA));
        assert_eq!(full.commit(), Some(FULL_SHA));
        assert_eq!(full.abbreviated().as_deref(), Some("4c1b9f52624c"));
        let short = BuildLeg::from_stamp(Some("4c1b9f52"));
        assert_eq!(short.abbreviated().as_deref(), Some("4c1b9f52"));
    }

    #[test]
    fn pending_text_leads_with_the_file_count() {
        let idle = sync_pending_text(&SyncLeg::default(), lookup);
        assert_eq!(idle, "0 files, 0 B");
        assert_eq!(idle.split_whitespace().next(), Some("0"));
        let busy = sync_pending_text(
            &SyncLeg {
                files_pending: 3,
                bytes_pending: 2_684_354_560,
                last_sync: None,
            },
            lookup,
        );
        assert_eq!(busy, "3 files, 2.5 GB");
    }

    #[test]
    fn last_sync_is_never_until_a_pass_finished_then_relative() {
        let now_ms = 1_700_000_000_000;
        let never = sync_last_text(&SyncLeg::default(), now_ms, lookup);
        assert_eq!(never, fauna_i18n::strings::common::NEVER);
        let two_minutes_ago = SyncLeg {
            last_sync: Some(1_700_000_000 - 120),
            ..Default::default()
        };
        let text = sync_last_text(&two_minutes_ago, now_ms, lookup);
        assert_eq!(
            text,
            fauna_core::format::relative_time_text(now_ms, (1_700_000_000 - 120) * 1_000, lookup)
        );
        assert_ne!(text, never);
        assert!(!text.is_empty());
    }

    #[test]
    fn the_clock_free_projection_is_the_full_one_minus_sync_last() {
        let snapshot = StatusSnapshot {
            node: Some(NodeLeg {
                domain: "nest.example".into(),
                version: "0.1.2".into(),
            }),
            sync: Some(SyncLeg::default()),
            mls: Some(MlsLeg {
                key_packages: 20,
                channels: 2,
            }),
            build: BuildLeg::from_stamp(Some(FULL_SHA)),
        };
        let full = render(&snapshot, 1_700_000_000_000, lookup);
        let clock_free = render_static(&snapshot, lookup);
        assert_eq!(clock_free.sync_last, None);
        assert_eq!(
            clock_free,
            StatusText {
                sync_last: None,
                ..full
            }
        );
    }

    #[test]
    fn nothing_loaded_renders_nothing() {
        assert_eq!(
            render(&StatusSnapshot::default(), 0, lookup),
            StatusText::default()
        );
    }

    #[test]
    fn every_loaded_leg_renders_its_bare_values() {
        let snapshot = StatusSnapshot {
            node: Some(NodeLeg {
                domain: "nest.example".into(),
                version: "0.1.2".into(),
            }),
            sync: Some(SyncLeg::default()),
            mls: Some(MlsLeg {
                key_packages: 20,
                channels: 2,
            }),
            build: BuildLeg::from_stamp(Some(FULL_SHA)),
        };
        let text = render(&snapshot, 1_700_000_000_000, lookup);
        assert_eq!(text.node_domain.as_deref(), Some("nest.example"));
        assert_eq!(text.node_version.as_deref(), Some("0.1.2"));
        assert_eq!(text.sync_pending.as_deref(), Some("0 files, 0 B"));
        assert_eq!(
            text.sync_last.as_deref(),
            Some(fauna_i18n::strings::common::NEVER)
        );
        assert_eq!(text.mls_key_packages.as_deref(), Some("20"));
        assert_eq!(text.mls_channels.as_deref(), Some("2"));
        assert_eq!(text.build_sha.as_deref(), Some("4c1b9f52624c"));
    }

    #[test]
    fn the_sync_leg_embeds_the_pipe_projection_verbatim() {
        let info = fauna_ipc::sync::SyncStatusInfo {
            connected: true,
            syncing: true,
            files_pending: 7,
            bytes_pending: 8,
            last_sync: Some(9),
        };
        assert_eq!(
            SyncLeg::from(&info),
            SyncLeg {
                files_pending: 7,
                bytes_pending: 8,
                last_sync: Some(9),
            }
        );
    }

    /// A typo'd key is invisible at compile time and renders as the raw key on
    /// all 7 apps, so the keys this crate emits are pinned against the
    /// generated lookup.
    #[test]
    fn every_emitted_key_resolves() {
        for key in [PENDING_SUMMARY_KEY, NEVER_KEY] {
            assert!(
                lookup(key).is_some(),
                "{key} is not in i18n/strings/en.yaml — it would render as the raw key"
            );
        }
    }
}
