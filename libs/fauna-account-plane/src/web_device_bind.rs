//! The web host's in-band device bind — the sender of
//! `fauna.auth.device_bind` (`devices.md` § Listing Devices → *The binding* →
//! *Web binds its one socket in band*; `account-client-lifecycle.md` § The
//! client-side lifecycle → *The trigger fired*, ruling (4)'s decision (e)).
//!
//! Web's one socket is seed-minted, so no bearer binds it to the tab's device.
//! [`run`] binds it over the session's own requester: once this runtime holds
//! the engine role and its grant is on the nest, and again on every new
//! connection, which carries no binding. The writer key is read from the slot
//! at each send, so a rotated writer signs the next bind.
//!
//! A refusal is logged once per connection and waits for the next trigger: no
//! retry loop, and never a dial. `not_registered` voids the registration latch
//! as a native principal client's refused handshake does; the driver's gate
//! never closes again, so the latch coming back is what re-sends the bind.
//!
//! Compiled natively for its tests alone: the loop is plain over its seats, and
//! the browser leg is [`crate::web_host`]'s.

use std::sync::Arc;
use std::time::Duration;

use ed25519_dalek::{Signer, SigningKey};
use fauna_core::identity::ActorId;
use fauna_protocol::auth::{
    DEVICE_BIND_KIND, DeviceBindReply, DeviceBindRequest, device_bind_signed_message,
};
use fauna_protocol::{RpcErrorClass, RpcRequester};
use tokio::sync::{oneshot, watch};

use crate::principal_bundle::{self, SecretStore};

/// How often a bind answered `not_registered` re-reads the shared slot's
/// registration latch for the pass that heals it. A local credential read,
/// never a dial.
const LATCH_POLL: Duration = Duration::from_secs(5);

/// The nest identity this browser pinned for the serving origin, when it has
/// one — what the bind's signature binds, read at each send
/// ([`fauna_client_core::nest_trust::read_bound_identity`] proves it over the
/// session's own socket otherwise).
pub type PinnedIdentity = Box<dyn Fn() -> Option<[u8; 32]>>;

/// What the bind reads and whom it answers to.
pub(crate) struct DeviceBindSeats<R, S> {
    pub rpc: R,
    pub credentials: Arc<S>,
    pub actor_id: ActorId,
    pub pinned_identity: PinnedIdentity,
    /// [`AccountStoreHandle::subscribe_grant_registered`].
    ///
    /// [`AccountStoreHandle::subscribe_grant_registered`]: crate::account_driver::AccountStoreHandle::subscribe_grant_registered
    pub grant_registered: watch::Receiver<bool>,
    /// [`AccountStoreHandle::subscribe_engine_role`] — only the role's holder
    /// binds, so a browser profile has at most one bound socket for its row.
    ///
    /// [`AccountStoreHandle::subscribe_engine_role`]: crate::account_driver::AccountStoreHandle::subscribe_engine_role
    pub engine_role: watch::Receiver<bool>,
    /// The session's reconnect counter; `None` binds the first connection only.
    pub reconnects: Option<watch::Receiver<u64>>,
}

/// What one send came to.
enum Sent {
    Bound,
    /// The nest holds no grant for this key: the latch is void.
    NotRegistered,
    Failed,
}

/// Bind the session's socket until `stop` resolves or the runtime's watches
/// close. Exactly one bind per connection reaches the nest while it succeeds.
pub(crate) async fn run<R, S>(seats: DeviceBindSeats<R, S>, mut stop: oneshot::Receiver<()>)
where
    R: RpcRequester,
    R::Error: RpcErrorClass,
    S: SecretStore,
{
    let DeviceBindSeats {
        rpc,
        credentials,
        actor_id,
        pinned_identity,
        mut grant_registered,
        mut engine_role,
        mut reconnects,
    } = seats;
    let actor_id_hex = hex::encode(actor_id.0);
    // The connection already bound, and the one whose refusal was logged.
    let mut bound_on: Option<u64> = None;
    let mut refused_on: Option<u64> = None;
    let mut latch_void = false;
    loop {
        let may_bind = *grant_registered.borrow_and_update() && *engine_role.borrow_and_update();
        let connection = rpc.live_connection_id();
        if may_bind && !latch_void && (connection.is_none() || connection != bound_on) {
            let quiet = connection.is_some() && connection == refused_on;
            let writer_key = principal_bundle::load_writer_key(&*credentials, &actor_id_hex);
            match send_bind(&rpc, &actor_id, writer_key, &pinned_identity, quiet).await {
                Sent::Bound => {
                    bound_on = connection;
                    refused_on = None;
                }
                Sent::NotRegistered => {
                    principal_bundle::void_grant_registration(&*credentials, &actor_id_hex);
                    latch_void = true;
                    refused_on = connection;
                }
                Sent::Failed => refused_on = connection,
            }
        }
        tokio::select! {
            biased;
            _ = &mut stop => return,
            changed = grant_registered.changed() => {
                if changed.is_err() {
                    return;
                }
            }
            changed = engine_role.changed() => {
                if changed.is_err() {
                    return;
                }
            }
            changed = async { reconnects.as_mut()?.changed().await.ok() }, if reconnects.is_some() => {
                if changed.is_none() {
                    reconnects = None;
                }
            }
            () = fauna_sleep::sleep(LATCH_POLL), if latch_void => {
                latch_void =
                    !principal_bundle::grant_registration_recorded(&*credentials, &actor_id_hex);
            }
        }
    }
}

