//! The platform-neutral archive model — `docs/goal/behavior/archive-import.md`
//! § The archive model. Vocabulary follows ActivityStreams (actor, post,
//! comment, reaction, media, album, event, group, friendship, thread,
//! message, profile). Nothing platform-specific leaks past a parser: the
//! only platform-shaped value here is [`Platform`] itself.
//!
//! Every type serializes as canonical IPLD dag-cbor (a consumer that stores
//! the model the way Fauna does encodes it with `serde_ipld_dagcbor`), so:
//! no floats (coordinates are decimal strings), string map keys only, and
//! every field name is part of the at-rest contract from the first commit —
//! evolve additively (`#[serde(default)]` on new fields), never rename.
//!
//! No `fauna-*` type appears here: [`Timestamp`] and the platform tokens are
//! this crate's own, so the model stands on its own for any consumer.

use std::collections::BTreeSet;

use serde::{Deserialize, Deserializer, Serialize};

/// Microseconds since the Unix epoch, UTC — every instant in the model.
///
/// A plain newtype over `u64`, serialized as the bare integer, so it is
/// byte-identical on the wire to Fauna's own `Timestamp` (which is the same
/// shape) and trivially convertible for any consumer's instant type. `0`
/// never means a real moment; the model uses `Option<Timestamp>` where an
/// export may carry no date.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct Timestamp(pub u64);

/// Bumped when a parser's output for the same archive would change
/// (recorded in the archive folder's marker so a later parser can re-mine).
pub const PARSER_VERSION: u32 = 1;

/// The Facebook platform token — [`Platform::Facebook`]'s serde form and
/// wire spelling.
pub const FACEBOOK_TOKEN: &str = "facebook";

/// The Instagram platform token — [`Platform::Instagram`]'s serde form and
/// wire spelling.
pub const INSTAGRAM_TOKEN: &str = "instagram";

/// The source platform — an additive enum. The serde form is the
/// lowercase token ([`FACEBOOK_TOKEN`], [`INSTAGRAM_TOKEN`]); Fauna's post
/// `source` vocabulary carries the same spellings, pinned equal by a test in
/// its import machine, so a badge and a feed filter read one token.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Platform {
    Facebook,
    Instagram,
    /// A platform a newer parser added that this build cannot name — the
    /// carrying open arm (`transport.md` § Schema and forward-compat
    /// discipline → *Rule 3 in full*). It sits inside every [`ExternalId`] a
    /// stored import state keeps for dedup, so it holds the exact token read
    /// and re-emits it; dedup compares it like any other value. No parser of
    /// this build produces one, and nothing is authored under it.
    #[serde(untagged)]
    Other(String),
}

impl Platform {
    /// Every platform, in declaration order — what a consumer iterates to
    /// mirror the set (a vocabulary pin, a picker).
    pub const ALL: [Platform; 2] = [Platform::Facebook, Platform::Instagram];

    /// The token: one lowercase word, the serde form of the variant (a
    /// carried [`Platform::Other`] gives back the token it was read as).
    pub fn token(&self) -> &str {
        match self {
            Platform::Facebook => FACEBOOK_TOKEN,
            Platform::Instagram => INSTAGRAM_TOKEN,
            Platform::Other(token) => token,
        }
    }

    /// The canonical display label — what a badge shows. A platform this
    /// build cannot name shows its raw token.
    pub fn label(&self) -> &str {
        match self {
            Platform::Facebook => "Facebook",
            Platform::Instagram => "Instagram",
            Platform::Other(token) => token,
        }
    }

    /// Whether this build names the platform (not a carried
    /// [`Platform::Other`]).
    pub fn is_known(&self) -> bool {
        !matches!(self, Platform::Other(_))
    }

    /// Case-insensitive, whitespace-tolerant token parse; `None` for anything
    /// outside the set.
    pub fn parse_token(token: &str) -> Option<Platform> {
        match token.trim().to_ascii_lowercase().as_str() {
            FACEBOOK_TOKEN => Some(Platform::Facebook),
            INSTAGRAM_TOKEN => Some(Platform::Instagram),
            _ => None,
        }
    }
}

