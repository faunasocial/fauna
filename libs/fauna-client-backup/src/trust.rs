//! Backup trust rows — the backup half of the Nests-page trust facet
//! (`docs/goal/ui/nests.md` § Trust facet — backup rows, ratified 2026-07-24).
//!
//! Destination-enroll mints **two standing grants, both empowering the source
//! (home) nest**, so both render on the home nest's row:
//!
//! 1. the owner→source-nest `NestBackupKey` **seal grant** (one row, present iff
//!    `fauna.backup.status` reports `enrolled`), and
//! 2. one **writer-grant row per backup destination** — the source nest's
//!    authorization to write custody *there*.
//!
//! The load-bearing asymmetry is *which nest each row is read from and revoked
//! at*. The seal row rides the already-open home connection. The writer rows are
//! read and revoked over **the destination's own authenticated connection**
//! (never the source nest, never the federation channel) — that is precisely
//! what keeps the freeze-the-backup affordance operable with the source nest
//! fully hostile. A hostile source could otherwise answer the read with a lie
//! and swallow the revoke.
//!
//! These are **live nest reads, not grant-event-log folds** (`nests.md:99`).
//! Both grants are standing-until-revoked and verifiable against the live
//! stores, so there is no `lasts-until` / renew / auto-renew and no History-lens
//! entry — a client-side mirror is the only thing here that *could* drift, so we
//! don't keep one.
//!
//! **Which writer a row and a revoke name is not the source's to say.** Every
//! writer row and writer revoke keys on the id the home connection **proved**
//! (the caller's bound nest id — the login's pin for the origin, else a
//! possession proof), never on the source's own `fauna.nest.info` answer: the
//! destination's writer gate keys on the handshake-verified id, so a row keyed
//! on a claim would show — and a revoke would delete — whatever grant a
//! hostile source names, a sibling's included, while its own survived. No
//! seam method reports a nest's identity, so the claim is not reachable here.
//!
//! The destination set iterated is the client's own pinned
//! `fauna.state.backup` destination list — source-untrusted by construction. A
//! configured destination the source nest "forgot" therefore still renders, as
//! `missing`, rather than silently vanishing from the facet.
//!
//! # Where the pieces live
//!
//! The protocol stays in [`BackupClient`](crate::BackupClient): the per-app
//! glue implements only [`BackupDestinationConnector`] — "open an authenticated
//! connection to this URL, and say which identity it proved" — and expands
//! [`impl_backup_nest_seam!`](crate::impl_backup_nest_seam) to get the typed call
//! surface over it. No client re-derives a kind name or a payload shape
//! (priority #2).
//!
//! **Which box is the destination is not the URL's to say either.** Every
//! destination connection passes [`connect_destination`], which refuses one
//! whose proven identity is not the `destination_actor_pubkey` the owner
//! enrolled (itself a proven id — [`proven_destination_id`]) — so a box
//! answering a lapsed or hijacked destination URL with valid TLS can neither
//! paint the writer rows nor swallow a revoke.

use std::sync::Arc;

use async_trait::async_trait;
use fauna_core::data::BackupDestination;
use fauna_protocol::MaybeSendSync;
use fauna_protocol::backup::{
    BackupStatusReply, CustodyListReply, GenerationListReply, GenerationRestoreReply,
    WriterGrantListReply,
};

/// Which backup power a [`BackupTrustRow`] describes — the discriminator the
/// shell turns into `nest-trust-backup-scope` copy.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BackupTrustKind {
    /// The owner→source-nest `NestBackupKey` seal grant: the nest is trusted to
    /// seal and upload this owner's message backups. Revoked at the **source**
    /// (`fauna.backup.nest_key.revoke`).
    Seal,
    /// The source nest's authorization to write custody at one destination.
    /// Revoked at the **destination** (`fauna.backup.writer_grant.revoke`).
    Writer {
        /// `BackupDestination::destination_id` — names the row for a revoke.
        destination_id: String,
        /// The destination's origin URL (the revoke's connect target).
        destination_url: String,
        /// User-facing destination label, via the shared
        /// [`fauna_core::format::backup_destination_label`] fallback — never
        /// re-derived per client (priority #4).
        destination_label: String,
    },
}

/// Row state (`nest-trust-backup-status`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BackupTrustStatus {
    /// The grant is in force.
    Active,
    /// The destination's writer-grant read failed — unreachable, refused, or
    /// malformed. Deliberately distinct from [`Missing`](Self::Missing): "we
    /// could not ask" must not read as "the trust is gone".
    Unreachable,
    /// The destination answered, and this owner has authorized **no** writer
    /// grant for the source nest there. The config lists the destination but
    /// backups cannot be written to it.
    Missing,
}

/// One backup trust row (`nest-trust-backup-item`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BackupTrustRow {
    pub kind: BackupTrustKind,
    pub status: BackupTrustStatus,
    /// The writer grant's `granted_at` (unix seconds). `None` for the seal row,
    /// which carries no timestamp on the wire — `nest-trust-backup-since`
    /// renders empty there (`nests.md:67`).
    pub since: Option<i64>,
}

