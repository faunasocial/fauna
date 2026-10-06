//! The **group-reception key** seam over the account store — one
//! implementation for every app that hosts the account runtime (priority #2),
//! the same reason the rest of this crate exists.
//!
//! A community conversation room seals its content under a room generation key
//! that reaches each member as an X-Wing wrap addressed to that member's
//! group-reception public half; the secret half rests on the **account plane**
//! as `fauna.state.group-reception-key`
//! (`docs/goal/behavior/conversation-rooms.md` § The three classes →
//! *Community*). `fauna-conversations` mints, persists and opens those wraps
//! but must never learn about the account runtime — the priority-#2 boundary
//! every seam in `fauna_conversations::backend` draws — so this is the few
//! lines that join the two, in the crate that sits above both and that
//! nothing depends back on.
//!
//! **Web registers this seam too, once it hosts the runtime** — the same plane
//! kind, served by the same `AccountStatePlane` over web's own store backend,
//! registered through the same [`crate::conversation_seams::wire`]; no web
//! twin of this seam is ever written (`community-rooms.md` § Implementation
//! status today, the founding entry; the program:
//! `account-client-lifecycle.md` § The client-side lifecycle → *The trigger
//! fired*). Until that host lands, the SPA registers nothing and the backend
//! takes the honest unset path: a community room's records stay unopened
//! rather than opening under a key that was never there, and founding one is
//! refused by name rather than half-done.

use std::sync::Arc;

use fauna_account_plane::account_driver::AccountStoreHandle;
use fauna_core::group_generation::GroupReceptionKeyRecord;

/// [`fauna_conversations::backend::GroupReceptionKeys`] over this
/// account's own store — hand it to
/// `ConversationsSession::set_group_reception_keys`.
pub struct AccountGroupReceptionKeys {
    store: AccountStoreHandle,
}

impl AccountGroupReceptionKeys {
    /// Wrap a live account-store handle as the conversations seam.
    #[must_use]
    pub fn new(store: AccountStoreHandle) -> Arc<AccountGroupReceptionKeys> {
        Arc::new(AccountGroupReceptionKeys { store })
    }
}

#[cfg_attr(not(target_arch = "wasm32"), async_trait::async_trait)]
#[cfg_attr(target_arch = "wasm32", async_trait::async_trait(?Send))]
impl fauna_conversations::backend::GroupReceptionKeys for AccountGroupReceptionKeys {
    async fn reception_keys(&self) -> Vec<GroupReceptionKeyRecord> {
        match self.store.group_reception_keys().await {
            Ok(records) => records,
            Err(e) => {
                // A store that cannot answer is not an account with no keys:
                // the records may be there and unreadable this instant (the
                // runtime restarting, a busy database). Empty is what the
                // caller gets either way, but saying so is the difference
                // between "this room is not readable on this device" and a
                // silence nobody can chase.
                tracing::warn!(
                    error = %e,
                    "the account store could not serve its group-reception keys — \
                     community-room records stay unopened this pass"
                );
                Vec::new()
            }
        }
    }

    async fn put_reception_key(&self, record: GroupReceptionKeyRecord) -> bool {
        match self.store.put_group_reception_key(record).await {
            Ok(_) => true,
            Err(e) => {
                // Louder than the read's warning, because the caller is about
                // to abandon a seating on the strength of it: a founder that
                // cannot persist its wrap target must not hand the public half
                // to a room, or the room keys itself to a secret this account
                // will not have after the next launch.
                // The whole chain: a refused first-need mint answers the
                // standing no-tip refusal with what actually failed as its
                // cause, and the outermost line alone names neither.
                tracing::error!(
                    error = %format_args!("{e:#}"),
                    "the account store could not persist a group-reception keypair — \
                     the room seating that needed it is abandoned"
                );
                false
            }
        }
    }
}
