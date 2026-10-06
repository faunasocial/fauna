---
slug: mail-aliases
title: Mail aliases
section: mail, calendar and contacts
goal: docs/goal/behavior/mail-aliases.md § Aliases UX
guide: docs/guides/own-your-mail.md § Addresses: aliases, plus-addresses, and role addresses
---

## What a user gets

Give out as many addresses as you like: exact aliases, a wildcard prefix,
throwaway addresses minted in one tap, and a list you paste in at once. Switch one
off and back on, set how much mail an alias may receive, and keep your primary
address, which cannot be removed.

## Coverage contract

Stamped 2026-09-23 at 039e9619ca.

1. [app] Mint a throwaway address, add an exact one, switch one off and on, revoke and delete — `docs/goal/behavior/mail-aliases.md` § Aliases UX
   - `tests/e2e-unified/tests/test_mail_aliases.py::test_mail_aliases_page_reachable`
   - `tests/e2e-unified/tests/test_mail_aliases.py::test_mail_aliases_generate_disposable_renders_row`
   - `tests/e2e-unified/tests/test_mail_aliases.py::test_mail_aliases_add_exact_via_sheet`
   - `tests/e2e-unified/tests/test_mail_aliases.py::test_mail_aliases_revoke_then_delete`
   - `tests/e2e-unified/tests/test_mail_aliases.py::test_mail_aliases_revoke_and_delete_by_address_with_canonical_row_present`
   - `tests/e2e-unified/tests/test_mail_aliases_toggle.py::test_active_toggle_disable_then_reenable_round_trips`
2. [app] Paste a list of addresses and they are created in one go, with a count of what was new — `docs/goal/behavior/mail-aliases.md` § Bulk import
   - `tests/e2e-unified/tests/test_mail_aliases_import.py::test_import_addresses_reports_and_relists`
3. [app] Your primary address is shown read-only — `docs/goal/behavior/mail-aliases.md` § Aliases UX
   - `tests/e2e-unified/tests/test_mail_aliases_canonical_readonly.py::test_canonical_alias_row_is_read_only`
4. [app] Turn mail on and the aliases page is ready to use straight away, with no restart — `docs/goal/behavior/mail-aliases.md` § Aliases UX
   - `tests/e2e-unified/tests/test_mail_aliases_midsession_enable.py::test_aliases_page_is_usable_after_enabling_mail_midsession`
5. [nest] Your nest keeps aliases per person, mints wildcards and throwaways, and never lets two people hold one address — `docs/goal/behavior/mail-aliases.md` § Alias kinds
   - `tests/e2e-unified/tests/api/test_mail_aliases_user.py::test_user_alias_crud_round_trip`
   - `tests/e2e-unified/tests/api/test_mail_aliases_user.py::test_wildcard_create_round_trip`
   - `tests/e2e-unified/tests/api/test_mail_aliases_user.py::test_disposable_mint_round_trip`
   - `tests/e2e-unified/tests/api/test_mail_aliases_user.py::test_duplicate_pattern_is_conflict_across_users`
   - `tests/e2e-unified/tests/api/test_alias_import.py::test_import_account_aliases_mixed_batch`
   - `tests/e2e-unified/tests/api/test_mail_aliases_user.py::test_cross_actor_isolation`
   - `tests/e2e-unified/tests/api/test_mail_aliases_user.py::test_wildcard_conflicts_with_other_users_exact`
6. [nest] A per-alias rate cap turns mail away, and a per-address spam threshold rides the message to the filter — `docs/goal/behavior/mail-aliases.md` § Per-alias controls
   - `tests/e2e-unified/tests/test_mail_bridge_mta.py::test_inbound_over_per_alias_rate_cap_tempfails_451`
   - `tests/e2e-unified/tests/test_mail_spam_threshold_override.py::test_per_account_spam_threshold_override_rides_the_message_to_the_scorer`
7. [app] You can add a wildcard prefix from the page, so every address under it is yours — `docs/goal/behavior/mail-aliases.md` § Kind 3 — Wildcard prefix
   - `tests/e2e-unified/tests/test_mail_aliases_controls.py::test_add_wildcard_prefix_from_the_sheet`
8. [app] You can give any address a label and see it on the row — `docs/goal/behavior/mail-aliases.md` § Layout
   - `tests/e2e-unified/tests/test_mail_aliases_controls.py::test_add_sheet_sets_label_spam_threshold_and_hourly_limit`
9. [app] Each address shows how much mail it has received and when it last did — `docs/goal/behavior/mail-aliases.md` § Layout
   - `tests/e2e-unified/tests/test_mail_aliases_controls.py::test_row_shows_hit_count_and_last_hit_after_real_inbound`
