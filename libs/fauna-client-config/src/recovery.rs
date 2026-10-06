//! Total-box-loss recovery projections over a folded deployment-seed custody
//! map — the shared, cross-app mapping behind the step-4 recovery UI.
//!
//! Authority: `docs/goal/architecture/nest/box-recovery.md` § Recovery UI
//! (step 4) — the `recover-box-item` rows on the `nest_recovery` page and the
//! `recover-selfhosted-command` element on `recover_selfhosted_instructions`.
//!
//! Everything here is a **pure projection of an already-folded custody map**:
//! no transport, no `async`, wasm-clean. Each projection takes the map
//! (`&[DeploymentSeedEntry]`) the plane's pre-login readers answer
//! (`fauna_account_plane::deployment_seed_recovery` — the local and cold reads
//! of `fauna.state.deployment-seeds`). That split is deliberate — *getting* the
//! map is per-source, but the *projection* of it into what the recovery UI
//! shows must be identical on every app (priorities #1/#3). Keeping the
//! projections here is what stops each app re-deriving the box list — and
//! quietly disagreeing about it.

use fauna_core::data::DeploymentSeedEntry;
use fauna_core::hex32;

/// The custodied boxes the `nest_recovery` hub enumerates — one 64-char hex
/// `nest_actor_id` per `recover-box-item` row, in map order.
///
/// **Only the public id crosses out.** The custodied *seed* stays inside
/// Rust; the re-provision drive
/// resolves it back from this id (`OnboardingMachine`'s `RecoveryConfigReader`),
/// and the one place a seed is *deliberately* surfaced is
/// [`selfhosted_recovery_command_in`] — the installer input the admin pastes.
///
/// This is the single shared mapping every app's box-list read feeds through,
/// whichever of the plane's reads folded the map.
///
/// **Superseded entries are excluded** (`DeploymentSeedEntry::superseded_by`;
/// `box-recovery.md` § Custody after rotation): a box that rotated its deployment
/// seed is recoverable only at its *successor* identity, and re-provisioning with
/// the predecessor seed would rebuild an identity every converged client now
/// refuses — so the hub must be structurally unable to list it. The successor's
/// own entry is a live row here and carries the same `domain` label, so the
/// admin's box list keeps exactly one row for the box across a rotation.
pub fn recoverable_box_ids_in(seeds: &[DeploymentSeedEntry]) -> Vec<String> {
    recoverable_boxes_in(seeds)
        .into_iter()
        .map(|row| hex32::encode(&row.nest_actor_id))
        .collect()
}

/// One `recover-box-item` row: the box's id plus the label the UI shows for it.
///
/// The id-only [`recoverable_box_ids_in`] is this projection's older, narrower face;
/// clients that also render the **domain** label (the native
/// `deployment_seeds`/`deployment_seeds_local` getters and web's box-list read)
/// used to iterate the raw custody map themselves *because* the
/// id-only projection could not carry the label — and thereby re-derived the box
/// list the shared projection exists to own. Re-deriving is how the apps quietly
/// disagree: when supersession filtering landed here, every re-deriving reader
/// would have kept listing a rotated-away box. So the row type is shared and the
/// filter has exactly one home.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecoverableBox {
    /// The box's `nest_actor_id`.
    pub nest_actor_id: [u8; 32],
    /// The box's own handle domain, or `None` for a domainless box — the row label
    /// and the cloud re-provision zone.
    pub domain: Option<String>,
}

/// Every recoverable box, in map order — the shared row projection behind
/// both [`recoverable_box_ids_in`] and every app's labelled box list.
///
/// **Superseded entries are excluded** ([`fauna_core::data::DeploymentSeedEntry::superseded_by`];
/// `box-recovery.md` § Custody after rotation): a box that rotated its deployment
/// seed is recoverable only at its *successor* identity, and re-provisioning with
/// the predecessor seed would rebuild an identity every converged client now
/// refuses. The successor's own entry is a live row carrying the same label, so the
/// admin's box list keeps exactly one row for the box across a rotation.
pub fn recoverable_boxes_in(seeds: &[DeploymentSeedEntry]) -> Vec<RecoverableBox> {
    seeds
        .iter()
        .filter(|entry| entry.superseded_by.is_none())
        .map(|entry| RecoverableBox {
            nest_actor_id: entry.nest_actor_id,
            domain: entry.domain.clone(),
        })
        .collect()
}

