//! UniFFI face of the shared post-blob plane (`fauna_client::post_blob`) — the
//! android / windows / apple seam onto the one host pick and the graduated
//! foreign byte plane that tui and linux drive as [`PostBlobHosts`] directly
//! (`feed.md` § The read model → *Opening a post the timeline never loaded* →
//! *A post homed on another nest*; `transport-trust.md` § Mechanism for the
//! federation-granted row → *The post arm*).
//!
//! The face adds no logic: which nest a post's bytes rest on is
//! [`fauna_feed::PostSummary::blob_home`] (crossed as [`post_blob_home`]), and
//! everything about reaching it — no bearer on a foreign home, the pin graduated
//! once per home for this handle's life when an identity is present (a
//! plain-`http` home skips graduation), a malformed identity failing closed
//! before any dial, the inline-blob cap, the content-address check — stays in
//! `fauna_client`. An app takes one [`FfiPostBlobHosts`] per signed-in session
//! ([`crate::FfiNestClient::post_blob_hosts`]) and hands each post's
//! [`post_blob_home`] back with the hash.
//!
//! Gated `feed-manager` with the Feed façade: [`post_blob_home`] takes the
//! cross-crate `fauna_feed::PostSummary` uniffi-bindgen-go cannot emit.

use std::sync::Arc;

use fauna_client::post_blob::{ForeignPlane, pinned_plane};
use fauna_client::{ForeignHome, NestClient, PostBlobHosts};
use fauna_nest_http::NestContentApi;

use crate::FfiError;

/// The nest a post's own bytes rest on when it is not this one — the record
/// form of [`fauna_feed::BlobHome`], read off a post by [`post_blob_home`].
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct FfiPostBlobHome {
    /// The home's base URL.
    pub url: String,
    /// The home's hex `nest_actor_id` its pin graduates against — `None`
    /// keeps the WebPKI floor.
    #[uniffi(default = None)]
    pub nest_actor_id: Option<String>,
}

impl From<FfiPostBlobHome> for ForeignHome {
    fn from(home: FfiPostBlobHome) -> Self {
        ForeignHome {
            url: home.url,
            nest_actor_id: home.nest_actor_id,
        }
    }
}

/// The home `post`'s own bytes are fetched from — `None` for this nest. The
/// UniFFI face of [`fauna_feed::PostSummary::blob_home`], so no app re-decides
/// what a blank home means. Never a link-preview og:image or a bridged post's
/// proxied path: those pass `None` to [`FfiPostBlobHosts`].
#[uniffi::export]
pub fn post_blob_home(post: fauna_feed::PostSummary) -> Option<FfiPostBlobHome> {
    post.blob_home().map(|home| FfiPostBlobHome {
        url: home.url.to_string(),
        nest_actor_id: home.nest_actor_id.map(str::to_string),
    })
}

/// Builds this nest's bearer-carrying plane per call, so an SRV-reconnect's
/// new nest URL is the one read.
type OwnPlane = Arc<dyn Fn() -> Arc<dyn NestContentApi> + Send + Sync>;

/// One session's post-blob planes: this nest's, and every foreign home's,
/// whose graduated pins live as long as this handle.
#[derive(uniffi::Object)]
pub struct FfiPostBlobHosts {
    own: OwnPlane,
    foreign: ForeignPlane,
}

impl FfiPostBlobHosts {
    /// Over the signed-in `nest`, with the production [`pinned_plane`].
    pub(crate) fn new(nest: Arc<NestClient>) -> Arc<Self> {
        Self::with_planes(
            Arc::new(move || Arc::new(nest.auth().content_api()) as Arc<dyn NestContentApi>),
            pinned_plane(),
        )
    }

    /// [`Self::new`] with both planes supplied — the seam a test fakes.
    fn with_planes(own: OwnPlane, foreign: ForeignPlane) -> Arc<Self> {
        Arc::new(Self { own, foreign })
    }

    fn hosts(&self) -> PostBlobHosts {
        PostBlobHosts::with_foreign((self.own)(), Arc::clone(&self.foreign))
    }
}

#[fauna_uniffi_async::export]
impl FfiPostBlobHosts {
    /// The blob `hash` of a post homed at `home` ([`post_blob_home`]; `None`
    /// = this nest), capped and verified against its content address — see
    /// [`PostBlobHosts::get`].
    pub async fn get(
        &self,
        home: Option<FfiPostBlobHome>,
        hash: String,
    ) -> Result<Vec<u8>, FfiError> {
        let home = home.map(ForeignHome::from);
        self.hosts()
            .get(home.as_ref(), &hash)
            .await
            .map(|bytes| bytes.to_vec())
            .map_err(|e| FfiError::General { msg: e.to_string() })
    }

