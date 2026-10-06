//! The **`WebdavKeysBlob` reconciler** — (re-)provision the actor's MSEK-sealed
//! served-set content-key blob from the current served-set flags + custody
//! (`docs/goal/behavior/webdav-server.md` § Key model: "the client re-provisions
//! on any served-set or generation change").
//!
//! ## Reconcile, not incrementally patch
//!
//! The blob is a **single per-actor value** the nest stores opaque
//! (`bridge_webdav_keys_blobs`, atomic replace) — so the correct provisioning
//! shape is *desired-state reconcile*: read the served flags
//! (`fauna.folders.list`), the MSEK (`fauna.state.mail`) and the custody (the account
//! plane's `fauna.state.folder-keys`, through a
//! [`FolderKeyReader`](crate::FolderKeyReader)), build the full
//! plaintext, seal, provision. Every serve-ON / serve-OFF / rotation calls this
//! **after** its custody + flag writes; any crash in between leaves the set
//! served-but-blobless (or blob-still-holding-rotated-out-keys for at most one
//! reconcile), and the next call heals — the MDA fails closed on a set the blob
//! lacks, never wider than the flags.
//!
//! An **empty** served list still provisions (an empty blob replaces the old
//! one) — revocation completeness: unflagging the last served set must not
//! leave the previous blob's keys standing.
//!
//! ## Placement (`mls` feature)
//!
//! Sealing needs `fauna_mls::wrapped_blob::seal_webdav_keys_blob` + the MSEK
//! (the account's mail custody, `fauna.state.mail`), so this module is `mls`-gated like
//! [`crate::engine_binding`] — the base crate stays wasm-light/fauna-mls-free,
//! and the per-app glue (FFI / wasm boundary) calls
//! [`reconcile_webdav_keys_blob`] right after
//! [`FoldersAuthor::serve_enable`](crate::orchestration::FoldersAuthor::serve_enable) /
//! [`serve_disable`](crate::orchestration::FoldersAuthor::serve_disable) (and
//! after a member-removal rotation of a served set).

use fauna_client_config::{MailStore, StoreError};
use fauna_core::data::MailConfig;
use fauna_mls::wrapped_blob::{ServedSetKeys, WebdavKeysPlaintext, seal_webdav_keys_blob};
use fauna_protocol::wrapped_blob::{ProvisionReply, ProvisionWebdavKeysBlobRequest};
use fauna_protocol::{ByteBuf, RpcErrorClass, RpcRequester};
use zeroize::Zeroizing;

use crate::FoldersClient;
use crate::engine_binding::{custody_served, owned_custody_channel};
use crate::principal_grants::PrincipalFolderGrants;

/// Whether this owner can serve **any** set over WebDAV — i.e. holds the MSEK
/// [`reconcile_webdav_keys_blob`] seals the `WebdavKeysBlob` under.
///
/// This is the predicate the `folder-webdav-toggle` is gated on
/// (`webdav-server.md` § Independent enablement point 2): an actor with no mail
/// credential sees the toggle **disabled with a "set up mail first" hint**
/// rather than clicking it and collecting a [`WebdavProvisionError::NoMsek`].
/// That is not merely cosmetic — [`FoldersAuthor::serve_set`] flips the nest
/// flag *before* it reconciles, so a doomed enable would commit the flag and
/// then fail, leaving the set served-but-blobless until the next reconcile
/// heals it (fail-closed, but a half-state worth never entering).
///
/// It lives here, next to the error it prevents and reading the same field
/// (the mail custody's `msek`), so the two cannot drift — see the
/// `can_serve_webdav_agrees_with_reconcile` test.
///
/// [`FoldersAuthor::serve_set`]: crate::orchestration::FoldersAuthor::serve_set
pub fn can_serve_webdav(mail: &MailConfig) -> bool {
    mail.msek.is_some()
}

/// Read the owner's mail custody and answer [`can_serve_webdav`] — the one
/// composition every app face calls to gate `folder-webdav-toggle`, so the
/// six apps share the capability query rather than each re-deriving it
/// (priority #2).
///
/// Deliberately takes only the mail custody — **no MLS engine, no
/// conversations session** — unlike
/// [`FoldersAuthor::serve_set`](crate::orchestration::FoldersAuthor::serve_set),
/// which needs the rail to rotate content keys. A page must be able to ask
/// "may I offer this control?" as soon as the account runtime exists; making
/// the question depend on the rail that *performs* the action would fail the
/// query spuriously on a page that has not wired the rail yet, and a failed
/// capability read is indistinguishable from "cannot serve".
pub async fn owner_can_serve_webdav(mail: &dyn MailStore) -> Result<bool, StoreError> {
    Ok(can_serve_webdav(&mail.load().await?))
}