/// The typed, object-safe `fauna.backup.*` call surface a trust row is built
/// from — **one** trait for both ends, exactly as [`BackupClient`](crate::BackupClient) is one type
/// for both: the source methods are spoken to the home nest and the destination
/// methods to a destination, and which nest a given seam points at is the
/// caller's context, not the type's.
///
/// Object-safe because the destination connection is chosen at runtime (one per
/// configured destination) and the machine holding the home connection has only
/// a `dyn` seam, never a concrete transport.
///
/// The transport error is flattened to `String` at this boundary on purpose: a
/// `dyn` seam cannot carry `R::Error`, and every consumer of a failed
/// destination read renders `status: unreachable` rather than branching on the
/// cause. `RpcRequester::Error` is bounded `Display`, so nothing a caller acts
/// on is lost.
///
/// Implement it with [`impl_backup_nest_seam!`] rather than by hand — the macro
/// writes the body over a concrete transport, keeping every `fauna.backup.*`
/// kind name inside [`BackupClient`](crate::BackupClient).
#[cfg_attr(not(target_arch = "wasm32"), async_trait)]
#[cfg_attr(target_arch = "wasm32", async_trait(?Send))]
pub trait BackupNestSeam: MaybeSendSync {
    /// `fauna.backup.status` (**source**) — `enrolled` drives the seal row.
    async fn status(&self) -> Result<BackupStatusReply, String>;
    /// `fauna.backup.nest_key.revoke` (**source**) — freeze new sealing.
    async fn nest_key_revoke(&self) -> Result<(), String>;
    /// `fauna.backup.writer_grant.list` (**destination**).
    async fn writer_grant_list(&self) -> Result<WriterGrantListReply, String>;
    /// `fauna.backup.writer_grant.revoke` (**destination**) — the reply's
    /// `revoked` flag: `true` if the destination held the grant and removed
    /// it, `false` if it held none (an unknown id, or a retry after an
    /// unacknowledged success). Carried, not dropped, because only a re-read
    /// can tell those two `false`s apart ([`revoke_backup_writer`]).
    async fn writer_grant_revoke(&self, writer_nest_id: String) -> Result<bool, String>;
    /// `fauna.backup.custody.list` (**destination**) — one page of the live
    /// custody this destination holds for the owner (`cursor: None` = first
    /// page; walk `next_cursor` to absence — `crate::audit::read_full_custody`
    /// owns the loop). Backs the audit loop ([`crate::audit`]), which rides
    /// this same seam rather than opening a second connection per destination.
    async fn custody_list(&self, cursor: Option<String>) -> Result<CustodyListReply, String>;
    /// `fauna.backup.generation.list` (**destination**) — one page of the
    /// superseded generations still retained inside the grace window `T`
    /// (same cursor contract as [`Self::custody_list`]). Backs generation
    /// recovery ([`crate::generations`]), which rides this same seam for the
    /// same reason the audit does.
    async fn generation_list(&self, cursor: Option<String>) -> Result<GenerationListReply, String>;
    /// `fauna.backup.generation.restore` (**destination**) — promote one
    /// retained generation back to live.
    async fn generation_restore(
        &self,
        folder_name: String,
        path_hash: String,
        manifest_hash: String,
    ) -> Result<GenerationRestoreReply, String>;
    /// `fauna.segments.list` (**source**) — the source's saved segment counter
    /// for one `(serve tag, scope)`: the backup ledger's generation, asked of
    /// the owner's own nest only to settle a regression the audit observed at
    /// a destination (`crate::audit::SourceLedgerVouch`).
    ///
    /// Defaulted to "could not ask" so a test double that never meets a
    /// regression needs no arm — and so a seam missing it fails **loud**: an
    /// unanswerable regression is a certain miss, never an accepted one. The
    /// production adapter (`impl_backup_nest_seam!`) always answers.
    async fn segments_next_id(&self, kind_tag: String, scope_hex: String) -> Result<u32, String> {
        Err(format!(
            "this seam does not answer fauna.segments.list (kind={kind_tag}, scope={scope_hex})"
        ))
    }
    /// `fauna.segments.counter_floor` (**source**) — raise the source's saved
    /// segment counter for one `(serve tag, scope)` to at least `floor`,
    /// answering the counter it then stands at. The audit's one write to the
    /// source, made when it accepts a source regression
    /// (`crate::audit::SourceLedgerVouch::floor_counter`).
    ///
    /// Defaulted to "could not ask", like [`Self::segments_next_id`]: an
    /// unanswered floor is retried by the next pass. The production adapter
    /// (`impl_backup_nest_seam!`) always answers.
    async fn segments_counter_floor(
        &self,
        kind_tag: String,
        scope_hex: String,
        floor: u32,
    ) -> Result<u32, String> {
        Err(format!(
            "this seam does not answer fauna.segments.counter_floor (kind={kind_tag}, scope={scope_hex}, floor={floor})"
        ))
    }
    /// `fauna.auth.rotation_chain` (**source**) — the bound box's rotation
    /// chain, oldest hop first (empty when it never rotated). Read only by the
    /// seat carry ([`crate::seat_carry`]), which verifies it before naming a
    /// predecessor.
    ///
    /// Defaulted to "could not ask", like [`Self::segments_next_id`]: a carry
    /// that cannot fetch the chain carries nothing and is retried. The
    /// production adapter (`impl_backup_nest_seam!`) always answers.
    async fn rotation_chain(
        &self,
    ) -> Result<Vec<fauna_protocol::nest_rotation::SignedNestRotation>, String> {
        Err("this seam does not answer fauna.auth.rotation_chain".to_string())
    }
    /// `fauna.backup.writer_grant.register { succeeds }` (**destination**) —
    /// hand the seat `succeeds` holds to `writer_nest_id`. The seam has no
    /// registration without `succeeds`: the carry is its only caller, and
    /// without a predecessor the successor is a second box.
    ///
    /// Defaulted to "could not ask", like [`Self::rotation_chain`]. The
    /// production adapter (`impl_backup_nest_seam!`) always answers.
    async fn writer_grant_succeed(
        &self,
        writer_nest_id: String,
        succeeds: String,
    ) -> Result<(), String> {
        Err(format!(
            "this seam does not answer fauna.backup.writer_grant.register \
             (writer={writer_nest_id}, succeeds={succeeds})"
        ))
    }
}

