//! **Engine content keys** — resolve every set's engine key material
//! (`FolderEngineKeys`: the fail-closed `(mls_group_id, content_keys)` pair,
//! retired serve generations, cross-nest routing) for one holder, from that
//! holder's own folder-key custody (`fauna.state.folder-keys`) and this nest's
//! folder list.
//!
//! The *impure* companion to the pure [`crate::engine_binding`] resolver: it does
//! the two reads the resolver deliberately does not — the holder's custody
//! ([`FolderKeyReader::load`]) and the nest's folders
//! ([`FoldersClient::list_owned_and_shared`]) — then feeds both into
//! [`engine_keys_from`].
//!
//! **One producer, source-agnostic.** The caller hands in the
//! [`FolderKeyReader`] its custody comes from: the seat's account store on an
//! identity-holding app (the share host's pump), the mounted store or a
//! capability host's cold fleet replica on a host that resolves every set's
//! keys in-process at its own re-resolve edges (`on-demand-files.md` § Shared
//! sets on a capability host → *One mechanism*, decision 1′). Keeping the
//! produce path here (priority #2) means the security-critical fail-closed
//! decision is resolved once, not re-implemented per host.
//!
//! Authority: `docs/goal/architecture/mls-group-key-material.md` § M2 content-key
//! mechanism; the agent's edges: `docs/goal/behavior/on-demand-files.md`
//! § Shared sets on a capability host.

use fauna_core::folder_keys::FolderEngineKeys;
use fauna_protocol::RpcRequester;
use fauna_protocol::folders::FolderSummary;

use crate::FoldersClient;
use crate::engine_binding::{
    EngineKeyBindingError, resolve_engine_key_bindings, resolve_foreign_engine_key_bindings,
};
use crate::key_reader::FolderKeyReader;
use fauna_core::data::FoldersConfig;

/// Resolve **every** bindable set's engine key material for the holder `own`
/// whose custody `custody` reads: load the custody, list this nest's folders, and
/// resolve the per-set fail-closed binding ([`engine_keys_from`]). An empty vec
/// when the holder has no file sets.
///
/// **The roster is [`FoldersClient::list_owned_and_shared`], NOT the owner-scoped
/// [`FoldersClient::list`]** — a *writer member* binds a folder to someone else's
/// set, and that set appears only in the member-visible projection. Listing
/// owner-scoped omitted every shared set from the batch, so the agent found no
/// entry, ran the engine unbound, and sealed and opened under the member's own
/// `BackupKey` instead of the set's M2 content key — the member could not decrypt
/// the owner's uploads (`aead::Error`), a silent wrong-key path, not a fail-closed
/// one. Pinned by `member_visible_projection_is_requested` below and end-to-end by
/// `tests/e2e-unified/tests/test_folder_agent_content_sync.py`. The member-read
/// twin, `NestFolderKeyResolver::resolve`, uses this roster too — the two
/// resolvers must see the same set of sets or they disagree about what a bound
/// folder's key material is.
///
/// **Every entry carries its `FolderRef` identity, and that is what a host
/// matches on.** Names are unique only per owner, so a caller who owns "docs"
/// *and* is a member of someone else's "docs" produces two same-named entries —
/// deliberately; the batch is the union of everything bindable, told apart by
/// `FolderRef::Local(summary.id)` / `FolderRef::Foreign(channel_id)`.
///
/// The whole batch fails (`Err`) when custody or the list cannot be read, or a
/// summary is a malformed nest projection (corrupt hex `mls_group_id`); the
/// caller keeps what it held and re-resolves at its next edge. A
/// served-but-keyless set is narrower: the resolver omits just that one set
/// (logged) rather than failing the rest of the fleet
/// ([`resolve_engine_key_bindings`]), because absent-from-the-batch already means
/// no engine for that one set, never owner-only.
pub async fn resolve_engine_keys<R>(
    custody: &dyn FolderKeyReader,
    own: fauna_core::identity::ActorId,
    nest: R,
) -> Result<Vec<FolderEngineKeys>, EngineKeysError>
where
    R: RpcRequester + Clone,
    R::Error: core::fmt::Display,
{
    let cfg = custody
        .load()
        .await
        .map_err(|e| EngineKeysError::CustodyLoad(format!("{e:#}")))?;
    let markers = adoption_markers_or_none(custody).await;
    // The wire rows, named from the custody just loaded
    // ([`crate::engine_binding::named_for_engine_host`]): this client holds no
    // label custody, so the rendered list would omit every sealed set — and a
    // host that resolved none served none ("No shared folders to serve").
    let summaries = FoldersClient::new(nest)
        .list_owned_and_shared_wire()
        .await
        .map_err(|e| EngineKeysError::FoldersList(e.to_string()))?
        .folders;
    let summaries = crate::engine_binding::named_for_engine_host(summaries, &cfg);
    engine_keys_from(&cfg, own, &summaries, &markers).map_err(EngineKeysError::Resolve)
}

