//! Typed-call wrapper for the user-facing `fauna.profile.*` WS-RPC surface —
//! the per-user *detail* fetch (`get`) the Profile page (and contact-row /
//! feed-author tap-through) renders identity from, and the owner's own-write
//! (`set`). See `docs/goal/ui/profile.md` § Where logic lives.
//!
//! Pattern: same shape as `fauna-client-posts` / `fauna-client-feed` — a thin
//! `pub struct ProfileClient<R: RpcRequester> { nest: R }`, one async method
//! per kind, no state machine, generic over the WS-RPC transport so the
//! kind-composition logic is written once and shared across native + wasm
//! (priority #2).
//!
//! Profile **build** + **decode** also live here so the client side is one
//! crate:
//!
//! - [`build_profile`] signs the owner's `Profile` into the `EmbedAsBytes`
//!   wire the nest stores — the client builds it (the nest holds no secret key
//!   and cannot sign), then ships it via [`ProfileClient::profile_set`].
//! - [`decode_profile`] is re-exported from `fauna_core::encoding` — the ONE
//!   shared signed-only verify decode used by both this read path AND the nest's
//!   `activitypub::actor_routes` federation serve (so the two never drift). It
//!   accepts the signed `EmbedAsBytes` wire (verify-on-receipt, the canonical
//!   stored shape) and a bare canonical `Profile` (the pre-publish-path shape).

use fauna_protocol::RpcRequester;
use fauna_protocol::profile::{
    ProfileGetReply, ProfileGetRequest, ProfileSetReply, ProfileSetRequest,
};

pub use fauna_protocol::profile;

use fauna_core::data::{ContentHash, InboxMode, Profile, ProfileLink, Timestamp};
use fauna_core::encoding::{AuthoringOrigin, sign_and_pack};
use fauna_core::identity::{ActorId, ActorKeypair};
use fauna_core::recovery::{ChainHead, SignedIdentitySuccession};

/// The ONE shared signed-only verify profile decode (`fauna_core::encoding`),
/// re-exported so client call sites keep importing it from this crate. Both the
/// `fauna.profile.get` read here and the nest's `activitypub::actor_routes`
/// serve decode through it.
pub use fauna_core::encoding::decode_profile;

/// Build + sign the owner's `Profile` into the at-rest / wire `EmbedAsBytes`
/// shape `fauna.profile.set` carries — the client-side half of the
/// publish/edit path (`profile.md` § Where logic lives → *Profile
/// publish/edit*). The nest cannot sign (it holds no secret key), so every
/// genuine profile originates here.
///
/// Asserts `profile.actor_id == keypair.actor_id()` defensively — a mismatch is
/// a caller bug the nest would reject anyway (it asserts the signed actor_id is
/// the authenticated caller). Every writer below assembles the full `Profile`
/// (the user's edits over a base [`admit_base`] vouched for — `profile.md`
/// § Field ownership), then hands it here so the sign step is written once for
/// all 7 apps.
pub fn build_profile(
    keypair: &ActorKeypair,
    profile: &Profile,
) -> Result<Vec<u8>, fauna_core::error::Error> {
    if profile.actor_id.0 != keypair.actor_id().0 {
        return Err(fauna_core::error::Error::Encoding(
            "build_profile: profile.actor_id does not match the signing keypair".into(),
        ));
    }
    sign_and_pack(keypair, profile)
}

/// What one edit does to one of the profile's two image fields (`avatar` /
/// `banner`).
///
/// Three states, because over a read-modify-write base "leave whatever is
/// stored alone" and "remove the picture" are genuinely different edits, and
/// `Option<ContentHash>` can only express two of them. A text-only save (the
/// v1 form, and every re-edit that doesn't touch the picture) is [`Keep`] on
/// both fields, which is exactly the preserve-the-rest behaviour
/// [`build_edited_profile`] has always had.
///
/// [`Keep`]: ProfileImageEdit::Keep
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum ProfileImageEdit {
    /// Leave the stored value untouched (first publish: stays `None`).
    #[default]
    Keep,
    /// Remove the picture — the field becomes `None`.
    Clear,
    /// Point the field at an uploaded blob.
    Set(ContentHash),
}

impl ProfileImageEdit {
    /// Build a [`Set`] from the hex blob hash the nest's blob-upload response
    /// carries (`{"hash": "<64 hex chars>"}` — what
    /// `fauna_client::upload_public_post_blob` and the wasm upload path both
    /// return). Written once here so no client re-implements the
    /// hex → raw-codec-CID conversion.
    ///
    /// [`Set`]: ProfileImageEdit::Set
    pub fn set_from_hex(blob_hash_hex: &str) -> Result<Self, fauna_core::error::Error> {
        Ok(Self::Set(content_hash_from_hex(blob_hash_hex)?))
    }

    /// Adapter for bindings that cannot carry an enum payload (the wasm face
    /// takes the two parts as separate JS arguments). `clear` wins over a
    /// supplied hash; neither ⇒ [`Keep`].
    ///
    /// [`Keep`]: ProfileImageEdit::Keep
    pub fn from_parts(
        clear: bool,
        blob_hash_hex: Option<&str>,
    ) -> Result<Self, fauna_core::error::Error> {
        match (clear, blob_hash_hex) {
            (true, _) => Ok(Self::Clear),
            (false, Some(hex)) => Self::set_from_hex(hex),
            (false, None) => Ok(Self::Keep),
        }
    }

    /// Apply this edit to one field of the profile being built.
    fn apply(self, field: &mut Option<ContentHash>) {
        match self {
            Self::Keep => {}
            Self::Clear => *field = None,
            Self::Set(hash) => *field = Some(hash),
        }
    }
}

/// The nest's blob-upload response hash (hex blake3 digest) → the raw-codec
/// `ContentHash` the `Profile` record's `avatar` / `banner` fields carry.
fn content_hash_from_hex(hex_str: &str) -> Result<ContentHash, fauna_core::error::Error> {
    let bytes = hex::decode(hex_str.trim()).map_err(|e| {
        fauna_core::error::Error::Encoding(format!("profile image: bad blob hash hex: {e}"))
    })?;
    let digest: [u8; 32] = bytes.try_into().map_err(|_| {
        fauna_core::error::Error::Encoding(
            "profile image: blob hash must be 64 hex chars".to_string(),
        )
    })?;
    Ok(ContentHash::from_digest_raw(digest))
}

/// The editable fields the profile edit form populates from a fetched `Profile`
/// (the projection of [`decode_profile_display`]) — the three text fields, plus
/// the two image references so the form can render the current picture and
/// offer a remove affordance.
///
/// The remaining non-display fields (`nests` / `admin_nests` / `load_hint` /
/// `inbox_mode`) are NOT exposed here — they are preserved internally by
/// [`build_edited_profile`]'s read-modify-write, so the client never has to
/// round-trip them across the binding boundary (`profile.md` § Field
/// ownership).
#[derive(Clone, Debug)]
pub struct ProfileDisplay {
    pub display_name: Option<String>,
    pub bio: Option<String>,
    pub links: Vec<ProfileLink>,
    /// The stored avatar blob reference, if the profile has one.
    pub avatar: Option<ContentHash>,
    /// The stored banner blob reference, if the profile has one.
    pub banner: Option<ContentHash>,
}

/// Decode the stored profile `body` (from [`ProfileClient::profile_get`]) and
/// project it to the fields the edit form populates. The fields left out of the
/// projection are preserved across an edit by [`build_edited_profile`]'s
/// read-modify-write.
pub fn decode_profile_display(body: &[u8]) -> Result<ProfileDisplay, fauna_core::error::Error> {
    let (profile, _origin) = decode_profile(body)?;
    Ok(ProfileDisplay {
        display_name: profile.display_name,
        bio: profile.bio,
        links: profile.links,
        avatar: profile.avatar,
        banner: profile.banner,
    })
}

/// Where a knock sent from `actor`'s profile page must go — `fauna.inbox.send`'s
/// `recipient_nest_url` (`profile.md` § Where logic lives →
/// *Request contact routing*).
///
/// The Contacts page knows the recipient's nest because its lookup found them
/// there; the Profile page has only the actor id and the profile it rendered, so
/// the route is the profile's own self-asserted home nest, `nests[0]` — the same
/// entry the peer-anchor harvest reads as the actor's home. `None` means
/// same-nest local delivery, and is returned whenever the profile gives no
/// better answer: it does not decode, it is about a different actor than the
/// one being knocked (a route must never come from someone else's word), it
/// names no nest, or its home nest has the same authority as `own_nest_url`
/// (host compared case-insensitively, port included — two nests on one host
/// are two nests).
pub fn knock_recipient_nest_url(
    actor: &ActorId,
    profile_body: &[u8],
    own_nest_url: &str,
) -> Option<String> {
    let (profile, _origin) = decode_profile(profile_body).ok()?;
    if &profile.actor_id != actor {
        return None;
    }
    let home = profile.nests.into_iter().next()?.url;
    let theirs = fauna_core::web::authority_of(&home);
    if theirs.is_empty()
        || theirs.eq_ignore_ascii_case(&fauna_core::web::authority_of(own_nest_url))
    {
        return None;
    }
    Some(home)
}

/// Why a stored profile was refused as the base a writer re-signs.
///
/// Every writer here is a read-modify-write over whatever the home nest serves
/// to `profile_get(<own actor id>)`, and what the base carries is re-published
/// under the owner's identity signature — which a peer's harvest then reads as
/// the owner's own word (`identity-succession.md` § The succession statement →
/// *the peer-profile harvest*). So a base is admitted only on evidence the nest
/// cannot manufacture ([`admit_base`]). Anything else is refused: never adopted,
/// and never silently swapped for defaults either, because the user's fields are
/// unknown then and republishing empty ones is a loss of its own.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum BaseRefusal {
    /// Signed and verified, but for an identity that is neither the signer nor
    /// one this device's account registry records it succeeding from.
    #[error(
        "the stored profile belongs to {}, which is neither this identity nor a predecessor \
         this device knows of",
        .actor.to_hex()
    )]
    NotOwn { actor: ActorId },
}

