//! The followed-folders seam's crossing of web's account port — both halves,
//! beside the [`FollowsStore`] trait they cross
//! (`docs/goal/architecture/account-client-lifecycle.md` § The client-side
//! lifecycle → *The account port*, decisions (c) and (h): the followed-folders
//! seam's doors are `follows`, `put_follow` and `unfollow`).
//!
//! The follow faces and the followed-folders source live in the folders and
//! media chunks; the account runtime whose `fauna.state.follows` rows they
//! read lives in the core chunk. So those chunks read and write through
//! [`PortFollowsStore`] — the **forwarder**, which encodes a method's
//! arguments, calls its door over the port and decodes the answer, and does
//! nothing else — and the core chunk's `accountPortCall` hands each door to
//! [`serve`] over the handle's own implementation of the seam. Every chunk
//! compiles this module, so the two ends of every crossing are one
//! definition.
//!
//! **A port fault is never a success (decision (f)).** No runtime, another
//! account's runtime, an unknown door, undecodable bytes and broken glue all
//! answer the method's own refusal, [`StoreError`] — so an unreadable list is
//! the same transient on web as a store not yet up is natively, never "no
//! follows", and a follow that did not cross is never reported stored.

use fauna_account_port::{PortFault, PortTransport, answer, forward};
use fauna_core::data::{FollowedFolder, FollowsConfig};

use crate::store_seam::{FollowsStore, StoreError};

/// The seam's doors, one per [`FollowsStore`] method, prefixed with the seam
/// (the core chunk's dispatch holds every seam's doors in one namespace).
pub mod doors {
    pub const FOLLOWS: &str = "follows.follows";
    pub const PUT_FOLLOW: &str = "follows.put_follow";
    pub const UNFOLLOW: &str = "follows.unfollow";

    /// Every door of the seam.
    pub const ALL: [&str; 3] = [FOLLOWS, PUT_FOLLOW, UNFOLLOW];
}

/// The forwarder: [`FollowsStore`] over an account-port transport.
pub struct PortFollowsStore<T> {
    transport: T,
}

impl<T: PortTransport> PortFollowsStore<T> {
    pub fn new(transport: T) -> Self {
        Self { transport }
    }
}

#[cfg_attr(not(target_arch = "wasm32"), async_trait::async_trait)]
#[cfg_attr(target_arch = "wasm32", async_trait::async_trait(?Send))]
impl<T: PortTransport> FollowsStore for PortFollowsStore<T> {
    async fn follows(&self) -> Result<FollowsConfig, StoreError> {
        let answered: Result<FollowsConfig, String> = forward(&self.transport, doors::FOLLOWS, &())
            .await
            .map_err(|f| StoreError::Load(f.to_string()))?;
        answered.map_err(StoreError::Load)
    }

    async fn put_follow(&self, follow: FollowedFolder) -> Result<bool, StoreError> {
        let answered: Result<bool, String> = forward(&self.transport, doors::PUT_FOLLOW, &follow)
            .await
            .map_err(|f| StoreError::Save(f.to_string()))?;
        answered.map_err(StoreError::Save)
    }

    async fn unfollow(&self, home_nest_url: String, folder_id: i64) -> Result<bool, StoreError> {
        let answered: Result<bool, String> = forward(
            &self.transport,
            doors::UNFOLLOW,
            &(home_nest_url, folder_id),
        )
        .await
        .map_err(|f| StoreError::Save(f.to_string()))?;
        answered.map_err(StoreError::Save)
    }
}

/// The forwarder over the SPA's `SharedAccountPort` object, as every consumer
/// chunk's follow faces and followed-folders source hold it — one constructor,
/// so the folders and media chunks wrap the port identically. A value that is
/// not a port is refused by name, where it was wired.
#[cfg(target_arch = "wasm32")]
pub fn from_js_port(
    port: fauna_account_port::JsAccountPort,
) -> Result<std::sync::Arc<dyn FollowsStore>, PortFault> {
    let transport = fauna_account_port::JsAccountTransport::new(port.into())?;
    Ok(std::sync::Arc::new(PortFollowsStore::new(transport)))
}

