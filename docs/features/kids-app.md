---
slug: kids-app
title: Fauna Kids
section: family and personalization
goal: docs/goal/behavior/account-age-band.md § The account age band
guide: docs/guides/cloud-sharing-family.md § A family on one nest
kids-owed: android-kids, ios-kids
absences:
  web: "docs/goal/behavior/account-age-band.md § The account age band"
  linux: "docs/goal/behavior/account-age-band.md § The account age band"
  windows: "docs/goal/behavior/account-age-band.md § The account age band"
  macos: "docs/goal/behavior/account-age-band.md § The account age band"
  tui: "docs/goal/behavior/account-age-band.md § The account age band"
  android: "docs/goal/behavior/account-age-band.md § The account age band"
  ios: "docs/goal/behavior/account-age-band.md § The account age band"
---

## What a user gets

Fauna Kids is a planned app for a child whose account a guardian supervises; it
is not yet available anywhere. It is the Fauna app for Android and iOS with the
open parts left out of the build itself: no feed, no searching for people you do
not know, no bridges to other networks, no publishing to the web, no payments
and no outside apps. A link in a message opens nothing, pictures from elsewhere
are never shown, another person's profile opens only once they are a confirmed
contact, and the four content categories a guardian can block stay blocked
whatever anyone sets. A child joins the family's nest by invite. An account
nobody supervises is told so and can only sign out.

## Coverage contract

Stamped 2026-11-08 at 485892afca.

1. [app] The app installs under its own store listing as Fauna Kids, and only an account a guardian supervises gets past sign-in: any other account is shown why and can only sign out — `docs/goal/behavior/account-age-band.md` § The account age band
   - `tests/e2e-unified/tests/test_kids_app.py::test_an_unsupervised_account_is_told_why_and_can_only_sign_out`
2. [app] The app has conversations, contacts, folders, calendar, family and settings and nothing else: no feed, no search, no bridges, no web publishing, no payments, no connected apps — `docs/goal/behavior/account-age-band.md` § The account age band
   - `tests/e2e-unified/tests/test_kids_app.py::test_the_app_keeps_conversations_contacts_folders_calendar_family_and_settings_only`
3. [app] A link in a message or a notification is plain text, shows no preview card and opens nothing when tapped — `docs/goal/behavior/account-age-band.md` § The account age band
   - `tests/e2e-unified/tests/test_kids_app.py::test_a_link_in_a_message_is_plain_text_with_no_preview_card`
4. [app] Another person's profile opens only from a confirmed contact, and shows no Follow button and no Tiers tab — `docs/goal/behavior/account-age-band.md` § The account age band
   - `tests/e2e-unified/tests/test_kids_app.py::test_a_profile_opens_only_from_a_confirmed_contact_with_no_follow_or_tiers`
5. [app] Pictures a message links to from elsewhere stay placeholders, with no button to fetch them, and nothing is requested from an outside site — `docs/goal/behavior/account-age-band.md` § The account age band
   - `tests/e2e-unified/tests/test_kids_app.py::test_a_remote_picture_stays_a_placeholder_with_no_way_to_fetch_it`
6. [app] Signing in never offers to set up a nest or to retire one; a child joins their guardian's nest by invite — `docs/goal/behavior/account-age-band.md` § The account age band
   - `tests/e2e-unified/tests/test_kids_app.py::test_signing_in_never_offers_to_set_up_a_nest`
7. [app] The four content categories a guardian can block stay blocked whatever the guardian set, and the guardian's own choices still add to that — `docs/goal/architecture/dynamic-features.md` § Compile-time excision
   - `tests/e2e-unified/tests/test_kids_app.py::test_the_four_content_categories_stay_blocked_whatever_the_guardian_set`

## Status

<!-- features-render:begin -->
| App | Status | Stamp |
|---|---|---|
| web | — absent | |
| linux | — absent | |
| windows | — absent | |
| macos | — absent | |
| ios | — absent | |
| android | — absent | |
| tui | — absent | |
| android-kids |  no run recorded | |
| ios-kids | ✅ full | 0.1.6-dev+58a0595f standalone |

| Outcome | Surface | Witness | Newest outcome |
|---|---|---|---|
| 1 | app | `tests/e2e-unified/tests/test_kids_app.py::test_an_unsupervised_account_is_told_why_and_can_only_sign_out` | ios (macos): skipped, ios-kids (macos): passed |
| 2 | app | `tests/e2e-unified/tests/test_kids_app.py::test_the_app_keeps_conversations_contacts_folders_calendar_family_and_settings_only` | ios (macos): skipped, ios-kids (macos): passed |
| 3 | app | `tests/e2e-unified/tests/test_kids_app.py::test_a_link_in_a_message_is_plain_text_with_no_preview_card` | ios (macos): skipped, ios-kids (macos): passed |
| 4 | app | `tests/e2e-unified/tests/test_kids_app.py::test_a_profile_opens_only_from_a_confirmed_contact_with_no_follow_or_tiers` | ios (macos): skipped, ios-kids (macos): passed |
| 5 | app | `tests/e2e-unified/tests/test_kids_app.py::test_a_remote_picture_stays_a_placeholder_with_no_way_to_fetch_it` | ios (macos): skipped, ios-kids (macos): passed |
| 6 | app | `tests/e2e-unified/tests/test_kids_app.py::test_signing_in_never_offers_to_set_up_a_nest` | ios (macos): skipped, ios-kids (macos): passed |
| 7 | app | `tests/e2e-unified/tests/test_kids_app.py::test_the_four_content_categories_stay_blocked_whatever_the_guardian_set` | ios (macos): skipped, ios-kids (macos): passed |
<!-- features-render:end -->
