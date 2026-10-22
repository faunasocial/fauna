//! The pure half of the profile seam: the record mirrors plus the three free
//! functions the native edit form drives — [`decode_profile_display`] (project
//! the stored `Profile` to the editable display fields), [`build_edited_profile`]
//! (the read-modify-write sign step — overwrite only the display fields,
//! preserve everything else, then `fauna_client_profile::build_profile`) and
//! [`build_edited_profile_with_images`] (the same, plus avatar / banner).
//!
//! Nothing here holds a nest connection, so — exactly like `src/post.rs` beside
//! `src/posts_client.rs` — this module compiles unconditionally and only the RPC
//! face (`src/profile_client.rs`, `FfiProfileClient`) sits behind the
//! `profile-client` feature. Composing and decoding a `Profile` is useful to any
//! consumer of the FFI, connection or not: the Go atproto bridge's projection
//! tests build real profile bytes with these builders rather than inventing a
//! fixture, which is what lets them assert against what the apps actually emit.
//!
//! The sign/decode/read-modify-write logic itself is written once in shared Rust
//! (`fauna_client_profile`, also called directly by the Rust-native Linux app
//! at `apps/fauna-linux/src/views/profile/edit.rs`); this seam only marshals
//! (priority #2). See `docs/goal/ui/profile.md` § Where logic lives →
//! *Profile publish/edit*.
//!
//! The edit form is text-only v1: the user edits `display_name` / `bio` / a
//! repeatable list of links; `avatar` / `banner` / `nests` / `admin_nests` /
//! `load_hint` / `inbox_mode` / `recovery_head` are carried through the
//! read-modify-write so a re-edit never clobbers them (mirrors the Linux
//! `submit` read-modify-write branch exactly) — from a base the shared
//! admission rule vouched for, which carries `recovery_head` / `nests` only from
//! this identity's own signature.

use fauna_client_profile::ProfileImageEdit;
use fauna_core::data::ProfileLink;

use crate::{FfiError, general_err, keypair_from_bytes};

// ── display-field mirrors ──────────────────────────────────────────────

/// FFI mirror of [`fauna_core::data::ProfileLink`] — one editable label + uri
/// row of the profile edit form.
#[derive(uniffi::Record, Clone, Debug, PartialEq)]
pub struct FfiProfileLink {
    pub label: String,
    pub uri: String,
}

impl From<ProfileLink> for FfiProfileLink {
    fn from(l: ProfileLink) -> Self {
        FfiProfileLink {
            label: l.label,
            uri: l.uri,
        }
    }
}

impl From<FfiProfileLink> for ProfileLink {
    fn from(l: FfiProfileLink) -> Self {
        ProfileLink {
            label: l.label,
            uri: l.uri,
        }
    }
}

/// The editable fields the native edit form populates from a fetched `Profile`
/// (the projection of [`decode_profile_display`]) — the three text fields, plus
/// the two image references (hex blob hashes) so the form can render the
/// current picture and offer a remove affordance. The remaining non-display
/// fields (`nests` / `admin_nests` / `load_hint` / `inbox_mode` /
/// `recovery_pubkey`) are NOT exposed here — they are preserved internally by
/// [`build_edited_profile`]'s read-modify-write, so the client never has to
/// round-trip them across the FFI boundary.
#[derive(uniffi::Record, Clone, Debug, PartialEq)]
pub struct FfiProfileDisplay {
    pub display_name: Option<String>,
    pub bio: Option<String>,
    pub links: Vec<FfiProfileLink>,
    /// Hex blob hash of the stored avatar, or `None` when the profile has no
    /// picture. Fetch the bytes over the ordinary blob download path.
    pub avatar_hash_hex: Option<String>,
    /// Hex blob hash of the stored banner, or `None`.
    pub banner_hash_hex: Option<String>,
    /// Whether the owner opened the profile to other nests (`audience =
    /// Everyone`) — `profile-edit-audience-toggle`'s current value. An audience
    /// this build does not name reads as off.
    pub audience_everyone: bool,
}

/// FFI mirror of [`fauna_client_profile::ProfileImageEdit`] — what one save
/// does to `avatar` / `banner`. `Keep` is the text-only form's value on both
/// fields (and the default the older [`build_edited_profile`] passes).
#[derive(uniffi::Enum, Clone, Debug, PartialEq)]
pub enum FfiProfileImageEdit {
    /// Leave the stored picture untouched.
    Keep,
    /// Remove the picture.
    Clear,
    /// Point the field at an uploaded blob (hex hash from the upload response).
    Set { blob_hash_hex: String },
}

