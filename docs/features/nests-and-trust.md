---
slug: nests-and-trust
title: Your nests and what each may read
section: your data and devices
goal: docs/goal/ui/nests.md § What this page is
guide: docs/guides/nest-relay-setup.md § Step 1 — Link the two nests
---

## What a user gets

Link another nest of yours so your account lives on both, and see on the Nests
page exactly what each nest is trusted to read: every grant you minted, its expiry,
its history, and a revoke for each. Backup destinations and past backup generations
show there too, and the admin can refuse linking altogether.

## Coverage contract

Stamped 2026-09-23 at bf97909dbe.

1. [app] Link a nest, see it listed, unlink it; one action links both ends — `docs/goal/behavior/linked-nests.md` § The surface
   - `tests/e2e-unified/tests/test_linked_nests.py::test_linked_nests_page_renders`
   - `tests/e2e-unified/tests/test_linked_nests.py::test_link_list_unlink_round_trip`
   - `tests/e2e-unified/tests/test_linked_nests.py::test_link_both_seeds_both_nests`
2. [app] When the admin turned pairing off, linking is refused with the reason — `docs/goal/behavior/linked-nests.md` § The surface
   - `tests/e2e-unified/tests/test_linked_nests.py::test_admin_knob_off_rejects_link`
3. [app] The trust facet says honestly when a nest may read nothing, and its history lens shows the grant timeline — `docs/goal/ui/nests.md` § Trust facet — grants (Now lens)
   - `tests/e2e-unified/tests/test_nest_trust.py::test_nest_trust_facet_renders_empty`
   - `tests/e2e-unified/tests/test_nest_trust.py::test_nest_trust_lens_toggle_switches_now_history`
4. [app] You mint a grant that lets your nest serve paywalled posts, and it is listed — `docs/goal/ui/nests.md` § Trust facet — grants (Now lens)
   - `tests/e2e-unified/tests/test_nest_trust.py::test_nest_trust_mint_paywalled_posts_grant`
5. [app] Backup destinations appear as trust rows, revoke reaches the destination, and an older backup generation can be restored from it — `docs/goal/ui/nests.md` § Trust facet — backup rows
   - `tests/e2e-unified/tests/test_nest_trust.py::test_backup_trust_rows_render_and_revoke_lands_at_the_destination`
   - `tests/e2e-unified/tests/test_nest_trust.py::test_retained_generations_render_and_restore_lands_at_the_destination`
6. [nest] Two linked nests sync your namespace both ways, and an unlinked nest is refused — `docs/goal/architecture/nest/private-mode.md` § Namespace Sync
   - `tests/e2e-unified/tests/api/test_namespace_sync.py::test_sync_push_and_pull_roundtrip`
   - `tests/e2e-unified/tests/api/test_namespace_sync.py::test_sync_pull_rejected_when_not_paired`
7. [app] Your home nest is on the page beside the linked ones, carrying its own trust facet — `docs/goal/ui/nests.md` § What this page is
   - `tests/e2e-unified/tests/test_nest_trust.py::test_nest_trust_facet_renders_empty`
8. [app] You revoke a grant from its row; it leaves Now, stays in History, and the nest goes dark on it — `docs/goal/ui/nests.md` § Trust facet — grants (Now lens)
   - `tests/e2e-unified/tests/test_nest_trust_grants.py::test_revoking_a_grant_leaves_now_stays_in_history_and_darkens_the_nest`
9. [app] A grant about to lapse, or lapsed, reads as paused work with a renew control — never as a feature that quietly stopped — `docs/goal/ui/nests.md` § Expiry / renewal — first-class states
   - `tests/e2e-unified/tests/test_nest_trust_grants.py::test_a_lapsing_grant_reads_expiring_then_paused_and_renew_recovers_it`
   - `tests/e2e-unified/tests/test_nest_trust_grants.py::test_a_subscribed_labelers_trust_renews_with_its_epoch_keys`
