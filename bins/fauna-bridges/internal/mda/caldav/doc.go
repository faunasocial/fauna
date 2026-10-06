// Package caldav serves the CalDAV wire surface for the
// `fauna-mail-bridge` MDA role per
// `docs/goal/behavior/caldav-server.md`.
//
// Built on `github.com/emersion/go-webdav/caldav` with a Fauna-
// specific Backend that authenticates over HTTP Basic via the same
// AEAD-unwrap-as-auth flow as IMAP (per the goal doc § Authentication
// — "AEAD-success = AUTH-success" applies to both surfaces). Every
// calendar / event row lives nest-side (`bridge_caldav_calendars`,
// `bridge_caldav_events`, `bridge_caldav_expunged`); this package is
// a stateless protocol terminator that holds only the per-session
// unwrapped MLS-decryption capability + a tiny decrypted-metadata
// cache for the AUTH'd session lifetime.
//
// W1 (account-data-plane.md § Workstreams) mitigations from the spec (PROPFIND depth ≤ 3, request body cap
// 16 MiB, XML depth ≤ 256) are enforced as a wrapping middleware —
// emersion/go-webdav v0.7.0's `caldav.Handler` exposes no Options
// struct for these limits, so the request gate runs before the
// library's XML parser ever touches the payload.
//
// Phase E.2 lands AUTH + PROPFIND (read surface) — the lazy
// "Personal" calendar on first PROPFIND, calendar listing on
// repeated PROPFINDs. PUT / DELETE / REPORT land in E.3; PROPPATCH
// in E.4.
package caldav