impl FfiProfileImageEdit {
    fn into_shared(self) -> Result<ProfileImageEdit, FfiError> {
        match self {
            Self::Keep => Ok(ProfileImageEdit::Keep),
            Self::Clear => Ok(ProfileImageEdit::Clear),
            Self::Set { blob_hash_hex } => {
                ProfileImageEdit::set_from_hex(&blob_hash_hex).map_err(general_err)
            }
        }
    }
}

// ── free functions ─────────────────────────────────────────────────────

/// Decode the stored profile `body` (from `FfiProfileClient::profile_get`)
/// and project it to the three editable display fields the edit form
/// populates. Thin UniFFI adapter over the shared
/// [`fauna_client_profile::decode_profile_display`] — the projection logic is
/// written once for native + web (priority #2); this only marshals
/// [`ProfileDisplay`](fauna_client_profile::ProfileDisplay) →
/// [`FfiProfileDisplay`]. The non-display fields are dropped from the
/// projection but preserved across an edit by [`build_edited_profile`]'s
/// read-modify-write.
#[uniffi::export]
pub fn decode_profile_display(body: Vec<u8>) -> Result<FfiProfileDisplay, FfiError> {
    fauna_client_profile::decode_profile_display(&body)
        .map(FfiProfileDisplay::from)
        .map_err(general_err)
}

/// The OWN edit form's projection — [`decode_profile_display`] with
/// `audience_everyone` resolved exactly as the save will sign it, over the
/// edit base [`crate::profile_client::FfiProfileClient::load_edit_base`] read
/// (`own_actor_id_hex` the signed-in identity). Thin UniFFI adapter over the
/// shared [`fauna_client_profile::decode_own_profile_display`] (`profile.md`
/// § Where logic lives → *Profile publish/edit* → *Edit surface*).
#[uniffi::export]
pub fn decode_own_profile_display(
    own_actor_id_hex: String,
    body: Vec<u8>,
    last_direct_body: Option<Vec<u8>>,
) -> Result<FfiProfileDisplay, FfiError> {
    let own = fauna_core::identity::ActorId::from_hex(&own_actor_id_hex).map_err(general_err)?;
    fauna_client_profile::decode_own_profile_display(&own, &body, last_direct_body.as_deref())
        .map(FfiProfileDisplay::from)
        .map_err(general_err)
}

impl From<fauna_client_profile::ProfileDisplay> for FfiProfileDisplay {
    fn from(display: fauna_client_profile::ProfileDisplay) -> Self {
        let (avatar_hash_hex, banner_hash_hex) =
            (display.avatar_hash_hex(), display.banner_hash_hex());
        Self {
            display_name: display.display_name,
            bio: display.bio,
            links: display
                .links
                .into_iter()
                .map(FfiProfileLink::from)
                .collect(),
            avatar_hash_hex,
            banner_hash_hex,
            audience_everyone: display.audience.is_everyone(),
        }
    }
}

/// Where a knock sent from `actor_id`'s profile page must go — the value for
/// `FfiInboxClient::send`'s `recipient_nest_url` (`profile.md` § Where logic
/// lives → *Request contact routing*). Thin UniFFI adapter over the shared
/// [`fauna_client_profile::knock_recipient_nest_url`]: `profile_body` is the
/// profile the page's open already fetched, `own_nest_url` the caller's home
/// nest. `None` = same-nest local delivery, and is also what a wrong-length
/// `actor_id` yields — a route is an optimisation of delivery, never a reason
/// to fail the profile open, so nothing here throws.
#[uniffi::export]
pub fn knock_recipient_nest_url(
    actor_id: Vec<u8>,
    profile_body: Vec<u8>,
    own_nest_url: String,
) -> Option<String> {
    let actor = crate::bytes_to_actor_id(&actor_id).ok()?;
    fauna_client_profile::knock_recipient_nest_url(&actor, &profile_body, &own_nest_url)
}

/// The other actor's home nest — the profile page's one `actor_home` slot, read
/// by the follow and the knock alike (`profile.md` § Where logic lives → *The
/// actor's home*). Thin UniFFI adapter over the shared
/// [`fauna_client_profile::actor_home`]: `carried` is the home the page's open
/// target carried, `profile_body` the header read's profile (`None` before it
/// answers). A wrong-length `actor_id` yields `None`, the same lenience as
/// [`knock_recipient_nest_url`].
#[uniffi::export]
pub fn actor_home(
    carried: Option<String>,
    actor_id: Vec<u8>,
    profile_body: Option<Vec<u8>>,
    own_nest_url: String,
) -> Option<String> {
    let actor = crate::bytes_to_actor_id(&actor_id).ok()?;
    fauna_client_profile::actor_home(
        carried.as_deref(),
        &actor,
        profile_body.as_deref(),
        &own_nest_url,
    )
}

