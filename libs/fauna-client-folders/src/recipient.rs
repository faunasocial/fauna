//! Recipient-side shared-folder **accept** and **decline** recipes — written
//! once, run by every app (`docs/goal/ui/folders.md` § Sharing → *Adding the
//! 2nd..Nth member*, the "Declining an invitation removes you from the roster"
//! rule; § *Recipient side* for the knock surface they act on).
//!
//! Both answer the same knock and share the same crash-safety shape: **the
//! durable, security-meaningful mutation runs first, and the `ack` only after
//! it succeeds** — the un-acked row is what makes a retry possible. Accept
//! joins then acks; decline leaves the roster then acks.
//!
//! Accept takes its join step as a **parameter** rather than depending on
//! `fauna-conversations`: the four app faces hold their MLS handle differently
//! (a `ConversationsSession` on native and tui, a raw `FaunaMlsBackend` on
//! wasm), so the recipe names the seven values a join needs
//! ([`FolderWelcomeJoin`]) and lets each caller supply the join itself. That
//! keeps this module transport-generic and wasm-safe, exactly as decline is.
//!
//! A decline is **two** mutations, not one. The obvious half is the durable-inbox
//! `ack` that drops the staged Welcome unprocessed (so declining never joins the
//! group). The load-bearing half is the **roster drop**: the recipient is already
//! on the set's roster before they ever see the knock — the owner's share writes
//! it at Welcome delivery (same-nest `actor_channels`; cross-nest
//! `channel_foreign_members` at the federation relay), which is exactly what makes
//! the owner's "Shared with" list truthful for an *accepted* share. Ack alone would
//! therefore leave a declined share rostered forever: the owner's list over-reports,
//! and — since the 2nd..Nth-member add path discriminates on that roster — a later
//! re-share would take the no-op access-refresh arm, making the decliner permanently
//! un-re-invitable.
//!
//! So the decline reuses the existing self-scoped [`FoldersClient::leave_with_home`]
//! (`fauna.folders.leave`, relayed cross-nest exactly like a voluntary leave), and
//! a later re-share becomes a genuine re-invite with a fresh Welcome. There is no
//! local MLS forget in this recipe — unlike a voluntary leave, a decliner never
//! joined the group, so there is nothing to forget.
//!
//! The same roster drop belongs to a **suppressed** (Blocked-sharer) knock, which
//! never reaches this module: it is acked-and-dropped inside the shared arrival
//! gate. That arm calls the seam method
//! `fauna_conversations::backend::FolderGateSink::drop_roster_row` instead — same
//! rule, different entry point (the gate cannot depend on this crate; see that
//! method's docs).

use fauna_client_inbox::{InboxClient, PENDING_SHARE_PEEK_LIMIT, list_folder_pending_shares};
use fauna_protocol::RpcRequester;

use crate::FoldersClient;

/// The resolved staged Welcome an [`accept_folder_share`] hands to its join
/// step — the seven values every app's join call needs, **named** rather than
/// positional.
///
/// Naming them is the point. Until this record existed the same seven travelled
/// as positional arguments through four hand-written copies of the accept
/// ceremony (fauna-ffi, tui, linux, wasm), and three of them —
/// [`Self::set_name`], [`Self::access`], [`Self::home_nest_actor_id`] — are
/// adjacent `Option<String>`s, so transposing any two compiled clean at every
/// one of those sites and would have surfaced only as a mis-rendered set name
/// or a silently-wrong byte-plane trust root.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FolderWelcomeJoin {
    /// The fauna channel id (hex) the Welcome claims — the join's address.
    pub channel_id_hex: String,
    /// The raw MLS Welcome bytes staged in the durable inbox.
    pub welcome_bytes: Vec<u8>,
    /// The share's home-nest URL, empty for a same-nest share (the callers'
    /// historical `unwrap_or_default()`, applied once here).
    pub home_nest_url: String,
    /// The shared set's name, home-nest-resolved. Display-only, never a lookup
    /// key; `None` from an unopenable seal or a relayed origin that omits it.
    pub set_name: Option<String>,
    /// The recipient's access grant (`"reader"`/`"writer"`), home-nest-resolved.
    /// Advisory-for-UI only — never an authorization input.
    pub access: Option<String>,
    /// The home nest's deployment `nest_actor_id` — the byte-plane SPKI-pin
    /// trust root for a cross-nest share.
    pub home_nest_actor_id: Option<String>,
    /// The sharer's handle + handle domain — the cross-nest owner label, a
    /// pair only when the recipient's own nest verified it
    /// (`federation.md` § … *The cross-nest owner label*).
    pub shared_by_handle: Option<String>,
    /// See [`Self::shared_by_handle`].
    pub shared_by_domain: Option<String>,
    /// The set name sealed under the set's content keys — what names the
    /// recipient's foreign-set record once the join has ingested those keys
    /// ([`Self::set_name`] is `None` whenever the plaintext was scrubbed).
    pub set_name_seal: Option<fauna_core::label_custody::SealedSetName>,
}