/// A failure from [`reconcile_webdav_keys_blob`], generic over the transport
/// error `E`.
#[derive(Debug)]
pub enum WebdavProvisionError<E> {
    /// `fauna.folders.list` or the provision call failed at the transport.
    Transport(E),
    /// Reading the folder-key custody (`fauna.state.folder-keys`) failed.
    Custody(anyhow::Error),
    /// Reading the owner's mail custody (the MSEK) failed.
    Mail(StoreError),
    /// The owner holds no MSEK (the mail custody's `msek`) — there is no DAV
    /// credential without one (MUA AUTH unwraps MSEK), so nothing can be sealed.
    NoMsek,
    /// A served set (named) holds no content keys at its custody channel — an
    /// unsynced custody or a serve-enable race. The blob is withheld rather
    /// than provisioned without a served set's keys (the MDA would serve that
    /// set keyless while the stale blob's other keys stand — reconcile again
    /// after the custody syncs).
    ServedSetKeysMissing(String),
    /// A served set's `mls_group_id` projection is malformed hex (nest-side
    /// corruption) — indeterminate custody identity, fail closed.
    MalformedGroupId(hex::FromHexError),
    /// Sealing or encoding the blob failed (practically unreachable for
    /// fixed-shape inputs).
    Seal(String),
}

impl<E: core::fmt::Display> core::fmt::Display for WebdavProvisionError<E> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::Transport(e) => write!(f, "webdav-keys provision transport: {e}"),
            Self::Custody(e) => write!(f, "webdav-keys provision custody: {e:#}"),
            Self::Mail(e) => write!(f, "webdav-keys provision mail custody: {e}"),
            Self::NoMsek => write!(f, "no MSEK held — cannot seal the webdav keys blob"),
            Self::ServedSetKeysMissing(name) => write!(
                f,
                "served folder {name:?} holds no content keys at its custody channel"
            ),
            Self::MalformedGroupId(e) => write!(f, "malformed mls_group_id projection: {e}"),
            Self::Seal(e) => write!(f, "webdav-keys blob seal: {e}"),
        }
    }
}

impl<E: core::fmt::Display + core::fmt::Debug> std::error::Error for WebdavProvisionError<E> {}

