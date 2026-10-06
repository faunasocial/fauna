//! **One** custody resolution for the sealed-label read surfaces
//! (`docs/goal/behavior/file-sync.md` § Sealed names & paths).
//!
//! [`crate::path_crypto::render_sealed_label`] is the render *policy* — seal
//! first, resting plaintext, otherwise omit — and
//! [`crate::file_download::FileDownloadKeys::label_open_roots`] is the *root
//! selection*. Both take custody as an input. This module is the missing third
//! piece: **how a read surface obtains that custody for a given folder**, and
//! **which salt it opens under**.
//!
//! It exists because there are four such surfaces (media list, snapshot browse,
//! snapshot diff, conflicts) and the first one to be built resolved custody
//! inline. Three more copies of a twelve-line resolution, each free to differ on
//! whether the owner key is carried through for a bound set or which salt wins
//! when the wire carries both, is precisely the drift the ruling's one-seam read
//! half exists to prevent — and the failure would be silent, because a wrong
//! root and a wrong salt both degrade to [`SealedLabelRender::Omit`] rather than
//! erroring.
//!
//! ## The two decisions this module owns
//!
//! 1. **The owner key is carried through even for a bound set.** The chunk path
//!    suppresses it (`effective_backup_key`, FS-5DC); the label path must not,
//!    or a set's owner cannot render the names it sealed under the owner root
//!    *before* the set was bound. Safe because a post-binding label is stamped
//!    `gen: Some(v)` and never reaches the owner arm — see
//!    [`crate::file_download::FileDownloadKeys::label_open_roots`], which
//!    documents why the extra candidate is unreachable rather than a downgrade.
//!    ⚠ That invariant is a property of the SEAL SITES, not of this module:
//!    [`LabelCustody::owner_only`] custody stamps a bound set's label
//!    `gen: None` under the owner root (its `keys_for` has no resolver to learn
//!    bound-ness from proven on the FFI snapshots
//!    façade), which is why every production sealing surface wires a real
//!    resolver and `owner_only` has no production seal-side caller. Adding one
//!    re-opens: the pin
//!    `owner_only_custody_seals_a_bound_set_where_a_member_cannot_follow`
//!    documents the trap.
//! 2. **The wire's `path_hash` is the salt — or there is no salt.** A `path`
//!    seal is *convergent*: its nonce derives from the salt, so a reader with
//!    no plaintext has nothing to derive from, and the nest rests no plaintext
//!    on a sealed plane. Every carrier ships `path_sealed` and `path_hash`
//!    together or withholds both (the label-audience gate), so a seal never
//!    arrives without its salt: a row with no wire hash is a row with no seal
//!    to open, and it renders from its resting plaintext (a `public`-audience
//!    folder) or omits. Deriving the salt from the plaintext was the
//!    expand-era fallback and is gone with the compat-remnant sweep
//!    (`version-compatibility.md` § Dimension 2, the fourth exception); the
//!    one derivation left is the fail-soft degrade for a *malformed* wire hash,
//!    shared with the set-name and import-source planes.

use std::sync::Arc;

use crate::crypto::BackupKey;
use crate::file_download::FileDownloadKeys;
use crate::folder_keys::FolderKeyResolver;
use crate::path_crypto::{self, LabelField, SealedLabelRender};

/// The reader's label-opening custody: an optional shared-set resolver plus the
/// optional owner [`BackupKey`].
///
/// Both halves are optional and the empty custody is meaningful — a surface
/// with no keys wired renders from the resting plaintext (public-audience folders, plaintext
/// planes, a keyless writer), which is what lets every consumer adopt this
/// additively.
#[derive(Clone, Default)]
pub struct LabelCustody {
    resolver: Option<Arc<dyn FolderKeyResolver>>,
    owner: Option<BackupKey>,
    predecessors: Vec<crate::file_download::PredecessorSealKey>,
}

impl std::fmt::Debug for LabelCustody {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Never render key material; a resolver has no renderable state.
        f.debug_struct("LabelCustody")
            .field("resolver", &self.resolver.is_some())
            .field("owner", &self.owner.is_some())
            .field("predecessors", &self.predecessors.len())
            .finish()
    }
}

impl LabelCustody {
    /// Custody from a shared-set resolver and/or an owner key. `LabelCustody::default()`
    /// is the keyless reader.
    pub fn new(resolver: Option<Arc<dyn FolderKeyResolver>>, owner: Option<BackupKey>) -> Self {
        Self {
            resolver,
            owner,
            predecessors: Vec::new(),
        }
    }

    /// Owner-only custody — no shared-set resolver wired (web today, tests, and
    /// any surface whose platform glue has not built one).
    pub fn owner_only(owner: BackupKey) -> Self {
        Self {
            resolver: None,
            owner: Some(owner),
            predecessors: Vec::new(),
        }
    }

    /// Offer the retired owner keys of the identities this account **succeeded
    /// from** as label-open candidates — builder-style, so the many existing
    /// [`Self::new`] / [`Self::owner_only`] call sites stay untouched and a
    /// surface that has no registry to resolve them from keeps exactly today's
    /// behavior (`succession-aftermath.md` § Re-key scope, the `BackupKey` corpus
    /// row).
    ///
    /// This is the **label** half of the read fallback whose byte half is
    /// [`FileDownloadKeys::predecessor_backup_keys`]; the two travel together
    /// deliberately, because a successor that opened its bytes but not its
    /// *names* renders an empty file list over a corpus it can in fact read.
    ///
    /// ⚠ **Read-only, and structurally so.** The value reaches
    /// [`FileDownloadKeys::predecessor_backup_keys`] and nothing else, and that
    /// field is never consulted by a seal root — see its own doc for why a seal
    /// under a retired key would be *silent* rather than an error. Do not
    /// "simplify" this into an extra entry on [`Self::owner`]: the separation is
    /// what makes the write-side mistake unrepresentable.
    ///
    /// The list is the whole ancestor chain, nearest hop first, exactly as
    /// `fauna_client_accounts::AccountRegistry::predecessor_backup_keys` returns
    /// it — a corpus can still rest under a grandpredecessor when an intermediate
    /// re-seal never finished. Empty for every identity that never succeeded,
    /// which is what keeps this free for the overwhelmingly common case.
    pub fn with_predecessors(mut self, predecessors: Vec<BackupKey>) -> Self {
        self.predecessors = predecessors.into_iter().map(Into::into).collect();
        self
    }

    /// [`Self::with_predecessors`] with each key **paired with the identity it
    /// belongs to**, nearest hop first — the shape
    /// `AccountRegistry::predecessor_backup_keys_by_actor` returns. Only a
    /// paired key is ever offered to a row signed as a predecessor
    /// (`mls-group-key-material.md` § M2 → *Writer-signed change records*,
    /// ruling (8)(c); [`crate::file_download::RecordSigner`]); an unpaired one
    /// still opens what the current identity signed.
    pub fn with_predecessor_chain(
        mut self,
        chain: Vec<(crate::identity::ActorId, BackupKey)>,
    ) -> Self {
        self.predecessors = crate::file_download::PredecessorSealKey::chain(chain);
        self
    }

    /// The retired keys as a caller already holds them — paired where it
    /// named them ([`Self::with_predecessor_chain`]), unpaired where not
    /// ([`Self::with_predecessors`]), nearest hop first.
    pub fn with_predecessor_keys(
        mut self,
        keys: Vec<crate::file_download::PredecessorSealKey>,
    ) -> Self {
        self.predecessors = keys;
        self
    }

    /// How many retired owner keys this custody offers. The call-site wiring pin's
    /// observable — the twin of [`Self::has_resolver`] / [`Self::has_owner_key`],
    /// and for the same reason (a shape pin inside `fauna-core`
    /// cannot observe a *caller* that stopped passing the value; the regression
    /// pin has to live at the call site, and this accessor is what it asserts).
    pub fn predecessor_count(&self) -> usize {
        self.predecessors.len()
    }

    /// Whether a shared-set resolver is wired. This is the load-bearing
    /// bit: without one, `keys_for` on a bound set falls to the owner arm and a
    /// label-audience field seals under a root no roster member can open. It is
    /// public so a *consumer* can pin its own wiring (a
    /// custody-shape pin in `fauna-core` cannot observe a caller reverting to
    /// [`Self::owner_only`] — the regression pin must live at the call site,
    /// and this accessor is what it asserts).
    pub fn has_resolver(&self) -> bool {
        self.resolver.is_some()
    }

    /// Whether the owner [`BackupKey`] is carried — the arm an *unbound* set
    /// seals and renders under. The positive-control twin of
    /// [`Self::has_resolver`] for call-site wiring pins.
    pub fn has_owner_key(&self) -> bool {
        self.owner.is_some()
    }

    /// The reader's own [`BackupKey`], for the surfaces whose label has **no
    /// folder** to resolve custody for — today just the device plane
    /// ([`seal_device_label`]), whose root is the registering actor's.
    ///
    /// Everything set-scoped must go through [`Self::keys_for`] instead: this
    /// accessor cannot see a shared set's content keys, so using it there would
    /// silently omit every bound set's labels.
    pub fn owner_key(&self) -> Option<BackupKey> {
        self.owner.clone()
    }

    /// The **read** custody for those same no-folder surfaces: the owner root
    /// plus every retired root this account succeeded from.
    ///
    /// The render-side twin of [`Self::owner_key`], which stays the **seal**-side
    /// accessor. Both device labels and a set's owner-audience include/exclude
    /// path lists are sealed under the registering/owning actor's root, so after
    /// a succession a successor's *existing* rows are still under a predecessor's
    /// — and the degrade is silent on both planes (a device label falls to
    /// `Omit`, a path pair to `None`, i.e. "this reader has no list to show").
    /// Offering the retired roots here is the same read fallback the set-scoped
    /// plane gets from [`Self::keys_for`].
    ///
    /// ⚠ **Render only.** `seal_device_label` and every other seal site must keep
    /// using [`Self::owner_key`]: sealing a *new* label under a retired root
    /// would be exactly the silent wrong-root write
    /// [`FileDownloadKeys::predecessor_backup_keys`] is shaped to make
    /// unrepresentable.
    pub fn owner_plane_read_keys(&self) -> FileDownloadKeys {
        match self.owner.clone() {
            Some(key) => FileDownloadKeys {
                predecessor_backup_keys: self.predecessor_roots(),
                ..FileDownloadKeys::owner(key)
            },
            None => FileDownloadKeys::default(),
        }
    }

    /// The reader's [`FileDownloadKeys`] for one folder, plus the set's
    /// **home-nest base URL** when it is a foreign (cross-nest) set.
    ///
    /// ⚠ **An empty custody must still go through here — never short-circuit the
    /// render on "this reader holds no keys".** It is tempting (it looks like a
    /// free optimization) and it is wrong: a keyless reader meeting a
    /// *sealed-only* row has to reach
    /// [`SealedLabelRender::Omit`], and skipping the render instead renders the
    /// row's blank plaintext as its name — the one outcome the ratified degrade
    /// exists to forbid. The cheap guard is *"nothing on this page is sealed"*,
    /// which is a pure field test and costs nothing. (A keyless
    /// `keys_for` does no I/O anyway: with no resolver it returns immediately.)
    ///
    /// The second element — a FOREIGN set's home nest, URL plus the identity
    /// its byte-plane dial is verified against — is only meaningful to a *byte*
    /// consumer; a label render ignores it (the sealed label rides the row,
    /// wherever the bytes live). It is returned here so the Media download path
    /// can share this one assembly rather than keep a second one that could
    /// resolve differently.
    ///
    /// The resolver's three-valued answer maps 1:1 onto key shapes — see
    /// [`FolderKeyResolver`]'s contract for why the distinction is
    /// load-bearing on the **seal** direction:
    ///
    /// - content-keyed (`Ok(ContentKeyed)` — bound to a group, or served
    ///   group-less, the `served` marker) → resolver keys, `content_keys`
    ///   possibly absent (content-keyed-but-unresolvable:
    ///   [`FileDownloadKeys::label_seal_root`] bails, opens fail closed);
    /// - positively owner-only (`Ok(OwnerOnly)`), or no resolver wired → the
    ///   owner fallback, the *correct* custody for an owned unbound set — plus
    ///   a since-unflagged serve window's generations as read candidates;
    /// - resolve **failed** (`Err`) → no keys at all. Never the owner
    ///   fallback: a seal site reading "could not determine" as "unbound"
    ///   would mint an owner-root seal for a possibly-bound set — the
    ///   wrong-root class. No keys means a seal records
    ///   plaintext-only (the ratified degrade; a later backfill converges it)
    ///   and a render omits for this one pass.
    pub async fn keys_for(
        &self,
        folder: &str,
    ) -> (FileDownloadKeys, Option<crate::folder_keys::ForeignHome>) {
        self.keys_for_hash(&path_crypto::set_name_hash(folder))
            .await
    }

    /// [`Self::keys_for`] for a **wire row** that names its set: by the row's
    /// own `name_hash` (`folder_hash`) when it carries one, the hash of its
    /// plaintext otherwise ([`set_name_label_salt`]).
    ///
    /// ⚠ Every render over a nest reply's set name goes through this, never
    /// [`Self::keys_for`] on the reply's plaintext: once the nest scrubs a
    /// sealed set's `name`, that plaintext is the empty-string sentinel and a
    /// bound set would resolve no content keys — its names and paths would all
    /// omit (`path-sealing.md` § the set-name plane). [`Self::keys_for`] stays
    /// right for a name the user chose or a render already opened.
    pub async fn keys_for_row(
        &self,
        plaintext: &str,
        wire_hash: Option<&[u8]>,
    ) -> (FileDownloadKeys, Option<crate::folder_keys::ForeignHome>) {
        self.keys_for_hash(&set_name_label_salt(wire_hash, plaintext))
            .await
    }

    /// [`Self::keys_for`] by the set's **`name_hash`** — the address a roster
    /// row carries once its plaintext `name` rests sealed. The sealed-label
    /// render of the set's own name is the caller that has nothing else: it
    /// needs these keys *before* it can open the name (`path-sealing.md`
    /// § the set-name plane).
    pub async fn keys_for_hash(
        &self,
        name_hash: &[u8; 32],
    ) -> (FileDownloadKeys, Option<crate::folder_keys::ForeignHome>) {
        if let Some(resolver) = self.resolver.as_ref() {
            match resolver.resolve(name_hash).await {
                Ok(crate::folder_keys::ResolvedCustody::ContentKeyed(resolved)) => {
                    let home =
                        resolved
                            .home_nest_url
                            .map(|nest_url| crate::folder_keys::ForeignHome {
                                nest_url,
                                nest_actor_id: resolved.home_nest_actor_id,
                            });
                    // A content-keyed set with no group is the served-unshared
                    // shape: the marker rides so a keyless resolve still fails
                    // closed instead of reading as owner-only.
                    let served = resolved.mls_group_id.is_none();
                    return (
                        FileDownloadKeys {
                            // Carried through for a bound set on purpose —
                            // decision (1) in this module's docs.
                            backup_key: self.owner.clone().map(Into::into),
                            // Travels with `backup_key` arm for arm: the label
                            // path keeps the owner root on a bound set so an
                            // owner still renders the names it sealed *before*
                            // binding, and a predecessor's pre-binding names are
                            // the same case one identity back. The chunk path
                            // suppresses both together (FS-5DC), in the one place
                            // that already decides it.
                            predecessor_backup_keys: self.predecessor_roots(),
                            mls_group_id: resolved.mls_group_id,
                            served,
                            content_keys: resolved.content_keys,
                            ..Default::default()
                        },
                        home,
                    );
                }
                Ok(crate::folder_keys::ResolvedCustody::OwnerOnly {
                    retired_content_keys,
                }) => {
                    // The owner path, plus any since-unflagged serve window's
                    // generations as READ candidates — the label twin of the
                    // engine's `set_retired_content_keys`. A separate field
                    // from `content_keys` so it can never trip the
                    // content-keyed gate (`FileDownloadKeys::retired_content_keys`).
                    return (
                        match self.owner.clone() {
                            Some(key) => FileDownloadKeys {
                                predecessor_backup_keys: self.predecessor_roots(),
                                retired_content_keys,
                                ..FileDownloadKeys::owner(key)
                            },
                            // No owner key, no retired candidates either: the
                            // retired generations widen the owner path, they
                            // are not a custody of their own (same rule as the
                            // predecessor list below).
                            None => FileDownloadKeys::default(),
                        },
                        None,
                    );
                }
                Err(e) => {
                    // No set name in the log (S7 — user-chosen names never
                    // rest in logs).
                    tracing::warn!(
                        error = %e,
                        "folder custody resolution failed; yielding no keys \
                         (seals record plaintext-only, renders omit this pass)"
                    );
                    return (FileDownloadKeys::default(), None);
                }
            }
        }
        (
            match self.owner.clone() {
                Some(key) => FileDownloadKeys {
                    predecessor_backup_keys: self.predecessor_roots(),
                    ..FileDownloadKeys::owner(key)
                },
                // No owner key: a reader with no current root of its own has no
                // business opening a *retired* one either — the predecessor list
                // is a fallback beside `backup_key`, never a standalone custody.
                // (In practice the two are populated from the same account, so
                // this arm is the keyless reader and stays exactly as it was.)
                None => FileDownloadKeys::default(),
            },
            None,
        )
    }

    /// The retired roots, paired where the host named them, in registry order (nearest hop
    /// first). One place, so the two `keys_for` arms cannot drift.
    fn predecessor_roots(&self) -> Vec<crate::file_download::PredecessorSealKey> {
        self.predecessors.clone()
    }
}

