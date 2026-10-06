//! **The road, in the runtime's pass** (`identity-succession.md` § Enforcement
//! on the home nest → *Every nest the identity is linked to*, the paragraph
//! **The road**): every full pass of a seed-holding runtime delivers the
//! succession statements its account is owed at — the owed nests the bound
//! nest serves the successor, and those each linked nest the secondary leg
//! reaches serves it (a nest that applied a statement burned pairings of its
//! own and keeps its own owed nests), so the set closes over any number of
//! nests.
//!
//! The delivery itself is shared Rust below both of its callers
//! (`fauna_client_core::succession_delivery`); what this module adds is what
//! the pass needs around it: the report one keeper's delivery leaves
//! ([`OwedNestsPass`]), its log line, and the retired identities' seeds a
//! chain replay signs in with ([`PredecessorSeeds`]). The host's reach — two
//! connection types per platform — is the host's
//! (`crate::account_driver::OwedNestDeliverer`).

use std::sync::Arc;

use fauna_client_core::succession_delivery::{Delivery, OwedReason};
use fauna_core::identity::ActorKeypair;
use fauna_protocol::recovery::OwedNest;

/// The seeds of this account's succeeded-from identities that this device
/// holds — what a chain replay at an owed nest signs in as
/// (`fauna_client_core::succession_delivery`, step 4). Taken from the
/// principal's own seed holder ([`Self::of`]) — the one registry walk that
/// hands out predecessor seeds — never from
/// [`crate::attested_predecessors::AttestedPredecessors`], which by design
/// holds no seed. The deliverer's own copy, since the principal moves into
/// the runtime. Cheap to clone; never printed.
#[derive(Clone, Default)]
pub struct PredecessorSeeds(Arc<Vec<ActorKeypair>>);

impl PredecessorSeeds {
    /// No predecessor seeds — an identity that never succeeded, or a device
    /// that holds none of their seeds: the replay arm answers "no seed".
    #[must_use]
    pub fn none() -> Self {
        Self::default()
    }

    /// From raw seeds, as the registry hands them out.
    #[must_use]
    pub fn from_seeds(seeds: impl IntoIterator<Item = [u8; 32]>) -> Self {
        Self(Arc::new(
            seeds.into_iter().map(ActorKeypair::from_secret).collect(),
        ))
    }

    /// The predecessors' seeds `holder` holds beside its own identity
    /// (`SeedHolder::from_registry`'s walk).
    #[cfg(feature = "account-driver")]
    #[must_use]
    pub fn of(holder: &crate::account_driver::SeedHolder) -> Self {
        Self::from_seeds(holder.predecessors().iter().map(|kp| *kp.secret_bytes()))
    }

    /// The keypair for `actor_id`, if this device holds its seed.
    #[must_use]
    pub fn keypair_for(&self, actor_id: &[u8; 32]) -> Option<ActorKeypair> {
        self.0
            .iter()
            .find(|kp| kp.actor_id().0 == *actor_id)
            .map(|kp| ActorKeypair::from_secret(*kp.secret_bytes()))
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

impl std::fmt::Debug for PredecessorSeeds {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "PredecessorSeeds({} held)", self.0.len())
    }
}

/// What one keeper's owed list came to in a pass.
#[derive(Debug, Clone, PartialEq)]
pub struct OwedNestsPass {
    /// The nest that served the list: `None` is the bound nest, `Some` the
    /// linked nest of that identity.
    pub keeper: Option<[u8; 32]>,
    /// Each served entry and what its delivery did — or why the list could
    /// not be read (retried next pass).
    pub outcome: Result<Vec<(OwedNest, Delivery)>, String>,
}

impl OwedNestsPass {
    /// The deliveries, or none when the list was unread.
    #[must_use]
    pub fn deliveries(&self) -> &[(OwedNest, Delivery)] {
        self.outcome.as_deref().unwrap_or_default()
    }
}

/// Log one keeper's deliveries on the event: a landed or settled entry at
/// info, an entry that stays owed for a reason a pass will not change at warn,
/// and the waits (no address, unreachable) at debug.
pub fn log_owed_nests(pass: &OwedNestsPass) {
    let keeper = pass.keeper.map_or_else(
        || "the bound nest".to_string(),
        |id| fauna_core::hex32::encode(&id),
    );
    let deliveries = match &pass.outcome {
        Ok(d) => d,
        Err(e) => {
            tracing::debug!(%keeper, "succession delivery: the owed nests could not be read (retried): {e}");
            return;
        }
    };
    for (owed, delivery) in deliveries {
        let owed_at = hex::encode(&owed.nest_id);
        match delivery {
            Delivery::Landed { landed, replayed } => tracing::info!(
                %keeper, %owed_at, landed, replayed,
                "succession delivery: the statement is at the owed nest — settled"
            ),
            Delivery::NoAccount => tracing::info!(
                %keeper, %owed_at,
                "succession delivery: the owed nest holds no account for the retired identity — settled"
            ),
            Delivery::Owed(OwedReason::NoAddress | OwedReason::Unreachable(_)) => {
                tracing::debug!(%keeper, %owed_at, ?delivery, "succession delivery: still owed");
            }
            Delivery::Owed(reason) => tracing::warn!(
                %keeper, %owed_at, ?reason,
                "succession delivery: the owed nest did not take the statement — still owed"
            ),
        }
    }
}
