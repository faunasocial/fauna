//! **Which sets an engine on this device holds** — the predicate the
//! projection walk's take-over asks before it claims an adoption marker
//! (`succession-cut.md` ruling (11)(l), *one consumer per marker*: the walk
//! claims a marker only for a set with no engine binding on this device at
//! that moment, and never joins an engine that has recorded `Begun` or `Spent`
//! for the nonce where the app can read that state).
//!
//! The author consults it through
//! [`crate::orchestration::FoldersAuthor::with_engine_bound_sets`]. A face
//! where no engine ever runs (web) wires none, which reads as "no set is
//! bound"; a native face wires the roster of its engine host — on a desktop
//! the sync agent's bound locations ([`agent_bound_sets`]).

use std::collections::BTreeSet;

/// What this device's engines hold at one reading.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct EngineHoldings {
    /// The `FolderRef` wire form (`fauna_core::folder_keys::FolderRef::to_wire`)
    /// of every set an engine on this device is bound to — a parked
    /// (access-revoked) binding included, since it is still a binding.
    pub bound: BTreeSet<String>,
    /// The adoption-marker nonces an engine on this device has recorded
    /// `Begun` or `Spent` for, where the face can read that state; empty where
    /// it cannot.
    pub engaged: BTreeSet<[u8; 32]>,
}

impl EngineHoldings {
    /// Whether the walk may claim `marker` for the set whose ref is
    /// `folder_ref`: no engine bound to the set, and none engaged on the
    /// marker. A set with no ref is never claimable — it cannot be told apart
    /// from a bound one.
    #[must_use]
    pub fn walk_may_claim(&self, folder_ref: Option<&str>, marker: &[u8; 32]) -> bool {
        folder_ref.is_some_and(|r| !self.bound.contains(r)) && !self.engaged.contains(marker)
    }

    /// Fold an engine host's roster into account `actor`'s holdings — the
    /// phones' roster, which the app reads off its own hosts and hands over
    /// through `fauna-ffi`'s installed roster. Each entry is a set's
    /// `FolderRef` wire form or its actor-scoped identity
    /// (`ActorScopedFolderRef`, the File Provider domain identifier on iOS,
    /// the provider's set id on android): a bare ref and `actor`'s scoped one
    /// are bound under the bare ref; another account's scoped identity binds
    /// nothing for this account, whose markers live in its own custody. An
    /// entry that reads as neither makes the whole roster unread (`Err`) — an
    /// unreadable binding may be an engine's. The hosts report no adoption
    /// state, so [`Self::engaged`] stays empty.
    pub fn from_host_roster<I, S>(actor: [u8; 32], entries: I) -> anyhow::Result<Self>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        use fauna_core::folder_keys::{ActorScopedFolderRef, FolderRef};
        let mut bound = BTreeSet::new();
        for entry in entries {
            let entry = entry.as_ref();
            if let Some(folder) = FolderRef::parse(entry) {
                bound.insert(folder.to_wire());
            } else if let Some(scoped) = ActorScopedFolderRef::parse(entry) {
                if scoped.actor_id == actor {
                    bound.insert(scoped.folder.to_wire());
                }
            } else {
                anyhow::bail!("the engine roster names {entry:?}, which is no set's identity");
            }
        }
        Ok(Self {
            bound,
            engaged: BTreeSet::new(),
        })
    }
}

/// The face's reading of [`EngineHoldings`]. An `Err` means the roster could
/// not be read, and the take-over claims no new marker on that pass — an
/// unread binding may be an engine's.
#[cfg_attr(not(target_arch = "wasm32"), async_trait::async_trait)]
#[cfg_attr(target_arch = "wasm32", async_trait::async_trait(?Send))]
pub trait EngineBoundSets: fauna_core::MaybeSendSync {
    /// Read what this device's engines hold now.
    async fn engine_holdings(&self) -> anyhow::Result<EngineHoldings>;
}

/// The desktop roster: this user's sync agent's bound locations
/// (`ListLocations`), read over its local endpoint. The agent hosts every
/// resident engine on a desktop (`sync-agent.md` § Control plane split), so
/// its location rows are the device's whole binding roster; no agent
/// answering is an unread roster. The agent reports no adoption state, so
/// [`EngineHoldings::engaged`] stays empty.
#[cfg(all(feature = "mls", not(target_arch = "wasm32"), any(unix, windows)))]
#[must_use]
pub fn agent_bound_sets() -> std::sync::Arc<dyn EngineBoundSets> {
    std::sync::Arc::new(AgentBoundSets)
}

#[cfg(all(feature = "mls", not(target_arch = "wasm32"), any(unix, windows)))]
struct AgentBoundSets;

#[cfg(all(feature = "mls", not(target_arch = "wasm32"), any(unix, windows)))]
#[async_trait::async_trait]
impl EngineBoundSets for AgentBoundSets {
    async fn engine_holdings(&self) -> anyhow::Result<EngineHoldings> {
        let bound = tokio::task::spawn_blocking(fauna_client_sync::agent::bound_folder_refs)
            .await
            .map_err(|e| anyhow::anyhow!("the agent roster read did not finish: {e}"))?
            .map_err(|e| anyhow::anyhow!("the agent roster is unreadable: {e}"))?;
        Ok(EngineHoldings {
            bound,
            engaged: BTreeSet::new(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_walk_claims_only_an_unbound_unengaged_set_with_a_ref() {
        let h = EngineHoldings {
            bound: ["local:1".to_string()].into_iter().collect(),
            engaged: [[7; 32]].into_iter().collect(),
        };
        assert!(h.walk_may_claim(Some("local:2"), &[1; 32]));
        assert!(!h.walk_may_claim(Some("local:1"), &[1; 32]), "bound");
        assert!(!h.walk_may_claim(Some("local:2"), &[7; 32]), "engaged");
        assert!(!h.walk_may_claim(None, &[1; 32]), "no ref");
    }

    /// A phone's engine-host roster: a bare ref and this account's scoped
    /// identity are bound under their bare ref, another account's scoped
    /// identity binds nothing here, and one entry that reads as neither makes
    /// the whole roster unread.
    #[test]
    fn a_host_roster_folds_bare_and_own_scoped_refs_and_refuses_an_unreadable_one() {
        use fauna_core::folder_keys::FolderRef;
        let me = [3; 32];
        let other = [4; 32];
        let a = FolderRef::parse("local:1").unwrap();
        let b = FolderRef::parse("local:2").unwrap();
        let c = FolderRef::parse("local:5").unwrap();
        let roster = [
            a.to_wire(),
            b.scoped_to(me).to_wire(),
            c.scoped_to(other).to_wire(),
        ];
        let h = EngineHoldings::from_host_roster(me, roster.iter()).unwrap();
        assert_eq!(
            h.bound,
            [a.to_wire(), b.to_wire()]
                .into_iter()
                .collect::<BTreeSet<_>>()
        );
        assert!(h.engaged.is_empty());
        assert!(EngineHoldings::from_host_roster(me, ["Photos"].iter()).is_err());
        assert_eq!(
            EngineHoldings::from_host_roster(me, Vec::<String>::new()).unwrap(),
            EngineHoldings::default()
        );
    }
}
