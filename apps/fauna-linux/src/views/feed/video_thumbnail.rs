//! `video-thumbnail` — the feed's inline player host (`render-model.md` § D6c →
//! *Inline playback*, answer (1)): a tap asks the shared
//! [`FeedManager::playback_source`](fauna_feed::FeedManager::playback_source)
//! what plays, and swaps the play glyph for GTK4's built-in, GStreamer-backed
//! `gtk::Video` in the same element. Never autoplay — the tap is what spends
//! the bytes; no new dependency — `gtk::Video` is GTK's own.
//!
//! **What the player reads.** A `Url` source is never handed to the player as a
//! URL (`security.md` § Transport trust → *The native-player leg*): GStreamer's
//! HTTP source trusts the OS store alone and refuses every nest on its
//! self-signed floor. It reads [`RangedNestStream`] — the shared pinned
//! `RangedNestReader`, consumed directly from Rust — through
//! `gtk::MediaFile::for_input_stream`. A `Sealed` source (a video of a post this
//! reader unlocked) is fetched whole and opened through the same
//! `open_media_bytes` leg a sealed `post-image` takes, and plays from memory.
//!
//! **What the element publishes** (`drivers/base.py::get_attr`): `state` ∈
//! `idle` / `loading` / `playing` / `error` as a `test-attr-state-*` class,
//! `source` (the nest-relative path the reader was opened on, `sealed:<hash>`
//! for opened bytes, `""` before a tap) and `position` (`"0.000"` until a player
//! stands in the slot; the automation agent then reads the live stream's
//! [`played_position_us`] off the `gtk::Video` child — an ended stream's whole
//! length, never the 0 GTK resets an ended stream's timestamp to).
//!
//! **The player outlives the card.** The post list rebuilds every card on every
//! manager notify, so a player owned by its card would snap back to idle on the
//! next unrelated update. The one live player — a second tap elsewhere stops the
//! first — is kept here on the GTK thread, keyed by its post and its video (the
//! hash, or a bridged video's path), and every `video-thumbnail` built for that
//! key adopts it: the list card and the detail pane show the same stream, while
//! another post carrying the same bytes stays idle until it is tapped itself.
//!
//! **The player feeds the cue probe.** Every `timestamp` / `ended` report the
//! stream makes is noted, per-mille through the shared
//! [`played_per_mille`], into the player's unread peak; the feed's cue probe
//! takes it for the post's row each sample ([`take_played_pm`], the contract on
//! [`fauna_feed::CueRow::media_played_pm`]), which is what lets a video played
//! to the end derive the media `watch-complete` (`engagement-cues.md` § Cue
//! vocabulary & derivation).

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::sync::Arc;

use fauna_client::RangedNestReader;
use fauna_core::load_cache::Finished;
use fauna_core::render::RenderBlock;
use fauna_core::scoring::cues::played_per_mille;
use fauna_feed::PlaybackSource;
use fauna_ui_ids as ids;
use gtk::prelude::*;
use gtk::{gio, glib};

use crate::client::FaunaClient;
use crate::feed::host::LinuxFeedManager;
use crate::feed::ranged_stream::RangedNestStream;
use crate::nest_content_api::ApiError;

/// What a tap resolved to: the `source` to publish and the stream the player
/// reads, or why nothing plays (a diagnostic for the log, never shown).
pub(crate) type Resolved = Result<(String, gio::InputStream), String>;

/// Where a tap's playback source comes from — the shared projection plus the
/// reader in the app, a controllable fake in the tests.
pub(crate) trait PlaybackResolver {
    fn resolve(&self, block: RenderBlock) -> async_channel::Receiver<Resolved>;
}

/// The app's resolver: the shared `playback_source` projection, then the
/// pinned ranged reader (`Url`) or the sealed-media open (`Sealed`).
pub(crate) struct LivePlayback {
    pub manager: Arc<LinuxFeedManager>,
    pub client: Rc<FaunaClient>,
}

/// The off-main-thread half of a resolve: everything that awaits the network.
enum Fetched {
    Reader {
        path: String,
        reader: RangedNestReader,
    },
    Sealed {
        hash: String,
    },
    Failed(String),
}

