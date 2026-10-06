package imap

import "github.com/emersion/go-imap/v2"

// capabilityList returns the per-server capability set the MDA
// advertises.
//
// **Important constraint:** emersion/go-imap v2's
// imapserver.Conn.availableCaps filters our advertised set through
// a hardcoded whitelist. Capabilities NOT in that whitelist are
// silently dropped from the CAPABILITY response — even if we list
// them here. The advertisement set the library will actually emit
// from this CapSet is:
//
//   - Pre-auth: IMAP4rev2, IMAP4rev1, SASL-IR, LITERAL- + AUTH=<mech>.
//   - Post-auth: IMAP4rev2, IMAP4rev1, SPECIAL-USE, LITERAL+, CONDSTORE,
//     QRESYNC, QUOTA (+ QUOTA=RES-STORAGE / QUOTA=RES-MESSAGE), and the
//     rev1-compat caps the fork unlocks once IMAP4rev1 is present
//     (UNSELECT, ENABLE, IDLE, UTF8=ACCEPT).
//     (UIDPLUS / LIST-EXTENDED / MOVE / etc. are folded into IMAP4rev2
//     per RFC 9051 appendix E; rev2 clients derive them from "IMAP4rev2".)
//
// **Why advertise IMAP4rev1 alongside IMAP4rev2** (imap-server.md
// § Capabilities): RFC 9051 §2 recommends a rev2 server also announce
// rev1 for backward compatibility, and that is what real servers
// (Dovecot, Gmail) do. A rev2-only advertisement is rejected outright by
// rev1-only clients — Python's stdlib `imaplib` raises "server not IMAP4
// compliant", and older MUAs / tooling can't connect. The fork implements
// both profiles in one server (branching on `enabled.Has(IMAP4rev2)`), so
// a rev2 MUA still ENABLEs and uses rev2 while a rev1 client gets rev1
// response formats — no feature loss, strictly wider compatibility.
//
// CONDSTORE (T3.2-a), QRESYNC (T3.2-b), and QUOTA (T3.2-c) now surface
// post-auth: the FAUNA-FORK of emersion/go-imap (vendored at
// third_party/go-imap, see FORK.md) added their wire seams — MODSEQ /
// UNCHANGEDSINCE / HIGHESTMODSEQ for CONDSTORE; the (QRESYNC ...)
// SELECT-param parser, (CHANGEDSINCE n VANISHED) FETCH modifier, and
// `* VANISHED` writers for QRESYNC; the GETQUOTA/GETQUOTAROOT parsers,
// SessionQuota dispatch hook, and `* QUOTA` / `* QUOTAROOT` writers for
// QUOTA — and whitelists all three in availableCaps. NOTIFY remains
// listed below for future-proofing — the library still drops it at
// render time until its fork seam lands (NOTIFY wire generator). Per
// imap-server.md § Upstream-blocked gaps, advertising what we can't
// generate is worse than silent non-implementation.
//
// AUTH=PLAIN / AUTH=OAUTHBEARER are surfaced per-conn by the server
// library — emersion advertises AUTH=PLAIN by default; C.3 lands
// SessionSASL.AuthenticateMechanisms so the OAUTHBEARER variant
// joins it (and PLAIN/OAUTHBEARER vanish from the response pre-TLS
// on 143 because InsecureAuth=false).
func capabilityList() imap.CapSet {
	return imap.CapSet{
		// Library-supported (appear in the post-auth CAPABILITY response).
		// CONDSTORE + QRESYNC + QUOTA surface post-auth via the FAUNA-FORK
		// seams. QUOTA additionally renders its RFC 9208 §6 resource-type
		// capabilities (QUOTA=RES-STORAGE / QUOTA=RES-MESSAGE) — emitted by
		// the fork's availableCaps, not enumerable in this CapSet.
		imap.CapIMAP4rev2: {},
		// IMAP4rev1 advertised alongside rev2 for backward compatibility
		// (RFC 9051 §2) — without it, rev1-only clients (Python imaplib,
		// older MUAs) reject the server. The fork serves both profiles.
		imap.CapIMAP4rev1:   {},
		imap.CapSpecialUse:  {},
		imap.CapLiteralPlus: {},
		imap.CapCondStore:   {},
		imap.CapQResync:     {},
		imap.CapQuota:       {},

		// Future-proofing (still dropped by the library at render time;
		// surfaces when the remaining fork seams / upstream PRs land).
		// Listed here so the diff is small when they catch up.
		imap.CapNotify: {},
	}
}