/// The salt a **path** label opens under: the wire's `path_hash`, or `None`
/// when the wire carries none — there is then no seal to open either.
///
/// Decision (2) in this module's docs. `wire_hash` is the row's `path_hash`
/// field as bytes. A **malformed** one (wrong length) degrades to the
/// plaintext derivation rather than poisoning the render — the same fail-soft
/// the set-name and import-source salts keep: inert where the plaintext is
/// scrubbed (the derivation of the empty sentinel opens nothing, and the row
/// omits as a bad hash deserves), correct where it rests (a `public`-audience
/// folder). An **absent** one is not a degrade case: every carrier ships
/// `path_sealed` and `path_hash` together or withholds both, so `None` here
/// means the render has no seal to attempt and falls straight to the
/// plaintext-or-omit half.
pub fn path_label_salt(wire_hash: Option<&[u8]>, plaintext: &str) -> Option<[u8; 32]> {
    let bytes = wire_hash?;
    Some(<[u8; 32]>::try_from(bytes).unwrap_or_else(|_| crate::sync::path_hash(plaintext)))
}

/// Render one **path** label sealed-first — [`path_label_salt`] +
/// [`crate::path_crypto::render_sealed_label`] in the one order every path
/// surface must use; with no wire salt the seal is never attempted and the
/// row renders from its resting plaintext or omits
/// ([`SealedLabelRender::from_plaintext`]).
///
/// Every surface that shows a folder-relative path (media list, snapshot
/// browse, snapshot diff, conflicts, the WebDAV MDA's listing) calls exactly
/// this, so none of them can drift on salt selection or on the degrade.
///
/// **An opened seal is bound to its own row** (`path-sealing.md` § *An opened
/// path is bound to its own row*): a seal whose plaintext does not hash to the
/// salt it opened under is treated exactly like a seal that did not open —
/// the row falls to its resting plaintext (which the nest itself hashed on
/// ingest) or omits. The same [`path_bound_to_salt`] check the apply path's
/// [`open_change_path`] refuses on, because at least one caller — the WebDAV
/// MDA — *acts* on the rendered name (GET, DELETE, COPY/MOVE resolve it), and
/// a name the nest files under another row would have it act on the wrong one.
pub fn render_path(
    keys: &FileDownloadKeys,
    sealed: Option<&[u8]>,
    plaintext: &str,
    wire_hash: Option<&[u8]>,
    field: LabelField,
) -> SealedLabelRender {
    let Some(salt) = path_label_salt(wire_hash, plaintext) else {
        return SealedLabelRender::from_plaintext(Some(plaintext));
    };
    match path_crypto::render_sealed_label(keys, sealed, Some(plaintext), &salt, field) {
        SealedLabelRender::Sealed(path) if !path_bound_to_salt(&path, &salt) => {
            SealedLabelRender::from_plaintext(Some(plaintext))
        }
        rendered => rendered,
    }
}

/// The binding check: does an opened path hash to the salt it was sealed
/// under — the row's own `path_hash`? The AEAD binds the salt, never what the
/// sealer put inside it, so this is the only thing that ties a label to its
/// row. One function for the render ([`render_path`]) and the apply
/// ([`open_change_path`]) halves, so they cannot disagree on it.
pub fn path_bound_to_salt(path: &str, salt: &[u8; 32]) -> bool {
    crate::sync::path_hash(path) == *salt
}

/// What opening a sync change row's sealed path yielded — the **apply**
/// half of [`render_path`], for the one consumer that must not degrade to
/// `Omit`.
///
/// A read surface that cannot open a label shows nothing and moves on; an
/// applier that cannot open one is deciding the row's fate in the anchor's
/// accounting, where "show nothing" is not an option. The three fates do
/// opposite things (`docs/goal/behavior/path-sealing.md` § Apply-path degrade
/// ruling), so the apply path needs them told apart, not collapsed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ChangePathOpen {
    /// The seal opened under one of this holder's roots **and** the plaintext
    /// hashes to the row's own `path_hash`. The only outcome that may be
    /// written to disk.
    Opened(String),
    /// This row can never be applied — here or on any other device, now or
    /// ever. Permanent for that change: record it and advance past it
    /// (`docs/goal/behavior/file-sync.md` § *A failed change must not strand
    /// the device*), never a cap that freezes the set behind it.
    Refused(ChangePathRefusal),
    /// No root this holder offers opens the envelope. Transient — an M2
    /// generation can lag its changes — so the caller holds the anchor below
    /// this row and a later pull retries it.
    NoRoot,
}

/// Why a sealed change path can never be applied. Every variant is decidable
/// **locally** — from the row's own bytes, or for
/// [`SignerBound`](Self::SignerBound) from its verdict and the reader's own
/// chain: no variant means "ask again later", which is what keeps
/// [`ChangePathOpen::Refused`] disjoint from [`ChangePathOpen::NoRoot`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChangePathRefusal {
    /// The row carries no `path_sealed` at all — and the applier consults the
    /// seal only for a row that also carries no plaintext `path`, so this is
    /// the row with neither. No current writer lands one on a plane whose
    /// plaintext scrubs (the nest refuses a seal-less record there,
    /// `path_seal_required`), and a plaintext-resting plane serves its `path`;
    /// so the shape names a row no applier anywhere can use, and it is refused
    /// like the other locally-decidable classes rather than skipped in silence.
    /// It was the pre-expand hash-only rows' skip-and-advance arm until the
    /// compat-remnant sweep's baseline reset retired those rows.
    NoSeal,
    /// `path_sealed` is not a decodable [`path_crypto::SealedLabel`]. No key
    /// makes malformed CBOR parse.
    Envelope,
    /// The row's `path_hash` is not 32 hex-encoded bytes, so the salt the
    /// writer sealed under cannot be reconstructed — and for a change row
    /// there is no plaintext to derive it from either (the nest rests none on
    /// a sealed plane). [`path_label_salt`]'s malformed-hash derivation is a
    /// *read* surface's degrade; on the apply path it would silently open
    /// nothing forever. Decided BEFORE every other class, so a malformed hash
    /// is always this refusal, never `NoSeal` or `Envelope`.
    Salt,
    /// The seal opened but its plaintext is not UTF-8, so there is no path to
    /// write to.
    NotUtf8,
    /// The seal opened and the plaintext is a path — but **not this row's**:
    /// `path_hash(opened) != path_hash`. A writer holding the set's label key
    /// sealed path P under Q's hash, so every device would write P while the
    /// nest's per-path heads, conflicts and history record Q. Refused, not
    /// applied.
    Unbound,
    /// The row is signed as a **retired identity** of the reading account and
    /// its unstamped label opens under none of the roots that identity may
    /// open (`mls-group-key-material.md` § M2 → *Writer-signed change
    /// records*, ruling (8)(c)): sealed under a later root, or this host holds
    /// no root of that identity. Decided by the applier from the row's verdict
    /// and its own chain, not by this opener (which never sees a signer) — the
    /// one class that is not a property of the row's bytes alone; ruling
    /// (8)(f)'s re-judge is what revisits it when a root arrives.
    SignerBound,
}

impl ChangePathRefusal {
    /// A stable, label-free reason for the recorded conflict row and the log.
    /// Never carries path content — the S7 log scrub must not have new work
    /// created for it here.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::NoSeal => "row carries neither a plaintext path nor a sealed one",
            Self::Envelope => "sealed path envelope did not decode",
            Self::Salt => "row path_hash is not 32 hex bytes",
            Self::NotUtf8 => "opened sealed path is not UTF-8",
            Self::Unbound => "opened sealed path does not hash to the row's path_hash",
            Self::SignerBound => "sealed path rests under a root its signer may not open",
        }
    }
}

/// Open a sync change row's sealed path **and bind it to the row's own
/// `path_hash`** — the one opener every applier runs (the shared engine's
/// `open_sealed_change_paths`).
///
/// `resolve_roots` is the caller's custody, taking the envelope's own `gen`:
/// the engine hands it
/// [`FileDownloadKeys::label_open_roots`](crate::file_download::FileDownloadKeys::label_open_roots)
/// (generation-aware, fail-closed for a bound set); a single-owner host hands
/// back its one convergent chunk root. Root *selection* is genuinely
/// per-host; everything the verdict turns on is here, so hosts cannot drift
/// on it — the same one-funnel constraint `path_crypto` holds for the seal.
///
/// **The binding check is the point.** The AEAD binds the *salt* (the wire
/// `path_hash`) but says nothing about what plaintext the sealer put inside,
/// so opening alone proves only "someone with the set's label key sealed
/// something under this hash". Requiring `path_hash(opened) == path_hash`
/// closes that: a row can only ever name the path the nest filed it under.
///
/// `path_sealed` is the row's `path_sealed` as it came off the wire: `None` is
/// the row with no seal at all, decided here as [`ChangePathRefusal::NoSeal`]
/// so that no host grows its own arm for it (the since-removed headless
/// daemon's silent skip-and-advance for that shape lived beside this opener
/// until the compat-remnant sweep classed it a pre-expand remnant).
pub fn open_change_path(
    resolve_roots: impl FnOnce(Option<u64>) -> anyhow::Result<Vec<[u8; 32]>>,
    path_sealed: Option<&[u8]>,
    path_hash_hex: &str,
) -> ChangePathOpen {
    // The hash first, whatever else is wrong with the row: `Salt` is THE
    // verdict for a malformed `path_hash`, so a host keying anything on the
    // hash (the skip floor) agrees with the verdict about whether it is a key.
    let Some(salt) = hex::decode(path_hash_hex)
        .ok()
        .and_then(|b| <[u8; 32]>::try_from(b).ok())
    else {
        return ChangePathOpen::Refused(ChangePathRefusal::Salt);
    };
    let Some(path_sealed) = path_sealed else {
        return ChangePathOpen::Refused(ChangePathRefusal::NoSeal);
    };
    let Ok(envelope) = path_crypto::SealedLabel::from_bytes(path_sealed) else {
        return ChangePathOpen::Refused(ChangePathRefusal::Envelope);
    };
    let roots = match resolve_roots(envelope.generation) {
        // Both arms are one outcome: this holder offers no root that could
        // open this envelope. `Err` additionally means a fail-closed posture
        // fired (bound set, generation not held) — still transient, since the
        // generation may yet arrive.
        Ok(roots) if !roots.is_empty() => roots,
        _ => return ChangePathOpen::NoRoot,
    };
    let Ok(opened) = path_crypto::open(
        roots.iter(),
        &salt,
        path_crypto::LabelField::SyncChangePath,
        &envelope,
    ) else {
        // A wrong root and a tampered ciphertext are indistinguishable at the
        // AEAD tag, so this stays the transient class: refusing here would let
        // one unopenable row be filed as permanent on a device that simply has
        // not caught up on keys yet.
        return ChangePathOpen::NoRoot;
    };
    let Ok(path) = String::from_utf8(opened) else {
        return ChangePathOpen::Refused(ChangePathRefusal::NotUtf8);
    };
    if !path_bound_to_salt(&path, &salt) {
        return ChangePathOpen::Refused(ChangePathRefusal::Unbound);
    }
    ChangePathOpen::Opened(path)
}

/// The salt a **set-name** label opens under: the wire's `name_hash` when
/// present, [`path_crypto::set_name_hash`] of the plaintext otherwise.
///
/// Decision (2) of this module, for the set-name plane. Same degrade as
/// [`path_label_salt`]: a malformed wire hash falls back to the derivation
/// rather than poisoning the render.
pub fn set_name_label_salt(wire_hash: Option<&[u8]>, plaintext: &str) -> [u8; 32] {
    match wire_hash {
        Some(bytes) => {
            <[u8; 32]>::try_from(bytes).unwrap_or_else(|_| path_crypto::set_name_hash(plaintext))
        }
        None => path_crypto::set_name_hash(plaintext),
    }
}

/// Render one **folder name** sealed-first — the set-name twin of
/// [`render_path`], and the one seam every surface that shows a set name must
/// call.
///
/// ⚠ **The salt is load-bearing, not decorative.** `wire_hash` is the row's
/// `name_hash`; a plane that carries `name_sealed` *without* it renders fine
/// today (the salt derives from the resting plaintext) and turns every row into
/// [`SealedLabelRender::Omit`] the moment the plaintext scrubs. That exact hole
/// was found twice already — on `fauna.media.list` (S2b) and `WebdavFile` (S4) —
/// so check it on every new carrier before shipping.
pub fn render_set_name(
    keys: &FileDownloadKeys,
    sealed: Option<&[u8]>,
    plaintext: &str,
    wire_hash: Option<&[u8]>,
) -> SealedLabelRender {
    let salt = set_name_label_salt(wire_hash, plaintext);
    path_crypto::render_sealed_label(keys, sealed, Some(plaintext), &salt, LabelField::FolderName)
}

/// A set name as a share's Welcome carries it once the plaintext is scrubbed:
/// the seal ([`seal_set_name`]'s output, under the set's M2 content keys) and
/// the convergent salt it opens under. The two travel as one value, because
/// either alone opens nothing. The reader that can open it is a member that has
/// joined (a pending share holds no key), so a carrier holds it until the join
/// has ingested custody ([`render_set_name`] opens it).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SealedSetName {
    /// The sealed name bytes (`WelcomeInbox.set_name_sealed`).
    pub sealed: Vec<u8>,
    /// The name's convergent salt (`WelcomeInbox.set_name_hash`).
    pub name_hash: Vec<u8>,
}

impl SealedSetName {
    /// The pair from a wire carrier's two optional fields — `None` unless both
    /// are present.
    pub fn from_wire(sealed: Option<&[u8]>, name_hash: Option<&[u8]>) -> Option<Self> {
        Some(Self {
            sealed: sealed?.to_vec(),
            name_hash: name_hash?.to_vec(),
        })
    }
}

/// Seal one **user-chosen folder name** — the single place a client turns a set
/// name into a [`path_crypto::SealedLabel`], the set-name twin of
/// `SyncEngine::seal_recorded_path`.
///
/// The caller picks the `root` (the *only* thing that legitimately varies per
/// call site: a bound/served set's M2 content-key generation, else the owner's
/// `convergent_chunk_root()` — `mls-group-key-material.md` § M2 → *Sealed names &
/// paths*, rule #1: no new key category). Everything else — the convergent nonce,
/// the `name_hash` salt, the [`LabelField::FolderName`] domain tag, the
/// encoding — lives here, so two call sites cannot disagree.
///
/// **Convergent** (salt = the name's own `name_hash`): the salt determines the
/// plaintext, so re-stamping an unchanged name reproduces a byte-identical blob.
/// An idempotent re-seal therefore does not churn the column, and two devices
/// seal the same name identically.
///
/// `Ok(None)` means the name is a **reserved routing constant** and must not
/// seal ([`crate::sync::is_reserved_folder_name`]). Refusing here rather than
/// at each caller is deliberate: a sealed `__mail` would make the name
/// unreadable to the ~28 nest decision sites that route on it, and "every caller
/// remembers to check" is not a guarantee.
pub fn seal_set_name(root: &path_crypto::LabelRoot, name: &str) -> anyhow::Result<Option<Vec<u8>>> {
    if crate::sync::is_reserved_folder_name(name) {
        return Ok(None);
    }
    let salt = path_crypto::set_name_hash(name);
    let sealed =
        path_crypto::seal_convergent(root, &salt, LabelField::FolderName, name.as_bytes())?;
    Ok(Some(sealed.to_bytes()?))
}

/// Seal a **folder-relative path** — the one place, tree-wide, that any
/// client turns a path into a [`path_crypto::SealedLabel`].
///
/// As with [`seal_set_name`], the caller picks the `root` (a bound set's M2
/// content-key generation, else the owner's `convergent_chunk_root()`) — the
/// only thing that legitimately varies per call site — and everything else
/// lives here, so no two writers can drift apart on salt, tag or encoding.
///
/// ⚠ **There is deliberately no per-table variant of this function**, and the
/// field tag is [`LabelField::SyncChangePath`] for *every* path regardless of
/// which table it lands in — `sync_changes`, `snapshot_files`,
/// `backup_custody`, `sync_conflicts`, all of them. The nest **copies
/// `path_sealed` verbatim between tables** and holds no key to re-seal with
/// (`sync_changes` → `snapshot_files` at `db/sync_storage.rs`'s
/// `INSERT INTO snapshot_files … f.path_sealed`), which is what the
/// self-describing `gen` field exists to make safe. Since the tag is mixed into
/// both the key derivation and the AAD, a per-table tag would make every copied
/// blob fail to open — and fail *silently*, as
/// [`path_crypto::SealedLabelRender::Omit`] rather than an error. The dead
/// per-table variants on [`LabelField`] carry the same warning.
///
/// **Convergent nonce** (salt = the path's own `path_hash`): the salt
/// determines the plaintext, so re-recording an unchanged path reproduces a
/// byte-identical blob — an idempotent retry does not churn the column, and two
/// devices seal the same path identically.
pub fn seal_path(root: &path_crypto::LabelRoot, path: &str) -> anyhow::Result<Vec<u8>> {
    let salt = crate::sync::path_hash(path);
    path_crypto::seal_convergent(root, &salt, LabelField::SyncChangePath, path.as_bytes())?
        .to_bytes()
}

/// Resolve `keys`' seal root and seal `path` under it — the shared mechanics
/// behind every per-domain "seal a path on a user gesture" machine method
/// (devices' re-point record, media's delete/restore record, …):
/// [`FileDownloadKeys::label_seal_root`] then [`seal_path`].
///
/// Every error variant means the same thing to a caller — record plaintext-only
/// (an S8 backfill row), never surface a user-facing error — but callers keep
/// their own logging: the `tracing::target` a call site logs under must be a
/// compile-time literal (it cannot flow through a shared function), and the
/// per-domain gesture being recorded deserves its own wording. This function
/// shares the resolve-root-then-seal mechanics only, never the text.
pub fn seal_path_from_keys(
    keys: &FileDownloadKeys,
    path: &str,
) -> Result<Vec<u8>, SealPathFromKeysError> {
    let root = keys
        .label_seal_root()
        .map_err(SealPathFromKeysError::RootUnresolved)?;
    let Some(root) = root else {
        return Err(SealPathFromKeysError::NoRoot);
    };
    seal_path(&root, path).map_err(SealPathFromKeysError::SealFailed)
}