/// Sign and send one bind, reading the identity it binds. `quiet` is a
/// connection whose refusal is already in the log.
async fn send_bind<R>(
    rpc: &R,
    actor_id: &ActorId,
    writer_key: Option<SigningKey>,
    pinned_identity: &PinnedIdentity,
    quiet: bool,
) -> Sent
where
    R: RpcRequester,
    R::Error: RpcErrorClass,
{
    let Some(writer_key) = writer_key else {
        tracing::warn!("device bind: the slot holds no writer key");
        return Sent::Failed;
    };
    let nest_id =
        match fauna_client_core::nest_trust::read_bound_identity(rpc, pinned_identity()).await {
            Ok(nest_id) => nest_id,
            Err(e) => {
                if !quiet {
                    tracing::info!("device bind: the nest's identity did not read: {e}");
                }
                return Sent::Failed;
            }
        };
    let request = match bind_request(actor_id, &writer_key, &nest_id) {
        Ok(request) => request,
        Err(e) => {
            tracing::warn!("device bind: no randomness for the nonce: {e}");
            return Sent::Failed;
        }
    };
    match rpc
        .request::<_, DeviceBindReply>(DEVICE_BIND_KIND, request)
        .await
    {
        Ok(_) => Sent::Bound,
        Err(e) => {
            if !quiet {
                tracing::info!("device bind: not bound on this connection: {e}");
            }
            if e.as_rpc_error()
                .is_some_and(fauna_protocol::RpcError::is_not_registered)
            {
                Sent::NotRegistered
            } else {
                Sent::Failed
            }
        }
    }
}

/// One signed request, fresh in timestamp and nonce: the bind every reconnect
/// sends is never its predecessor's replay.
fn bind_request(
    actor_id: &ActorId,
    writer_key: &SigningKey,
    nest_id: &[u8; 32],
) -> Result<DeviceBindRequest, getrandom::Error> {
    let device_key = writer_key.verifying_key().to_bytes();
    let timestamp = fauna_core::data::Timestamp::now_millis();
    let mut nonce = [0u8; 32];
    getrandom::fill(&mut nonce)?;
    let message = device_bind_signed_message(&actor_id.0, &device_key, timestamp, nest_id, &nonce);
    Ok(DeviceBindRequest {
        actor_id: fauna_core::hex32::encode(&actor_id.0),
        device_key: fauna_core::hex32::encode(&device_key),
        timestamp,
        nonce: fauna_protocol::ByteBuf::from(nonce.to_vec()),
        signature: hex::encode(writer_key.sign(&message).to_bytes()),
        extra: Default::default(),
    })
}

#[cfg(test)]
mod tests {
    use std::cell::{Cell, RefCell};
    use std::collections::HashMap;
    use std::rc::Rc;
    use std::sync::Mutex;

    use fauna_protocol::{RpcError, decode_strict, encode_canonical};

    use super::*;

    const ACTOR: ActorId = ActorId([0x11; 32]);
    const NEST: [u8; 32] = [0x4E; 32];

    #[derive(Default)]
    struct Secrets(Mutex<HashMap<String, String>>);

    impl SecretStore for Secrets {
        fn get(&self, key: &str) -> Option<String> {
            self.0.lock().unwrap().get(key).cloned()
        }
        fn set(&self, key: &str, value: &str) {
            self.0
                .lock()
                .unwrap()
                .insert(key.to_owned(), value.to_owned());
        }
        fn delete(&self, key: &str) {
            self.0.lock().unwrap().remove(key);
        }
    }

    impl Secrets {
        fn record_latch(&self) {
            self.set(&format!("{}/grant-registered", actor_hex()), "row");
        }
    }

    fn actor_hex() -> String {
        hex::encode(ACTOR.0)
    }

    #[derive(Debug)]
    struct Refused(RpcError);