/// Build + seal + provision the actor's `WebdavKeysBlob` from custody —
/// **the sets the owner-scoped list names AND custody calls served**
/// (`writer-signed-change-records.md` ruling (7)(b)(ii) rule (2): owned ∩
/// [`custody_served`], never a member entry; the nest's `webdav_enabled`
/// selects nothing, so a nest flagging a set served gets no key for it).
/// Returns the number of served sets carried.
///
/// Custody is read as a write reads it
/// ([`crate::key_reader::FolderKeyStore::load_for_write`] — a fresh fetch, so
/// a lagging device provisions the plane's state, not its cache). Per served
/// set the custody identity comes from the one shared resolution
/// ([`owned_custody_channel`] — the derived `ChannelId` for a shared set, the
/// serve pseudo-channel for an unshared one) and the **full** generation
/// history rides the blob (`read_only = false` for
/// v1 — folders serve read-write; the read-only browse of backup sets is
/// a recorded deferral). Sealed under MSEK (`fauna_mls::wrapped_blob`,
/// `for_webdav_keys` AAD) and stored opaque via
/// `fauna.bridges.provision_webdav_keys_blob` (idempotent atomic replace).
///
/// **The same reconcile renews the principals' folder read twins** over every
/// served set (`webdav-server.md` § Key model → *A principal's read* rule
/// (4)) when `principals` is wired: each live twin is re-sent the set's full
/// generation bundle at its recorded window end
/// ([`PrincipalFolderGrants::renew_over`]), so a rotation reaches the
/// principal as it reaches the MDA. The renew needs no MSEK, so it runs on
/// every device before the seal; a failed renew is logged and retried by the
/// next reconcile (the nest dedups), never a reason to withhold the blob.
pub async fn reconcile_webdav_keys_blob<R>(
    files: &FoldersClient<R>,
    owner: &fauna_core::identity::ActorId,
    custody_store: &dyn crate::key_reader::FolderKeyStore,
    mail: &dyn MailStore,
    principals: Option<&PrincipalFolderGrants>,
) -> Result<usize, WebdavProvisionError<R::Error>>
where
    R: RpcRequester,
    R::Error: RpcErrorClass,
{
    let listed = files
        .list_wire()
        .await
        .map_err(WebdavProvisionError::Transport)?
        .folders;
    let custody = custody_store
        .load_for_write()
        .await
        .map_err(WebdavProvisionError::Custody)?;
    // Named from custody by hash: a served set's blob entry and its serve
    // custody channel are both keyed by the set's name, and a sealed row rests
    // none — the rendered list on a client without label custody dropped every
    // sealed served set, so the blob carried no keys for it.
    let summaries = crate::custody::named_from_custody(listed, &custody);

    let mut served_sets = Vec::new();
    let mut served_channels = Vec::new();
    for summary in summaries
        .iter()
        .filter(|s| s.role.as_deref() != Some("member") && custody_served(s, &custody))
    {
        let channel =
            owned_custody_channel(summary).map_err(WebdavProvisionError::MalformedGroupId)?;
        let keys = crate::custody::content_keys(&custody, &channel)
            .ok_or_else(|| WebdavProvisionError::ServedSetKeysMissing(summary.name.clone()))?;
        served_sets.push(ServedSetKeys {
            set_name: summary.name.clone(),
            read_only: false,
            keys,
        });
        served_channels.push((summary.name.as_str(), channel));
    }
    let count = served_sets.len();

    if let Some(principals) = principals {
        let now_secs = fauna_core::data::Timestamp::now().0 / 1_000_000;
        for (name, channel) in &served_channels {
            if let Err(e) = principals
                .renew_over(files.requester(), &custody, name, channel, now_secs)
                .await
            {
                tracing::warn!(
                    folder = %fauna_core::log_redact::log_folder_name(name),
                    "folders: renewing a principal's folder grant failed (the next reconcile \
                     retries): {e}"
                );
            }
        }
    }

    let mail = mail.load().await.map_err(WebdavProvisionError::Mail)?;
    let msek = mail.msek.as_ref().ok_or(WebdavProvisionError::NoMsek)?;

    let plaintext = Zeroizing::new(
        WebdavKeysPlaintext::new(served_sets)
            .to_canonical_bytes()
            .map_err(|e| WebdavProvisionError::Seal(e.to_string()))?,
    );
    // The seal's owner is the session identity.
    let blob = seal_webdav_keys_blob(&plaintext, &owner.0, msek)
        .map_err(|e| WebdavProvisionError::Seal(e.to_string()))?
        .to_canonical_bytes()
        .map_err(|e| WebdavProvisionError::Seal(e.to_string()))?;

    let _: ProvisionReply = files
        .requester()
        .request(
            "fauna.bridges.provision_webdav_keys_blob",
            ProvisionWebdavKeysBlobRequest {
                blob: ByteBuf::from(blob),
                extra: Default::default(),
            },
        )
        .await
        .map_err(WebdavProvisionError::Transport)?;
    Ok(count)
}

/// What custody says the blob should carry, as far as custody alone can say:
/// every live entry whose channel custody calls served — its channel, its
/// name and its keys. The follower compares it across notices, so a custody
/// change that moves no serve window and no served set's keys (most
/// fleet-scope writes: a device row, a mail edit, an unserved set's rotation)
/// provisions nothing.
type ServedProjection = Vec<(
    [u8; 32],
    Option<String>,
    Option<fauna_core::folder_keys::FolderContentKeys>,
)>;

fn served_projection(cfg: &fauna_core::data::FoldersConfig) -> ServedProjection {
    let mut served: ServedProjection = cfg
        .sets
        .iter()
        .filter(|s| s.is_live())
        .filter_map(|s| {
            let channel = s.channel_id?;
            crate::custody::channel_served(cfg, &channel)
                .then(|| (channel, s.name.clone(), s.keys.clone()))
        })
        .collect();
    served.sort_by(|a, b| (a.0, &a.1).cmp(&(b.0, &b.1)));
    served
}

/// One [`ServedBlobFollower::on_change`] outcome.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FollowStep {
    /// Custody's served projection is what the blob was last reconciled to.
    Unchanged,
    /// The blob was re-provisioned; the count of served sets it carries.
    Provisioned(usize),
    /// This device holds no MSEK — nothing to seal, nothing owed.
    NoMsek,
    /// The read or the reconcile failed; the next notice retries.
    Failed,
}