impl PlaybackResolver for LivePlayback {
    fn resolve(&self, block: RenderBlock) -> async_channel::Receiver<Resolved> {
        let (tx, rx) = async_channel::bounded(1);
        let (fetched_tx, fetched_rx) = async_channel::bounded(1);
        let manager = Arc::clone(&self.manager);
        let rt = self.client.runtime_handle();
        // The string the WS login minted on, so the reader's pinned client
        // keys the same SPKI pin the content API's does.
        let nest_url = self.client.node_url().to_string();
        rt.spawn(async move {
            let fetched = match manager.playback_source(&block).await {
                PlaybackSource::Url { url } => {
                    match RangedNestReader::open(&nest_url, &url).await {
                        Ok(reader) => Fetched::Reader { path: url, reader },
                        Err(e) => Fetched::Failed(format!("ranged reader open {url}: {e}")),
                    }
                }
                PlaybackSource::Sealed { hash } => Fetched::Sealed { hash },
                PlaybackSource::Unplayable { reason } => Fetched::Failed(reason),
            };
            let _ = fetched_tx.send(fetched).await;
        });
        let manager = Arc::clone(&self.manager);
        let client = Rc::clone(&self.client);
        glib::spawn_future_local(async move {
            let resolved = match fetched_rx.recv().await {
                Ok(Fetched::Reader { path, reader }) => {
                    let stream = RangedNestStream::new(Arc::new(reader), client.runtime_handle());
                    Ok((path, stream.upcast()))
                }
                Ok(Fetched::Sealed { hash }) => {
                    let fetched = client
                        .fetch_blob_bytes(&hash)
                        .recv()
                        .await
                        .unwrap_or_else(|e| Err(ApiError::Transport(e.to_string())));
                    // The same open a sealed `post-image` takes: the bytes reach
                    // the player opened, never as ciphertext.
                    match crate::feed::post_media::open_fetched_post_image(&manager, &hash, fetched)
                    {
                        Finished::Loaded(bytes) => {
                            let stream =
                                gio::MemoryInputStream::from_bytes(&glib::Bytes::from_owned(bytes));
                            Ok((format!("sealed:{hash}"), stream.upcast()))
                        }
                        Finished::Failed | Finished::Transient => {
                            Err(format!("sealed video {hash} did not open"))
                        }
                    }
                }
                Ok(Fetched::Failed(reason)) => Err(reason),
                Err(e) => Err(format!("playback resolve dropped: {e}")),
            };
            // A fresh one-slot channel: the send never waits.
            let _ = tx.try_send(resolved);
        });
        rx
    }
}

/// The element's published phase.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Phase {
    Idle,
    Loading,
    Playing,
    Error,
}

impl Phase {
    fn as_str(self) -> &'static str {
        match self {
            Phase::Idle => "idle",
            Phase::Loading => "loading",
            Phase::Playing => "playing",
            Phase::Error => "error",
        }
    }
}

/// The one live player. See the module docs.
struct Player {
    key: Key,
    phase: Phase,
    source: String,
    media: Option<gtk::MediaStream>,
    generation: u64,
    /// The highest per-mille the stream reported since the cue probe last took
    /// it ([`take_played_pm`]); `None` when it has made no progress since.
    unread_pm: Option<u32>,
}

thread_local! {
    static PLAYER: RefCell<Option<Player>> = const { RefCell::new(None) };
    static HOSTS: RefCell<Vec<(Key, glib::WeakRef<gtk::Box>)>> = const { RefCell::new(Vec::new()) };
    static GENERATION: Cell<u64> = const { Cell::new(0) };
}

/// The key a video is played and adopted by: the post it sits on and the
/// video itself. Two posts may carry the same bytes (one content hash); a tap
/// on one plays in that post's element only (§ D6c → *Inline playback*).
#[derive(Clone, PartialEq, Eq)]
struct Key {
    post: String,
    video: String,
}

impl Key {
    fn of(post_id: &str, block: &RenderBlock) -> Self {
        Key {
            post: post_id.to_string(),
            video: video_of(block).to_string(),
        }
    }
}

/// The label the idle element paints: the hash, or a bridged video's
/// nest-relative path.
fn video_of(block: &RenderBlock) -> &str {
    match block {
        RenderBlock::Video { hash, .. } => hash,
        RenderBlock::ProxiedVideo { path, .. } => path,
        _ => "",
    }
}