    impl std::fmt::Display for Refused {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.write_str(&self.0.code)
        }
    }

    impl RpcErrorClass for Refused {
        fn is_rejection(&self) -> bool {
            true
        }
        fn as_rpc_error(&self) -> Option<&RpcError> {
            Some(&self.0)
        }
    }

    /// A socket: which connection is live, each bind seen with the connection
    /// it rode, and the refusal the next bind is answered with, if any.
    #[derive(Clone, Default)]
    struct Socket {
        connection: Rc<Cell<u64>>,
        binds: Rc<RefCell<Vec<(u64, DeviceBindRequest)>>>,
        refuse: Rc<RefCell<Option<RpcError>>>,
    }

    impl Socket {
        fn connections_bound(&self) -> Vec<u64> {
            self.binds.borrow().iter().map(|(c, _)| *c).collect()
        }
    }

    impl RpcRequester for Socket {
        type Error = Refused;

        async fn request<Req, Reply>(
            &self,
            kind: &'static str,
            payload: Req,
        ) -> Result<Reply, Refused>
        where
            Req: serde::Serialize,
            Reply: serde::de::DeserializeOwned,
        {
            assert_eq!(kind, DEVICE_BIND_KIND, "a pinned identity reads no RPC");
            let request = decode_strict(&encode_canonical(&payload).expect("encode"))
                .expect("a bind request");
            self.binds
                .borrow_mut()
                .push((self.connection.get(), request));
            if let Some(refusal) = self.refuse.borrow_mut().take() {
                return Err(Refused(refusal));
            }
            let reply = encode_canonical(&DeviceBindReply::default()).expect("encode reply");
            Ok(decode_strict(&reply).expect("reply decodes"))
        }

        fn live_connection_id(&self) -> Option<u64> {
            Some(self.connection.get())
        }
    }

    /// One bind's world, the loop not yet running.
    struct Rig {
        socket: Socket,
        secrets: Arc<Secrets>,
        grant: watch::Sender<bool>,
        role: watch::Sender<bool>,
        reconnects: watch::Sender<u64>,
        stop: oneshot::Sender<()>,
    }

    fn rig(
        granted: bool,
        holder: bool,
    ) -> (Rig, DeviceBindSeats<Socket, Secrets>, oneshot::Receiver<()>) {
        let socket = Socket::default();
        socket.connection.set(1);
        let secrets = Arc::new(Secrets::default());
        principal_bundle::mint_or_load_writer_key(&*secrets, &actor_hex()).expect("mint");
        secrets.record_latch();
        let (grant, grant_registered) = watch::channel(granted);
        let (role, engine_role) = watch::channel(holder);
        let (reconnects, reconnects_rx) = watch::channel(0);
        let (stop, stop_rx) = oneshot::channel();
        let seats = DeviceBindSeats {
            rpc: socket.clone(),
            credentials: Arc::clone(&secrets),
            actor_id: ACTOR,
            pinned_identity: Box::new(|| Some(NEST)),
            grant_registered,
            engine_role,
            reconnects: Some(reconnects_rx),
        };
        (
            Rig {
                socket,
                secrets,
                grant,
                role,
                reconnects,
                stop,
            },
            seats,
            stop_rx,
        )
    }

    /// Let the loop run to its next wait.
    async fn settle() {
        for _ in 0..32 {
            tokio::task::yield_now().await;
        }
    }

    impl Rig {
        fn reconnect(&self) {
            self.socket.connection.set(self.socket.connection.get() + 1);
            self.reconnects.send_modify(|n| *n += 1);
        }
    }

    #[tokio::test(start_paused = true)]
    async fn one_bind_per_connection_once_the_gate_opens_and_one_more_per_reconnect() {
        let (rig, seats, stop) = rig(false, true);
        let script = async {
            settle().await;
            assert!(
                rig.socket.connections_bound().is_empty(),
                "a closed gate binds nothing"
            );

            rig.grant.send_replace(true);
            settle().await;
            assert_eq!(rig.socket.connections_bound(), [1], "the gate's opening");

            // The counter moving with no new connection is not a new socket.
            rig.reconnects.send_modify(|n| *n += 1);
            settle().await;
            assert_eq!(rig.socket.connections_bound(), [1]);

            rig.reconnect();
            settle().await;
            assert_eq!(rig.socket.connections_bound(), [1, 2], "a reconnect");

            rig.stop.send(()).unwrap();
        };
        tokio::join!(run(seats, stop), script);
    }

    #[tokio::test(start_paused = true)]
    async fn a_launch_with_the_gate_already_open_binds_at_once() {
        let (rig, seats, stop) = rig(true, true);
        let script = async {
            settle().await;
            assert_eq!(rig.socket.connections_bound(), [1]);
            rig.stop.send(()).unwrap();
        };
        tokio::join!(run(seats, stop), script);
    }

    #[tokio::test(start_paused = true)]
    async fn the_bind_is_signed_for_this_nest_by_the_slots_writer_as_read_at_send_time() {
        let (rig, seats, stop) = rig(true, true);
        let script = async {
            settle().await;
            let rotated = SigningKey::from_bytes(&[0x77; 32]);
            rig.secrets
                .set(&actor_hex(), &hex::encode(rotated.to_bytes()));
            rig.reconnect();
            settle().await;

            let binds = rig.socket.binds.borrow();
            assert_eq!(binds.len(), 2);
            assert_ne!(
                binds[0].1.nonce, binds[1].1.nonce,
                "fresh for the replay guard"
            );
            let second = &binds[1].1;
            let device_key = rotated.verifying_key().to_bytes();
            assert_eq!(second.device_key, hex::encode(device_key));
            assert_eq!(second.actor_id, actor_hex());
            let message = device_bind_signed_message(
                &ACTOR.0,
                &device_key,
                second.timestamp,
                &NEST,
                &second.nonce,
            );
            let signature = hex::decode(&second.signature).unwrap();
            assert!(
                fauna_core::identity::verify_detached(&device_key, &message, &signature),
                "the rotated writer signs the next bind"
            );
            drop(binds);
            rig.stop.send(()).unwrap();
        };
        tokio::join!(run(seats, stop), script);
    }

    #[tokio::test(start_paused = true)]
    async fn only_the_engine_roles_holder_binds() {
        let (rig, seats, stop) = rig(true, false);
        let script = async {
            settle().await;
            assert!(rig.socket.connections_bound().is_empty(), "a non-holder");

            rig.role.send_replace(true);
            settle().await;
            assert_eq!(rig.socket.connections_bound(), [1], "the role's win");

            rig.stop.send(()).unwrap();
        };
        tokio::join!(run(seats, stop), script);
    }

    #[tokio::test(start_paused = true)]
    async fn not_registered_voids_the_latch_and_waits_for_the_pass_that_heals_it() {
        let (rig, seats, stop) = rig(true, true);
        *rig.socket.refuse.borrow_mut() = Some(RpcError::not_registered());
        let script = async {
            settle().await;
            assert_eq!(rig.socket.connections_bound(), [1]);
            assert!(
                !principal_bundle::grant_registration_recorded(&*rig.secrets, &actor_hex()),
                "the refusal voids the latch"
            );

            // No retry loop, and a reconnect is no remedy while the latch is
            // void: the same key would meet the same refusal. (Time moves by
            // `fauna_sleep` on the paused clock: the plane names no tokio
            // timer, its tests included — `tests/no_native_time.rs`.)
            fauna_sleep::sleep(LATCH_POLL * 4).await;
            rig.reconnect();
            settle().await;
            assert_eq!(rig.socket.connections_bound(), [1]);

            // The pass re-registers: the latch is back, the bind follows.
            rig.secrets.record_latch();
            fauna_sleep::sleep(LATCH_POLL).await;
            settle().await;
            assert_eq!(rig.socket.connections_bound(), [1, 2]);

            rig.stop.send(()).unwrap();
        };
        tokio::join!(run(seats, stop), script);
    }

    #[tokio::test(start_paused = true)]
    async fn any_other_refusal_keeps_the_latch_and_waits_for_the_next_connection() {
        let (rig, seats, stop) = rig(true, true);
        *rig.socket.refuse.borrow_mut() = Some(RpcError::new(
            "fauna.protocol.unknown_kind",
            "error.protocol.unknown_kind",
        ));
        let script = async {
            settle().await;
            assert_eq!(rig.socket.connections_bound(), [1]);
            fauna_sleep::sleep(Duration::from_secs(600)).await;
            settle().await;
            assert_eq!(rig.socket.connections_bound(), [1], "no retry loop");
            assert!(principal_bundle::grant_registration_recorded(
                &*rig.secrets,
                &actor_hex()
            ));

            rig.reconnect();
            settle().await;
            assert_eq!(rig.socket.connections_bound(), [1, 2]);

            rig.stop.send(()).unwrap();
        };
        tokio::join!(run(seats, stop), script);
    }

    #[tokio::test(start_paused = true)]
    async fn the_loop_ends_with_the_runtime_that_owns_its_watches() {
        let (rig, seats, stop) = rig(false, true);
        let Rig {
            grant,
            role,
            stop: _stop,
            reconnects: _reconnects,
            ..
        } = rig;
        drop(grant);
        drop(role);
        run(seats, stop).await;
    }
}
