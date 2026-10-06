---
slug: subscriptions-and-tiers
title: Subscribe to creators; offer tiers of your own
section: money
goal: docs/goal/behavior/monetization.md § Pillar 1 — Fauna-app subscriptions (built)
guide: docs/guides/who-can-see-what.md § Paid posts — what is public and what is not
---

## What a user gets

Offer tiers from your profile and see who subscribes; subscribe to someone
else's from theirs, and their posts for that tier unlock for you. A follow unlocks by
itself; a paid tier is granted the moment the payment arrives, through a payment
provider you connect or a code you hand out by hand. Your active subscriptions are
listed in Settings, where you also unsubscribe.

## Coverage contract

Stamped 2026-10-01 at dfa27a899f.

1. [app] A subscription is granted and the subscriber reads the tier's posts, with the author doing nothing — `docs/goal/behavior/monetization.md` § Pillar 1 — Fauna-app subscriptions (built)
   - `tests/e2e-unified/tests/test_subscriptions.py::test_subscription_approve_grants_keyblob`
   - `tests/e2e-unified/tests/test_subscriptions.py::test_follow_auto_grants_in_encrypted_mode`
   - `tests/e2e-unified/tests/test_subscriptions.py::test_the_author_pumps_poll_backstop_grants_a_ui_subscribe_with_no_author_action`
   - `tests/e2e-unified/tests/test_subscriptions.py::test_ui_subscribe_publishes_pq_hybrid_encapsulation_key`
2. [app] Someone else's offers show on their profile and you subscribe from there — `docs/goal/behavior/monetization.md` § Pillar 1 — UX
   - `tests/e2e-unified/tests/test_profile.py::test_other_profile_offers_render_and_subscribe`
   - `tests/e2e-unified/tests/test_profile.py::test_activating_the_tiers_tab_rereads_the_offers_section`
3. [app] Your subscriptions are listed in Settings and you unsubscribe there — `docs/goal/behavior/monetization.md` § Pillar 1 — UX
   - `tests/e2e-unified/tests/test_subscriptions.py::test_subscription_settings_lists_active_subscription_and_unsubscribes`
4. [app] Connect a payment provider, see the address to register with it, and remove it; a paid subscriber redeems a code — `docs/goal/behavior/monetization.md` § Pillars 2+3 — app UX
   - `tests/e2e-unified/tests/test_subscription_payments.py::test_provider_config_add_and_remove`
   - `tests/e2e-unified/tests/test_subscription_payments.py::test_provider_webhook_url_preview`
   - `tests/e2e-unified/tests/test_subscription_payments.py::test_claim_redeem_via_ui`
   - `tests/e2e-unified/tests/test_subscription_payments.py::test_manual_claim_mint_and_list`
5. [nest] A signed payment grants the tier, an unmatched one becomes a code, a refund before approval demotes it, and forgeries are refused — `docs/goal/behavior/monetization.md` § Pillar 3
   - `tests/e2e-unified/tests/api/test_payments_webhook.py::test_signed_webhook_with_reference_enqueues_the_grant`
   - `tests/e2e-unified/tests/api/test_payments_webhook.py::test_unbound_payment_mints_claim_and_redeem_binds_the_actor`
   - `tests/e2e-unified/tests/api/test_payments_webhook.py::test_claims_mint_and_list_cover_the_manual_no_api_path`
   - `tests/e2e-unified/tests/api/test_payments_webhook.py::test_refund_before_approval_strips_the_payment_marker`
   - `tests/e2e-unified/tests/api/test_payments_webhook.py::test_bad_signature_and_unknown_provider_are_non_2xx`
6. [app] You create a tier from your own profile, giving it a name, a rank and a price — `docs/goal/behavior/monetization.md` § Pillar 1 — UX
   - `tests/e2e-unified/tests/test_subscriptions.py::test_subscription_approve_grants_keyblob`
7. [app] You change a tier you already offer: its rank, description, price, payment link and whether it grants by itself — `docs/goal/behavior/monetization.md` § Pillar 1 — UX
   - (none)
8. [app] You delete a tier you no longer offer — `docs/goal/behavior/monetization.md` § Pillar 1 — UX
   - (none)
9. [app] A request to subscribe to a tier you approve by hand waits for you, and approving it lets the subscriber in — `docs/goal/behavior/monetization.md` § Pillar 1 — UX
   - `tests/e2e-unified/tests/test_subscriptions.py::test_subscription_approve_grants_keyblob`
10. [app] You can turn a request to subscribe down — `docs/goal/behavior/monetization.md` § Pillar 1 — UX
   - (none)
11. [app] You see who subscribes to each of your tiers — `docs/goal/behavior/monetization.md` § Pillar 1 — UX
   - `tests/e2e-unified/tests/test_subscriptions.py::test_subscription_approve_grants_keyblob`
12. [app] You remove a subscriber from a tier, and what you post to that tier afterwards no longer opens for them — `docs/goal/behavior/monetization.md` § Pillar 1 — UX
   - (none)