/// The read-modify-write sign step for the profile edit form. Thin UniFFI
/// adapter over the shared [`fauna_client_profile::build_edited_profile`] — the
/// read-modify-write (overwrite only `display_name` / `bio` / `links`, preserve
/// everything else; first-publish defaults) lives once in shared Rust, shared
/// by the Linux native app, this FFI seam, and the wasm SPA (priority #2).
/// This only builds the keypair from the raw secret and marshals
/// [`FfiProfileLink`] → [`ProfileLink`].
///
/// `secret` is the 32-byte ed25519 actor secret. `base_body` is the current
/// stored profile bytes (from `FfiProfileClient::profile_get`) when
/// re-editing, or `None` for a first publish; `last_direct_body` the owner's
/// last identity-signed row served beside it (`FfiProfileEditBase`), which the
/// save takes `audience` and the harvest anchors from over an external app's
/// edit — `None` degrades them (`profile.md` § After an identity succession →
/// base-admission rule 3). `predecessors` is
/// `FfiAccountRegistry::predecessors_of(<this actor>)` — the hex ids this
/// device records the identity succeeding from, empty for one that never
/// succeeded. A stored base that is unsigned, or signed for anyone else, is
/// refused rather than re-signed (`profile.md` § After an identity succession,
/// the successor RE-PUBLISHES). Returns the signed `EmbedAsBytes` wire ready
/// for `FfiProfileClient::profile_set`.
#[uniffi::export]
pub fn build_edited_profile(
    secret: Vec<u8>,
    base_body: Option<Vec<u8>>,
    last_direct_body: Option<Vec<u8>>,
    predecessors: Vec<String>,
    display_name: Option<String>,
    bio: Option<String>,
    links: Vec<FfiProfileLink>,
) -> Result<Vec<u8>, FfiError> {
    let kp = keypair_from_bytes(&secret)?;
    let links: Vec<ProfileLink> = links.into_iter().map(ProfileLink::from).collect();
    fauna_client_profile::build_edited_profile(
        &kp,
        base_body.as_deref(),
        last_direct_body.as_deref(),
        &fauna_client_profile::predecessors_from_hex(&predecessors),
        display_name,
        bio,
        links,
    )
    .map_err(general_err)
}

/// [`build_edited_profile`] plus the two image fields — the full edit-form
/// write once a client can set or remove a profile picture / banner. Thin
/// UniFFI adapter over the shared
/// [`fauna_client_profile::build_edited_profile_with_images`].
///
/// The picture bytes are uploaded separately, through the ordinary public-post
/// blob path (`media.md` § Encryption at rest — avatar / banner blobs are
/// signed plaintext, the same shape as public-post attachments); this records
/// the resulting hash on the signed profile. `last_direct_body` and
/// `predecessors` follow [`build_edited_profile`]'s rule.
///
/// `audience_everyone` is `profile-edit-audience-toggle`: `Some(true)` opens
/// the profile to other nests, `Some(false)` closes it, `None` (the toggle
/// untouched) keeps what the base holds.
#[uniffi::export]
#[allow(clippy::too_many_arguments)]
pub fn build_edited_profile_with_images(
    secret: Vec<u8>,
    base_body: Option<Vec<u8>>,
    last_direct_body: Option<Vec<u8>>,
    predecessors: Vec<String>,
    display_name: Option<String>,
    bio: Option<String>,
    links: Vec<FfiProfileLink>,
    avatar: FfiProfileImageEdit,
    banner: FfiProfileImageEdit,
    audience_everyone: Option<bool>,
) -> Result<Vec<u8>, FfiError> {
    let kp = keypair_from_bytes(&secret)?;
    let links: Vec<ProfileLink> = links.into_iter().map(ProfileLink::from).collect();
    fauna_client_profile::build_edited_profile_with_images(
        &kp,
        base_body.as_deref(),
        last_direct_body.as_deref(),
        &fauna_client_profile::predecessors_from_hex(&predecessors),
        display_name,
        bio,
        links,
        avatar.into_shared()?,
        banner.into_shared()?,
        audience_everyone.map(fauna_client_profile::audience_from_toggle),
    )
    .map_err(general_err)
}