/// **The blob reconcile's re-run on the custody nudge**
/// (`writer-signed-change-records.md` ruling (7)(b)(ii) rule (3)): on an
/// MSEK-holding device, a sibling's serve flip that reaches this device's
/// custody re-provisions the `WebdavKeysBlob` without waiting for the next
/// launch. The ONE shared follower every MSEK-holding host runs beside its
/// launch pass — the native launch hook (`FolderRemovalResume`) and web's
/// launch face — never wired per app.
///
/// It follows custody, not the push: its wake is the store's
/// [`FolderKeyStore::change_notices`](crate::FolderKeyStore::change_notices),
/// which fires once the walk the fleet-scope nudge started has landed the
/// sibling's row — whichever process on the box walked — so the re-read never
/// races that walk. Each notice re-reads custody and reconciles only when the
/// served projection moved (sources coalesce, so a burst costs one
/// reconcile); a failed reconcile leaves the projection owed and the next
/// notice retries.
pub struct ServedBlobFollower<R: RpcRequester> {
    files: FoldersClient<R>,
    owner: fauna_core::identity::ActorId,
    custody: std::sync::Arc<dyn crate::key_reader::FolderKeyStore>,
    mail: std::sync::Arc<dyn MailStore>,
    /// The principals' folder read twins the reconcile renews
    /// ([`Self::with_principal_grants`]); `None` renews none.
    principals: Option<PrincipalFolderGrants>,
    /// The projection the blob was last reconciled to (or the launch pass is
    /// about to reconcile it to); `None` when custody could not be read.
    reconciled: Option<ServedProjection>,
}