/// What went wrong building a profile over a stored base.
#[derive(Debug, thiserror::Error)]
pub enum ProfileWriteError {
    /// The stored base is not this identity's to re-sign. Retrying with the
    /// same inputs refuses again.
    #[error("{0}")]
    Refused(#[from] BaseRefusal),
    /// The stored base did not decode (a signature that fails to verify
    /// included), or the rebuilt profile would not sign.
    #[error(transparent)]
    Encoding(#[from] fauna_core::error::Error),
}

/// The account registry's predecessor rows — `AccountRegistry::predecessors_of`,
/// hex actor ids — as the typed list every writer here takes. Written once so
/// no face re-derives it. A row that does not parse cannot name a base, so it
/// is skipped.
pub fn predecessors_from_hex<S: AsRef<str>>(ids: &[S]) -> Vec<ActorId> {
    ids.iter()
        .filter_map(|id| ActorId::from_hex(id.as_ref()).ok())
        .collect()
}

/// The one shared link verifier (`fauna_core::recovery::proven_predecessors`),
/// under the name this crate's writers have always called it by — lifted to
/// sit beside the statement type when the change-row reader took the same walk
/// (`mls-group-key-material.md` § M2 → *Writer-signed change records*, ruling
/// (8)(b)). What it proves here: a predecessor's base keeps its display fields
/// and loses both harvest anchors ([`admit_base`] rule 3).
pub use fauna_core::recovery::proven_predecessors;

/// What [`learn_inherited_predecessors`] could not do.
#[derive(Debug, thiserror::Error)]
pub enum LearnPredecessorsError<E> {
    /// `fauna.profile.get` failed for a reason other than "never published".
    #[error("read the current profile: {0}")]
    Fetch(E),
}

/// Learn, from the nest's public record, the succession link this device's
/// registry does not hold — the hop that un-strands a successor's profile on a
/// device that never held the predecessor's row, or lost it when the user
/// removed the retired account.
///
/// Reads `keypair`'s stored profile. When it is signed by an identity that is
/// neither the signer nor in `recorded`, it fetches the succession path forward
/// from that identity and returns what [`proven_predecessors`] establishes —
/// empty when there is nothing to learn (no profile, an own base, a recorded
/// predecessor's base) and empty when nothing is proven (a foreign base stays
/// foreign).
///
/// **The caller persists the answer** —
/// `AccountRegistry::record_predecessors(<signer>, <answer>)` — because this
/// wasm-safe crate does not reach the registry. Once persisted, every writer
/// here admits the base through its ordinary `predecessors` argument, on every
/// later launch, with no further lookup. Call it once per sign-in, before the
/// post-succession aftermath gate reads `predecessors_of`.
///
/// A failed lookup is not an error: the link simply stays unlearned, and the
/// next sign-in asks again. Only the profile read reports, since a caller may
/// want to tell "offline" from "nothing to learn".
///
/// [`fetch_own_profile_base`] is the same read that also hands back the bytes,
/// for the edit form's base load.
pub async fn learn_inherited_predecessors<R>(
    nest: R,
    keypair: &ActorKeypair,
    recorded: &[ActorId],
) -> Result<Vec<ActorId>, LearnPredecessorsError<R::Error>>
where
    R: RpcRequester,
    R::Error: fauna_protocol::RpcErrorClass,
{
    Ok(fetch_own_profile_base(nest, keypair, recorded)
        .await?
        .proven)
}

/// A signer's own stored profile, with what the public record proved about it
/// — [`fetch_own_profile_base`]'s answer.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct OwnProfileBase {
    /// The stored bytes, or `None` when the signer never published a profile.
    pub body: Option<Vec<u8>>,
    /// Predecessors the base proves beyond the caller's `recorded` list,
    /// nearest hop first ([`learn_inherited_predecessors`]'s answer). Empty for
    /// an own base, a recorded predecessor's base, and a base nothing proves.
    pub proven: Vec<ActorId>,
}

/// Read `keypair`'s own stored profile — the base an edit form re-signs — and
/// prove, in the same read, the succession link that base needs when the
/// registry lacks it.
///
/// **Why the edit form proves in-line rather than trusting the sign-in hop.**
/// The per-sign-in hop ([`learn_inherited_predecessors`]) runs in the
/// background, so a save can overtake it, and a hop that failed waits for the
/// next sign-in. Either way a linkless successor's first edit would be refused
/// over a base the public record already proves. This is the same in-line
/// proof [`publish_recovery_head`] runs, and it costs nothing extra: the base
/// is fetched anyway, and the lookup runs only over a base the registry cannot
/// place.
///
/// The caller persists `proven` exactly as for the sign-in hop
/// (`AccountRegistry::record_predecessors`) and passes the widened list to the
/// writer. `recorded` and the error contract are
/// [`learn_inherited_predecessors`]'s: a never-published profile is `Ok` with
/// no body.
pub async fn fetch_own_profile_base<R>(
    nest: R,
    keypair: &ActorKeypair,
    recorded: &[ActorId],
) -> Result<OwnProfileBase, LearnPredecessorsError<R::Error>>
where
    R: RpcRequester,
    R::Error: fauna_protocol::RpcErrorClass,
{
    let client = ProfileClient::new(nest);
    let base = match client.profile_get(keypair.actor_id().to_hex()).await {
        Ok(reply) => reply.body.into_vec(),
        Err(e) if is_profile_not_found(&e) => return Ok(OwnProfileBase::default()),
        Err(e) => return Err(LearnPredecessorsError::Fetch(e)),
    };
    let proven = prove_base(&client, &keypair.actor_id(), &base, recorded).await;
    Ok(OwnProfileBase {
        body: Some(base),
        proven,
    })
}

/// The shared half of [`fetch_own_profile_base`] and
/// [`publish_recovery_head`]: given base bytes already in hand, the predecessors
/// provable for it beyond `recorded`. Asks the nest nothing unless the base is
/// signed and names an identity that is neither the signer nor recorded.
async fn prove_base<R: RpcRequester>(
    client: &ProfileClient<R>,
    signer: &ActorId,
    base_body: &[u8],
    recorded: &[ActorId],
) -> Vec<ActorId> {
    let Ok((profile, _)) = decode_profile(base_body) else {
        return Vec::new();
    };
    if profile.actor_id == *signer || recorded.contains(&profile.actor_id) {
        return Vec::new();
    }
    let reply: fauna_protocol::recovery::SuccessionLookupReply = match client
        .nest
        .request(
            fauna_protocol::RpcError::SUCCESSION_LOOKUP_KIND,
            fauna_protocol::recovery::SuccessionLookupRequest {
                actor_id: fauna_protocol::ByteBuf::from(profile.actor_id.0.to_vec()),
                ..Default::default()
            },
        )
        .await
    {
        Ok(reply) => reply,
        Err(_) => return Vec::new(),
    };
    let Ok(statements) = reply
        .statements
        .iter()
        .map(|b| fauna_core::encoding::canonical_decode::<SignedIdentitySuccession>(b.as_ref()))
        .collect::<Result<Vec<_>, _>>()
    else {
        return Vec::new();
    };
    proven_predecessors(signer, &profile.actor_id, &statements)
}

/// The read-modify-write sign step for the profile edit form (the shared core
/// of the linux `submit` branch in `apps/fauna-linux/src/views/profile/edit.rs`
/// and the fauna-ffi / fauna-wasm bindings — written once, priority #2).
///
/// `base_body` is the current stored profile bytes (from
/// [`ProfileClient::profile_get`]) when re-editing, or `None` for a first
/// publish:
///
/// - `Some(base)`: admit it ([`admit_base`] — the rule and what it drops),
///   overwrite ONLY `display_name` / `bio` / `links` and bump `updated_at`,
///   carrying the rest of what the admitted base holds — so a re-edit of the
///   text fields never clobbers the rest of the identity.
/// - `None` (first publish): build a fresh `Profile` with minimal defaults
///   (`inbox_mode: Open`, empty `nests` / `admin_nests`, `None` for
///   avatar/banner/load_hint) — matching `profile.md` § Onboarding =
///   publish-on-first-edit.
///
/// `predecessors` is every identity this device's account registry records the
/// signer succeeding from (`AccountRegistry::predecessors_of`, through
/// [`predecessors_from_hex`]); empty for an identity that never succeeded.
///
/// Returns the signed `EmbedAsBytes` wire ready for [`ProfileClient::profile_set`].
pub fn build_edited_profile(
    keypair: &ActorKeypair,
    base_body: Option<&[u8]>,
    predecessors: &[ActorId],
    display_name: Option<String>,
    bio: Option<String>,
    links: Vec<ProfileLink>,
) -> Result<Vec<u8>, ProfileWriteError> {
    build_edited_profile_with_images(
        keypair,
        base_body,
        predecessors,
        display_name,
        bio,
        links,
        ProfileImageEdit::Keep,
        ProfileImageEdit::Keep,
    )
}

/// Admit a stored profile as the **base `keypair` is about to re-publish**,
/// keeping only what the evidence behind it vouches for. The one funnel every
/// writer here decodes its base through (`profile.md` § After an identity
/// succession, the successor RE-PUBLISHES → the admission rule, amended
/// 2026-09-19).
///
/// 1. **Envelope-verified, or refused** — [`decode_profile`] is signed-only, so
///    an unsigned body (any host's fabrication) fails to decode.
/// 2. **The signer's own, or a listed predecessor's, or refused**
///    ([`BaseRefusal::NotOwn`]). Verification is self-consistent, not
///    self-targeted: a hostile home nest can serve *any* identity's genuine
///    signed profile, including one it minted itself, so "the nest answered
///    this row is yours" is not evidence. A predecessor is admitted only when
///    `predecessors` lists it — the device's own record of a succession it
///    took part in or restored. A base naming a predecessor is re-labelled to
///    the signer, because a succession re-points the row's ownership nest-side
///    while its signed bytes are still the predecessor's.
/// 3. **The two harvest anchors survive only a base the signer itself signed
///    with its own identity key** ([`AuthoringOrigin::Direct`]). `recovery_head`
///    and `nests` are what a peer's harvest seeds as this identity's succession
///    anchor and dial domain. A delegated edit is signed by the nest-held
///    authoring sub-key, and a predecessor's profile is another identity's word
///    (a seed thief's, if it was doctored during the compromise window). Either
///    one carried forward would put that party's anchor under the owner's
///    signature. So both fields degrade to absent — a cache miss that costs
///    peers a chain fetch, never a poisoned anchor.
/// 4. **Everything else the admitted base holds travels with the account** —
///    the display fields, `avatar` / `banner`, `admin_nests`, `load_hint` and
///    `inbox_mode`. None of them is an anchor: nothing reads them to decide
///    whom to trust. They are within the scope a delegated `UpdateProfile`
///    grant already covers. And resetting them would itself publish a loss
///    (`inbox_mode` would read `Open`).
///
/// **This admits only what `predecessors` lists** (rule 2), so a successor
/// device whose registry records no link refuses a predecessor-signed base
/// here. Such a device learns the link first, by proof, never by the nest's
/// word: [`fetch_own_profile_base`] for an edit form, the in-line proof in
/// [`publish_recovery_head`], or the per-sign-in
/// [`learn_inherited_predecessors`]. It then passes the widened list
/// (`profile.md` § After an identity succession → *A successor device with no
/// recorded succession link*).
fn admit_base(
    keypair: &ActorKeypair,
    base_body: &[u8],
    predecessors: &[ActorId],
) -> Result<Profile, ProfileWriteError> {
    let (mut profile, origin) = decode_profile(base_body)?;
    let own = keypair.actor_id();
    let signed_by_self = if profile.actor_id == own {
        origin == AuthoringOrigin::Direct
    } else if predecessors.contains(&profile.actor_id) {
        false
    } else {
        return Err(BaseRefusal::NotOwn {
            actor: profile.actor_id,
        }
        .into());
    };
    if !signed_by_self {
        profile.recovery_head = None;
        profile.nests.clear();
    }
    profile.actor_id = own;
    Ok(profile)
}

/// [`build_edited_profile`] plus the two image fields — the full edit-form
/// write once a client can set or remove a profile picture / banner
/// (`profile.md` § Field ownership).
///
/// Same read-modify-write contract: `base_body` `Some` is admitted
/// ([`admit_base`]), then only the text fields are overwritten and `avatar` /
/// `banner` applied on top of the stored values; `None` builds a fresh profile
/// with minimal defaults and applies the edits to the empty fields.
/// [`ProfileImageEdit::Keep`] on both (what [`build_edited_profile`] passes) is
/// the text-only v1 behaviour.
///
/// The images themselves are ordinary blobs already uploaded through the
/// public-post blob path — `media.md` § Encryption at rest: avatar / banner
/// blobs are signed plaintext, "the same shape" as public-post attachments —
/// so this only records the resulting `ContentHash` on the signed record.
#[allow(clippy::too_many_arguments)]
pub fn build_edited_profile_with_images(
    keypair: &ActorKeypair,
    base_body: Option<&[u8]>,
    predecessors: &[ActorId],
    display_name: Option<String>,
    bio: Option<String>,
    links: Vec<ProfileLink>,
    avatar: ProfileImageEdit,
    banner: ProfileImageEdit,
) -> Result<Vec<u8>, ProfileWriteError> {
    let mut profile = match base_body {
        Some(b) => {
            // `admit_base`, never a bare `decode_profile`: it re-labels a base
            // a succession moved onto this identity, and it refuses or strips
            // what the nest could have chosen.
            let mut p = admit_base(keypair, b, predecessors)?;
            p.display_name = display_name;
            p.bio = bio;
            p.links = links;
            p.updated_at = Timestamp::now();
            p
        }
        None => Profile {
            actor_id: keypair.actor_id(),
            display_name,
            bio,
            avatar: None,
            banner: None,
            links,
            nests: vec![],
            admin_nests: vec![],
            load_hint: None,
            inbox_mode: InboxMode::Open,
            recovery_head: None,
            updated_at: Timestamp::now(),
        },
    };
    avatar.apply(&mut profile.avatar);
    banner.apply(&mut profile.banner);
    Ok(build_profile(keypair, &profile)?)
}

/// Mirror a newly registered RecoveryKey's chain head into the signed
/// profile — `identity-succession.md` § The RecoveryKey ("the user's signed
/// `Profile` gains an additive `recovery_head` field so peers cache the
/// binding with the profile they already hold"). The whole [`ChainHead`] is
/// mirrored, never the pubkey alone: a consumer needs both halves to anchor
/// `resolve_successor`'s rewrite/truncation guard, and
/// [`RecoveryKit`](fauna_core::recovery) ceremonies return both
/// (`RecoveryKit::chain_head`).
///
/// **Why this is its own function rather than a parameter on the edit path.**
/// The edit path above is the *user's edit form*: it overwrites every text
/// field from the form's current contents. Registering a recovery kit is not
/// an edit of those fields, and routing it through the form-shaped writer would
/// mean a caller that holds no form state must first reconstruct one — and any
/// field it reconstructs wrongly is silently published. This writer touches
/// exactly one field and preserves every other, including the ones the form
/// path also preserves (`nests` / `admin_nests` / `load_hint` / `inbox_mode`).
///
/// `base_body` is the caller's current stored profile, admitted exactly as the
/// edit path admits it ([`admit_base`], over the same `predecessors`). Passing
/// `None` is legitimate for an account that has never published one (the kit
/// ceremony can run before the profile does) and mints a minimal profile
/// carrying only the binding.
///
/// `home`, when given, fills the profile's **primary `nests` entry if — and
/// only if — the list is empty** (as it always is once admission has dropped a
/// `nests` list the signer did not sign itself): the home entry is what gives a peer-profile
/// harvest its dial domain (`identity-succession.md` § The succession
/// statement → *the peer-profile harvest*, whose residual paragraph declared
/// this producer a follow-on), and this landed-registration hop is the one
/// moment every app already republishes the profile. Fill-if-absent because
/// this is a background mirror, never an editor: a `nests` list the user (or a
/// future editor surface) authored is preserved verbatim.
///
/// The field is a **cache** — the registration chain stays authoritative — so a
/// failure to publish it costs peers a chain fetch, never correctness. Callers
/// should treat it that way and not fail a completed kit ceremony over it.
pub fn build_profile_with_recovery_head(
    keypair: &ActorKeypair,
    base_body: Option<&[u8]>,
    predecessors: &[ActorId],
    head: ChainHead,
    home: Option<&HomeNest>,
) -> Result<Vec<u8>, ProfileWriteError> {
    let mut profile = match base_body {
        Some(b) => admit_base(keypair, b, predecessors)?,
        None => Profile {
            actor_id: keypair.actor_id(),
            display_name: None,
            bio: None,
            avatar: None,
            banner: None,
            links: vec![],
            nests: vec![],
            admin_nests: vec![],
            load_hint: None,
            inbox_mode: InboxMode::Open,
            recovery_head: None,
            updated_at: Timestamp::now(),
        },
    };
    profile.recovery_head = Some(head);
    if let Some(home) = home
        && profile.nests.is_empty()
    {
        profile.nests = vec![fauna_core::data::NestEntry {
            nest_id: home.nest_id.map(|id| id.to_vec()).unwrap_or_default(),
            url: home.url.clone(),
            // The home nest is the everything-nest for the account: it
            // serves the social planes and accepts MLS delivery. No
            // consumer reads roles yet (the harvest reads only the URL's
            // host); these are the honest defaults for the primary home.
            roles: vec![
                fauna_core::data::NestRole::Social,
                fauna_core::data::NestRole::Mls,
            ],
        }];
    }
    profile.updated_at = Timestamp::now();
    Ok(build_profile(keypair, &profile)?)
}

/// The caller's home-nest facts for the profile's primary `nests` entry —
/// resolved app-side, because the TOFU pin store this reads from is a
/// per-platform connection-layer concern this wasm-safe crate deliberately
/// does not reach (native apps read
/// `fauna_anon_client::trust::pinned_identity`; the web SPA its localStorage
/// pin store).
pub struct HomeNest {
    /// The home nest's URL as the app dialed it. Consumers harvest only its
    /// host (`identity-succession.md`'s harvest rule 3 — the domain arm), but
    /// the full URL is the profile wire shape.
    pub url: String,
    /// The TOFU-pinned `nest_actor_id` (the nest's Ed25519 public key), when
    /// the caller holds one. `None` publishes an empty `nest_id` — honest
    /// "not known here", and the harvest reads only the URL.
    pub nest_id: Option<[u8; 32]>,
}

/// What went wrong publishing the recovery-head mirror
/// ([`publish_recovery_head`]).
///
/// Four variants rather than one because they mean different things to the
/// caller: a [`Fetch`](Self::Fetch) or [`Publish`](Self::Publish) is a
/// round-trip that can simply be retried on the next connect, while a
/// [`Refused`](Self::Refused) or [`Sign`](Self::Sign) means the stored profile
/// is not this identity's to re-sign, did not decode, or would not sign —
/// retrying that with the same inputs produces the same failure.
#[derive(Debug, thiserror::Error)]
pub enum RecoveryHeadError<E> {
    /// `fauna.profile.get` failed for a reason other than "never published"
    /// (which is not an error here — it mints a minimal profile instead).
    #[error("read the current profile: {0}")]
    Fetch(E),
    /// The stored profile was refused as the base ([`BaseRefusal`]). Nothing is
    /// published: the mirror never replaces a profile it cannot vouch for.
    #[error("rebuild the profile with the recovery head: {0}")]
    Refused(BaseRefusal),
    /// The stored profile did not decode, or the rebuilt one would not sign.
    #[error("rebuild the profile with the recovery head: {0}")]
    Sign(fauna_core::error::Error),
    /// `fauna.profile.set` refused or never arrived.
    #[error("publish the profile: {0}")]
    Publish(E),
}

/// Mirror a landed RecoveryKey registration's chain head into this identity's
/// signed profile — the read-modify-write hop
/// [`build_profile_with_recovery_head`] is the pure half of.
///
/// **Why this hop is shared rather than per-app glue.** It is the one step
/// `fauna-client-recovery`'s kit ceremonies deliberately leave to their caller
/// (that crate does not reach into the profile plane), and *every* app owes it
/// after *every* landed registration — so writing it once here is what stops
/// seven apps each re-deriving "fetch, decode, set one field, re-sign, publish"
/// and six of them getting the not-found arm wrong (priority #2).
///
/// **Call it where a registration actually LANDED and moved the chain**: kit
/// creation and kit replacement (`fauna_client_recovery::create_kit` /
/// `create_kit_with_root`, whose `RecoveryKit::chain_head` is exactly this
/// `head`). Deliberately **not** after a seed-alone replacement request — that
/// one only opens the 30-day window, and the chain head does not move until it
/// lands, so mirroring there would publish a binding no consumer should honor
/// yet (`identity-succession.md` § The RecoveryKey → *Replacement*).
///
/// **A never-published profile is not a failure.** The kit ceremony can
/// legitimately run before the user has ever published a profile (the
/// onboarding kit screen precedes every profile edit), so
/// `fauna.profile.not_found` mints a minimal profile carrying only the binding
/// rather than aborting. Every other refusal aborts — a profile that failed to
/// decode must not be silently replaced by a minimal one, which would erase
/// the user's display name and bio — and the same goes for one refused as
/// the base ([`RecoveryHeadError::Refused`]).
///
/// `predecessors` is the signer's registry-recorded predecessor set, as
/// [`build_edited_profile`] takes it.
///
/// **The field is a cache; the registration chain stays authoritative**
/// (`identity-succession.md` § The RecoveryKey). A failure here costs peers a
/// chain fetch, never correctness — so a caller must log it and carry on, never
/// fail a completed kit ceremony over it. The secret is already minted and
/// registered by the time this runs; turning a mirror failure into a ceremony
/// failure would report "no kit" for a kit that exists.
pub async fn publish_recovery_head<R>(
    nest: R,
    keypair: &ActorKeypair,
    predecessors: &[ActorId],
    head: ChainHead,
    home: Option<HomeNest>,
) -> Result<(), RecoveryHeadError<R::Error>>
where
    R: RpcRequester,
    R::Error: fauna_protocol::RpcErrorClass,
{
    let client = ProfileClient::new(nest);
    let base = match client.profile_get(keypair.actor_id().to_hex()).await {
        Ok(reply) => Some(reply.body.into_vec()),
        Err(e) if is_profile_not_found(&e) => None,
        Err(e) => return Err(RecoveryHeadError::Fetch(e)),
    };
    // A base this device's registry cannot place may still be the signer's own
    // inherited row: ask the public record, and admit only what the signer's
    // own `new_sig` proves ([`proven_predecessors`]). Free on every other base.
    let mut predecessors = predecessors.to_vec();
    if let Some(base) = &base {
        predecessors.extend(prove_base(&client, &keypair.actor_id(), base, &predecessors).await);
    }
    let body = build_profile_with_recovery_head(
        keypair,
        base.as_deref(),
        &predecessors,
        head,
        home.as_ref(),
    )
    .map_err(|e| match e {
        ProfileWriteError::Refused(r) => RecoveryHeadError::Refused(r),
        ProfileWriteError::Encoding(e) => RecoveryHeadError::Sign(e),
    })?;
    client
        .profile_set(body)
        .await
        .map_err(RecoveryHeadError::Publish)?;
    Ok(())
}

/// What [`restore_delegated_anchors`] found. Every arm but
/// [`Published`](Self::Published) wrote nothing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AnchorRestore {
    /// The stored profile is not a delegated profile of this identity — its
    /// own direct publish, a never-published one, or anything else the writers
    /// would refuse anyway. Nothing was asked of the nest.
    NotDelegated,
    /// Delegated, but the identity has registered no RecoveryKey: there is no
    /// head to publish, and the device publishes nothing rather than a
    /// domain-only profile write for the sake of it.
    NoChain,
    /// The nest served a chain that does not verify as this identity's own.
    /// Nothing is published over it: the head must come from a chain the
    /// device checked, never from the nest's word.
    Unverified,
    /// The verified head and the home `nests` entry were re-published under
    /// the identity key.
    Published,
}

