// Package dav is the shared DAV-over-HTTPS serving substrate for the MDA role.
//
// CalDAV and CardDAV are both WebDAV on the wire and both reached at
// mail.<domain>:443 — their SRV records (`_caldavs._tcp`, `_carddavs._tcp`)
// target the same port. Two listeners cannot bind one port, so the MDA
// terminates TLS once on 443 and routes by URL path to the per-protocol
// handler: `/carddav/…` → the CardDAV chain, everything else (the CalDAV
// principal at root `/{user}/`, `/caldav/…`, `/.well-known/caldav`) → the
// CalDAV chain. This package owns that one `http.Server` + `http.ServeMux`;
// the caldav / carddav packages each expose their middleware+backend chain via
// `Server.Handler()` and this package mounts them.
//
// The per-protocol packages keep their own standalone `Serve`/`Shutdown`
// (exercised by their unit tests) so each is usable in isolation; production
// serving goes through this shared substrate. A future WebDAV-files "Sequel"
// (design § Sequel) mounts under `/webdav/` by adding one more Mount — no new
// listener, no new port.
package dav