/// Why [`seal_path_from_keys`] did not produce sealed bytes.
#[derive(Debug)]
pub enum SealPathFromKeysError {
    /// This holder cannot seal at all — [`FileDownloadKeys::label_seal_root`]
    /// returned `None`. Not an error condition; nothing to log.
    NoRoot,
    /// A bound set's keys could not be resolved; fails closed rather than
    /// sealing under a root the roster could not open.
    RootUnresolved(anyhow::Error),
    /// The root resolved but sealing itself failed.
    SealFailed(anyhow::Error),
}

/// Seal a **conflict's free-text `details`**, salted by that conflict's own
/// path hash.
///
/// **Random nonce, not convergent.** `details` is mutable prose describing the
/// conflict; the salt does not determine it, so a derived nonce would reuse a
/// (key, nonce) pair across two different descriptions of the same path.
/// `file-sync.md` § Sealed names & paths names conflict `details` in the
/// random-nonce list explicitly.
///
/// **Sharing the path's salt is sound** — unlike the path, `details` keeps its
/// own [`LabelField::ConflictDetails`] tag (it is never copied between tables,
/// so it carries none of [`seal_path`]'s verbatim-copy constraint),
/// and the tag is mixed into both the key derivation and the AAD, so the two
/// fields derive different keys from the same salt value and neither blob opens
/// as the other.
pub fn seal_conflict_details(
    root: &path_crypto::LabelRoot,
    path: &str,
    details: &str,
) -> anyhow::Result<Vec<u8>> {
    seal_conflict_details_by_hash(root, &crate::sync::path_hash(path), details)
}

/// [`seal_conflict_details`] for a conflict filed with **no plaintext path** —
/// salted by the `path_hash` the report carries, exactly the salt
/// [`render_conflict_details`] takes from the wire. The one writer is a
/// catch-up change whose sealed path never opened (`conflicts.md` § Skipped
/// catch-up changes reach the review list): it forwards the change row's own
/// label pair and has only its hash to salt with.
pub fn seal_conflict_details_by_hash(
    root: &path_crypto::LabelRoot,
    path_hash: &[u8; 32],
    details: &str,
) -> anyhow::Result<Vec<u8>> {
    path_crypto::seal_random(
        root,
        path_hash,
        LabelField::ConflictDetails,
        details.as_bytes(),
    )?
    .to_bytes()
}

/// Render one conflict's **`details`** sealed-first — the seam every surface
/// showing conflict prose must call.
///
/// ⚠ `path_salt` is the conflict's **path** salt — [`path_label_salt`] of the
/// row's `path_hash`/`path`, the very same value the row's path render uses.
/// It is an explicit parameter rather than derived from `plaintext` precisely
/// because `plaintext` here is the *details* text: deriving from it would salt
/// with `path_hash(details)` and silently omit every row. `None` (no wire
/// salt) degrades exactly as [`render_path`] does: the seal is never
/// attempted and the details render from their plaintext or omit.
pub fn render_conflict_details(
    keys: &FileDownloadKeys,
    sealed: Option<&[u8]>,
    plaintext: &str,
    path_salt: Option<&[u8; 32]>,
) -> SealedLabelRender {
    match path_salt {
        Some(salt) => path_crypto::render_sealed_label(
            keys,
            sealed,
            Some(plaintext),
            salt,
            LabelField::ConflictDetails,
        ),
        None => SealedLabelRender::from_plaintext(Some(plaintext)),
    }
}

/// The nest-authored pseudo-device a WebDAV session's writes are attributed to
/// (`bins/fauna-nest/src/bridge_blob_handlers.rs`). Named in `file-sync.md`
/// § Sealed names & paths → *Deliberate non-seals*.
pub const WEBDAV_PSEUDO_DEVICE_LABEL: &str = "WebDAV";

/// The deterministic `sync_devices.device_id` of that pseudo-device, derived
/// from the actor it attributes writes to.
///
/// One home rather than an inline `derive_key` at the register site, because
/// two unrelated readers now need the same answer: the WebDAV write path that
/// *creates* the row, and the nest's device-quota count, which must not charge
/// a user's tier for a row the **nest itself** wrote. Deriving it per actor
/// bounds the exclusion to exactly one row per actor — but the id is only
/// *derivable*, not *reserved*, so `fauna.sync.register` refuses a
/// client-supplied `device_id` equal to this value outright (rather than
/// treating it as an ordinary re-register), the same way it refuses malformed
/// hex. (A label-based exclusion would not be safe either:
/// [`is_synthetic_device_label`] matches whatever label a caller sends, so any
/// number of rows could claim it.)
pub fn webdav_pseudo_device_id(actor_id: &[u8; 32]) -> [u8; 32] {
    blake3::derive_key("fauna.folders.webdav_pseudo_device.v1", actor_id)
}

/// The `device_id` every row a re-seed ceremony re-homes into a live folder
/// carries — the owner's **re-seed pseudo-device**, derived beside
/// [`webdav_pseudo_device_id`] and computed identically by the signing
/// ceremony and the nest that verifies it
/// (`writer-signed-change-records.md` ruling (7)(a)(ii)).
///
/// Neither the ceremony device's own id (every engine treats a row under its
/// own device id as a self-echo and downloads nothing for it, so the device
/// that ran the ceremony would never hydrate the folder it restored) nor the
/// nest's identity (no client holds a handle to it then). A label the row
/// carries — never a registered device, never an exemption: the row is
/// owner-signed. `fauna.sync.register` refuses it like the WebDAV id.
pub fn reseed_pseudo_device_id(actor_id: &[u8; 32]) -> [u8; 32] {
    blake3::derive_key("fauna.folders.reseed_pseudo_device.v1", actor_id)
}

/// Placeholder label for a self-heal registration — the first time an otherwise
/// unregistered device records a change. The device's *authoritative* label
/// arrives with its real registration, whose upsert supersedes this.
///
/// Re-exported as `fauna_client_sync::SELF_REGISTER_LABEL`, where it was
/// originally defined; it lives here so [`is_synthetic_device_label`] can name
/// it without `fauna-core` depending on a client crate.
pub const SELF_REGISTER_LABEL: &str = "fauna";

/// Is this device label machine-authored rather than user-chosen?
///
/// The *paths-are-content* ruling seals **user-chosen** labels
/// (`encryption-at-rest.md` § Carve-outs); these two are constants some writer
/// picked, identical across every deployment and every account, so sealing them
/// would protect nothing and cost every reader a key. `file-sync.md` § Sealed
/// names & paths names them in its *Deliberate non-seals* list.
///
/// The exact-match is deliberate — a user is free to name their laptop
/// `"fauna"`, and if they do, that label seals under their own root like any
/// other. What this predicate refuses is the *writer* class, and each of
/// those two writers passes its own frozen constant.
pub fn is_synthetic_device_label(label: &str) -> bool {
    matches!(label, WEBDAV_PSEUDO_DEVICE_LABEL | SELF_REGISTER_LABEL)
}

/// Seal one **user-chosen device label** — the single place a client turns a
/// device label into a [`path_crypto::SealedLabel`], the device-plane twin of
/// [`seal_set_name`].
///
/// ⚠ **The root is the registering owner's, always**
/// (`LabelRoot::owner_of(backup_key)`) — never
/// `SyncEngine::label_seal_root()`, which resolves a *folder's* M2 content-key
/// generation. A device belongs to the **actor**, not to any set: sealing under a
/// set's key would make one device's label readable by that set's co-members and
/// unreadable everywhere else, and — like every wrong-root mistake on this plane
/// — it would fail *silently*, as [`SealedLabelRender::Omit`] rather than an
/// error. The ratified shape is the registering owner's root, salted by the
/// `device_id`, with a random nonce (`file-sync.md` § Sealed names & paths).
///
/// **Random nonce, not convergent.** A label is mutable and the salt does not
/// determine it, so a derived nonce would reuse a (key, nonce) pair across two
/// labels of the same device (`file-sync.md` § Sealed names & paths names the
/// device label in the random-nonce list).
///
/// **Salt = the raw `device_id`**, which every reader of the label already holds
/// on the same wire row — so unlike the set-name and path planes, this one needs
/// no hash companion to stay openable once the plaintext scrubs.
///
/// `Ok(None)` means the label is machine-authored
/// ([`is_synthetic_device_label`]) and must not seal. Refusing here rather than
/// at each writer is the [`seal_set_name`] precedent and the same reasoning:
/// there are five writers across four crates — one nest-side — and "every caller
/// remembers to check" is not a guarantee.
pub fn seal_device_label(
    root: &path_crypto::LabelRoot,
    device_id: &[u8; 32],
    label: &str,
) -> anyhow::Result<Option<Vec<u8>>> {
    if is_synthetic_device_label(label) {
        return Ok(None);
    }
    let sealed =
        path_crypto::seal_random(root, device_id, LabelField::DeviceLabel, label.as_bytes())?;
    Ok(Some(sealed.to_bytes()?))
}

/// Render one **device label** sealed-first — the seam every surface showing a
/// device name must call.
///
/// Custody here is **owner-only** ([`FileDownloadKeys::owner`]), never
/// [`LabelCustody::keys_for`]: a device label has no folder to resolve custody
/// for, which is the read-side consequence of [`seal_device_label`]'s
/// actor-scoped root.
pub fn render_device_label(
    keys: &FileDownloadKeys,
    sealed: Option<&[u8]>,
    plaintext: &str,
    device_id: &[u8; 32],
) -> SealedLabelRender {
    path_crypto::render_sealed_label(
        keys,
        sealed,
        Some(plaintext),
        device_id,
        LabelField::DeviceLabel,
    )
}

/// Seal a **share link's filename** for its author's own list
/// (`share-links.md` § The filename rests sealed; the spec is
/// `path-sealing.md` § Sealed names & paths): the author's owner root, salted
/// by the raw 32-byte `token_id`, with a random nonce.
///
/// **Owner root, never a folder's.** The list is the author's alone — the same
/// actor-scoped reasoning as [`seal_device_label`]. **Salt = the token id**,
/// which every list row already carries, so no hash companion is needed once
/// the plaintext scrubs. **Random nonce**: the salt does not determine the
/// name — a convergent seal would be the wrong primitive, and the ruling
/// names this plane in the random-nonce list.
pub fn seal_share_filename(
    root: &path_crypto::LabelRoot,
    token_id: &[u8; 32],
    filename: &str,
) -> anyhow::Result<Vec<u8>> {
    path_crypto::seal_random(
        root,
        token_id,
        LabelField::ShareFilename,
        filename.as_bytes(),
    )?
    .to_bytes()
}

/// Render one share link's filename sealed-first — the seam every surface
/// listing a user's share links calls. Custody is owner-plane
/// ([`LabelCustody::owner_plane_read_keys`]): the seal is actor-scoped, as in
/// [`render_device_label`].
pub fn render_share_filename(
    keys: &FileDownloadKeys,
    sealed: &[u8],
    token_id: &[u8; 32],
) -> SealedLabelRender {
    path_crypto::render_sealed_label(
        keys,
        Some(sealed),
        None,
        token_id,
        LabelField::ShareFilename,
    )
}

/// The salt an **import-source** label opens under: the wire's `source_hash`
/// when present, [`path_crypto::import_source_hash`] of the plaintext
/// otherwise. Same degrade as [`set_name_label_salt`] — a malformed wire hash
/// falls back to the derivation rather than poisoning the render.
///
/// ⚠ The fallback is a **narrow degrade arm only**, not a safety net. This salt derives
/// from the descriptor, and the descriptor is exactly what the boot scrub
/// blanks once the seal rests, so a carrier that ships `source_sealed` without
/// `source_hash` renders correctly right up until the first reboot and then
/// turns every session into [`SealedLabelRender::Omit`] — the identical hole
/// [`render_set_name`] documents finding twice on the set-name plane.
pub fn import_source_label_salt(wire_hash: Option<&[u8]>, plaintext: &str) -> [u8; 32] {
    match wire_hash {
        Some(bytes) => <[u8; 32]>::try_from(bytes)
            .unwrap_or_else(|_| path_crypto::import_source_hash(plaintext)),
        None => path_crypto::import_source_hash(plaintext),
    }
}

/// Seal one **import session's source descriptor** — the user's external
/// mailbox identity ("provider + hostname + username, no password",
/// `mailbox-migration.md` § Progress lives nest-side), which without this rests
/// in plaintext for the row's 30-day life.
///
/// **Convergent, not random**, and the salt determines the plaintext: the
/// descriptor *is* its own [`path_crypto::import_source_hash`] pre-image, so
/// the (key, nonce) reuse [`seal_random`](path_crypto::seal_random) exists to
/// avoid cannot arise — two seals of one descriptor are the same bytes, which
/// also makes a retried `start_import_session` idempotent rather than a column
/// churn. The column's own declaration states this shape.
///
/// **Owner root, like the device label** — an import session is actor-scoped
/// and never shared with a roster, so there is no audience to widen and no
/// folder whose custody to resolve.
pub fn seal_import_source(
    root: &path_crypto::LabelRoot,
    source_descriptor: &str,
) -> anyhow::Result<Vec<u8>> {
    let salt = path_crypto::import_source_hash(source_descriptor);
    path_crypto::seal_convergent(
        root,
        &salt,
        LabelField::ImportSource,
        source_descriptor.as_bytes(),
    )?
    .to_bytes()
}

/// Render one **import source descriptor** sealed-first — the seam every
/// surface showing an import session's source must call (§ Resume protocol
/// step 1 lists the user's resumable sessions by this label).
///
/// Custody is **owner-only**, the [`render_device_label`] posture and for the
/// same reason: an import session has no folder to resolve custody for, which
/// is the read-side consequence of [`seal_import_source`]'s actor-scoped root.
///
/// `wire_hash` is the row's `source_hash` — see [`import_source_label_salt`]
/// for why omitting it is a post-reboot-only failure, i.e. the kind that
/// passes review.
pub fn render_import_source(
    keys: &FileDownloadKeys,
    sealed: Option<&[u8]>,
    plaintext: &str,
    wire_hash: Option<&[u8]>,
) -> SealedLabelRender {
    let salt = import_source_label_salt(wire_hash, plaintext);
    path_crypto::render_sealed_label(
        keys,
        sealed,
        Some(plaintext),
        &salt,
        LabelField::ImportSource,
    )
}

/// The salt both selective-sync path lists seal under: the set's own
/// `folders.id`, little-endian in the low 8 bytes and zero elsewhere.
///
/// **Why the row id and not `name_hash`.** The id is nest-local, discloses
/// nothing about the set, and rides every `FolderSummary` as a non-`Option`
/// field — so unlike the path and set-name planes this one needs **no hash
/// companion on the wire** to stay openable once the plaintext scrubs (the S2b /
/// S4 "carrier without its salt" hole cannot form here). It is the same choice
/// S5c-1 made when it re-keyed the `fauna.media.list` v2 cursor onto
/// `folders.id`.
///
/// **Why raw bytes and not a digest.** A salt needs to be *stable and
/// reproducible*, not uniform: [`path_crypto`] already domain-separates it by
/// mixing the field tag into both the key derivation and the AAD, and derives
/// the key in two BLAKE3 steps on top. Hashing here would buy nothing and would
/// mint a fifth KAT-pinned digest space — a data-migration surface, per
/// `file-sync.md` § Sealed names & paths. Raw is also what the device plane does
/// with its `device_id` salt.
///
/// ⚠ **`folders.id` is therefore load-bearing, not incidental: a migration that
/// RENUMBERS it orphans every include/exclude seal on the nest** — and it would
/// do it silently, as an omitted list rather than an error. The S9 flip's PK
/// rebuild is scoped to `snapshot_files` and `import_sessions`, neither of which
/// touches this column; anything that widens that scope owes a client-driven
/// re-seal, exactly as a companion-digest re-key would.
pub fn folder_paths_salt(folder_id: i64) -> [u8; 32] {
    let mut salt = [0u8; 32];
    salt[..8].copy_from_slice(&folder_id.to_le_bytes());
    salt
}

/// The on-the-wire encoding of a selective-sync path list: the same JSON string
/// array the nest stores in the plaintext `include_paths`/`exclude_paths`
/// columns, so the sealed and plaintext halves describe one value in one shape.
///
/// Owned here rather than at either end on purpose — the nest's `paths_json`
/// and a client's `Vec<String>` never have to agree with each other, only with
/// this function.
fn encode_path_list(paths: &[String]) -> String {
    // Infallible for `Vec<String>`; the `unwrap_or_default` arm would yield `""`,
    // which the render seam reads as a scrubbed column and degrades to `Omit` —
    // the safe direction if serde ever surprises us.
    serde_json::to_string(paths).unwrap_or_default()
}