/// The `recover-selfhosted-command` for one selected box: resolve that box's
/// custodied seed out of the folded custody map and render the installer `.env`
/// line. The seed resolves through the resolution-point read
/// ([`DeploymentSeedEntry::seed_for`]), so a superseded box yields no command.
///
/// `None` when `nest_actor_id_hex` is not 64-char hex, or when the map custodies
/// no seed for that box (it was never custodied) — the caller shows the pending
/// placeholder rather than a command that would boot a box with the *wrong*
/// identity.
///
/// The seed **is** surfaced here by design — see [`selfhosted_recovery_command`].
pub fn selfhosted_recovery_command_in(
    seeds: &[DeploymentSeedEntry],
    nest_actor_id_hex: &str,
) -> Option<String> {
    let id: [u8; 32] = hex32::decode(nest_actor_id_hex).ok()?;
    let seed = DeploymentSeedEntry::seed_for(seeds, &id)?;
    Some(selfhosted_recovery_command(&seed))
}

/// Render the `recover-selfhosted-command` for the box-recovery step-4
/// self-hosted install page — the `.env` line carrying the custodied
/// deployment seed (`box-recovery.md` § Recovery UI (step 4)).
///
/// The admin runs their self-hosted installer on a fresh box with this
/// `FAUNA_DEPLOYMENT_SEED` set (env-only, never prompted — the installer flag
/// of `box-recovery.md` § Implementation status, step-1), so the nest's
/// `deployment_key::reconcile_deployment_keypair` adopts the saved seed and the
/// rebuilt box re-presents the **same** `nest_actor_id`; every TOFU-pinned
/// client reconnects without a trust break.
///
/// **The seed IS surfaced here by design.** Unlike the box-list read (where the
/// raw seed never crosses into the client), the self-hosted
/// installer input legitimately shows it: it is exactly the recovery material
/// the admin pastes into their box's deploy `.env` / prepends to the installer
/// invocation (`box-recovery.md` § Trust & audience — "the self-hosted
/// installer `.env` (root-owned, `0600`)"). Rendered installer- and
/// download-URL-agnostic (the one `FAUNA_DEPLOYMENT_SEED` line both
/// `install-fauna-public.sh` and `install-fauna-home.sh` read) so the command
/// is identical whichever installer / deploy shape the admin uses.
///
/// This is the single, shared render site so the wasm (`WsRpcClient`) and native
/// (`fauna-ffi`) recovery getters emit a byte-identical command across all six
/// apps (priorities #1/#3).
pub fn selfhosted_recovery_command(seed: &[u8; 32]) -> String {
    format!("FAUNA_DEPLOYMENT_SEED={}", hex32::encode(seed))
}

#[cfg(test)]
mod tests {
    use super::selfhosted_recovery_command;

    #[test]
    fn renders_the_env_line_with_the_64_char_hex_seed() {
        // The installers validate a 64-char (32-byte) hex Ed25519 seed
        // (box-recovery.md § Implementation status, step-1 installer flag), so
        // the rendered value must be exactly `FAUNA_DEPLOYMENT_SEED=<64 hex>`.
        let seed = [0xABu8; 32];
        let cmd = selfhosted_recovery_command(&seed);
        assert_eq!(cmd, format!("FAUNA_DEPLOYMENT_SEED={}", "ab".repeat(32)));

        let hex_value = cmd.strip_prefix("FAUNA_DEPLOYMENT_SEED=").unwrap();
        assert_eq!(hex_value.len(), 64, "seed must render as 64 hex chars");
        assert!(
            hex_value.chars().all(|c| c.is_ascii_hexdigit()),
            "seed must render as lowercase hex"
        );
    }

    #[test]
    fn distinct_seeds_render_distinct_commands() {
        let a = selfhosted_recovery_command(&[0x01u8; 32]);
        let b = selfhosted_recovery_command(&[0x02u8; 32]);
        assert_ne!(a, b);
    }

    // ── the projections the recovery UI reads ───────────────────────────────

    use super::{recoverable_box_ids_in, selfhosted_recovery_command_in};
    use fauna_core::data::DeploymentSeedEntry;