/// Build a `video-thumbnail` for a `Video` or `ProxiedVideo` block (render-model.md
/// § D6b / § D6c → *Proxied video*): the play glyph + the hash (or path) until a
/// tap, the player after. No poster frame exists to paint — `MediaItem`'s
/// `thumbnail` is `None` from every writer — and nothing is fetched before the
/// tap.
pub(crate) fn build_post_video(
    post_id: &str,
    block: RenderBlock,
    manager: &Arc<LinuxFeedManager>,
    client: &Rc<FaunaClient>,
) -> gtk::Box {
    build_post_video_from(
        post_id,
        block,
        Rc::new(LivePlayback {
            manager: Arc::clone(manager),
            client: Rc::clone(client),
        }),
    )
}

pub(crate) fn build_post_video_from(
    post_id: &str,
    block: RenderBlock,
    resolver: Rc<dyn PlaybackResolver>,
) -> gtk::Box {
    let host = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    host.set_halign(gtk::Align::Start);
    host.set_margin_top(4);
    crate::testid::set_test_id(&host, ids::VIDEO_THUMBNAIL);
    let key = Key::of(post_id, &block);

    HOSTS.with_borrow_mut(|hosts| {
        hosts.retain(|(_, weak)| weak.upgrade().is_some());
        hosts.push((key.clone(), host.downgrade()));
    });
    paint_host(&host, &key);

    let gesture = gtk::GestureClick::new();
    gesture.connect_released(move |_, _, _, _| tap(key.clone(), block.clone(), &resolver));
    host.add_controller(gesture);
    host
}

/// A tap: start this video, stopping whatever else plays. A tap on the video
/// already loading or playing is the player's own (its controls), not a restart.
fn tap(key: Key, block: RenderBlock, resolver: &Rc<dyn PlaybackResolver>) {
    let busy = PLAYER.with_borrow(|p| {
        p.as_ref()
            .is_some_and(|p| p.key == key && matches!(p.phase, Phase::Loading | Phase::Playing))
    });
    if busy {
        return;
    }
    let generation = GENERATION.with(|g| {
        g.set(g.get() + 1);
        g.get()
    });
    let previous = PLAYER.replace(Some(Player {
        key: key.clone(),
        phase: Phase::Loading,
        source: String::new(),
        media: None,
        generation,
        unread_pm: None,
    }));
    if let Some(previous) = previous {
        if let Some(media) = &previous.media {
            media.pause();
        }
        if previous.key != key {
            repaint(&previous.key);
        }
    }
    repaint(&key);

    let rx = resolver.resolve(block);
    glib::spawn_future_local(async move {
        let resolved = rx
            .recv()
            .await
            .unwrap_or_else(|e| Err(format!("playback resolve dropped: {e}")));
        if !is_current(generation) {
            return;
        }
        match resolved {
            Ok((source, stream)) => {
                let media: gtk::MediaStream = gtk::MediaFile::for_input_stream(&stream).upcast();
                for property in ["prepared", "playing", "error"] {
                    media.connect_notify_local(Some(property), move |media, _| {
                        settle(generation, media);
                    });
                }
                for property in ["timestamp", "ended"] {
                    media.connect_notify_local(Some(property), move |media, _| {
                        note_stream(generation, media);
                    });
                }
                with_current(generation, |p| {
                    p.source = source;
                    p.media = Some(media.clone());
                });
                media.play();
                repaint(&key);
            }
            Err(reason) => {
                tracing::warn!(target: "feed", video = %key.video, %reason, "inline playback failed");
                with_current(generation, |p| p.phase = Phase::Error);
                repaint(&key);
            }
        }
    });
}

fn is_current(generation: u64) -> bool {
    PLAYER.with_borrow(|p| p.as_ref().is_some_and(|p| p.generation == generation))
}

fn with_current(generation: u64, f: impl FnOnce(&mut Player)) {
    PLAYER.with_borrow_mut(|p| {
        if let Some(p) = p.as_mut().filter(|p| p.generation == generation) {
            f(p);
        }
    });
}