/// The entity kind an [`ExternalId`] names — the ID namespace.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EntityKind {
    Profile,
    Post,
    Album,
    Comment,
    Reaction,
    Media,
    Event,
    Group,
    Friendship,
    Thread,
    Message,
    /// A kind a newer parser added that this build cannot name — carried for
    /// the same reason as [`Platform::Other`] (it sits inside every stored
    /// [`ExternalId`]). It belongs to no category this build authors.
    #[serde(untagged)]
    Other(String),
}

impl EntityKind {
    /// The snake_case token — the value hashed into a derived ID (a carried
    /// [`EntityKind::Other`] gives back the token it was read as).
    pub fn token(&self) -> &str {
        match self {
            EntityKind::Profile => "profile",
            EntityKind::Post => "post",
            EntityKind::Album => "album",
            EntityKind::Comment => "comment",
            EntityKind::Reaction => "reaction",
            EntityKind::Media => "media",
            EntityKind::Event => "event",
            EntityKind::Group => "group",
            EntityKind::Friendship => "friendship",
            EntityKind::Thread => "thread",
            EntityKind::Message => "message",
            EntityKind::Other(token) => token,
        }
    }

    /// Whether this build names the kind (not a carried
    /// [`EntityKind::Other`]).
    pub fn is_known(&self) -> bool {
        !matches!(self, EntityKind::Other(_))
    }
}

/// A category a parser can count and stream — the wizard's Scope rows and
/// the `model/<category>.cbor` files map one-to-one onto these.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Category {
    Posts,
    Albums,
    Comments,
    Reactions,
    Events,
    Groups,
    Friends,
    Threads,
    Messages,
    Profile,
    /// A category a newer parser added that this build cannot name — the
    /// carrying open arm (`transport.md` § *Rule 3 in full*). It appears in a
    /// stored import's scope, phase and skip list; a state holding one stays
    /// readable for dedup and is not resumable by this build. It is never in
    /// [`Category::ALL`], counts nothing and is offered by no Scope row.
    /// Declared last, so the derived order (the run's order) puts it after
    /// every known category.
    #[serde(untagged)]
    Other(String),
}

impl Category {
    /// Every category, in the order the wizard lists them and the import
    /// runs them (posts before events before threads).
    pub const ALL: [Category; 10] = [
        Category::Posts,
        Category::Albums,
        Category::Comments,
        Category::Reactions,
        Category::Events,
        Category::Groups,
        Category::Friends,
        Category::Threads,
        Category::Messages,
        Category::Profile,
    ];

    /// The snake_case token (also the `model/<token>.cbor` file stem); a
    /// carried [`Category::Other`] gives back the token it was read as.
    pub fn token(&self) -> &str {
        match self {
            Category::Posts => "posts",
            Category::Albums => "albums",
            Category::Comments => "comments",
            Category::Reactions => "reactions",
            Category::Events => "events",
            Category::Groups => "groups",
            Category::Friends => "friends",
            Category::Threads => "threads",
            Category::Messages => "messages",
            Category::Profile => "profile",
            Category::Other(token) => token,
        }
    }

    /// Whether this build names the category (not a carried
    /// [`Category::Other`]).
    pub fn is_known(&self) -> bool {
        !matches!(self, Category::Other(_))
    }
}

/// Per-category record totals — an explicit struct rather than a map so the
/// CBOR shape is fixed and every key is a plain string.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CategoryCounts {
    pub posts: u64,
    pub albums: u64,
    pub comments: u64,
    pub reactions: u64,
    pub events: u64,
    pub groups: u64,
    pub friends: u64,
    pub threads: u64,
    pub messages: u64,
    pub profile: u64,
}

impl CategoryCounts {
    /// The count for `category`; a category this build cannot name counts
    /// nothing.
    pub fn get(&self, category: &Category) -> u64 {
        match category {
            Category::Posts => self.posts,
            Category::Albums => self.albums,
            Category::Comments => self.comments,
            Category::Reactions => self.reactions,
            Category::Events => self.events,
            Category::Groups => self.groups,
            Category::Friends => self.friends,
            Category::Threads => self.threads,
            Category::Messages => self.messages,
            Category::Profile => self.profile,
            Category::Other(_) => 0,
        }
    }