/// Answer one door of the followed-folders seam from `seam` — the core
/// chunk's half. `None` when `door` is not this seam's, so the dispatch can
/// ask the next seam and refuse a door nobody knows by name.
pub async fn serve(
    seam: &dyn FollowsStore,
    door: &str,
    payload: &[u8],
) -> Option<Result<Vec<u8>, PortFault>> {
    Some(match door {
        doors::FOLLOWS => {
            answer(payload, |(): ()| async move {
                let reply: Result<FollowsConfig, String> =
                    seam.follows().await.map_err(|e| e.to_string());
                reply
            })
            .await
        }
        doors::PUT_FOLLOW => {
            answer(payload, |follow: FollowedFolder| async move {
                let reply: Result<bool, String> =
                    seam.put_follow(follow).await.map_err(|e| e.to_string());
                reply
            })
            .await
        }
        doors::UNFOLLOW => {
            answer(
                payload,
                |(home_nest_url, folder_id): (String, i64)| async move {
                    let reply: Result<bool, String> = seam
                        .unfollow(home_nest_url, folder_id)
                        .await
                        .map_err(|e| e.to_string());
                    reply
                },
            )
            .await
        }
        _ => return None,
    })
}

#[cfg(all(test, not(target_arch = "wasm32")))]
mod tests {
    use std::sync::Arc;

    use fauna_account_port::loopback::Loopback;

    use super::*;
    use crate::test_helpers::FakeFollowsStore;

    fn over(seam: Arc<FakeFollowsStore>) -> PortFollowsStore<Loopback> {
        PortFollowsStore::new(Loopback::new(move |door, payload| {
            let seam = Arc::clone(&seam);
            async move {
                serve(&*seam, door, &payload)
                    .await
                    .unwrap_or_else(|| Err(PortFault::UnknownDoor(door.to_string())))
            }
        }))
    }

    fn followed(id: i64) -> FollowedFolder {
        FollowedFolder {
            home_nest_url: "https://peer.example".into(),
            home_nest_actor_id: Some("ab".repeat(32)),
            owner_actor_id: "cd".repeat(32),
            owner_handle: Some("alice@peer.example".into()),
            folder_id: id,
            display_name: "site".into(),
        }
    }

    /// Every method crosses and comes back with the seam's own answer — the
    /// record intact, the written flag faithful — and the refusal arm comes
    /// back as the method's own `StoreError`.
    #[tokio::test]
    async fn every_method_round_trips_both_arms() {
        let seam = Arc::new(FakeFollowsStore::empty());
        let port = over(Arc::clone(&seam));
        assert_eq!(port.follows().await.unwrap(), FollowsConfig::default());

        assert!(port.put_follow(followed(7)).await.unwrap(), "a new follow");
        assert!(
            !port.put_follow(followed(7)).await.unwrap(),
            "an equal re-put"
        );
        assert_eq!(
            seam.current().followed,
            vec![followed(7)],
            "the write reached the seam"
        );
        assert_eq!(port.follows().await.unwrap().followed, vec![followed(7)]);

        assert!(
            port.unfollow("https://peer.example".into(), 7)
                .await
                .unwrap()
        );
        assert!(
            !port
                .unfollow("https://peer.example".into(), 7)
                .await
                .unwrap(),
            "unfollowing what is gone writes nothing"
        );
        assert!(seam.current().followed.is_empty());

        seam.refuse_next_writes(2);
        assert!(matches!(
            port.put_follow(followed(8)).await,
            Err(StoreError::Save(why)) if why.contains("no generation tip")
        ));
        assert!(matches!(
            port.unfollow("https://peer.example".into(), 8).await,
            Err(StoreError::Save(why)) if why.contains("no generation tip")
        ));
    }

    /// A faulting transport answers every method with its refusal — never a
    /// success, so an unreachable runtime is a transient, never "no follows".
    #[tokio::test]
    async fn a_faulting_transport_is_refused_on_every_method() {
        for fault in [
            PortFault::NoRuntime("none".into()),
            PortFault::UnknownDoor("x".into()),
            PortFault::Codec("bad".into()),
            PortFault::Glue("boom".into()),
        ] {
            let port = PortFollowsStore::new(Loopback::faulting(fault.clone()));
            let said = fault.to_string();
            assert!(matches!(port.follows().await, Err(StoreError::Load(w)) if w == said));
            assert!(matches!(
                port.put_follow(followed(7)).await,
                Err(StoreError::Save(w)) if w == said
            ));
            assert!(matches!(
                port.unfollow(String::new(), 7).await,
                Err(StoreError::Save(w)) if w == said
            ));
        }
    }

    /// A door that is not this seam's is not answered, so the dispatch can
    /// refuse it by name.
    #[tokio::test]
    async fn a_foreign_door_is_not_this_seams() {
        let seam = FakeFollowsStore::empty();
        assert!(
            serve(&seam, "custody_ceremony.custody", &[])
                .await
                .is_none()
        );
        for door in doors::ALL {
            assert!(serve(&seam, door, &[0xff]).await.is_some(), "{door}");
        }
    }
}
