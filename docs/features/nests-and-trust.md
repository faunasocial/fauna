---
slug: nests-and-trust
title: Your nests and what each may read
section: your data and devices
goal: docs/goal/ui/nests.md § What this page is
guide: docs/guides/nest-relay-setup.md § Step 1 — Link the two nests
absences:
  android-kids: "docs/goal/behavior/account-age-band.md § The account age band"
  ios-kids: "docs/goal/behavior/account-age-band.md § The account age band"
---

## What a user gets

Link another nest of yours so your account lives on both, and see on the Nests
page what each nest is trusted to read: every permission you gave it, its
expiry, its history, and a revoke for each. Backup destinations and past backup
generations show there too, and the admin can refuse linking altogether.

## Coverage contract

Stamped 2026-11-07 at ac836c937b.

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
12. [app] From the same picker you can let a nest read and filter your mail, or read your calendar — and a calendar grant opens calendar records and never your mail — `docs/goal/ui/nests.md` § Trust facet — grants (Now lens)
   - `tests/e2e-unified/tests/test_nest_trust_grants.py::test_the_picker_mints_mail_and_calendar_trust_to_the_mail_holder`
   - `tests/e2e-unified/tests/test_nest_trust_grants.py::test_a_calendar_grant_opens_calendar_records_and_never_mail`
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
   - `tests/e2e-unified/tests/test_custody_ceremony_journey.py::test_a_custodian_nests_receipt_reads_never_fresh_then_stale_on_web`
20. [app] The nest holding your recovery escrow wears a badge saying so — `docs/goal/behavior/participants.md` § The participant model
   - `tests/e2e-unified/tests/test_custody_ceremony_journey.py::test_custody_ceremony_nest_anchored`
   - `tests/e2e-unified/tests/test_escrow_holder_badge_web.py::test_web_home_nest_wears_the_escrow_holder_badge`
21. [nest] A linked nest can sync only your own account's entries — never read or overwrite another account's — `docs/goal/architecture/nest/private-mode.md` § Namespace Sync
   - `tests/e2e-unified/tests/api/test_namespace_sync.py::test_sync_refuses_a_namespace_that_is_not_the_paired_actors`
22. [nest] A nest relaying your synced data holds only sealed bytes it cannot read — `docs/goal/architecture/nest/private-mode.md` § Namespace Sync
   - `tests/e2e-unified/tests/api/test_namespace_sync.py::test_the_relay_holds_only_sealed_bytes_it_cannot_read`
23. [app] A post your home nest could not hand to its relay shows on the Nests page with the nest's own reason; linking the relay from there delivers it, and you can stop forwarding the ones you no longer want relayed — `docs/goal/ui/nests.md` § Forward queue
   - `tests/e2e-unified/tests/test_forward_queue.py::test_a_refused_forward_shows_on_the_nests_page_and_linking_the_relay_delivers_it`
   - `tests/e2e-unified/tests/test_forward_queue.py::test_stop_forwarding_drops_the_queue_and_keeps_the_post`
24. [app] With every device and your nest gone, a new device holding your identity secret gets your account's records back from a friend's nest, by the address their app shows you; a refused or unanswered address says so, and what came back sealed is counted, never called restored — `docs/goal/ui/nests.md` § Trust facet — restore from a custodian nest
   - `tests/e2e-unified/tests/test_custody_cold_device_restore.py::test_a_cold_device_restores_its_account_state_from_a_custodian_nest`
25. [app] The grant picker offers to serve paywalled posts only for tiers you created yourself, never for your free followers, only-me or room tiers — `docs/goal/ui/nests.md` § Trust facet — grants (Now lens)
   - `tests/e2e-unified/tests/test_nest_trust.py::test_the_post_option_names_only_tiers_the_author_created`
26. [nest] A grant to read your calendar lets the nest open calendar records only, and a grant to read your mail opens no event or contact card — `docs/goal/ui/nests.md` § Trust facet — grants (Now lens)
   - `tests/e2e-unified/tests/api/test_nest_trust_content_keys.py::test_a_calendar_trust_opens_only_calendar_records_and_a_mail_trust_no_event_or_card`
27. [app] A live grant whose holder the nest leaves out of its roster, or whose roster could not be read, is still listed with its revoke control, and the page says the roster could not be read — `docs/goal/ui/nests.md` § Trust facet — grants (Now lens)
   - `tests/e2e-unified/tests/test_nest_trust.py::test_a_live_grant_stays_listed_with_revoke_when_the_roster_omits_or_fails`