#[cfg(feature = "mls")]
impl FolderWelcomeJoin {
    /// The join's arguments in `join_folder_welcome`'s order — channel hex,
    /// Welcome bytes, home-nest URL, and the home-nest-resolved fields grouped
    /// as its [`fauna_conversations::session::FolderWelcomeContext`] — so every
    /// app's join step hands them over in one shape.
    pub fn into_join_args(
        self,
    ) -> (
        String,
        Vec<u8>,
        String,
        fauna_conversations::session::FolderWelcomeContext,
    ) {
        (
            self.channel_id_hex,
            self.welcome_bytes,
            self.home_nest_url,
            fauna_conversations::session::FolderWelcomeContext {
                // The accept bypasses the contact gate, the one reader of it.
                shared_by: None,
                set_name: self.set_name,
                access: self.access,
                home_nest_actor_id: self.home_nest_actor_id,
                shared_by_handle: self.shared_by_handle,
                shared_by_domain: self.shared_by_domain,
                set_name_seal: self.set_name_seal,
            },
        )
    }
}

/// Why [`accept_folder_share`] could not complete. `E` is the per-app transport
/// error; `J` is whatever the caller's join step failed with.
#[derive(Debug)]
pub enum AcceptShareError<E, J> {
    /// No un-acked pending share with this `inbox_id` — already accepted or
    /// declined (possibly by another device, or by a concurrent drain).
    NotFound {
        /// The durable-inbox row id the client asked to accept.
        inbox_id: i64,
    },
    /// The staged Welcome carries no `channel_id`, so there is no address to
    /// join. Refused **before** the join, and left un-acked.
    Unjoinable {
        /// The durable-inbox row id the client asked to accept.
        inbox_id: i64,
    },
    /// The caller's join step failed. The row is deliberately left un-acked.
    Join(J),
    /// A nest call failed: the peek or the ack.
    Rpc(E),
}

impl<E: core::fmt::Display, J: core::fmt::Display> core::fmt::Display for AcceptShareError<E, J> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::NotFound { inbox_id } => write!(
                f,
                "pending folder share {inbox_id} not found (already accepted or declined?)"
            ),
            Self::Unjoinable { inbox_id } => write!(
                f,
                "pending folder share {inbox_id} missing channel_id (unjoinable)"
            ),
            Self::Join(e) => write!(f, "{e}"),
            Self::Rpc(e) => write!(f, "{e}"),
        }
    }
}

impl<E: core::fmt::Display + core::fmt::Debug, J: core::fmt::Display + core::fmt::Debug>
    std::error::Error for AcceptShareError<E, J>
{
}

