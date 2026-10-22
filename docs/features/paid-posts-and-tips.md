---
slug: paid-posts-and-tips
title: Gated posts, sold posts and tips
section: money
goal: docs/goal/behavior/monetization.md § Per-post pay-to-unlock
guide: docs/guides/who-can-see-what.md § Paid posts — what is public and what is not
---

## What a user gets

Gate a post to a tier as you write it, or sell a single post for a price shown
on its teaser; a buyer unlocks it, and an existing subscriber reads it free. A folder
on the web can be paywalled the same way. Tips on a post show their total and who
gave them.

## Coverage contract

Stamped 2026-10-01 at f5dec3d933.

1. [app] A post gated to a tier unlocks for a subscriber — `docs/goal/behavior/monetization.md` § Pillars 2+3 — app UX
   - `tests/e2e-unified/tests/test_gated_post_compose.py::test_gate_to_tier_compose_and_subscriber_unlock`
2. [app] A photo attached to an audience-restricted post — gated to a tier, or sold on its own — is sealed under that post's own key, and no readable copy of it is ever uploaded — `docs/goal/ui/media.md` § Encryption at rest
   - `tests/e2e-unified/tests/test_gated_post_compose.py::test_gated_post_attachment_is_sealed_and_never_uploaded_in_plaintext`
   - `tests/e2e-unified/tests/test_sell_post.py::test_sold_post_attachment_is_sealed_and_never_uploaded_in_plaintext`
3. [app] Selling a post gates it at a price, a buyer sees the price and unlocks it, and an existing subscriber reads it free — `docs/goal/behavior/monetization.md` § Per-post pay-to-unlock
   - `tests/e2e-unified/tests/test_sell_post.py::test_sell_post_mints_an_unlock_tier_and_gates_the_post`
   - `tests/e2e-unified/tests/test_sell_post.py::test_sell_post_buyer_redeems_claim_and_unseals`
   - `tests/e2e-unified/tests/test_sell_post.py::test_sell_post_teaser_price_and_self_serve_buy`
   - `tests/e2e-unified/tests/test_sell_post.py::test_sell_post_existing_subscriber_reads_it_free`
4. [app] Tips on a post show their total and count, and who gave them — `docs/goal/behavior/monetization.md` § Tips
   - `tests/e2e-unified/tests/test_feed_tips.py::test_a_tipped_post_shows_its_total_and_count`
   - `tests/e2e-unified/tests/test_feed_tips.py::test_tips_with_no_readable_amount_show_the_count_and_no_total`
   - `tests/e2e-unified/tests/test_feed_tips.py::test_the_attribution_window_names_every_tipper_it_was_given`
5. [app] A web folder is paywalled to one of your tiers from its row — `docs/goal/behavior/monetization.md` § Pillar 2
   - `tests/e2e-unified/tests/test_folder_paywall.py::test_folder_paywall_select_paywalls_a_website_set`
6. [nest] A paywalled post or folder serves a teaser to visitors, the full content to a token holder, and nothing after a revoke or expiry — `docs/goal/behavior/monetization.md` § Pillar 2
   - `tests/e2e-unified/tests/api/test_web_paywall.py::test_web_paywall_teaser_token_and_revoke`
   - `tests/e2e-unified/tests/api/test_web_paywall_folder.py::test_web_paywall_folder_teaser_token_and_revoke`
   - `tests/e2e-unified/tests/api/test_web_paywall_folder.py::test_web_paywall_folder_expired_token_serves_teaser`
   - `tests/e2e-unified/tests/api/test_web_paywall_folder.py::test_web_paywall_folder_rotation_renews_the_grant`
7. [app] Someone not subscribed to a post's tier sees only its teaser and a badge naming the tier, never the post itself — `docs/goal/behavior/restricted-posts.md` § Encryption at rest
   - `tests/e2e-unified/tests/test_paid_post_offers.py::test_a_non_subscriber_sees_only_the_teaser_and_the_tier_badge`
8. [app] You can sell a post so that even your existing subscribers have to buy it — `docs/goal/behavior/monetization.md` § Per-post pay-to-unlock
   - `tests/e2e-unified/tests/test_paid_post_offers.py::test_a_post_sold_to_everyone_leaves_existing_subscribers_out`
9. [app] Buying a post that the author's subscribers read free also makes you a follower of the author — `docs/goal/behavior/monetization.md` § Per-post pay-to-unlock
   - `tests/e2e-unified/tests/test_paid_post_offers.py::test_buying_a_subscribers_free_post_makes_you_a_follower`
10. [app] A post you sell never appears in your own list of tiers — `docs/goal/behavior/monetization.md` § Per-post pay-to-unlock
   - `tests/e2e-unified/tests/test_sell_post.py::test_sell_post_mints_an_unlock_tier_and_gates_the_post`