/// Generate a [`BackupNestSeam`] adapter over a **concrete** transport.
///
/// **Why a macro, not a blanket impl** —
/// a generic `impl<R: RpcRequester> BackupNestSeam for BackupClient<R>` cannot
/// satisfy the native `async_trait`'s `+ Send` boxing, because
/// [`fauna_protocol::RpcRequester::request`] is an async-fn-in-trait whose future is only
/// provably `Send` per concrete impl. A concrete transport (`Arc<NestClient>` /
/// `WsRpcClient`) does yield a `Send` future, and the orphan rule then forces
/// the impl into the crate owning a local type — so the adapter is a small
/// newtype in each app's glue and this macro writes its body once.
///
/// Emits `struct $name { pub client: BackupClient<$transport> }` plus the impl.
/// Invoke inside the `#[cfg]`-gated native/wasm glue module; requires
/// `async-trait` in the caller's deps. Construct as
/// `Arc::new($name { client: BackupClient::new(transport) })`.
///
/// ```ignore
/// // native glue:
/// fauna_client_backup::impl_backup_nest_seam!(struct RpcBackupNest<Arc<NestClient>>);
/// // wasm glue:
/// fauna_client_backup::impl_backup_nest_seam!(struct RpcBackupNest<TokenWsRpcClient>);
/// ```
#[macro_export]
macro_rules! impl_backup_nest_seam {
    ($vis:vis struct $name:ident<$transport:ty>) => {
        /// A shared-`BackupNestSeam` adapter over the concrete `BackupClient`
        /// transport, generated by `fauna_client_backup::impl_backup_nest_seam!`.
        $vis struct $name {
            pub client: $crate::BackupClient<$transport>,
        }

        #[cfg_attr(not(target_arch = "wasm32"), ::async_trait::async_trait)]
        #[cfg_attr(target_arch = "wasm32", ::async_trait::async_trait(?Send))]
        impl $crate::trust::BackupNestSeam for $name {
            async fn status(
                &self,
            ) -> ::core::result::Result<
                ::fauna_protocol::backup::BackupStatusReply,
                ::std::string::String,
            > {
                self.client.status().await.map_err(|e| e.to_string())
            }

            async fn nest_key_revoke(
                &self,
            ) -> ::core::result::Result<(), ::std::string::String> {
                self.client
                    .nest_key_revoke()
                    .await
                    .map(|_| ())
                    .map_err(|e| e.to_string())
            }

            async fn writer_grant_list(
                &self,
            ) -> ::core::result::Result<
                ::fauna_protocol::backup::WriterGrantListReply,
                ::std::string::String,
            > {
                self.client
                    .writer_grant_list()
                    .await
                    .map_err(|e| e.to_string())
            }

            async fn writer_grant_revoke(
                &self,
                writer_nest_id: ::std::string::String,
            ) -> ::core::result::Result<bool, ::std::string::String> {
                self.client
                    .writer_grant_revoke(writer_nest_id)
                    .await
                    .map(|r| r.revoked)
                    .map_err(|e| e.to_string())
            }

            async fn custody_list(
                &self,
                cursor: ::core::option::Option<::std::string::String>,
            ) -> ::core::result::Result<
                ::fauna_protocol::backup::CustodyListReply,
                ::std::string::String,
            > {
                self.client
                    .custody_list(cursor)
                    .await
                    .map_err(|e| e.to_string())
            }

            async fn generation_list(
                &self,
                cursor: ::core::option::Option<::std::string::String>,
            ) -> ::core::result::Result<
                ::fauna_protocol::backup::GenerationListReply,
                ::std::string::String,
            > {
                self.client
                    .generation_list(cursor)
                    .await
                    .map_err(|e| e.to_string())
            }

            async fn generation_restore(
                &self,
                folder_name: ::std::string::String,
                path_hash: ::std::string::String,
                manifest_hash: ::std::string::String,
            ) -> ::core::result::Result<
                ::fauna_protocol::backup::GenerationRestoreReply,
                ::std::string::String,
            > {
                self.client
                    .generation_restore(folder_name, path_hash, manifest_hash)
                    .await
                    .map_err(|e| e.to_string())
            }

            async fn segments_next_id(
                &self,
                kind_tag: ::std::string::String,
                scope_hex: ::std::string::String,
            ) -> ::core::result::Result<u32, ::std::string::String> {
                self.client
                    .segments_next_id(kind_tag, scope_hex)
                    .await
                    .map_err(|e| e.to_string())
            }

            async fn segments_counter_floor(
                &self,
                kind_tag: ::std::string::String,
                scope_hex: ::std::string::String,
                floor: u32,
            ) -> ::core::result::Result<u32, ::std::string::String> {
                self.client
                    .segments_counter_floor(kind_tag, scope_hex, floor)
                    .await
                    .map_err(|e| e.to_string())
            }

            async fn rotation_chain(
                &self,
            ) -> ::core::result::Result<
                ::std::vec::Vec<::fauna_protocol::nest_rotation::SignedNestRotation>,
                ::std::string::String,
            > {
                self.client
                    .rotation_chain()
                    .await
                    .map(|reply| reply.chain)
                    .map_err(|e| e.to_string())
            }

            async fn writer_grant_succeed(
                &self,
                writer_nest_id: ::std::string::String,
                succeeds: ::std::string::String,
            ) -> ::core::result::Result<(), ::std::string::String> {
                match self
                    .client
                    .writer_grant_succeed(writer_nest_id, succeeds)
                    .await
                {
                    ::core::result::Result::Ok(reply) if reply.ok => {
                        ::core::result::Result::Ok(())
                    }
                    ::core::result::Result::Ok(_) => ::core::result::Result::Err(
                        "the destination did not confirm the seat handover".into(),
                    ),
                    ::core::result::Result::Err(e) => ::core::result::Result::Err(e.to_string()),
                }
            }
        }
    };
}

/// An open, authenticated connection to one nest, and the identity that
/// connection is **bound** to — what [`BackupDestinationConnector::connect`]
/// hands back. `bound_nest_id` is proven over the connection itself (the
/// origin's pin, else a possession proof), never read from the nest's own
/// `fauna.nest.info` claim, which whoever answers the URL can set to anything.
pub struct DestinationConnection {
    pub seam: Arc<dyn BackupNestSeam>,
    pub bound_nest_id: [u8; 32],
}