/// Accept a staged folder share (`folder-share-accept-button`), by `inbox_id`:
/// resolve the Welcome, join the MLS group **off the chat rail**, then `ack` the
/// durable row.
///
/// Three steps:
///
/// 1. **Re-peek** the pending list to resolve the row — the client passes only
///    the id, exactly as the decline path does. The large Welcome blob never
///    round-trips through a client (or across the FFI boundary).
/// 2. **Join**, through the caller-supplied step, handed a [`FolderWelcomeJoin`].
///    Accept **bypasses the contact gate on purpose** — the user explicitly
///    accepted, so no auto/knock/suppress decision applies (the gate only
///    decides *pushed* welcomes).
/// 3. **Ack** the durable row.
///
/// **Ordering is load-bearing: join before ack** — the mirror of decline's
/// leave-before-ack, and the reason this recipe exists in one place. The join is
/// idempotent, so a crash between the two re-lists the share and re-accepting
/// converges. Acking first — or acking after a *failed* join — would discard the
/// only handle on a Welcome that was never joined: the share would vanish from
/// the recipient's list without them ever being in the group, with no client-side
/// way back (`nest/common.md` § Client-state recoverability). That is the
/// property `a_failed_join_never_acks` pins.
pub async fn accept_folder_share<R, F, Fut, J>(
    inbox: &InboxClient<R>,
    inbox_id: i64,
    join: F,
) -> Result<(), AcceptShareError<R::Error, J>>
where
    R: RpcRequester,
    F: FnOnce(FolderWelcomeJoin) -> Fut,
    Fut: core::future::Future<Output = Result<(), J>>,
{
    let share = list_folder_pending_shares(inbox, PENDING_SHARE_PEEK_LIMIT)
        .await
        .map_err(AcceptShareError::Rpc)?
        .into_iter()
        .find(|s| s.inbox_id == inbox_id)
        .ok_or(AcceptShareError::NotFound { inbox_id })?;

    let channel_id_hex = share
        .channel_id
        .ok_or(AcceptShareError::Unjoinable { inbox_id })?;

    join(FolderWelcomeJoin {
        channel_id_hex,
        welcome_bytes: share.welcome_bytes,
        home_nest_url: share.home_nest_url.unwrap_or_default(),
        set_name: share.set_name,
        access: share.access,
        home_nest_actor_id: share.home_nest_actor_id,
        shared_by_handle: share.shared_by_handle,
        shared_by_domain: share.shared_by_domain,
        set_name_seal: share.set_name_seal,
    })
    .await
    .map_err(AcceptShareError::Join)?;

    inbox
        .ack(vec![inbox_id])
        .await
        .map_err(AcceptShareError::Rpc)?;
    Ok(())
}

/// Why [`decline_folder_share`] could not complete. Carries the transport error
/// generically (`R::Error` is per-app) plus the one domain outcome the recipe
/// itself can decide, so clients render both through one `Display` instead of each
/// inventing a message.
#[derive(Debug)]
pub enum DeclineShareError<E> {
    /// No un-acked pending share with this `inbox_id` — already accepted or
    /// declined (possibly by another device, or by a concurrent drain).
    NotFound {
        /// The durable-inbox row id the client asked to decline.
        inbox_id: i64,
    },
    /// A nest call failed: the peek, the roster leave, or the ack.
    Rpc(E),
}

impl<E: core::fmt::Display> core::fmt::Display for DeclineShareError<E> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::NotFound { inbox_id } => write!(
                f,
                "pending folder share {inbox_id} not found (already accepted or declined?)"
            ),
            Self::Rpc(e) => write!(f, "{e}"),
        }
    }
}

impl<E: core::fmt::Display + core::fmt::Debug> std::error::Error for DeclineShareError<E> {}

