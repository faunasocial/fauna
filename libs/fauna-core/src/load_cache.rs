//! A per-key **load-state cache** — the bookkeeping every app page that paints
//! fetched bytes hangs off: a post image or link-preview og:image keyed by
//! content hash, a Media thumbnail, a revealed remote image keyed by url, a
//! C2PA provenance verdict.
//!
//! **Why a cache at all.** An app page is rebuilt far more often than its
//! content changes — tui's element list on every frame, linux's feed cards on
//! every snapshot notification (`docs/goal/architecture/apps/linux.md`
//! § Message Flow: a repaint re-reads the whole snapshot) — so a fetch driven
//! from the element builder would re-issue every image's GET, and re-decode its
//! bytes, on every repaint. The cache is what makes rebuilding affordable: a key
//! is looked up, not refetched, once its state is recorded. Keying by content
//! hash makes it correct as well as cheap: a hash *is* the bytes, so an entry
//! can never go stale, and the same blob referenced twice is fetched once.
//!
//! **What it is NOT.** It owns no async machinery and no payload type. Each app
//! keeps its own byte source (tui's `Op`s, linux's `FaunaClient` channels), its
//! own decode (tui rasterizes to terminal art, linux decodes a `gdk::Texture`)
//! and its own delivery (tui's immediate-mode frame reads the cache, linux's
//! retained widgets queue for the result) and folds the finished payload in
//! through [`LoadCache::set`]. Extracting the *bookkeeping* — not the fetch, not
//! the payload — is the shared surface (priority #2): the fetch and the payload
//! legitimately differ per app and per page; which key is loading, loaded or
//! hopeless does not.
//!
//! **A transient failure is forgotten, not recorded.** [`LoadState::Failed`]
//! is terminal so a rebuild cannot re-issue a hopeless fetch — but "hopeless"
//! is a property of the *bytes* (absent, refused, unopenable, undecodable), not
//! of one attempt's transport. A load that failed because the source was
//! unreachable or overloaded (a timeout, a dropped connection, a `5xx`) is
//! folded as [`Finished::Transient`], which [`LoadCache::finish`] turns into
//! *no entry at all*: the next ask starts a fresh load. Recording it as
//! `Failed` would blank that image for the rest of the session over one blip,
//! with no way back short of signing in again. The retry needs no timer of its
//! own: it rides the page's own rebuild cadence, which on both Rust apps is
//! notification-driven (tui kicks its fetches on a data tick, linux rebuilds a
//! card on a snapshot notification) — never per frame, so a nest that stays
//! down is asked again only as often as something else changed. Which errors
//! count as transient is the byte source's call (for an own-nest read,
//! `fauna_nest_http::ApiError::is_transient`); a source that cannot tell folds
//! every failure as [`Finished::Failed`] — the revealed third-party image does,
//! since its shared fetch collapses every refusal into one "no picture" and a
//! hostile host could otherwise make every refusal look retryable and turn the
//! reader into a repeat caller.
//!
//! **Scope is the caller's.** A cache lives exactly as long as the state it
//! hangs off. An app whose payload can be a post-media item OPENED under a key
//! the signed-in reader holds must drop the cache with that reader's session, so
//! the next account never paints bytes it could not have opened itself.

use std::collections::HashMap;

/// One key's load state in a [`LoadCache`].
///
/// [`Failed`](Self::Failed) is a **terminal** state, not an absence: it is what
/// stops a rebuild from re-issuing a hopeless fetch every time. Every non-ready
/// state paints the same placeholder — the user sees "no picture", never an
/// error, because one unreadable image must not blank the row or raise the page
/// banner.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LoadState<T> {
    /// A load is in flight — do not start a second one.
    Loading,
    /// Loaded, and turned into whatever the app paints.
    Ready(T),
    /// The bytes could not be had — refused, unopenable or undecodable; never
    /// retried while this cache lives. (A *transient* failure never lands here:
    /// [`Finished::Transient`] forgets the entry instead.)
    Failed,
}

/// How one finished load folds into a [`LoadCache`] ([`LoadCache::finish`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Finished<T> {
    /// Loaded, and turned into whatever the app paints → [`LoadState::Ready`].
    Loaded(T),
    /// The bytes are absent, refused, unopenable or undecodable — asking again
    /// would fail again → the terminal [`LoadState::Failed`].
    Failed,
    /// The attempt failed on the way (source unreachable, timed out,
    /// overloaded), saying nothing about the bytes → the entry is forgotten, so
    /// the next ask starts a fresh load (the module doc says why).
    Transient,
}

impl<T> From<Option<T>> for Finished<T> {
    /// A source that cannot tell a transient failure from a hopeless one:
    /// `None` is [`Finished::Failed`].
    fn from(value: Option<T>) -> Self {
        match value {
            Some(value) => Self::Loaded(value),
            None => Self::Failed,
        }
    }
}

/// A `key → `[`LoadState`] cache. Bounded by what the user has browsed while it
/// lives; dropped with whatever owns it.
#[derive(Debug, Clone)]
pub struct LoadCache<T>(HashMap<String, LoadState<T>>);

// Hand-written, not derived: `#[derive(Default)]` would demand `T: Default`,
// which a decoded image type has no reason to implement.
impl<T> Default for LoadCache<T> {
    fn default() -> Self {
        Self(HashMap::new())
    }
}

impl<T> LoadCache<T> {
    /// An empty cache.
    pub fn new() -> Self {
        Self::default()
    }

    /// The recorded state for `key`, if a load has been started or finished.
    pub fn get(&self, key: &str) -> Option<&LoadState<T>> {
        self.0.get(key)
    }

