//! UniFFI façade for the `fauna.profile.*` WS-RPC surface — the per-actor
//! profile *detail* read (`get`) the Profile page renders identity from, and the
//! owner's own-write (`set`).
//!
//! This is the RPC half of the profile seam; the pure half — the record mirrors
//! and the three free functions the native edit form drives
//! ([`decode_profile_display`](crate::decode_profile_display),
//! [`build_edited_profile`](crate::build_edited_profile),
//! [`build_edited_profile_with_images`](crate::build_edited_profile_with_images))
//! — lives ungated in `src/profile.rs`. That is the same split `src/post.rs`
//! (pure builders) / `src/posts_client.rs` (the RPC face) already has: a build
//! with no nest connection still needs to compose and decode records, so only
//! the part that actually holds a [`NestClient`] is feature-gated.
//!
//! [`FfiProfileClient`] wraps `fauna_client_profile::ProfileClient` (which in
//! turn wraps the shared `NestClient`); the Rust-native Linux app calls the
//! same `ProfileClient` directly (`apps/fauna-linux/src/views/profile/edit.rs`)
//! — this seam gives Apple / Windows / Android the identical surface over
//! UniFFI, with the sign/decode/read-modify-write logic written once in shared
//! Rust (priority #2). See `docs/goal/ui/profile.md` § Where logic lives →
//! *Profile publish/edit*.

use std::sync::Arc;

use fauna_client::NestClient;
use fauna_client_profile::ProfileClient;

use crate::{FfiAccountRegistry, FfiError, secret32, stringify};

/// UniFFI handle for the `fauna.profile.*` kinds. Construct via
/// [`crate::nest_client::FfiNestClient::profile`]; methods are exposed to Swift
/// as `async throws` and Kotlin as `suspend fun`.
#[derive(uniffi::Object)]
pub struct FfiProfileClient {
    nest: Arc<NestClient>,
}

impl FfiProfileClient {
    pub(crate) fn from_nest(nest: Arc<NestClient>) -> Arc<Self> {
        Arc::new(Self { nest })
    }

    fn client(&self) -> ProfileClient<Arc<NestClient>> {
        ProfileClient::new(Arc::clone(&self.nest))
    }
}

/// The Profile header's read ([`FfiProfileClient::profile_header_read`]): the
/// stored signed profile bytes, the home they were relayed from — `None` when
/// this nest answered — and that home's identity as this nest stamped it on
/// the relay reply. The pictures the profile names rest on that nest: hand
/// the pair to the post-blob plane as its home (`FfiPostBlobHome { url:
/// relayed_from, nest_actor_id: home_nest_actor_id }`), so a home serving the
/// self-signed floor paints; never derive the identity from the profile's own
/// `nests` (`transport-trust.md` § The post arm → *A relayed profile's
/// pictures*).
#[derive(uniffi::Record, Clone, Debug, PartialEq)]
pub struct FfiProfileHeaderRead {
    pub body: Vec<u8>,
    pub relayed_from: Option<String>,
    pub home_nest_actor_id: Option<String>,
}

/// The own edit form's base ([`FfiProfileClient::load_edit_base`]): the stored
/// bytes the save re-signs, and the owner's last identity-signed row the nest
/// served beside a delegated `body` — `None` beside a direct one or when the
/// nest withheld it. Both go to `decode_own_profile_display` and to the save
/// (`profile.md` § Where logic lives → *Profile publish/edit* → *Edit
/// surface*).
#[derive(uniffi::Record, Clone, Debug, PartialEq)]
pub struct FfiProfileEditBase {
    pub body: Vec<u8>,
    pub last_direct_body: Option<Vec<u8>>,
}

impl From<fauna_client_profile::EditBase> for FfiProfileEditBase {
    fn from(base: fauna_client_profile::EditBase) -> Self {
        Self {
            body: base.body,
            last_direct_body: base.last_direct_body,
        }
    }
}

#[fauna_uniffi_async::export]
impl FfiProfileClient {
    /// `fauna.profile.get` — fetch a user's stored profile bytes by hex
    /// `actor_id`. Returns the raw stored `body` (the signed `EmbedAsBytes`
    /// wire); decode it with [`crate::decode_profile_display`]. A missing
    /// profile surfaces as `fauna.profile.not_found` — propagated as an
    /// [`FfiError`] (the C# seam maps it to "no profile yet"), NOT swallowed.
    pub async fn profile_get(&self, actor_id: String) -> Result<Vec<u8>, FfiError> {
        let reply = self
            .client()
            .profile_get(actor_id)
            .await
            .map_err(stringify)?;
        Ok(reply.body.into_vec())
    }

