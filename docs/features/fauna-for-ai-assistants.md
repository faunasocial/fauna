---
slug: fauna-for-ai-assistants
title: fauna-for-ai-assistants
section: your data and devices
goal: docs/goal/architecture/apps/cli.md § Goal
guide: docs/guides/connect-an-ai-assistant.md § How it works
absences:
  android-kids: "docs/goal/behavior/account-age-band.md § The account age band"
  ios-kids: "docs/goal/behavior/account-age-band.md § The account age band"
---

## What a user gets

This text is being written and will appear here once it has been reviewed.

## Coverage contract

Stamped 2026-11-07 at 69a49ad33d.

1. [nest] An assistant on your computer operates Fauna through the command you install: its host launches the command and talks to it directly, or over an address on your own machine that the command prints once and that nothing beyond your machine can reach — `docs/goal/architecture/apps/cli.md` § The device shell
   - `tests/e2e-unified/tests/test_cli_mcp.py::test_an_llm_host_connects_the_cli_reads_the_feed_and_posts_as_the_user`
2. [nest] Another program or page on your machine cannot borrow that local address: a request from a foreign origin or host is refused before any tool runs — `docs/goal/architecture/apps/cli.md` § Implementation status today
   - `tests/e2e-unified/tests/test_cli_mcp.py::test_the_http_listener_refuses_a_foreign_origin_and_host`
3. [nest] Connecting it asks you for your handle, never a password or a key: the assistant shows you a short code, you type it into Connect an app on any of your Fauna apps, and the connection then rests on your computer and resumes in later sessions without a new approval — `docs/goal/architecture/apps/cli.md` § The device shell
   - `tests/e2e-unified/tests/test_cli_mcp.py::test_an_llm_host_connects_the_cli_reads_the_feed_and_posts_as_the_user`
4. [nest] Until you approve, every tool answers that it is waiting for you, and once you approve the assistant reaches only what its card listed: a tool for anything else is refused — `docs/goal/architecture/apps/cli.md` § The principal shape
   - `tests/e2e-unified/tests/test_cli_mcp.py::test_an_llm_host_connects_the_cli_reads_the_feed_and_posts_as_the_user`
5. [nest] It reads your feeds, the public posts on your nest and what is trending there, posts publicly as you and takes back its own posts, and a post it made shows on your nest's local timeline — `docs/goal/architecture/apps/cli.md` § The feature ledger
   - `tests/e2e-unified/tests/test_cli_mcp.py::test_an_llm_host_connects_the_cli_reads_the_feed_and_posts_as_the_user`
   - `tests/e2e-unified/tests/test_cli_mcp.py::test_the_feed_tools_read_one_feeds_definition_and_its_posts`
6. [nest] It reads your profile and the public profile of anyone your nest knows, and edits your display name, bio and links as you — `docs/goal/architecture/apps/cli.md` § The feature ledger
   - `tests/e2e-unified/tests/test_cli_mcp.py::test_an_llm_host_connects_the_cli_reads_the_feed_and_posts_as_the_user`
7. [nest] It reads your notifications with the unread count and marks them read, and can neither dismiss nor clear them — `docs/goal/architecture/apps/cli.md` § The feature ledger
   - `tests/e2e-unified/tests/test_cli_mcp.py::test_an_llm_host_connects_the_cli_reads_the_feed_and_posts_as_the_user`
8. [app] It reads and sends in your conversations only through your Fauna app open on the same computer: it lists your rooms, reads and searches their messages and sends a message as you into one, never holds a key to any room, and the moment you revoke it its next read is refused — `docs/goal/behavior/conversations-oracle.md` § The custodian and the door
   - `tests/e2e-unified/tests/test_cli_conversations.py::test_the_cli_reads_and_sends_through_tui_as_custodian_until_revoked`