11. [app] A post you sell is never offered on your profile as a tier to subscribe to — `docs/goal/behavior/monetization.md` § Per-post pay-to-unlock
   - `tests/e2e-unified/tests/test_paid_post_offers.py::test_a_sold_post_is_never_offered_as_a_tier_or_an_audience`
12. [app] A post you sell is never offered as an audience when you write a new post — `docs/goal/behavior/monetization.md` § Per-post pay-to-unlock
   - `tests/e2e-unified/tests/test_paid_post_offers.py::test_a_sold_post_is_never_offered_as_a_tier_or_an_audience`
13. [app] You are never offered a purchase of a post you sold yourself — `docs/goal/behavior/monetization.md` § Per-post pay-to-unlock
   - `tests/e2e-unified/tests/test_paid_post_offers.py::test_you_are_never_offered_your_own_sold_post`
14. [app] A post you have bought is not offered to you again while the author's approval is still on its way — `docs/goal/behavior/monetization.md` § Per-post pay-to-unlock
   - `tests/e2e-unified/tests/test_paid_post_offers.py::test_a_bought_post_is_not_offered_again_while_the_approval_is_on_its_way`
15. [nest] A post you buy stays unlocked for you with no end date — `docs/goal/behavior/monetization.md` § Per-post pay-to-unlock
   - `tests/e2e-unified/tests/api/test_payments_webhook.py::test_a_bought_post_stays_unlocked_with_no_end_date`
16. [app] With no tier of your own, a folder's paywall control is unavailable and tells you to create a tier first — `docs/goal/ui/folders.md` § Element IDs
    - `tests/e2e-unified/tests/test_folder_paywall.py::test_folder_paywall_select_is_unavailable_without_a_tier`
17. [nest] A visitor with no Fauna app pays, or enters a code, on a paywalled page's teaser and then reads the full content — `docs/goal/behavior/monetization.md` § Pillar 2
   - `tests/e2e-unified/tests/api/test_web_paywall.py::test_anonymous_visitor_pays_or_redeems_on_the_teaser`
18. [app] When someone tips your post, you get a notification — `docs/goal/behavior/monetization.md` § Tips
   - `tests/e2e-unified/tests/test_tip_notification.py::test_a_tip_on_your_post_arrives_as_a_notification`
19. [nest] A zap counts as a tip on your post only when a signer you named reports it; with nobody named, none counts — `docs/goal/behavior/monetization.md` § Zap receipts — the trust model
   - `tests/e2e-unified/tests/api/test_zap_trust_gate.py::test_with_no_signer_named_no_zap_counts`
   - `tests/e2e-unified/tests/api/test_zap_trust_gate.py::test_a_zap_counts_only_from_the_signer_the_payee_named`
20. [nest] A zap signer you named cannot credit a tip to a post that is not yours — `docs/goal/behavior/monetization.md` § Zap receipts — the trust model
   - `tests/e2e-unified/tests/api/test_zap_trust_gate.py::test_a_named_signer_cannot_credit_a_tip_to_another_users_post`

## Status

<!-- features-render:begin -->
| App | Status | Stamp |
|---|---|---|
| web | ⚠ partial | 0.1.5-dev+110d1248 standalone |
| linux | ⚠ partial | 0.1.4-dev+ea391d07 standalone |
| windows | ⚠ partial | 0.1.5-dev+61b844ca standalone |
| macos | ⚠ partial | 0.1.5-dev+61b844ca standalone |
| ios | ⚠ partial | 0.1.5-dev+61b844ca standalone |
| android |  no run recorded | |
| tui | ✅ full | 0.1.4-dev+df282566 standalone |