/// This device's adoption markers (`writer-signed-change-records.md` ruling
/// (11)(d)), or none when the source cannot read them — a marker unread is a
/// licence not granted this resolve, never a wrong key, so the batch goes on.
pub async fn adoption_markers_or_none(custody: &dyn FolderKeyReader) -> Vec<[u8; 32]> {
    custody.adoption_markers().await.unwrap_or_else(|e| {
        tracing::warn!(
            error = %format!("{e:#}"),
            "engine keys: this device's adoption markers are unreadable; none carried this resolve"
        );
        Vec::new()
    })
}

/// The pure half of [`resolve_engine_keys`]: the same-nest sets `summaries`
/// lists, resolved against the custody in `cfg`, with the holder's cross-nest
/// sets appended ([`union_foreign_bindings`]). `own` is the session's own
/// identity — the declassification anchor.
///
/// Public for a host that reads the two inputs itself and must decide what a
/// failed custody read leaves it with: an empty custody
/// ([`FoldersConfig::default`]) resolves every bound set **keyless**
/// (`Some(gid)` + `None` keys — the engine fails closed per operation), omits
/// every served-but-keyless set, and names no cross-nest set, so no set is ever
/// handed an owner-only key it does not have.
///
/// `adoption_markers` are this device's (`FolderKeyReader::adoption_markers`).
pub fn engine_keys_from(
    cfg: &FoldersConfig,
    own: fauna_core::identity::ActorId,
    summaries: &[FolderSummary],
    adoption_markers: &[[u8; 32]],
) -> Result<Vec<FolderEngineKeys>, EngineKeyBindingError> {
    let same_nest = resolve_engine_key_bindings(cfg, own, summaries, adoption_markers)?;
    Ok(union_foreign_bindings(same_nest, cfg))
}

/// Append this holder's **cross-nest** sets to the nest-projected
/// batch.
///
/// A set homed on another nest appears in **no** `fauna.folders.list`
/// projection, owner-scoped or member-visible: the holder's own nest has no row
/// for it at all. It exists only in their custody
/// ([`resolve_foreign_engine_key_bindings`]). Unioning the two sources here is
/// what lets a cross-nest writer's agent build a **bound, foreign-routed** engine
/// instead of the unbound one that would seal their edits under their own
/// `BackupKey` — the exact defect the same-nest member roster fix closed on
/// 2026-07-22, one plane over.
///
/// Same-nest rows win a group-id tie (belt and braces — mirroring
/// `DevicesMachine::foreign_rows`, which drops a foreign row whose group the
/// own-nest list already carries): a set should never be in both planes, and if
/// it somehow is, the nest's own row is the one the rest of the same-nest seam
/// agrees with.
fn union_foreign_bindings(
    mut same_nest: Vec<FolderEngineKeys>,
    cfg: &FoldersConfig,
) -> Vec<FolderEngineKeys> {
    let same_nest_groups: Vec<Option<Vec<u8>>> =
        same_nest.iter().map(|b| b.mls_group_id.clone()).collect();
    let foreign = resolve_foreign_engine_key_bindings(cfg)
        .into_iter()
        .filter(|f| !same_nest_groups.contains(&f.mls_group_id));

    // A name collision across the two planes is *legal* — names are unique only
    // per owner — and harmless: every entry carries its `FolderRef`, and a host
    // resolves by nothing else.
    same_nest.extend(foreign);
    same_nest
}