10. [app] Every grant row says plainly what revoking cannot undo — `docs/goal/ui/nests.md` § Honest bound
   - `tests/e2e-unified/tests/test_nest_trust_grants.py::test_revoking_a_grant_leaves_now_stays_in_history_and_darkens_the_nest`
11. [app] You choose a grant's length and which nests you bless; a blessed nest's grants renew themselves and a one-off grant lasts hours — `docs/goal/ui/nests.md` § Expiry / renewal — first-class states
   - `tests/e2e-unified/tests/test_nest_trust_grants.py::test_a_blessed_nests_trust_renews_itself_and_a_one_off_trust_lasts_hours`
12. [app] From the same picker you can let a nest read and filter your mail, or read your calendar — `docs/goal/ui/nests.md` § Trust facet — grants (Now lens)
   - `tests/e2e-unified/tests/test_nest_trust_grants.py::test_the_picker_mints_mail_and_calendar_trust_to_the_mail_holder`
13. [app] You are offered only trust that can actually be given; with nothing to give, no mint control shows — `docs/goal/ui/nests.md` § Trust facet — grants (Now lens)
   - `tests/e2e-unified/tests/test_nest_trust.py::test_nest_trust_lens_toggle_switches_now_history`
14. [app] A backup destination you cannot reach reads unreachable, never missing — `docs/goal/ui/nests.md` § Trust facet — backup rows
   - `tests/e2e-unified/tests/test_nest_trust_backup_states.py::test_an_unreachable_destination_reads_unreachable_never_missing_or_nothing_to_recover`
15. [app] You can stop your nest sealing new backups; copies already held stay until you reclaim them — `docs/goal/ui/nests.md` § Trust facet — backup rows
   - `tests/e2e-unified/tests/test_nest_trust_backup_states.py::test_stopping_the_seal_stops_new_backups_and_leaves_held_copies`
16. [app] In recovery, a destination that cannot be asked shows as unreachable — never as nothing to recover — and a version with no readable path is still listed — `docs/goal/ui/nests.md` § Trust facet — generation recovery
   - `tests/e2e-unified/tests/test_nest_trust_backup_states.py::test_an_unreachable_destination_reads_unreachable_never_missing_or_nothing_to_recover`
   - `tests/e2e-unified/tests/test_nest_trust_backup_states.py::test_a_path_less_version_is_listed_and_a_restore_past_the_window_says_so`
17. [app] Restoring a version past its recovery window says so, never as a failure — `docs/goal/ui/nests.md` § Trust facet — generation recovery
   - `tests/e2e-unified/tests/test_nest_trust_backup_states.py::test_a_path_less_version_is_listed_and_a_restore_past_the_window_says_so`
18. [app] Old versions kept for recovery are shown as counting against your storage until they expire — `docs/goal/ui/nests.md` § Trust facet — generation recovery
   - `tests/e2e-unified/tests/test_nest_trust.py::test_retained_generations_render_and_restore_lands_at_the_destination`
19. [app] A custodian's latest confirmation reads fresh, stale or never-confirmed in different words, with how much it holds; a stale one shows as weakened protection rather than vanishing — `docs/goal/ui/nests.md` § Trust facet — custody rows
   - `tests/e2e-unified/tests/test_custody_ceremony_journey.py::test_custody_ceremony_nest_anchored`
20. [app] The nest holding your recovery escrow wears a badge saying so — `docs/goal/behavior/participants.md` § The participant model
   - `tests/e2e-unified/tests/test_custody_ceremony_journey.py::test_custody_ceremony_nest_anchored`
21. [nest] A linked nest can sync only your own account's entries — never read or overwrite another account's — `docs/goal/architecture/nest/private-mode.md` § Namespace Sync
   - `tests/e2e-unified/tests/api/test_namespace_sync.py::test_sync_refuses_a_namespace_that_is_not_the_paired_actors`