    /// The loaded payload for `key` — `None` while absent, loading or failed,
    /// which is exactly when a page paints its placeholder.
    pub fn ready(&self, key: &str) -> Option<&T> {
        match self.0.get(key) {
            Some(LoadState::Ready(value)) => Some(value),
            _ => None,
        }
    }

    /// Whether `key` has *any* recorded state (loading, ready, or failed).
    /// Terminal states included on purpose: a `Failed` entry must suppress a
    /// re-fetch exactly as a `Ready` one does.
    pub fn contains(&self, key: &str) -> bool {
        self.0.contains_key(key)
    }

    /// The fetch gate, in one step: when `key` has no recorded state, mark it
    /// [`Loading`](LoadState::Loading) and return `true` — the caller must start
    /// the load. Otherwise return `false` and change nothing.
    ///
    /// Marking in the same call that answers is what makes a batch dedupe
    /// *within* one pass as well as across passes: `keys.retain(|k|
    /// cache.begin(k))` keeps the first of two posts sharing an og:image and
    /// drops the second.
    pub fn begin(&mut self, key: &str) -> bool {
        if self.0.contains_key(key) {
            return false;
        }
        self.0.insert(key.to_string(), LoadState::Loading);
        true
    }

    /// Record that a load for `key` has been started, unconditionally.
    /// [`begin`](Self::begin) is the gate callers want; this is for a caller
    /// that has already decided.
    pub fn mark_loading(&mut self, key: String) {
        self.0.insert(key, LoadState::Loading);
    }

    /// Fold a finished load: `Some(value)` → [`LoadState::Ready`], `None` →
    /// [`LoadState::Failed`] (the terminal "unreadable, don't retry" state).
    /// The form for a source that cannot tell a transient failure apart;
    /// [`finish`](Self::finish) is the one that can forget.
    pub fn set(&mut self, key: String, value: Option<T>) {
        self.finish(key, value.into());
    }

    /// Fold a finished load: [`Finished::Loaded`] → [`LoadState::Ready`],
    /// [`Finished::Failed`] → [`LoadState::Failed`], [`Finished::Transient`] →
    /// no entry, so the next [`begin`](Self::begin) opens the gate again.
    pub fn finish(&mut self, key: String, outcome: Finished<T>) {
        match outcome {
            Finished::Loaded(value) => {
                self.0.insert(key, LoadState::Ready(value));
            }
            Finished::Failed => {
                self.0.insert(key, LoadState::Failed);
            }
            Finished::Transient => {
                self.0.remove(&key);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_fresh_cache_has_no_entry_and_the_gate_is_open() {
        let cache: LoadCache<u8> = LoadCache::new();
        assert!(!cache.contains("h"));
        assert!(cache.get("h").is_none());
        assert!(cache.ready("h").is_none());
    }

    #[test]
    fn begin_opens_once_then_closes_the_gate_before_bytes_arrive() {
        let mut cache: LoadCache<u8> = LoadCache::new();
        assert!(cache.begin("h"), "the first ask must start the load");
        assert!(!cache.begin("h"), "a second ask while loading must not");
        assert_eq!(cache.get("h"), Some(&LoadState::Loading));
        assert!(cache.ready("h").is_none());
    }

    #[test]
    fn begin_dedupes_within_one_batch() {
        let mut cache: LoadCache<u8> = LoadCache::new();
        let mut keys = vec!["a".to_string(), "b".to_string(), "a".to_string()];
        keys.retain(|k| cache.begin(k));
        assert_eq!(keys, ["a", "b"]);
    }

    #[test]
    fn set_some_is_ready_and_keeps_the_gate_closed() {
        let mut cache = LoadCache::new();
        assert!(cache.begin("h"));
        cache.set("h".into(), Some(7u8));
        assert_eq!(cache.ready("h"), Some(&7));
        assert!(
            !cache.begin("h"),
            "a loaded key must never be fetched again"
        );
    }

    #[test]
    fn set_none_is_terminal_failed_and_still_suppresses_a_refetch() {
        let mut cache: LoadCache<u8> = LoadCache::new();
        cache.set("h".into(), None);
        assert_eq!(cache.get("h"), Some(&LoadState::Failed));
        assert!(cache.ready("h").is_none());
        // A `Failed` entry gates a refetch exactly as `Ready` would — the whole
        // point of it being terminal rather than an absence.
        assert!(cache.contains("h"));
        assert!(!cache.begin("h"));
    }

    /// A transient failure leaves no trace: the gate reopens, so the next ask
    /// retries — where a hopeless one (above) keeps it shut for good.
    #[test]
    fn a_transient_failure_is_forgotten_and_the_next_ask_retries() {
        let mut cache: LoadCache<u8> = LoadCache::new();
        assert!(cache.begin("h"));
        cache.finish("h".into(), Finished::Transient);
        assert!(!cache.contains("h"), "a transient failure records nothing");
        assert!(cache.begin("h"), "the next ask must start a fresh load");
        cache.finish("h".into(), Finished::Loaded(7));
        assert_eq!(cache.ready("h"), Some(&7));
    }

    #[test]
    fn a_hopeless_failure_through_finish_is_terminal() {
        let mut cache: LoadCache<u8> = LoadCache::new();
        assert!(cache.begin("h"));
        cache.finish("h".into(), Finished::Failed);
        assert_eq!(cache.get("h"), Some(&LoadState::Failed));
        assert!(!cache.begin("h"));
    }

    /// The payload needs no `Default` (or any other bound) for the cache to be
    /// built — a decoded image type implements none of them.
    #[test]
    fn the_payload_needs_no_default() {
        struct Opaque;
        let mut cache: LoadCache<Opaque> = LoadCache::default();
        cache.set("h".into(), Some(Opaque));
        assert!(cache.ready("h").is_some());
    }
}