/// A failure resolving a holder's engine keys ([`resolve_engine_keys`]).
#[derive(Debug)]
pub enum EngineKeysError {
    /// Reading the holder's folder-key custody failed — "custody unreadable":
    /// the caller keeps what it held (or builds keyless), never plaintext.
    CustodyLoad(String),
    /// Listing the nest's folders failed (nest round-trip).
    FoldersList(String),
    /// The per-set resolver refused a summary — a malformed nest projection
    /// (corrupt hex `mls_group_id`); the batch is withheld, not shipped
    /// degradable. A served-but-keyless set never reaches this arm — the
    /// resolver omits it from the batch instead.
    Resolve(EngineKeyBindingError),
}

impl core::fmt::Display for EngineKeysError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::CustodyLoad(e) => write!(f, "load folder-key custody: {e}"),
            Self::FoldersList(e) => write!(f, "list folders: {e}"),
            Self::Resolve(e) => write!(f, "resolve engine key bindings: {e}"),
        }
    }
}

impl std::error::Error for EngineKeysError {}

#[cfg(test)]
mod tests {
    use super::*;
    use fauna_client_testkit::block_on;
    use fauna_core::folder_keys::FolderEngineKeys;
    use fauna_protocol::folders::{FolderSummary, FoldersListReply, FoldersListRequest};
    use std::sync::Mutex;

    /// The raw MLS group id the shared set below is bound to; the resolver
    /// derives its custody `ChannelId` from these bytes.
    const SHARED_GROUP_ID: [u8; 20] = [0x7c; 20];

    /// A mock nest whose folder roster holds one owned set and one
    /// shared-with-me set. Records the `fauna.folders.list` request payload so
    /// a test can assert which projection was asked for.
    #[derive(Clone, Default)]
    struct RosterNest {
        list_request: std::sync::Arc<Mutex<Option<Vec<u8>>>>,
    }

    impl fauna_protocol::RpcRequester for RosterNest {
        type Error = std::convert::Infallible;

        async fn request<Req, Reply>(
            &self,
            kind: &'static str,
            payload: Req,
        ) -> Result<Reply, Self::Error>
        where
            Req: serde::Serialize,
            Reply: serde::de::DeserializeOwned,
        {
            let reply = match kind {
                "fauna.folders.list" => {
                    let encoded = fauna_protocol::encode_canonical(&payload)
                        .expect("encode request")
                        .to_vec();
                    let req: FoldersListRequest =
                        fauna_protocol::decode_strict(&encoded).expect("request decodes");
                    *self.list_request.lock().unwrap() = Some(encoded);
                    // Mirror `list_core`: the shared pass runs only when the
                    // caller opted in, and owned rows always come first.
                    let mut folders = vec![FolderSummary {
                        id: 1,
                        name: "my-own".into(),
                        ..Default::default()
                    }];
                    if req.include_shared_with_me == Some(true) {
                        folders.push(FolderSummary {
                            id: 2,
                            name: "shared-with-me".into(),
                            mls_group_id: Some(hex::encode(SHARED_GROUP_ID)),
                            role: Some("member".into()),
                            access: Some("writer".into()),
                            ..Default::default()
                        });
                    }
                    fauna_protocol::encode_canonical(&FoldersListReply {
                        folders,
                        extra: Default::default(),
                    })
                }
                other => panic!("RosterNest: unhandled kind {other}"),
            }
            .expect("encode reply");
            Ok(fauna_protocol::decode_strict(&reply).expect("decode reply"))
        }
    }

    /// The holder's identity — the declassification anchor.
    const OWN: fauna_core::identity::ActorId = fauna_core::identity::ActorId([0x11; 32]);

    fn compute() -> (RosterNest, Vec<FolderEngineKeys>) {
        let nest = RosterNest::default();
        let custody = crate::key_reader::MemoryFolderKeyStore::default();
        let keys = block_on(resolve_engine_keys(&custody, OWN, nest.clone()))
            .expect("the mock roster resolves");
        (nest, keys)
    }