22. [nest] A nest relaying your synced data holds only sealed bytes it cannot read — `docs/goal/architecture/nest/private-mode.md` § Namespace Sync
   - `tests/e2e-unified/tests/api/test_namespace_sync.py::test_the_relay_holds_only_sealed_bytes_it_cannot_read`
23. [app] A post your home nest could not hand to its relay shows on the Nests page with the nest's own reason; linking the relay from there delivers it, and you can stop forwarding the ones you no longer want relayed — `docs/goal/ui/nests.md` § Forward queue
   - `tests/e2e-unified/tests/test_forward_queue.py::test_a_refused_forward_shows_on_the_nests_page_and_linking_the_relay_delivers_it`
   - `tests/e2e-unified/tests/test_forward_queue.py::test_stop_forwarding_drops_the_queue_and_keeps_the_post`

## Status

<!-- features-render:begin -->
| App | Status | Stamp |
|---|---|---|
| web | ⚠ partial | 0.1.2-dev+a1f84cc6 standalone |
| linux | ⚠ partial | 0.1.2-dev+a1f84cc6 standalone |
| windows | ⚠ partial | 0.1.2-dev+a1f84cc6 standalone |
| macos | ⚠ partial | 0.1.2-dev+a1f84cc6 standalone |
| ios | ⚠ partial | 0.1.2-dev+a1f84cc6 standalone |
| android |  no run recorded | |
| tui | ⚠ partial | 0.1.2-dev+7663afa6 standalone |