/// The stream's own report moves the phase: `playing` once it is prepared and
/// rolling, `error` on its error (the player is dropped; a tap retries).
fn settle(generation: u64, media: &gtk::MediaStream) {
    let mut key = None;
    with_current(generation, |p| {
        let phase = if let Some(error) = media.error() {
            tracing::warn!(target: "feed", video = %p.key.video, %error, "inline player error");
            p.media = None;
            Phase::Error
        } else if media.is_prepared() && media.is_playing() {
            Phase::Playing
        } else {
            p.phase
        };
        if phase != p.phase {
            p.phase = phase;
            key = Some(p.key.clone());
        }
    });
    if let Some(key) = key {
        repaint(&key);
    }
}

/// Note the stream's progress into the live player's unread peak. An ended
/// stream reads as played to its end, whatever its last timestamp said.
fn note_stream(generation: u64, media: &gtk::MediaStream) {
    note_progress(
        generation,
        played_position_us(media),
        media.duration() as f64,
    );
}

/// How far `media` has played, in µs. An ended stream reads as its whole
/// length: GTK snaps an ended stream's `timestamp` back to 0, which would read
/// a video played to its end as never started. The cue probe's notes and the
/// automation agent's `position` both read this, so they cannot disagree.
pub(crate) fn played_position_us(media: &gtk::MediaStream) -> f64 {
    played_position(media.is_ended(), media.timestamp(), media.duration())
}

fn played_position(ended: bool, timestamp: i64, duration: i64) -> f64 {
    let played = if ended { duration } else { timestamp };
    played as f64
}

fn note_progress(generation: u64, position: f64, duration: f64) {
    let Some(pm) = played_per_mille(position, duration) else {
        return;
    };
    with_current(generation, |p| {
        p.unread_pm = Some(p.unread_pm.map_or(pm, |seen| seen.max(pm)));
    });
}

/// The cue probe's read: the peak the live player reached on `post_id` since
/// the last take, cleared by the take; `None` when the post has no player or
/// it made no progress since (the contract on
/// [`fauna_feed::CueRow::media_played_pm`]).
pub(crate) fn take_played_pm(post_id: &str) -> Option<u32> {
    PLAYER.with_borrow_mut(|p| {
        p.as_mut()
            .filter(|p| p.key.post == post_id)
            .and_then(|p| p.unread_pm.take())
    })
}

/// Repaint every live `video-thumbnail` built for `key`.
fn repaint(key: &Key) {
    let hosts: Vec<gtk::Box> = HOSTS.with_borrow(|hosts| {
        hosts
            .iter()
            .filter(|(k, _)| k == key)
            .filter_map(|(_, weak)| weak.upgrade())
            .collect()
    });
    for host in hosts {
        paint_host(&host, key);
    }
}

/// Paint `host` from the live player when it plays `key`, else idle.
fn paint_host(host: &gtk::Box, key: &Key) {
    let (phase, source, media) =
        PLAYER.with_borrow(|p| match p.as_ref().filter(|p| &p.key == key) {
            Some(p) => (p.phase, p.source.clone(), p.media.clone()),
            None => (Phase::Idle, String::new(), None),
        });
    crate::testid::set_test_attr(host, "state", phase.as_str());
    crate::testid::set_test_attr_text(host, "source", &source);
    crate::testid::set_test_attr_text(host, "position", "0.000");

    let shows = host
        .first_child()
        .and_downcast::<gtk::Video>()
        .and_then(|v| v.media_stream());
    match media {
        Some(media) if shows.as_ref() == Some(&media) => {}
        Some(media) => {
            clear(host);
            let video = gtk::Video::for_media_stream(Some(&media));
            video.set_size_request(320, 180);
            host.append(&video);
        }
        None => {
            if shows.is_some() || host.first_child().is_none() {
                clear(host);
                host.append(&gtk::Image::from_icon_name("media-playback-start-symbolic"));
                let label = gtk::Label::new(Some(&key.video));
                label.add_css_class("dim-label");
                host.append(&label);
            }
        }
    }
}