/// The one platform seam leg (c) needs: open an authenticated connection to a
/// backup destination **as the owner**, wrap it in a [`BackupNestSeam`], and
/// report the identity the connection proved.
///
/// Native glue reuses `segment_backup::resolve_destination_connected`'s
/// `NestClient::connect`; web glue reuses the enroll path's two-step
/// anonymous-handshake → `TokenWsRpcClient` connect. Both already exist and are
/// proven by the enroll sequence — this trait only names the capability so the
/// row projection can stay shared. A destination is opened through
/// [`connect_destination`], never by calling `connect` directly: that is where
/// the proven id is held to the one the owner enrolled.
#[cfg_attr(not(target_arch = "wasm32"), async_trait)]
#[cfg_attr(target_arch = "wasm32", async_trait(?Send))]
pub trait BackupDestinationConnector: MaybeSendSync {
    async fn connect(&self, url: &str) -> Result<DestinationConnection, String>;
}

/// The identity to enroll a destination under (`destination_actor_pubkey`):
/// `bound`, the id the enroll connection **proved** (the origin's pin, else a
/// possession proof over it), refused when the destination's own
/// `fauna.nest.info` claim (`claimed_hex`) names another nest — a disagreement
/// is hostile or broken, and a silent substitution would hide it. The one
/// check both the native and the web enroll resolvers run, so the id every
/// later [`connect_destination`] holds a connection to is a proven one, never
/// the claim (`backup-destinations.md` § Create / edit / remove protocol →
/// Create step 1).
pub fn proven_destination_id(
    url: &str,
    bound: [u8; 32],
    claimed_hex: &str,
) -> Result<[u8; 32], String> {
    match fauna_core::hex32::decode(claimed_hex) {
        Ok(claimed) if claimed == bound => Ok(bound),
        _ => Err(format!(
            "backup destination {url} reports an identity other than the one its \
             connection proved; refusing to enroll it"
        )),
    }
}

/// Open `destination` — **the one door every destination connection goes
/// through** (the trust facet's writer rows and revoke, the recovery
/// surface's generation list and restore, the audit): connect to its URL and
/// refuse the connection unless the identity it proved is the
/// `destination_actor_pubkey` the owner enrolled.
///
/// Without this a box answering the destination URL with valid TLS — a lapsed
/// or hijacked domain — would be taken for the destination: forged writer-grant
/// and generation views, a writer revoke silently swallowed. A changed identity
/// at the same URL is a different nest, which the destination's own contract
/// already routes to remove + re-add ([`BackupDestination`]'s field docs;
/// `backup-destinations.md` § Create / edit / remove protocol → Edit).
pub async fn connect_destination(
    connector: &dyn BackupDestinationConnector,
    destination: &BackupDestination,
) -> Result<Arc<dyn BackupNestSeam>, String> {
    let url = &destination.destination_nest_url;
    let conn = connector.connect(url).await?;
    if conn.bound_nest_id != destination.destination_actor_pubkey {
        return Err(format!(
            "{url} proved identity {}, not the destination enrolled as {} — \
             refusing to treat it as that destination",
            fauna_core::hex32::encode(&conn.bound_nest_id),
            fauna_core::hex32::encode(&destination.destination_actor_pubkey),
        ));
    }
    Ok(conn.seam)
}

/// Project the home nest's backup trust rows: the seal row (when enrolled) then
/// one writer row per configured destination, in config order.
///
/// `writer_nest_id` is the home nest's hex identity **as its connection proved
/// it** — the caller's bound nest id, never the source's `fauna.nest.info`
/// claim (module docs). `destinations` is the client's own pinned
/// `fauna.state.backup` destination list (source-untrusted). Only the source-nest
/// status read can fail the call — a per-destination failure degrades that row
/// to [`BackupTrustStatus::Unreachable`] and never the whole facet, so one dead
/// destination can't blank out the others or the seal row.
///
/// **The read carries a rotated box's seat** (`segment-backup-protocol.md`
/// § Cross-location backup protocol → *Where the carry runs*): over each
/// destination connection it opens, a seat naming a verified predecessor of
/// the bound identity is handed to it ([`crate::seat_carry`]), the chain read
/// from `source` — the bound box itself — and the row then reads the
/// destination's own answer after the handover. A carry that cannot complete
/// changes nothing the row shows: the row is the destination's list either way.
pub async fn backup_trust_rows(
    source: &dyn BackupNestSeam,
    writer_nest_id: &str,
    destinations: &[BackupDestination],
    connector: &dyn BackupDestinationConnector,
) -> Result<Vec<BackupTrustRow>, String> {
    let mut rows = Vec::new();

    if source.status().await?.enrolled {
        rows.push(BackupTrustRow {
            kind: BackupTrustKind::Seal,
            status: BackupTrustStatus::Active,
            since: None,
        });
    }

    // One row per DESTINATION, matching this function's own doc — a
    // destination with N covered folders has N+1 rows sharing one
    // `destination_id`, and a naive per-row loop would both over-query
    // `writer_row_state` and paint the same destination N+1 times here too.
    // The bound identity the carry moves a seat to — the same proven id every
    // row keys on. Unparseable, there is nothing to carry to.
    let bound = fauna_core::hex32::decode(writer_nest_id).ok();
    let chain = crate::seat_carry::SeamChain(source);
    for dest in &fauna_core::data::distinct_destinations(destinations) {
        let (status, since) = writer_row_state(writer_nest_id, bound, &chain, dest, connector)
            .await
            .unwrap_or((BackupTrustStatus::Unreachable, None));
        rows.push(BackupTrustRow {
            kind: BackupTrustKind::Writer {
                destination_id: dest.destination_id.clone(),
                destination_url: dest.destination_nest_url.clone(),
                destination_label: fauna_core::format::backup_destination_label(
                    dest.display_name.as_deref(),
                    &dest.destination_nest_url,
                ),
            },
            status,
            since,
        });
    }
    Ok(rows)
}