/// What went wrong restoring the anchors ([`restore_delegated_anchors`]).
/// Both arms are best-effort failures: the anchors stay absent, and the next
/// sign-in tries again.
#[derive(Debug, thiserror::Error)]
pub enum AnchorRestoreError<E> {
    /// `fauna.recovery.registration.chain` failed.
    #[error("read the registration chain: {0}")]
    Chain(E),
    /// The re-publish itself failed ([`publish_recovery_head`]).
    #[error(transparent)]
    Publish(#[from] RecoveryHeadError<E>),
}

/// Put the two identity-signed harvest anchors — `recovery_head` and the home
/// `nests` entry — back on a profile an external app last wrote, the producer
/// `identity-succession.md` § the peer-profile harvest, rule 1's honest-flow
/// paragraph rules. Run once per sign-in, over the base that sign-in already
/// read (`base_body`, the stored bytes, or nothing when never published).
///
/// **Why it exists.** A profile edited through an external app is signed by the
/// nest-held authoring sub-key (`AuthoringOrigin::Delegated`). The harvest
/// refuses it outright, and the writers' admission rule ([`admit_base`] rule 3)
/// drops both anchors from it, so peers meeting the user afterwards learn
/// neither — until one of the user's own apps publishes them again. Without
/// this, that waits for the next kit landing, which may be months away.
///
/// **Where the head comes from.** Never the served profile — that is the very
/// value the harvest refuses to trust. The device fetches the identity's own
/// registration chain and verifies it itself
/// ([`fauna_core::recovery::verify_registration_chain`]): every link carries
/// the identity seed's own signature, so the nest can withhold links but cannot
/// mint one. The grade is the one a peer gets from its own cache-miss fetch of
/// the same chain — never better, never worse. No chain → nothing is
/// published.
///
/// **Why it cannot ping-pong.** It acts only when the stored profile is
/// delegated, it writes a direct one, and it runs once per sign-in. An external
/// app that re-writes the profile after every change therefore costs one
/// extra write per sign-in at most, never a loop; and on the ordinary path —
/// a direct profile — it asks the nest nothing at all.
///
/// `predecessors` and `home` are [`publish_recovery_head`]'s.
pub async fn restore_delegated_anchors<R>(
    nest: R,
    keypair: &ActorKeypair,
    predecessors: &[ActorId],
    base_body: Option<&[u8]>,
    home: Option<HomeNest>,
) -> Result<AnchorRestore, AnchorRestoreError<R::Error>>
where
    R: RpcRequester,
    R::Error: fauna_protocol::RpcErrorClass,
{
    let own = keypair.actor_id();
    let delegated = base_body.is_some_and(|b| {
        matches!(
            decode_profile(b),
            Ok((profile, AuthoringOrigin::Delegated { .. })) if profile.actor_id == own
        )
    });
    if !delegated {
        return Ok(AnchorRestore::NotDelegated);
    }
    let reply: fauna_protocol::recovery::RegistrationChainReply = nest
        .request(
            REGISTRATION_CHAIN_KIND,
            fauna_protocol::recovery::RegistrationChainRequest {
                actor_id: fauna_protocol::ByteBuf::from(own.0.to_vec()),
                ..Default::default()
            },
        )
        .await
        .map_err(AnchorRestoreError::Chain)?;
    if reply.registrations.is_empty() {
        return Ok(AnchorRestore::NoChain);
    }
    let Ok(chain) = reply
        .registrations
        .iter()
        .map(|b| {
            fauna_core::encoding::canonical_decode::<
                fauna_core::recovery::SignedRecoveryKeyRegistration,
            >(b.as_ref())
        })
        .collect::<Result<Vec<_>, _>>()
    else {
        return Ok(AnchorRestore::Unverified);
    };
    let Ok(head) = fauna_core::recovery::verify_registration_chain(own, &chain, None) else {
        return Ok(AnchorRestore::Unverified);
    };
    publish_recovery_head(nest, keypair, predecessors, head, home).await?;
    Ok(AnchorRestore::Published)
}

/// `fauna.recovery.registration.chain` — spelled here rather than borrowed from
/// `fauna-client-recovery`, which depends on this crate, not the reverse.
const REGISTRATION_CHAIN_KIND: &str = "fauna.recovery.registration.chain";

/// Whether a `fauna.profile.get` error is the "never published one" refusal —
/// read through the [`RpcErrorClass`](fauna_protocol::RpcErrorClass) seam so
/// the same code answers for the native and wasm transports, neither of whose
/// concrete error type this crate names.
fn is_profile_not_found<E: fauna_protocol::RpcErrorClass>(e: &E) -> bool {
    e.as_rpc_error()
        .is_some_and(|r| r.code == fauna_protocol::RpcError::CODE_PROFILE_NOT_FOUND)
}

/// Typed `fauna.profile.*` call surface, generic over the WS-RPC transport
/// (`R: RpcRequester`): native call sites pass `Arc<NestClient>`, the wasm SPA
/// passes its `WsRpcClient`. Errors propagate as the transport's `R::Error`.
pub struct ProfileClient<R: RpcRequester> {
    nest: R,
}

impl<R: RpcRequester> ProfileClient<R> {
    pub fn new(nest: R) -> Self {
        Self { nest }
    }

