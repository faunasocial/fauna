//! The bounded-mail grant **assembly core** — the ONE place the § 2 bounded
//! mint shape and the 2026-07-19 rotation-heal amendment's generation-interval
//! rule live (content-sealing-epochs design § 2 + Revision history 2026-07-19;
//! owner doc `encryption-at-rest.md` § Capability tiering). Both production
//! mint paths call it — the client wrapper
//! (`fauna_client_capabilities::mint_bounded_mail_grant`) and the FFI mirror
//! (`fauna_ffi::build_bounded_mail_grant_blob`) — so the mirror, which exists
//! to represent the production mint in tier_3 harnesses, cannot drift from it.
//!
//! The amendment in brief: a mail grant carries at most ONE wrap per
//! `(scope, epoch)` (universal — the renew plane's slot invariant, enforced
//! mint-side by [`super::check_scope_wraps_policy`]). An epoch that more than
//! one MSEK generation could have sealed (a rotation-boundary epoch + its
//! one-epoch propagation tail) gets a payload CONCATENATING each qualifying
//! generation's 2432-byte per-epoch secret, newest generation first; the one
//! shared length-dispatch site (`unseal_mail_record_with_derived_key`)
//! chunk-trials it, AEAD arbitrating per chunk.

use zeroize::Zeroizing;

use super::{
    GrantBlob, GrantWindow, ScopeTuple, ScopeWraps, WrapError, build_grant_blob_with_epochs,
    derive_mail_epoch_root, derive_recipient_mail_epoch_capability_secret_from_root,
    mail_epoch_range_for_window, mail_sealing_epoch_of,
};

/// One **prior** (retired) MSEK generation feeding a bounded-mail mint,
/// newest-first alongside `MailConfig.prior_mseks`.
///
/// The retirement instant is required: every rotation records it with the
/// retention. (A pre-2026-07-19 retention recorded without one was skipped as
/// provably pre-flip until the compat-remnant sweep retired that arm,
/// `version-compatibility.md` § Dimension 2, program 4.)
#[derive(Debug, Clone)]
pub struct PriorMsekGeneration {
    /// The retired generation's MSEK (the mint derives its mail-epoch root
    /// on demand; the root itself is never wrapped into a grant).
    pub msek: [u8; 32],
    /// Unix-seconds instant the rotation retiring this generation committed.
    pub retired_at_unix: u64,
}

/// The inclusive epoch interval a generation could have sealed content in —
/// the amendment's rule, derived purely from recorded rotation instants (no
/// wall-clock input): from the generation's activation (= the next-older
/// generation's retirement; unbounded-down when unknown) to its retirement
/// **+ 1 epoch** (the schedule-republish propagation / boundary-straddle
/// tail). The current generation is unbounded-up. Past schedule rows are
/// never republished, so a generation's keys stop being sealed-to one
/// propagation tail after its retirement — the interval is sound, not
/// heuristic.
fn generation_seal_interval(activated_unix: Option<u64>, retired_at_unix: u64) -> (u64, u64) {
    let first = activated_unix.map_or(0, mail_sealing_epoch_of);
    let last = mail_sealing_epoch_of(retired_at_unix) + 1;
    (first, last)
}

/// The per-epoch `(epoch, payload)` wraps of a bounded mail grant covering
/// `window` — one wrap per epoch in the window's intersection, each payload
/// the concatenation of every qualifying generation's per-epoch capability
/// secret (newest generation first, k × 2432 bytes). `prior_generations` is
/// newest-first (the `MailConfig.prior_mseks` order). Every window epoch always gets at least one secret: the
/// current generation covers everything from its activation up, and each
/// prior generation covers down to the next-older one's retirement
/// (unbounded-down at the oldest retained).
#[must_use]
pub fn bounded_mail_epoch_wraps(
    msek: &[u8; 32],
    prior_generations: &[PriorMsekGeneration],
    window: &GrantWindow,
) -> Vec<(Option<u64>, Vec<u8>)> {
    bounded_mail_epoch_wraps_for_range(msek, prior_generations, mail_epoch_range_for_window(window))
}