10. [app] You can edit an address's label, spam threshold and limit; its kind stays what it was — `docs/goal/behavior/mail-aliases.md` § Layout
   - `tests/e2e-unified/tests/test_mail_aliases_controls.py::test_edit_sheet_changes_label_and_controls_but_never_the_kind`
11. [app] You can set a spam threshold and an hourly mail limit for each address — `docs/goal/behavior/mail-aliases.md` § Per-alias controls
   - `tests/e2e-unified/tests/test_mail_aliases_controls.py::test_add_sheet_sets_label_spam_threshold_and_hourly_limit`
   - `tests/e2e-unified/tests/test_mail_aliases_controls.py::test_edit_sheet_changes_label_and_controls_but_never_the_kind`
12. [app] You choose how long a throwaway address lasts and how many messages it takes — `docs/goal/behavior/mail-aliases.md` § Layout
   - `tests/e2e-unified/tests/test_mail_aliases_controls.py::test_a_throwaway_address_lasts_and_takes_what_you_chose`
13. [app] A new throwaway address goes to the top of the list and is copied for you, with a confirmation — `docs/goal/behavior/mail-aliases.md` § Layout
   - `tests/e2e-unified/tests/test_mail_aliases_controls.py::test_generated_disposable_lands_on_top_copied_and_confirmed`
14. [app] You can see when a throwaway address expires and extend it before it does — `docs/goal/behavior/mail-aliases.md` § Don't do these
   - (none)
15. [app] Opening an address's history shows what came in through it: the address that was hit, the sender's domain and when — `docs/goal/behavior/mail-aliases.md` § Per-alias-hit audit list
   - (none)
16. [app] An address's label is shown beside "delivered to" on the messages it received — `docs/goal/behavior/mail-aliases.md` § Label
   - (none)
17. [app] Claiming a reserved name such as postmaster or abuse is refused, and the page says why — `docs/goal/behavior/mail-aliases.md` § Reserved local-parts (uncircumventable)
   - `tests/e2e-unified/tests/test_mail_aliases_controls.py::test_reserved_name_is_refused_on_the_page`
18. [nest] Mail to your address with anything added after a plus sign reaches you, with nothing to set up — `docs/goal/behavior/mail-aliases.md` § Kind 2 — +suffix sub-addressing (RFC 5233)
   - `tests/e2e-unified/tests/test_mail_bridge_alias_delivery.py::test_plus_suffix_reaches_the_owner_with_nothing_set_up`
19. [nest] Mail to any address under your wildcard prefix reaches you — `docs/goal/behavior/mail-aliases.md` § Kind 3 — Wildcard prefix
   - `tests/e2e-unified/tests/test_mail_bridge_alias_delivery.py::test_wildcard_prefix_address_reaches_the_owner`
20. [nest] You can hold one wildcard prefix, and one too short to be safe is refused — `docs/goal/behavior/mail-aliases.md` § Kind 3 — Wildcard prefix
   - `tests/e2e-unified/tests/api/test_mail_aliases_user.py::test_wildcard_one_per_actor`
   - `tests/e2e-unified/tests/api/test_mail_aliases_user.py::test_wildcard_create_rejections`
21. [nest] A throwaway address stops taking mail once its lifetime or its uses run out, and the sender gets a clear error — `docs/goal/behavior/mail-aliases.md` § Kind 5 — Disposable
   - `tests/e2e-unified/tests/test_mail_bridge_alias_delivery.py::test_disposable_refused_once_uses_or_lifetime_run_out`
22. [nest] Mail to an address you switched off is refused while you keep the address and its history — `docs/goal/behavior/mail-aliases.md` § Disable
   - `tests/e2e-unified/tests/test_mail_bridge_alias_delivery.py::test_disabled_alias_refused_and_kept`
23. [nest] Mail to an address you deleted is refused as unknown, or falls to the domain's catch-all if the admin set one — `docs/goal/behavior/mail-aliases.md` § Don't do these
   - `tests/e2e-unified/tests/test_mail_bridge_alias_delivery.py::test_deleted_alias_unknown_or_falls_to_catch_all`
24. [nest] A reserved name such as postmaster or abuse can never be claimed as an alias — `docs/goal/behavior/mail-aliases.md` § Reserved local-parts (uncircumventable)
   - `tests/e2e-unified/tests/api/test_mail_aliases_user.py::test_create_rejects_reserved_charclass_and_nonexact_kind`
