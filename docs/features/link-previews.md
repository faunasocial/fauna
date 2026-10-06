---
slug: link-previews
title: Link previews
section: everyday
goal: docs/goal/architecture/render-model.md § D4 — Link previews — a new async-resolved embed node
guide: docs/guides/app-tour.md § Feeds
---

## What a user gets

A bare link in a post or a message grows a preview card with the page's title
and description. Your nest fetches the page, not your device, so the site never sees
you, and the preview image stays off until you ask for it.

## Coverage contract

Stamped 2026-09-19 at 23b3189e2b.

1. [app] A post with a bare link shows a preview card, one per link — `docs/goal/architecture/render-model.md` § D4 — Link previews — a new async-resolved embed node
   - `tests/e2e-unified/tests/test_feed_link_preview.py::test_link_preview_card_paints_for_a_resolved_preview`
   - `tests/e2e-unified/tests/test_feed_link_preview.py::test_two_bare_urls_paint_a_card_each`
2. [app] A message with a bare link shows the same card — `docs/goal/architecture/render-model.md` § D4 — Link previews — a new async-resolved embed node
   - `tests/e2e-unified/tests/test_conversations_link_preview.py::test_conversations_link_preview_card_paints_and_reveals`
3. [nest] Your nest resolves the preview, refuses private addresses, and caches the result — `docs/goal/architecture/render-model.md` § D4 — Link previews — a new async-resolved embed node
   - `tests/e2e-unified/tests/api/test_link_preview.py::test_resolves_served_og_page_with_image`
   - `tests/e2e-unified/tests/api/test_link_preview.py::test_private_ip_url_fails`
   - `tests/e2e-unified/tests/api/test_link_preview.py::test_cache_hit_returns_first_result`
4. [app] A preview's picture stays hidden until you reveal the post's remote content, and one reveal shows it together with the body's images — `docs/goal/architecture/render-model.md` § D4 — Link previews — a new async-resolved embed node
   - `tests/e2e-unified/tests/test_feed_link_preview.py::test_link_preview_card_paints_for_a_resolved_preview`
   - `tests/e2e-unified/tests/test_conversations_link_preview.py::test_conversations_link_preview_card_paints_and_reveals`
5. [app] A link the nest cannot preview stays a plain link in the text, with no card — `docs/goal/architecture/render-model.md` § D4 — Link previews — a new async-resolved embed node
   - `tests/e2e-unified/tests/test_feed_link_preview_failed.py::test_a_link_the_nest_cannot_preview_stays_a_plain_link`

## Status

<!-- features-render:begin -->
| App | Status | Stamp |
|---|---|---|
| web | ✅ full | 0.1.2-dev+a1f84cc6 standalone |
| linux | ✅ full | 0.1.2-dev+a1f84cc6 standalone |
| windows | ✅ full | 0.1.2-dev+f5c0a21a.dirty standalone |
| macos | ✅ full | 0.1.2-dev+a1f84cc6 standalone |
| ios | ✅ full | 0.1.2-dev+a1f84cc6 standalone |
| android | ⚠ partial | 0.1.2-dev+a1f84cc6 standalone |
| tui | ✅ full | 0.1.2-dev+831d48ef live |

| Outcome | Surface | Witness | Newest outcome |
|---|---|---|---|
| 1 | app | `tests/e2e-unified/tests/test_feed_link_preview.py::test_link_preview_card_paints_for_a_resolved_preview` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 1 | app | `tests/e2e-unified/tests/test_feed_link_preview.py::test_two_bare_urls_paint_a_card_each` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 2 | app | `tests/e2e-unified/tests/test_conversations_link_preview.py::test_conversations_link_preview_card_paints_and_reveals` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 3 | nest | `tests/e2e-unified/tests/api/test_link_preview.py::test_resolves_served_og_page_with_image` | nest (linux): passed |
| 3 | nest | `tests/e2e-unified/tests/api/test_link_preview.py::test_private_ip_url_fails` | nest (linux): passed |
| 3 | nest | `tests/e2e-unified/tests/api/test_link_preview.py::test_cache_hit_returns_first_result` | nest (linux): passed |
| 4 | app | `tests/e2e-unified/tests/test_feed_link_preview.py::test_link_preview_card_paints_for_a_resolved_preview` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 4 | app | `tests/e2e-unified/tests/test_conversations_link_preview.py::test_conversations_link_preview_card_paints_and_reveals` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 5 | app | `tests/e2e-unified/tests/test_feed_link_preview_failed.py::test_a_link_the_nest_cannot_preview_stays_a_plain_link` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
<!-- features-render:end -->
