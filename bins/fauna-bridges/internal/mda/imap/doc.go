// Package imap is the IMAP4rev2 wire surface for the MDA role of
// fauna-mail-bridge. The package wraps emersion/go-imap v2's
// imapserver.Server, with a Fauna-specific Backend that owns the
// nest-side mailbox state through WS-RPC. Per imap-server.md:
//
//   - Two listeners share one Backend: 993 implicit TLS + 143 STARTTLS.
//   - Authentication is mode-conditional. Encrypted mode (the only
//     mode wired here) uses AEAD-unwrap-as-auth via PLAIN or
//     OAUTHBEARER; AllowInsecureAuth = false.
//   - Capabilities advertised: IMAP4rev2, IDLE, CONDSTORE, QRESYNC,
//     MOVE, UIDPLUS, ENABLE, SPECIAL-USE, LITERAL+, LIST-EXTENDED,
//     NOTIFY, QUOTA. SORT / THREAD / ManageSieve are upstream-blocked
//     and intentionally absent (per imap-server.md § Upstream-blocked
//     gaps).
//   - All mailbox state lives in nest's SQLite (`bridge_imap_*`
//     placement tables); the MDA process is a stateless protocol
//     terminator holding only the per-session unwrapped MLS-decryption
//     capability and ephemeral caches.
//
// The server skeleton (this file plus server.go + capabilities.go)
// and the public Backend interface sit beside the concrete Backend +
// Session implementations; the one authenticated-state command still
// stubbed is STATUS (it answers NO via errNotImplemented).
package imap