    /// The custody a bound set's keys come from is the reader's — whichever
    /// source the host hands in (the seat's store, a capability host's cold
    /// replica), the batch carries the generation it holds.
    #[test]
    fn a_bound_sets_generation_comes_from_the_custody_reader() {
        let channel = fauna_core::folder_keys::channel_id_for_group(&SHARED_GROUP_ID);
        let mut custody = FoldersConfig::default();
        crate::custody::record_new_set(&mut custody, channel, [0x42; 32], 1_000);
        let reader = crate::key_reader::MemoryFolderKeyStore::with(custody);
        let keys =
            block_on(resolve_engine_keys(&reader, OWN, RosterNest::default())).expect("resolves");
        let shared = keys
            .iter()
            .find(|b| b.folder == "shared-with-me")
            .expect("the shared set resolves");
        assert_eq!(
            shared.content_keys.as_ref().map(|k| k.current_version()),
            Some(1),
            "the custody generation reaches the batch"
        );
    }

    /// An unreadable custody fails the whole batch — never an empty custody
    /// that would resolve every bound set keyless as though that were the truth.
    #[test]
    fn an_unreadable_custody_fails_the_batch() {
        struct Unreadable;
        #[async_trait::async_trait]
        impl FolderKeyReader for Unreadable {
            async fn load(&self) -> anyhow::Result<FoldersConfig> {
                anyhow::bail!("no runtime")
            }
        }
        let err = block_on(resolve_engine_keys(&Unreadable, OWN, RosterNest::default()))
            .expect_err("custody unreadable");
        assert!(matches!(err, EngineKeysError::CustodyLoad(_)), "{err}");
    }

    /// The regression pin (2026-07-22): the producer must ask for the
    /// **member-visible** projection. Asking owner-scoped omitted every
    /// shared set from the pushed blob, so a writer member's agent-side
    /// `engine_keys_for` found no entry and ran the engine unbound — sealing
    /// and opening under the member's own `BackupKey` instead of the set's M2
    /// content key (`aead::Error` on the owner's uploads).
    #[test]
    fn member_visible_projection_is_requested() {
        let (nest, _) = compute();
        let recorded = nest
            .list_request
            .lock()
            .unwrap()
            .clone()
            .expect("the producer listed folders");
        let req: FoldersListRequest =
            fauna_protocol::decode_strict(&recorded).expect("request decodes");
        assert_eq!(
            req.include_shared_with_me,
            Some(true),
            "a writer member binds a set it does not own; the owner-scoped list \
             would omit it from the pushed blob and run its engine unbound"
        );
    }

    /// The consequence the projection buys: the shared set reaches the agent as
    /// a **bound** entry. `BoundKeysMissing` (`Some(gid)` + `None` keys) here —
    /// the custody is empty, so it holds no generation yet — is
    /// the fail-closed shape; what must never happen is the set being absent or
    /// carrying `(None, None)`, which is the silent owner-key path.
    #[test]
    fn a_shared_set_reaches_the_blob_bound_not_absent() {
        let (_, decoded) = compute();
        let shared = decoded
            .iter()
            .find(|b| b.folder == "shared-with-me")
            .expect("the shared set must be in the pushed blob");
        assert_eq!(
            shared.mls_group_id.as_deref(),
            Some(SHARED_GROUP_ID.as_slice()),
            "a shared set must carry its group binding so the engine fails \
             closed rather than sealing under the member's own BackupKey"
        );
        assert!(
            shared.content_keys.is_none(),
            "empty custody in this fixture ⇒ BoundKeysMissing, the fail-closed arm"
        );

        let owned = decoded
            .iter()
            .find(|b| b.folder == "my-own")
            .expect("owned sets stay in the blob");
        assert_eq!(
            (owned.mls_group_id.as_deref(), owned.content_keys.is_none()),
            (None, true),
            "an owner-only set is genuinely unbound — the owner-key path is \
             correct for it"
        );
    }