| Outcome | Surface | Witness | Newest outcome |
|---|---|---|---|
| 1 | app | `tests/e2e-unified/tests/test_linked_nests.py::test_linked_nests_page_renders` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 1 | app | `tests/e2e-unified/tests/test_linked_nests.py::test_link_list_unlink_round_trip` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 1 | app | `tests/e2e-unified/tests/test_linked_nests.py::test_link_both_seeds_both_nests` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): failed |
| 2 | app | `tests/e2e-unified/tests/test_linked_nests.py::test_admin_knob_off_rejects_link` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 3 | app | `tests/e2e-unified/tests/test_nest_trust.py::test_nest_trust_facet_renders_empty` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed, tui (windows): passed |
| 3 | app | `tests/e2e-unified/tests/test_nest_trust.py::test_nest_trust_lens_toggle_switches_now_history` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed, tui (windows): passed |
| 4 | app | `tests/e2e-unified/tests/test_nest_trust.py::test_nest_trust_mint_paywalled_posts_grant` | web (linux): passed, linux (linux): passed, windows (windows): failed, macos (macos): passed, ios (macos): passed, tui (linux): passed, tui (windows): passed |
| 5 | app | `tests/e2e-unified/tests/test_nest_trust.py::test_backup_trust_rows_render_and_revoke_lands_at_the_destination` | web (linux): passed, linux (linux): failed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed, tui (windows): passed |
| 5 | app | `tests/e2e-unified/tests/test_nest_trust.py::test_retained_generations_render_and_restore_lands_at_the_destination` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed, tui (windows): passed |
| 6 | nest | `tests/e2e-unified/tests/api/test_namespace_sync.py::test_sync_push_and_pull_roundtrip` | nest (linux): passed |
| 6 | nest | `tests/e2e-unified/tests/api/test_namespace_sync.py::test_sync_pull_rejected_when_not_paired` | nest (linux): passed |
| 7 | app | `tests/e2e-unified/tests/test_nest_trust.py::test_nest_trust_facet_renders_empty` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed, tui (windows): passed |
| 8 | app | `tests/e2e-unified/tests/test_nest_trust_grants.py::test_revoking_a_grant_leaves_now_stays_in_history_and_darkens_the_nest` | web (linux): passed, linux (linux): passed, tui (linux): passed |
| 9 | app | `tests/e2e-unified/tests/test_nest_trust_grants.py::test_a_lapsing_grant_reads_expiring_then_paused_and_renew_recovers_it` | web (linux): passed, linux (linux): passed, tui (linux): passed |
| 9 | app | `tests/e2e-unified/tests/test_nest_trust_grants.py::test_a_subscribed_labelers_trust_renews_with_its_epoch_keys` | tui (linux): passed |
| 10 | app | `tests/e2e-unified/tests/test_nest_trust_grants.py::test_revoking_a_grant_leaves_now_stays_in_history_and_darkens_the_nest` | web (linux): passed, linux (linux): passed, tui (linux): passed |
| 11 | app | `tests/e2e-unified/tests/test_nest_trust_grants.py::test_a_blessed_nests_trust_renews_itself_and_a_one_off_trust_lasts_hours` | web (linux): passed, linux (linux): passed, tui (linux): passed |
| 12 | app | `tests/e2e-unified/tests/test_nest_trust_grants.py::test_the_picker_mints_mail_and_calendar_trust_to_the_mail_holder` | web (linux): passed, linux (linux): passed, tui (linux): passed |
| 13 | app | `tests/e2e-unified/tests/test_nest_trust.py::test_nest_trust_lens_toggle_switches_now_history` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed, tui (windows): passed |
| 14 | app | `tests/e2e-unified/tests/test_nest_trust_backup_states.py::test_an_unreachable_destination_reads_unreachable_never_missing_or_nothing_to_recover` | web (linux): passed, linux (linux): passed, tui (linux): failed |
| 15 | app | `tests/e2e-unified/tests/test_nest_trust_backup_states.py::test_stopping_the_seal_stops_new_backups_and_leaves_held_copies` | web (linux): passed, linux (linux): passed, tui (linux): passed |
| 16 | app | `tests/e2e-unified/tests/test_nest_trust_backup_states.py::test_an_unreachable_destination_reads_unreachable_never_missing_or_nothing_to_recover` | web (linux): passed, linux (linux): passed, tui (linux): failed |
| 16 | app | `tests/e2e-unified/tests/test_nest_trust_backup_states.py::test_a_path_less_version_is_listed_and_a_restore_past_the_window_says_so` | web (linux): passed, linux (linux): failed, tui (linux): failed |
| 17 | app | `tests/e2e-unified/tests/test_nest_trust_backup_states.py::test_a_path_less_version_is_listed_and_a_restore_past_the_window_says_so` | web (linux): passed, linux (linux): failed, tui (linux): failed |
| 18 | app | `tests/e2e-unified/tests/test_nest_trust.py::test_retained_generations_render_and_restore_lands_at_the_destination` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed, tui (windows): passed |
| 19 | app | `tests/e2e-unified/tests/test_custody_ceremony_journey.py::test_custody_ceremony_nest_anchored` | linux (linux): passed, macos (macos): passed, tui (linux): passed, tui (macos): passed |
| 20 | app | `tests/e2e-unified/tests/test_custody_ceremony_journey.py::test_custody_ceremony_nest_anchored` | linux (linux): passed, macos (macos): passed, tui (linux): passed, tui (macos): passed |
| 21 | nest | `tests/e2e-unified/tests/api/test_namespace_sync.py::test_sync_refuses_a_namespace_that_is_not_the_paired_actors` | nest (linux): passed |
| 22 | nest | `tests/e2e-unified/tests/api/test_namespace_sync.py::test_the_relay_holds_only_sealed_bytes_it_cannot_read` | nest (linux): passed |
| 23 | app | `tests/e2e-unified/tests/test_forward_queue.py::test_a_refused_forward_shows_on_the_nests_page_and_linking_the_relay_delivers_it` | linux (linux): passed, tui (linux): passed |
| 23 | app | `tests/e2e-unified/tests/test_forward_queue.py::test_stop_forwarding_drops_the_queue_and_keeps_the_post` | linux (linux): passed, tui (linux): passed |
<!-- features-render:end -->
