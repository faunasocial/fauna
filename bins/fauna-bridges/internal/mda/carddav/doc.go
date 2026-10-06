// Package carddav serves the CardDAV wire surface (RFC 6352) for the
// `fauna-mail-bridge` MDA role — see
// `docs/goal/behavior/carddav-server.md`.
//
// Built on `github.com/emersion/go-webdav/carddav` with a Fauna-specific
// Backend that authenticates over HTTP Basic via the shared
// `internal/mda/davauth` middleware — the same AEAD-unwrap-as-auth flow the
// CalDAV terminator and IMAP use ("AEAD-success = AUTH-success"). Every
// address book / card row lives nest-side (`bridge_carddav_addressbooks`,
// `bridge_carddav_cards`, `bridge_carddav_expunged`); this package is a
// stateless protocol terminator that holds only the per-session unwrapped
// MLS-decryption capability + the AUTH'd actor's public keys for the request
// lifetime.
//
// SEAL-ALWAYS. Unlike the CalDAV terminator (which branches on StorageMode to
// ship plaintext iCal at rest in plaintext-mode deployments), the CardDAV
// terminator seals EVERY vCard body + index hint to the AUTH'd actor's MLS
// pubkey before `put_card_ciphertext`, in BOTH storage modes. Contacts have no
// external plaintext-ingest boundary (design § 7 Threat model) and no
// server-side-read-at-rest purpose, so they fail the plaintext-at-rest purpose
// test; `bridge_carddav_cards.encrypted_body` always holds ciphertext. There is
// therefore no `StorageMode` / `plaintextStorage` / `nestPlaintext()` anywhere
// in this package — the seal/open path is unconditional.
//
// W1 (account-data-plane.md § Workstreams) mitigations (PROPFIND depth ≤ 3, request body cap 16 MiB, XML depth ≤ 256)
// are enforced as a wrapping middleware — emersion/go-webdav v0.7.0's
// `carddav.Handler` exposes no Options struct for these limits, so the request
// gate runs before the library's XML parser ever touches the payload.
package carddav