    /// One custody row keyed the way production keys it:
    /// `nest_actor_id == ed25519(seed).public`.
    ///
    /// **That pairing is load-bearing in a fixture, not just in production.**
    /// A reader keyed by the id a seed **derives**
    /// (`DeploymentSeedEntry::seed_for`'s self-consistency refusal) never
    /// matches an entry whose id was chosen independently of its seed, so such
    /// a fixture exercises a shape no production writer produces. Always derive
    /// the id.
    fn entry(seed: [u8; 32], domain: Option<&str>) -> DeploymentSeedEntry {
        DeploymentSeedEntry {
            nest_actor_id: fauna_core::data::DeploymentSeedEntry::nest_actor_id_for_seed(seed),
            seed: seed.into(),
            domain: domain.map(Into::into),
            ..Default::default()
        }
    }

    /// A custody map holding two boxes, the second domainless.
    fn map_with_two_boxes() -> (Vec<DeploymentSeedEntry>, [u8; 32], [u8; 32]) {
        let (a, b) = (
            entry([0x11u8; 32], Some("a.example")),
            entry([0x22u8; 32], None),
        );
        let (id_a, id_b) = (a.nest_actor_id, b.nest_actor_id);
        (vec![a, b], id_a, id_b)
    }

    #[test]
    fn box_ids_project_every_custodied_entry_as_64_char_hex() {
        let (map, id_a, id_b) = map_with_two_boxes();
        assert_eq!(
            recoverable_box_ids_in(&map),
            vec![hex::encode(id_a), hex::encode(id_b)],
            "every custodied box must surface as one `recover-box-item` row, in \
             map order — a domainless box (id_b) included"
        );
    }

    /// A box that rotated its deployment seed is offered only at its **successor**
    /// identity: the hub drops the superseded row, and the installer command for it
    /// resolves to `None` rather than a `.env` line that would rebuild an identity
    /// every converged client refuses (`box-recovery.md` § Custody after rotation).
    ///
    /// The `selfhosted_recovery_command_in` half is deliberately *not* a second
    /// filter — it inherits the refusal from `DeploymentSeedEntry::seed_for`, which
    /// is the one resolution point every recovery reader passes through (the
    /// re-provision drive included).
    #[test]
    fn a_superseded_box_leaves_the_hub_and_its_installer_command() {
        let (mut map, id_a, id_b) = map_with_two_boxes();
        map[0].superseded_by = Some(id_b);

        assert_eq!(
            recoverable_box_ids_in(&map),
            vec![hex::encode(id_b)],
            "the hub must be structurally unable to offer the refused ancestor"
        );
        assert_eq!(
            selfhosted_recovery_command_in(&map, &hex::encode(id_a)),
            None
        );
        assert!(selfhosted_recovery_command_in(&map, &hex::encode(id_b)).is_some());
    }

    #[test]
    fn no_custodied_boxes_projects_an_empty_list() {
        // Drives `recover-box-empty-message` on every app.
        assert!(recoverable_box_ids_in(&[]).is_empty());
    }

    #[test]
    fn selfhosted_command_resolves_the_selected_boxs_own_seed() {
        let (map, id_a, id_b) = map_with_two_boxes();
        // Each box must render ITS seed: rendering the wrong box's seed would
        // boot the rebuilt box with a DIFFERENT nest_actor_id, which every
        // TOFU-pinned client then rejects (box-recovery.md § Goal assertion 1).
        assert_eq!(
            selfhosted_recovery_command_in(&map, &hex::encode(id_a)).unwrap(),
            selfhosted_recovery_command(&[0x11u8; 32]),
        );
        assert_eq!(
            selfhosted_recovery_command_in(&map, &hex::encode(id_b)).unwrap(),
            selfhosted_recovery_command(&[0x22u8; 32]),
        );
    }

    #[test]
    fn selfhosted_command_is_none_for_an_uncustodied_or_malformed_box() {
        let (map, _, _) = map_with_two_boxes();
        // Not custodied → the caller shows the pending placeholder, never a
        // command carrying someone else's seed.
        let uncustodied =
            fauna_core::data::DeploymentSeedEntry::nest_actor_id_for_seed([0xCCu8; 32]);
        assert!(selfhosted_recovery_command_in(&map, &hex::encode(uncustodied)).is_none());
        // Malformed input → None, not a panic.
        assert!(selfhosted_recovery_command_in(&map, "not-hex").is_none());
        assert!(selfhosted_recovery_command_in(&map, "ab").is_none());
    }
}
