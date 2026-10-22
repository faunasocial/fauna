//! The conversations oracle's door, hosted at login by every app that holds
//! the account's conversations engine (`conversations-oracle.md` § The
//! custodian and the door) — one call per app, the assembly here.
//!
//! The custodian's account is [`RuntimeAccount`], built over this crate's
//! concrete stores: the app's [`NestClient`] for the roster and the record
//! kind, the account store's meta rows through the handle the app's runtime
//! slot resolves, the owner's grant log, and the content index the app's
//! Search page already resolves. The door is kept by
//! [`serve::keep_door`] for as long as the returned [`ConversationsDoor`]
//! lives: an app holds it beside its conversations session, so signing out
//! or losing the engine drops it, the door is given up, and the account's
//! next app takes it over on its next tick.

use std::sync::Arc;

use fauna_client::NestClient;
use fauna_client_config::SuccessionLedgerStore;
use fauna_client_conversations_oracle::ConversationsCustodian;
use fauna_client_conversations_oracle::runtime::{MetaRows, NestPrincipals, RuntimeAccount};
use fauna_client_conversations_oracle::serve;
use fauna_client_search::LocalIndexResolver;
use fauna_conversations::ConversationsManager;
use fauna_ipc::oracle::DoorFuture;
use fauna_protocol::RpcRequester;
use fauna_protocol::principals::{
    ListPrincipalsReply, ListPrincipalsRequest, PrincipalInfo, RecordPrincipalOpsReply,
    RecordPrincipalOpsRequest,
};
use fauna_sync_engine::account_runtime::AccountStoreHandle;

/// The roster read and the record kind over the app's own nest session.
struct NestRpc(Arc<NestClient>);

impl NestPrincipals for NestRpc {
    fn list_principals(&self) -> DoorFuture<'_, Result<Vec<PrincipalInfo>, String>> {
        Box::pin(async move {
            self.0
                .request::<_, ListPrincipalsReply>(
                    "fauna.principals.list",
                    ListPrincipalsRequest::default(),
                )
                .await
                .map(|reply| reply.principals)
                .map_err(|e| e.to_string())
        })
    }

    fn record_ops(
        &self,
        request: RecordPrincipalOpsRequest,
    ) -> DoorFuture<'_, Result<u32, String>> {
        Box::pin(async move {
            self.0
                .request::<_, RecordPrincipalOpsReply>("fauna.principals.ops.record", request)
                .await
                .map(|reply| reply.recorded)
                .map_err(|e| e.to_string())
        })
    }
}

/// The account store's meta rows, through the handle the app's runtime slot
/// holds — resolved at every call, so a door started before the store
/// assembles reads "not open yet" (the ceiling refusing) until it does.
struct StoreMeta(Arc<dyn Fn() -> Option<AccountStoreHandle> + Send + Sync>);

impl StoreMeta {
    fn handle(&self) -> Result<AccountStoreHandle, String> {
        (self.0)().ok_or_else(|| "the account store is not open yet".to_string())
    }
}

impl MetaRows for StoreMeta {
    fn get<'a>(&'a self, key: &'a str) -> DoorFuture<'a, Result<Option<Vec<u8>>, String>> {
        Box::pin(async move {
            self.handle()?
                .meta_get(key)
                .await
                .map_err(|e| format!("{e:#}"))
        })
    }

    fn put<'a>(&'a self, key: &'a str, bytes: Vec<u8>) -> DoorFuture<'a, Result<(), String>> {
        Box::pin(async move {
            self.handle()?
                .meta_put(key, bytes)
                .await
                .map_err(|e| format!("{e:#}"))
        })
    }
}

/// What an app brings: every field a per-app fact it already holds at the
/// point its conversations engine opens.
pub struct ConversationsDoorInputs {
    /// The account the door serves.
    pub actor_id: [u8; 32],
    /// The page's own manager — what the custodian performs through.
    pub manager: Arc<ConversationsManager>,
    /// The app's authenticated nest session.
    pub nest: Arc<NestClient>,
    /// The owner's grant log (a resolving door over the runtime slot).
    pub ledger: Arc<dyn SuccessionLedgerStore>,
    /// The account store's handle, as the app's runtime slot resolves it.
    pub store: Arc<dyn Fn() -> Option<AccountStoreHandle> + Send + Sync>,
    /// The device's content index, as the Search page resolves it; `None`
    /// when the app wires none.
    pub index: Option<Arc<dyn LocalIndexResolver>>,
}

/// The account's door, kept while this value lives.
pub struct ConversationsDoor {
    _may_serve: tokio::sync::watch::Sender<bool>,
}

/// Start keeping the account's door on `runtime`. `None` when the account's
/// endpoint cannot be resolved on this device (logged); the app then serves
/// no door, which the principal's tool names as the declared gap.
pub fn spawn_conversations_door(
    inputs: ConversationsDoorInputs,
    runtime: &tokio::runtime::Handle,
) -> Option<ConversationsDoor> {
    let account = RuntimeAccount::new(
        inputs.actor_id,
        Arc::new(NestRpc(inputs.nest)),
        inputs.ledger,
        Arc::new(StoreMeta(inputs.store)),
        inputs.index,
    );
    let custodian = Arc::new(ConversationsCustodian::new(account, inputs.manager));
    let keeper = match serve::DoorKeeper::new(custodian) {
        Ok(keeper) => keeper,
        Err(e) => {
            tracing::warn!("the conversations oracle's door has no endpoint here: {e}");
            return None;
        }
    };
    let (may_serve, rx) = tokio::sync::watch::channel(true);
    runtime.spawn(serve::keep_door(keeper, rx));
    Some(ConversationsDoor {
        _may_serve: may_serve,
    })
}