    /// `fauna.profile.get` — fetch a user's profile record by hex `actor_id`
    /// (the viewer's own or anyone else's). Pure read; replay-safe at 5 s. The
    /// reply carries the raw stored bytes (`body`) — decode them with
    /// [`decode_profile`]. A missing profile surfaces as a
    /// `fauna.profile.not_found` `R::Error`.
    pub async fn profile_get(
        &self,
        actor_id: impl Into<String>,
    ) -> Result<ProfileGetReply, R::Error> {
        self.nest
            .request(
                "fauna.profile.get",
                ProfileGetRequest {
                    actor_id: actor_id.into(),
                    extra: std::collections::BTreeMap::new(),
                },
            )
            .await
    }

    /// `fauna.profile.set` — publish/replace the caller's own profile. `body`
    /// is the signed `EmbedAsBytes` wire from [`build_profile`]. The nest
    /// verifies the signature, asserts the inner `actor_id` is the caller, then
    /// stores it (append + keep-latest prune). Immediate; replay-safe at 10 s
    /// (content-addressed, idempotent). Mirrors `fauna-client-posts`'s
    /// `posts_create(body)`.
    pub async fn profile_set(&self, body: Vec<u8>) -> Result<ProfileSetReply, R::Error> {
        self.nest
            .request(
                "fauna.profile.set",
                ProfileSetRequest {
                    body: fauna_protocol::ByteBuf::from(body),
                    extra: std::collections::BTreeMap::new(),
                },
            )
            .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fauna_client_testkit::{RecordingRequester, block_on};
    use fauna_protocol::profile::{ProfileGetRequest, ProfileSetRequest};
    use serde_bytes::ByteBuf;

    /// This crate's reply table for the shared [`RecordingRequester`]:
    /// one arm per kind, each the minimal valid shape its `Reply` decodes.
    fn reply(kind: &'static str) -> Vec<u8> {
        match kind {
            "fauna.profile.get" => {
                fauna_protocol::encode_canonical(&fauna_protocol::profile::ProfileGetReply {
                    body: ByteBuf::from(vec![0x01, 0x02]),
                    extra: Default::default(),
                })
            }
            "fauna.profile.set" => {
                fauna_protocol::encode_canonical(&fauna_protocol::profile::ProfileSetReply {
                    extra: Default::default(),
                })
            }
            other => panic!("RecordingRequester: unhandled kind {other}"),
        }
        .expect("encode reply")
        .to_vec()
    }

    fn hex32() -> String {
        "ab".repeat(32)
    }

    #[test]
    fn profile_get_composes_kind_and_payload() {
        let rec = std::sync::Arc::new(RecordingRequester::new(reply));
        let client = ProfileClient::new(rec.clone());
        block_on(client.profile_get(hex32())).expect("infallible mock");
        let (kind, payload) = rec.recorded();
        assert_eq!(kind, "fauna.profile.get");
        let req: ProfileGetRequest = fauna_protocol::decode_strict(&payload).expect("decodes");
        assert_eq!(req.actor_id, hex32());
    }

    #[test]
    fn profile_set_composes_kind_and_payload() {
        let rec = std::sync::Arc::new(RecordingRequester::new(reply));
        let client = ProfileClient::new(rec.clone());
        block_on(client.profile_set(vec![0xDE, 0xAD, 0xBE, 0xEF])).expect("infallible mock");
        let (kind, payload) = rec.recorded();
        assert_eq!(kind, "fauna.profile.set");
        let req: ProfileSetRequest = fauna_protocol::decode_strict(&payload).expect("decodes");
        assert_eq!(req.body, ByteBuf::from(vec![0xDE, 0xAD, 0xBE, 0xEF]));
    }

    // ── build_profile (sign) → decode_profile (verify) round-trip ────────

    use fauna_core::data::{InboxMode, Timestamp};
    use fauna_core::identity::{ActorId, ActorKeypair};

    fn sample_profile(actor: [u8; 32]) -> Profile {
        Profile {
            actor_id: ActorId(actor),
            display_name: Some("Alice".into()),
            bio: Some("hi".into()),
            avatar: None,
            banner: None,
            links: vec![],
            nests: vec![],
            admin_nests: vec![],
            load_hint: None,
            inbox_mode: InboxMode::Open,
            recovery_head: None,
            updated_at: Timestamp(0),
        }
    }

    #[test]
    fn build_profile_signs_and_decode_profile_verifies() {
        let kp = ActorKeypair::generate();
        let profile = sample_profile(kp.actor_id().0);
        let body = build_profile(&kp, &profile).expect("build+sign");
        let (decoded, _) = decode_profile(&body).expect("decode+verify");
        assert_eq!(decoded.actor_id.0, profile.actor_id.0);
        assert_eq!(decoded.display_name, profile.display_name);
        assert_eq!(decoded.bio, profile.bio);
    }

    fn homed_at(kp: &ActorKeypair, urls: &[&str]) -> Vec<u8> {
        let mut profile = sample_profile(kp.actor_id().0);
        profile.nests = urls
            .iter()
            .map(|u| fauna_core::data::NestEntry {
                nest_id: vec![7u8; 32],
                url: (*u).to_string(),
                roles: vec![fauna_core::data::NestRole::Social],
            })
            .collect();
        build_profile(kp, &profile).expect("build+sign")
    }

    #[test]
    fn a_knock_routes_to_the_profiles_home_nest_only_when_it_is_foreign() {
        let kp = ActorKeypair::generate();
        let me = "https://home.example:8443";
        let foreign = homed_at(
            &kp,
            &["https://peer.example:9000/", "https://other.example"],
        );
        assert_eq!(
            knock_recipient_nest_url(&kp.actor_id(), &foreign, me).as_deref(),
            Some("https://peer.example:9000/"),
            "the first entry is the home nest",
        );
        // Same authority, spelled differently (case, trailing path) → local.
        let same = homed_at(&kp, &["https://HOME.example:8443/"]);
        assert_eq!(knock_recipient_nest_url(&kp.actor_id(), &same, me), None);
        // Same host, other port: a different nest (the e2e two-nest shape).
        let other_port = homed_at(&kp, &["https://home.example:9443"]);
        assert_eq!(
            knock_recipient_nest_url(&kp.actor_id(), &other_port, me).as_deref(),
            Some("https://home.example:9443"),
        );
        // No nest named → local.
        let none = homed_at(&kp, &[]);
        assert_eq!(knock_recipient_nest_url(&kp.actor_id(), &none, me), None);
    }

    #[test]
    fn a_knock_route_never_comes_from_someone_elses_or_an_unreadable_profile() {
        let kp = ActorKeypair::generate();
        let body = homed_at(&kp, &["https://peer.example"]);
        let stranger = ActorKeypair::generate().actor_id();
        assert_eq!(
            knock_recipient_nest_url(&stranger, &body, "https://home.example"),
            None,
            "a profile about another actor must not route this knock",
        );
        assert_eq!(
            knock_recipient_nest_url(&kp.actor_id(), &[0x01, 0x02], "https://home.example"),
            None,
        );
    }

    #[test]
    fn build_profile_rejects_actor_id_keypair_mismatch() {
        let kp = ActorKeypair::generate();
        // Profile claims a different actor than the signing keypair.
        let profile = sample_profile([3u8; 32]);
        assert!(
            build_profile(&kp, &profile).is_err(),
            "a profile whose actor_id != signing keypair must not build"
        );
    }

    // ── build_edited_profile / decode_profile_display (read-modify-write) ──

    use fauna_core::data::{AdminNestEntry, ProfileLink};

    /// A base profile with NON-display fields set to non-defaults so the
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
            updated_at: Timestamp(0),
        }
    }