    /// A holder with no cross-nest shares must produce a byte-identical blob to
    /// the pre-Phase-4 one — the union is additive, never a same-nest change.
    #[test]
    fn no_foreign_sets_leaves_the_same_nest_batch_untouched() {
        let same_nest = vec![FolderEngineKeys {
            folder: "my-own".into(),
            ..Default::default()
        }];
        let cfg = FoldersConfig::default();
        assert_eq!(union_foreign_bindings(same_nest.clone(), &cfg), same_nest);
    }

    /// The union's reason to exist: a cross-nest set reaches the agent even
    /// though no nest projection can name it.
    #[test]
    fn a_foreign_set_is_appended_to_the_nest_projected_batch() {
        let mut cfg = FoldersConfig::default();
        cfg.foreign_sets.push(fauna_core::data::ForeignFolder {
            channel_id: [0xa1; 32],
            mls_group_id: b"foreign-gid".to_vec(),
            home_nest_url: "https://home.example".into(),
            home_nest_actor_id: Some("cd".repeat(32)),
            set_name: Some("xnest-docs".into()),
            access: Some("writer".into()),
            content_key_floor: None,
            ..Default::default()
        });

        let out = union_foreign_bindings(
            vec![FolderEngineKeys {
                folder: "my-own".into(),
                ..Default::default()
            }],
            &cfg,
        );
        assert_eq!(out.len(), 2);
        assert_eq!(out[0].folder, "my-own", "same-nest rows keep their order");
        assert_eq!(out[1].folder, "xnest-docs");
        assert_eq!(
            out[1].foreign_routing(),
            Some(("https://home.example".to_string(), hex::encode([0xa1; 32])))
        );
        // The byte-plane trust root flows from the custody record onto the
        // pushed blob so the agent can graduate the home-nest pin for a set
        // with no local row.
        assert_eq!(
            out[1].home_nest_actor_id.as_deref(),
            Some("cd".repeat(32).as_str())
        );
    }

    /// A set that somehow appears in BOTH planes must not be pushed twice — the
    /// agent's `.find` would take one arbitrarily. The nest's own row wins,
    /// because it is what the rest of the same-nest seam resolves against.
    #[test]
    fn a_group_already_in_the_nest_projection_is_not_duplicated_by_the_foreign_pass() {
        let gid = b"same-group".to_vec();
        let mut cfg = FoldersConfig::default();
        cfg.foreign_sets.push(fauna_core::data::ForeignFolder {
            channel_id: [0xb2; 32],
            mls_group_id: gid.clone(),
            home_nest_url: "https://home.example".into(),
            set_name: Some("shared-with-me".into()),
            access: Some("writer".into()),
            home_nest_actor_id: None,
            content_key_floor: None,
            ..Default::default()
        });

        let out = union_foreign_bindings(
            vec![FolderEngineKeys {
                folder: "shared-with-me".into(),
                mls_group_id: Some(gid),
                ..Default::default()
            }],
            &cfg,
        );
        assert_eq!(out.len(), 1, "the nest-projected row is the one that ships");
        assert_eq!(
            out[0].foreign_routing(),
            None,
            "a set this nest claims must stay same-nest-routed"
        );
    }

    /// Two DIFFERENT sets sharing a name both ship (dropping one would silently
    /// unbind a working folder). The name collision is the documented residual;
    /// what this pins is that the union never resolves it by discarding data.
    #[test]
    fn an_owned_and_a_foreign_set_sharing_a_name_both_ship() {
        let mut cfg = FoldersConfig::default();
        cfg.foreign_sets.push(fauna_core::data::ForeignFolder {
            channel_id: [0xc3; 32],
            mls_group_id: b"foreign-gid".to_vec(),
            home_nest_url: "https://home.example".into(),
            set_name: Some("docs".into()),
            access: Some("writer".into()),
            home_nest_actor_id: None,
            content_key_floor: None,
            ..Default::default()
        });

        let out = union_foreign_bindings(
            vec![FolderEngineKeys {
                folder: "docs".into(),
                ..Default::default()
            }],
            &cfg,
        );
        assert_eq!(out.len(), 2, "neither entry may be silently dropped");
        assert_eq!(out[0].foreign_routing(), None);
        assert!(out[1].foreign_routing().is_some());
    }
}