/// One destination's writer-grant lookup. `Err` ⇒ we could not ask (the caller
/// renders `unreachable`); `Ok` carries `Active` + `granted_at`, or `Missing`
/// when the destination answered and holds no grant in force for `writer_id` —
/// the seat is another nest's, or it is `writer_id`'s and revoked (a revoke
/// keeps the seat's row).
async fn writer_row_state(
    writer_id: &str,
    bound: Option<[u8; 32]>,
    chain: &dyn crate::seat_carry::RotationChainSource,
    destination: &BackupDestination,
    connector: &dyn BackupDestinationConnector,
) -> Result<(BackupTrustStatus, Option<i64>), String> {
    let seam = connect_destination(connector, destination).await?;
    let mut reply = seam.writer_grant_list().await?;
    if let Some(bound) = bound
        && crate::seat_carry::carries_a_seat(destination)
        // A failed carry is retried by the next read or audit pass; the row
        // below is the destination's list as it stands.
        && let Ok(crate::seat_carry::SeatCarry::Carried) =
            crate::seat_carry::carry_listed_seat(seam.as_ref(), &reply, &bound, chain).await
    {
        reply = seam.writer_grant_list().await?;
    }
    Ok(reply
        .grants
        .iter()
        // The nest id is hex on the wire; compare case-insensitively rather
        // than assuming both ends chose the same case.
        .find(|g| !g.revoked && g.writer_nest_id.eq_ignore_ascii_case(writer_id))
        .map(|g| (BackupTrustStatus::Active, Some(g.granted_at)))
        .unwrap_or((BackupTrustStatus::Missing, None)))
}

/// Revoke the seal grant at the **source** nest — the source nest can no longer
/// seal new segments for this owner. Custody already held at each destination is
/// untouched (`nests.md:96`), which is what the row's required honest-bound copy
/// says.
pub async fn revoke_backup_seal(source: &dyn BackupNestSeam) -> Result<(), String> {
    source.nest_key_revoke().await
}

/// What a successful [`revoke_backup_writer`] found at the destination.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WriterRevokeOutcome {
    /// The destination held the grant in force and revoked it.
    Revoked,
    /// The destination held no grant in force for the id, and its writer list
    /// confirms it: the grant was already revoked (a retry after an
    /// unacknowledged success) or never registered. Nothing is left for this
    /// writer to use.
    NothingToRevoke,
}

/// Revoke the source nest's writer grant at **one destination**, spoken over the
/// destination's own connection. Operable with the source nest hostile — which
/// is the entire point of the row.
///
/// `writer_nest_id` is the home nest's hex identity as its connection proved
/// it — the same id the row was built on ([`backup_trust_rows`]).
///
/// The destination answers `revoked: false` both for an id it never held and
/// for a retry after an unacknowledged success, so a `false` is settled by
/// re-reading the destination's writer list over the same connection: a grant
/// still listed in force is a revoke that did **not** happen and is an `Err`,
/// never a silent success; an absent or revoked one is
/// [`WriterRevokeOutcome::NothingToRevoke`].
pub async fn revoke_backup_writer(
    connector: &dyn BackupDestinationConnector,
    enrolled: &BackupDestination,
    writer_nest_id: &str,
) -> Result<WriterRevokeOutcome, String> {
    let destination_url = &enrolled.destination_nest_url;
    let destination = connect_destination(connector, enrolled).await?;
    if destination
        .writer_grant_revoke(writer_nest_id.to_string())
        .await?
    {
        return Ok(WriterRevokeOutcome::Revoked);
    }
    let still_held = destination
        .writer_grant_list()
        .await?
        .grants
        .iter()
        .any(|g| !g.revoked && g.writer_nest_id.eq_ignore_ascii_case(writer_nest_id));
    if still_held {
        return Err(format!(
            "{destination_url} did not revoke the writer grant it still holds"
        ));
    }
    Ok(WriterRevokeOutcome::NothingToRevoke)
}

#[cfg(test)]
mod tests {
    use super::*;
    use fauna_client_testkit::block_on;
    use fauna_protocol::backup::{BackupStatusReply, WriterGrantItem};
    use std::collections::HashMap;
    use std::sync::Mutex;

    /// A canned **source** seam: the two reads `backup_trust_rows` makes of the
    /// home nest, plus a recording revoke. The `fauna.backup.*` kind names and
    /// payload shapes are already pinned by the sibling `MockNest` tests in
    /// `lib.rs` (which do ride the real dag-cbor pipeline); these tests pin the
    /// row *projection*, so they drive the seam directly.
    struct MockSource {
        enrolled: bool,
        seal_revoked: Mutex<bool>,
        /// The bound box's rotation chain; `None` = the kind is not answered.
        chain: Option<Vec<fauna_protocol::nest_rotation::SignedNestRotation>>,
    }

    impl MockSource {
        fn enrolled(enrolled: bool) -> Self {
            Self {
                enrolled,
                seal_revoked: Mutex::new(false),
                chain: None,
            }
        }
    }

    #[async_trait]
    impl BackupNestSeam for MockSource {
        async fn status(&self) -> Result<BackupStatusReply, String> {
            Ok(BackupStatusReply {
                enrolled: self.enrolled,
                destinations: vec![],
                extra: Default::default(),
            })
        }
        async fn nest_key_revoke(&self) -> Result<(), String> {
            *self.seal_revoked.lock().unwrap() = true;
            Ok(())
        }
        async fn writer_grant_list(&self) -> Result<WriterGrantListReply, String> {
            panic!("the source nest must never be asked for writer grants")
        }
        async fn writer_grant_revoke(&self, _: String) -> Result<bool, String> {
            panic!("a writer-grant revoke must never land at the source nest")
        }
        async fn custody_list(&self, _cursor: Option<String>) -> Result<CustodyListReply, String> {
            panic!("custody lives at the destination — asking the source defeats the audit")
        }
        async fn generation_list(
            &self,
            _cursor: Option<String>,
        ) -> Result<GenerationListReply, String> {
            panic!("retained generations live at the destination, never at the source")
        }
        async fn generation_restore(
            &self,
            _: String,
            _: String,
            _: String,
        ) -> Result<GenerationRestoreReply, String> {
            panic!("a restore routed through the source defeats the rogue-source recovery")
        }
        async fn rotation_chain(
            &self,
        ) -> Result<Vec<fauna_protocol::nest_rotation::SignedNestRotation>, String> {
            self.chain
                .clone()
                .ok_or_else(|| "no rotation chain kind".into())
        }
    }