9. [nest] You can connect more than one account: each is its own connection with its own card, a tool call names which account it is for, and a relaunch resumes them all — `docs/goal/architecture/apps/cli.md` § The device shell
   - `tests/e2e-unified/tests/test_cli_mcp.py::test_two_accounts_are_two_principals_and_a_call_names_which`
10. [nest] When your nest restarts or the network drops, the assistant reconnects on its own: a read is repeated for it, a write never is, and its answer says so — `docs/goal/architecture/apps/cli.md` § The device shell
    - `tests/e2e-unified/tests/test_cli_mcp.py::test_a_dropped_session_is_redialled_a_read_runs_again_and_a_write_never_does`
11. [nest] On a computer with no usable secret store the connection rests in a file only your own user account can read, and the assistant's first answer says so and that Connected apps revokes it in one row — `docs/goal/architecture/apps/cli.md` § The device shell
    - `tests/e2e-unified/tests/test_cli_mcp.py::test_with_no_os_store_the_connection_rests_in_a_declared_0600_file_and_resumes`
12. [nest] The same commands work for you at the terminal, on the same account under the same approval: every tool is a command and every argument a flag, the output is readable or JSON for a script, and a command that deletes something asks first or needs `--yes` — `docs/goal/architecture/apps/cli.md` § One catalog, many front-ends
    - `tests/e2e-unified/tests/test_cli_mcp.py::test_the_argv_shell_is_the_same_principal_at_the_terminal`
13. [nest] An admin who connects it can read the nest's dashboard and do the daily work of running a nest — members, invite codes and requests, tiers and quotas, settings and services, logs and the audit record, reports and blocks, domains and DNS, mail settings and reports down to a report's raw archive, forwarders, the calendar, contacts and files switches, and the list of pending bridges — while a member's approval of an admin permission grants nothing — `docs/goal/architecture/apps/cli.md` § The feature ledger
    - `tests/e2e-unified/tests/test_cli_mcp.py::test_the_admin_plane_serves_an_admin_consented_principal_and_no_member`
    - `tests/e2e-unified/tests/test_cli_mcp.py::test_admission_admits_a_supervised_member_under_a_guardian_and_an_age_band`
    - `tests/e2e-unified/tests/test_cli_mcp.py::test_the_admin_reads_one_dmarc_reports_raw_archive`
14. [nest] Nothing that widens authority or touches a key or an identity reaches it: it cannot add or remove an admin, approve a bridge, mint, rotate or reveal a key, certificate or secret, rename the nest's domain, clear a handle, delete a user or reset the nest; and your own identity, recovery kit, devices, sessions, nests, backups, the Connected apps page and deleting your account stay on your Fauna apps — `docs/goal/architecture/apps/cli.md` § The sovereign-act boundary
    - `tests/e2e-unified/tests/test_cli_mcp.py::test_the_admin_plane_serves_an_admin_consented_principal_and_no_member`
    - `tests/e2e-unified/tests/test_cli_mcp.py::test_the_catalog_names_no_tool_for_a_sovereign_act`
15. [nest] Installing the terminal app on a computer puts the assistant's command beside it, ready to run — `docs/goal/architecture/installers/tui.md` § Decisions
    - `tests/e2e-unified/tests/artifact/test_tui_installed_product.py::test_the_install_puts_the_cli_app_beside_the_terminal_app_and_it_runs`

## Status

<!-- features-render:begin -->
| App | Status | Stamp |
|---|---|---|
| web | ⚠ partial | |
| linux | ✅ full | 0.1.6-dev+3aeb19b0 standalone |
| windows |  no run recorded | |
| macos |  no run recorded | |
| ios | ⚠ partial | |
| android | ⚠ partial | |
| tui | ✅ full | 0.1.6-dev+64373b38 standalone |
| android-kids | — absent | |
| ios-kids | — absent | |