28. [app] While your own nest cannot be reached, your devices keep getting your account's records from a friend's nest that holds a copy, with nothing to do, and go back to your nest once it answers; a nest whose permission you took back is never asked — `docs/goal/architecture/account-data-plane.md` § Implementation status today
   - `tests/e2e-unified/tests/test_custody_custodian_nest_fallback.py::test_devices_fall_back_to_a_custodian_nest_only_while_the_home_nest_is_down`
29. [app] A nest linked or a grant minted on another of your devices shows on an open Nests page without leaving it — `docs/goal/architecture/account-runtime.md` § Multi-instance concurrency
   - `tests/e2e-unified/tests/test_nests_page_sibling_repaint.py::test_a_grant_minted_on_a_sibling_paints_on_an_open_nests_page`
   - `tests/e2e-unified/tests/test_nests_page_sibling_repaint.py::test_a_nest_linked_on_a_sibling_paints_on_an_open_nests_page`
30. [app] A linked nest's capability line says in plain words that it keeps a copy of your account; new links carry it by default, re-linking an old pairing adds it, and unlinking deletes your copy's key wraps there — `docs/goal/behavior/linked-nests.md` § The surface
   - `tests/e2e-unified/tests/test_linked_nests.py::test_the_account_copy_capability_is_named_carried_on_relink_and_its_wraps_swept_on_unlink`
31. [app] You let your nest keep your mail searchable, told plainly before you confirm what it will read and keep; its search finds your mail, and revoking deletes the copy — `docs/goal/ui/nests.md` § Trust facet — grants (Now lens)
   - `tests/e2e-unified/tests/test_nest_trust_grants.py::test_keeping_your_mail_searchable_on_your_nest_states_its_bound_and_revoking_deletes_the_copy`
32. [app] You let your nest keep your calendar searchable, told plainly before you confirm what it will read and keep; its search finds an event your calendar app saved, and revoking deletes that copy and leaves your contacts' copy — `docs/goal/ui/nests.md` § Trust facet — grants (Now lens)
   - `tests/e2e-unified/tests/test_nest_trust_grants.py::test_keeping_your_calendar_searchable_on_your_nest_states_its_bound_and_revoking_deletes_that_copy_alone`
33. [app] You let your nest keep your contacts searchable, told plainly before you confirm what it will read and keep; its search finds a card your contacts app saved, and revoking deletes that copy and leaves your calendar's copy — `docs/goal/ui/nests.md` § Trust facet — grants (Now lens)
   - `tests/e2e-unified/tests/test_nest_trust_grants.py::test_keeping_your_contacts_searchable_on_your_nest_states_its_bound_and_revoking_deletes_that_copy_alone`

## Status

<!-- features-render:begin -->
| App | Status | Stamp |
|---|---|---|
| web | ⚠ partial | 0.1.5-dev+3ef45f1b standalone |
| linux | ⚠ partial | 0.1.5-dev+3ef45f1b standalone |
| windows | 🚧 unbuilt | 0.1.5-dev+3ef45f1b standalone |
| macos | 🚧 unbuilt | 0.1.5-dev+3ef45f1b standalone |
| ios | ⚠ partial | 0.1.5-dev+3ef45f1b standalone |
| android |  no run recorded | |
| tui | ✅ full | 0.1.5-dev+3ef45f1b standalone |
| android-kids | — absent | |
| ios-kids | — absent | |

