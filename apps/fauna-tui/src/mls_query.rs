//! The tui impl of the member-row join-filter seam
//! ([`fauna_protocol::member_rows::MlsQuery`]) — one answer for every page
//! machine that reads `fauna.folders.list`'s member-visible projection: the
//! Folders page's `DevicesMachine` (`settings::devices::wire_mls_query`) and the
//! Media page's `MediaMachine` (`media::wire_mls_query`). Each machine drops
//! every member row this answers `false` for, so a rostered-but-un-joined knock
//! never reaches a snapshot — a stranger's share surfaces ONLY as a
//! `folder-pending-share` (`ui/folders.md` § Sharing: "a stranger cannot force a
//! set into your list"; `ui/media.md` § Layout & flow → *Where the offerable
//! sets come from*).
//!
//! ⚠ **Until a machine is wired it is fail-safe, which means EVERY member row is
//! dropped** — so a set legitimately shared *with* this user is invisible on
//! that page, no matter what it paints. tui shipped the Folders page un-wired
//! from the core control plane (2026-07-22) until 2026-07-30; the recipient half
//! of the sharing slice could not work without it.
//!
//! Reads the ONE live per-actor `MlsEngine` off the conversations rail
//! (`ConversationsSession::engine`, over the single `mls_state.db`) — never a
//! second engine racing the SQLite file. Unlike linux's `LinuxMlsQuery`, which
//! re-resolves a process-global `active_session()` on every call, tui holds the
//! session `Arc` directly: `session.rs` starts the conversations session *before*
//! the post-auth wiring, so the handle exists by wiring time, and a re-login
//! rebuilds both. Derives the set's `ChannelId` the SAME way the nest does
//! (`ChannelId::from_group_id_hex` — openMLS group ids are variable length, so
//! NOT `hex32`; the same derivation `settings::folders` uses).

use std::sync::Arc;

use fauna_devices_machine::MlsQuery;

pub(crate) struct TuiMlsQuery {
    session: Arc<fauna_conversations::ConversationsSession>,
}

impl TuiMlsQuery {
    pub(crate) fn new(session: &Arc<fauna_conversations::ConversationsSession>) -> Arc<Self> {
        Arc::new(Self {
            session: Arc::clone(session),
        })
    }
}

impl MlsQuery for TuiMlsQuery {
    fn is_joined_shared_set(&self, mls_group_id_hex: &str) -> bool {
        let Ok(channel_id) = fauna_mls::types::ChannelId::from_group_id_hex(mls_group_id_hex)
        else {
            return false;
        };
        self.session.engine().has_group(&channel_id)
    }
}
