# i18n String Architecture — target state

Owns: i18n
Status: ratified
Authority: the i18n string architecture — the single-source string set (`i18n/strings/en.yaml`), the no-platform-prefixed-sections rule, the named-placeholder + format-string-punctuation rules, and the dedup tooling; the session workflow (edit `en.yaml` → `just i18n-generate` → use the generated constant, never the generated per-app files directly); codegen specifics → `i18n/generator/generate.py`; recipe gating/freshness → [`build-system.md`](build-system.md) § What's gated. On conflict in those domains, raise it.

Last verified: 2026-08-28 (docs-consistency sweep — architecture/tooling re-confirmed accurate against `generate.py`/`justfile`/the coverage lint; Rust emission re-confirmed single-source, `libs/fauna-i18n/src/strings.rs`, with linux re-exporting it rather than holding its own generated copy; the one-key `macos:` dead-duplicate section this doc previously flagged as backlog was deleted, so `en.yaml` now carries no platform-prefixed sections outside the `tui_*` carve-out) | Sources: `i18n/strings/en.yaml`, `i18n/generator/generate.py`, `justfile`, the i18n-coverage lint

## Goal

One shared string set in `i18n/strings/en.yaml` covers all seven apps. There are no per-platform sections — Web, Windows, macOS, iOS, Android, Linux, and the terminal app (tui) read the same key paths through their native string mechanisms, with platform-specific output files generated from the same source. Punctuation goes through format strings so locale rules (like French's space before `?`) live in one place rather than being baked into per-string literals.

## Overview

All i18n strings live in `i18n/strings/en.yaml` as a single shared set. There are **no platform-specific sections** — every app (Web, Windows, macOS, iOS, Android, Linux, tui) uses the same key paths.

The generator (`i18n/generator/generate.py`) reads en.yaml and produces the platform-specific files via `just i18n-generate` (the exact folder is [`build-system.md`](build-system.md) § What's gated's row — don't count it here, counts rot). Each platform accesses strings through its native mechanism (TypeScript objects, one shared FaunaKit Swift file for macOS+iOS, Rust modules, C# resource loader, Android resources, Python classes).

## Rules for New Strings

1. **Check `common.*` first.** Generic words (Cancel, Save, Reply, Settings, Files, etc.) are already in `common.*`. Use them instead of adding section-specific duplicates.
2. **Run `just i18n-lint`** before committing to catch new duplicates.
3. **Use format strings for punctuation.** Don't hardcode `?`, `!`, or `...` — use `common.fmt_question`, `common.fmt_exclamation`, or `common.fmt_ellipsis` so punctuation can be localized.
4. **Section-specific strings** go in the relevant feature section (e.g. `events.*`, `groups.*`). Only add a string to a section if it's semantically tied to that feature.
5. **Never add platform-prefixed sections** (no `windows.*`, `macos.*`, `*_ios`, `*_view`, `*_screen`). All strings are shared. **Carve-out:** a section is allowed to be platform-prefixed only when the *UI concept itself* doesn't exist on any other app — not merely rendered differently there (the shell-vs-leaf-component distinction: per-app shells may diverge, the leaf strings inside a shared concept may not). Each such section must carry an explanatory comment stating why the concept is platform-exclusive. `tui_unlock`/`tui_nav_hints`/`tui_settings` are the standing example: a headless-credential-store passphrase flow, a keyboard-only key-hint footer, and a terminal-specific external-media-handoff setting have no analog on any pointer-driven app. A section with no such comment, or one whose strings duplicate an existing shared key, is not a carve-out — it's drift to converge (see the i18n cross-app uniformity backlog).
6. **Placeholders are named (`{count}`, `{domain}`), never positional (`{0}`).** The generator preserves named placeholders as-is on every platform; the old `{name}`→`%N$s`/`{N}` positional rewrites forced multi-arg keys to depend on argument order (`generate.py:154-199` records why they were removed), and a numeric `{0}` placeholder breaks the Rust generator output. (Windows RESW is flat — C# substitutes into the named form at the call site.)

## Deduplication Tools

| Tool | Purpose |
|------|---------|
| `just i18n-lint` | Report exact duplicates, near-duplicates, and common.* shadows |
| `just i18n-generate` | Regenerate all platform files from en.yaml |
| `just i18n-check` | Verify generated files are up to date |
| `i18n-coverage` | Advisory 7-app string-coverage lint (a dedicated dev-fleet checker) |
| `i18n/generator/dedup_strings.py` | Automated deduplication script (run manually when needed) |
| `i18n/generator/key_mapping.json` | Mapping of removed keys to canonical replacements (reference for code migration) |

## Format Strings for Punctuation

Punctuation is locale-dependent (e.g. French puts a space before `?`). Use these format strings instead of hardcoding punctuation:

```yaml
common:
  fmt_question: "{text}?"      # e.g. "Delete Account?"
  fmt_exclamation: "{text}!"   # e.g. "Done!"
  fmt_ellipsis: "{text}..."    # e.g. "Downloading..."
```

Usage per platform:
- **Swift (FaunaKit, macOS + iOS — one generated `L.swift` for both):** `L.common.fmtQuestion(text: L.settings.accountPage.deleteAccount)`
- **Kotlin:** `stringResourceFmt(R.string.common_fmt_question, stringResource(R.string.settings_account_page_delete_account))` (plain `stringResource(id, args…)` won't substitute — the generated resource keeps the `{name}` token; `stringResourceFmt`/`getStringFmt` in `ui/util/Localized.kt` map positional args onto it by name)
- **TypeScript:** `t.common.fmt_question({text: t.settings.account_page.delete_account})`
- **Rust:** `common::fmt_question(settings::account_page::DELETE_ACCOUNT)`
- **C#:** `S.Format("common/fmt_question", S.Get("settings/account_page/delete_account"))` (`Strings.Format` in `Services/IStringLocalizer.cs` maps positional args onto the `{name}` tokens — plain `string.Format` doesn't understand named tokens)

## History

All platform-specific sections were consolidated in April 2026:

| Former Section | Merged Into |
|---|---|
| `windows.*` (15 sub-sections) | Respective shared sections |
| `macos.*` (6 sub-sections) | Respective shared sections |
| `settings_screen`, `settings_ios` | `settings` |
| `status_screen`, `status_view` | `status` |
| `events_ios` | `events` |
| `groups_ios` | `groups` |
| `contacts_ios` | `contacts` |
| `onboarding_ios` | `onboarding` |
| `messages_ios` | `conversations` |
| `bridges_ios` | `bridges` |
| `search_view` | `search_page` |
| `admin_view` | `admin` |
| `peers_view` | `devices.peers` |
| `notifications_page` | `notifications` |
| `sidebar` | `navigation` |

Additionally, 366 duplicate string values were consolidated into `common.*` and redundant keys removed. The lint issue count went from 538 to 1 (one intentional case difference: placeholder `"subdomain"` vs label `"Subdomain"`).
