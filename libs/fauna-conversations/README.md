# fauna-conversations

Shared-Rust crate that owns multi-rail thread management for the unified
conversations page. One `RailBackend` trait, five impls (`fauna_mls`,
`smtp`, `bluesky`, `nostr`, `activitypub`). All UI clients render off
snapshots produced by `ConversationsManager`.

See:
- design tracked internally
- `docs/goal/ui/conversations.md` — page-level UX
- `docs/goal/behavior/direct-messages.md` — protocol-side DM contract

## Modules

- `address`, `thread`, `message`, `compose`, `snapshot` — value types
- `capabilities` — per-(rail, flavor) capability matrix
- `keying` — subject normalization + inbound routing rule
- `backend` — `RailBackend` trait + adapter types
- `manager` — `ConversationsManager` (top-level entry point)
- `store` — thread + draft persistence (in-memory v1)
- `observer` — snapshot diff observer
- `backends` — per-rail impls
- `contacts` — cross-rail address-book stub