/// [`bounded_mail_epoch_wraps`] over an explicit inclusive epoch range — the
/// renew-extension form (`epoch_of(old_end) + 1 ..= epoch_of(new_end)`) and
/// the rotation-heal re-wrap (the grant's whole window range) share it.
#[must_use]
pub fn bounded_mail_epoch_wraps_for_range(
    msek: &[u8; 32],
    prior_generations: &[PriorMsekGeneration],
    epochs: std::ops::RangeInclusive<u64>,
) -> Vec<(Option<u64>, Vec<u8>)> {
    // Roots + seal intervals, newest generation first. The current
    // generation's activation is the newest prior's retirement (no prior ⇒
    // unbounded-down, the single-generation behavior); each prior's
    // activation is the next-older prior's retirement.
    let current_root = derive_mail_epoch_root(msek);
    let current_first = prior_generations
        .first()
        .map_or(0, |g| mail_sealing_epoch_of(g.retired_at_unix));

    let mut prior_roots: Vec<(Zeroizing<[u8; 32]>, u64, u64)> = Vec::new();
    for (i, g) in prior_generations.iter().enumerate() {
        let activated = prior_generations.get(i + 1).map(|p| p.retired_at_unix);
        let (first, last) = generation_seal_interval(activated, g.retired_at_unix);
        prior_roots.push((derive_mail_epoch_root(&g.msek), first, last));
    }

    epochs
        .map(|e| {
            let mut payload = Vec::new();
            if e >= current_first {
                payload.extend_from_slice(
                    &derive_recipient_mail_epoch_capability_secret_from_root(&current_root, e),
                );
            }
            for (root, first, last) in &prior_roots {
                if e >= *first && e <= *last {
                    payload.extend_from_slice(
                        &derive_recipient_mail_epoch_capability_secret_from_root(root, e),
                    );
                }
            }
            debug_assert!(
                !payload.is_empty(),
                "every window epoch has at least one qualifying generation"
            );
            (Some(e), payload)
        })
        .collect()
}

/// The full scope list of a bounded mail grant: the `content.read{mail}`
/// tuple with its per-epoch wraps, plus (optionally) the keyless
/// `content.label-write` tuple the background scorer's composed role carries.
///
/// `factor` confines BOTH tuples to one bus factor (`ScopeTuple::factor`):
/// `None` is the composed MDA role over the built-in perimeter factors;
/// `Some("labeler:<hex>")` is the **per-labeler** grant a subscription over
/// sealed mail mints — its wraps open the owner's mail
/// only for that labeler's score, and its label-write licenses only that
/// factor. The factor rides inside every wrap's AAD, so the store cannot
/// re-label a wrap as licensing a different labeler.
#[must_use]
pub fn bounded_mail_grant_scopes(
    msek: &[u8; 32],
    prior_generations: &[PriorMsekGeneration],
    window: &GrantWindow,
    include_label_write: bool,
    factor: Option<&str>,
) -> Vec<ScopeWraps> {
    let mail_tuple = match factor {
        Some(f) => ScopeTuple::mail_for_factor(f),
        None => ScopeTuple::mail(),
    };
    let mut scopes: Vec<ScopeWraps> = vec![(
        mail_tuple,
        bounded_mail_epoch_wraps(msek, prior_generations, window),
    )];
    if include_label_write {
        scopes.push((ScopeTuple::label_write(factor), Vec::new()));
    }
    scopes
}