#[cfg(test)]
mod tests {
    use super::*;
    use fauna_client_profile::{build_profile, decode_profile};
    use fauna_core::data::{AdminNestEntry, InboxMode, Profile, Timestamp};
    use fauna_core::identity::ActorKeypair;
    use fauna_core::recovery::ChainHead;

    /// Build a base profile with NON-display fields set to non-defaults so the
    /// round-trip below can assert they survive the read-modify-write.
    fn base_profile(kp: &ActorKeypair) -> Profile {
        Profile {
            actor_id: kp.actor_id(),
            display_name: Some("old".into()),
            bio: Some("oldbio".into()),
            avatar: None,
            banner: None,
            links: vec![],
            nests: vec![],
            admin_nests: vec![AdminNestEntry {
                nest_id: vec![7u8; 32],
                url: "https://nest.example".into(),
                name: "My Nest".into(),
            }],
            load_hint: None,
            // Non-`Open` so the preservation assertion is meaningful.
            inbox_mode: InboxMode::ContactsOnly,
            // Non-`None` for the same reason: an edit of the display fields
            // must never drop the cached RecoveryKey binding.
            recovery_head: Some(ChainHead::new([0xA7; 32], 3)),
            audience: fauna_core::data::ProfileAudience::ThisNest,
            updated_at: Timestamp(0),
        }
    }

    /// The profile page's knock route over UniFFI: the shared rule answers, the
    /// face only marshals ids. Foreign home nest → its URL; same authority →
    /// `None` (local delivery); a profile about someone else, or a wrong-length
    /// id, → `None` too — a route never comes from another actor's word.
    #[test]
    fn the_knock_route_face_answers_with_the_shared_rule() {
        let kp = ActorKeypair::generate();
        let mut profile = base_profile(&kp);
        profile.nests = vec![fauna_core::data::NestEntry {
            nest_id: vec![7u8; 32],
            url: "https://peer.example:9000/".into(),
            roles: vec![fauna_core::data::NestRole::Social],
        }];
        let body = build_profile(&kp, &profile).expect("build");
        let actor = kp.actor_id().0.to_vec();

        assert_eq!(
            knock_recipient_nest_url(actor.clone(), body.clone(), "https://home.example".into())
                .as_deref(),
            Some("https://peer.example:9000/"),
        );
        assert_eq!(
            knock_recipient_nest_url(
                actor.clone(),
                body.clone(),
                "https://PEER.example:9000".into()
            ),
            None,
            "the peer's own nest is local delivery"
        );
        let stranger = ActorKeypair::generate().actor_id().0.to_vec();
        assert_eq!(
            knock_recipient_nest_url(stranger, body.clone(), "https://home.example".into()),
            None
        );
        assert_eq!(
            knock_recipient_nest_url(vec![1, 2, 3], body, "https://home.example".into()),
            None,
            "a malformed actor id routes nowhere rather than erroring the profile open"
        );
    }

    /// The `actor_home` face: the signed profile beats the carried hint, the
    /// hint stands before the header read, and a wrong-length id is `None`.
    #[test]
    fn the_actor_home_face_answers_with_the_shared_rule() {
        let kp = ActorKeypair::generate();
        let mut profile = base_profile(&kp);
        profile.nests = vec![fauna_core::data::NestEntry {
            nest_id: vec![7u8; 32],
            url: "https://peer.example:9000/".into(),
            roles: vec![fauna_core::data::NestRole::Social],
        }];
        let body = build_profile(&kp, &profile).expect("build");
        let actor = kp.actor_id().0.to_vec();
        let hint = Some("https://met.example".to_string());
        let me = "https://home.example".to_string();

        assert_eq!(
            actor_home(hint.clone(), actor.clone(), Some(body), me.clone()).as_deref(),
            Some("https://peer.example:9000/"),
        );
        assert_eq!(
            actor_home(hint.clone(), actor.clone(), None, me.clone()).as_deref(),
            Some("https://met.example"),
        );
        assert_eq!(actor_home(Some(me.clone()), actor, None, me.clone()), None);
        assert_eq!(actor_home(hint, vec![1, 2, 3], None, me), None);
    }

