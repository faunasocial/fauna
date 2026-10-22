//! A post's own bytes — the sealed body behind `gated_blob_hash`, its image,
//! video and C2PA probe — fetched by content address from the nest they rest on.
//!
//! That nest is this one for a post it holds, and the post's **home** for one it
//! only relays or was delivered (`feed.md` § The read model → *A post homed on
//! another nest*; `federated-follow.md` § Decision 3, *What is not stored*): the
//! home's public `GET /api/v1/blob/{hash}`, reached direct, and nothing fetches a
//! foreign post's bytes from this nest, which holds none. The host is
//! `fauna_feed::PostSummary::blob_home`; this module is what every native app
//! does with it, so the three properties of the foreign leg hold on each one
//! rather than on whichever remembered them (priority #2):
//!
//! - **no bearer** — a foreign home is reached anonymously
//!   ([`ReqwestNestContentApi::anonymous`]); this session's bearer is a
//!   credential for its own nest and must never reach another;
//! - **pinned** — the home is dialed over the pin-or-WebPKI plane every native
//!   byte leg uses ([`crate::pinned_http_client`]), and when the post carries
//!   the identity the reader's own nest stamped for its home, that pin is
//!   graduated against it once before the first byte ([`GraduatedHome`]) — so
//!   a self-signed home is reached, and no home is reached on less than the
//!   WebPKI floor (`transport-trust.md` § Mechanism for the federation-granted
//!   row → *The post arm*);
//! - **capped** — no post blob comes to rest except through a nest's inline blob
//!   door, so a reply longer than [`INLINE_BLOB_BODY_LIMIT`] is refused rather
//!   than buffered (the room-attachment leg's bound,
//!   `conversation-rooms.md` § The home nest → *The reader bounds what it
//!   fetches*);
//! - **content-addressed** — the bytes are kept only when their blake3 digest is
//!   the one the post names ([`fauna_core::encoding::content_address_matches`],
//!   the check the browser's plane shares), so a home serving other bytes under
//!   a hash paints nothing rather than a substitute.
//!
//! [`INLINE_BLOB_BODY_LIMIT`]: fauna_core::attachment_limits::INLINE_BLOB_BODY_LIMIT

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use bytes::Bytes;
use fauna_nest_http::{ApiError, NestContentApi, ReqwestNestContentApi, paths};

use crate::GraduatedHome;

/// A post's foreign home: the nest its own bytes rest on, and the identity the
/// reader's own nest stamped for it (`fauna_feed::BlobHome`, owned) — `None`
/// keeps the WebPKI floor.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ForeignHome {
    /// The home's base URL.
    pub url: String,
    /// The home's hex `nest_actor_id`, when a path delivered one.
    pub nest_actor_id: Option<String>,
}

impl ForeignHome {
    /// The home at `url`, verified against `nest_actor_id` when present.
    pub fn new(url: &str, nest_actor_id: Option<&str>) -> Self {
        Self {
            url: url.to_string(),
            nest_actor_id: nest_actor_id.map(str::to_string),
        }
    }
}

/// Hands out the content plane for one foreign home, its pin graduated first —
/// production's is [`pinned_plane`]; a test supplies a closure so no host is
/// dialled (any `Fn(&ForeignHome) -> Arc<dyn NestContentApi>` is one).
#[async_trait::async_trait]
pub trait ForeignPlanes: Send + Sync {
    /// The plane `home` serves its bytes from. A graduation that fails is an
    /// error: the caller fetches nothing, never falls back to the floor.
    async fn plane(&self, home: &ForeignHome) -> Result<Arc<dyn NestContentApi>, ApiError>;
}

#[async_trait::async_trait]
impl<F> ForeignPlanes for F
where
    F: Fn(&ForeignHome) -> Arc<dyn NestContentApi> + Send + Sync,
{
    async fn plane(&self, home: &ForeignHome) -> Result<Arc<dyn NestContentApi>, ApiError> {
        Ok(self(home))
    }
}

/// The foreign planes one session hands to [`PostBlobHosts::with_foreign`].
pub type ForeignPlane = Arc<dyn ForeignPlanes>;

/// The production [`ForeignPlane`]: per home, one pin-or-WebPKI client
/// ([`crate::pinned_http_client`] — no cookies, no bearer, no connection
/// shared with the own nest's client) and one [`GraduatedHome`], so the pin
/// graduates once per home for the plane's life. Build it once per session and
/// hand it to [`PostBlobHosts::with_foreign`].
pub fn pinned_plane() -> ForeignPlane {
    Arc::new(PinnedPlanes::default())
}

/// The HTTP client a foreign home's bytes are read over, its pin graduated
/// first — for a byte leg that reads the home's raw HTTP rather than through a
/// [`NestContentApi`] (a room's attachment read, `fauna-client-conversations`'
/// `blob_get`, which bounds the body itself). Production's is
/// [`PinnedHomes`]; a fixed [`reqwest::Client`] is one too — the test seam for
/// an in-process home, which graduates nothing.
#[async_trait::async_trait]
pub trait ForeignHomeHttp: Send + Sync {
    /// The client `home` is dialled over. A graduation that fails is an error:
    /// the caller reads nothing, never falls back to the floor.
    async fn client(&self, home: &ForeignHome) -> Result<reqwest::Client, ApiError>;
}