25. [nest] Your nest keeps each address's recent hits for a month, readable by you alone — `docs/goal/behavior/mail-aliases.md` § Per-alias-hit audit list
   - `tests/e2e-unified/tests/api/test_mail_aliases_user.py::test_list_alias_hits_empty_for_owned_alias`
   - `tests/e2e-unified/tests/api/test_mail_aliases_user.py::test_list_alias_hits_owner_isolation`
26. [nest] Changing an address's spam threshold never re-sorts mail already delivered — `docs/goal/behavior/mail-aliases.md` § Spam-threshold override
   - `tests/e2e-unified/tests/test_mail_spam_threshold_override.py::test_changing_an_address_threshold_never_refiles_delivered_mail`
27. [nest] Extra addresses are capped at a number the admin sets, and you are told when you reach it — `docs/goal/behavior/mail-aliases.md` § Kind 1 — Exact
   - `tests/e2e-unified/tests/api/test_mail_alias_policy.py::test_exact_alias_cap_enforced_from_put_alias_policy`
28. [nest] The throwaway addresses you mint in a day are capped too — `docs/goal/behavior/mail-aliases.md` § Don't do these
   - `tests/e2e-unified/tests/api/test_mail_aliases_user.py::test_disposable_mints_capped_per_day`
29. [app] Deleting an address asks you to confirm first, and nothing is deleted until you do — `docs/goal/behavior/mail-aliases.md` § Layout
   - `tests/e2e-unified/tests/test_mail_aliases_controls.py::test_delete_confirm_relabels_the_button_before_deleting`

## Status

<!-- features-render:begin -->
| App | Status | Stamp |
|---|---|---|
| web | ⚠ partial | 0.1.2-dev+0d95e205 standalone |
| linux | ⚠ partial | 0.1.2-dev+c4a95c20 standalone |
| windows | ⚠ partial | |
| macos | ⚠ partial | |
| ios | ⚠ partial | |
| android |  no run recorded | |
| tui | ⚠ partial | 0.1.2-dev+7663afa6 standalone |