/// Build a **bounded** (crypto-time-boxed) mail grant blob — the shared
/// assembly both production mint paths call. One `WrappedScopeKey { epoch:
/// Some(e) }` per epoch intersecting `window`, never the standing secret
/// (§ 2 mint policy, enforced by the builder), cross-generation boundary
/// coverage inside each payload per the 2026-07-19 amendment.
///
/// # Errors
///
/// [`WrapError::HpkeFailed`] on a wrap seal failure;
/// [`WrapError::InvalidInput`] on a policy violation.
#[allow(clippy::too_many_arguments)]
pub fn build_bounded_mail_grant(
    owner_actor_id: &[u8; 32],
    grant_id: &[u8; 16],
    holder_pubkey: &[u8; 32],
    holder_mlkem_ek: Option<&[u8]>,
    window: GrantWindow,
    msek: &[u8; 32],
    prior_generations: &[PriorMsekGeneration],
    include_label_write: bool,
    factor: Option<&str>,
) -> Result<GrantBlob, WrapError> {
    let scopes = bounded_mail_grant_scopes(
        msek,
        prior_generations,
        &window,
        include_label_write,
        factor,
    );
    build_grant_blob_with_epochs(
        owner_actor_id,
        grant_id,
        holder_pubkey,
        holder_mlkem_ek,
        window,
        &scopes,
    )
}

#[cfg(test)]
mod tests {
    use super::super::MAIL_SEALING_EPOCH_SECS;
    use super::*;

    const SECRET_LEN: usize = 32 + super::super::MLKEM768_DECAPS_KEY_LEN;

    fn instant_in_epoch(e: u64) -> u64 {
        e * MAIL_SEALING_EPOCH_SECS + 10
    }

    fn window_over_epochs(first: u64, last: u64) -> GrantWindow {
        GrantWindow(instant_in_epoch(first), instant_in_epoch(last))
    }

    #[test]
    fn no_priors_yields_one_current_secret_per_epoch() {
        let wraps = bounded_mail_epoch_wraps(&[1u8; 32], &[], &window_over_epochs(100, 104));
        assert_eq!(wraps.len(), 5);
        for (i, (e, payload)) in wraps.iter().enumerate() {
            assert_eq!(*e, Some(100 + i as u64));
            assert_eq!(payload.len(), SECRET_LEN, "single-generation payload");
        }
    }

    #[test]
    fn rotation_boundary_epochs_carry_both_generations_newest_first() {
        // Rotation retired the prior generation inside epoch 102 → epochs
        // 100..=101 are prior-only, 102..=103 (boundary + tail) carry both,
        // 104.. are current-only.
        let msek = [2u8; 32];
        let prior_msek = [3u8; 32];
        let priors = [PriorMsekGeneration {
            msek: prior_msek,
            retired_at_unix: instant_in_epoch(102),
        }];
        let wraps = bounded_mail_epoch_wraps(&msek, &priors, &window_over_epochs(100, 105));

        let by_epoch: Vec<(u64, usize)> = wraps
            .iter()
            .map(|(e, p)| (e.unwrap(), p.len() / SECRET_LEN))
            .collect();
        assert_eq!(
            by_epoch,
            vec![(100, 1), (101, 1), (102, 2), (103, 2), (104, 1), (105, 1)],
            "prior-only below the boundary, both at boundary + tail, current-only above"
        );

        // Pre-boundary epochs carry the PRIOR generation's secret (the only
        // one that sealed content there), not the current's.
        let prior_root = derive_mail_epoch_root(&prior_msek);
        let expected_prior =
            derive_recipient_mail_epoch_capability_secret_from_root(&prior_root, 100);
        assert_eq!(wraps[0].1, expected_prior, "epoch 100 is prior-root");

        // Boundary payload is newest-generation-first.
        let current_root = derive_mail_epoch_root(&msek);
        let expected_current =
            derive_recipient_mail_epoch_capability_secret_from_root(&current_root, 102);
        assert_eq!(
            &wraps[2].1[..SECRET_LEN],
            expected_current.as_slice(),
            "boundary chunk 0 is the current generation"
        );
    }