| Outcome | Surface | Witness | Newest outcome |
|---|---|---|---|
| 1 | app | `tests/e2e-unified/tests/test_linked_nests.py::test_linked_nests_page_renders` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed, tui (macos): passed |
| 1 | app | `tests/e2e-unified/tests/test_linked_nests.py::test_link_list_unlink_round_trip` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed, tui (macos): passed |
| 1 | app | `tests/e2e-unified/tests/test_linked_nests.py::test_link_both_seeds_both_nests` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed, tui (macos): passed |
| 2 | app | `tests/e2e-unified/tests/test_linked_nests.py::test_admin_knob_off_rejects_link` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed, tui (macos): passed |
| 3 | app | `tests/e2e-unified/tests/test_nest_trust.py::test_nest_trust_facet_renders_empty` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed, tui (macos): passed, tui (windows): passed |
| 3 | app | `tests/e2e-unified/tests/test_nest_trust.py::test_nest_trust_lens_toggle_switches_now_history` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed, tui (macos): passed, tui (windows): passed |
| 4 | app | `tests/e2e-unified/tests/test_nest_trust.py::test_nest_trust_mint_paywalled_posts_grant` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed, tui (macos): passed, tui (windows): passed |
| 5 | app | `tests/e2e-unified/tests/test_nest_trust.py::test_backup_trust_rows_render_and_revoke_lands_at_the_destination` — requires `backup-destination-trust-rows` (test harness) (not built: android, android-kids) | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed, tui (macos): passed, tui (windows): passed |
| 5 | app | `tests/e2e-unified/tests/test_nest_trust.py::test_retained_generations_render_and_restore_lands_at_the_destination` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed, tui (macos): passed, tui (windows): passed |
| 6 | nest | `tests/e2e-unified/tests/api/test_namespace_sync.py::test_sync_push_and_pull_roundtrip` | nest (linux): passed, nest (macos): passed |
| 6 | nest | `tests/e2e-unified/tests/api/test_namespace_sync.py::test_sync_pull_rejected_when_not_paired` | nest (linux): passed, nest (macos): passed |
| 7 | app | `tests/e2e-unified/tests/test_nest_trust.py::test_nest_trust_facet_renders_empty` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed, tui (macos): passed, tui (windows): passed |
| 8 | app | `tests/e2e-unified/tests/test_nest_trust_grants.py::test_revoking_a_grant_leaves_now_stays_in_history_and_darkens_the_nest` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 9 | app | `tests/e2e-unified/tests/test_nest_trust_grants.py::test_a_lapsing_grant_reads_expiring_then_paused_and_renew_recovers_it` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 9 | app | `tests/e2e-unified/tests/test_nest_trust_grants.py::test_a_subscribed_labelers_trust_renews_with_its_epoch_keys` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 10 | app | `tests/e2e-unified/tests/test_nest_trust_grants.py::test_revoking_a_grant_leaves_now_stays_in_history_and_darkens_the_nest` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 11 | app | `tests/e2e-unified/tests/test_nest_trust_grants.py::test_a_blessed_nests_trust_renews_itself_and_a_one_off_trust_lasts_hours` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 12 | app | `tests/e2e-unified/tests/test_nest_trust_grants.py::test_the_picker_mints_mail_and_calendar_trust_to_the_mail_holder` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 12 | app | `tests/e2e-unified/tests/test_nest_trust_grants.py::test_a_calendar_grant_opens_calendar_records_and_never_mail` | web (linux): passed, linux (linux): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 13 | app | `tests/e2e-unified/tests/test_nest_trust.py::test_nest_trust_lens_toggle_switches_now_history` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed, tui (macos): passed, tui (windows): passed |
| 14 | app | `tests/e2e-unified/tests/test_nest_trust_backup_states.py::test_an_unreachable_destination_reads_unreachable_never_missing_or_nothing_to_recover` — requires `backup-destination-trust-rows` (test harness) (not built: android, android-kids) | web (linux): passed, linux (linux): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed, tui (macos): passed |
| 15 | app | `tests/e2e-unified/tests/test_nest_trust_backup_states.py::test_stopping_the_seal_stops_new_backups_and_leaves_held_copies` — requires `backup-destination-trust-rows` (test harness) (not built: android, android-kids) | web (linux): passed, linux (linux): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed, tui (macos): passed |
| 16 | app | `tests/e2e-unified/tests/test_nest_trust_backup_states.py::test_an_unreachable_destination_reads_unreachable_never_missing_or_nothing_to_recover` — requires `backup-destination-trust-rows` (test harness) (not built: android, android-kids) | web (linux): passed, linux (linux): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed, tui (macos): passed |
| 16 | app | `tests/e2e-unified/tests/test_nest_trust_backup_states.py::test_a_path_less_version_is_listed_and_a_restore_past_the_window_says_so` | web (linux): passed, linux (linux): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed, tui (macos): passed |
| 17 | app | `tests/e2e-unified/tests/test_nest_trust_backup_states.py::test_a_path_less_version_is_listed_and_a_restore_past_the_window_says_so` | web (linux): passed, linux (linux): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed, tui (macos): passed |
| 18 | app | `tests/e2e-unified/tests/test_nest_trust.py::test_retained_generations_render_and_restore_lands_at_the_destination` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed, tui (macos): passed, tui (windows): passed |
| 19 | app | `tests/e2e-unified/tests/test_custody_ceremony_journey.py::test_custody_ceremony_nest_anchored` | linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): failed, tui (linux): passed, tui (macos): passed |
| 19 | app | `tests/e2e-unified/tests/test_custody_ceremony_journey.py::test_a_custodian_nests_receipt_reads_never_fresh_then_stale_on_web` | web (linux): passed |
| 20 | app | `tests/e2e-unified/tests/test_custody_ceremony_journey.py::test_custody_ceremony_nest_anchored` | linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): failed, tui (linux): passed, tui (macos): passed |
| 20 | app | `tests/e2e-unified/tests/test_escrow_holder_badge_web.py::test_web_home_nest_wears_the_escrow_holder_badge` | web (linux): passed |
| 21 | nest | `tests/e2e-unified/tests/api/test_namespace_sync.py::test_sync_refuses_a_namespace_that_is_not_the_paired_actors` | nest (linux): passed, nest (macos): passed |
| 22 | nest | `tests/e2e-unified/tests/api/test_namespace_sync.py::test_the_relay_holds_only_sealed_bytes_it_cannot_read` | nest (linux): passed, nest (macos): passed |
| 23 | app | `tests/e2e-unified/tests/test_forward_queue.py::test_a_refused_forward_shows_on_the_nests_page_and_linking_the_relay_delivers_it` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 23 | app | `tests/e2e-unified/tests/test_forward_queue.py::test_stop_forwarding_drops_the_queue_and_keeps_the_post` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 24 | app | `tests/e2e-unified/tests/test_custody_cold_device_restore.py::test_a_cold_device_restores_its_account_state_from_a_custodian_nest` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): failed, tui (linux): passed, tui (macos): passed |
| 25 | app | `tests/e2e-unified/tests/test_nest_trust.py::test_the_post_option_names_only_tiers_the_author_created` | web (linux): passed, linux (linux): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed, tui (macos): passed |
| 26 | nest | `tests/e2e-unified/tests/api/test_nest_trust_content_keys.py::test_a_calendar_trust_opens_only_calendar_records_and_a_mail_trust_no_event_or_card` | nest (linux): passed |
| 27 | app | `tests/e2e-unified/tests/test_nest_trust.py::test_a_live_grant_stays_listed_with_revoke_when_the_roster_omits_or_fails` | web (linux): passed, linux (linux): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed, tui (macos): passed |
| 28 | app | `tests/e2e-unified/tests/test_custody_custodian_nest_fallback.py::test_devices_fall_back_to_a_custodian_nest_only_while_the_home_nest_is_down` | web (linux): passed, linux (linux): passed, macos (macos): passed, ios (macos): failed, tui (linux): passed |
| 29 | app | `tests/e2e-unified/tests/test_nests_page_sibling_repaint.py::test_a_grant_minted_on_a_sibling_paints_on_an_open_nests_page` | tui (linux): passed |
| 29 | app | `tests/e2e-unified/tests/test_nests_page_sibling_repaint.py::test_a_nest_linked_on_a_sibling_paints_on_an_open_nests_page` | tui (linux): passed |
| 30 | app | `tests/e2e-unified/tests/test_linked_nests.py::test_the_account_copy_capability_is_named_carried_on_relink_and_its_wraps_swept_on_unlink` | web (linux): passed, linux (linux): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed, tui (macos): passed |
| 31 | app | `tests/e2e-unified/tests/test_nest_trust_grants.py::test_keeping_your_mail_searchable_on_your_nest_states_its_bound_and_revoking_deletes_the_copy` — requires `nest-trust-mint-materialize-note` (not built: windows, macos, ios, android-kids, ios-kids) | web (linux): passed, linux (linux): passed, ios (macos): skipped, tui (linux): passed |
| 32 | app | `tests/e2e-unified/tests/test_nest_trust_grants.py::test_keeping_your_calendar_searchable_on_your_nest_states_its_bound_and_revoking_deletes_that_copy_alone` — requires `nest-trust-mint-materialize-note` (not built: windows, macos, ios, android-kids, ios-kids) | web (linux): passed, ios (macos): skipped, tui (linux): passed |
| 33 | app | `tests/e2e-unified/tests/test_nest_trust_grants.py::test_keeping_your_contacts_searchable_on_your_nest_states_its_bound_and_revoking_deletes_that_copy_alone` — requires `nest-trust-mint-materialize-note` (not built: windows, macos, ios, android-kids, ios-kids) | web (linux): passed, ios (macos): skipped, tui (linux): passed |
<!-- features-render:end -->