/// Seal one **selective-sync path list** under the set owner's own root.
///
/// ⚠ **The root is the OWNER's, and this signature is why.** Every other
/// set-scoped funnel here takes a [`path_crypto::LabelRoot`], so the caller may
/// legitimately pass a bound set's M2 content-key generation — the root every
/// roster member holds. `include_paths`/`exclude_paths` is the one field in the
/// tightening set that is **owner-only rather than label-audience**: the nest
/// already withholds the plaintext from a roster member (*"they can leak the
/// owner's filesystem layout, and a member neither syncs the owner's folders nor
/// manages their config"* — `bins/fauna-nest/src/folder_handlers.rs`'s
/// `member_summary`), while it deliberately *ships* `name_sealed` to that same
/// member. Sealing this pair under a generation root would hand every member a
/// blob they can open, turning a sealing change into a disclosure widening — and
/// it would do it *silently*, the way every wrong-root mistake on this plane
/// does. Taking the [`BackupKey`] instead of a `LabelRoot` makes that
/// unrepresentable at the call site rather than merely warned against.
/// (`encryption-at-rest.md` § Carve-outs calls this field *"the sharpest: the
/// owner's absolute local filesystem layout"*.)
///
/// **Random nonce, not convergent.** The list is mutable and its salt (the row
/// id) does not determine it, so a derived nonce would reuse a (key, nonce) pair
/// across two path lists of the same set — `file-sync.md` § Sealed names & paths
/// names include/exclude in the random-nonce list.
fn seal_folder_paths(
    owner: &BackupKey,
    folder_id: i64,
    field: LabelField,
    paths: &[String],
) -> anyhow::Result<Vec<u8>> {
    let root = path_crypto::LabelRoot::owner_of(owner);
    let salt = folder_paths_salt(folder_id);
    path_crypto::seal_random(&root, &salt, field, encode_path_list(paths).as_bytes())?.to_bytes()
}

/// Seal a set's `include_paths` — see [`seal_folder_paths`] for the owner-root
/// and nonce reasoning.
///
/// The tag is hard-coded rather than taken as an argument for the same reason
/// the root is a [`BackupKey`]: a caller that passed
/// [`LabelField::FolderExcludePaths`] here would seal a blob that opens to the
/// *other* list, and it would fail as
/// [`path_crypto::SealedLabelRender::Omit`] rather than as an error.
pub fn seal_include_paths(
    owner: &BackupKey,
    folder_id: i64,
    paths: &[String],
) -> anyhow::Result<Vec<u8>> {
    seal_folder_paths(owner, folder_id, LabelField::FolderIncludePaths, paths)
}

/// Seal a set's `exclude_paths` — the [`seal_include_paths`] twin, under its own
/// [`LabelField::FolderExcludePaths`] tag.
pub fn seal_exclude_paths(
    owner: &BackupKey,
    folder_id: i64,
    paths: &[String],
) -> anyhow::Result<Vec<u8>> {
    seal_folder_paths(owner, folder_id, LabelField::FolderExcludePaths, paths)
}

/// Render one **selective-sync path list** sealed-first — the read seam every
/// surface that shows or applies a set's include/exclude patterns must call.
///
/// `None` is the ratified [`path_crypto::SealedLabelRender::Omit`] degrade,
/// widened to also cover a list that will not decode: for a *pattern list* the
/// consumer either has it or does not, and the distinction this collapses
/// (sealed vs. resting plaintext) is one no caller of the label seams branches
/// on. `Some(vec![])` stays meaningfully different — "no filters", the value the
/// wire's `Option<Vec<String>>` has always been able to express.
///
/// ⚠ Custody must be the reader's **owner** keys
/// ([`FileDownloadKeys::owner`] / [`LabelCustody::owner_key`]), never
/// [`LabelCustody::keys_for`]'s bound-set resolution — the read-side consequence
/// of [`seal_folder_paths`]'s owner-only root. A member's resolver-derived
/// custody cannot open these and must not be handed them in the first place.
fn render_folder_paths(
    keys: &FileDownloadKeys,
    sealed: Option<&[u8]>,
    plaintext: Option<&[String]>,
    folder_id: i64,
    field: LabelField,
) -> Option<Vec<String>> {
    let salt = folder_paths_salt(folder_id);
    let encoded = plaintext.map(encode_path_list);
    match path_crypto::render_sealed_label(keys, sealed, encoded.as_deref(), &salt, field) {
        SealedLabelRender::Sealed(json) | SealedLabelRender::Plaintext(json) => {
            serde_json::from_str(&json).ok()
        }
        SealedLabelRender::Omit => None,
    }
}

/// Render a set's `include_paths` sealed-first — see [`render_folder_paths`].
pub fn render_include_paths(
    keys: &FileDownloadKeys,
    sealed: Option<&[u8]>,
    plaintext: Option<&[String]>,
    folder_id: i64,
) -> Option<Vec<String>> {
    render_folder_paths(
        keys,
        sealed,
        plaintext,
        folder_id,
        LabelField::FolderIncludePaths,
    )
}

/// Render a set's `exclude_paths` sealed-first — the [`render_include_paths`]
/// twin.
pub fn render_exclude_paths(
    keys: &FileDownloadKeys,
    sealed: Option<&[u8]>,
    plaintext: Option<&[String]>,
    folder_id: i64,
) -> Option<Vec<String>> {
    render_folder_paths(
        keys,
        sealed,
        plaintext,
        folder_id,
        LabelField::FolderExcludePaths,
    )
}

// ── snapshots.tags — the display copy (path-sealing S6-d) ───────────────────

/// The on-the-wire encoding of a snapshot's tag list: the same JSON string array
/// the nest stores in the plaintext `snapshots.tags` column, so the sealed and
/// plaintext halves describe one value in one shape.
///
/// Owned here for the same reason [`encode_path_list`] is: the nest's `tags_json`
/// and a client's `Vec<String>` never have to agree with each other, only with
/// this function.
fn encode_tag_list(tags: &[String]) -> String {
    // Infallible for `Vec<String>`; the `unwrap_or_default` arm would yield `""`,
    // which the render seam reads as a scrubbed column and degrades to `Omit` —
    // the safe direction if serde ever surprises us.
    serde_json::to_string(tags).unwrap_or_default()
}

/// Seal a snapshot's **tag list** — the human-readable display copy of
/// `snapshots.tags` (path-sealing S6-d).
///
/// ⚠ **This is the DISPLAY copy only. The retention pruner is not a reader.**
/// Retention matching is already hash-to-hash on both sides — the snapshot's
/// `tag_hashes` (a positional array of [`path_crypto::snapshot_tag_hash`]
/// digests written nest-side at insert) against the policy's `keep_tags` hashed
/// through the same derivation — so nothing server-side ever opens this blob,
/// and a slice that "simplifies" the pruner onto it would move a server-side
/// equality test behind a key the server does not hold.
///
/// **The audience is the LABEL AUDIENCE, not the owner** — and this is the one
/// place S6-c's sibling funnel must *not* be copied, though they sit lines
/// apart. [`seal_include_paths`] takes a [`BackupKey`] precisely to make the
/// member-openable root unrepresentable; this one takes a
/// [`path_crypto::LabelRoot`] precisely so a bound set's M2 generation *can* be
/// passed. The difference is graded from where each field rests today, not from
/// taste: the nest already withholds the plaintext `include_paths` from a roster
/// member, while it ships the plaintext `tags` to that same member on every
/// `fauna.filesync.snapshot.get`. Sealing under the owner root here would take
/// tags away from a reader who has them today — a narrowing no goal doc
/// sanctions, arriving disguised as a hardening. The ruling removes exactly one
/// reader, the Q5 `AdminDiscovery` admin, and that is a *projection* question
/// (`folder_authz::FolderReadGrant::is_label_audience()`), not a root one.
///
/// **Salt = the set's `name_hash`** ([`path_crypto::set_name_hash`]), not the
/// snapshot's row id and not the set's row id. The snapshot id is nest-minted at
/// INSERT, so the create gesture — the only writer that will still hold a
/// plaintext tag after the flip — cannot know it. The set id is not on this
/// plane's wire at all: neither the create reply nor the get reply carries one,
/// while `name_hash` is already this set's wire address (S5a/S5b) and already
/// rides the sibling `SnapshotDiffReply.folder_hash`. No fifth digest shape is
/// minted (`encryption-at-rest.md` § Carve-outs, the settlement).
///
/// **Random nonce, not convergent.** The list is mutable and its salt (the set's
/// name digest) does not determine it — two snapshots of one set carry different
/// tags under the same salt — so a derived nonce would reuse a (key, nonce) pair
/// across differing plaintexts. `file-sync.md` § Sealed names & paths names
/// `snapshots.tags` in the random-nonce list.
pub fn seal_snapshot_tags(
    root: &path_crypto::LabelRoot,
    set_name: &str,
    tags: &[String],
) -> anyhow::Result<Vec<u8>> {
    // The writer always holds the plaintext name (it is the create gesture's own
    // first argument), so it takes the derive arm of `set_name_label_salt` — the
    // same function the reader salts from, rather than a second derivation that
    // could drift from it.
    let salt = set_name_label_salt(None, set_name);
    path_crypto::seal_random(
        root,
        &salt,
        LabelField::SnapshotTags,
        encode_tag_list(tags).as_bytes(),
    )?
    .to_bytes()
}

/// Render a snapshot's tag list sealed-first — the read seam every surface that
/// displays `snapshots.tags` must call.
///
/// `None` is the ratified [`path_crypto::SealedLabelRender::Omit`] degrade,
/// widened to also cover a list that will not decode, exactly as
/// [`render_folder_paths`] does: for a *tag list* the consumer either has it or
/// does not, and no caller branches on sealed-vs-resting-plaintext.
/// `Some(vec![])` stays meaningfully different — "no tags".
///
/// Custody here is [`LabelCustody::keys_for`]'s set-scoped resolution (the
/// audience keys), the read-side consequence of this funnel taking a
/// [`path_crypto::LabelRoot`] rather than a [`BackupKey`].
///
/// ⚠ **`wire_hash` is what makes this survive the flip, and it is not optional
/// in practice.** The salt is the *set name's* digest, so a reader that could
/// only derive it from `plaintext` would need the very column the flip scrubs —
/// the exact trap S2b hit on `fauna.media.list` (a plane carrying the seal but
/// not its salt is unrenderable the moment the plaintext goes). Pass the reply's
/// `folder_hash`; [`set_name_label_salt`] falls back to deriving from the
/// plaintext name only while it still rests.
pub fn render_snapshot_tags(
    keys: &FileDownloadKeys,
    sealed: Option<&[u8]>,
    plaintext: Option<&[String]>,
    set_name: &str,
    wire_hash: Option<&[u8]>,
) -> Option<Vec<String>> {
    let salt = set_name_label_salt(wire_hash, set_name);
    let encoded = plaintext.map(encode_tag_list);
    match path_crypto::render_sealed_label(
        keys,
        sealed,
        encoded.as_deref(),
        &salt,
        LabelField::SnapshotTags,
    ) {
        SealedLabelRender::Sealed(json) | SealedLabelRender::Plaintext(json) => {
            serde_json::from_str(&json).ok()
        }
        SealedLabelRender::Omit => None,
    }
}

/// Seal a folder's `retention_policy` JSON string (S6-e) — the write half of
/// `folders.retention_policy_sealed`.
///
/// **Takes a [`path_crypto::LabelRoot`], deliberately — the [`seal_snapshot_tags`]
/// shape, NOT [`seal_include_paths`]'s `BackupKey`.** The two neighbours on this
/// same table pull opposite ways and the difference is graded from who receives
/// the plaintext today: the nest withholds `include_paths` from a roster member
/// (`member_summary`, the owner's local filesystem layout is not a member's
/// business) but ships `retention_policy` to one **unmodified**. So a bound set's
/// M2 content-key generation is a legal root here, exactly as it is for the tag
/// list — sealing under the owner root instead would take retention away from
/// readers who hold it today, which is a *narrowing* and no more sanctioned than
/// a widening (path-sealing S6-d's lesson, in the direction that reads as
/// hardening).
///
/// Salt is the set name's digest, not the row `id`, and that is forced rather
/// than chosen: `retention_policy` is settable at **create**
/// (`FolderCreateRequest::retention_policy`) while the id is minted at INSERT,
/// so an id salt could never be sealed by the create gesture. Random nonce — the
/// salt is stable across an edit while the policy changes, so a derived nonce
/// would reuse a (key, nonce) pair across two policies of one set.
pub fn seal_retention_policy(
    root: &path_crypto::LabelRoot,
    set_name: &str,
    policy: &str,
) -> anyhow::Result<Vec<u8>> {
    // The writer always holds the plaintext name (it is the update request's own
    // WHERE key), so it takes the derive arm of `set_name_label_salt` — the same
    // function the reader salts from, rather than a second derivation that could
    // drift from it.
    let salt = set_name_label_salt(None, set_name);
    path_crypto::seal_random(
        root,
        &salt,
        LabelField::FolderRetentionPolicy,
        policy.as_bytes(),
    )?
    .to_bytes()
}