    /// Whether the blob `hash` of a post homed at `home` asserts C2PA
    /// provenance — a pre-filter only ([`PostBlobHosts::head_has_c2pa`]).
    pub async fn head_has_c2pa(
        &self,
        home: Option<FfiPostBlobHome>,
        hash: String,
    ) -> Result<bool, FfiError> {
        let home = home.map(ForeignHome::from);
        self.hosts()
            .head_has_c2pa(home.as_ref(), &hash)
            .await
            .map_err(|e| FfiError::General { msg: e.to_string() })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fauna_nest_http::{FakeNestContentApi, Verb, paths};
    use std::sync::Mutex;

    fn hash_hex(bytes: &[u8]) -> String {
        hex::encode(fauna_core::encoding::content_hash(bytes).digest())
    }

    /// A post homed elsewhere reads its home through the face: the plane is
    /// handed the identity the post carries, on both legs, and this nest is
    /// never asked; a post with no home reads this nest.
    #[tokio::test]
    async fn the_face_reads_a_foreign_home_with_the_identity_the_post_carries() {
        let own = Arc::new(FakeNestContentApi::new());
        let foreign = Arc::new(FakeNestContentApi::new());
        let asked = Arc::new(Mutex::new(Vec::<ForeignHome>::new()));
        let (own_plane, plane, log) = (Arc::clone(&own), Arc::clone(&foreign), Arc::clone(&asked));
        let face = FfiPostBlobHosts::with_planes(
            Arc::new(move || Arc::clone(&own_plane) as Arc<dyn NestContentApi>),
            Arc::new(move |home: &ForeignHome| {
                log.lock().unwrap().push(home.clone());
                Arc::clone(&plane) as Arc<dyn NestContentApi>
            }),
        );

        let there = hash_hex(b"there");
        foreign.set_ok(Verb::Get, &paths::blob::by_hash(&there), &b"there"[..]);
        foreign.set_c2pa(&paths::blob::by_hash(&there), Ok(true));
        let id = "ab".repeat(32);
        let post = fauna_feed::PostSummary {
            home_nest_url: Some(" https://home.example ".into()),
            home_nest_actor_id: Some(id.clone()),
            ..Default::default()
        };
        let home = post_blob_home(post).expect("a post homed elsewhere names its home");
        assert_eq!(
            home,
            FfiPostBlobHome {
                url: "https://home.example".into(),
                nest_actor_id: Some(id.clone()),
            }
        );
        assert_eq!(
            face.get(Some(home.clone()), there.clone()).await.unwrap(),
            b"there"
        );
        assert!(face.head_has_c2pa(Some(home), there).await.unwrap());
        assert_eq!(
            *asked.lock().unwrap(),
            vec![ForeignHome::new("https://home.example", Some(&id)); 2]
        );
        assert!(own.calls().is_empty(), "nothing asked this nest");

        let here = hash_hex(b"here");
        own.set_ok(Verb::Get, &paths::blob::by_hash(&here), &b"here"[..]);
        assert_eq!(post_blob_home(fauna_feed::PostSummary::default()), None);
        assert_eq!(face.get(None, here).await.unwrap(), b"here");
        assert_eq!(
            asked.lock().unwrap().len(),
            2,
            "no foreign plane for this nest's post"
        );
    }

    /// The plane's refusal of substitute bytes crosses as an error, never as
    /// bytes to paint.
    #[tokio::test]
    async fn substitute_bytes_cross_as_an_error() {
        let foreign = Arc::new(FakeNestContentApi::new());
        let plane = Arc::clone(&foreign);
        let face = FfiPostBlobHosts::with_planes(
            Arc::new(|| Arc::new(FakeNestContentApi::new()) as Arc<dyn NestContentApi>),
            Arc::new(move |_: &ForeignHome| Arc::clone(&plane) as Arc<dyn NestContentApi>),
        );
        let hash = hash_hex(b"the real image");
        foreign.set_ok(
            Verb::Get,
            &paths::blob::by_hash(&hash),
            &b"a substitute"[..],
        );
        let home = FfiPostBlobHome {
            url: "https://home.example".into(),
            nest_actor_id: None,
        };
        assert!(face.get(Some(home), hash).await.is_err());
    }
}