13. [app] Subscribing to a higher tier also opens the posts of every lower tier, the author's follower posts included — `docs/goal/behavior/monetization.md` § The unifying model — tier as entitlement
   - (none)
14. [nest] A paid subscription ends when its paid period runs out, and a renewal payment extends it — `docs/goal/behavior/monetization.md` § Pillar 3
   - (none)
15. [nest] A refund or dispute on a subscription already granted ends the buyer's access — `docs/goal/behavior/monetization.md` § Pillar 3
   - (none)
16. [nest] A payment provider's secret, once saved, is never shown back to you or anyone else — `docs/goal/behavior/monetization.md` § Implementation status today
   - `tests/e2e-unified/tests/api/test_payments_webhook.py::test_providers_list_shows_config_without_secret`
17. [nest] A tier cannot be named "room": the name is kept for posts addressed to a room — `docs/goal/behavior/restricted-posts.md` § Encryption at rest
    - (none)

## Status

<!-- features-render:begin -->
| App | Status | Stamp |
|---|---|---|
| web | ⚠ partial | 0.1.2-dev+a1f84cc6 standalone |
| linux | ⚠ partial | 0.1.2-dev+a1f84cc6 standalone |
| windows | ⚠ partial | 0.1.2-dev+068e152c.dirty standalone |
| macos | ⚠ partial | 0.1.2-dev+a1f84cc6 standalone |
| ios | ⚠ partial | 0.1.2-dev+a1f84cc6 standalone |
| android |  no run recorded | |
| tui | ⚠ partial | 0.1.2-dev+7663afa6 standalone |

| Outcome | Surface | Witness | Newest outcome |
|---|---|---|---|
| 1 | app | `tests/e2e-unified/tests/test_subscriptions.py::test_subscription_approve_grants_keyblob` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 1 | app | `tests/e2e-unified/tests/test_subscriptions.py::test_follow_auto_grants_in_encrypted_mode` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 1 | app | `tests/e2e-unified/tests/test_subscriptions.py::test_the_author_pumps_poll_backstop_grants_a_ui_subscribe_with_no_author_action` | web (linux): skipped, linux (linux): passed, windows (windows): skipped, macos (macos): error, ios (macos): error, tui (linux): passed |
| 1 | app | `tests/e2e-unified/tests/test_subscriptions.py::test_ui_subscribe_publishes_pq_hybrid_encapsulation_key` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 2 | app | `tests/e2e-unified/tests/test_profile.py::test_other_profile_offers_render_and_subscribe` | linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 2 | app | `tests/e2e-unified/tests/test_profile.py::test_activating_the_tiers_tab_rereads_the_offers_section` | linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 3 | app | `tests/e2e-unified/tests/test_subscriptions.py::test_subscription_settings_lists_active_subscription_and_unsubscribes` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 4 | app | `tests/e2e-unified/tests/test_subscription_payments.py::test_provider_config_add_and_remove` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 4 | app | `tests/e2e-unified/tests/test_subscription_payments.py::test_provider_webhook_url_preview` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 4 | app | `tests/e2e-unified/tests/test_subscription_payments.py::test_claim_redeem_via_ui` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 4 | app | `tests/e2e-unified/tests/test_subscription_payments.py::test_manual_claim_mint_and_list` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 5 | nest | `tests/e2e-unified/tests/api/test_payments_webhook.py::test_signed_webhook_with_reference_enqueues_the_grant` | nest (linux): passed |
| 5 | nest | `tests/e2e-unified/tests/api/test_payments_webhook.py::test_unbound_payment_mints_claim_and_redeem_binds_the_actor` | nest (linux): passed |
| 5 | nest | `tests/e2e-unified/tests/api/test_payments_webhook.py::test_claims_mint_and_list_cover_the_manual_no_api_path` | nest (linux): passed |
| 5 | nest | `tests/e2e-unified/tests/api/test_payments_webhook.py::test_refund_before_approval_strips_the_payment_marker` | nest (linux): passed |
| 5 | nest | `tests/e2e-unified/tests/api/test_payments_webhook.py::test_bad_signature_and_unknown_provider_are_non_2xx` | nest (linux): passed |
| 6 | app | `tests/e2e-unified/tests/test_subscriptions.py::test_subscription_approve_grants_keyblob` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 7 | app | (none) | — |
| 8 | app | (none) | — |
| 9 | app | `tests/e2e-unified/tests/test_subscriptions.py::test_subscription_approve_grants_keyblob` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 10 | app | (none) | — |
| 11 | app | `tests/e2e-unified/tests/test_subscriptions.py::test_subscription_approve_grants_keyblob` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 12 | app | (none) | — |
| 13 | app | (none) | — |
| 14 | nest | (none) | — |
| 15 | nest | (none) | — |
| 16 | nest | `tests/e2e-unified/tests/api/test_payments_webhook.py::test_providers_list_shows_config_without_secret` | nest (linux): passed |
| 17 | nest | (none) | — |
<!-- features-render:end -->