impl<R> ServedBlobFollower<R>
where
    R: RpcRequester,
    R::Error: RpcErrorClass,
{
    /// Seed from custody as it reads now. A host seeds BEFORE its launch
    /// pass's own reconcile, so a change landing between that reconcile and
    /// the follower's first wait is still a change.
    pub async fn new(
        files: FoldersClient<R>,
        owner: fauna_core::identity::ActorId,
        custody: std::sync::Arc<dyn crate::key_reader::FolderKeyStore>,
        mail: std::sync::Arc<dyn MailStore>,
    ) -> Self {
        let reconciled = custody.load().await.ok().map(|c| served_projection(&c));
        Self {
            files,
            owner,
            custody,
            mail,
            principals: None,
            reconciled,
        }
    }

    /// Renew the principals' folder read twins on every reconcile this
    /// follower runs (rule (4): the reconcile "re-run on the custody nudge").
    #[must_use]
    pub fn with_principal_grants(mut self, principals: PrincipalFolderGrants) -> Self {
        self.principals = Some(principals);
        self
    }

    /// Custody may have changed: re-read it, and reconcile the blob when the
    /// served projection moved.
    pub async fn on_change(&mut self) -> FollowStep {
        let projection = match self.custody.load().await {
            Ok(custody) => served_projection(&custody),
            Err(e) => {
                tracing::debug!("folders: served-blob follower could not read custody: {e:#}");
                return FollowStep::Failed;
            }
        };
        if self.reconciled.as_ref() == Some(&projection) {
            return FollowStep::Unchanged;
        }
        let step = match reconcile_webdav_keys_blob(
            &self.files,
            &self.owner,
            &*self.custody,
            &*self.mail,
            self.principals.as_ref(),
        )
        .await
        {
            Ok(served) => FollowStep::Provisioned(served),
            Err(WebdavProvisionError::NoMsek) => FollowStep::NoMsek,
            Err(e) => {
                tracing::warn!(
                    "folders: re-provisioning the webdav keys blob on a custody change failed \
                     (the next change or launch retries): {e}"
                );
                return FollowStep::Failed;
            }
        };
        self.reconciled = Some(projection);
        step
    }

    /// Follow `notices` until the source ends (the runtime is gone).
    pub async fn run(mut self, mut notices: Box<dyn crate::key_reader::CustodyNotices>) {
        while notices.changed().await {
            if let FollowStep::Provisioned(n) = self.on_change().await {
                tracing::info!(
                    "folders: re-provisioned the webdav keys blob after a custody change \
                     ({n} served set(s))"
                );
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fauna_client_testkit::ClassifiedError;
    use fauna_client_testkit::block_on;
    use fauna_core::folder_keys::serve_custody_channel_id;
    use fauna_core::identity::ActorKeypair;
    use fauna_mls::types::ChannelId;
    use fauna_mls::wrapped_blob::{WebdavKeysBlob, unseal_webdav_keys_blob};
    use fauna_protocol::folders::{FolderSummary, FoldersListReply};
    use std::sync::{Arc, Mutex};

    const MSEK: [u8; 32] = [0x77; 32];

    /// In-memory nest: a fixed folder list + the provision arm capturing the
    /// sealed blob.
    #[derive(Default)]
    struct FakeNest {
        summaries: Mutex<Vec<FolderSummary>>,
        provisioned: Mutex<Vec<Vec<u8>>>,
    }
    impl RpcRequester for FakeNest {
        type Error = ClassifiedError;
        async fn request<Req, Reply>(
            &self,
            kind: &'static str,
            payload: Req,
        ) -> Result<Reply, Self::Error>
        where
            Req: serde::Serialize,
            Reply: serde::de::DeserializeOwned,
        {
            let bytes = fauna_protocol::encode_canonical(&payload).expect("encode");
            let out: Vec<u8> = match kind {
                "fauna.folders.list" => fauna_protocol::encode_canonical(&FoldersListReply {
                    folders: self.summaries.lock().unwrap().clone(),
                    extra: Default::default(),
                })
                .unwrap()
                .to_vec(),
                "fauna.bridges.provision_webdav_keys_blob" => {
                    let req: ProvisionWebdavKeysBlobRequest =
                        fauna_protocol::decode_strict(&bytes).expect("decode provision");
                    self.provisioned.lock().unwrap().push(req.blob.into_vec());
                    fauna_protocol::encode_canonical(&ProvisionReply {
                        ok: true,
                        extra: Default::default(),
                    })
                    .unwrap()
                    .to_vec()
                }
                other => panic!("FakeNest: unexpected kind {other}"),
            };
            Ok(fauna_protocol::decode_strict(&out).expect("decode reply"))
        }
    }

    fn summary(name: &str, served: bool, mls_group_id: Option<&[u8]>) -> FolderSummary {
        FolderSummary {
            id: 1,
            name: name.into(),
            retention_policy: None,
            cached_snapshot_count: 0,
            cached_total_bytes: 0,
            cached_last_snapshot_at: None,
            include_paths: None,
            exclude_paths: None,
            mls_group_id: mls_group_id.map(hex::encode),
            role: None,
            owner_handle: None,
            webdav_enabled: served,
            ..Default::default()
        }
    }

    struct Harness {
        nest: Arc<FakeNest>,
        files: FoldersClient<Arc<FakeNest>>,
        owner: fauna_core::identity::ActorId,
        custody: crate::key_reader::MemoryFolderKeyStore,
        mail: fauna_client_config::test_helpers::FakeMailStore,
    }

    fn harness(summaries: Vec<FolderSummary>, msek: Option<[u8; 32]>) -> Harness {
        let nest = Arc::new(FakeNest::default());
        *nest.summaries.lock().unwrap() = summaries;
        let owner = ActorKeypair::from_secret([0xA0; 32]).actor_id();
        let mail =
            fauna_client_config::test_helpers::FakeMailStore::with(&fauna_core::data::MailConfig {
                msek: msek.map(Into::into),
                ..Default::default()
            });
        Harness {
            files: FoldersClient::new(nest.clone()),
            owner,
            custody: Default::default(),
            mail,
            nest,
        }
    }

    /// Custody keyed at `channel` and SERVED by the owner's word.
    fn seed_custody(h: &Harness, channel: [u8; 32], key: [u8; 32]) {
        block_on(crate::key_reader::update(&h.custody, |cfg| {
            crate::custody::record_new_set(cfg, channel, key, 1_000);
            crate::custody::serve_on(cfg, &channel, 1_000);
        }))
        .expect("seed custody");
    }

    /// Custody keyed at `channel` and never served (a bind, a paywall).
    fn seed_unserved_custody(h: &Harness, channel: [u8; 32], key: [u8; 32]) {
        block_on(crate::key_reader::update(&h.custody, |cfg| {
            crate::custody::record_new_set(cfg, channel, key, 1_000);
        }))
        .expect("seed custody");
    }

    /// Ruling (7)(b)(ii): the nest's flag selects nothing. A lying nest
    /// flagging served a SHARED set the owner never served, and an unshared
    /// set keyed by a paywall, gets no key for either; and a set shared WITH
    /// this account, served by its owner, never rides this account's blob.
    #[test]
    fn a_nest_flagging_an_unserved_set_served_gets_no_keys() {
        let shared = b"a-shared-set-never-served".to_vec();
        let theirs = b"a-set-shared-with-this-account".to_vec();
        let mut member_row = summary("theirs", true, Some(&theirs));
        member_row.role = Some("member".into());
        let h = harness(
            vec![
                summary("shared", true, Some(&shared)),
                summary("paywalled", true, None),
                member_row,
                summary("served", true, None),
            ],
            Some(MSEK),
        );
        seed_unserved_custody(&h, ChannelId::from_group_id(&shared).0, [0x33; 32]);
        seed_unserved_custody(&h, serve_custody_channel_id("paywalled"), [0x44; 32]);
        // The member's received copy carries the owner's serve stamps.
        seed_custody(&h, ChannelId::from_group_id(&theirs).0, [0x55; 32]);
        seed_custody(&h, serve_custody_channel_id("served"), [0x11; 32]);

        let n = block_on(reconcile_webdav_keys_blob(
            &h.files, &h.owner, &h.custody, &h.mail, None,
        ))
        .expect("reconcile");
        assert_eq!(n, 1);
        let pt = unseal_last(&h);
        assert_eq!(
            pt.served_sets
                .iter()
                .map(|s| s.set_name.as_str())
                .collect::<Vec<_>>(),
            vec!["served"]
        );
        for key in [[0x33; 32], [0x44; 32], [0x55; 32]] {
            assert!(
                !pt.served_sets
                    .iter()
                    .any(|s| s.keys.generations().any(|g| g.key == key)),
                "a set custody does not call served rides no key"
            );
        }
    }

    /// The owner served the set and the nest says it is off (a serve-on that
    /// died before the flag, a nest lying "off"): the blob still follows
    /// custody — the launch pass's unserve arm is what reverts it.
    #[test]
    fn the_blob_follows_custody_when_the_nest_says_off() {
        let h = harness(vec![summary("docs", false, None)], Some(MSEK));
        seed_custody(&h, serve_custody_channel_id("docs"), [0x11; 32]);
        let n = block_on(reconcile_webdav_keys_blob(
            &h.files, &h.owner, &h.custody, &h.mail, None,
        ))
        .expect("reconcile");
        assert_eq!(n, 1);
    }

    /// Unseal the last-provisioned blob with `MSEK` and decode the plaintext.
    fn unseal_last(h: &Harness) -> WebdavKeysPlaintext {
        let blobs = h.nest.provisioned.lock().unwrap();
        let blob = WebdavKeysBlob::from_canonical_bytes(blobs.last().expect("provisioned"))
            .expect("decode blob");
        let pt = unseal_webdav_keys_blob(&blob, &MSEK).expect("unseal");
        WebdavKeysPlaintext::from_canonical_bytes(&pt).expect("decode plaintext")
    }

    #[test]
    fn reconcile_carries_served_sets_and_excludes_unserved() {
        let raw_gid = b"raw-openmls-group-id".to_vec();
        let raw_gid_unserved = b"another-raw-group-id-unserved".to_vec();
        let h = harness(
            vec![
                summary("served-unshared", true, None),
                summary("served-shared", true, Some(&raw_gid)),
                summary("plain", false, None),
                // Shared (custody-holding) but NOT flag-ON: its keys must never
                // enter the blob (the nest cannot
                // see inside the blob).
                summary("shared-unserved", false, Some(&raw_gid_unserved)),
            ],
            Some(MSEK),
        );
        seed_custody(&h, serve_custody_channel_id("served-unshared"), [0x11; 32]);
        seed_custody(&h, ChannelId::from_group_id(&raw_gid).0, [0x22; 32]);
        seed_unserved_custody(
            &h,
            ChannelId::from_group_id(&raw_gid_unserved).0,
            [0x33; 32],
        );

        let n = block_on(reconcile_webdav_keys_blob(
            &h.files, &h.owner, &h.custody, &h.mail, None,
        ))
        .expect("reconcile");
        assert_eq!(n, 2);

        let pt = unseal_last(&h);
        assert_eq!(pt.served_sets.len(), 2);
        let by_name = |n: &str| {
            pt.served_sets
                .iter()
                .find(|s| s.set_name == n)
                .unwrap_or_else(|| panic!("{n} in blob"))
        };
        // The blob's generations are exactly the custody generations — what the
        // MDA decrypts historical + current files with.
        assert_eq!(by_name("served-unshared").keys.current_key(), &[0x11; 32]);
        assert_eq!(by_name("served-shared").keys.current_key(), &[0x22; 32]);
        assert!(
            !by_name("served-unshared").read_only,
            "v1 serves read-write"
        );
        assert!(!pt.served_sets.iter().any(|s| s.set_name == "plain"));
        // The blast-radius bound: custody exists for "shared-unserved", but its
        // flag is OFF, so neither its name nor its keys may appear anywhere in
        // the blob (an inclusion here would widen the MDA past rule #7,
        // undetectable server-side).
        assert!(
            !pt.served_sets
                .iter()
                .any(|s| s.set_name == "shared-unserved")
        );
        assert!(
            !pt.served_sets
                .iter()
                .any(|s| s.keys.generations().any(|g| g.key == [0x33; 32])),
            "an unserved set's key material never rides the blob"
        );
    }

    #[test]
    fn reconcile_after_rotation_carries_the_full_history() {
        let h = harness(vec![summary("docs", true, None)], Some(MSEK));
        let chan = serve_custody_channel_id("docs");
        seed_custody(&h, chan, [0x11; 32]);
        block_on(crate::key_reader::update(&h.custody, |cfg| {
            crate::custody::rotate_set(cfg, &chan, [0x22; 32], 2_000);
        }))
        .expect("rotate");

        block_on(reconcile_webdav_keys_blob(
            &h.files, &h.owner, &h.custody, &h.mail, None,
        ))
        .expect("reconcile");
        let pt = unseal_last(&h);
        let keys = &pt.served_sets[0].keys;
        assert_eq!(keys.current_key(), &[0x22; 32]);
        assert_eq!(
            keys.key_for(1),
            Some(&[0x11; 32]),
            "historical generation rides the blob (back-catalogue decrypts)"
        );
    }

    #[test]
    fn reconcile_with_no_served_sets_provisions_an_empty_blob() {
        // Revocation completeness: unflagging the last set must REPLACE the old
        // blob, not leave its keys standing.
        let h = harness(vec![summary("plain", false, None)], Some(MSEK));
        let n = block_on(reconcile_webdav_keys_blob(
            &h.files, &h.owner, &h.custody, &h.mail, None,
        ))
        .expect("reconcile");
        assert_eq!(n, 0);
        assert_eq!(unseal_last(&h).served_sets.len(), 0);
    }

    #[test]
    fn reconcile_withholds_the_blob_when_a_served_set_lacks_custody() {
        let h = harness(vec![summary("docs", true, None)], Some(MSEK));
        // The serve stamp reached this device before the generations did.
        block_on(crate::key_reader::update(&h.custody, |cfg| {
            cfg.sets.push(fauna_core::data::FolderKeyCustody {
                channel_id: Some(serve_custody_channel_id("docs")),
                served_at: Some(1_000),
                ..Default::default()
            });
        }))
        .expect("seed custody");
        let err = block_on(reconcile_webdav_keys_blob(
            &h.files, &h.owner, &h.custody, &h.mail, None,
        ))
        .unwrap_err();
        assert!(
            matches!(err, WebdavProvisionError::ServedSetKeysMissing(ref n) if n == "docs"),
            "got {err:?}"
        );
        assert!(h.nest.provisioned.lock().unwrap().is_empty(), "withheld");
    }

    #[test]
    fn reconcile_without_msek_errors() {
        let h = harness(vec![summary("docs", true, None)], None);
        seed_custody(&h, serve_custody_channel_id("docs"), [0x11; 32]);
        let err = block_on(reconcile_webdav_keys_blob(
            &h.files, &h.owner, &h.custody, &h.mail, None,
        ))
        .unwrap_err();
        assert!(matches!(err, WebdavProvisionError::NoMsek));
        assert!(h.nest.provisioned.lock().unwrap().is_empty());
    }

    /// The anti-drift proof for the `folder-webdav-toggle` disable-with-hint
    /// (`webdav-server.md` § Independent enablement point 2): the predicate the
    /// UI gates the toggle on must be *exactly* the condition under which the
    /// reconcile fails `NoMsek`. If a later change moves the MSEK or adds a
    /// second serving precondition to only one of the two, this test goes red
    /// rather than the UI silently re-offering a toggle that cannot succeed.
    #[test]
    fn can_serve_webdav_agrees_with_reconcile() {
        // No MSEK → predicate says "cannot", and the reconcile indeed fails NoMsek.
        let without = harness(vec![summary("docs", true, None)], None);
        seed_custody(&without, serve_custody_channel_id("docs"), [0x11; 32]);
        assert!(
            !can_serve_webdav(&without.mail.current()),
            "no msek → cannot serve"
        );
        assert!(
            !block_on(owner_can_serve_webdav(&without.mail)).expect("capability read"),
            "the async composition the client faces call agrees"
        );
        assert!(matches!(
            block_on(reconcile_webdav_keys_blob(
                &without.files,
                &without.owner,
                &without.custody,
                &without.mail,
                None
            ))
            .unwrap_err(),
            WebdavProvisionError::NoMsek
        ));

        // MSEK held → predicate says "can", and the reconcile indeed succeeds.
        let with = harness(vec![summary("docs", true, None)], Some(MSEK));
        seed_custody(&with, serve_custody_channel_id("docs"), [0x11; 32]);
        assert!(
            can_serve_webdav(&with.mail.current()),
            "msek held → can serve"
        );
        assert!(
            block_on(owner_can_serve_webdav(&with.mail)).expect("capability read"),
            "the async composition the client faces call agrees"
        );
        assert_eq!(
            block_on(reconcile_webdav_keys_blob(
                &with.files,
                &with.owner,
                &with.custody,
                &with.mail,
                None
            ))
            .expect("reconciles"),
            1
        );
    }

    /// A notice source that yields `n` notices, then ends.
    struct Notices(usize);
    #[cfg_attr(not(target_arch = "wasm32"), async_trait::async_trait)]
    #[cfg_attr(target_arch = "wasm32", async_trait::async_trait(?Send))]
    impl crate::key_reader::CustodyNotices for Notices {
        async fn changed(&mut self) -> bool {
            let more = self.0 > 0;
            self.0 = self.0.saturating_sub(1);
            more
        }
    }

    fn follower(
        h: &Harness,
        custody: Arc<crate::key_reader::MemoryFolderKeyStore>,
    ) -> ServedBlobFollower<Arc<FakeNest>> {
        block_on(ServedBlobFollower::new(
            FoldersClient::new(h.nest.clone()),
            h.owner,
            custody,
            Arc::new(h.mail.clone()),
        ))
    }

    fn served_names(h: &Harness) -> Vec<String> {
        unseal_last(h)
            .served_sets
            .iter()
            .map(|s| s.set_name.clone())
            .collect()
    }

    /// Ruling (7)(b)(ii) rule (3), the custody-nudge re-run: a sibling's
    /// serve-on lands in this device's custody (the set's stamps move, the
    /// nest's roster row does not) and the follower re-provisions the blob
    /// with the set — no launch in between.
    #[test]
    fn a_siblings_serve_on_reaching_custody_reprovisions_the_blob_with_the_set() {
        let h = harness(vec![summary("docs", false, None)], Some(MSEK));
        let custody = Arc::new(crate::key_reader::MemoryFolderKeyStore::default());
        let channel = serve_custody_channel_id("docs");
        block_on(crate::key_reader::update(&*custody, |cfg| {
            crate::custody::record_new_set(cfg, channel, [0x11; 32], 1_000);
        }))
        .unwrap();
        let mut f = follower(&h, custody.clone());

        // A notice that moved nothing served provisions nothing.
        assert_eq!(block_on(f.on_change()), FollowStep::Unchanged);
        assert!(h.nest.provisioned.lock().unwrap().is_empty());

        // The sibling's serve-on, as the walk lands it.
        block_on(crate::key_reader::update(&*custody, |cfg| {
            crate::custody::serve_on(cfg, &channel, 2_000);
        }))
        .unwrap();
        assert_eq!(block_on(f.on_change()), FollowStep::Provisioned(1));
        assert_eq!(served_names(&h), vec!["docs".to_string()]);

        // The same projection again is not a second provision.
        assert_eq!(block_on(f.on_change()), FollowStep::Unchanged);
        assert_eq!(h.nest.provisioned.lock().unwrap().len(), 1);
    }

    /// The other direction: a sibling's serve-off reaching custody drops the
    /// set's keys from the blob.
    #[test]
    fn a_siblings_serve_off_reaching_custody_drops_the_set_from_the_blob() {
        let h = harness(vec![summary("docs", true, None)], Some(MSEK));
        let custody = Arc::new(crate::key_reader::MemoryFolderKeyStore::default());
        let channel = serve_custody_channel_id("docs");
        block_on(crate::key_reader::update(&*custody, |cfg| {
            crate::custody::record_new_set(cfg, channel, [0x11; 32], 1_000);
            crate::custody::serve_on(cfg, &channel, 1_000);
        }))
        .unwrap();
        let f = follower(&h, custody.clone());

        block_on(crate::key_reader::update(&*custody, |cfg| {
            crate::custody::serve_off(cfg, &channel, 2_000);
        }))
        .unwrap();
        // Driven through `run`: one notice, then the source ends.
        block_on(f.run(Box::new(Notices(1))));
        assert!(served_names(&h).is_empty(), "the unserved set rides no key");
        assert_eq!(h.nest.provisioned.lock().unwrap().len(), 1);
    }

    /// A device with no MSEK has nothing to seal: the follower provisions
    /// nothing and does not retry the same projection.
    #[test]
    fn a_device_without_msek_follows_without_provisioning() {
        let h = harness(vec![summary("docs", false, None)], None);
        let custody = Arc::new(crate::key_reader::MemoryFolderKeyStore::default());
        let channel = serve_custody_channel_id("docs");
        let mut f = follower(&h, custody.clone());
        block_on(crate::key_reader::update(&*custody, |cfg| {
            crate::custody::record_new_set(cfg, channel, [0x11; 32], 1_000);
            crate::custody::serve_on(cfg, &channel, 1_000);
        }))
        .unwrap();
        assert_eq!(block_on(f.on_change()), FollowStep::NoMsek);
        assert_eq!(block_on(f.on_change()), FollowStep::Unchanged);
        assert!(h.nest.provisioned.lock().unwrap().is_empty());
    }
}