| Outcome | Surface | Witness | Newest outcome |
|---|---|---|---|
| 1 | app | `tests/e2e-unified/tests/test_gated_post_compose.py::test_gate_to_tier_compose_and_subscriber_unlock` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 2 | app | `tests/e2e-unified/tests/test_gated_post_compose.py::test_gated_post_attachment_is_sealed_and_never_uploaded_in_plaintext` | web (linux): passed, web (windows): passed, linux (linux): passed, windows (windows): skipped, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 2 | app | `tests/e2e-unified/tests/test_sell_post.py::test_sold_post_attachment_is_sealed_and_never_uploaded_in_plaintext` | web (linux): passed, web (windows): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed, tui (windows): passed |
| 3 | app | `tests/e2e-unified/tests/test_sell_post.py::test_sell_post_mints_an_unlock_tier_and_gates_the_post` | web (linux): passed, web (windows): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed, tui (windows): passed |
| 3 | app | `tests/e2e-unified/tests/test_sell_post.py::test_sell_post_buyer_redeems_claim_and_unseals` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 3 | app | `tests/e2e-unified/tests/test_sell_post.py::test_sell_post_teaser_price_and_self_serve_buy` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 3 | app | `tests/e2e-unified/tests/test_sell_post.py::test_sell_post_existing_subscriber_reads_it_free` | web (linux): failed, linux (linux): passed, windows (windows): passed, macos (macos): failed, ios (macos): failed, tui (linux): passed |
| 4 | app | `tests/e2e-unified/tests/test_feed_tips.py::test_a_tipped_post_shows_its_total_and_count` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 4 | app | `tests/e2e-unified/tests/test_feed_tips.py::test_tips_with_no_readable_amount_show_the_count_and_no_total` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 4 | app | `tests/e2e-unified/tests/test_feed_tips.py::test_the_attribution_window_names_every_tipper_it_was_given` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 5 | app | `tests/e2e-unified/tests/test_folder_paywall.py::test_folder_paywall_select_paywalls_a_website_set` | web (linux): passed, linux (linux): passed, windows (windows): passed, tui (linux): passed |
| 6 | nest | `tests/e2e-unified/tests/api/test_web_paywall.py::test_web_paywall_teaser_token_and_revoke` | nest (linux): passed |
| 6 | nest | `tests/e2e-unified/tests/api/test_web_paywall_folder.py::test_web_paywall_folder_teaser_token_and_revoke` | nest (linux): passed |
| 6 | nest | `tests/e2e-unified/tests/api/test_web_paywall_folder.py::test_web_paywall_folder_expired_token_serves_teaser` | nest (linux): passed |
| 6 | nest | `tests/e2e-unified/tests/api/test_web_paywall_folder.py::test_web_paywall_folder_rotation_renews_the_grant` | nest (linux): passed |
| 7 | app | `tests/e2e-unified/tests/test_paid_post_offers.py::test_a_non_subscriber_sees_only_the_teaser_and_the_tier_badge` | web (linux): passed, linux (linux): passed, tui (linux): passed |
| 8 | app | `tests/e2e-unified/tests/test_paid_post_offers.py::test_a_post_sold_to_everyone_leaves_existing_subscribers_out` | web (linux): passed, linux (linux): passed, tui (linux): passed |
| 9 | app | `tests/e2e-unified/tests/test_paid_post_offers.py::test_buying_a_subscribers_free_post_makes_you_a_follower` | web (linux): passed, linux (linux): passed, tui (linux): passed |
| 10 | app | `tests/e2e-unified/tests/test_sell_post.py::test_sell_post_mints_an_unlock_tier_and_gates_the_post` | web (linux): passed, web (windows): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed, tui (windows): passed |
| 11 | app | `tests/e2e-unified/tests/test_paid_post_offers.py::test_a_sold_post_is_never_offered_as_a_tier_or_an_audience` | web (linux): passed, linux (linux): passed, tui (linux): passed |
| 12 | app | `tests/e2e-unified/tests/test_paid_post_offers.py::test_a_sold_post_is_never_offered_as_a_tier_or_an_audience` | web (linux): passed, linux (linux): passed, tui (linux): passed |
| 13 | app | `tests/e2e-unified/tests/test_paid_post_offers.py::test_you_are_never_offered_your_own_sold_post` | web (linux): passed, linux (linux): passed, tui (linux): passed |
| 14 | app | `tests/e2e-unified/tests/test_paid_post_offers.py::test_a_bought_post_is_not_offered_again_while_the_approval_is_on_its_way` | web (linux): passed, linux (linux): passed, tui (linux): passed |
| 15 | nest | `tests/e2e-unified/tests/api/test_payments_webhook.py::test_a_bought_post_stays_unlocked_with_no_end_date` | nest (linux): passed |
| 16 | app | `tests/e2e-unified/tests/test_folder_paywall.py::test_folder_paywall_select_is_unavailable_without_a_tier` | web (linux): passed, linux (linux): passed, tui (linux): passed |
| 17 | nest | `tests/e2e-unified/tests/api/test_web_paywall.py::test_anonymous_visitor_pays_or_redeems_on_the_teaser` | nest (linux): passed |
| 18 | app | `tests/e2e-unified/tests/test_tip_notification.py::test_a_tip_on_your_post_arrives_as_a_notification` | web (linux): passed, linux (linux): failed, tui (linux): passed |
| 19 | nest | `tests/e2e-unified/tests/api/test_zap_trust_gate.py::test_with_no_signer_named_no_zap_counts` | nest (linux): passed |
| 19 | nest | `tests/e2e-unified/tests/api/test_zap_trust_gate.py::test_a_zap_counts_only_from_the_signer_the_payee_named` | nest (linux): passed |
| 20 | nest | `tests/e2e-unified/tests/api/test_zap_trust_gate.py::test_a_named_signer_cannot_credit_a_tip_to_another_users_post` | nest (linux): passed |
<!-- features-render:end -->