| Outcome | Surface | Witness | Newest outcome |
|---|---|---|---|
| 1 | nest | `tests/e2e-unified/tests/test_cli_mcp.py::test_an_llm_host_connects_the_cli_reads_the_feed_and_posts_as_the_user` | nest (linux): passed, nest (macos): passed |
| 2 | nest | `tests/e2e-unified/tests/test_cli_mcp.py::test_the_http_listener_refuses_a_foreign_origin_and_host` | nest (linux): passed, nest (macos): passed |
| 3 | nest | `tests/e2e-unified/tests/test_cli_mcp.py::test_an_llm_host_connects_the_cli_reads_the_feed_and_posts_as_the_user` | nest (linux): passed, nest (macos): passed |
| 4 | nest | `tests/e2e-unified/tests/test_cli_mcp.py::test_an_llm_host_connects_the_cli_reads_the_feed_and_posts_as_the_user` | nest (linux): passed, nest (macos): passed |
| 5 | nest | `tests/e2e-unified/tests/test_cli_mcp.py::test_an_llm_host_connects_the_cli_reads_the_feed_and_posts_as_the_user` | nest (linux): passed, nest (macos): passed |
| 5 | nest | `tests/e2e-unified/tests/test_cli_mcp.py::test_the_feed_tools_read_one_feeds_definition_and_its_posts` | nest (linux): passed, nest (macos): passed |
| 6 | nest | `tests/e2e-unified/tests/test_cli_mcp.py::test_an_llm_host_connects_the_cli_reads_the_feed_and_posts_as_the_user` | nest (linux): passed, nest (macos): passed |
| 7 | nest | `tests/e2e-unified/tests/test_cli_mcp.py::test_an_llm_host_connects_the_cli_reads_the_feed_and_posts_as_the_user` | nest (linux): passed, nest (macos): passed |
| 8 | app | `tests/e2e-unified/tests/test_cli_conversations.py::test_the_cli_reads_and_sends_through_tui_as_custodian_until_revoked` | linux (linux): passed, tui (linux): passed, tui (macos): passed |
| 9 | nest | `tests/e2e-unified/tests/test_cli_mcp.py::test_two_accounts_are_two_principals_and_a_call_names_which` | nest (linux): passed, nest (macos): passed |
| 10 | nest | `tests/e2e-unified/tests/test_cli_mcp.py::test_a_dropped_session_is_redialled_a_read_runs_again_and_a_write_never_does` | nest (linux): passed, nest (macos): passed |
| 11 | nest | `tests/e2e-unified/tests/test_cli_mcp.py::test_with_no_os_store_the_connection_rests_in_a_declared_0600_file_and_resumes` | nest (linux): passed, nest (macos): passed |
| 12 | nest | `tests/e2e-unified/tests/test_cli_mcp.py::test_the_argv_shell_is_the_same_principal_at_the_terminal` | nest (linux): passed, nest (macos): passed |
| 13 | nest | `tests/e2e-unified/tests/test_cli_mcp.py::test_the_admin_plane_serves_an_admin_consented_principal_and_no_member` | nest (linux): passed, nest (macos): passed |
| 13 | nest | `tests/e2e-unified/tests/test_cli_mcp.py::test_admission_admits_a_supervised_member_under_a_guardian_and_an_age_band` | nest (linux): passed, nest (macos): passed |
| 13 | nest | `tests/e2e-unified/tests/test_cli_mcp.py::test_the_admin_reads_one_dmarc_reports_raw_archive` | nest (linux): passed, nest (macos): passed |
| 14 | nest | `tests/e2e-unified/tests/test_cli_mcp.py::test_the_admin_plane_serves_an_admin_consented_principal_and_no_member` | nest (linux): passed, nest (macos): passed |
| 14 | nest | `tests/e2e-unified/tests/test_cli_mcp.py::test_the_catalog_names_no_tool_for_a_sovereign_act` | nest (linux): passed, nest (macos): passed |
| 15 | nest | `tests/e2e-unified/tests/artifact/test_tui_installed_product.py::test_the_install_puts_the_cli_app_beside_the_terminal_app_and_it_runs` | nest (linux): passed |
<!-- features-render:end -->