    #[test]
    fn build_edited_profile_overwrites_display_preserves_rest() {
        let kp = ActorKeypair::generate();
        let base = base_profile(&kp);
        let base_bytes = build_profile(&kp, &base).expect("build base");

        let edited = build_edited_profile(
            kp.secret_bytes().to_vec(),
            Some(base_bytes),
            None,
            vec![],
            Some("new".into()),
            Some("newbio".into()),
            vec![FfiProfileLink {
                label: "L".into(),
                uri: "https://x".into(),
            }],
        )
        .expect("build edited");

        let (decoded, _) = decode_profile(&edited).expect("decode edited");
        // Display fields overwritten.
        assert_eq!(decoded.display_name, Some("new".into()));
        assert_eq!(decoded.bio, Some("newbio".into()));
        assert_eq!(decoded.links.len(), 1);
        assert_eq!(decoded.links[0].label, "L");
        assert_eq!(decoded.links[0].uri, "https://x");
        // Non-display fields preserved from the base.
        assert_eq!(decoded.admin_nests.len(), 1);
        assert_eq!(decoded.admin_nests[0].name, "My Nest");
        assert_eq!(decoded.inbox_mode, InboxMode::ContactsOnly);
        assert_eq!(decoded.recovery_head, Some(ChainHead::new([0xA7; 32], 3)));
    }

    #[test]
    fn build_edited_profile_first_publish_uses_defaults() {
        let kp = ActorKeypair::generate();
        let fresh = build_edited_profile(
            kp.secret_bytes().to_vec(),
            None,
            None,
            vec![],
            Some("fresh".into()),
            None,
            vec![],
        )
        .expect("build fresh");

        let (decoded, _) = decode_profile(&fresh).expect("decode fresh");
        assert_eq!(decoded.display_name, Some("fresh".into()));
        assert_eq!(decoded.bio, None);
        assert!(decoded.admin_nests.is_empty());
        assert_eq!(decoded.inbox_mode, InboxMode::Open);
    }

    #[test]
    fn the_audience_toggle_round_trips_through_the_ffi_faces() {
        let kp = ActorKeypair::generate();
        let save = |base: Option<Vec<u8>>, toggle: Option<bool>| {
            build_edited_profile_with_images(
                kp.secret_bytes().to_vec(),
                base,
                None,
                vec![],
                Some("name".into()),
                None,
                vec![],
                FfiProfileImageEdit::Keep,
                FfiProfileImageEdit::Keep,
                toggle,
            )
            .expect("build")
        };
        let fresh = save(None, None);
        assert!(
            !decode_profile_display(fresh)
                .expect("decode")
                .audience_everyone
        );
        let opened = save(None, Some(true));
        assert!(
            decode_profile_display(opened.clone())
                .expect("decode")
                .audience_everyone
        );
        // The toggle untouched keeps the owner's own `Everyone`; off closes it.
        let kept = save(Some(opened.clone()), None);
        assert!(
            decode_profile_display(kept)
                .expect("decode")
                .audience_everyone
        );
        let closed = save(Some(opened), Some(false));
        assert!(
            !decode_profile_display(closed)
                .expect("decode")
                .audience_everyone
        );
    }

    #[test]
    fn decode_profile_display_projects_three_fields() {
        let kp = ActorKeypair::generate();
        let mut base = base_profile(&kp);
        base.links = vec![ProfileLink {
            label: "site".into(),
            uri: "https://s".into(),
        }];
        let bytes = build_profile(&kp, &base).expect("build base");

        let display = decode_profile_display(bytes).expect("decode display");
        assert_eq!(display.display_name, Some("old".into()));
        assert_eq!(display.bio, Some("oldbio".into()));
        assert_eq!(
            display.links,
            vec![FfiProfileLink {
                label: "site".into(),
                uri: "https://s".into(),
            }]
        );
    }

    /// The face's own job: the registry's hex predecessor ids reach the
    /// shared admission rule. A successor's edit over the profile it inherited
    /// works only when the predecessor is listed, and without it the base is
    /// refused rather than re-signed.
    #[test]
    fn the_listed_predecessor_is_what_admits_an_inherited_base() {
        let predecessor = ActorKeypair::generate();
        let successor = ActorKeypair::generate();
        let inherited =
            build_profile(&predecessor, &base_profile(&predecessor)).expect("inherited");
        let edit = |predecessors: Vec<String>| {
            build_edited_profile(
                successor.secret_bytes().to_vec(),
                Some(inherited.clone()),
                None,
                predecessors,
                Some("new".into()),
                None,
                vec![],
            )
        };

        let edited = edit(vec![predecessor.actor_id().to_hex()]).expect("a listed predecessor");
        let (decoded, _) = decode_profile(&edited).expect("decode");
        assert_eq!(decoded.actor_id, successor.actor_id());
        assert_eq!(
            decoded.recovery_head, None,
            "a predecessor's head is not carried"
        );

        assert!(
            edit(vec![]).is_err(),
            "an unlisted predecessor's base is refused, never adopted"
        );
    }
}