    /// `fauna.profile.get_remote` — the same read for an actor homed on
    /// another nest, relayed by this nest to `nest_url` (`profile.md` § Where
    /// logic lives → *The remote read*). The home's signed bytes, unstored; a
    /// profile the home does not serve is the same `fauna.profile.not_found`.
    pub async fn profile_get_remote(
        &self,
        actor_id: String,
        nest_url: String,
    ) -> Result<Vec<u8>, FfiError> {
        let reply = self
            .client()
            .profile_get_remote(actor_id, nest_url)
            .await
            .map_err(stringify)?;
        Ok(reply.body.into_vec())
    }

    /// The Profile page's header read for an OTHER actor — this nest first,
    /// the relay to `home` (the page's `actor_home` slot) on its `not_found`
    /// (the shared `ProfileClient::profile_get_or_relay`). Every app's OTHER
    /// header reads here; a `not_found` from either leg, or any error, is the
    /// header's actor-id fallback, never `error-message`.
    pub async fn profile_get_or_relay(
        &self,
        actor_id: String,
        home: Option<String>,
    ) -> Result<Vec<u8>, FfiError> {
        let reply = self
            .client()
            .profile_get_or_relay(actor_id, home.as_deref())
            .await
            .map_err(stringify)?;
        Ok(reply.body.into_vec())
    }

    /// [`Self::profile_get_or_relay`], also naming which nest answered — the
    /// Profile header's read (the shared `ProfileClient::profile_header_read`).
    /// The header's avatar and banner rest on that nest: `relayed_from` is
    /// `None` when this nest served the profile, the relayed-to `home`
    /// otherwise, whose public blob route the app reads through the session's
    /// post-blob planes (`profile.md` § Where logic lives → *The remote
    /// read*, *Bytes direct, as for a relayed post*). Errors as
    /// [`Self::profile_get_or_relay`]'s: the header's actor-id fallback.
    pub async fn profile_header_read(
        &self,
        actor_id: String,
        home: Option<String>,
    ) -> Result<FfiProfileHeaderRead, FfiError> {
        let read = self
            .client()
            .profile_header_read(actor_id, home.as_deref())
            .await
            .map_err(stringify)?;
        Ok(FfiProfileHeaderRead {
            body: read.reply.body.into_vec(),
            relayed_from: read.relayed_from,
            home_nest_actor_id: read.home_nest_actor_id,
        })
    }

    /// The profile edit form's base load — `owner_secret`'s OWN stored profile
    /// bytes, read through the shared read-prove-record
    /// (`fauna_client_recovery::ceremony::load_profile_edit_base`, tui's and
    /// linux's door), so a succession link the base needs is proven and
    /// recorded in `accounts` **before** the form can save over it. Without it a
    /// linkless successor's first save races the spawned per-sign-in hop, and a
    /// hop that failed refuses every edit until the next sign-in (profile.md
    /// § After an identity succession → the linkless bullet).
    ///
    /// Every FFI app's edit form reads its base here rather than through
    /// [`Self::profile_get`], whose `not_found` error this answers as `None` —
    /// a never-published profile, which is a first publish. An error is the
    /// read itself failing. `accounts` is the same registry the app hands
    /// `run_succession_aftermath`; the save reads its `predecessors_of`. The
    /// base's `last_direct_body` goes to the form's projection
    /// (`decode_own_profile_display`) and its save
    /// (`build_edited_profile_with_images`).
    pub async fn load_edit_base(
        &self,
        owner_secret: Vec<u8>,
        accounts: Arc<FfiAccountRegistry>,
    ) -> Result<Option<FfiProfileEditBase>, FfiError> {
        let keypair = fauna_core::identity::ActorKeypair::from_secret(secret32(&owner_secret)?);
        fauna_client_recovery::ceremony::load_profile_edit_base(
            Arc::clone(&self.nest),
            accounts.registry(),
            &keypair,
        )
        .await
        .map(|base| base.map(FfiProfileEditBase::from))
        // The one variant carries the read's own `NestClientError`, mapped the
        // way every other read here maps it (the typed identity-changed arm).
        .map_err(|fauna_client_profile::LearnPredecessorsError::Fetch(e)| stringify(e))
    }

    /// `fauna.profile.set` — publish/replace the caller's own profile. `body`
    /// is the signed wire from [`crate::build_edited_profile`].
    pub async fn profile_set(&self, body: Vec<u8>) -> Result<(), FfiError> {
        self.client()
            .profile_set(body)
            .await
            .map(|_| ())
            .map_err(stringify)
    }
}