    #[test]
    fn build_edited_profile_overwrites_display_preserves_rest() {
        let kp = ActorKeypair::generate();
        let base_bytes = build_profile(&kp, &base_profile(&kp)).expect("build base");

        let edited = build_edited_profile(
            &kp,
            Some(&base_bytes),
            &[],
            Some("new".into()),
            Some("newbio".into()),
            vec![ProfileLink {
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
        // Non-display fields preserved from the base.
        assert_eq!(decoded.admin_nests.len(), 1);
        assert_eq!(decoded.admin_nests[0].name, "My Nest");
        assert_eq!(decoded.inbox_mode, InboxMode::ContactsOnly);
        assert_eq!(decoded.recovery_head, Some(ChainHead::new([0xA7; 32], 3)));
    }

    #[test]
    fn build_edited_profile_first_publish_uses_defaults() {
        let kp = ActorKeypair::generate();
        let fresh = build_edited_profile(&kp, None, &[], Some("fresh".into()), None, vec![])
            .expect("build fresh");

        let (decoded, _) = decode_profile(&fresh).expect("decode fresh");
        assert_eq!(decoded.display_name, Some("fresh".into()));
        assert_eq!(decoded.bio, None);
        assert!(decoded.admin_nests.is_empty());
        assert_eq!(decoded.inbox_mode, InboxMode::Open);
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

        let display = decode_profile_display(&bytes).expect("decode display");
        assert_eq!(display.display_name, Some("old".into()));
        assert_eq!(display.bio, Some("oldbio".into()));
        assert_eq!(display.links.len(), 1);
        assert_eq!(display.links[0].label, "site");
    }

    // ── avatar / banner: the three-state image edit ──────────────────────

    /// A base profile that already HAS both pictures, so the Keep / Clear /
    /// Set assertions below each have something to act on.
    fn base_profile_with_images(kp: &ActorKeypair) -> Profile {
        let mut p = base_profile(kp);
        p.avatar = Some(ContentHash::from_digest_raw([0x11; 32]));
        p.banner = Some(ContentHash::from_digest_raw([0x22; 32]));
        p
    }

    #[test]
    fn text_only_edit_keeps_both_pictures() {
        // The v1 form's exact call — the regression guard for every app
        // still on `build_edited_profile`: a text edit must not drop a picture.
        let kp = ActorKeypair::generate();
        let base = build_profile(&kp, &base_profile_with_images(&kp)).expect("build base");

        let edited = build_edited_profile(&kp, Some(&base), &[], Some("new".into()), None, vec![])
            .expect("build edited");

        let (decoded, _) = decode_profile(&edited).expect("decode edited");
        assert_eq!(decoded.display_name, Some("new".into()));
        assert_eq!(
            decoded.avatar,
            Some(ContentHash::from_digest_raw([0x11; 32]))
        );
        assert_eq!(
            decoded.banner,
            Some(ContentHash::from_digest_raw([0x22; 32]))
        );
    }

    #[test]
    fn set_replaces_and_clear_removes_independently() {
        let kp = ActorKeypair::generate();
        let base = build_profile(&kp, &base_profile_with_images(&kp)).expect("build base");
        let new_avatar = "cd".repeat(32);

        // Avatar set to a freshly uploaded blob; banner removed. The two
        // fields move independently in one save.
        let edited = build_edited_profile_with_images(
            &kp,
            Some(&base),
            &[],
            Some("new".into()),
            None,
            vec![],
            ProfileImageEdit::set_from_hex(&new_avatar).expect("valid hex"),
            ProfileImageEdit::Clear,
        )
        .expect("build edited");

        let (decoded, _) = decode_profile(&edited).expect("decode edited");
        assert_eq!(
            decoded.avatar,
            Some(ContentHash::from_digest_raw([0xCD; 32])),
            "avatar points at the newly uploaded blob"
        );
        assert_eq!(decoded.banner, None, "banner was cleared");
        // The rest of the identity still survives an image edit.
        assert_eq!(decoded.inbox_mode, InboxMode::ContactsOnly);
        assert_eq!(decoded.recovery_head, Some(ChainHead::new([0xA7; 32], 3)));
    }

    #[test]
    fn first_publish_can_carry_a_picture() {
        let kp = ActorKeypair::generate();
        let fresh = build_edited_profile_with_images(
            &kp,
            None,
            &[],
            Some("fresh".into()),
            None,
            vec![],
            ProfileImageEdit::set_from_hex(&"ef".repeat(32)).expect("valid hex"),
            ProfileImageEdit::Keep,
        )
        .expect("build fresh");

        let (decoded, _) = decode_profile(&fresh).expect("decode fresh");
        assert_eq!(
            decoded.avatar,
            Some(ContentHash::from_digest_raw([0xEF; 32]))
        );
        assert_eq!(decoded.banner, None, "Keep on a fresh profile stays None");
    }

    #[test]
    fn decode_profile_display_projects_the_image_refs() {
        let kp = ActorKeypair::generate();
        let bytes = build_profile(&kp, &base_profile_with_images(&kp)).expect("build base");

        let display = decode_profile_display(&bytes).expect("decode display");
        assert_eq!(
            display.avatar,
            Some(ContentHash::from_digest_raw([0x11; 32]))
        );
        assert_eq!(
            display.banner,
            Some(ContentHash::from_digest_raw([0x22; 32]))
        );
    }

    #[test]
    fn image_edit_from_parts_maps_the_three_states() {
        assert_eq!(
            ProfileImageEdit::from_parts(false, None).expect("keep"),
            ProfileImageEdit::Keep
        );
        assert_eq!(
            ProfileImageEdit::from_parts(true, None).expect("clear"),
            ProfileImageEdit::Clear
        );
        // `clear` wins over a supplied hash — a remove is unambiguous.
        assert_eq!(
            ProfileImageEdit::from_parts(true, Some(&"ab".repeat(32))).expect("clear wins"),
            ProfileImageEdit::Clear
        );
        assert_eq!(
            ProfileImageEdit::from_parts(false, Some(&"ab".repeat(32))).expect("set"),
            ProfileImageEdit::Set(ContentHash::from_digest_raw([0xAB; 32]))
        );
    }

    #[test]
    fn image_edit_rejects_malformed_blob_hash() {
        assert!(
            ProfileImageEdit::set_from_hex("nothex").is_err(),
            "non-hex must not build"
        );
        assert!(
            ProfileImageEdit::set_from_hex(&"ab".repeat(16)).is_err(),
            "a 32-hex-char (16-byte) digest is the wrong length"
        );
    }

    #[test]
    fn the_recovery_head_mirror_preserves_every_other_field() {
        // `identity-succession.md` § The RecoveryKey — registering a kit caches
        // the binding on the signed profile. It must not be a back door that
        // silently blanks the fields the user actually authored.
        let keypair = ActorKeypair::generate();
        let base = build_edited_profile(
            &keypair,
            None,
            &[],
            Some("Ada".into()),
            Some("counts things".into()),
            vec![ProfileLink {
                label: "site".into(),
                uri: "https://example.test".into(),
            }],
        )
        .expect("base profile");

        let mirrored = build_profile_with_recovery_head(
            &keypair,
            Some(&base),
            &[],
            ChainHead::new([0x7A; 32], 1),
            None,
        )
        .expect("mirror");

        let (profile, _) = decode_profile(&mirrored).expect("decode");
        assert_eq!(profile.recovery_head, Some(ChainHead::new([0x7A; 32], 1)));
        assert_eq!(profile.display_name.as_deref(), Some("Ada"));
        assert_eq!(profile.bio.as_deref(), Some("counts things"));
        assert_eq!(profile.links.len(), 1, "authored links survive the mirror");
        assert_eq!(profile.actor_id, keypair.actor_id());
    }

    /// A successor's recovery-head mirror over a profile the succession moved
    /// onto it. Before 2026-08-23 this returned
    /// `build_profile: profile.actor_id does not match the signing keypair`,
    /// so the mirror was a permanent no-op for the one case it matters most in
    /// — measured on the first `--app macos` succession
    /// run.
    #[test]
    fn the_mirror_adopts_a_profile_a_succession_moved_onto_this_identity() {
        let predecessor = ActorKeypair::generate();
        let successor = ActorKeypair::generate();

        // What the nest still serves for the successor's own actor id: the
        // predecessor's signed bytes, on a row whose ownership already moved.
        let inherited = build_profile(
            &predecessor,
            &Profile {
                actor_id: predecessor.actor_id(),
                display_name: Some("Ada".into()),
                bio: Some("counts things".into()),
                avatar: None,
                banner: None,
                links: vec![],
                nests: vec![],
                admin_nests: vec![],
                load_hint: None,
                inbox_mode: InboxMode::Open,
                recovery_head: None,
                updated_at: Timestamp::now(),
            },
        )
        .expect("the predecessor's own publish");

        let mirrored = build_profile_with_recovery_head(
            &successor,
            Some(&inherited),
            &[predecessor.actor_id()],
            ChainHead::new([0x7A; 32], 1),
            None,
        )
        .expect("the successor mirrors its head onto the row it now owns");

        let (profile, _) = decode_profile(&mirrored).expect("decode");
        assert_eq!(
            profile.actor_id,
            successor.actor_id(),
            "the new version is authored BY the successor — signed by it, and saying so"
        );
        assert_eq!(profile.recovery_head, Some(ChainHead::new([0x7A; 32], 1)));
        assert_eq!(
            profile.display_name.as_deref(),
            Some("Ada"),
            "the user's own fields travel with the account; only authorship changes"
        );
        assert_eq!(profile.bio.as_deref(), Some("counts things"));
    }

    /// The larger half of the same defect, and the one no surface would have
    /// reported as a succession problem: a successor's **ordinary profile
    /// edit** failed to sign for exactly the same reason.
    #[test]
    fn a_successor_can_edit_the_profile_it_inherited() {
        let predecessor = ActorKeypair::generate();
        let successor = ActorKeypair::generate();

        let inherited = build_profile(
            &predecessor,
            &Profile {
                actor_id: predecessor.actor_id(),
                display_name: Some("Ada".into()),
                bio: Some("counts things".into()),
                avatar: None,
                banner: None,
                links: vec![],
                nests: vec![],
                admin_nests: vec![],
                load_hint: None,
                inbox_mode: InboxMode::Open,
                recovery_head: None,
                updated_at: Timestamp::now(),
            },
        )
        .expect("the predecessor's own publish");

        let edited = build_edited_profile(
            &successor,
            Some(&inherited),
            &[predecessor.actor_id()],
            Some("Ada Lovelace".into()),
            Some("still counting".into()),
            vec![],
        )
        .expect("the successor edits the profile it now owns");

        let (profile, _) = decode_profile(&edited).expect("decode");
        assert_eq!(profile.actor_id, successor.actor_id());
        assert_eq!(profile.display_name.as_deref(), Some("Ada Lovelace"));
        assert_eq!(profile.bio.as_deref(), Some("still counting"));
    }

    /// The guard that must NOT be softened by the above: a hand-assembled
    /// `Profile` naming somebody else is still a caller bug, and
    /// [`build_profile`] still refuses it. Only a *decoded base* is adopted,
    /// and only because every writer fetches that base by its own actor id.
    #[test]
    fn build_profile_still_refuses_a_hand_assembled_foreign_actor() {
        let mine = ActorKeypair::generate();
        let theirs = ActorKeypair::generate();

        let err = build_profile(
            &mine,
            &Profile {
                actor_id: theirs.actor_id(),
                display_name: None,
                bio: None,
                avatar: None,
                banner: None,
                links: vec![],
                nests: vec![],
                admin_nests: vec![],
                load_hint: None,
                inbox_mode: InboxMode::Open,
                recovery_head: None,
                updated_at: Timestamp::now(),
            },
        );
        assert!(
            err.is_err(),
            "the defensive assertion is the base-adoption's counterpart, not its casualty"
        );
    }

    // ── base admission: the nest's word is not the owner's ───────────────
    //
    // `profile.md` § After an identity succession …, amended 2026-09-19. Each
    // pin below reddens on its own mutation of `admit_base`; the control
    // (`an_own_direct_base_round_trips_its_anchors`) proves the honest path
    // still carries every field.

    /// A base carrying a doctored harvest anchor: a chain head no kit of the
    /// owner's ever registered, and a home the owner never dialed.
    fn anchored_profile(actor: ActorId) -> Profile {
        Profile {
            actor_id: actor,
            display_name: Some("Ada".into()),
            bio: Some("counts things".into()),
            avatar: Some(ContentHash::from_digest_raw([0x11; 32])),
            banner: None,
            links: vec![],
            nests: vec![fauna_core::data::NestEntry {
                nest_id: vec![0xEE; 32],
                url: "https://attacker.example.net".into(),
                roles: vec![],
            }],
            admin_nests: vec![AdminNestEntry {
                nest_id: vec![7u8; 32],
                url: "https://nest.example".into(),
                name: "My Nest".into(),
            }],
            load_hint: None,
            inbox_mode: InboxMode::ContactsOnly,
            recovery_head: Some(ChainHead::new([0xAA; 32], 9)),
            updated_at: Timestamp(0),
        }
    }

    /// `profile` owned by `owner`, signed by a delegated authoring sub-key
    /// under an owner-signed `UpdateProfile` cert — the shape an external-app
    /// edit through the PDS write path is stored as (D10), and the key the
    /// nest itself holds.
    fn delegated_wire(owner: &ActorKeypair, profile: &Profile) -> Vec<u8> {
        use fauna_core::data::{Capability, DeviceAuthorization};
        use fauna_core::encoding::{EmbedAsBytes, canonical_encode, sign_envelope};
        let sub = ActorKeypair::generate();
        let cert = DeviceAuthorization {
            actor_id: owner.actor_id(),
            device_key: sub.actor_id().0,
            capabilities: vec![Capability::UpdateProfile],
            created_at: Timestamp(0),
            expires_at: None,
        };
        let (cert_bytes, cert_env) = sign_envelope(owner, &cert).expect("sign cert");
        let (bytes, env) = sign_envelope(&sub, profile).expect("sign with the sub-key");
        let wire = EmbedAsBytes::from_signed(bytes, env)
            .with_signer_auth(EmbedAsBytes::from_signed(cert_bytes, cert_env));
        canonical_encode(&wire).expect("encode the delegated wire")
    }

    fn edit(
        kp: &ActorKeypair,
        base: &[u8],
        predecessors: &[ActorId],
    ) -> Result<Profile, ProfileWriteError> {
        let body = build_edited_profile(
            kp,
            Some(base),
            predecessors,
            Some("new".into()),
            None,
            vec![],
        )?;
        Ok(decode_profile(&body).expect("decode the edit").0)
    }

    fn mirror(
        kp: &ActorKeypair,
        base: &[u8],
        predecessors: &[ActorId],
    ) -> Result<Profile, ProfileWriteError> {
        let body = build_profile_with_recovery_head(
            kp,
            Some(base),
            predecessors,
            ChainHead::new([0x3E; 32], 4),
            Some(&HomeNest {
                url: "https://home.example".into(),
                nest_id: None,
            }),
        )?;
        Ok(decode_profile(&body).expect("decode the mirror").0)
    }

    #[test]
    fn an_unsigned_base_naming_the_owner_is_refused() {
        // The unsigned door: a bare profile names the owner and carries the
        // nest's head. It never decodes (`decode_profile` is signed-only), so
        // both writers refuse it — never adopted, never swapped for defaults.
        let owner = ActorKeypair::generate();
        let bare = fauna_core::encoding::canonical_encode(&anchored_profile(owner.actor_id()))
            .expect("encode the bare profile");
        for (label, result) in [
            ("edit", edit(&owner, &bare, &[])),
            ("mirror", mirror(&owner, &bare, &[])),
        ] {
            assert!(
                matches!(result, Err(ProfileWriteError::Encoding(_))),
                "{label}: an unsigned base must be refused, got {result:?}"
            );
        }
    }

    #[test]
    fn a_foreign_signed_base_is_refused() {
        // The foreign door: a genuine signed profile of an identity that is neither the
        // signer nor a listed predecessor — a stranger's, or one the nest
        // minted itself. Listing SOME predecessor does not widen the door.
        let owner = ActorKeypair::generate();
        let stranger = ActorKeypair::generate();
        let listed = ActorKeypair::generate().actor_id();
        let foreign = build_profile(&stranger, &anchored_profile(stranger.actor_id()))
            .expect("the stranger's own publish");
        for (label, result) in [
            ("edit", edit(&owner, &foreign, &[listed])),
            ("mirror", mirror(&owner, &foreign, &[listed])),
        ] {
            assert!(
                matches!(
                    &result,
                    Err(ProfileWriteError::Refused(BaseRefusal::NotOwn { actor }))
                        if *actor == stranger.actor_id()
                ),
                "{label}: a foreign-signed base must be refused, got {result:?}"
            );
        }
    }

    #[test]
    fn a_predecessor_signed_base_is_adopted_without_its_anchors() {
        // The honest succession path keeps working — the row is re-published
        // under the successor with the user's own fields — but the
        // predecessor's head and home are another identity's word, so they
        // degrade to absent (the mirror then writes the successor's own).
        let predecessor = ActorKeypair::generate();
        let successor = ActorKeypair::generate();
        let inherited = build_profile(&predecessor, &anchored_profile(predecessor.actor_id()))
            .expect("the predecessor's own publish");

        let edited = edit(&successor, &inherited, &[predecessor.actor_id()]).expect("adopted");
        assert_eq!(edited.actor_id, successor.actor_id());
        assert_eq!(
            edited.recovery_head, None,
            "the predecessor's head is not carried"
        );
        assert!(
            edited.nests.is_empty(),
            "the predecessor's home is not carried"
        );
        assert_eq!(edited.display_name.as_deref(), Some("new"));
        assert_eq!(edited.bio, None, "the form's value wins");
        assert_eq!(
            edited.avatar,
            Some(ContentHash::from_digest_raw([0x11; 32]))
        );
        assert_eq!(edited.inbox_mode, InboxMode::ContactsOnly);
        assert_eq!(edited.admin_nests.len(), 1);

        let mirrored = mirror(&successor, &inherited, &[predecessor.actor_id()]).expect("adopted");
        assert_eq!(mirrored.recovery_head, Some(ChainHead::new([0x3E; 32], 4)));
        assert_eq!(mirrored.nests.len(), 1);
        assert_eq!(
            mirrored.nests[0].url, "https://home.example",
            "the successor's own home fills the slot the predecessor's list vacated"
        );
    }

    #[test]
    fn a_delegated_own_base_is_adopted_without_its_anchors() {
        // A profile edit made through an external app is signed by the
        // nest-held authoring sub-key. It is still the owner's profile, so an
        // edit over it must work — but its head and home are what that key
        // chose, and re-signing them with the identity key would launder them
        // into the harvest's anchor.
        let owner = ActorKeypair::generate();
        let delegated = delegated_wire(&owner, &anchored_profile(owner.actor_id()));
        assert!(
            matches!(
                decode_profile(&delegated).expect("decodes").1,
                AuthoringOrigin::Delegated { .. }
            ),
            "the fixture really is a delegated edit"
        );

        let edited = edit(&owner, &delegated, &[]).expect("a delegated own base is admitted");
        assert_eq!(edited.recovery_head, None);
        assert!(edited.nests.is_empty());
        assert_eq!(edited.display_name.as_deref(), Some("new"));
        assert_eq!(edited.inbox_mode, InboxMode::ContactsOnly);
        assert_eq!(edited.admin_nests.len(), 1);
        assert_eq!(
            edited.avatar,
            Some(ContentHash::from_digest_raw([0x11; 32]))
        );

        let mirrored = mirror(&owner, &delegated, &[]).expect("admitted");
        assert_eq!(mirrored.nests[0].url, "https://home.example");
    }

    #[test]
    fn an_own_direct_base_round_trips_its_anchors() {
        // The control: what the owner signed with its own key, it keeps.
        let owner = ActorKeypair::generate();
        let own = build_profile(&owner, &anchored_profile(owner.actor_id())).expect("own publish");

        let edited = edit(&owner, &own, &[]).expect("own base");
        assert_eq!(edited.recovery_head, Some(ChainHead::new([0xAA; 32], 9)));
        assert_eq!(edited.nests.len(), 1);
        assert_eq!(edited.nests[0].url, "https://attacker.example.net");

        let mirrored = mirror(&owner, &own, &[]).expect("own base");
        assert_eq!(
            mirrored.nests[0].url, "https://attacker.example.net",
            "an authored list is never displaced"
        );
    }

    /// An older device editing a profile a newer build signed keeps what it
    /// cannot read: a role, a load hint and an inbox mode this build does not
    /// name survive the read-modify-write re-sign (`transport.md` § Schema and
    /// forward-compat discipline → *Rule 3 in full* — open and carrying
    /// wherever a reader writes the value back out). The newer writer's shape
    /// of each value is pinned in `fauna-core/tests/unknown_variant_arms.rs`.
    #[test]
    fn a_re_sign_keeps_the_values_a_newer_build_wrote() {
        use fauna_core::carried::CarriedValue;
        use fauna_core::data::{AccountLoadHint, NestRole};
        use fauna_core::encoding::{canonical_decode, canonical_encode};

        // `{"Window": [10, 20]}` — a newer `AccountLoadHint::Window(u32, u32)`.
        let window: CarriedValue = canonical_decode(
            &canonical_encode(&std::collections::BTreeMap::from([(
                "Window",
                (10u32, 20u32),
            )]))
            .unwrap(),
        )
        .unwrap();
        let owner = ActorKeypair::generate();
        let mut newer = anchored_profile(owner.actor_id());
        newer.nests[0].roles = vec![NestRole::Social, NestRole::Other("Relay".into())];
        newer.load_hint = Some(AccountLoadHint::Unknown(window.clone()));
        newer.inbox_mode = InboxMode::Other("Moderated".into());
        let base = build_profile(&owner, &newer).expect("the newer build's publish");

        let edited = edit(&owner, &base, &[]).expect("own base");
        assert_eq!(
            edited.display_name.as_deref(),
            Some("new"),
            "the edit landed"
        );
        assert_eq!(
            edited.nests[0].roles,
            vec![NestRole::Social, NestRole::Other("Relay".into())]
        );
        assert!(
            matches!(&edited.load_hint, Some(AccountLoadHint::Unknown(v)) if *v == window),
            "{:?}",
            edited.load_hint
        );
        assert_eq!(edited.inbox_mode, InboxMode::Other("Moderated".into()));
    }

    #[test]
    fn publish_recovery_head_publishes_nothing_over_a_refused_base() {
        // The background mirror has no human watching it, so a refusal must
        // surface as its own arm and leave the stored profile alone.
        let owner = ActorKeypair::generate();
        let stranger = ActorKeypair::generate();
        let foreign = build_profile(&stranger, &anchored_profile(stranger.actor_id()))
            .expect("the stranger's own publish");
        let nest = MirrorNest::serving(foreign);

        let err = block_on(publish_recovery_head(
            &nest,
            &owner,
            &[],
            ChainHead::new([0x3E; 32], 4),
            None,
        ))
        .expect_err("a foreign base is refused");
        assert!(
            matches!(err, RecoveryHeadError::Refused(BaseRefusal::NotOwn { .. })),
            "got {err:?}"
        );
        assert!(nest.published().is_empty(), "nothing may be published");
    }

    /// "`new` succeeds `old`", signed as the ceremony signs it. The recovery
    /// key is a throwaway: the successor's own admission reads `new_sig` alone.
    fn succession(old: ActorId, new: &ActorKeypair) -> SignedIdentitySuccession {
        let recovery = fauna_core::recovery::RecoveryKey::generate();
        fauna_core::recovery::IdentitySuccession {
            old_actor_id: old,
            new_actor_id: new.actor_id(),
            recovery_pubkey: recovery.public(),
            seq: 2,
            created_at: Timestamp(0),
        }
        .sign(&recovery, new.signing_key(), None)
        .expect("sign the succession")
    }

    #[test]
    fn a_linkless_successor_republishes_on_its_own_new_sig() {
        // The device's registry never held the predecessor's row (or the user
        // removed the retired account), so `predecessors` is empty. The landed
        // statement carries the successor's OWN signature naming the base's
        // signer, which no nest can forge — that admits the base.
        let predecessor = ActorKeypair::generate();
        let successor = ActorKeypair::generate();
        let inherited = build_profile(&predecessor, &anchored_profile(predecessor.actor_id()))
            .expect("the predecessor's own publish");
        let nest = MirrorNest::serving(inherited)
            .with_statements(&[succession(predecessor.actor_id(), &successor)]);

        block_on(publish_recovery_head(
            &nest,
            &successor,
            &[],
            ChainHead::new([0x3E; 32], 4),
            None,
        ))
        .expect("the statement's new_sig admits the inherited base");

        assert_eq!(
            nest.lookups(),
            vec![predecessor.actor_id().0.to_vec()],
            "the lookup asks about the identity the refused base names"
        );
        let published = nest.published();
        assert_eq!(published.len(), 1);
        let (profile, origin) = decode_profile(&published[0]).expect("decode published");
        assert_eq!(origin, AuthoringOrigin::Direct);
        assert_eq!(profile.actor_id, successor.actor_id());
        assert_eq!(
            profile.display_name.as_deref(),
            Some("Ada"),
            "the user's own fields travel with the account"
        );
        assert_eq!(profile.recovery_head, Some(ChainHead::new([0x3E; 32], 4)));
        assert!(
            profile.nests.is_empty(),
            "the predecessor's home is still another identity's word"
        );
    }

    #[test]
    fn a_multi_hop_chain_proves_every_ancestor_nearest_first() {
        // A→B→C where B never re-published: C's row is still A's bytes. The
        // lookup from A answers both links; C's own signature pins B, and B's
        // pins A.
        let (a, b, c) = (
            ActorKeypair::generate(),
            ActorKeypair::generate(),
            ActorKeypair::generate(),
        );
        let chain = [succession(a.actor_id(), &b), succession(b.actor_id(), &c)];
        assert_eq!(
            proven_predecessors(&c.actor_id(), &a.actor_id(), &chain),
            vec![b.actor_id(), a.actor_id()]
        );
        // A base B signed is proven by the tail alone.
        assert_eq!(
            proven_predecessors(&c.actor_id(), &b.actor_id(), &chain[1..]),
            vec![b.actor_id()]
        );
        // A truncated reply stops short of the signer, so it proves nothing.
        assert!(proven_predecessors(&c.actor_id(), &a.actor_id(), &chain[..1]).is_empty());
    }

    #[test]
    fn a_broken_or_forged_chain_proves_nothing() {
        let (a, b, c) = (
            ActorKeypair::generate(),
            ActorKeypair::generate(),
            ActorKeypair::generate(),
        );
        // Reordered: the first link does not start at the base.
        let reordered = [succession(b.actor_id(), &c), succession(a.actor_id(), &b)];
        assert!(proven_predecessors(&c.actor_id(), &a.actor_id(), &reordered).is_empty());
        // A gap: A→B then X→C.
        let gap = [
            succession(a.actor_id(), &b),
            succession(ActorKeypair::generate().actor_id(), &c),
        ];
        assert!(proven_predecessors(&c.actor_id(), &a.actor_id(), &gap).is_empty());
        // The nest's forgery: a link NAMING the signer as successor, whose
        // `new_sig` some other key made.
        let mut forged = succession(a.actor_id(), &b);
        forged.statement.new_actor_id = c.actor_id();
        assert!(proven_predecessors(&c.actor_id(), &a.actor_id(), &[forged]).is_empty());
    }

    #[test]
    fn learning_the_link_asks_nothing_over_a_placeable_base() {
        // Zero cost for everyone else: an own base, a recorded predecessor's
        // base and a never-published profile all answer empty with no lookup.
        let predecessor = ActorKeypair::generate();
        let successor = ActorKeypair::generate();
        let own = build_profile(&successor, &anchored_profile(successor.actor_id())).expect("own");
        let inherited = build_profile(&predecessor, &anchored_profile(predecessor.actor_id()))
            .expect("inherited");

        let nest = MirrorNest::serving(own);
        assert!(
            block_on(learn_inherited_predecessors(&nest, &successor, &[]))
                .expect("reads")
                .is_empty()
        );
        assert!(nest.lookups().is_empty());

        let nest = MirrorNest::serving(inherited.clone());
        assert!(
            block_on(learn_inherited_predecessors(
                &nest,
                &successor,
                &[predecessor.actor_id()]
            ))
            .expect("reads")
            .is_empty()
        );
        assert!(nest.lookups().is_empty());

        let nest = MirrorNest::refusing(fauna_protocol::RpcError::CODE_PROFILE_NOT_FOUND);
        assert!(
            block_on(learn_inherited_predecessors(&nest, &successor, &[]))
                .expect("not-found is not an error")
                .is_empty()
        );

        // And the case it exists for.
        let nest = MirrorNest::serving(inherited)
            .with_statements(&[succession(predecessor.actor_id(), &successor)]);
        assert_eq!(
            block_on(learn_inherited_predecessors(&nest, &successor, &[])).expect("reads"),
            vec![predecessor.actor_id()]
        );
    }

    #[test]
    fn the_edit_base_read_proves_the_link_it_hands_back() {
        // The edit form's base load: the bytes it re-signs, and the link that
        // admits them, from one read, so a save cannot overtake a background
        // sign-in hop. The proof it returns is what the writer then accepts.
        let predecessor = ActorKeypair::generate();
        let successor = ActorKeypair::generate();
        let inherited = build_profile(&predecessor, &anchored_profile(predecessor.actor_id()))
            .expect("the predecessor's own publish");
        let nest = MirrorNest::serving(inherited.clone())
            .with_statements(&[succession(predecessor.actor_id(), &successor)]);

        let base = block_on(fetch_own_profile_base(&nest, &successor, &[])).expect("reads");
        assert_eq!(base.body.as_deref(), Some(inherited.as_slice()));
        assert_eq!(base.proven, vec![predecessor.actor_id()]);
        let edited = edit(&successor, &inherited, &base.proven)
            .expect("the proven link admits the inherited base");
        assert_eq!(edited.actor_id, successor.actor_id());

        // A never-published profile is a first publish, not an error.
        let nest = MirrorNest::refusing(fauna_protocol::RpcError::CODE_PROFILE_NOT_FOUND);
        assert_eq!(
            block_on(fetch_own_profile_base(&nest, &successor, &[])).expect("not-found is Ok"),
            OwnProfileBase::default()
        );
    }

    #[test]
    fn a_statement_the_signer_did_not_sign_admits_nothing() {
        // The hostile nest's move: serve a stranger's profile as the row, and a
        // GENUINE statement in which some other key claims to succeed that
        // stranger. Every signature verifies; none of them is the signer's.
        let owner = ActorKeypair::generate();
        let stranger = ActorKeypair::generate();
        let accomplice = ActorKeypair::generate();
        let foreign = build_profile(&stranger, &anchored_profile(stranger.actor_id()))
            .expect("the stranger's own publish");
        let nest = MirrorNest::serving(foreign)
            .with_statements(&[succession(stranger.actor_id(), &accomplice)]);

        let err = block_on(publish_recovery_head(
            &nest,
            &owner,
            &[],
            ChainHead::new([0x3E; 32], 4),
            None,
        ))
        .expect_err("a chain that never reaches the signer proves nothing");
        assert!(
            matches!(err, RecoveryHeadError::Refused(BaseRefusal::NotOwn { .. })),
            "got {err:?}"
        );
        assert!(nest.published().is_empty(), "nothing may be published");
    }

    #[test]
    fn the_recovery_head_mirror_works_before_any_profile_is_published() {
        // The kit ceremony can run before the user ever edits a profile; that
        // must produce a minimal profile carrying the binding, not an error.
        let keypair = ActorKeypair::generate();

        let body = build_profile_with_recovery_head(
            &keypair,
            None,
            &[],
            ChainHead::new([0x11; 32], 1),
            None,
        )
        .expect("mirror");

        let (profile, _) = decode_profile(&body).expect("decode");
        assert_eq!(profile.recovery_head, Some(ChainHead::new([0x11; 32], 1)));
        assert_eq!(profile.display_name, None);
        assert_eq!(profile.actor_id, keypair.actor_id());
    }

    #[test]
    fn the_edit_form_never_clears_a_registered_recovery_head() {
        // The read-modify-write contract the field-add relied on: a later
        // profile edit preserves the binding it knows nothing about.
        let keypair = ActorKeypair::generate();
        let with_key = build_profile_with_recovery_head(
            &keypair,
            None,
            &[],
            ChainHead::new([0x5C; 32], 2),
            None,
        )
        .expect("mirror");

        let edited = build_edited_profile(
            &keypair,
            Some(&with_key),
            &[],
            Some("new name".into()),
            None,
            vec![],
        )
        .expect("edit");

        let (profile, _) = decode_profile(&edited).expect("decode");
        assert_eq!(
            profile.recovery_head,
            Some(ChainHead::new([0x5C; 32], 2)),
            "an ordinary profile edit must not retire the user's recovery binding"
        );
        assert_eq!(profile.display_name.as_deref(), Some("new name"));
    }

    // ── publish_recovery_head: the read-modify-write transport hop ────────

    /// [`MirrorNest`]'s transport error: `Some` = a server rejection carrying
    /// its wire code, `None` = a transport fault that never reached nest.
    /// Shaped exactly like the real transports' errors so the not-found arm is
    /// exercised through the same `RpcErrorClass` seam production reads.
    #[derive(Debug, Clone)]
    struct MirrorErr(Option<fauna_protocol::RpcError>);

    impl core::fmt::Display for MirrorErr {
        fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
            match &self.0 {
                Some(e) => write!(f, "rejected: {}", e.code),
                None => write!(f, "transport fault"),
            }
        }
    }

    impl fauna_protocol::RpcErrorClass for MirrorErr {
        fn is_rejection(&self) -> bool {
            self.0.is_some()
        }
        fn as_rpc_error(&self) -> Option<&fauna_protocol::RpcError> {
            self.0.as_ref()
        }
    }

    /// A profile-plane fake: `fauna.profile.get` answers with a stored body or
    /// a configured failure, and `fauna.profile.set` records what was
    /// published so the assertions can decode it.
    struct MirrorNest {
        get: Result<Vec<u8>, MirrorErr>,
        published: std::sync::Mutex<Vec<Vec<u8>>>,
        /// What `fauna.recovery.succession.lookup` answers, whatever actor is
        /// asked about — a hostile nest serves what it likes.
        statements: Vec<Vec<u8>>,
        /// Every actor id a lookup asked about, in order.
        lookups: std::sync::Mutex<Vec<Vec<u8>>>,
        /// What `fauna.recovery.registration.chain` answers, whatever actor is
        /// asked about.
        chain: Vec<Vec<u8>>,
        /// How many chain reads arrived.
        chain_reads: std::sync::atomic::AtomicUsize,
    }

    impl MirrorNest {
        fn serving(body: Vec<u8>) -> Self {
            Self {
                get: Ok(body),
                published: std::sync::Mutex::new(vec![]),
                statements: vec![],
                lookups: std::sync::Mutex::new(vec![]),
                chain: vec![],
                chain_reads: Default::default(),
            }
        }
        fn with_chain(
            mut self,
            chain: &[fauna_core::recovery::SignedRecoveryKeyRegistration],
        ) -> Self {
            self.chain = chain
                .iter()
                .map(|r| fauna_core::encoding::canonical_encode(r).expect("encode registration"))
                .collect();
            self
        }
        fn chain_reads(&self) -> usize {
            self.chain_reads.load(std::sync::atomic::Ordering::SeqCst)
        }
        fn with_statements(mut self, statements: &[SignedIdentitySuccession]) -> Self {
            self.statements = statements
                .iter()
                .map(|s| fauna_core::encoding::canonical_encode(s).expect("encode statement"))
                .collect();
            self
        }
        fn lookups(&self) -> Vec<Vec<u8>> {
            self.lookups.lock().unwrap().clone()
        }
        fn refusing(code: &str) -> Self {
            Self {
                get: Err(MirrorErr(Some(fauna_protocol::RpcError::new(
                    code,
                    "error.test",
                )))),
                published: std::sync::Mutex::new(vec![]),
                statements: vec![],
                lookups: std::sync::Mutex::new(vec![]),
                chain: vec![],
                chain_reads: Default::default(),
            }
        }
        fn unreachable() -> Self {
            Self {
                get: Err(MirrorErr(None)),
                published: std::sync::Mutex::new(vec![]),
                statements: vec![],
                lookups: std::sync::Mutex::new(vec![]),
                chain: vec![],
                chain_reads: Default::default(),
            }
        }
        fn published(&self) -> Vec<Vec<u8>> {
            self.published.lock().unwrap().clone()
        }
    }

    impl RpcRequester for &MirrorNest {
        type Error = MirrorErr;

        async fn request<Req, Reply>(
            &self,
            kind: &'static str,
            payload: Req,
        ) -> Result<Reply, Self::Error>
        where
            Req: serde::Serialize,
            Reply: serde::de::DeserializeOwned,
        {
            let bytes = fauna_protocol::encode_canonical(&payload).expect("encode request");
            let reply = match kind {
                "fauna.profile.get" => {
                    let body = self.get.clone()?;
                    fauna_protocol::encode_canonical(&fauna_protocol::profile::ProfileGetReply {
                        body: ByteBuf::from(body),
                        extra: Default::default(),
                    })
                }
                "fauna.profile.set" => {
                    let req: ProfileSetRequest =
                        fauna_protocol::decode_strict(&bytes).expect("set request decodes");
                    self.published.lock().unwrap().push(req.body.into_vec());
                    fauna_protocol::encode_canonical(&fauna_protocol::profile::ProfileSetReply {
                        extra: Default::default(),
                    })
                }
                "fauna.recovery.succession.lookup" => {
                    let req: fauna_protocol::recovery::SuccessionLookupRequest =
                        fauna_protocol::decode_strict(&bytes).expect("lookup request decodes");
                    self.lookups.lock().unwrap().push(req.actor_id.into_vec());
                    fauna_protocol::encode_canonical(
                        &fauna_protocol::recovery::SuccessionLookupReply {
                            statements: self
                                .statements
                                .iter()
                                .map(|b| ByteBuf::from(b.clone()))
                                .collect(),
                            extra: Default::default(),
                        },
                    )
                }
                "fauna.recovery.registration.chain" => {
                    self.chain_reads
                        .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                    fauna_protocol::encode_canonical(
                        &fauna_protocol::recovery::RegistrationChainReply {
                            registrations: self
                                .chain
                                .iter()
                                .map(|b| ByteBuf::from(b.clone()))
                                .collect(),
                            extra: Default::default(),
                        },
                    )
                }
                other => panic!("MirrorNest: unhandled kind {other}"),
            }
            .expect("encode reply");
            Ok(fauna_protocol::decode_strict(&reply).expect("decode reply"))
        }
    }

    #[test]
    fn publish_recovery_head_reads_modifies_and_republishes() {
        let keypair = ActorKeypair::generate();
        let base = build_profile(&keypair, &base_profile(&keypair)).expect("base");
        let nest = MirrorNest::serving(base);

        block_on(publish_recovery_head(
            &nest,
            &keypair,
            &[],
            ChainHead::new([0x3E; 32], 4),
            None,
        ))
        .expect("mirror publishes");

        let published = nest.published();
        assert_eq!(published.len(), 1, "exactly one profile write per ceremony");
        let (profile, _) = decode_profile(&published[0]).expect("decode published");
        assert_eq!(profile.recovery_head, Some(ChainHead::new([0x3E; 32], 4)));
        // The whole point of the read-modify-write: mirroring a binding is not
        // a licence to blank what the user authored.
        assert_eq!(profile.display_name.as_deref(), Some("old"));
        assert_eq!(profile.inbox_mode, InboxMode::ContactsOnly);
        assert_eq!(profile.admin_nests.len(), 1);
    }

    #[test]
    fn publish_recovery_head_mints_a_profile_when_none_was_ever_published() {
        // The onboarding kit screen precedes every profile edit, so the very
        // first mirror routinely finds no base document. That is the ordinary
        // path, not a failure.
        let keypair = ActorKeypair::generate();
        let nest = MirrorNest::refusing(fauna_protocol::RpcError::CODE_PROFILE_NOT_FOUND);

        block_on(publish_recovery_head(
            &nest,
            &keypair,
            &[],
            ChainHead::new([0x0B; 32], 1),
            None,
        ))
        .expect("a never-published profile is not an error");

        let published = nest.published();
        assert_eq!(published.len(), 1);
        let (profile, _) = decode_profile(&published[0]).expect("decode published");
        assert_eq!(profile.recovery_head, Some(ChainHead::new([0x0B; 32], 1)));
        assert_eq!(profile.actor_id, keypair.actor_id());
    }

    #[test]
    fn the_mirror_fills_an_absent_home_nests_entry() {
        // The producer half of the harvest's domain arm: a registered-kit
        // owner's published profile must carry its home `nests` entry, or a
        // peer's chain that advances past the harvested head degrades to the
        // bare add for want of a dial domain (`identity-succession.md` § the
        // peer-profile harvest, the residual paragraph's declared follow-on).
        let keypair = ActorKeypair::generate();
        // Base WITHOUT a nests entry (base_profile's default), plus the
        // never-published mint — both must gain the home entry.
        let base = build_profile(&keypair, &base_profile(&keypair)).expect("base");
        for (label, nest) in [
            ("over a stored profile", MirrorNest::serving(base)),
            (
                "over a never-published profile",
                MirrorNest::refusing(fauna_protocol::RpcError::CODE_PROFILE_NOT_FOUND),
            ),
        ] {
            block_on(publish_recovery_head(
                &nest,
                &keypair,
                &[],
                ChainHead::new([0x3E; 32], 4),
                Some(HomeNest {
                    url: "https://nest.example:8443".into(),
                    nest_id: Some([0xA1; 32]),
                }),
            ))
            .expect(label);
            let published = nest.published();
            let (profile, _) = decode_profile(&published[0]).expect("decode");
            assert_eq!(profile.nests.len(), 1, "{label}: the home entry is set");
            assert_eq!(profile.nests[0].url, "https://nest.example:8443");
            assert_eq!(
                profile.nests[0].nest_id,
                vec![0xA1; 32],
                "{label}: the TOFU-pinned nest identity rides along"
            );
            assert_eq!(
                profile.recovery_head,
                Some(ChainHead::new([0x3E; 32], 4)),
                "{label}: the head mirror is unchanged by the home fill"
            );
        }
    }

    #[test]
    fn the_mirror_never_displaces_an_authored_nests_list() {
        // Fill-if-absent only: this hop is a background mirror, not an editor.
        // A `nests` list the user (or a future editor surface) authored is
        // preserved verbatim, whatever home the caller resolves.
        let keypair = ActorKeypair::generate();
        let mut authored = base_profile(&keypair);
        authored.nests = vec![fauna_core::data::NestEntry {
            nest_id: vec![9u8; 32],
            url: "https://authored.example".into(),
            roles: vec![],
        }];
        let base = build_profile(&keypair, &authored).expect("base");
        let nest = MirrorNest::serving(base);

        block_on(publish_recovery_head(
            &nest,
            &keypair,
            &[],
            ChainHead::new([0x3E; 32], 4),
            Some(HomeNest {
                url: "https://other.example".into(),
                nest_id: None,
            }),
        ))
        .expect("mirror publishes");

        let published = nest.published();
        let (profile, _) = decode_profile(&published[0]).expect("decode");
        assert_eq!(profile.nests.len(), 1);
        assert_eq!(
            profile.nests[0].url, "https://authored.example",
            "an authored entry is never displaced by the mirror's home"
        );
        assert_eq!(profile.nests[0].nest_id, vec![9u8; 32]);
    }

    #[test]
    fn publish_recovery_head_aborts_rather_than_overwrite_an_unreadable_profile() {
        // The mutation this test kills: widening the not-found arm to "any get
        // failure mints a minimal profile" would answer a transient
        // disconnect by REPLACING the user's display name, bio and links with
        // an empty document — silent, signed, and irreversible from the app.
        let keypair = ActorKeypair::generate();

        for (label, nest) in [
            ("a transport fault", MirrorNest::unreachable()),
            (
                "an unrelated refusal",
                MirrorNest::refusing("fauna.auth.permission_denied"),
            ),
        ] {
            let err = block_on(publish_recovery_head(
                &nest,
                &keypair,
                &[],
                ChainHead::new([0x77; 32], 2),
                None,
            ))
            .expect_err(label);
            assert!(
                matches!(err, RecoveryHeadError::Fetch(_)),
                "{label} must surface as Fetch, got {err:?}"
            );
            assert!(
                nest.published().is_empty(),
                "{label}: nothing may be published when the base could not be read"
            );
        }
    }

    // ── restore_delegated_anchors — the owner's producer after an external edit ──

    /// A genuine first registration for `owner`: signed by the identity seed and
    /// by the RecoveryKey it registers, exactly what the nest stores.
    fn registered(
        owner: &ActorKeypair,
    ) -> (
        fauna_core::recovery::SignedRecoveryKeyRegistration,
        ChainHead,
    ) {
        let root = fauna_core::recovery::RecoveryKey::generate();
        let reg = fauna_core::recovery::RecoveryKeyRegistration {
            actor_id: owner.actor_id(),
            recovery_pubkey: root.public(),
            seq: 1,
            created_at: Timestamp(0),
        }
        .sign(owner.signing_key(), &root, None)
        .expect("sign the registration");
        (reg, ChainHead::new(root.public(), 1))
    }

    fn home() -> Option<HomeNest> {
        Some(HomeNest {
            url: "https://home.example".into(),
            nest_id: Some([0x5A; 32]),
        })
    }

    #[test]
    fn a_delegated_edit_regains_its_anchors_at_the_owners_next_sign_in() {
        // The walk the producer exists for: an external-app edit leaves a
        // delegated profile at rest, a peer's harvest seeds nothing from it,
        // the owner's next sign-in re-publishes from its own verified chain,
        // and the harvest then seeds the owner's REAL head and home — not the
        // ones the delegated edit carried.
        let owner = ActorKeypair::generate();
        let (reg, head) = registered(&owner);
        let delegated = delegated_wire(&owner, &anchored_profile(owner.actor_id()));
        let peer = fauna_core::data::PeerAnchors::default();

        let mut before = peer.clone();
        assert!(
            matches!(
                before.seed_from_peer_profile_bytes(&owner.actor_id(), &delegated),
                Err(fauna_core::data::PeerAnchorRefusal::Delegated)
            ),
            "the door stays shut on the delegated profile itself"
        );

        let nest = MirrorNest::serving(delegated.clone()).with_chain(&[reg]);
        let outcome = block_on(restore_delegated_anchors(
            &nest,
            &owner,
            &[],
            Some(&delegated),
            home(),
        ))
        .expect("restores");
        assert_eq!(outcome, AnchorRestore::Published);

        let published = nest.published();
        assert_eq!(published.len(), 1, "one write per sign-in");
        let (profile, origin) = decode_profile(&published[0]).expect("decodes");
        assert_eq!(origin, AuthoringOrigin::Direct);
        assert_eq!(profile.recovery_head, Some(head));
        assert_eq!(profile.nests.len(), 1);
        assert_eq!(profile.nests[0].url, "https://home.example");
        assert_eq!(profile.nests[0].nest_id, vec![0x5A; 32]);
        // The external app's display edits survive the re-publish.
        assert_eq!(profile.display_name.as_deref(), Some("Ada"));
        assert_eq!(profile.inbox_mode, InboxMode::ContactsOnly);

        let mut after = peer;
        let seeded = after
            .seed_from_peer_profile_bytes(&owner.actor_id(), &published[0])
            .expect("the re-published profile is admitted");
        assert!(seeded.seeded_head && seeded.seeded_domain);
        assert_eq!(after.known_chain_head(&owner.actor_id()), Some(head));
        assert_eq!(
            after.known_anchor_domain(&owner.actor_id()).as_deref(),
            Some("home.example")
        );
    }

    #[test]
    fn a_direct_profile_asks_the_nest_nothing() {
        // The ordinary sign-in, and the pass after a restore: nothing to do,
        // and no round-trip spent finding that out — which is also what keeps
        // an app that re-writes after every change from inducing a loop.
        let owner = ActorKeypair::generate();
        let (reg, _) = registered(&owner);
        let own = build_profile(&owner, &base_profile(&owner)).expect("own publish");
        for base in [Some(own.as_slice()), None] {
            let nest = MirrorNest::serving(own.clone()).with_chain(std::slice::from_ref(&reg));
            let outcome = block_on(restore_delegated_anchors(&nest, &owner, &[], base, home()))
                .expect("no-op");
            assert_eq!(outcome, AnchorRestore::NotDelegated);
            assert_eq!(nest.chain_reads(), 0);
            assert!(nest.published().is_empty());
        }
    }

    #[test]
    fn another_identitys_delegated_profile_is_not_ours_to_restore() {
        let owner = ActorKeypair::generate();
        let stranger = ActorKeypair::generate();
        let (reg, _) = registered(&owner);
        let foreign = delegated_wire(&stranger, &anchored_profile(stranger.actor_id()));
        let nest = MirrorNest::serving(foreign.clone()).with_chain(&[reg]);
        let outcome = block_on(restore_delegated_anchors(
            &nest,
            &owner,
            &[],
            Some(&foreign),
            home(),
        ))
        .expect("no-op");
        assert_eq!(outcome, AnchorRestore::NotDelegated);
        assert!(nest.published().is_empty());
    }

    #[test]
    fn a_user_with_no_registered_kit_publishes_nothing() {
        let owner = ActorKeypair::generate();
        let delegated = delegated_wire(&owner, &anchored_profile(owner.actor_id()));
        let nest = MirrorNest::serving(delegated.clone());
        let outcome = block_on(restore_delegated_anchors(
            &nest,
            &owner,
            &[],
            Some(&delegated),
            home(),
        ))
        .expect("no chain is not an error");
        assert_eq!(outcome, AnchorRestore::NoChain);
        assert_eq!(nest.chain_reads(), 1);
        assert!(nest.published().is_empty());
    }

    #[test]
    fn a_chain_the_device_cannot_verify_is_never_published() {
        // The nest's word is not the head: a chain that names another identity,
        // or whose links do not verify, publishes nothing.
        let owner = ActorKeypair::generate();
        let stranger = ActorKeypair::generate();
        let (strangers_reg, _) = registered(&stranger);
        let (mut forged, _) = registered(&owner);
        forged.seed_sig[0] ^= 0xFF;
        let delegated = delegated_wire(&owner, &anchored_profile(owner.actor_id()));
        for (label, chain) in [
            ("another identity's chain", strangers_reg),
            ("a forged link", forged),
        ] {
            let nest = MirrorNest::serving(delegated.clone()).with_chain(&[chain]);
            let outcome = block_on(restore_delegated_anchors(
                &nest,
                &owner,
                &[],
                Some(&delegated),
                home(),
            ))
            .expect("refusal is an outcome, not an error");
            assert_eq!(outcome, AnchorRestore::Unverified, "{label}");
            assert!(nest.published().is_empty(), "{label}");
        }
    }
}