    /// The source nest's hex identity, as its connection proved it.
    const SOURCE_ID: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    /// A *different* nest — the "authorized, but not our source" case.
    const OTHER_ID: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";

    /// A destination that answers `writer_grant_list` with a canned grant set,
    /// or refuses the connect entirely (the `unreachable` path).
    struct MockDestination {
        grants: Mutex<Vec<WriterGrantItem>>,
        revoked: Mutex<Vec<String>>,
        /// Answers every revoke `revoked: false` while its list keeps the
        /// grant — a destination that did not perform the revoke.
        refuses_revoke: bool,
    }

    #[async_trait]
    impl BackupNestSeam for MockDestination {
        async fn status(&self) -> Result<BackupStatusReply, String> {
            panic!("a destination is never asked for backup status")
        }
        async fn nest_key_revoke(&self) -> Result<(), String> {
            panic!("the seal grant lives at the source, never at a destination")
        }
        async fn writer_grant_list(&self) -> Result<WriterGrantListReply, String> {
            Ok(WriterGrantListReply {
                grants: self.grants.lock().unwrap().clone(),
                extra: Default::default(),
            })
        }
        async fn writer_grant_revoke(&self, writer_nest_id: String) -> Result<bool, String> {
            let held = self
                .grants
                .lock()
                .unwrap()
                .iter()
                .any(|g| g.writer_nest_id.eq_ignore_ascii_case(&writer_nest_id));
            self.revoked.lock().unwrap().push(writer_nest_id);
            Ok(held && !self.refuses_revoke)
        }
        async fn custody_list(&self, _cursor: Option<String>) -> Result<CustodyListReply, String> {
            panic!("the trust facet reads grants, not custody — that is the audit's read")
        }
        async fn generation_list(
            &self,
            _cursor: Option<String>,
        ) -> Result<GenerationListReply, String> {
            panic!("the trust facet reads grants — generations are the recovery surface's read")
        }
        async fn generation_restore(
            &self,
            _: String,
            _: String,
            _: String,
        ) -> Result<GenerationRestoreReply, String> {
            panic!("the trust facet never restores — that is the recovery surface's write")
        }
        async fn writer_grant_succeed(
            &self,
            writer_nest_id: String,
            _succeeds: String,
        ) -> Result<(), String> {
            *self.grants.lock().unwrap() = vec![granted(&writer_nest_id, 1_800_000_000)];
            Ok(())
        }
    }

    /// The identity every [`dest`] row enrolled, and the one an honest
    /// [`MockConnector`] destination proves.
    const ENROLLED_ID: [u8; 32] = [7u8; 32];

    /// Maps URL → canned destination; an unmapped URL fails the connect.
    /// Every connection proves [`ENROLLED_ID`] unless `proves` overrides it —
    /// the box that answers the URL is then not the one the owner enrolled.
    #[derive(Default)]
    struct MockConnector {
        by_url: HashMap<String, Arc<MockDestination>>,
        proves: Option<[u8; 32]>,
    }

    impl MockConnector {
        fn with(mut self, url: &str, grants: Vec<WriterGrantItem>) -> Self {
            self.by_url.insert(
                url.to_string(),
                Arc::new(MockDestination {
                    grants: Mutex::new(grants),
                    revoked: Mutex::new(Vec::new()),
                    refuses_revoke: false,
                }),
            );
            self
        }

        fn with_refusing(mut self, url: &str, grants: Vec<WriterGrantItem>) -> Self {
            self.by_url.insert(
                url.to_string(),
                Arc::new(MockDestination {
                    grants: Mutex::new(grants),
                    revoked: Mutex::new(Vec::new()),
                    refuses_revoke: true,
                }),
            );
            self
        }

        fn proving(mut self, id: [u8; 32]) -> Self {
            self.proves = Some(id);
            self
        }
    }

    #[async_trait]
    impl BackupDestinationConnector for MockConnector {
        async fn connect(&self, url: &str) -> Result<DestinationConnection, String> {
            self.by_url
                .get(url)
                .map(|d| DestinationConnection {
                    seam: d.clone() as Arc<dyn BackupNestSeam>,
                    bound_nest_id: self.proves.unwrap_or(ENROLLED_ID),
                })
                .ok_or_else(|| format!("unreachable: {url}"))
        }
    }

    fn dest(id: &str, url: &str, name: Option<&str>) -> BackupDestination {
        BackupDestination {
            destination_id: id.into(),
            destination_nest_url: url.into(),
            destination_actor_pubkey: ENROLLED_ID,
            folder_name: "__mail".into(),
            added_at: 0,
            display_name: name.map(|s| s.to_string()),
            ..Default::default()
        }
    }

    fn granted(writer: &str, at: i64) -> WriterGrantItem {
        WriterGrantItem {
            writer_nest_id: writer.into(),
            granted_at: at,
            ..Default::default()
        }
    }