fn clear(host: &gtk::Box) {
    while let Some(child) = host.first_child() {
        host.remove(&child);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A resolver whose answers the test sends, counting the taps it saw.
    #[derive(Default)]
    struct Scripted {
        pending: RefCell<Vec<async_channel::Sender<Resolved>>>,
        blocks: RefCell<Vec<RenderBlock>>,
    }

    impl PlaybackResolver for Scripted {
        fn resolve(&self, block: RenderBlock) -> async_channel::Receiver<Resolved> {
            let (tx, rx) = async_channel::bounded(1);
            self.pending.borrow_mut().push(tx);
            self.blocks.borrow_mut().push(block);
            rx
        }
    }

    fn state(host: &gtk::Box) -> Option<String> {
        host.css_classes()
            .iter()
            .find_map(|c| c.strip_prefix("test-attr-state-").map(str::to_string))
    }

    fn press(host: &gtk::Box) {
        let controllers = host.observe_controllers();
        let gesture = (0..controllers.n_items())
            .find_map(|i| controllers.item(i).and_downcast::<gtk::GestureClick>())
            .expect("video-thumbnail carries its tap gesture");
        gesture.emit_by_name::<()>("pressed", &[&1i32, &0.0f64, &0.0f64]);
        gesture.emit_by_name::<()>("released", &[&1i32, &0.0f64, &0.0f64]);
    }

    fn video(hash: &str) -> RenderBlock {
        RenderBlock::Video {
            hash: hash.into(),
            alt: String::new(),
        }
    }

    fn on_private_context(body: impl FnOnce(&glib::MainContext) + Send + 'static) {
        crate::testid::run_on_gtk_thread(move || {
            PLAYER.set(None);
            let ctx = glib::MainContext::new();
            ctx.with_thread_default(|| body(&ctx))
                .expect("acquire a private main context");
            PLAYER.set(None);
        });
    }

    /// Before a tap the element is idle and has asked nothing; the tap walks it
    /// to `loading` and asks the projection once for that block; a resolve
    /// that fails lands `error`, and the glyph + hash stay addressable.
    #[test]
    fn a_tap_walks_idle_loading_then_error_on_a_failed_resolve() {
        on_private_context(|ctx| {
            let resolver = Rc::new(Scripted::default());
            let host = build_post_video_from("p1", video("aa11"), resolver.clone());
            assert_eq!(state(&host).as_deref(), Some("idle"), "never autoplay");
            assert!(
                resolver.blocks.borrow().is_empty(),
                "nothing asked before the tap"
            );

            press(&host);
            assert_eq!(state(&host).as_deref(), Some("loading"));
            assert_eq!(*resolver.blocks.borrow(), vec![video("aa11")]);

            press(&host);
            assert_eq!(
                resolver.blocks.borrow().len(),
                1,
                "a tap while loading is no restart"
            );

            let tx = resolver.pending.borrow()[0].clone();
            tx.send_blocking(Err("not a video block".into())).unwrap();
            while ctx.iteration(false) {}
            assert_eq!(state(&host).as_deref(), Some("error"));
            let label = host
                .last_child()
                .and_downcast::<gtk::Label>()
                .expect("the hash label");
            assert_eq!(label.text(), "aa11");

            press(&host);
            assert_eq!(
                resolver.blocks.borrow().len(),
                2,
                "a tap after an error retries"
            );
        });
    }

    /// The post list rebuilds every card on every manager notify: a card built
    /// while its video loads adopts that phase, and the resolve's answer reaches
    /// the card standing at the time — never only the one that was tapped.
    #[test]
    fn a_rebuilt_card_adopts_the_live_player() {
        on_private_context(|ctx| {
            let resolver = Rc::new(Scripted::default());
            let tapped = build_post_video_from("p1", video("bb22"), resolver.clone());
            press(&tapped);
            drop(tapped);

            let rebuilt = build_post_video_from("p1", video("bb22"), resolver.clone());
            assert_eq!(state(&rebuilt).as_deref(), Some("loading"));
            let other = build_post_video_from("p1", video("cc33"), resolver.clone());
            assert_eq!(state(&other).as_deref(), Some("idle"));

            resolver.pending.borrow()[0]
                .send_blocking(Err("ranged reader open refused".into()))
                .unwrap();
            while ctx.iteration(false) {}
            assert_eq!(state(&rebuilt).as_deref(), Some("error"));
            assert_eq!(state(&other).as_deref(), Some("idle"));
        });
    }

    /// Two posts carrying the same bytes (one content hash) are two elements:
    /// a tap on one plays there, and the other — a reload's freshly prepended
    /// card, say — stays idle until it is tapped itself (§ D6c → *Inline
    /// playback*), while the tapped post's rebuilt card still adopts its player.
    #[test]
    fn the_same_video_on_another_post_stays_idle() {
        on_private_context(|_| {
            let resolver = Rc::new(Scripted::default());
            let tapped = build_post_video_from("p1", video("ff66"), resolver.clone());
            press(&tapped);
            let other = build_post_video_from("p2", video("ff66"), resolver.clone());
            assert_eq!(state(&other).as_deref(), Some("idle"), "never autoplay");
            let rebuilt = build_post_video_from("p1", video("ff66"), resolver.clone());
            assert_eq!(state(&rebuilt).as_deref(), Some("loading"));
        });
    }

    /// A second video's tap supersedes the first: the first card returns to
    /// idle, and the first resolve's late answer changes nothing.
    #[test]
    fn a_second_tap_supersedes_the_first() {
        on_private_context(|ctx| {
            let resolver = Rc::new(Scripted::default());
            let first = build_post_video_from("p1", video("dd44"), resolver.clone());
            let second = build_post_video_from("p1", video("ee55"), resolver.clone());
            press(&first);
            press(&second);
            assert_eq!(state(&first).as_deref(), Some("idle"));
            assert_eq!(state(&second).as_deref(), Some("loading"));

            resolver.pending.borrow()[0]
                .send_blocking(Err("late".into()))
                .unwrap();
            while ctx.iteration(false) {}
            assert_eq!(state(&first).as_deref(), Some("idle"));
            assert_eq!(state(&second).as_deref(), Some("loading"));
        });
    }

    /// The cue probe takes the peak the player reported since its last take —
    /// an ended stream as the whole play — and nothing for a post the player
    /// is not on; a second take with no progress between is no reading.
    #[test]
    fn the_cue_probe_takes_the_peak_since_its_last_take() {
        on_private_context(|_| {
            let resolver = Rc::new(Scripted::default());
            let host = build_post_video_from("p1", video("a1b2"), resolver.clone());
            press(&host);
            let generation = GENERATION.with(Cell::get);
            assert_eq!(take_played_pm("p1"), None, "nothing played yet");

            note_progress(generation, 0.0, 0.0);
            assert_eq!(
                take_played_pm("p1"),
                None,
                "a preparing stream reads nothing"
            );
            note_progress(generation, 3_000_000.0, 5_000_000.0);
            note_progress(generation, 1_000_000.0, 5_000_000.0);
            assert_eq!(take_played_pm("p2"), None, "another post has no player");
            assert_eq!(take_played_pm("p1"), Some(600), "the peak, not the latest");
            assert_eq!(take_played_pm("p1"), None, "taken once");

            note_progress(generation, 5_000_000.0, 5_000_000.0);
            note_progress(generation + 1, 2_000_000.0, 5_000_000.0);
            assert_eq!(
                take_played_pm("p1"),
                Some(1000),
                "a stale player notes nothing"
            );
        });
    }

    /// A stream that played to its end reads its whole length, whatever its
    /// timestamp: GTK resets an ended stream's timestamp to 0.
    #[test]
    fn an_ended_stream_reads_as_played_to_its_end() {
        assert_eq!(played_position(false, 2_500_000, 5_000_000), 2_500_000.0);
        assert_eq!(played_position(true, 0, 5_000_000), 5_000_000.0);
    }

    /// A bridged post's `ProxiedVideo` paints its nest-relative path in the
    /// same slot (render-model.md § D6c → *Proxied video*) and is played by it.
    #[test]
    fn a_bridged_proxied_video_paints_its_path_and_plays_by_it() {
        on_private_context(|_| {
            let path = "/api/v1/media/proxy?url=https%3A%2F%2Fcdn.example%2Fclip.mp4";
            let resolver = Rc::new(Scripted::default());
            let block = RenderBlock::ProxiedVideo {
                path: path.into(),
                alt: String::new(),
            };
            let host = build_post_video_from("p1", block.clone(), resolver.clone());
            assert!(
                host.first_child().and_downcast::<gtk::Image>().is_some(),
                "the play glyph comes first"
            );
            let label = host
                .last_child()
                .and_downcast::<gtk::Label>()
                .expect("the path label");
            assert_eq!(label.text(), path);
            press(&host);
            assert_eq!(*resolver.blocks.borrow(), vec![block]);
        });
    }
}
