//! The door's endpoint lock and its serving loop
//! (`conversations-oracle.md` § The custodian and the door: **one app serves
//! at a time**).
//!
//! Every signed-in app hosting the account's conversations engine may serve
//! the account's door, and exactly one does: the endpoint is taken under the
//! agent's single-instance shape ([`OracleEndpoint::lock`]) — a
//! kernel-arbitrated exclusive `flock` on `<socket>.lock` on unix, the door's
//! own named mutex on windows — per (OS user, account), since the endpoint
//! name carries both. First come; released when that app signs out, exits,
//! or loses its engine role, and a second app takes it over on its next
//! [`DoorKeeper::tick`].

use std::io;
use std::sync::Arc;
use std::time::Duration;

use fauna_ipc::oracle::{EndpointLock, OracleEndpoint, OracleListener};

use crate::{ConversationsCustodian, CustodianAccount};

/// A door this app is serving: the endpoint lock, and the server task.
pub struct ServingDoor {
    _lock: EndpointLock,
    shutdown: tokio::sync::watch::Sender<bool>,
    task: tokio::task::JoinHandle<io::Result<()>>,
}

impl ServingDoor {
    /// Stop serving and give the endpoint up for the next app.
    pub async fn stop(self) {
        let _ = self.shutdown.send(true);
        match self.task.await {
            Ok(Err(e)) => tracing::warn!("the oracle door's server failed: {e}"),
            Err(e) => tracing::warn!("the oracle door's server task ended abnormally: {e}"),
            Ok(Ok(())) => {}
        }
        // `_lock` drops here, after the endpoint is gone.
    }

    /// Whether the server ended without being asked to — an accept that
    /// failed; the keeper then gives the endpoint up and takes it afresh.
    fn has_ended(&self) -> bool {
        self.task.is_finished()
    }
}

/// Take the account's door if no other app holds it: `Ok(None)` when another
/// app serves it already. The endpoint is bound before this answers, so a
/// principal reaches the door from then on. Needs a tokio runtime.
pub fn try_serve<A: CustodianAccount>(
    endpoint: &OracleEndpoint,
    custodian: Arc<ConversationsCustodian<A>>,
) -> io::Result<Option<ServingDoor>> {
    let lock = match endpoint.lock() {
        Ok(lock) => lock,
        Err(e) if e.kind() == io::ErrorKind::AddrInUse => return Ok(None),
        Err(e) => return Err(e),
    };
    // Bound under the lock: the unix stale-socket unlink is safe only there.
    // On windows a name held from another logon session (this user's app
    // there) or by another account fails this, and the lock goes with it.
    let listener = OracleListener::bind(endpoint)?;
    let (shutdown, rx) = tokio::sync::watch::channel(false);
    let task = tokio::spawn(listener.serve(custodian, rx));
    Ok(Some(ServingDoor {
        _lock: lock,
        shutdown,
        task,
    }))
}

/// How often [`keep_door`] ticks: a second app takes a door the first gave up
/// within this, and the record pump runs this often.
pub const DOOR_TICK: Duration = Duration::from_secs(5);

/// What an app's account runtime ticks to keep the account's door served for
/// as long as it may serve it.
pub struct DoorKeeper<A: CustodianAccount> {
    endpoint: OracleEndpoint,
    custodian: Arc<ConversationsCustodian<A>>,
    serving: Option<ServingDoor>,
}

impl<A: CustodianAccount> DoorKeeper<A> {
    /// The keeper of `custodian`'s account's door, at the account's endpoint.
    pub fn new(custodian: Arc<ConversationsCustodian<A>>) -> io::Result<Self> {
        let endpoint = OracleEndpoint::for_account(&custodian.account().actor_id())?;
        Ok(Self::at(endpoint, custodian))
    }

    /// A keeper at an explicit endpoint (tests, and a harness-isolated launch
    /// whose agent dir moved with its runtime dir).
    pub fn at(endpoint: OracleEndpoint, custodian: Arc<ConversationsCustodian<A>>) -> Self {
        Self {
            endpoint,
            custodian,
            serving: None,
        }
    }

    /// Whether this app holds the door now.
    pub fn is_serving(&self) -> bool {
        self.serving.is_some()
    }

    /// One tick: serve while `may_serve` (signed in, holding the account's
    /// conversations engine) and the endpoint is free; stop the moment it is
    /// not. Answers whether this app serves the door after the tick.
    pub async fn tick(&mut self, may_serve: bool) -> bool {
        if self.serving.as_ref().is_some_and(ServingDoor::has_ended)
            && let Some(ended) = self.serving.take()
        {
            ended.stop().await;
        }
        match (may_serve, self.serving.is_some()) {
            (true, false) => match try_serve(&self.endpoint, self.custodian.clone()) {
                Ok(serving) => self.serving = serving,
                Err(e) => tracing::warn!("taking the oracle door failed: {e}"),
            },
            (false, true) => {
                if let Some(serving) = self.serving.take() {
                    serving.stop().await;
                }
            }
            _ => {}
        }
        self.serving.is_some()
    }
}

/// Keep `keeper`'s door for as long as `may_serve` lives: tick at every
/// change of it and every [`DOOR_TICK`] — serving while it reads `true` and
/// the endpoint is free, running the account's upkeep (the record pump) on
/// every tick — and give the door up when the sender drops (the app signed
/// out, or its conversations session ended). The app holds the sender for
/// as long as it holds the account's conversations engine.
pub async fn keep_door<A: CustodianAccount>(
    keeper: DoorKeeper<A>,
    may_serve: tokio::sync::watch::Receiver<bool>,
) {
    keep_door_every(keeper, may_serve, DOOR_TICK).await;
}

/// [`keep_door`] at an explicit tick (tests).
pub async fn keep_door_every<A: CustodianAccount>(
    mut keeper: DoorKeeper<A>,
    mut may_serve: tokio::sync::watch::Receiver<bool>,
    tick: Duration,
) {
    loop {
        let may = *may_serve.borrow_and_update();
        keeper.tick(may).await;
        keeper.custodian.account().upkeep().await;
        tokio::select! {
            changed = may_serve.changed() => {
                if changed.is_err() {
                    break;
                }
            }
            () = tokio::time::sleep(tick) => {}
        }
    }
    keeper.tick(false).await;
}
