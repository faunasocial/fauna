# Groups — retired

Owns: groups
Status: ratified — **retired 2026-09-26.** The dormant `fauna.conversations.group.*` plane this doc recorded — nine kinds, the `groups` + `group_members` tables, their conformance tests and the never-called app-side builders — was deleted under the alpha deletion carve-out, on the user's approval; nothing of it is a target state or an as-built record any more. This file is a pointer stub, kept so the registry row and inbound references resolve.
Authority: none. The room model owns every concern this plane once carried (below); the old text lives in git history (`git log -- docs/goal/behavior/groups.md`).

## Where each concern went

- **Membership, roles, invites, the floor roster, the community class's sealed send** — [`conversation-rooms.md`](conversation-rooms.md) (§ The room, § Roles and authorization, § Join rules and invites, § The floor roster). The room family (`fauna.conversations.room.*`) replaced the nine `group.*` kinds; the room tables were **minted fresh**, so `groups` and `group_members` were dropped (nest schema 86; no genesis creates them) rather than reshaped. § The group plane's fate there records the retirement and its approval gate.
- **MLS channel mechanics (Welcome, key packages, channel send/fetch, typed push frames), and what actually ships as "group chat"** (the MLS-native fork-a-group mechanism, `ThreadFlavor::MlsGroup`) — [`direct-messages.md`](direct-messages.md) and [`../ui/conversations.md`](../ui/conversations.md).
- **Reach policy on a group Welcome, and the rule that a routing decision is never keyed on a sender's self-declaration** — [`direct-messages.md`](direct-messages.md) § Reach policy.
- **The shared `content` table's one-id-one-plane guards** — the post read answers for post rows only, `insert_content` refuses a cross-plane replace, the post store and the discovery index stub attach nothing to another plane's id. These outlive the plane whose messages made them necessary: [`../architecture/nest/common.md`](../architecture/nest/common.md) § One id, one plane.

## What was not retired

- **`tier_mls_groups`** — a subscriptions-plane table that linked a legacy MLS-keyed tier to its epoch material. It shares a word with the group plane and nothing else, so this retirement left it alone; its own plane was retired by the compat-remnant sweep on 2026-09-27 ([`restricted-posts.md`](restricted-posts.md) § Encryption at rest, room ruling 8, lifted), and the writer-less table left the schema with that plane's retire step (schema 91).
- **The account-data storage-group plane** (`fauna_sync_engine`, `fauna-protocol`'s `group_state`) — a different mechanism entirely ([`../architecture/account-data-plane.md`](../architecture/account-data-plane.md)).
- **Any `group/message` row or `posted_to` link in the shared `content` tables** — not part of the approved deletion set. Nothing reads such a row once the kinds are gone; the guards above keep it out of every other plane.

## Implementation status today

Retired. The nest's genesis creates none of `groups`, `group_members` and the `posted_to` link index (dropped at schema 86, by a step the 2026-10-04 genesis collapse folded away). The wire kinds, their registrations (`kind.rs`, `offline_class.rs`, `bridge_method_allowlist.rs`), the nest handlers and DB layer, the per-group `export/groups/<id>.json` entries of the account archive, `fauna-client-core`'s `group.rs` builders and every twin of them (UniFFI, the C-ABI `fauna_group_*` exports, the wasm `buildGroup*` exports, the Swift `FFICompat` wrappers, the tracked Go binding), and the watchOS group views were removed with it.