    #[test]
    fn two_recorded_generations_partition_the_window() {
        // G2 retired in epoch 101, G1 retired in epoch 103: epochs 100..=101
        // reach G2 (+G1 from its activation at 101), boundary tails overlap.
        let msek = [6u8; 32];
        let priors = [
            PriorMsekGeneration {
                msek: [7u8; 32],
                retired_at_unix: instant_in_epoch(103),
            },
            PriorMsekGeneration {
                msek: [8u8; 32],
                retired_at_unix: instant_in_epoch(101),
            },
        ];
        let wraps = bounded_mail_epoch_wraps(&msek, &priors, &window_over_epochs(100, 106));
        let by_epoch: Vec<(u64, usize)> = wraps
            .iter()
            .map(|(e, p)| (e.unwrap(), p.len() / SECRET_LEN))
            .collect();
        // e=100: G2 only (G1 activates at 101, current at 103).
        // e=101: G2 (retired 101 → covers ≤102) + G1 (activated 101) = 2.
        // e=102: G2 tail (≤102) + G1 = 2.  e=103: G1 + current = 2.
        // e=104: G1 tail (≤104) + current = 2.  e=105,106: current only.
        assert_eq!(
            by_epoch,
            vec![
                (100, 1),
                (101, 2),
                (102, 2),
                (103, 2),
                (104, 2),
                (105, 1),
                (106, 1)
            ]
        );
    }

    #[test]
    fn built_blob_has_one_wrap_per_epoch_and_passes_the_builder_policy() {
        let (_sk, holder_pk) = super::super::generate_x25519_keypair();
        let priors = [PriorMsekGeneration {
            msek: [9u8; 32],
            retired_at_unix: instant_in_epoch(301),
        }];
        let blob = build_bounded_mail_grant(
            &[0xAA; 32],
            &[0xBB; 16],
            &holder_pk,
            None,
            window_over_epochs(300, 303),
            &[10u8; 32],
            &priors,
            true,
            None,
        )
        .expect("bounded grant builds");
        let mail_wraps: Vec<_> = blob
            .wrapped_keys
            .iter()
            .filter(|k| k.scope.kind.as_deref() == Some(ScopeTuple::KIND_MAIL))
            .collect();
        assert_eq!(mail_wraps.len(), 4, "one wrap per window epoch");
        let mut epochs: Vec<u64> = mail_wraps.iter().map(|k| k.epoch.unwrap()).collect();
        epochs.dedup();
        assert_eq!(epochs.len(), 4, "no duplicate (scope, epoch) slots");
        assert_eq!(blob.scope.len(), 2, "mail + label-write tuples");
        assert!(
            blob.scope.iter().all(|t| t.factor.is_none()),
            "the composed MDA role licenses the built-in factors only"
        );
    }

    /// The per-labeler shape: both tuples carry the
    /// factor, and so does every wrap — the license rides inside the AAD, so a
    /// holder opening the wrap under a different factor fails to open it.
    #[test]
    fn per_labeler_grant_confines_both_tuples_and_every_wrap_to_the_factor() {
        let (holder_sk, holder_pk) = super::super::generate_x25519_keypair();
        let owner = [0xAA; 32];
        let factor = "labeler:aa";
        let blob = build_bounded_mail_grant(
            &owner,
            &[0xBB; 16],
            &holder_pk,
            None,
            window_over_epochs(300, 301),
            &[10u8; 32],
            &[],
            true,
            Some(factor),
        )
        .expect("bounded labeler grant builds");
        assert_eq!(blob.scope.len(), 2);
        assert!(
            blob.scope
                .iter()
                .all(|t| t.factor.as_deref() == Some(factor)),
            "read and label-write tuples both name the labeler"
        );
        assert_eq!(blob.wrapped_keys.len(), 2, "one wrap per window epoch");
        for wk in &blob.wrapped_keys {
            assert_eq!(wk.scope.factor.as_deref(), Some(factor));
            super::super::unseal_capability(wk, &owner, &holder_sk)
                .expect("the honest wrap opens under its own factor");
            // The store re-labels the wrap as licensing another labeler (or
            // no labeler): the AAD no longer matches and the key stays sealed.
            let mut relabeled = wk.clone();
            relabeled.scope.factor = Some("labeler:bb".into());
            assert!(super::super::unseal_capability(&relabeled, &owner, &holder_sk).is_err());
            let mut widened = wk.clone();
            widened.scope.factor = None;
            assert!(super::super::unseal_capability(&widened, &owner, &holder_sk).is_err());
        }
    }
}
