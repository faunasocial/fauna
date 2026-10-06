---
slug: post-badges
title: Know where a post came from
section: everyday
goal: docs/goal/architecture/security.md § App display of unverified content
guide: docs/guides/who-can-see-what.md § Public posts and your profile
---

## What a user gets

Small badges on a post tell you what you are looking at: which network it came
through, whether your app could verify who signed it, whether it was written through
a connected third-party app, and any content label attached to it.

## Coverage contract

Stamped 2026-09-19 at 3ac2cf0c74.

1. [app] A post shows one badge per network it arrived through — `docs/goal/architecture/render-model.md` § D5 — Rail-icon semantic token (`SourceGlyph`)
   - `tests/e2e-unified/tests/test_feed_protocol_badge.py::test_protocol_badge_count_matches_classified_sources`
2. [app] A post your app could not verify is marked, and only that post, quoted posts included — `docs/goal/architecture/security.md` § App display of unverified content
   - `tests/e2e-unified/tests/test_feed_unverified_source.py::test_unverified_source_badge_shows_only_on_failed`
   - `tests/e2e-unified/tests/test_feed_unverified_source.py::test_quoted_embed_badge_shows_only_on_failed_quote`
3. [app] A post written through a connected third-party app says so — `docs/goal/behavior/atproto-pds-full.md` § D10
   - `tests/e2e-unified/tests/test_feed_delegated_origin.py::test_delegated_origin_badge_shows_only_on_a_delegated_post`
   - `tests/e2e-unified/tests/test_feed_delegated_origin.py::test_a_post_that_failed_verification_is_never_badged_as_delegated`
   - `tests/e2e-unified/tests/test_feed_delegated_origin.py::test_quoted_embed_badge_shows_only_on_a_delegated_quote`
4. [app] A post carrying a content label shows it — `docs/goal/behavior/moderation.md` § Per-row badge data path
   - `tests/e2e-unified/tests/test_feed_content_label_badge.py::test_content_label_badge_shows_only_on_labeled_post`
5. [nest] Your nest serves each post's labels with the post — `docs/goal/behavior/moderation.md` § Per-row badge data path
   - `tests/e2e-unified/tests/api/test_feed_content_labels.py::test_attached_label_is_served_per_row_on_feed_reads`
6. [nest] A label on your post can only come from you or from an app you gave that permission — anyone else's is refused — `docs/goal/behavior/moderation.md` § Per-row badge data path
   - `tests/e2e-unified/tests/api/test_content_moderation.py::test_spam_filtering_pipeline`
7. [app] A message in a conversation carrying a content label shows it — `docs/goal/behavior/moderation.md` § Per-row badge data path
   - `tests/e2e-unified/tests/test_conversations_content_label_badge.py::test_a_labeled_message_shows_its_content_label`

## Status

<!-- features-render:begin -->
| App | Status | Stamp |
|---|---|---|
| web | ✅ full | 0.1.2-dev+a1f84cc6 standalone |
| linux | ✅ full | 0.1.2-dev+a1f84cc6 standalone |
| windows | ✅ full | 0.1.2-dev+f5c0a21a.dirty standalone |
| macos | ✅ full | 0.1.2-dev+a1f84cc6 standalone |
| ios | ✅ full | 0.1.2-dev+a1f84cc6 standalone |
| android |  no run recorded | |
| tui | ✅ full | 0.1.2-dev+831d48ef live |

| Outcome | Surface | Witness | Newest outcome |
|---|---|---|---|
| 1 | app | `tests/e2e-unified/tests/test_feed_protocol_badge.py::test_protocol_badge_count_matches_classified_sources` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 2 | app | `tests/e2e-unified/tests/test_feed_unverified_source.py::test_unverified_source_badge_shows_only_on_failed` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 2 | app | `tests/e2e-unified/tests/test_feed_unverified_source.py::test_quoted_embed_badge_shows_only_on_failed_quote` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 3 | app | `tests/e2e-unified/tests/test_feed_delegated_origin.py::test_delegated_origin_badge_shows_only_on_a_delegated_post` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 3 | app | `tests/e2e-unified/tests/test_feed_delegated_origin.py::test_a_post_that_failed_verification_is_never_badged_as_delegated` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 3 | app | `tests/e2e-unified/tests/test_feed_delegated_origin.py::test_quoted_embed_badge_shows_only_on_a_delegated_quote` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 4 | app | `tests/e2e-unified/tests/test_feed_content_label_badge.py::test_content_label_badge_shows_only_on_labeled_post` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 5 | nest | `tests/e2e-unified/tests/api/test_feed_content_labels.py::test_attached_label_is_served_per_row_on_feed_reads` | nest (linux): passed |
| 6 | nest | `tests/e2e-unified/tests/api/test_content_moderation.py::test_spam_filtering_pipeline` | nest (linux): passed |
| 7 | app | `tests/e2e-unified/tests/test_conversations_content_label_badge.py::test_a_labeled_message_shows_its_content_label` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
<!-- features-render:end -->