/// Decline a staged folder share (`folder-share-decline-button`), by
/// `inbox_id`: drop the recipient's roster row, then `ack` the durable row so the
/// Welcome is discarded **unprocessed** (declining never joins the group).
///
/// Three steps:
///
/// 1. **Re-peek** the pending list to resolve the row — the client passes only the
///    id, exactly as the accept path does (the Welcome blob never round-trips
///    through a client). The row carries both addresses the leave needs:
///    `group_id` (the leave is addressed by the raw MLS group id, never the
///    owner-only set name) and `home_nest_url` (`Some` ⇒ a cross-nest share, whose
///    roster row lives on the set's home nest and is dropped through the
///    `fauna.federation.channel.leave` relay).
/// 2. **Roster drop** via [`FoldersClient::leave_with_home`] — self-scoped, needs
///    no `ownerSecret`, and does **not** rotate the owner's content key (nothing
///    was ever shared *to* a decliner to protect —
///    `mls-group-key-material.md` § M2). Skipped only when the row carries no
///    `group_id` (a relayed origin that omitted it): there is no address
///    to leave, so the recipe degrades to the historical bare ack rather than
///    stranding the knock un-declinable.
/// 3. **Ack** the durable row.
///
/// **Ordering is load-bearing: leave before ack** — the same crash-safety shape as
/// accept (join, *then* ack) and as `folders_leave` (nest drop, then local
/// forget). The durable, security-meaningful mutation runs first, and the un-acked
/// row is what makes a retry possible: a crash between the two re-lists the share,
/// and re-declining converges (the leave is idempotent — `left: false` the second
/// time — and the ack then lands). Acking first would drop the only handle on a
/// roster row that still needs removing.
pub async fn decline_folder_share<R: RpcRequester>(
    inbox: &InboxClient<R>,
    folders: &FoldersClient<R>,
    inbox_id: i64,
) -> Result<(), DeclineShareError<R::Error>> {
    let share = list_folder_pending_shares(inbox, PENDING_SHARE_PEEK_LIMIT)
        .await
        .map_err(DeclineShareError::Rpc)?
        .into_iter()
        .find(|s| s.inbox_id == inbox_id)
        .ok_or(DeclineShareError::NotFound { inbox_id })?;

    if let Some(group_id) = share.group_id {
        folders
            .leave_with_home(group_id, share.home_nest_url)
            .await
            .map_err(DeclineShareError::Rpc)?;
    }

    inbox
        .ack(vec![inbox_id])
        .await
        .map_err(DeclineShareError::Rpc)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use fauna_client_testkit::block_on;
    use fauna_protocol::inbox::{InboxEnvelope, InboxFetchReply, InboxItem, WelcomeInbox};
    use std::sync::Mutex;

    /// Records the ordered kind sequence so the leave-before-ack contract is
    /// observable, and answers each kind with the minimal decodable reply.
    /// Transport-free (runs on every target, wasm included), mirroring the
    /// `RecordingRequester` pattern in `lib.rs`.
    struct SeqRequester {
        kinds: Mutex<Vec<&'static str>>,
        leave_payloads: Mutex<Vec<fauna_protocol::folders::MemberLeaveRequest>>,
        /// The staged folder welcome `fauna.inbox.fetch` returns; empty ⇒ the
        /// pending list is empty.
        staged: Vec<InboxItem>,
    }

    impl SeqRequester {
        fn new(staged: Vec<InboxItem>) -> Self {
            Self {
                kinds: Mutex::new(Vec::new()),
                leave_payloads: Mutex::new(Vec::new()),
                staged,
            }
        }

        fn kinds(&self) -> Vec<&'static str> {
            self.kinds.lock().unwrap().clone()
        }
    }

    impl RpcRequester for SeqRequester {
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
            self.kinds.lock().unwrap().push(kind);
            let bytes = fauna_protocol::encode_canonical(&payload).expect("encode request");
            if kind == "fauna.folders.leave" {
                self.leave_payloads
                    .lock()
                    .unwrap()
                    .push(fauna_protocol::decode_strict(&bytes).expect("decode leave request"));
            }
            let reply = match kind {
                "fauna.inbox.fetch" => fauna_protocol::encode_canonical(&InboxFetchReply {
                    items: self.staged.clone(),
                    more: false,
                    extra: Default::default(),
                }),
                "fauna.inbox.ack" => {
                    fauna_protocol::encode_canonical(&fauna_protocol::inbox::InboxAckReply {
                        acked: 1,
                        extra: Default::default(),
                    })
                }
                "fauna.folders.leave" => {
                    fauna_protocol::encode_canonical(&fauna_protocol::folders::MemberLeaveReply {
                        ok: true,
                        channel_id: "ab".repeat(32),
                        left: true,
                        extra: Default::default(),
                    })
                }
                other => panic!("unexpected kind {other}"),
            }
            .expect("encode reply");
            Ok(fauna_protocol::decode_strict(&reply).expect("decode reply"))
        }
    }

    fn staged_share(inbox_id: i64, group_id: Option<&str>, home: Option<&str>) -> InboxItem {
        InboxItem {
            extra: Default::default(),
            id: inbox_id,
            payload: InboxEnvelope::welcome(&WelcomeInbox {
                welcome_bytes: vec![9, 8, 7],
                channel_id: Some("cd".repeat(32)),
                channel_type: Some("folder".into()),
                group_id: group_id.map(Into::into),
                nest_url: home.map(Into::into),
                ..Default::default()
            })
            .unwrap()
            .to_canonical_bytes()
            .unwrap()
            .to_vec(),
        }
    }

    fn decline(r: &std::sync::Arc<SeqRequester>, inbox_id: i64) -> Result<(), String> {
        block_on(decline_folder_share(
            &InboxClient::new(r.clone()),
            &FoldersClient::new(r.clone()),
            inbox_id,
        ))
        .map_err(|e| e.to_string())
    }

    /// The whole point of the slice: a decline drops the roster row the owner's
    /// share wrote at Welcome delivery — and does it BEFORE the ack, so a crash
    /// between them re-lists the share and re-declining converges (the ack is the
    /// only handle on a row that still needs removing).
    #[test]
    fn decline_leaves_the_roster_then_acks() {
        let r = std::sync::Arc::new(SeqRequester::new(vec![staged_share(7, Some("aabb"), None)]));
        decline(&r, 7).unwrap();
        assert_eq!(
            r.kinds(),
            vec![
                "fauna.inbox.fetch",
                "fauna.folders.leave",
                "fauna.inbox.ack"
            ],
            "leave BEFORE ack — the durable, security-meaningful half runs first"
        );
        let leaves = r.leave_payloads.lock().unwrap();
        assert_eq!(leaves.len(), 1);
        assert_eq!(
            leaves[0].group_id, "aabb",
            "the leave is addressed by the Welcome's raw MLS group id"
        );
        assert_eq!(leaves[0].nest_url, None, "same-nest share ⇒ no relay URL");
    }

    /// A cross-nest share's roster row lives on the SET's home nest
    /// (`channel_foreign_members`), so the decline must thread the staged
    /// `nest_url` for the `fauna.federation.channel.leave` relay — exact parity
    /// with a voluntary cross-nest leave.
    #[test]
    fn decline_relays_cross_nest() {
        let r = std::sync::Arc::new(SeqRequester::new(vec![staged_share(
            3,
            Some("ccdd"),
            Some("https://home.example"),
        )]));
        decline(&r, 3).unwrap();
        let leaves = r.leave_payloads.lock().unwrap();
        assert_eq!(
            leaves[0].nest_url.as_deref(),
            Some("https://home.example"),
            "the decline relays the roster drop to the set's home nest"
        );
    }

    /// A row with no `group_id` (a relayed origin omitted it) has no address to
    /// leave — degrade to the historical bare ack rather than stranding the knock
    /// un-declinable.
    #[test]
    fn decline_without_a_group_id_still_acks() {
        let r = std::sync::Arc::new(SeqRequester::new(vec![staged_share(5, None, None)]));
        decline(&r, 5).unwrap();
        assert_eq!(r.kinds(), vec!["fauna.inbox.fetch", "fauna.inbox.ack"]);
    }

    // ---- accept ----------------------------------------------------------

    /// A staged share with every one of the seven join values distinctly
    /// populated, so a test can tell each apart from its neighbours.
    fn staged_rich_share(inbox_id: i64) -> InboxItem {
        InboxItem {
            extra: Default::default(),
            id: inbox_id,
            payload: InboxEnvelope::welcome(&WelcomeInbox {
                welcome_bytes: vec![1, 2, 3, 4],
                channel_id: Some("cd".repeat(32)),
                channel_type: Some("folder".into()),
                group_id: Some("aabb".into()),
                nest_url: Some("https://home.example".into()),
                set_name: Some("the-set-name".into()),
                access: Some("writer".into()),
                home_nest_actor_id: Some("the-home-actor".into()),
                ..Default::default()
            })
            .unwrap()
            .to_canonical_bytes()
            .unwrap()
            .to_vec(),
        }
    }

    /// Run the accept recipe with a join step that records what it was handed
    /// and answers with `outcome`.
    fn accept_with(
        r: &std::sync::Arc<SeqRequester>,
        inbox_id: i64,
        seen: &std::sync::Arc<Mutex<Vec<FolderWelcomeJoin>>>,
        outcome: Result<(), &'static str>,
    ) -> Result<(), String> {
        let seen = seen.clone();
        block_on(accept_folder_share(
            &InboxClient::new(r.clone()),
            inbox_id,
            move |join| {
                seen.lock().unwrap().push(join);
                async move { outcome }
            },
        ))
        .map_err(|e| e.to_string())
    }

    /// The contract every app's accept path states in prose and none of them
    /// tested: join FIRST, ack only after it succeeds.
    #[test]
    fn accept_joins_then_acks() {
        let r = std::sync::Arc::new(SeqRequester::new(vec![staged_rich_share(7)]));
        let seen = std::sync::Arc::new(Mutex::new(Vec::new()));
        accept_with(&r, 7, &seen, Ok(())).unwrap();
        assert_eq!(r.kinds(), vec!["fauna.inbox.fetch", "fauna.inbox.ack"]);
        assert_eq!(seen.lock().unwrap().len(), 1, "the join ran exactly once");
    }

    /// **The crash-safety half.** A failed join must leave the row UN-acked, so
    /// the share re-lists and re-accepting converges. Acking anyway would
    /// discard the only handle on a Welcome that was never joined — the share
    /// would vanish from the recipient's list without them ever being in the
    /// group.
    #[test]
    fn a_failed_join_never_acks() {
        let r = std::sync::Arc::new(SeqRequester::new(vec![staged_rich_share(7)]));
        let seen = std::sync::Arc::new(Mutex::new(Vec::new()));
        let err = accept_with(&r, 7, &seen, Err("mls exploded")).unwrap_err();
        assert!(
            err.contains("mls exploded"),
            "surfaces the join error: {err}"
        );
        assert_eq!(
            r.kinds(),
            vec!["fauna.inbox.fetch"],
            "NO ack after a failed join — the un-acked row is the retry handle"
        );
    }

    /// Every one of the seven values reaches the join step in its own slot.
    /// `set_name`, `access` and `home_nest_actor_id` are all `Option<String>`
    /// and were adjacent positional arguments at four hand-written call sites,
    /// where any two could be swapped and still compile.
    #[test]
    fn accept_hands_the_join_every_welcome_field() {
        let r = std::sync::Arc::new(SeqRequester::new(vec![staged_rich_share(7)]));
        let seen = std::sync::Arc::new(Mutex::new(Vec::new()));
        accept_with(&r, 7, &seen, Ok(())).unwrap();
        let join = seen.lock().unwrap()[0].clone();
        assert_eq!(join.channel_id_hex, "cd".repeat(32));
        assert_eq!(join.welcome_bytes, vec![1, 2, 3, 4]);
        assert_eq!(join.home_nest_url, "https://home.example");
        assert_eq!(join.set_name.as_deref(), Some("the-set-name"));
        assert_eq!(join.access.as_deref(), Some("writer"));
        assert_eq!(join.home_nest_actor_id.as_deref(), Some("the-home-actor"));
    }

    /// A share the nest never stamped a `channel_id` on is unjoinable: refuse
    /// before the join, and — like the not-found arm — leave it un-acked.
    #[test]
    fn accept_refuses_a_share_with_no_channel_id() {
        let unaddressed = InboxItem {
            extra: Default::default(),
            id: 7,
            payload: InboxEnvelope::welcome(&WelcomeInbox {
                welcome_bytes: vec![1, 2, 3],
                channel_id: None,
                channel_type: Some("folder".into()),
                ..Default::default()
            })
            .unwrap()
            .to_canonical_bytes()
            .unwrap()
            .to_vec(),
        };
        let r = std::sync::Arc::new(SeqRequester::new(vec![unaddressed]));
        let seen = std::sync::Arc::new(Mutex::new(Vec::new()));
        let err = accept_with(&r, 7, &seen, Ok(())).unwrap_err();
        assert!(err.contains("unjoinable"), "names the reason: {err}");
        assert!(seen.lock().unwrap().is_empty(), "never reached the join");
        assert_eq!(r.kinds(), vec!["fauna.inbox.fetch"], "no ack");
    }

    /// Same not-found shape as decline: no join, no ack.
    #[test]
    fn accept_of_an_unknown_id_is_not_found() {
        let r = std::sync::Arc::new(SeqRequester::new(vec![staged_rich_share(1)]));
        let seen = std::sync::Arc::new(Mutex::new(Vec::new()));
        let err = accept_with(&r, 42, &seen, Ok(())).unwrap_err();
        assert!(err.contains("42"), "names the id: {err}");
        assert!(seen.lock().unwrap().is_empty(), "never reached the join");
        assert_eq!(r.kinds(), vec!["fauna.inbox.fetch"], "no ack");
    }

    /// Nothing is acked or left when the id resolves to no staged share — the
    /// same not-found shape the accept path reports.
    #[test]
    fn decline_of_an_unknown_id_is_not_found() {
        let r = std::sync::Arc::new(SeqRequester::new(vec![staged_share(1, Some("aabb"), None)]));
        let err = decline(&r, 42).unwrap_err();
        assert!(err.contains("42"), "names the id: {err}");
        assert_eq!(r.kinds(), vec!["fauna.inbox.fetch"], "no ack, no leave");
    }
}
