# `fauna-mail`

Shared mail-handling Rust core for the Fauna mail bridge and all native apps.

## What's in here

- **`parser`** — RFC 5322 / MIME parsing wrapper around [`mail-parser`](https://crates.io/crates/mail-parser). Returns a UniFFI-friendly `ParsedMessage` with envelope fields, body text/html, headers, and MIME-part metadata.
- **`auth`** — SPF, DKIM, DMARC, ARC verification wrapping [`mail-auth`](https://crates.io/crates/mail-auth). Async; uses system DNS configuration via `hickory-resolver`. Returns small `AuthVerdicts` enums shaped for UniFFI export.
- **`spam`** — DNSBL weight table + score computation + disposition decision. Pure-functional: caller does the DNSBL DNS lookups and feeds the hits in. Ported from `bins/fauna-bridge-daemon/src/dnsbl_weights.rs`.
- **`tokenizer`** — deterministic Unicode tokenizer for encrypted-search index hints. NFKC + UAX#29 word segmentation + lowercase + filter + sort + dedupe. Same plaintext input produces byte-identical output on every platform.
- **`kind_registry`** — pass-through to `fauna-protocol`'s `KindRegistry` for UniFFI consumers.

## How to use it

From Rust (e.g., in `bins/fauna-bridge-daemon/`):

```rust
use fauna_mail::{parse_rfc5322, verify_inbound, score_inbound, decide_spam_disposition, SpamPolicy};
```

From any UniFFI-bound client (Swift, Kotlin, C#, soon Go), the same types are auto-generated under the `fauna_mail` namespace. See `apps/fauna-apple/FaunaKit/Sources/FaunaKit/Generated/uniffi/fauna_mail.swift` (and Kotlin / C# equivalents under their respective `apps/` directories) once the FFI-aggregation crate is built.

## What's *not* in here

- **iCalendar parsing.** Out of scope for this crate's first revision. Will be added when the MDA bridge work begins (or when calendar UI in clients needs it).
- **DKIM canonicalization and signing.** Out of scope for I1; will be added when MTA bridge work begins (the only consumer). Canonicalization without a private key is a thin wrapper around `mail-auth`'s low-level helpers; signing happens at the bridge with the unwrapped DKIM key.
- **MLS encryption / decryption.** Lives in [`fauna-mls`](../fauna-mls/). This crate produces post-validation, post-tokenization plaintext structures; the calling code seals them under MLS for at-rest storage.
- **DNSBL DNS lookups.** Caller's job. We provide the weight table; the caller does the DNS query and feeds hits in.

## Tests

```bash
cargo test -p fauna-mail
```

Run from the workspace root. Unit tests live inline in modules; integration tests in `tests/`. Fixtures (`.eml` files for parser/auth tests) in `tests/fixtures/`.

## Cross-platform reproducibility

The tokenizer's output is a hard correctness property: every platform must produce byte-identical canonical_bytes for a given plaintext. The `tokenizer_tests::reproducibility_vectors_are_stable` test pins this; failures indicate algorithm drift (a `unicode-normalization` or `unicode-segmentation` version bump can cause this — re-pin the table after vetting the change).