    #[test]
    fn enrolled_owner_with_no_destinations_gets_the_seal_row_only() {
        let src = MockSource::enrolled(true);
        let rows = block_on(backup_trust_rows(
            &src,
            SOURCE_ID,
            &[],
            &MockConnector::default(),
        ))
        .unwrap();

        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].kind, BackupTrustKind::Seal);
        assert_eq!(rows[0].status, BackupTrustStatus::Active);
        assert_eq!(rows[0].since, None, "the seal grant carries no timestamp");
    }

    #[test]
    fn an_unenrolled_owner_gets_no_seal_row() {
        let src = MockSource::enrolled(false);
        let rows = block_on(backup_trust_rows(
            &src,
            SOURCE_ID,
            &[],
            &MockConnector::default(),
        ))
        .unwrap();
        assert!(rows.is_empty());
    }

    #[test]
    fn a_destination_holding_the_source_writer_grant_is_active_with_its_granted_at() {
        let src = MockSource::enrolled(true);
        let conn = MockConnector::default().with(
            "https://aunt.example",
            vec![granted(SOURCE_ID, 1_700_000_000)],
        );
        let rows = block_on(backup_trust_rows(
            &src,
            SOURCE_ID,
            &[dest("d1", "https://aunt.example", Some("Aunt's nest"))],
            &conn,
        ))
        .unwrap();

        assert_eq!(rows.len(), 2, "seal row then one writer row");
        assert_eq!(rows[1].status, BackupTrustStatus::Active);
        assert_eq!(rows[1].since, Some(1_700_000_000));
        match &rows[1].kind {
            BackupTrustKind::Writer {
                destination_id,
                destination_label,
                ..
            } => {
                assert_eq!(destination_id, "d1");
                assert_eq!(destination_label, "Aunt's nest");
            }
            other => panic!("expected a writer row, got {other:?}"),
        }
    }

    /// a destination with an attached folder has two rows sharing
    /// one `destination_id` — the enrollment row plus a coverage clone — and
    /// this function's own doc promises "one writer row per configured
    /// destination".
    #[test]
    fn a_destination_with_a_covered_folder_gets_one_writer_row_not_one_per_coverage_row() {
        let src = MockSource::enrolled(true);
        let conn = MockConnector::default().with(
            "https://aunt.example",
            vec![granted(SOURCE_ID, 1_700_000_000)],
        );
        let enrolled = dest("d1", "https://aunt.example", Some("Aunt's nest"));
        let covered = BackupDestination {
            folder_name: "__folder/deadbeef/1".into(),
            ..enrolled.clone()
        };
        let rows = block_on(backup_trust_rows(
            &src,
            SOURCE_ID,
            &[enrolled, covered],
            &conn,
        ))
        .unwrap();

        assert_eq!(rows.len(), 2, "seal row then exactly one writer row");
    }

    #[test]
    fn a_destination_answering_without_the_source_grant_is_missing_not_unreachable() {
        let src = MockSource::enrolled(true);
        // The destination is reachable and authorizes *some other* nest.
        let conn =
            MockConnector::default().with("https://aunt.example", vec![granted(OTHER_ID, 42)]);
        let rows = block_on(backup_trust_rows(
            &src,
            SOURCE_ID,
            &[dest("d1", "https://aunt.example", None)],
            &conn,
        ))
        .unwrap();

        assert_eq!(rows[1].status, BackupTrustStatus::Missing);
        assert_eq!(rows[1].since, None);
    }

    /// **The read carries a rotated box's seat** (`segment-backup-protocol.md`
    /// § *Where the carry runs*): the destination's seat names the box's
    /// predecessor, the bound box's chain verifies the hop, so the facet's
    /// read hands the seat over and paints the row from the destination's
    /// answer after it — Active, under the bound identity.
    #[test]
    fn the_read_carries_a_rotated_boxs_seat_and_paints_the_row_after_it() {
        use fauna_core::identity::ActorKeypair;
        use fauna_protocol::nest_rotation::NestRotation;
        let (p, b) = (
            ActorKeypair::from_secret([0x11; 32]),
            ActorKeypair::from_secret([0x22; 32]),
        );
        let hop = NestRotation {
            old_nest_actor_id: p.actor_id().0,
            new_nest_actor_id: b.actor_id().0,
            seq: 1,
            rotated_at: 1_800_000_000,
        }
        .sign(p.signing_key(), b.signing_key())
        .unwrap();
        let src = MockSource {
            chain: Some(vec![hop]),
            ..MockSource::enrolled(true)
        };
        let bound_hex = fauna_core::hex32::encode(&b.actor_id().0);
        let conn = MockConnector::default().with(
            "https://aunt.example",
            vec![granted(
                &fauna_core::hex32::encode(&p.actor_id().0),
                1_700_000_000,
            )],
        );

        let rows = block_on(backup_trust_rows(
            &src,
            &bound_hex,
            &[dest("d1", "https://aunt.example", None)],
            &conn,
        ))
        .unwrap();

        assert_eq!(rows[1].status, BackupTrustStatus::Active);
        assert_eq!(rows[1].since, Some(1_800_000_000), "the handover's grant");
        let held = conn.by_url["https://aunt.example"]
            .grants
            .lock()
            .unwrap()
            .clone();
        assert_eq!(held.len(), 1);
        assert_eq!(held[0].writer_nest_id, bound_hex);
    }

    #[test]
    fn an_unreachable_destination_degrades_only_its_own_row() {
        let src = MockSource::enrolled(true);
        // Only the second destination is reachable.
        let conn =
            MockConnector::default().with("https://up.example", vec![granted(SOURCE_ID, 99)]);
        let rows = block_on(backup_trust_rows(
            &src,
            SOURCE_ID,
            &[
                dest("down", "https://down.example", None),
                dest("up", "https://up.example", None),
            ],
            &conn,
        ))
        .unwrap();

        assert_eq!(rows.len(), 3);
        assert_eq!(
            rows[0].status,
            BackupTrustStatus::Active,
            "seal row survives"
        );
        assert_eq!(rows[1].status, BackupTrustStatus::Unreachable);
        assert_eq!(
            rows[2].status,
            BackupTrustStatus::Active,
            "a dead sibling must not blank a live destination"
        );
    }

    #[test]
    fn the_writer_grant_lookup_is_hex_case_insensitive() {
        let src = MockSource::enrolled(true);
        let conn = MockConnector::default().with(
            "https://aunt.example",
            vec![granted(&SOURCE_ID.to_uppercase(), 7)],
        );
        let rows = block_on(backup_trust_rows(
            &src,
            SOURCE_ID,
            &[dest("d1", "https://aunt.example", None)],
            &conn,
        ))
        .unwrap();

        assert_eq!(rows[1].status, BackupTrustStatus::Active);
    }

    #[test]
    fn a_labelless_destination_falls_back_to_the_shared_label_helper() {
        let src = MockSource::enrolled(true);
        let conn = MockConnector::default().with("https://aunt.example", vec![]);
        let rows = block_on(backup_trust_rows(
            &src,
            SOURCE_ID,
            &[dest("d1", "https://aunt.example", None)],
            &conn,
        ))
        .unwrap();

        match &rows[1].kind {
            BackupTrustKind::Writer {
                destination_label, ..
            } => assert_eq!(
                destination_label,
                &fauna_core::format::backup_destination_label(None, "https://aunt.example"),
                "never re-derive the label locally"
            ),
            other => panic!("expected a writer row, got {other:?}"),
        }
    }

    #[test]
    fn revoking_a_writer_grant_speaks_to_the_destination_not_the_source() {
        let conn =
            MockConnector::default().with("https://aunt.example", vec![granted(SOURCE_ID, 1)]);
        let target = conn.by_url["https://aunt.example"].clone();

        let outcome = block_on(revoke_backup_writer(
            &conn,
            &dest("d1", "https://aunt.example", None),
            SOURCE_ID,
        ))
        .unwrap();

        assert_eq!(outcome, WriterRevokeOutcome::Revoked);
        assert_eq!(
            target.revoked.lock().unwrap().as_slice(),
            &[SOURCE_ID.to_string()],
            "the revoke must land at the destination — it is the hostile-source path"
        );
    }

    /// `revoked: false` is also the idempotent-retry answer, so it alone proves
    /// nothing: the destination's own list settles it. Still listed ⇒ the
    /// revoke did not happen and the caller must hear so.
    #[test]
    fn a_revoke_answered_false_while_the_grant_is_still_listed_is_an_error() {
        let conn = MockConnector::default()
            .with_refusing("https://aunt.example", vec![granted(SOURCE_ID, 1)]);
        let err = block_on(revoke_backup_writer(
            &conn,
            &dest("d1", "https://aunt.example", None),
            SOURCE_ID,
        ))
        .expect_err("a grant still held after the revoke must not read as revoked");
        assert!(
            err.contains("aunt.example"),
            "error names the destination: {err}"
        );
    }

    #[test]
    fn a_revoke_answered_false_with_no_grant_listed_is_nothing_to_revoke() {
        let conn =
            MockConnector::default().with("https://aunt.example", vec![granted(OTHER_ID, 1)]);
        let outcome = block_on(revoke_backup_writer(
            &conn,
            &dest("d1", "https://aunt.example", None),
            SOURCE_ID,
        ))
        .unwrap();
        assert_eq!(outcome, WriterRevokeOutcome::NothingToRevoke);
    }

    /// A box answering the destination URL — valid TLS, a lapsed
    /// or hijacked domain — that proves an identity other than the one the
    /// owner enrolled is not the destination: its writer-grant view is not
    /// rendered as the destination's, and a revoke is never spoken to it (it
    /// would answer "revoked" and the real grant would survive).
    #[test]
    fn a_destination_proving_a_different_identity_than_enrolled_is_refused() {
        let impostor = [9u8; 32];
        let conn = MockConnector::default()
            .with("https://aunt.example", vec![granted(SOURCE_ID, 1)])
            .proving(impostor);
        let target = conn.by_url["https://aunt.example"].clone();

        let rows = block_on(backup_trust_rows(
            &MockSource::enrolled(true),
            SOURCE_ID,
            &[dest("d1", "https://aunt.example", None)],
            &conn,
        ))
        .unwrap();
        assert_eq!(
            rows[1].status,
            BackupTrustStatus::Unreachable,
            "an impostor's grant list must not paint the destination's row"
        );

        let err = block_on(revoke_backup_writer(
            &conn,
            &dest("d1", "https://aunt.example", None),
            SOURCE_ID,
        ))
        .expect_err("a revoke must never be spoken to a box that is not the destination");
        assert!(
            err.contains("aunt.example"),
            "error names the destination: {err}"
        );
        assert!(
            target.revoked.lock().unwrap().is_empty(),
            "the impostor was never asked to revoke"
        );

        let err = block_on(connect_destination(
            &conn,
            &dest("d1", "https://aunt.example", None),
        ))
        .err()
        .expect("the shared door refuses the connection");
        assert!(
            err.contains(&fauna_core::hex32::encode(&impostor)),
            "names the proven id: {err}"
        );
    }

    /// **The enroll half.** The id a destination is enrolled under is
    /// the one its connection proved; a `fauna.nest.info` claim naming another
    /// nest (or nothing parseable) refuses the enroll instead of being recorded.
    #[test]
    fn a_destination_is_enrolled_under_its_proven_id_and_a_disagreeing_claim_refuses() {
        let url = "https://aunt.example";
        let proven = [7u8; 32];
        assert_eq!(
            proven_destination_id(url, proven, &fauna_core::hex32::encode(&proven)),
            Ok(proven)
        );
        let err = proven_destination_id(url, proven, &fauna_core::hex32::encode(&[9u8; 32]))
            .expect_err("a claim naming another nest must not be enrolled");
        assert!(err.contains("aunt.example"), "names the destination: {err}");
        proven_destination_id(url, proven, "not-hex").expect_err("a malformed claim refuses");
    }

    #[test]
    fn revoking_the_seal_grant_speaks_to_the_source_nest() {
        let src = MockSource::enrolled(true);
        block_on(revoke_backup_seal(&src)).unwrap();
        assert!(*src.seal_revoked.lock().unwrap());
    }

    #[test]
    fn revoking_a_writer_grant_at_an_unreachable_destination_reports_the_failure() {
        let conn = MockConnector::default();
        let err = block_on(revoke_backup_writer(
            &conn,
            &dest("d1", "https://gone.example", None),
            SOURCE_ID,
        ))
        .expect_err("an unreachable destination must not report a silent success");
        assert!(
            err.contains("gone.example"),
            "error names the destination: {err}"
        );
    }
}