#[async_trait::async_trait]
impl ForeignHomeHttp for reqwest::Client {
    async fn client(&self, _home: &ForeignHome) -> Result<reqwest::Client, ApiError> {
        Ok(self.clone())
    }
}

/// A home's graduation and the client its bytes are fetched over.
struct PinnedHome {
    home: GraduatedHome,
    http: reqwest::Client,
}

/// The production [`ForeignHomeHttp`], and the state behind [`pinned_plane`]:
/// one pin-or-WebPKI client ([`crate::pinned_http_client`] — no cookies, no
/// bearer, no connection shared with the own nest's client) and one
/// [`GraduatedHome`] per (URL, identity), so a home graduates once for the
/// value's life — and a second identity for the same URL graduates against
/// its own root, never borrows the first one's.
#[derive(Default)]
pub struct PinnedHomes {
    homes: Mutex<HashMap<ForeignHome, Arc<PinnedHome>>>,
}

impl PinnedHomes {
    /// `home`'s entry, its pin graduated.
    async fn graduated(&self, home: &ForeignHome) -> Result<Arc<PinnedHome>, ApiError> {
        let pinned = {
            let mut homes = self.homes.lock().unwrap_or_else(|e| e.into_inner());
            Arc::clone(homes.entry(home.clone()).or_insert_with(|| {
                Arc::new(PinnedHome {
                    home: GraduatedHome::new(&home.url, home.nest_actor_id.as_deref()),
                    http: crate::pinned_http_client(&home.url),
                })
            }))
        };
        pinned.home.ensure_graduated().await.map_err(|e| {
            ApiError::Transport(format!(
                "graduate the pin of foreign home {}: {e:#}",
                pinned.home.url()
            ))
        })?;
        Ok(pinned)
    }
}

#[async_trait::async_trait]
impl ForeignHomeHttp for PinnedHomes {
    async fn client(&self, home: &ForeignHome) -> Result<reqwest::Client, ApiError> {
        Ok(self.graduated(home).await?.http.clone())
    }
}

/// [`pinned_plane`]'s state: the anonymous content plane over [`PinnedHomes`].
#[derive(Default)]
struct PinnedPlanes {
    homes: PinnedHomes,
}

#[async_trait::async_trait]
impl ForeignPlanes for PinnedPlanes {
    async fn plane(&self, home: &ForeignHome) -> Result<Arc<dyn NestContentApi>, ApiError> {
        let pinned = self.homes.graduated(home).await?;
        Ok(Arc::new(ReqwestNestContentApi::anonymous(
            pinned.home.url(),
            pinned.http.clone(),
        )))
    }
}

/// The two planes a post's bytes can come from, and the one pick between them.
#[derive(Clone)]
pub struct PostBlobHosts {
    own: Arc<dyn NestContentApi>,
    foreign: ForeignPlane,
}

impl PostBlobHosts {
    /// `own` is the session's bearer-carrying plane; a foreign home gets a
    /// [`pinned_plane`] built for this value alone.
    pub fn new(own: Arc<dyn NestContentApi>) -> Self {
        Self::with_foreign(own, pinned_plane())
    }

    /// [`Self::new`] with the foreign plane supplied — the seam a test fakes.
    pub fn with_foreign(own: Arc<dyn NestContentApi>, foreign: ForeignPlane) -> Self {
        Self { own, foreign }
    }

    /// This nest's own plane — the bearer-carrying one a blob this nest serves
    /// itself (a bridged post's proxied path) is read over.
    pub fn own(&self) -> Arc<dyn NestContentApi> {
        Arc::clone(&self.own)
    }

    /// The plane a post homed at `home` serves its bytes from — `None` (the
    /// post's `PostSummary::blob_home` is this nest) is the own plane; a
    /// foreign home's pin is graduated first.
    pub async fn plane(
        &self,
        home: Option<&ForeignHome>,
    ) -> Result<Arc<dyn NestContentApi>, ApiError> {
        match home.filter(|home| !home.url.trim().is_empty()) {
            None => Ok(Arc::clone(&self.own)),
            Some(home) => self.foreign.plane(home).await,
        }
    }

    /// The blob `hash` of a post homed at `home`: capped and verified against
    /// its content address. A digest mismatch is a non-transient refusal
    /// (`ApiError::Status` 422) — the same home serves the same wrong bytes on
    /// every retry.
    pub async fn get(&self, home: Option<&ForeignHome>, hash: &str) -> Result<Bytes, ApiError> {
        let limit = fauna_core::attachment_limits::INLINE_BLOB_BODY_LIMIT as u64;
        let bytes = self
            .plane(home)
            .await?
            .get_capped(&paths::blob::by_hash(hash), &[], limit)
            .await?;
        if !fauna_core::encoding::content_address_matches(hash, &bytes) {
            return Err(ApiError::Status {
                code: 422,
                message: format!("blob {hash}: the served bytes are not the named content"),
            });
        }
        Ok(bytes)
    }

