//! The credential-slot seam — what the account driver, the fleet-removal
//! completion leg and the peer leg read and write on the machine's T10
//! principal bundle, stated as a trait so the driver compiles for every host
//! (`account-data-plane.md` § The client-side lifecycle → *The trigger
//! fired*, ruling (2): the slot seam).
//!
//! The native slot (`fauna_sync_engine::principal_bundle::PrincipalSlot`,
//! over `fauna-credential-store` and the platform keyrings) implements it;
//! web's own slot implements it over the SPA's secret store. The trait names
//! **what** the slot carries — the enrollment grant, its registration latch,
//! a standing refusal, the staged device removals, the retained generation
//! keys — never **where**: the carriage's attribute names, its serialization
//! and its write section are the implementer's
//! (`account-replica-posture.md` § The store device principal owns the
//! slot's mechanics).
//!
//! Every method is synchronous, as the native slot is: a slot read is a
//! local secret-store read, and the driver serves it as a local command at a
//! pass's yield point (`Cmd::is_local`, the slot-read verdict).

use fauna_core::data::DeviceAuthorization;
use fauna_core::encoding::{EmbedAsBytes, canonical_encode};
use fauna_core::fleet_removal::StagedFleetRemoval;

use crate::generation_tip::RetainedKeyCustody;

/// A nest refusal of this machine's enrollment that **stands** until something
/// outside the pump changes — the one enrollment answer a user has to act on,
/// so the one the Devices page renders (`ui/devices.md` § Errors & edge
/// cases). Recorded in the credential slot by the process that met it and
/// read by every process sharing the slot.
///
/// Not every failed pass belongs here: an offline nest is retried next tick
/// and says nothing to the user; a revoked principal
/// (`EnrollmentPass::RemovedFromAccount`) heals itself at the next
/// ceremony-capable sign-in. This type is for a verdict with a **remedy the
/// user performs**.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EnrollmentRefusal {
    /// `fauna.sync.device_limit_exceeded` — the account is at its tier's
    /// device cap (`devices.md` § Step 4), so the machine's row was never
    /// created. Clears only when a slot frees: the user removes a device
    /// (Settings → Devices, the page rendering this), or the admin moves the
    /// account to a bigger tier.
    DeviceLimitExceeded,
}

impl EnrollmentRefusal {
    /// The slot value — the wire code's last segment, spelled once here. The
    /// spelling every implementer persists, so a refusal recorded by one
    /// process reads back in another.
    pub fn slot_value(self) -> &'static str {
        match self {
            Self::DeviceLimitExceeded => "device_limit_exceeded",
        }
    }

    /// The inverse of [`Self::slot_value`]; `None` for a value this build
    /// does not know.
    pub fn from_slot_value(value: &str) -> Option<Self> {
        match value {
            "device_limit_exceeded" => Some(Self::DeviceLimitExceeded),
            _ => None,
        }
    }

    /// The sentence the Devices page paints on `error-message` — the SAME
    /// string `fauna_protocol::RpcError::localized` renders for the wire code,
    /// so a refusal met on the wire and one read back from the slot cannot
    /// drift apart.
    pub fn notice(self) -> &'static str {
        match self {
            Self::DeviceLimitExceeded => fauna_i18n::strings::error::sync::DEVICE_LIMIT_EXCEEDED,
        }
    }
}

/// The verified enrollment grant as the slot carries it, together with the
/// wire it rode in on, kept so a consumer (the handshake mint, a
/// re-registration, the peer leg's witness) never has to re-sign anything.
#[derive(Debug, Clone)]
pub struct LoadedDeviceAuthorization {
    pub authorization: DeviceAuthorization,
    pub wire: EmbedAsBytes,
}

/// A read-only snapshot of what the slot carries — the runtime's observable
/// for tests and status surfaces (`AccountStoreHandle::principal_bundle_status`).
#[derive(Debug, Clone)]
pub struct PrincipalBundleStatus {
    /// The verified enrollment grant, when one is in the slot.
    pub device_authorization: Option<DeviceAuthorization>,
    /// How many retained generation keys ride the bundle.
    pub retained_generations: usize,
    /// Whether the account's backup key is persisted in the slot.
    pub backup_key_persisted: bool,
}

/// The slot seam. Supertrait [`RetainedKeyCustody`] is the half the
/// generation passes already consume (the retained generation keys); the
/// methods here are the driver's, the fleet-removal completion leg's and the
/// peer leg's.
pub trait PrincipalCustody: RetainedKeyCustody {
    /// What the slot carries right now.
    fn status(&self) -> PrincipalBundleStatus;

    /// The verified enrollment grant, wire included — `None` until a ceremony
    /// has run on this machine.
    fn device_authorization(&self) -> Option<LoadedDeviceAuthorization>;

    /// The loaded grant's **canonical `EmbedAsBytes` carriage** — the exact
    /// bytes a group-plane row, a roster entry or a generation mint embeds as
    /// its `authorization`.
    ///
    /// One derivation, two consumers: the group-ceremony authority seam
    /// (`Cmd::GroupCeremonyAuthority`) and the authority-device severance
    /// pass. `None` when this machine has run no enrollment ceremony, and
    /// also when the wire will not re-encode — the same quiet, self-healing
    /// absence in both cases, because the value heals through the ceremony
    /// and refusing the assembly over it would strand a machine that can
    /// otherwise work.
    fn device_authorization_carriage(&self) -> Option<Vec<u8>> {
        let loaded = self.device_authorization()?;
        match canonical_encode(&loaded.wire) {
            Ok(bytes) => Some(bytes.to_vec()),
            Err(e) => {
                tracing::warn!(
                    "principal bundle: device-authorization carriage will not re-encode ({e}) \
                     — treating this machine as carrying no grant"
                );
                None
            }
        }
    }

    /// **Which `sync_devices` row** the ceremony's nest legs last succeeded on,
    /// for the grant the slot currently carries — `None` when the grant is
    /// unregistered, or when a re-ceremony has since minted a different wire
    /// (the latch is content-addressed against the grant's encoding).
    fn grant_registration_row(&self) -> Option<String>;

    /// Latch the grant the slot carries as registered on `device_id_hex`.
    fn record_grant_registered_on(&self, device_id_hex: &str);

    /// Void the registration latch: the nest this machine's device principal
    /// dials answered `not_registered` (a second nest, a rebuilt box), so the
    /// latch describes a replica that is no longer the one bound. The next
    /// pass of a runtime holding the owner session re-registers by the
    /// grant-first probe (`account-replica-posture.md` § The store device
    /// principal).
    fn void_grant_registration(&self);

    /// The nest's standing refusal of this machine's enrollment, if any.
    fn enrollment_refusal(&self) -> Option<EnrollmentRefusal>;

    /// Record that the nest refused this machine's enrollment, so every
    /// process sharing the slot can render it.
    fn record_enrollment_refused(&self, refusal: EnrollmentRefusal);

    /// The staged device removals (`fleet_removal` § The completion rule).
    fn pending_fleet_removals(&self) -> Vec<StagedFleetRemoval>;

    /// Read-modify-write the staged removals under the slot's write section:
    /// `change` edits the list and answers whether it changed anything;
    /// `Ok(true)` means the edit persisted and read back, `Ok(false)` that
    /// nothing changed. An error means the intent did not persist — the
    /// caller must not go on with a deletion it cannot promise to finish.
    fn update_pending_fleet_removals(
        &self,
        change: &mut dyn FnMut(&mut Vec<StagedFleetRemoval>) -> bool,
    ) -> anyhow::Result<bool>;
}