    /// Sets the count for `category`; a category this build cannot name has
    /// no slot, so the value is dropped.
    pub fn set(&mut self, category: &Category, value: u64) {
        match category {
            Category::Posts => self.posts = value,
            Category::Albums => self.albums = value,
            Category::Comments => self.comments = value,
            Category::Reactions => self.reactions = value,
            Category::Events => self.events = value,
            Category::Groups => self.groups = value,
            Category::Friends => self.friends = value,
            Category::Threads => self.threads = value,
            Category::Messages => self.messages = value,
            Category::Profile => self.profile = value,
            Category::Other(_) => {}
        }
    }
}

/// How many audience-bearing records (posts and albums) the export labels
/// with each audience, and how many it leaves unlabelled — what the Scope
/// step states so a user knows, per archive, how much "at its original
/// audience" can mean (`archive-import.md` § Audience mapping; an export
/// with no per-post `privacy` field imports everything owner-only).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct AudienceCounts {
    pub public: u64,
    pub friends: u64,
    pub custom: u64,
    pub only_me: u64,
    pub unknown: u64,
}

impl AudienceCounts {
    pub fn bump(&mut self, audience: ArchiveAudience) {
        match audience {
            ArchiveAudience::Public => self.public += 1,
            ArchiveAudience::Friends => self.friends += 1,
            ArchiveAudience::Custom => self.custom += 1,
            ArchiveAudience::OnlyMe => self.only_me += 1,
            ArchiveAudience::Unknown => self.unknown += 1,
        }
    }

    /// Records that carry a recorded audience.
    pub fn known(&self) -> u64 {
        self.public + self.friends + self.custom + self.only_me
    }
}

/// The pivot of the whole design: a platform-scoped, kind-scoped identity.
/// `id` is the platform's own identifier or permalink when the export
/// carries one, else `h:<blake3-hex>` (see `ExternalId::derive`).
/// Re-imports, dedup and cross-user matching all key on it.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct ExternalId {
    pub platform: Platform,
    pub kind: EntityKind,
    pub id: String,
}

/// A person as the platform names them. `id` is the platform's user ID when
/// the export has one; `name_key` is the normalized display name — the
/// matching key when IDs are absent (Facebook comment authors are names only).
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct ExternalActorRef {
    pub platform: Platform,
    pub id: Option<String>,
    pub display_name: String,
    pub name_key: String,
}

impl ExternalActorRef {
    /// Builds a ref, normalizing `display_name` (collapsed whitespace) and
    /// deriving `name_key` with [`crate::text::name_key`].
    pub fn new(platform: Platform, id: Option<String>, display_name: &str) -> Self {
        let display_name = crate::text::normalize_text(display_name);
        let name_key = crate::text::name_key(&display_name);
        ExternalActorRef {
            platform,
            id,
            display_name,
            name_key,
        }
    }
}

/// The audience the platform recorded for an item. `Unknown` is the honest
/// default when the export carries no audience; the mapping in the goal doc
/// sends it to the owner-only tier, never to public.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ArchiveAudience {
    Public,
    Friends,
    Custom,
    OnlyMe,
    #[serde(other)]
    Unknown,
}

/// A media file inside the zip. `blake3_hex`/`size` are filled once the
/// member has been read (streaming posts hashes them); `mime` is mapped from
/// the extension.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ArchiveMediaRef {
    pub path: String,
    pub size: Option<u64>,
    pub blake3_hex: Option<String>,
    pub mime: Option<String>,
    pub taken_at: Option<Timestamp>,
    pub caption: Option<String>,
}

/// A place attached to a post or event. Coordinates are decimal strings —
/// canonical DAG-CBOR forbids floats.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ArchivePlace {
    pub name: String,
    pub address: Option<String>,
    pub latitude: Option<String>,
    pub longitude: Option<String>,
    pub url: Option<String>,
}

