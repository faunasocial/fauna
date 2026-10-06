# fauna-mail-bridge — the mail/calendar bridge

The Go daemon that gives a nest standards-speaking mail and calendar
endpoints: SMTP (inbound MX + submission) and IMAP/CalDAV/CardDAV/WebDAV. It
talks WS-RPC directly to the nest and links the shared Rust mail logic
(`libs/fauna-mail`: recipient routing, DKIM/ARC verification, MTA-STS) through
a cgo FFI (`libs/fauna-ffi` → the checked-in bindings in `libs/fauna-mail-go`).
The bridge verifies inbound signatures but holds no DKIM key and signs nothing:
the nest signs each outbound message as it hands it to the bridge for delivery.

One binary, two roles — **MTA** (SMTP) and **MDA** (IMAP/DAV). A process
learns its role from its nest service-user enrollment, not from flags: the
deployment supervisor starts it with the right keypair and the binary asks the
nest who it is.

## Build & test

Needs Go (see `go.mod` for the version) plus the repo's Rust toolchain. From
the repository root:

```sh
just mail-bridge-build    # builds the Rust FFI cdylib, then the Go binary
just mail-bridge-test     # Go test suite (rebuilds the FFI first)
```

The generated Go bindings are checked in; any `libs/fauna-ffi` surface change
requires regenerating them (the `just` recipes handle this).

## In deployment

The nest Docker image runs two instances (`fauna-mail-bridge-mta`,
`fauna-mail-bridge-mda`) under s6 supervision, each with its own UID and
keypair, started/stopped on the admin's in-app mail toggle — see
[`docs/goal/architecture/installers/docker.md`](../../docs/goal/architecture/installers/docker.md)
and [`docs/goal/behavior/mail-bridge-lifecycle.md`](../../docs/goal/behavior/mail-bridge-lifecycle.md).

## Layout

- `cmd/fauna-mail-bridge/` — main; `cmd/fauna-supervisor/` — the s6 sidekick
- `internal/mta/` — SMTP receive/submit/relay, spam/virus scan gate
- `internal/mda/` — IMAP, CalDAV/CardDAV/WebDAV serving
- `third_party/` — vendored protocol libraries