/// Render a folder's retention policy sealed-first — the read seam every
/// surface that displays `folders.retention_policy` must call.
///
/// `None` is the ratified [`path_crypto::SealedLabelRender::Omit`] degrade: a
/// consumer either has the policy or does not, and no caller branches on
/// sealed-vs-resting-plaintext.
///
/// ⚠ **`wire_hash` is what makes this survive the flip** — same reasoning as
/// [`render_snapshot_tags`]: the salt is the *set name's* digest, so a reader
/// deriving it from `set_name` alone needs the very column the flip scrubs. Pass
/// the summary row's `name_hash`, which rides both projection arms beside this
/// field precisely so the pair is never separated.
pub fn render_retention_policy(
    keys: &FileDownloadKeys,
    sealed: Option<&[u8]>,
    plaintext: Option<&str>,
    set_name: &str,
    wire_hash: Option<&[u8]>,
) -> Option<String> {
    let salt = set_name_label_salt(wire_hash, set_name);
    match path_crypto::render_sealed_label(
        keys,
        sealed,
        plaintext,
        &salt,
        LabelField::FolderRetentionPolicy,
    ) {
        SealedLabelRender::Sealed(p) | SealedLabelRender::Plaintext(p) => Some(p),
        SealedLabelRender::Omit => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::folder_keys::{
        ContentKeyGeneration, FolderContentKeys, ResolvedCustody, ResolvedFolderKeys,
    };
    use crate::secret::SecretArray32;

    fn owner_key() -> BackupKey {
        BackupKey::from_bytes([7u8; 32])
    }

    /// The whole point of sealing this surface: **the label survives the boot
    /// scrub**. Post-flip the nest blanks `source_descriptor` the moment the
    /// seal rests, so the render must work from the seal plus the wire's
    /// `source_hash` alone — with the plaintext already gone.
    ///
    /// Without this the seal is worse than useless: the graduated scrub arm
    /// fires, the descriptor is destroyed, and the resume UX shows a nameless
    /// session (`mailbox-migration.md` § Resume protocol step 1).
    #[test]
    fn an_import_source_renders_from_seal_and_wire_hash_after_the_plaintext_scrubs() {
        const SOURCE: &str = "gmail:imap.gmail.com:alice";
        let root = path_crypto::LabelRoot::owner_of(&owner_key());
        let sealed = seal_import_source(&root, SOURCE).unwrap();
        let wire_hash = path_crypto::import_source_hash(SOURCE);
        let keys = FileDownloadKeys::owner(owner_key());

        // The scrubbed row: plaintext is the `''` NOT NULL sentinel.
        let rendered = render_import_source(&keys, Some(&sealed), "", Some(&wire_hash));
        assert_eq!(
            rendered,
            SealedLabelRender::Sealed(SOURCE.to_string()),
            "a scrubbed row must still render its source from the seal"
        );

        // And the pre-scrub row renders the same text, sealed-first.
        assert_eq!(
            render_import_source(&keys, Some(&sealed), SOURCE, Some(&wire_hash))
                .text()
                .unwrap(),
            SOURCE
        );
    }

    /// ⚠ The regression this plane has already shipped twice on the set-name
    /// side: a carrier that ships `source_sealed` but forgets `source_hash`
    /// renders perfectly until the first reboot, then degrades every session to
    /// `Omit`. Pinned so a future wire change cannot drop the salt quietly.
    #[test]
    fn dropping_the_wire_hash_only_breaks_once_the_plaintext_is_gone() {
        const SOURCE: &str = "icloud:imap.mail.me.com:alice";
        let root = path_crypto::LabelRoot::owner_of(&owner_key());
        let sealed = seal_import_source(&root, SOURCE).unwrap();
        let keys = FileDownloadKeys::owner(owner_key());

        // Pre-scrub: the salt derives from the resting plaintext, so it works —
        // which is exactly why the omission passes review.
        assert_eq!(
            render_import_source(&keys, Some(&sealed), SOURCE, None).text(),
            Some(SOURCE)
        );
        // Post-scrub, with no wire hash to fall back on: unrenderable.
        assert_eq!(
            render_import_source(&keys, Some(&sealed), "", None),
            SealedLabelRender::Omit
        );
    }

    /// Convergent, per the column's declaration — so a retried
    /// `start_import_session` re-sends byte-identical bytes instead of churning
    /// the column, and two devices racing the same source agree.
    #[test]
    fn sealing_one_import_source_twice_is_byte_identical() {
        let root = path_crypto::LabelRoot::owner_of(&owner_key());
        assert_eq!(
            seal_import_source(&root, "gmail:imap.gmail.com:alice").unwrap(),
            seal_import_source(&root, "gmail:imap.gmail.com:alice").unwrap()
        );
    }

    /// A reader who is not the owner cannot open it — the disclosure this
    /// surface exists to close is the hosting admin who is not the data owner.
    #[test]
    fn another_readers_custody_cannot_open_an_import_source() {
        const SOURCE: &str = "gmail:imap.gmail.com:alice";
        let root = path_crypto::LabelRoot::owner_of(&owner_key());
        let sealed = seal_import_source(&root, SOURCE).unwrap();
        let stranger = FileDownloadKeys::owner(BackupKey::from_bytes([0x99u8; 32]));
        assert_eq!(
            render_import_source(
                &stranger,
                Some(&sealed),
                "",
                Some(&path_crypto::import_source_hash(SOURCE))
            ),
            SealedLabelRender::Omit
        );
    }

    struct StubResolver {
        set: &'static str,
        /// `None` = bound-but-unresolvable — the cell.
        keys: Option<FolderContentKeys>,
    }

    #[cfg_attr(not(target_arch = "wasm32"), async_trait::async_trait)]
    #[cfg_attr(target_arch = "wasm32", async_trait::async_trait(?Send))]
    impl FolderKeyResolver for StubResolver {
        async fn resolve(&self, name_hash: &[u8; 32]) -> anyhow::Result<ResolvedCustody> {
            Ok(if *name_hash == path_crypto::set_name_hash(self.set) {
                ResolvedCustody::ContentKeyed(ResolvedFolderKeys {
                    mls_group_id: Some(vec![9u8; 32]),
                    content_keys: self.keys.clone(),
                    home_nest_url: None,
                    home_nest_actor_id: None,
                })
            } else {
                ResolvedCustody::owner_only()
            })
        }
    }

    /// A scrubbed wire row — blank plaintext, the set's `name_hash` beside it —
    /// resolves the BOUND set's content keys through `keys_for_row`, while the
    /// plaintext-only `keys_for("")` lands on an unrelated hash and finds
    /// owner-only custody: the render-site hole leg 6b closes.
    #[tokio::test]
    async fn a_scrubbed_rows_wire_hash_resolves_the_bound_sets_keys() {
        let keys = content_keys(2, [0x33; 32]);
        let custody = LabelCustody::new(
            Some(Arc::new(StubResolver {
                set: "shared",
                keys: Some(keys),
            })),
            Some(owner_key()),
        );
        let hash = path_crypto::set_name_hash("shared");

        let (by_row, _) = custody.keys_for_row("", Some(&hash)).await;
        assert!(
            by_row.is_content_keyed(),
            "the row's hash finds the bound set"
        );
        assert_eq!(by_row.mls_group_id, Some(vec![9u8; 32]));

        let (by_blank, _) = custody.keys_for("").await;
        assert!(
            by_blank.mls_group_id.is_none(),
            "the blank plaintext alone names no set"
        );

        // No wire hash: the plaintext still addresses the set (a row on a plane
        // whose set name still rests plaintext).
        let (no_hash, _) = custody.keys_for_row("shared", None).await;
        assert!(no_hash.is_content_keyed());
    }

    /// A resolver answering the WebDAV-served, group-less shape for one set
    /// — content-keyed at the serve pseudo-channel, no `mls_group_id` — and
    /// the served-then-unflagged shape (owner-only with retired generations)
    /// for another.
    struct ServedStubResolver {
        served: &'static str,
        keys: Option<FolderContentKeys>,
        unflagged: &'static str,
        retired: Option<FolderContentKeys>,
    }

    #[cfg_attr(not(target_arch = "wasm32"), async_trait::async_trait)]
    #[cfg_attr(target_arch = "wasm32", async_trait::async_trait(?Send))]
    impl FolderKeyResolver for ServedStubResolver {
        async fn resolve(&self, name_hash: &[u8; 32]) -> anyhow::Result<ResolvedCustody> {
            Ok(if *name_hash == path_crypto::set_name_hash(self.served) {
                ResolvedCustody::ContentKeyed(ResolvedFolderKeys {
                    mls_group_id: None,
                    content_keys: self.keys.clone(),
                    home_nest_url: None,
                    home_nest_actor_id: None,
                })
            } else if *name_hash == path_crypto::set_name_hash(self.unflagged) {
                ResolvedCustody::OwnerOnly {
                    retired_content_keys: self.retired.clone(),
                }
            } else {
                ResolvedCustody::owner_only()
            })
        }
    }

    /// A served set's row is stamped with the generation the MDA sealed it
    /// under; the owner's Media custody opens it under the content keys the
    /// resolver produced for the serve pseudo-channel — no group id in sight,
    /// and the owner root never consulted for the chunk side.
    #[tokio::test]
    async fn a_served_unshared_set_renders_under_its_content_keys_without_a_group() {
        let keys = content_keys(3, [0x51; 32]);
        let custody = LabelCustody::new(
            Some(Arc::new(ServedStubResolver {
                served: "photos",
                keys: Some(keys.clone()),
                unflagged: "docs",
                retired: None,
            })),
            Some(owner_key()),
        );
        let (dl, home) = custody.keys_for("photos").await;
        assert!(home.is_none());
        assert!(dl.mls_group_id.is_none(), "a served set fakes no group id");
        assert!(dl.served, "the served marker rides instead");
        assert!(dl.is_content_keyed());

        let root = crate::path_crypto::LabelRoot::content_key(*keys.current_key(), 3);
        let sealed = crate::path_crypto::seal_convergent(
            &root,
            &crate::sync::path_hash("a/b.jpg"),
            LabelField::SyncChangePath,
            b"a/b.jpg",
        )
        .unwrap()
        .to_bytes()
        .unwrap();
        let hash = crate::sync::path_hash("a/b.jpg");
        assert_eq!(
            render_path(
                &dl,
                Some(&sealed),
                "",
                Some(&hash),
                LabelField::SyncChangePath
            ),
            SealedLabelRender::Sealed("a/b.jpg".into()),
        );
        // And the seal side stamps under the served set's current generation —
        // the root the MDA holds — never the owner root.
        assert!(matches!(
            dl.label_seal_root().unwrap(),
            Some(root) if root.generation() == Some(3)
        ));
    }

    /// Served with the serve custody not (yet) on the account plane — the serve-enable
    /// write racing this device, or an unsynced store — fails CLOSED exactly
    /// as a bound-but-keyless set does: no owner-root seal, no owner-key chunk
    /// read. (`EngineKeyBinding::ServedKeysMissing`'s read-side twin.)
    #[tokio::test]
    async fn a_served_set_with_missing_custody_fails_closed_like_a_bound_one() {
        let custody = LabelCustody::new(
            Some(Arc::new(ServedStubResolver {
                served: "photos",
                keys: None,
                unflagged: "docs",
                retired: None,
            })),
            Some(owner_key()),
        );
        let (dl, _) = custody.keys_for("photos").await;
        assert!(dl.served && dl.content_keys.is_none());
        assert!(
            dl.label_seal_root().is_err(),
            "served-but-keyless must refuse to mint a seal root — never the owner arm"
        );
        assert!(
            seal_path_from_keys(&dl, "a/b.jpg").is_err(),
            "and every seal site sees that refusal"
        );
    }

    /// A served-then-unflagged set is owner-only again, but the names the
    /// served window sealed under the rotated-out generation must still
    /// render — through the retired candidates the resolver hands back on the
    /// owner-only arm, beside the owner root, never as a seal root.
    #[tokio::test]
    async fn an_unflagged_sets_served_era_names_still_render_via_retired_generations() {
        let retired = content_keys(2, [0x62; 32]);
        let custody = LabelCustody::new(
            Some(Arc::new(ServedStubResolver {
                served: "photos",
                keys: None,
                unflagged: "docs",
                retired: Some(retired.clone()),
            })),
            Some(owner_key()),
        );
        let (dl, _) = custody.keys_for("docs").await;
        assert!(!dl.is_content_keyed(), "unflagged ⇒ owner-only again");
        assert!(dl.backup_key.is_some());

        let root = crate::path_crypto::LabelRoot::content_key(*retired.current_key(), 2);
        let sealed = crate::path_crypto::seal_convergent(
            &root,
            &crate::sync::path_hash("served/era.txt"),
            LabelField::SyncChangePath,
            b"served/era.txt",
        )
        .unwrap()
        .to_bytes()
        .unwrap();
        let hash = crate::sync::path_hash("served/era.txt");
        assert_eq!(
            render_path(
                &dl,
                Some(&sealed),
                "",
                Some(&hash),
                LabelField::SyncChangePath
            ),
            SealedLabelRender::Sealed("served/era.txt".into()),
        );
        // New seals go under the OWNER root — the retired generation is a read
        // candidate only.
        assert!(matches!(
            dl.label_seal_root().unwrap(),
            Some(root) if root.generation().is_none()
        ));
    }

    fn content_keys(version: u64, key: [u8; 32]) -> FolderContentKeys {
        FolderContentKeys {
            current: ContentKeyGeneration {
                version,
                key: SecretArray32::from(key),
                rotated_at: 1,
            },
            prior: Vec::new(),
        }
    }

    #[tokio::test]
    async fn keyless_custody_resolves_to_no_roots() {
        let custody = LabelCustody::default();
        let (keys, home) = custody.keys_for("photos").await;
        assert!(home.is_none());
        assert!(
            keys.label_open_roots(None).unwrap().is_empty(),
            "a keyless reader holds nothing that could open a label"
        );
    }

    #[tokio::test]
    async fn owner_only_custody_offers_the_owner_root() {
        let custody = LabelCustody::owner_only(owner_key());
        let (keys, _) = custody.keys_for("photos").await;
        assert_eq!(
            keys.label_open_roots(None).unwrap(),
            vec![owner_key().convergent_chunk_root()]
        );
    }

    /// The structural fix, pinned at the assembly seam: a
    /// bound-but-unresolvable resolve (`content_keys: None`) keeps its
    /// `mls_group_id` through `keys_for`, so `label_seal_root()`'s fail-closed
    /// bail is REACHABLE — the owner key rides for the *open* direction, but a
    /// *seal* refuses rather than stamping the owner root. Before the fix the
    /// resolver contract forced this cell to the owner fallback and the bail
    /// was dead code on exactly the path it was written for.
    #[tokio::test]
    async fn a_bound_unresolvable_resolve_makes_the_seal_bail_reachable() {
        let custody = LabelCustody::new(
            Some(Arc::new(StubResolver {
                set: "shared",
                keys: None,
            })),
            Some(owner_key()),
        );
        let (keys, _) = custody.keys_for("shared").await;

        assert!(
            keys.mls_group_id.is_some(),
            "bound-ness must survive the assembly"
        );
        assert!(
            keys.label_seal_root().is_err(),
            "a bound set with no content keys must refuse to mint a seal root \
             — never fall to the owner arm"
        );
    }

    struct FailingResolver;

    #[cfg_attr(not(target_arch = "wasm32"), async_trait::async_trait)]
    #[cfg_attr(target_arch = "wasm32", async_trait::async_trait(?Send))]
    impl FolderKeyResolver for FailingResolver {
        async fn resolve(&self, _name_hash: &[u8; 32]) -> anyhow::Result<ResolvedCustody> {
            anyhow::bail!("roster read failed")
        }
    }

    /// A resolve FAILURE (transport, corrupt state) yields no keys at all —
    /// never the owner fallback. "Could not determine" must not read as
    /// "unbound": a seal site would stamp the owner root for a possibly-bound
    /// set. With no keys, a seal records plaintext-only (the ratified degrade)
    /// and a render omits for the pass.
    #[tokio::test]
    async fn a_failed_resolve_yields_no_keys_not_the_owner_fallback() {
        let custody = LabelCustody::new(Some(Arc::new(FailingResolver)), Some(owner_key()));
        let (keys, home) = custody.keys_for("photos").await;

        assert!(home.is_none());
        assert!(keys.mls_group_id.is_none());
        assert!(
            keys.label_seal_root().unwrap().is_none(),
            "no seal root can be minted while the custody answer is unknown"
        );
        assert!(
            keys.label_open_roots(None).unwrap().is_empty(),
            "and no open root either — the owner key must not ride a failed resolve"
        );
    }

    /// The load-bearing half of decision (1): a resolver hit must NOT drop the
    /// owner key, or a set's owner loses the names it sealed before binding.
    #[tokio::test]
    async fn a_bound_set_keeps_the_owner_key_for_the_unstamped_arm() {
        let custody = LabelCustody::new(
            Some(Arc::new(StubResolver {
                set: "shared",
                keys: Some(content_keys(3, [4u8; 32])),
            })),
            Some(owner_key()),
        );
        let (keys, _) = custody.keys_for("shared").await;

        // A pre-binding label (`gen: None`) still opens under the owner root...
        assert_eq!(
            keys.label_open_roots(None).unwrap(),
            vec![owner_key().convergent_chunk_root()],
            "the owner must still render names sealed before the set was bound"
        );
        // ...while a post-binding label resolves to the content key alone, so
        // the extra owner candidate is unreachable there (never a downgrade).
        assert_eq!(keys.label_open_roots(Some(3)).unwrap(), vec![[4u8; 32]]);
        // A generation this holder lacks fails closed rather than falling back.
        assert!(keys.label_open_roots(Some(9)).is_err());
    }

    #[tokio::test]
    async fn an_unresolved_set_falls_back_to_owner_only() {
        let custody = LabelCustody::new(
            Some(Arc::new(StubResolver {
                set: "shared",
                keys: Some(content_keys(1, [4u8; 32])),
            })),
            Some(owner_key()),
        );
        let (keys, home) = custody.keys_for("not-that-one").await;
        assert!(home.is_none());
        assert_eq!(
            keys.label_open_roots(None).unwrap(),
            vec![owner_key().convergent_chunk_root()]
        );
    }

    /// Decision (2): the wire hash IS the salt — the half that outlives the
    /// plaintext scrub — and a row with no wire hash has no salt at all. The
    /// plaintext-derived salt for that row was the expand-era fallback; a
    /// carrier ships the pair together or withholds both, so deriving one here
    /// would only ever open a seal the wire deliberately withheld the salt of.
    #[test]
    fn the_wire_hash_is_the_salt_and_no_wire_hash_is_no_salt() {
        let wire = [42u8; 32];
        assert_eq!(path_label_salt(Some(&wire), "a/b.txt"), Some(wire));
        assert_eq!(path_label_salt(None, "a/b.txt"), None);
    }

    /// The one derivation kept — the fail-soft for a MALFORMED wire hash, the
    /// twin of the set-name and import-source degrades: it must not make the
    /// row unrenderable while its plaintext rests (a `public`-audience folder),
    /// and where the plaintext is scrubbed it opens nothing, which is what a
    /// bad hash deserves.
    #[test]
    fn a_malformed_wire_hash_falls_back_to_the_derivation() {
        assert_eq!(
            path_label_salt(Some(&[1u8, 2, 3]), "a/b.txt"),
            Some(crate::sync::path_hash("a/b.txt"))
        );
    }

    /// A seal with no wire salt is never attempted: the row renders from its
    /// resting plaintext (a `public`-audience folder's row, whose seal is a
    /// lingering over-seal) or omits when that is scrubbed — never `Sealed`,
    /// and never a plaintext-derived open of a seal the wire withheld the salt
    /// of.
    #[test]
    fn render_path_without_a_wire_hash_never_attempts_the_seal() {
        let owner = owner_key();
        let plaintext = "photos/2026/a.jpg";
        let salt = crate::sync::path_hash(plaintext);
        let sealed = path_crypto::seal_convergent(
            &path_crypto::LabelRoot::owner_of(&owner),
            &salt,
            LabelField::SyncChangePath,
            plaintext.as_bytes(),
        )
        .unwrap()
        .to_bytes()
        .unwrap();
        let keys = FileDownloadKeys::owner(owner);

        assert_eq!(
            render_path(
                &keys,
                Some(&sealed),
                plaintext,
                None,
                LabelField::SyncChangePath
            ),
            SealedLabelRender::Plaintext(plaintext.to_string()),
            "the holder could open it — but the wire sent no salt, so it must not try"
        );
        assert_eq!(
            render_path(&keys, Some(&sealed), "", None, LabelField::SyncChangePath),
            SealedLabelRender::Omit
        );
        assert_eq!(
            render_path(&keys, None, plaintext, None, LabelField::SyncChangePath),
            SealedLabelRender::Plaintext(plaintext.to_string()),
            "the ordinary plaintext-plane row: no seal, no hash, the path rides in `path`"
        );
    }

    #[test]
    fn render_path_prefers_the_seal_and_omits_when_it_cannot_open() {
        let owner = owner_key();
        let plaintext = "vacation/beach.jpg";
        let salt = crate::sync::path_hash(plaintext);
        let sealed = path_crypto::seal_convergent(
            &path_crypto::LabelRoot::owner_of(&owner),
            &salt,
            LabelField::SyncChangePath,
            plaintext.as_bytes(),
        )
        .unwrap()
        .to_bytes()
        .unwrap();

        // The holder renders from the seal even with the plaintext blanked —
        // the post-flip shape.
        let keys = FileDownloadKeys::owner(owner);
        assert_eq!(
            render_path(
                &keys,
                Some(&sealed),
                "",
                Some(&salt),
                LabelField::SyncChangePath
            ),
            SealedLabelRender::Sealed(plaintext.to_string())
        );

        // A keyless reader with the plaintext scrubbed omits the row.
        assert_eq!(
            render_path(
                &FileDownloadKeys::default(),
                Some(&sealed),
                "",
                Some(&salt),
                LabelField::SyncChangePath
            ),
            SealedLabelRender::Omit
        );

        // ...and while the plaintext still rests, that same reader keeps it.
        assert_eq!(
            render_path(
                &FileDownloadKeys::default(),
                Some(&sealed),
                plaintext,
                Some(&salt),
                LabelField::SyncChangePath
            ),
            SealedLabelRender::Plaintext(plaintext.to_string())
        );
    }

    /// A seal that OPENS but names another row's path — P sealed
    /// under Q's hash by a writer holding the label key — renders as if it had
    /// not opened: the nest-hashed resting plaintext, else `Omit`. Never P.
    #[test]
    fn render_path_refuses_a_seal_that_opens_to_another_rows_path() {
        let owner = owner_key();
        let row = "shared/q.txt";
        let salt = crate::sync::path_hash(row);
        let forged = path_crypto::seal_convergent(
            &path_crypto::LabelRoot::owner_of(&owner),
            &salt,
            LabelField::SyncChangePath,
            b"shared/p.txt",
        )
        .unwrap()
        .to_bytes()
        .unwrap();
        let keys = FileDownloadKeys::owner(owner);

        assert_eq!(
            render_path(
                &keys,
                Some(&forged),
                "",
                Some(&salt),
                LabelField::SyncChangePath
            ),
            SealedLabelRender::Omit,
            "post-flip: nothing bound to render"
        );
        assert_eq!(
            render_path(
                &keys,
                Some(&forged),
                row,
                Some(&salt),
                LabelField::SyncChangePath
            ),
            SealedLabelRender::Plaintext(row.to_string()),
            "a resting plaintext (nest-hashed) is the row's own name"
        );
    }

    // ── set names (S5) ───────────────────────────────────────────────────────

    /// Decision (2) again, for the set-name plane: the wire's `name_hash` wins,
    /// because it is the half that outlives the plaintext scrub.
    #[test]
    fn the_wire_name_hash_is_preferred_over_the_plaintext_derivation() {
        let wire = [42u8; 32];
        assert_eq!(set_name_label_salt(Some(&wire), "Family photos"), wire);
        assert_eq!(
            set_name_label_salt(None, "Family photos"),
            path_crypto::set_name_hash("Family photos")
        );
    }

    #[test]
    fn a_malformed_wire_name_hash_falls_back_to_the_derivation() {
        assert_eq!(
            set_name_label_salt(Some(&[1u8, 2, 3]), "Family photos"),
            path_crypto::set_name_hash("Family photos")
        );
    }

    /// The seal funnel and the render seam must agree by construction — sealing
    /// with `seal_set_name` and rendering with `render_set_name` round-trips
    /// with the plaintext blanked (the post-flip shape).
    #[test]
    fn seal_set_name_round_trips_through_render_set_name_with_no_plaintext() {
        let owner = owner_key();
        let name = "Family photos";
        let sealed = seal_set_name(&path_crypto::LabelRoot::owner_of(&owner), name)
            .expect("a user-chosen name seals")
            .expect("a user-chosen name is not reserved");
        let salt = path_crypto::set_name_hash(name);

        assert_eq!(
            render_set_name(
                &FileDownloadKeys::owner(owner),
                Some(&sealed),
                "",
                Some(&salt)
            ),
            SealedLabelRender::Sealed(name.to_string())
        );
    }

    /// A reader who cannot open the seal must not see the name — and must not
    /// see an empty one either.
    #[test]
    fn render_set_name_omits_for_a_wrong_root_once_the_plaintext_is_scrubbed() {
        let name = "Family photos";
        let sealed = seal_set_name(
            &path_crypto::LabelRoot::owner_of(&BackupKey::from_bytes([1u8; 32])),
            name,
        )
        .unwrap()
        .unwrap();
        let salt = path_crypto::set_name_hash(name);

        // Wrong owner root, plaintext scrubbed ⇒ omit.
        assert_eq!(
            render_set_name(
                &FileDownloadKeys::owner(BackupKey::from_bytes([2u8; 32])),
                Some(&sealed),
                "",
                Some(&salt)
            ),
            SealedLabelRender::Omit
        );
        // ...but during expand the resting plaintext still lists.
        assert_eq!(
            render_set_name(
                &FileDownloadKeys::owner(BackupKey::from_bytes([2u8; 32])),
                Some(&sealed),
                name,
                Some(&salt)
            ),
            SealedLabelRender::Plaintext(name.to_string())
        );
    }

    /// The salt is load-bearing: a sealed name whose `name_hash` never rode the
    /// wire is unrenderable once the plaintext scrubs. This is the S2b/S4 hole
    /// (`fauna.media.list`, `WebdavFile`) pinned for the set-name plane, so a
    /// future surface cannot ship `name_sealed` without `name_hash` and pass.
    #[test]
    fn a_sealed_name_without_its_salt_on_the_wire_is_unrenderable_after_the_scrub() {
        let owner = owner_key();
        let name = "Family photos";
        let sealed = seal_set_name(&path_crypto::LabelRoot::owner_of(&owner), name)
            .unwrap()
            .unwrap();

        // No wire hash and no plaintext to derive one from: the right root is
        // held and it still cannot open.
        assert_eq!(
            render_set_name(
                &FileDownloadKeys::owner(owner.clone()),
                Some(&sealed),
                "",
                None
            ),
            SealedLabelRender::Omit
        );
        // The same reader with the salt on the wire renders fine — so the
        // failure above is the missing salt, not the missing key.
        assert_eq!(
            render_set_name(
                &FileDownloadKeys::owner(owner),
                Some(&sealed),
                "",
                Some(&path_crypto::set_name_hash(name))
            ),
            SealedLabelRender::Sealed(name.to_string())
        );
    }

    /// Reserved `__` names are routing constants at 17 nest decision sites and
    /// **never** seal (`encryption-at-rest.md` § Carve-outs). The funnel refuses
    /// them by construction, so no caller can seal one by forgetting to check.
    #[test]
    fn seal_set_name_refuses_reserved_names_by_construction() {
        let root = path_crypto::LabelRoot::owner_of(&owner_key());
        for reserved in ["__config", "__mail", "__conv/abcd", "__mls"] {
            assert_eq!(
                seal_set_name(&root, reserved).unwrap(),
                None,
                "{reserved} is a routing constant and must never seal"
            );
        }
        assert!(
            seal_set_name(&root, "__ not actually reserved is still prefixed")
                .unwrap()
                .is_none(),
            "the prefix predicate is the single source of truth — not a name whitelist"
        );
    }

    /// A set name seals **convergent**: the same (root, name) reproduces
    /// byte-identical ciphertext, so an idempotent re-stamp does not churn the
    /// column and two devices agree. Asserted by recomputing the derivation
    /// rather than pinning a golden blob (a golden blob passes just as happily
    /// when the writer reaches for the wrong root).
    #[test]
    fn seal_set_name_is_convergent_and_field_tagged() {
        let root = path_crypto::LabelRoot::owner_of(&owner_key());
        let a = seal_set_name(&root, "Family photos").unwrap().unwrap();
        let b = seal_set_name(&root, "Family photos").unwrap().unwrap();
        assert_eq!(
            a, b,
            "convergent seal must be a pure function of (root, name)"
        );

        let expected = path_crypto::seal_convergent(
            &root,
            &path_crypto::set_name_hash("Family photos"),
            LabelField::FolderName,
            "Family photos".as_bytes(),
        )
        .unwrap()
        .to_bytes()
        .unwrap();
        assert_eq!(
            a, expected,
            "the funnel must use LabelField::FolderName and the name_hash salt"
        );

        // A different root yields a different blob — the seal is bound to it.
        let other = path_crypto::LabelRoot::owner_of(&BackupKey::from_bytes([9u8; 32]));
        assert_ne!(a, seal_set_name(&other, "Family photos").unwrap().unwrap());
    }

    // ── conflicts (S6-a) ─────────────────────────────────────────────────────

    /// **The load-bearing property of the whole path plane.** A conflict's
    /// `path_sealed` must be byte-identical to the same path sealed by any
    /// other path writer under the same root — because the nest copies
    /// `path_sealed` *verbatim* between tables and holds no key to re-seal
    /// with (`db/sync_storage.rs` `INSERT INTO snapshot_files … f.path_sealed`).
    /// A per-table field tag would make every copied blob fail to open, and it
    /// would fail *silently* (`Omit`, never an error).
    #[test]
    fn a_conflict_path_seals_identically_to_every_other_path_writer() {
        let root = path_crypto::LabelRoot::owner_of(&owner_key());
        let path = "docs/eviction_notice.pdf";

        let via_helper = seal_path(&root, path).unwrap();
        let via_plane = path_crypto::seal_convergent(
            &root,
            &crate::sync::path_hash(path),
            LabelField::SyncChangePath,
            path.as_bytes(),
        )
        .unwrap()
        .to_bytes()
        .unwrap();

        assert_eq!(
            via_helper, via_plane,
            "a conflict path must seal under the path plane's single tag \
             (SyncChangePath) and the path_hash salt — the nest copies these \
             blobs between tables without a key"
        );
    }

    /// The writer and the already-built reader
    /// (`DevicesMachine::render_conflicts`) must agree by construction, with
    /// the plaintext blanked — the post-flip shape.
    #[test]
    fn seal_path_round_trips_through_render_path_with_no_plaintext() {
        let owner = owner_key();
        let path = "docs/eviction_notice.pdf";
        let sealed = seal_path(&path_crypto::LabelRoot::owner_of(&owner), path).unwrap();
        let salt = crate::sync::path_hash(path);

        assert_eq!(
            render_path(
                &FileDownloadKeys::owner(owner),
                Some(&sealed),
                "",
                Some(&salt),
                LabelField::SyncChangePath
            ),
            SealedLabelRender::Sealed(path.to_string())
        );
    }

    /// `details` is free text a user can edit, so its nonce is **random**, not
    /// derived — a convergent nonce there would reuse a (key, nonce) pair
    /// across two different descriptions of the same conflicting path
    /// (`file-sync.md` § Sealed names & paths names conflict `details` in the
    /// random-nonce list explicitly).
    #[test]
    fn conflict_details_seals_under_a_random_nonce_and_still_round_trips() {
        let owner = owner_key();
        let root = path_crypto::LabelRoot::owner_of(&owner);
        let path = "docs/eviction_notice.pdf";
        let details = "both devices wrote while offline";

        let a = seal_conflict_details(&root, path, details).unwrap();
        let b = seal_conflict_details(&root, path, details).unwrap();
        assert_ne!(
            a, b,
            "a mutable field must take a fresh random nonce per seal, so two \
             seals of identical plaintext must differ"
        );

        let keys = FileDownloadKeys::owner(owner);
        let salt = crate::sync::path_hash(path);
        for blob in [&a, &b] {
            assert_eq!(
                render_conflict_details(&keys, Some(blob), "", Some(&salt)),
                SealedLabelRender::Sealed(details.to_string())
            );
        }
    }

    /// The two conflict fields share one salt (the row's `path_hash`), which is
    /// only sound because the field tag is mixed into **both** the key
    /// derivation and the AAD. Pin it: neither blob opens as the other.
    #[test]
    fn the_two_conflict_fields_do_not_open_each_others_blob() {
        let owner = owner_key();
        let root = path_crypto::LabelRoot::owner_of(&owner);
        let keys = FileDownloadKeys::owner(owner);
        let path = "docs/eviction_notice.pdf";
        let salt = crate::sync::path_hash(path);

        let path_blob = seal_path(&root, path).unwrap();
        let details_blob = seal_conflict_details(&root, path, "who wrote last?").unwrap();

        assert_eq!(
            render_conflict_details(&keys, Some(&path_blob), "", Some(&salt)),
            SealedLabelRender::Omit,
            "a path blob must not open under the details field tag"
        );
        assert_eq!(
            render_path(
                &keys,
                Some(&details_blob),
                "",
                Some(&salt),
                LabelField::SyncChangePath
            ),
            SealedLabelRender::Omit,
            "a details blob must not open under the path field tag"
        );
    }

    /// A reader who can open neither half must not see the text — and must not
    /// see an empty string either.
    #[test]
    fn render_conflict_details_degrades_the_same_way_every_other_seam_does() {
        let details = "both devices wrote while offline";
        let path = "docs/eviction_notice.pdf";
        let salt = crate::sync::path_hash(path);
        let sealed = seal_conflict_details(
            &path_crypto::LabelRoot::owner_of(&BackupKey::from_bytes([1u8; 32])),
            path,
            details,
        )
        .unwrap();

        // Wrong root, plaintext scrubbed → omit.
        assert_eq!(
            render_conflict_details(
                &FileDownloadKeys::owner(owner_key()),
                Some(&sealed),
                "",
                Some(&salt)
            ),
            SealedLabelRender::Omit
        );
        // ...but while the plaintext still rests, that reader keeps it.
        assert_eq!(
            render_conflict_details(
                &FileDownloadKeys::owner(owner_key()),
                Some(&sealed),
                details,
                Some(&salt)
            ),
            SealedLabelRender::Plaintext(details.to_string())
        );
    }

    // ── device labels (S6-b) ────────────────────────────────────────────────

    fn owner_root() -> path_crypto::LabelRoot {
        path_crypto::LabelRoot::owner_of(&owner_key())
    }

    #[test]
    fn a_device_label_round_trips_under_the_owner_root_salted_by_its_device_id() {
        let device_id = [3u8; 32];
        let sealed = seal_device_label(&owner_root(), &device_id, "Work laptop")
            .unwrap()
            .expect("a user-chosen label seals");

        // The owner renders it — and does so with the plaintext already gone,
        // which is the post-flip shape: the salt is the device id, carried on
        // the same wire row, so this plane needs no hash companion.
        assert_eq!(
            render_device_label(
                &FileDownloadKeys::owner(owner_key()),
                Some(&sealed),
                "",
                &device_id
            ),
            SealedLabelRender::Sealed("Work laptop".to_string())
        );
    }

    // ── share-link filenames ───────────────────────────────────────────────

    #[test]
    fn a_share_filename_renders_from_its_seal_alone() {
        let token_id = [9u8; 32];
        let sealed = seal_share_filename(&owner_root(), &token_id, "holiday.jpg").unwrap();
        let keys = FileDownloadKeys::owner(owner_key());
        assert_eq!(
            render_share_filename(&keys, &sealed, &token_id),
            SealedLabelRender::Sealed("holiday.jpg".to_string())
        );
        // Bound to its token: a seal replayed onto another link's row omits.
        assert_eq!(
            render_share_filename(&keys, &sealed, &[1u8; 32]),
            SealedLabelRender::Omit
        );
        // Nothing that is not a seal renders.
        assert_eq!(
            render_share_filename(&keys, b"", &token_id),
            SealedLabelRender::Omit
        );
        // Random nonce: the same name under the same token seals differently.
        assert_ne!(
            sealed,
            seal_share_filename(&owner_root(), &token_id, "holiday.jpg").unwrap()
        );
    }

    #[test]
    fn each_synthetic_label_is_refused_centrally() {
        // The refusal is the funnel's job, not the five writers' — a nest-side
        // writer never reaches a client helper at all.
        for synthetic in [WEBDAV_PSEUDO_DEVICE_LABEL, SELF_REGISTER_LABEL] {
            assert!(is_synthetic_device_label(synthetic));
            assert_eq!(
                seal_device_label(&owner_root(), &[4u8; 32], synthetic).unwrap(),
                None,
                "{synthetic} is machine-authored and must not seal"
            );
        }
        // The retired backup coordinator's label is an ordinary user label now.
        assert!(!is_synthetic_device_label("fauna-backup-coordinator"));
        // A user-chosen label that merely *contains* one is still user-chosen.
        assert!(!is_synthetic_device_label("fauna phone"));
        assert!(
            seal_device_label(&owner_root(), &[4u8; 32], "fauna phone")
                .unwrap()
                .is_some()
        );
    }

    #[test]
    fn a_wrong_root_omits_rather_than_erroring_and_a_wrong_device_id_does_too() {
        let device_id = [5u8; 32];
        let sealed = seal_device_label(&owner_root(), &device_id, "Phone")
            .unwrap()
            .unwrap();

        // Sealed under a *different* owner — the silent degrade this plane's
        // whole trap class turns on. Plaintext scrubbed so the fallback cannot
        // mask it.
        let stranger = FileDownloadKeys::owner(BackupKey::from_bytes([8u8; 32]));
        assert_eq!(
            render_device_label(&stranger, Some(&sealed), "", &device_id),
            SealedLabelRender::Omit
        );
        // Right key, wrong salt: the AAD binds the device id, so a blob cannot
        // be replayed onto another device's row.
        assert_eq!(
            render_device_label(
                &FileDownloadKeys::owner(owner_key()),
                Some(&sealed),
                "",
                &[6u8; 32]
            ),
            SealedLabelRender::Omit
        );
    }

    #[test]
    fn two_devices_sharing_a_label_seal_to_distinct_blobs() {
        // Random nonce, and the salt differs too — so neither the ciphertext
        // nor a repeat seal of the same device leaks label equality.
        let a = seal_device_label(&owner_root(), &[1u8; 32], "laptop")
            .unwrap()
            .unwrap();
        let b = seal_device_label(&owner_root(), &[2u8; 32], "laptop")
            .unwrap()
            .unwrap();
        let a_again = seal_device_label(&owner_root(), &[1u8; 32], "laptop")
            .unwrap()
            .unwrap();
        assert_ne!(a, b);
        assert_ne!(a, a_again, "the nonce is random, not derived from the salt");
    }

    #[test]
    fn a_keyless_reader_keeps_the_resting_plaintext_and_omits_once_it_scrubs() {
        let device_id = [7u8; 32];
        let sealed = seal_device_label(&owner_root(), &device_id, "Desk PC")
            .unwrap()
            .unwrap();
        let keyless = FileDownloadKeys::default();

        assert_eq!(
            render_device_label(&keyless, Some(&sealed), "Desk PC", &device_id),
            SealedLabelRender::Plaintext("Desk PC".to_string())
        );
        assert_eq!(
            render_device_label(&keyless, Some(&sealed), "", &device_id),
            SealedLabelRender::Omit
        );
    }

    // ── selective-sync path lists (S6-c) ────────────────────────────────────

    fn includes() -> Vec<String> {
        vec!["/home/me/Documents".to_string(), "/home/me/tax".to_string()]
    }

    #[test]
    fn a_path_list_round_trips_under_the_owner_root_salted_by_the_row_id() {
        let sealed = seal_include_paths(&owner_key(), 42, &includes()).unwrap();

        // Rendered with the plaintext already gone — the post-flip shape. The
        // salt is the row id, which rides every `FolderSummary` as a
        // non-`Option` field, so this plane needs no hash companion on the wire.
        assert_eq!(
            render_include_paths(
                &FileDownloadKeys::owner(owner_key()),
                Some(&sealed),
                None,
                42
            ),
            Some(includes())
        );
    }

    #[test]
    fn the_two_lists_do_not_open_under_each_others_tag() {
        // Both seal under the same (root, salt) — the set's row id — so the
        // field tag is the only thing keeping them apart. If it were ever passed
        // as an argument and a caller got it wrong, the failure would be a
        // silent `Omit`, never an error; the two hard-coded wrappers exist to
        // make that unrepresentable.
        let inc = seal_include_paths(&owner_key(), 7, &includes()).unwrap();
        let exc = seal_exclude_paths(&owner_key(), 7, &["/home/me/cache".to_string()]).unwrap();
        let keys = FileDownloadKeys::owner(owner_key());

        assert_eq!(
            render_include_paths(&keys, Some(&inc), None, 7),
            Some(includes())
        );
        assert_eq!(render_exclude_paths(&keys, Some(&inc), None, 7), None);
        assert_eq!(render_include_paths(&keys, Some(&exc), None, 7), None);
    }

    #[test]
    fn a_wrong_row_id_omits_rather_than_rendering_another_sets_layout() {
        let sealed = seal_include_paths(&owner_key(), 1, &includes()).unwrap();
        let keys = FileDownloadKeys::owner(owner_key());
        assert_eq!(render_include_paths(&keys, Some(&sealed), None, 2), None);
    }

    #[test]
    fn a_roster_members_content_key_custody_cannot_open_an_owner_sealed_path_list() {
        // include/exclude is owner-only, not label-audience.
        // `seal_*_paths` takes a `BackupKey` precisely so a bound set's M2
        // generation — the root every roster member holds — cannot be passed;
        // this pins the read side of that choice. A member's custody carries the
        // set's content keys and no owner key at all.
        let sealed = seal_include_paths(&owner_key(), 11, &includes()).unwrap();
        let member = FileDownloadKeys {
            backup_key: None,
            mls_group_id: Some(vec![9u8; 32]),
            content_keys: Some(content_keys(3, [5u8; 32])),
            ..Default::default()
        };
        assert_eq!(render_include_paths(&member, Some(&sealed), None, 11), None);
    }

    #[test]
    fn an_empty_list_is_not_the_absent_list() {
        // `Some(vec![])` means "no filters" and `None` means "this reader has no
        // list"; the wire's `Option<Vec<String>>` has always distinguished them,
        // and the render seam must not collapse the first into the second the
        // way an empty *string* degrades to `Omit`.
        let sealed = seal_include_paths(&owner_key(), 5, &[]).unwrap();
        assert_eq!(
            render_include_paths(
                &FileDownloadKeys::owner(owner_key()),
                Some(&sealed),
                None,
                5
            ),
            Some(Vec::new())
        );
    }

    #[test]
    fn the_nonce_is_random_so_two_seals_of_one_list_differ() {
        let a = seal_include_paths(&owner_key(), 9, &includes()).unwrap();
        let b = seal_include_paths(&owner_key(), 9, &includes()).unwrap();
        assert_ne!(
            a, b,
            "include/exclude is mutable under its salt, so the nonce is random"
        );
    }

    #[test]
    fn a_keyless_reader_keeps_the_resting_list_and_loses_it_once_it_scrubs() {
        let sealed = seal_include_paths(&owner_key(), 3, &includes()).unwrap();
        let keyless = FileDownloadKeys::default();

        assert_eq!(
            render_include_paths(&keyless, Some(&sealed), Some(&includes()), 3),
            Some(includes())
        );
        assert_eq!(render_include_paths(&keyless, Some(&sealed), None, 3), None);
    }

    // ── snapshots.tags — the display copy (S6-d) ────────────────────────────

    fn tags() -> Vec<String> {
        vec!["manual".to_string(), "before-upgrade".to_string()]
    }

    /// A member's content-key custody, the audience shape for a bound set.
    fn member_keys(version: u64, key: [u8; 32]) -> FileDownloadKeys {
        FileDownloadKeys {
            backup_key: None,
            mls_group_id: Some(vec![9u8; 32]),
            content_keys: Some(content_keys(version, key)),
            ..Default::default()
        }
    }

    #[test]
    fn a_sealed_tag_list_round_trips_once_the_plaintext_is_gone() {
        let keys = FileDownloadKeys::owner(owner_key());
        let root = keys.label_seal_root().unwrap().unwrap();
        let sealed = seal_snapshot_tags(&root, "Family photos", &tags()).unwrap();
        // No plaintext and no wire hash — the post-flip shape, salted by
        // re-deriving from the name only because it still rests here.
        assert_eq!(
            render_snapshot_tags(&keys, Some(&sealed), None, "Family photos", None),
            Some(tags())
        );
    }

    #[test]
    fn the_wire_hash_salts_the_tag_render_when_the_set_name_has_scrubbed() {
        // The load-bearing case: post-flip the reply's `folder` is blank, so the
        // salt can only come from `folder_hash`. A render that depended on the
        // plaintext name would silently degrade to `Omit` here.
        let keys = FileDownloadKeys::owner(owner_key());
        let root = keys.label_seal_root().unwrap().unwrap();
        let sealed = seal_snapshot_tags(&root, "Family photos", &tags()).unwrap();
        let wire = crate::path_crypto::set_name_hash("Family photos");
        assert_eq!(
            render_snapshot_tags(&keys, Some(&sealed), None, "", Some(&wire)),
            Some(tags())
        );
    }

    #[test]
    fn a_roster_member_opens_a_bound_sets_tag_seal() {
        // The audience call this slice turned on, pinned from the read side: tags
        // seal under a `LabelRoot`, so a bound set's M2 generation IS a legal root
        // and every roster member renders them. This is the exact inverse of
        // `a_roster_members_content_key_custody_cannot_open_an_owner_sealed_path_list`
        // above — the two fields sit lines apart and their audiences differ, which
        // is why the funnels take different argument types.
        let member = member_keys(3, [5u8; 32]);
        let root = member.label_seal_root().unwrap().unwrap();
        let sealed = seal_snapshot_tags(&root, "shared", &tags()).unwrap();
        assert_eq!(
            render_snapshot_tags(&member, Some(&sealed), None, "shared", None),
            Some(tags())
        );
    }

    #[test]
    fn a_different_sets_custody_cannot_open_another_sets_tag_seal() {
        let sealed = seal_snapshot_tags(
            &member_keys(3, [5u8; 32])
                .label_seal_root()
                .unwrap()
                .unwrap(),
            "shared",
            &tags(),
        )
        .unwrap();
        // Same generation number, different key — the AEAD tag is the gate.
        let stranger = member_keys(3, [6u8; 32]);
        assert_eq!(
            render_snapshot_tags(&stranger, Some(&sealed), None, "shared", None),
            None
        );
    }

    /// **Driven from the FFI's ACTUAL custody shape, not a hand-built
    /// `FileDownloadKeys`** — the pin, on the field the finding was proven
    /// on. `a_roster_member_opens_a_bound_sets_tag_seal` above builds member
    /// custody by hand and so cannot see this class: the defect is in how the
    /// custody is *obtained*, not in the seal.
    ///
    /// `LabelCustody::owner_only` does **not** fail closed on a bound set: its
    /// no-resolver arm yields `FileDownloadKeys::owner(..)` with
    /// `mls_group_id: None`, so `label_seal_root`'s bound-set bail is skipped
    /// and the tags seal under the OWNER root. A snapshot is immutable — no
    /// re-record gesture ever re-stamps it — so at the flip the roster loses
    /// the tags permanently. That is why `FfiSnapshotsClient::client` wires a
    /// real resolver, and this pin is what proves the difference is real: the
    /// same set, sealed through both custody shapes.
    #[tokio::test]
    async fn owner_only_custody_seals_a_bound_sets_tags_where_a_member_cannot_follow() {
        let member = member_keys(3, [5u8; 32]);

        // (1) The wrong shape — and note it does NOT refuse. Both unwraps succeed.
        let owner_only = LabelCustody::owner_only(owner_key());
        let (keys, _) = owner_only.keys_for("shared").await;
        let root = keys
            .label_seal_root()
            .expect("owner-only custody does not error here")
            .expect("...and it does not decline to seal either — that is the trap");
        let sealed = seal_snapshot_tags(&root, "shared", &tags()).unwrap();
        assert_eq!(
            render_snapshot_tags(&member, Some(&sealed), None, "shared", None),
            None,
            "a roster member cannot open what owner-only custody sealed — and a \
             snapshot is immutable, so at the flip the tags are gone for good"
        );

        // (2) MANDATORY POSITIVE CONTROL — a resolved custody seals the same tags
        // so the same member DOES open them. Without this, a funnel that refused
        // everything would pass the assertion above and read as a fix.
        let resolved = LabelCustody::new(
            Some(Arc::new(StubResolver {
                set: "shared",
                keys: Some(content_keys(3, [5u8; 32])),
            })),
            Some(owner_key()),
        );
        let (good, _) = resolved.keys_for("shared").await;
        let good_root = good.label_seal_root().unwrap().unwrap();
        let good_sealed = seal_snapshot_tags(&good_root, "shared", &tags()).unwrap();
        assert_eq!(
            render_snapshot_tags(&member, Some(&good_sealed), None, "shared", None),
            Some(tags()),
            "positive control: correctly resolved custody IS member-openable"
        );
    }

    // ── S6-e: folders.retention_policy ──────────────────────────────────

    const POLICY: &str = r#"{"max_snapshots":7,"max_age_days":30}"#;

    #[test]
    fn a_sealed_retention_policy_round_trips_once_the_plaintext_is_gone() {
        let keys = FileDownloadKeys::owner(owner_key());
        let root = keys.label_seal_root().unwrap().unwrap();
        let sealed = seal_retention_policy(&root, "Family photos", POLICY).unwrap();
        // The post-flip shape: no plaintext column left to fall back on.
        assert_eq!(
            render_retention_policy(&keys, Some(&sealed), None, "Family photos", None),
            Some(POLICY.to_string())
        );
    }

    /// **The audience pin, and the one this slice was closest to getting wrong.**
    /// Retention is sealed to the LABEL AUDIENCE, so a roster member — who holds
    /// the set's M2 content keys but not the owner's `BackupKey` — must be able to
    /// open it. Sealing under the owner root instead (the `seal_include_paths`
    /// shape three functions up) would leave this member with an unopenable blob
    /// and blank their retention display at the flip: a NARROWING that reads as
    /// hardening. If someone "tightens" the root, this test is what should redden.
    #[test]
    fn a_roster_member_can_open_the_retention_seal_because_the_audience_includes_them() {
        let member = member_keys(3, [4u8; 32]);
        let root = member.label_seal_root().unwrap().unwrap();
        let sealed = seal_retention_policy(&root, "shared", POLICY).unwrap();
        assert_eq!(
            render_retention_policy(&member, Some(&sealed), None, "shared", None),
            Some(POLICY.to_string())
        );
        // ...and a stranger to the set still cannot, so "member-openable" is not
        // "everyone-openable" — the positive control that keeps the pin honest.
        let stranger = member_keys(3, [6u8; 32]);
        assert_eq!(
            render_retention_policy(&stranger, Some(&sealed), None, "shared", None),
            None
        );
    }

    /// **Driven from the FFI's ACTUAL custody shape, not a hand-built
    /// `FileDownloadKeys`** — a lesson, applied to the field
    /// S6-e added rather than only to the one that provoked it.
    ///
    /// `LabelCustody::owner_only` does **not** fail closed on a bound set: its
    /// no-resolver arm yields `FileDownloadKeys::owner(..)` with
    /// `mls_group_id: None`, so `label_seal_root`'s bound-set bail is skipped and
    /// the label seals under the OWNER root. For a label-audience field that is
    /// silent data loss at the flip — the plaintext scrubs, a sealed sibling
    /// exists so S8 skips the row, and every roster member loses it.
    ///
    /// So the FFI façade wires a real resolver, and this pin is what proves the
    /// difference is real: the same set, sealed through both custody shapes.
    #[tokio::test]
    async fn owner_only_custody_seals_a_bound_set_where_a_member_cannot_follow() {
        let member = member_keys(3, [5u8; 32]);

        // (1) The wrong shape — and note it does NOT refuse. Both unwraps succeed.
        let owner_only = LabelCustody::owner_only(owner_key());
        let (keys, _) = owner_only.keys_for("shared").await;
        let root = keys
            .label_seal_root()
            .expect("owner-only custody does not error here")
            .expect("...and it does not decline to seal either — that is the trap");
        let sealed = seal_retention_policy(&root, "shared", POLICY).unwrap();
        assert_eq!(
            render_retention_policy(&member, Some(&sealed), None, "shared", None),
            None,
            "a roster member cannot open what owner-only custody sealed — which at \
             the flip means they lose the field permanently"
        );

        // (2) MANDATORY POSITIVE CONTROL — a resolved custody seals the same policy
        // so the same member DOES open it. Without this, a funnel that refused
        // everything would pass the assertion above and read as a fix.
        let resolved = LabelCustody::new(
            Some(Arc::new(StubResolver {
                set: "shared",
                keys: Some(content_keys(3, [5u8; 32])),
            })),
            Some(owner_key()),
        );
        let (good, _) = resolved.keys_for("shared").await;
        let good_root = good.label_seal_root().unwrap().unwrap();
        let good_sealed = seal_retention_policy(&good_root, "shared", POLICY).unwrap();
        assert_eq!(
            render_retention_policy(&member, Some(&good_sealed), None, "shared", None),
            Some(POLICY.to_string()),
            "positive control: correctly resolved custody IS member-openable"
        );
    }

    #[test]
    fn the_salt_binds_the_retention_seal_to_its_own_set() {
        let keys = FileDownloadKeys::owner(owner_key());
        let root = keys.label_seal_root().unwrap().unwrap();
        let sealed = seal_retention_policy(&root, "Family photos", POLICY).unwrap();
        // Right key, wrong set: the salt is mixed into both the derivation and the
        // AAD, so a blob cannot be replayed onto a sibling set's row.
        assert_eq!(
            render_retention_policy(&keys, Some(&sealed), None, "Work docs", None),
            None
        );
    }

    #[test]
    fn the_retention_nonce_is_random_so_two_saves_of_one_set_differ() {
        // The salt is the *set's* name digest, stable across an edit, so a derived
        // nonce would reuse (key, nonce) across two differing policies of one set.
        // `file-sync.md` § Sealed names & paths puts every mutable-under-salt field
        // in the random-nonce list.
        let keys = FileDownloadKeys::owner(owner_key());
        let root = keys.label_seal_root().unwrap().unwrap();
        let a = seal_retention_policy(&root, "Family photos", POLICY).unwrap();
        let b = seal_retention_policy(&root, "Family photos", POLICY).unwrap();
        assert_ne!(a, b);
    }

    #[test]
    fn a_keyless_reader_keeps_the_resting_policy_and_loses_it_once_it_scrubs() {
        let root = FileDownloadKeys::owner(owner_key())
            .label_seal_root()
            .unwrap()
            .unwrap();
        let sealed = seal_retention_policy(&root, "Family photos", POLICY).unwrap();
        let keyless = FileDownloadKeys::default();
        // During expand the plaintext still rests, so a keyless reader shows it.
        assert_eq!(
            render_retention_policy(&keyless, Some(&sealed), Some(POLICY), "Family photos", None),
            Some(POLICY.to_string())
        );
        // Post-flip it is gone, and the ratified degrade is Omit — never the blank
        // plaintext presented as the policy.
        assert_eq!(
            render_retention_policy(&keyless, Some(&sealed), None, "Family photos", None),
            None
        );
    }

    /// The wire `name_hash` is what keeps the seal openable once the plaintext
    /// `name` scrubs — the S2b/S4 hole, pinned for this field too.
    #[test]
    fn the_retention_seal_opens_from_the_wire_hash_alone() {
        let keys = FileDownloadKeys::owner(owner_key());
        let root = keys.label_seal_root().unwrap().unwrap();
        let sealed = seal_retention_policy(&root, "Family photos", POLICY).unwrap();
        let hash = crate::path_crypto::set_name_hash("Family photos");
        assert_eq!(
            render_retention_policy(&keys, Some(&sealed), None, "", Some(&hash)),
            Some(POLICY.to_string())
        );
    }

    #[test]
    fn the_salt_binds_the_tag_seal_to_its_own_set() {
        let keys = FileDownloadKeys::owner(owner_key());
        let root = keys.label_seal_root().unwrap().unwrap();
        let sealed = seal_snapshot_tags(&root, "Family photos", &tags()).unwrap();
        // Right key, wrong set: the salt is mixed into the derivation and the AAD,
        // so a blob cannot be replayed onto a sibling set's snapshot.
        assert_eq!(
            render_snapshot_tags(&keys, Some(&sealed), None, "Work docs", None),
            None
        );
    }

    #[test]
    fn the_tag_nonce_is_random_so_two_snapshots_of_one_set_differ() {
        // The salt is the *set's* name digest, which does not vary between two
        // snapshots of that set — so a derived nonce would reuse (key, nonce)
        // across differing tag lists. `file-sync.md` names tags in the random list.
        let keys = FileDownloadKeys::owner(owner_key());
        let root = keys.label_seal_root().unwrap().unwrap();
        let a = seal_snapshot_tags(&root, "Family photos", &tags()).unwrap();
        let b = seal_snapshot_tags(&root, "Family photos", &tags()).unwrap();
        assert_ne!(a, b);
    }

    #[test]
    fn an_empty_tag_list_is_not_the_absent_tag_list() {
        let keys = FileDownloadKeys::owner(owner_key());
        let root = keys.label_seal_root().unwrap().unwrap();
        let sealed = seal_snapshot_tags(&root, "Family photos", &[]).unwrap();
        assert_eq!(
            render_snapshot_tags(&keys, Some(&sealed), None, "Family photos", None),
            Some(Vec::new())
        );
    }

    #[test]
    fn a_keyless_reader_keeps_the_resting_tags_and_loses_them_once_they_scrub() {
        let root = FileDownloadKeys::owner(owner_key())
            .label_seal_root()
            .unwrap()
            .unwrap();
        let sealed = seal_snapshot_tags(&root, "Family photos", &tags()).unwrap();
        let keyless = FileDownloadKeys::default();
        assert_eq!(
            render_snapshot_tags(
                &keyless,
                Some(&sealed),
                Some(&tags()),
                "Family photos",
                None
            ),
            Some(tags())
        );
        assert_eq!(
            render_snapshot_tags(&keyless, Some(&sealed), None, "Family photos", None),
            None
        );
    }

    // ─────────────────────────────────────────────────────────────────────
    // The identity-succession read fallback, pinned AT THE FUNNEL.
    //
    // ⚠ These pins exist because the five tier_1 tests in `file_download.rs`
    // cannot serve: every one hand-builds
    // `FileDownloadKeys { predecessor_backup_keys: vec![..], .. }`, so they pass
    // identically whether or not any production assembly ever produces that
    // value — which is exactly how the read half shipped with **zero production
    // writers**. A pin is only about
    // the code if it starts from the thing production actually builds.
    // ─────────────────────────────────────────────────────────────────────

    fn predecessor_key() -> BackupKey {
        BackupKey::from_bytes([0x11u8; 32])
    }

    /// The successor's custody, assembled exactly as a client's post-auth hook
    /// does it: `LabelCustody::new(resolver, own_key).with_predecessors(walk)`.
    fn successor_custody(resolver: Option<Arc<dyn FolderKeyResolver>>) -> LabelCustody {
        LabelCustody::new(resolver, Some(owner_key())).with_predecessors(vec![predecessor_key()])
    }

    /// **The funnel pin, byte half.** A successor-configured production custody
    /// yields a non-empty predecessor list and opens a file sealed under the
    /// retired root.
    ///
    /// Mutation contract: drop `predecessor_backup_keys` from `keys_for`'s
    /// unbound arm and this reds (and, with its label twin below, is the only
    /// new red).
    #[tokio::test]
    async fn the_unbound_funnel_yields_keys_that_open_a_predecessor_sealed_file() {
        let (keys, _) = successor_custody(None).keys_for("photos").await;

        assert_eq!(
            keys.predecessor_backup_keys.len(),
            1,
            "the assembly a successor's client builds must carry the retired root; \
             empty here is the shape that made the whole read half inert"
        );

        // Positive control on the same assembly: the *current* owner key is
        // still offered, so a red above is about the predecessor threading and
        // not about custody assembly in general.
        assert!(
            keys.backup_key.is_some(),
            "the current owner key must still arrive — predecessors are a fallback \
             BESIDE it, never instead of it"
        );

        // The observable that matters to a reader: the retired root is among the
        // candidates every open loops over. `label_open_roots` is the public
        // face of that selection; the chunk twin (`content_open_roots`) is
        // private and reads the same two fields.
        let roots = keys.label_open_roots(None).expect("owner arm yields roots");
        let pre_label_root = FileDownloadKeys::owner(predecessor_key())
            .label_open_roots(None)
            .expect("the predecessor's own custody yields its label root");
        assert!(
            roots.contains(&pre_label_root[0]),
            "the retired root must be an OPEN candidate: a successor's corpus is \
             still sealed under it, and without this every fetch succeeds and only \
             the AEAD tag fails — dark, indistinguishable from corruption"
        );
    }

    /// **The funnel pin, label half.** The same assembly renders a name sealed
    /// under the retired root — a successor that opened its bytes but not its
    /// names would show an empty file list over a corpus it can read.
    #[tokio::test]
    async fn the_unbound_funnel_renders_a_predecessor_sealed_name() {
        let (keys, _) = successor_custody(None).keys_for("photos").await;

        let pre_root = FileDownloadKeys::owner(predecessor_key())
            .label_seal_root()
            .expect("the predecessor's own custody resolves a seal root")
            .expect("owner arm always has one");
        let sealed =
            seal_path(&pre_root, "holiday/beach.jpg").expect("seal under the retired root");
        // A sealed-only row after the flip carries its `path_hash` on the wire
        // and an empty plaintext column — the shape this render must survive.
        let salt = crate::sync::path_hash("holiday/beach.jpg");

        assert_eq!(
            render_path(
                &keys,
                Some(&sealed),
                "",
                Some(&salt),
                LabelField::SyncChangePath,
            ),
            SealedLabelRender::Sealed("holiday/beach.jpg".to_string()),
            "the successor must render the name its predecessor sealed"
        );
    }

    /// A **bound** set's arm carries the retired roots too — the label plane
    /// keeps the owner root on a bound set so an owner still renders the names
    /// it sealed *before* binding, and a predecessor's pre-binding names are the
    /// same case one identity back. (The chunk plane suppresses both together;
    /// that decision lives in `effective_backup_key`, not here.)
    #[tokio::test]
    async fn the_bound_funnel_also_carries_the_retired_roots() {
        let resolver: Arc<dyn FolderKeyResolver> = Arc::new(StubResolver {
            set: "shared",
            keys: Some(content_keys(1, [9u8; 32])),
        });
        let (keys, _) = successor_custody(Some(resolver)).keys_for("shared").await;

        assert_eq!(
            keys.predecessor_backup_keys.len(),
            1,
            "the bound arm must travel with `backup_key` arm for arm"
        );
        assert!(
            keys.backup_key.is_some(),
            "positive control: so must the owner key"
        );
    }

    /// A custody with **no** predecessors — the overwhelmingly common fleet —
    /// is byte-for-byte the pre-succession shape. The negative control that
    /// stops the two pins above from passing on a custody that always offers
    /// something.
    #[tokio::test]
    async fn a_never_succeeded_custody_offers_no_retired_roots() {
        let (keys, _) = LabelCustody::new(None, Some(owner_key()))
            .keys_for("photos")
            .await;
        assert!(
            keys.predecessor_backup_keys.is_empty(),
            "an identity that never succeeded must pay nothing"
        );
    }

    /// The **owner plane** (device labels, a set's owner-audience include/exclude
    /// lists) reads through its own accessor, and it must offer the retired roots
    /// while the *seal*-side accessor beside it must not.
    ///
    /// ⚠ This pair is the write-side guard: `owner_key()` is what
    /// `seal_device_label` uses, and a retired root reaching it would be a
    /// **silent** wrong-root seal (a wrong label root degrades to `Omit`, never
    /// an error).
    #[test]
    fn the_owner_plane_reads_with_retired_roots_and_seals_without_them() {
        let custody = successor_custody(None);

        let read = custody.owner_plane_read_keys();
        assert_eq!(
            read.predecessor_backup_keys.len(),
            1,
            "the read accessor must offer the retired root, else a successor's \
             device labels and path lists silently degrade to their no-list state"
        );

        // `BackupKey` is deliberately neither `Debug` nor `PartialEq` (it is
        // secret), so the seal-side assertion is made through the same public
        // face a seal site uses: the root it resolves.
        let seal_root = FileDownloadKeys::owner(
            custody
                .owner_key()
                .expect("the seal accessor yields the current key"),
        )
        .label_seal_root()
        .expect("resolves")
        .expect("owner arm always has one");
        let current_root = FileDownloadKeys::owner(owner_key())
            .label_seal_root()
            .expect("resolves")
            .expect("owner arm always has one");
        let retired_root = FileDownloadKeys::owner(predecessor_key())
            .label_seal_root()
            .expect("resolves")
            .expect("owner arm always has one");
        assert_eq!(
            seal_path(&seal_root, "x").unwrap(),
            seal_path(&current_root, "x").unwrap(),
            "the SEAL accessor must yield the CURRENT key — there is no shape in \
             which a new seal may land under a key the aftermath exists to retire"
        );
        assert_ne!(
            seal_path(&seal_root, "x").unwrap(),
            seal_path(&retired_root, "x").unwrap(),
            "positive control: the two roots really are distinguishable, so the \
             assertion above is about the accessor and not about seal_path"
        );
    }

    // seal_path_from_keys: pins the shared mechanics behind every per-domain
    // "seal a path on a user gesture" machine method (devices' re-point
    // record, media's delete/restore record). No single domain's own test
    // suite states these — each pins only its own logging wording.

    #[test]
    fn seal_path_from_keys_has_no_root_when_keyless() {
        let keys = FileDownloadKeys::default();
        assert!(matches!(
            seal_path_from_keys(&keys, "photos/a.jpg"),
            Err(SealPathFromKeysError::NoRoot)
        ));
    }

    #[test]
    fn seal_path_from_keys_seals_under_the_owner_root() {
        let keys = FileDownloadKeys::owner(owner_key());
        let root = keys.label_seal_root().unwrap().unwrap();
        let sealed = seal_path_from_keys(&keys, "photos/a.jpg").expect("owner root always seals");
        assert_eq!(
            sealed,
            seal_path(&root, "photos/a.jpg").unwrap(),
            "must seal under exactly the same root label_seal_root resolves"
        );
    }

    #[tokio::test]
    async fn seal_path_from_keys_fails_closed_when_bound_but_unresolvable() {
        // The cell, exercised through the shared seal helper this
        // time rather than `label_seal_root` directly: a bound-but-unresolvable
        // resolve must surface as `RootUnresolved`, never silently degrade to
        // `NoRoot` (which a caller would treat as "nothing to log") or to a
        // seal under the owner root no roster member could open.
        let custody = LabelCustody::new(
            Some(Arc::new(StubResolver {
                set: "shared",
                keys: None,
            })),
            Some(owner_key()),
        );
        let (keys, _) = custody.keys_for("shared").await;
        assert!(matches!(
            seal_path_from_keys(&keys, "x"),
            Err(SealPathFromKeysError::RootUnresolved(_))
        ));
    }

    // ─────────────────────────────────────────────────────────────────
    // `open_change_path` — the apply path's opener
    // ─────────────────────────────────────────────────────────────────

    /// The owner root every cell below seals and opens under.
    fn change_path_root() -> path_crypto::LabelRoot {
        path_crypto::LabelRoot::owner_of(&owner_key())
    }

    fn one_root(root: &path_crypto::LabelRoot) -> Vec<[u8; 32]> {
        // The single-owner shape: exactly one root, whatever the envelope's `gen`.
        let _ = root;
        vec![owner_key().convergent_chunk_root()]
    }

    #[test]
    fn a_well_formed_row_opens_and_binds() {
        const PATH: &str = "photos/a.jpg";
        let root = change_path_root();
        let sealed = seal_path(&root, PATH).unwrap();
        let wire = hex::encode(crate::sync::path_hash(PATH));

        assert_eq!(
            open_change_path(|_| Ok(one_root(&root)), Some(&sealed), &wire),
            ChangePathOpen::Opened(PATH.to_string())
        );
    }

    /// Do 4 — **the binding check**. A writer holding the set's label key
    /// seals path P under Q's hash: the AEAD binds the salt, so the envelope
    /// opens perfectly, and without this check every device writes P while
    /// the nest's per-path heads, conflicts and history record Q.
    #[test]
    fn a_path_sealed_under_another_rows_hash_is_refused() {
        const REAL: &str = "photos/a.jpg";
        const IMPERSONATED: &str = "keys/id_ed25519";
        let root = change_path_root();
        // The attack row: IMPERSONATED's plaintext, sealed under REAL's salt.
        let sealed = path_crypto::seal_convergent(
            &root,
            &crate::sync::path_hash(REAL),
            LabelField::SyncChangePath,
            IMPERSONATED.as_bytes(),
        )
        .unwrap()
        .to_bytes()
        .unwrap();
        let wire = hex::encode(crate::sync::path_hash(REAL));

        // It DOES open — that is exactly why opening alone is not enough.
        assert!(matches!(
            path_crypto::render_sealed_label(
                &FileDownloadKeys::owner(owner_key()),
                Some(&sealed),
                None,
                &crate::sync::path_hash(REAL),
                LabelField::SyncChangePath,
            ),
            SealedLabelRender::Sealed(ref p) if p == IMPERSONATED
        ));

        assert_eq!(
            open_change_path(|_| Ok(one_root(&root)), Some(&sealed), &wire),
            ChangePathOpen::Refused(ChangePathRefusal::Unbound),
            "an opened path that does not hash to its row must be refused"
        );
    }

    /// Do 3 — the malformed split. A blob that is not an envelope can never
    /// decode under any key, so it must NOT be filed as "key material may
    /// still be syncing": that class holds the anchor below the row forever.
    #[test]
    fn a_malformed_envelope_is_permanent_not_transient() {
        let wire = hex::encode(crate::sync::path_hash("photos/a.jpg"));
        assert_eq!(
            open_change_path(
                |_| Ok(one_root(&change_path_root())),
                Some(b"not cbor"),
                &wire
            ),
            ChangePathOpen::Refused(ChangePathRefusal::Envelope)
        );
    }

    /// A row with no seal at all — and, by the time the opener sees it, no
    /// plaintext `path` either — is refused, never held and never skipped in
    /// silence: no current writer lands the shape on a plane whose plaintext
    /// scrubs (the nest refuses a seal-less record there) and a plaintext plane
    /// serves its `path`, so nothing a later pull brings can make it apply.
    /// Its silent skip-and-advance arm was the pre-expand hash-only remnant.
    #[test]
    fn a_seal_less_row_is_refused_not_skipped() {
        let wire = hex::encode(crate::sync::path_hash("photos/a.jpg"));
        assert_eq!(
            open_change_path(|_| Ok(one_root(&change_path_root())), None, &wire),
            ChangePathOpen::Refused(ChangePathRefusal::NoSeal)
        );
        // Decided before any root is consulted: a keyless holder gets the
        // same permanent verdict, not the transient `NoRoot`.
        assert_eq!(
            open_change_path(|_| Ok(Vec::new()), None, &wire),
            ChangePathOpen::Refused(ChangePathRefusal::NoSeal)
        );
    }

    /// The other locally-decidable half: a row whose `path_hash` is not 32 hex
    /// bytes has no reconstructable salt, and on the apply path there is no
    /// plaintext to fall back to ([`path_label_salt`]'s malformed-hash degrade
    /// is a *read* surface's). Permanent, not a freeze.
    #[test]
    fn a_malformed_row_hash_is_permanent_not_transient() {
        const PATH: &str = "photos/a.jpg";
        let root = change_path_root();
        let sealed = seal_path(&root, PATH).unwrap();

        for bad in ["", "zz", &hex::encode([1u8; 31])] {
            assert_eq!(
                open_change_path(|_| Ok(one_root(&root)), Some(&sealed), bad),
                ChangePathOpen::Refused(ChangePathRefusal::Salt),
                "row hash {bad:?} must refuse, not freeze the set"
            );
        }
    }

    /// The hash is judged FIRST, whatever else is wrong with the row: a
    /// malformed `path_hash` is exactly the `Salt` refusal, so every host's
    /// verdict and the skip floor it records agree that the row has no usable
    /// per-path key .
    #[test]
    fn a_malformed_row_hash_is_the_salt_refusal_whatever_the_seal() {
        for sealed in [None, Some(&b"not cbor"[..])] {
            assert_eq!(
                open_change_path(|_| Ok(one_root(&change_path_root())), sealed, "x/../escape"),
                ChangePathOpen::Refused(ChangePathRefusal::Salt),
                "seal {sealed:?}"
            );
        }
    }

    /// The class that MUST stay transient: a generation this holder does not
    /// hold yet. Both shapes of "no candidates" — an empty list and the
    /// fail-closed `Err` — are one verdict.
    #[test]
    fn an_unheld_generation_stays_transient() {
        const PATH: &str = "photos/a.jpg";
        let sealed = seal_path(&change_path_root(), PATH).unwrap();
        let wire = hex::encode(crate::sync::path_hash(PATH));

        assert_eq!(
            open_change_path(|_| Ok(Vec::new()), Some(&sealed), &wire),
            ChangePathOpen::NoRoot
        );
        assert_eq!(
            open_change_path(
                |_| anyhow::bail!("generation not held"),
                Some(&sealed),
                &wire
            ),
            ChangePathOpen::NoRoot
        );
    }

    /// A root that simply is not the sealing root fails at the AEAD tag, which
    /// is indistinguishable from a tampered ciphertext — so it stays
    /// transient. Filing it permanent would strand a device that is merely
    /// behind on key material.
    #[test]
    fn a_wrong_root_stays_transient() {
        const PATH: &str = "photos/a.jpg";
        let sealed = seal_path(&change_path_root(), PATH).unwrap();
        let wire = hex::encode(crate::sync::path_hash(PATH));

        assert_eq!(
            open_change_path(|_| Ok(vec![[9u8; 32]]), Some(&sealed), &wire),
            ChangePathOpen::NoRoot
        );
    }

    /// The envelope's own `gen` is what reaches the caller's custody — the
    /// engine's `label_open_roots` needs it to pick the M2 candidates.
    #[test]
    fn the_envelopes_generation_reaches_the_root_resolver() {
        const PATH: &str = "photos/a.jpg";
        let secret = [3u8; 32];
        let sealed = seal_path(&path_crypto::LabelRoot::content_key(secret, 7), PATH).unwrap();
        let wire = hex::encode(crate::sync::path_hash(PATH));

        let mut seen = None;
        let out = open_change_path(
            |generation| {
                seen = generation;
                Ok(vec![secret])
            },
            Some(&sealed),
            &wire,
        );
        assert_eq!(seen, Some(7));
        assert_eq!(out, ChangePathOpen::Opened(PATH.to_string()));
    }
}