/// What a comment or reaction points at. Facebook exports name the target
/// only by prose ("… commented on Friend One's post."), so `id` is usually
/// absent and `owner` + `kind_hint` carry what was recoverable; phase two's
/// origin index resolves them.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ArchiveTargetRef {
    pub id: Option<ExternalId>,
    pub url: Option<String>,
    pub owner: Option<ExternalActorRef>,
    pub kind_hint: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ArchiveProfile {
    pub external_id: ExternalId,
    pub actor: ExternalActorRef,
    pub bio: Option<String>,
    pub links: Vec<String>,
    pub picture: Option<ArchiveMediaRef>,
    pub registered_at: Option<Timestamp>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ArchivePost {
    pub external_id: ExternalId,
    pub created_at: Timestamp,
    pub audience: ArchiveAudience,
    pub text: Option<String>,
    pub media: Vec<ArchiveMediaRef>,
    pub links: Vec<String>,
    pub tagged: Vec<ExternalActorRef>,
    /// The album the platform filed the media under, as it labels it.
    pub album_name: Option<String>,
    pub url: Option<String>,
    pub place: Option<ArchivePlace>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ArchiveAlbum {
    pub external_id: ExternalId,
    pub name: String,
    pub description: Option<String>,
    pub created_at: Timestamp,
    pub audience: ArchiveAudience,
    pub media: Vec<ArchiveMediaRef>,
    pub cover: Option<ArchiveMediaRef>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ArchiveComment {
    pub external_id: ExternalId,
    pub created_at: Timestamp,
    pub author: ExternalActorRef,
    pub target: ArchiveTargetRef,
    pub text: Option<String>,
    pub media: Vec<ArchiveMediaRef>,
}

/// The platform's reaction vocabulary, closed with a raw fallback (the same
/// shape as `SourceKind::Other`).
///
/// The decoder is hand-written so that fallback is also the open arm
/// (`transport.md` § Schema and forward-compat discipline → *Rule 3 in
/// full*): a tag a newer parser added — a bare unit tag or the key of a data
/// variant — reads as `Other { raw: <that tag> }` instead of failing the
/// reaction (and with it the whole model file). Model files are written once,
/// from a fresh parse, and never rewritten from a decoded value, so the
/// re-encoding of a routed tag (as `Other`) never reaches a stored file.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ReactionKind {
    Like,
    Love,
    Care,
    Haha,
    Wow,
    Sad,
    Angry,
    Other { raw: String },
}

impl ReactionKind {
    /// Facebook's uppercase tokens (`LIKE`, `LOVE`, `SUPPORT`, `HAHA`, `WOW`,
    /// `SORRY`, `ANGER`); anything else is `Other { raw }` verbatim.
    pub fn from_facebook(raw: &str) -> ReactionKind {
        match raw.trim().to_ascii_uppercase().as_str() {
            "LIKE" => ReactionKind::Like,
            "LOVE" => ReactionKind::Love,
            "SUPPORT" | "CARE" => ReactionKind::Care,
            "HAHA" => ReactionKind::Haha,
            "WOW" => ReactionKind::Wow,
            "SORRY" | "SAD" => ReactionKind::Sad,
            "ANGER" | "ANGRY" => ReactionKind::Angry,
            _ => ReactionKind::Other {
                raw: raw.trim().to_string(),
            },
        }
    }

    /// The language-independent token hashed into a reaction's derived ID:
    /// the serde name for a known kind, the raw platform token lowercased for
    /// `Other`. Never the platform's prose title — that is localized, embeds
    /// the owner's current display name and drifts across export vintages,
    /// so hashing it would re-import every reaction as a duplicate.
    pub fn token(&self) -> String {
        match self {
            ReactionKind::Like => "like".to_string(),
            ReactionKind::Love => "love".to_string(),
            ReactionKind::Care => "care".to_string(),
            ReactionKind::Haha => "haha".to_string(),
            ReactionKind::Wow => "wow".to_string(),
            ReactionKind::Sad => "sad".to_string(),
            ReactionKind::Angry => "angry".to_string(),
            ReactionKind::Other { raw } => raw.to_ascii_lowercase(),
        }
    }

    /// A known kind by its serde tag; `None` for `other` (which carries a
    /// payload) and for every tag this build cannot name.
    fn from_unit_tag(tag: &str) -> Option<ReactionKind> {
        Some(match tag {
            "like" => ReactionKind::Like,
            "love" => ReactionKind::Love,
            "care" => ReactionKind::Care,
            "haha" => ReactionKind::Haha,
            "wow" => ReactionKind::Wow,
            "sad" => ReactionKind::Sad,
            "angry" => ReactionKind::Angry,
            _ => return None,
        })
    }
}

impl<'de> Deserialize<'de> for ReactionKind {
    /// The derive's externally tagged form — a unit tag as a bare string,
    /// `{"other": {"raw": …}}` for the fallback — with every tag this build
    /// cannot name routed into `Other { raw: <tag> }` (the type doc).
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        use ipld_core::ipld::Ipld;
        use serde::de::Error;

        match Ipld::deserialize(deserializer)? {
            Ipld::String(tag) => {
                Ok(ReactionKind::from_unit_tag(&tag).unwrap_or(ReactionKind::Other { raw: tag }))
            }
            Ipld::Map(map) if map.len() == 1 => {
                let (tag, payload) = map.into_iter().next().expect("one entry");
                if tag != "other" {
                    // A newer data variant, or a known unit tag spelled as a
                    // map: the tag is all this build can read of it.
                    return Ok(ReactionKind::from_unit_tag(&tag)
                        .unwrap_or(ReactionKind::Other { raw: tag }));
                }
                match payload {
                    Ipld::Map(fields) => match fields.get("raw") {
                        Some(Ipld::String(raw)) => Ok(ReactionKind::Other { raw: raw.clone() }),
                        _ => Err(D::Error::custom(
                            "reaction kind `other` without a `raw` string",
                        )),
                    },
                    _ => Err(D::Error::custom("reaction kind `other` is not a map")),
                }
            }
            _ => Err(D::Error::custom(
                "a reaction kind is a tag string or a one-entry map",
            )),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ArchiveReaction {
    pub external_id: ExternalId,
    pub created_at: Timestamp,
    pub author: ExternalActorRef,
    pub target: ArchiveTargetRef,
    pub kind: ReactionKind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Rsvp {
    Joined,
    Interested,
    Declined,
    Invited,
    Hosted,
    #[serde(other)]
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ArchiveEvent {
    pub external_id: ExternalId,
    pub title: String,
    pub description: Option<String>,
    pub start: Timestamp,
    pub end: Option<Timestamp>,
    pub place: Option<ArchivePlace>,
    pub rsvp: Rsvp,
    pub attendees: Vec<ExternalActorRef>,
    pub url: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ArchiveGroup {
    pub external_id: ExternalId,
    pub name: String,
    pub joined_at: Option<Timestamp>,
    pub url: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FriendshipKind {
    Friend,
    Follower,
    Following,
    Removed,
    RequestSent,
    RequestReceived,
    #[serde(other)]
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ArchiveFriendship {
    pub actor: ExternalActorRef,
    pub since: Option<Timestamp>,
    pub kind: FriendshipKind,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ArchiveThread {
    pub external_id: ExternalId,
    pub title: Option<String>,
    pub participants: Vec<ExternalActorRef>,
    pub message_count: u64,
    pub first_at: Option<Timestamp>,
    pub last_at: Option<Timestamp>,
    /// The thread's directory inside the export (e.g. `inbox/friendone_abc123`).
    pub path: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MessageReaction {
    pub actor: ExternalActorRef,
    pub emoji: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ArchiveMessage {
    pub external_id: ExternalId,
    pub thread: ExternalId,
    pub sender: ExternalActorRef,
    pub created_at: Timestamp,
    pub text: Option<String>,
    pub media: Vec<ArchiveMediaRef>,
    pub reactions: Vec<MessageReaction>,
}

/// An entity a newer parser added that this build cannot read, held as the
/// undecoded IPLD value so it re-encodes to exactly the bytes it was read
/// from (the payload of [`Entity::Unknown`]; this crate depends on no
/// `fauna-*` crate, so it carries its own twin of `fauna_core`'s
/// `CarriedValue`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct UnknownEntity(pub ipld_core::ipld::Ipld);

/// `Eq` holds: the only value `PartialEq` is not reflexive over is a NaN
/// float, which canonical dag-cbor refuses to decode, so a carried entity
/// never holds one.
impl Eq for UnknownEntity {}

/// One record a category stream yields.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Entity {
    Profile(ArchiveProfile),
    Post(ArchivePost),
    Album(ArchiveAlbum),
    Comment(ArchiveComment),
    Reaction(ArchiveReaction),
    Event(ArchiveEvent),
    Group(ArchiveGroup),
    Friendship(ArchiveFriendship),
    Thread(ArchiveThread),
    Message(ArchiveMessage),
    /// A record a newer parser wrote that this build cannot read — a new
    /// entity kind, or a known one whose payload this build cannot decode —
    /// carried whole (`transport.md` § Schema and forward-compat discipline →
    /// *Rule 3 in full*). A reader counts it as skipped and walks on; it never
    /// fails the model file around it, and nothing is authored from it. No
    /// parser of this build produces one.
    #[serde(untagged)]
    Unknown(UnknownEntity),
}

impl Entity {
    /// The category this record belongs to; `None` for an
    /// [`Entity::Unknown`], whose category this build cannot tell.
    pub fn category(&self) -> Option<Category> {
        Some(match self {
            Entity::Profile(_) => Category::Profile,
            Entity::Post(_) => Category::Posts,
            Entity::Album(_) => Category::Albums,
            Entity::Comment(_) => Category::Comments,
            Entity::Reaction(_) => Category::Reactions,
            Entity::Event(_) => Category::Events,
            Entity::Group(_) => Category::Groups,
            Entity::Friendship(_) => Category::Friends,
            Entity::Thread(_) => Category::Threads,
            Entity::Message(_) => Category::Messages,
            Entity::Unknown(_) => return None,
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct DateRange {
    pub first: Timestamp,
    pub last: Timestamp,
}

impl DateRange {
    /// Widens the range to include `t`; a `None` range becomes `[t, t]`.
    pub fn extend(range: &mut Option<DateRange>, t: Timestamp) {
        match range {
            None => *range = Some(DateRange { first: t, last: t }),
            Some(r) => {
                if t < r.first {
                    r.first = t;
                }
                if t > r.last {
                    r.last = t;
                }
            }
        }
    }
}

/// The index a parser produces before anything is uploaded: what the
/// wizard's Archive step shows (`archive-import.md` § The wizard, step 2).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ArchiveSummary {
    pub platform: Platform,
    pub owner: ExternalActorRef,
    pub date_range: Option<DateRange>,
    pub counts: CategoryCounts,
    pub media_bytes: u64,
    pub parser_version: u32,
    /// Per-audience record counts over posts + albums. Additive: absent on a
    /// summary written before 2026-09-08, which reads as all-zero.
    #[serde(default)]
    pub audiences: AudienceCounts,
}

/// The sorted, de-duplicated media creation instants (epoch micros) a post's
/// or message's derived ID folds in — a set, so member order inside the export
/// never changes an ID. Why instants and not bytes: `crate::external_id`'s
/// module doc.
pub type MediaInstants = BTreeSet<u64>;

/// The instants of `media` that carry one (`taken_at`); a ref without a
/// recorded instant contributes nothing, so such a post's ID rests on
/// `created_at` + text alone.
pub fn media_instants(media: &[ArchiveMediaRef]) -> MediaInstants {
    media
        .iter()
        .filter_map(|m| m.taken_at)
        .map(|t| t.0)
        .collect()
}

#[cfg(test)]
mod platform_token_tests {
    use super::{FACEBOOK_TOKEN, INSTAGRAM_TOKEN, Platform, Timestamp};

    /// The serde form and `Platform::token()` can never split: this pins the
    /// actual wire byte shape `#[serde(rename_all = "lowercase")]` produces
    /// against the token constants, for every platform in `ALL`.
    #[test]
    fn the_serde_form_matches_the_token() {
        assert_eq!(FACEBOOK_TOKEN, "facebook");
        assert_eq!(INSTAGRAM_TOKEN, "instagram");
        for platform in Platform::ALL {
            assert_eq!(
                serde_json::to_string(&platform).unwrap(),
                format!("\"{}\"", platform.token()),
            );
            assert_eq!(
                Platform::parse_token(platform.token()).as_ref(),
                Some(&platform)
            );
            assert_eq!(
                Platform::parse_token(&format!(" {} ", platform.token().to_uppercase())).as_ref(),
                Some(&platform)
            );
        }
        assert_eq!(Platform::parse_token("myspace"), None);
    }

    /// The instant is the bare integer on the wire — the shape a consumer's
    /// own epoch-micros type (Fauna's included) shares byte for byte.
    #[test]
    fn a_timestamp_is_the_bare_integer_on_the_wire() {
        let bytes = serde_ipld_dagcbor::to_vec(&Timestamp(1_600_000_000_000_000)).unwrap();
        assert_eq!(
            bytes,
            serde_ipld_dagcbor::to_vec(&1_600_000_000_000_000u64).unwrap()
        );
        let back: Timestamp = super::unknown_arm_tests::try_dec(&bytes).unwrap();
        assert_eq!(back, Timestamp(1_600_000_000_000_000));
    }
}

/// The open arms (`transport.md` § Schema and forward-compat discipline →
/// *Rule 3 in full*): each newer writer is a test-only twin with one variant
/// this build lacks, encoded as canonical dag-cbor (the form every model and
/// state file is stored in) and decoded with the real type.
#[cfg(test)]
mod unknown_arm_tests {
    use super::*;

    fn enc<T: Serialize>(v: &T) -> Vec<u8> {
        serde_ipld_dagcbor::to_vec(v).unwrap()
    }

    /// The one raw dag-cbor decode in this file's tests (the crate is
    /// crates.io-only, so `fauna_cbor` is out of reach; the bytes are the
    /// tests' own encodes).
    pub(super) fn try_dec<T: serde::de::DeserializeOwned>(
        bytes: &[u8],
    ) -> Result<T, serde_ipld_dagcbor::DecodeError<std::convert::Infallible>> {
        serde_ipld_dagcbor::from_slice(bytes)
    }

    fn dec<T: serde::de::DeserializeOwned>(bytes: &[u8]) -> T {
        try_dec(bytes).unwrap()
    }

    #[derive(Serialize)]
    #[serde(rename_all = "lowercase")]
    enum NewerPlatform {
        Facebook,
        Mastodon,
    }

    #[derive(Serialize)]
    #[serde(rename_all = "snake_case")]
    enum NewerEntityKind {
        Post,
        Story,
    }

    #[derive(Serialize)]
    struct NewerExternalId {
        platform: NewerPlatform,
        kind: NewerEntityKind,
        id: String,
    }

    /// An `ExternalId` naming a platform and a kind this build lacks decodes,
    /// carries both tokens, and re-encodes to the newer writer's bytes — so a
    /// stored dedup list survives an older reader.
    #[test]
    fn an_unknown_platform_and_kind_are_carried_inside_an_external_id() {
        let bytes = enc(&NewerExternalId {
            platform: NewerPlatform::Mastodon,
            kind: NewerEntityKind::Story,
            id: "s1".into(),
        });
        let id: ExternalId = dec(&bytes);
        assert_eq!(id.platform, Platform::Other("mastodon".into()));
        assert_eq!(id.kind, EntityKind::Other("story".into()));
        assert!(!id.platform.is_known());
        assert_eq!(id.platform.token(), "mastodon");
        assert_eq!(id.kind.token(), "story");
        // No hand-written projection reads it as a known platform.
        assert_eq!(Platform::parse_token(id.platform.token()), None);
        assert_eq!(enc(&id), bytes);

        // The known spellings still decode as themselves.
        let known = enc(&NewerExternalId {
            platform: NewerPlatform::Facebook,
            kind: NewerEntityKind::Post,
            id: "p1".into(),
        });
        let id: ExternalId = dec(&known);
        assert_eq!(
            id,
            ExternalId::native(Platform::Facebook, EntityKind::Post, "p1")
        );
    }

    #[derive(Serialize)]
    #[serde(rename_all = "snake_case")]
    enum NewerCategory {
        Posts,
        Reels,
    }

    /// A category list naming one this build lacks decodes; the unknown one
    /// counts nothing, sorts after every known category, and re-encodes as
    /// read.
    #[test]
    fn an_unknown_category_is_carried_and_counts_nothing() {
        let bytes = enc(&vec![NewerCategory::Posts, NewerCategory::Reels]);
        let list: Vec<Category> = dec(&bytes);
        let reels = Category::Other("reels".into());
        assert_eq!(list, vec![Category::Posts, reels.clone()]);
        assert!(!reels.is_known());
        assert!(!Category::ALL.contains(&reels));
        assert!(Category::ALL.iter().all(|c| c < &reels));
        let mut counts = CategoryCounts::default();
        counts.set(&reels, 9);
        assert_eq!(counts, CategoryCounts::default());
        assert_eq!(counts.get(&reels), 0);
        assert_eq!(enc(&list), bytes);
    }

    #[derive(Serialize)]
    #[serde(rename_all = "snake_case")]
    enum NewerReactionKind {
        Like,
        Pride,
        Sticker { pack: String },
        Other { raw: String },
    }

    /// A reaction tag this build lacks — a unit tag or a data variant's key —
    /// routes into the parser's own `Other { raw }` fallback instead of failing.
    #[test]
    fn an_unknown_reaction_tag_routes_into_other() {
        let kinds: Vec<ReactionKind> = dec(&enc(&vec![
            NewerReactionKind::Like,
            NewerReactionKind::Pride,
            NewerReactionKind::Sticker { pack: "p".into() },
            NewerReactionKind::Other {
                raw: "PRIDE".into(),
            },
        ]));
        assert_eq!(
            kinds,
            vec![
                ReactionKind::Like,
                ReactionKind::Other {
                    raw: "pride".into()
                },
                ReactionKind::Other {
                    raw: "sticker".into()
                },
                ReactionKind::Other {
                    raw: "PRIDE".into()
                },
            ]
        );
        // Every kind this build writes reads back as itself.
        let all = vec![
            ReactionKind::Like,
            ReactionKind::Love,
            ReactionKind::Care,
            ReactionKind::Haha,
            ReactionKind::Wow,
            ReactionKind::Sad,
            ReactionKind::Angry,
            ReactionKind::Other { raw: "X".into() },
        ];
        assert_eq!(dec::<Vec<ReactionKind>>(&enc(&all)), all);
        // A malformed fallback is still refused.
        let bad = enc(&std::collections::BTreeMap::from([("other", 5u8)]));
        assert!(try_dec::<ReactionKind>(&bad).is_err());
    }

    #[derive(Serialize)]
    #[serde(rename_all = "snake_case")]
    enum NewerEntity {
        Group(ArchiveGroup),
        Story {
            #[serde(with = "serde_bytes_twin")]
            raw: Vec<u8>,
            n: i64,
            tags: Vec<String>,
        },
    }

    /// `serde_bytes` without the dependency: a byte string, as a newer writer
    /// would store raw bytes.
    mod serde_bytes_twin {
        pub fn serialize<S: serde::Serializer>(v: &[u8], s: S) -> Result<S::Ok, S::Error> {
            s.serialize_bytes(v)
        }
    }

    fn group(id: &str) -> ArchiveGroup {
        ArchiveGroup {
            external_id: ExternalId::native(Platform::Facebook, EntityKind::Group, id),
            name: format!("group {id}"),
            joined_at: None,
            url: None,
        }
    }

    /// A model file holding an entity this build lacks decodes with the rest;
    /// the unknown one belongs to no category and re-encodes byte-identically.
    #[test]
    fn an_unknown_entity_is_carried_and_the_file_decodes() {
        let bytes = enc(&vec![
            NewerEntity::Group(group("a")),
            NewerEntity::Story {
                raw: vec![0, 1, 255],
                n: -7,
                tags: vec!["x".into()],
            },
            NewerEntity::Group(group("b")),
        ]);
        let entities: Vec<Entity> = dec(&bytes);
        assert_eq!(entities.len(), 3);
        assert_eq!(entities[0], Entity::Group(group("a")));
        assert!(
            matches!(entities[1], Entity::Unknown(_)),
            "{:?}",
            entities[1]
        );
        assert_eq!(entities[1].category(), None);
        assert_eq!(entities[2], Entity::Group(group("b")));
        assert_eq!(enc(&entities), bytes);
    }
}