    /// Whether the blob `hash` of a post homed at `home` asserts C2PA
    /// provenance — the `x-c2pa` HEAD, a pre-filter only (the verdict is a
    /// manifest found in the bytes [`Self::get`] returns).
    pub async fn head_has_c2pa(
        &self,
        home: Option<&ForeignHome>,
        hash: &str,
    ) -> Result<bool, ApiError> {
        self.plane(home)
            .await?
            .head_has_c2pa(&paths::blob::by_hash(hash))
            .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fauna_nest_http::{FakeNestContentApi, Verb};
    use std::sync::Mutex;

    fn hash_hex(bytes: &[u8]) -> String {
        hex::encode(fauna_core::encoding::content_hash(bytes).digest())
    }

    /// The hosts, the own and foreign fakes, and the homes asked for.
    type Fixture = (
        PostBlobHosts,
        Arc<FakeNestContentApi>,
        Arc<FakeNestContentApi>,
        Arc<Mutex<Vec<ForeignHome>>>,
    );

    /// Two fakes, and the homes the foreign plane was asked for.
    fn hosts() -> Fixture {
        let own = Arc::new(FakeNestContentApi::new());
        let foreign = Arc::new(FakeNestContentApi::new());
        let asked = Arc::new(Mutex::new(Vec::new()));
        let (plane, log) = (Arc::clone(&foreign), Arc::clone(&asked));
        let hosts = PostBlobHosts::with_foreign(
            Arc::clone(&own) as Arc<dyn NestContentApi>,
            Arc::new(move |home: &ForeignHome| {
                log.lock().unwrap().push(home.clone());
                Arc::clone(&plane) as Arc<dyn NestContentApi>
            }),
        );
        (hosts, own, foreign, asked)
    }

    #[tokio::test]
    async fn a_post_with_no_home_reads_this_nest() {
        let (hosts, own, _, asked) = hosts();
        let hash = hash_hex(b"here");
        own.set_ok(
            Verb::Get,
            &paths::blob::by_hash(&hash),
            Bytes::from_static(b"here"),
        );
        assert_eq!(&hosts.get(None, &hash).await.unwrap()[..], b"here");
        let blank = ForeignHome::new("  ", None);
        assert_eq!(&hosts.get(Some(&blank), &hash).await.unwrap()[..], b"here");
        assert!(
            asked.lock().unwrap().is_empty(),
            "no foreign plane was built"
        );
    }

    #[tokio::test]
    async fn a_post_homed_elsewhere_reads_its_home() {
        let (hosts, own, foreign, asked) = hosts();
        let hash = hash_hex(b"there");
        foreign.set_ok(
            Verb::Get,
            &paths::blob::by_hash(&hash),
            Bytes::from_static(b"there"),
        );
        foreign.set_c2pa(&paths::blob::by_hash(&hash), Ok(true));
        let id = "ab".repeat(32);
        let home = ForeignHome::new("https://home.example", Some(&id));
        assert_eq!(&hosts.get(Some(&home), &hash).await.unwrap()[..], b"there");
        assert!(hosts.head_has_c2pa(Some(&home), &hash).await.unwrap());
        // The plane is handed the home with the identity its pin graduates
        // against, on every leg.
        assert_eq!(*asked.lock().unwrap(), vec![home; 2]);
        assert!(own.calls().is_empty(), "nothing asked this nest");
    }

    #[tokio::test]
    async fn bytes_that_are_not_the_named_content_are_refused() {
        let (hosts, _, foreign, _) = hosts();
        let hash = hash_hex(b"the real image");
        foreign.set_ok(
            Verb::Get,
            &paths::blob::by_hash(&hash),
            Bytes::from_static(b"a substitute"),
        );
        let err = hosts
            .get(Some(&ForeignHome::new("https://home.example", None)), &hash)
            .await
            .expect_err("a substitute must not paint");
        assert!(!err.is_transient(), "the same bytes come back on a retry");
    }

    /// The production plane graduates before it dials: a malformed home
    /// identity fails closed at the decode guard, and nothing reaches the
    /// (unroutable) home — the post stays teased, never fetched on the floor.
    #[tokio::test]
    async fn the_pinned_plane_refuses_a_malformed_home_identity_before_dialing() {
        let planes = pinned_plane();
        // TEST-NET-1 (RFC 5737): a hang here would mean the guard failed.
        let home = ForeignHome::new("https://192.0.2.1:1", Some("junk"));
        let err = match planes.plane(&home).await {
            Ok(_) => panic!("a malformed identity never yields a plane"),
            Err(err) => err,
        };
        assert!(format!("{err:?}").contains("32-byte hex"), "got: {err:?}");
    }

    /// A plain-`http` home has no TLS to pin: the plane skips the graduation
    /// (no dial happens building it) — the loopback rig's posture, as the
    /// chunk-store fetcher's.
    #[tokio::test]
    async fn the_pinned_plane_skips_graduation_for_a_plain_http_home() {
        let planes = pinned_plane();
        let home = ForeignHome::new("http://192.0.2.1:1", Some(&"ab".repeat(32)));
        assert!(planes.plane(&home).await.is_ok());
    }
}