| Outcome | Surface | Witness | Newest outcome |
|---|---|---|---|
| 1 | app | `tests/e2e-unified/tests/test_mail_aliases.py::test_mail_aliases_page_reachable` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 1 | app | `tests/e2e-unified/tests/test_mail_aliases.py::test_mail_aliases_generate_disposable_renders_row` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 1 | app | `tests/e2e-unified/tests/test_mail_aliases.py::test_mail_aliases_add_exact_via_sheet` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 1 | app | `tests/e2e-unified/tests/test_mail_aliases.py::test_mail_aliases_revoke_then_delete` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 1 | app | `tests/e2e-unified/tests/test_mail_aliases.py::test_mail_aliases_revoke_and_delete_by_address_with_canonical_row_present` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 1 | app | `tests/e2e-unified/tests/test_mail_aliases_toggle.py::test_active_toggle_disable_then_reenable_round_trips` | web (linux): passed, linux (linux): passed, windows (windows): passed, tui (linux): passed |
| 2 | app | `tests/e2e-unified/tests/test_mail_aliases_import.py::test_import_addresses_reports_and_relists` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 3 | app | `tests/e2e-unified/tests/test_mail_aliases_canonical_readonly.py::test_canonical_alias_row_is_read_only` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 4 | app | `tests/e2e-unified/tests/test_mail_aliases_midsession_enable.py::test_aliases_page_is_usable_after_enabling_mail_midsession` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 5 | nest | `tests/e2e-unified/tests/api/test_mail_aliases_user.py::test_user_alias_crud_round_trip` | nest (linux): passed |
| 5 | nest | `tests/e2e-unified/tests/api/test_mail_aliases_user.py::test_wildcard_create_round_trip` | nest (linux): passed |
| 5 | nest | `tests/e2e-unified/tests/api/test_mail_aliases_user.py::test_disposable_mint_round_trip` | nest (linux): passed |
| 5 | nest | `tests/e2e-unified/tests/api/test_mail_aliases_user.py::test_duplicate_pattern_is_conflict_across_users` | nest (linux): passed |
| 5 | nest | `tests/e2e-unified/tests/api/test_alias_import.py::test_import_account_aliases_mixed_batch` | nest (linux): passed |
| 5 | nest | `tests/e2e-unified/tests/api/test_mail_aliases_user.py::test_cross_actor_isolation` | nest (linux): passed |
| 5 | nest | `tests/e2e-unified/tests/api/test_mail_aliases_user.py::test_wildcard_conflicts_with_other_users_exact` | nest (linux): passed |
| 6 | nest | `tests/e2e-unified/tests/test_mail_bridge_mta.py::test_inbound_over_per_alias_rate_cap_tempfails_451` | nest (linux): passed |
| 6 | nest | `tests/e2e-unified/tests/test_mail_spam_threshold_override.py::test_per_account_spam_threshold_override_rides_the_message_to_the_scorer` | nest (linux): passed, nest (windows): passed |
| 7 | app | `tests/e2e-unified/tests/test_mail_aliases_controls.py::test_add_wildcard_prefix_from_the_sheet` | macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 8 | app | `tests/e2e-unified/tests/test_mail_aliases_controls.py::test_add_sheet_sets_label_spam_threshold_and_hourly_limit` | macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 9 | app | `tests/e2e-unified/tests/test_mail_aliases_controls.py::test_row_shows_hit_count_and_last_hit_after_real_inbound` | macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 10 | app | `tests/e2e-unified/tests/test_mail_aliases_controls.py::test_edit_sheet_changes_label_and_controls_but_never_the_kind` | macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 11 | app | `tests/e2e-unified/tests/test_mail_aliases_controls.py::test_add_sheet_sets_label_spam_threshold_and_hourly_limit` | macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 11 | app | `tests/e2e-unified/tests/test_mail_aliases_controls.py::test_edit_sheet_changes_label_and_controls_but_never_the_kind` | macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 12 | app | `tests/e2e-unified/tests/test_mail_aliases_controls.py::test_a_throwaway_address_lasts_and_takes_what_you_chose` | tui (linux): passed |
| 13 | app | `tests/e2e-unified/tests/test_mail_aliases_controls.py::test_generated_disposable_lands_on_top_copied_and_confirmed` | macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 14 | app | (none) | — |
| 15 | app | (none) | — |
| 16 | app | (none) | — |
| 17 | app | `tests/e2e-unified/tests/test_mail_aliases_controls.py::test_reserved_name_is_refused_on_the_page` | macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 18 | nest | `tests/e2e-unified/tests/test_mail_bridge_alias_delivery.py::test_plus_suffix_reaches_the_owner_with_nothing_set_up` | nest (linux): passed |
| 19 | nest | `tests/e2e-unified/tests/test_mail_bridge_alias_delivery.py::test_wildcard_prefix_address_reaches_the_owner` | nest (linux): passed |
| 20 | nest | `tests/e2e-unified/tests/api/test_mail_aliases_user.py::test_wildcard_one_per_actor` | nest (linux): passed |
| 20 | nest | `tests/e2e-unified/tests/api/test_mail_aliases_user.py::test_wildcard_create_rejections` | nest (linux): passed |
| 21 | nest | `tests/e2e-unified/tests/test_mail_bridge_alias_delivery.py::test_disposable_refused_once_uses_or_lifetime_run_out` | nest (linux): passed |
| 22 | nest | `tests/e2e-unified/tests/test_mail_bridge_alias_delivery.py::test_disabled_alias_refused_and_kept` | nest (linux): passed |
| 23 | nest | `tests/e2e-unified/tests/test_mail_bridge_alias_delivery.py::test_deleted_alias_unknown_or_falls_to_catch_all` | nest (linux): passed |
| 24 | nest | `tests/e2e-unified/tests/api/test_mail_aliases_user.py::test_create_rejects_reserved_charclass_and_nonexact_kind` | nest (linux): passed |
| 25 | nest | `tests/e2e-unified/tests/api/test_mail_aliases_user.py::test_list_alias_hits_empty_for_owned_alias` | nest (linux): passed |
| 25 | nest | `tests/e2e-unified/tests/api/test_mail_aliases_user.py::test_list_alias_hits_owner_isolation` | nest (linux): passed |
| 26 | nest | `tests/e2e-unified/tests/test_mail_spam_threshold_override.py::test_changing_an_address_threshold_never_refiles_delivered_mail` | nest (linux): passed |
| 27 | nest | `tests/e2e-unified/tests/api/test_mail_alias_policy.py::test_exact_alias_cap_enforced_from_put_alias_policy` | nest (linux): passed |
| 28 | nest | `tests/e2e-unified/tests/api/test_mail_aliases_user.py::test_disposable_mints_capped_per_day` | nest (linux): passed |
| 29 | app | `tests/e2e-unified/tests/test_mail_aliases_controls.py::test_delete_confirm_relabels_the_button_before_deleting` | macos (macos): passed, ios (macos): passed, tui (linux): passed, tui (macos): passed |
<!-- features-render:end -->
