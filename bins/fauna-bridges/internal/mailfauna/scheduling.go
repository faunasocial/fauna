// Thin wrapper for the shared-Rust sealed-scheduling-delivery builder
// (libs/fauna-mail/src/scheduling.rs, over fauna_mls::engine::build_scheduling_delivery).
//
// The CalDAV MDA's server-side auto-schedule gateway consumes this to deliver an
// iMIP to a MAILBOX-LESS Fauna attendee (CalDAV enabled, email disabled) over
// the WS-RPC sealed MLS scheduling rail instead of email
// (caldav-server.md § Server-side auto-schedule). One-call wrapper, not a
// re-implementation (priority #2).

package mailfauna

import (
	faunaFfi "github.com/faunasocial/fauna/libs/fauna-mail-go/fauna_ffi"
	faunaMail "github.com/faunasocial/fauna/libs/fauna-mail-go/fauna_mail"
)

// SealedSchedulingDelivery mirrors libs/fauna-mail/src/scheduling.rs::
// SealedSchedulingDelivery — the MDA-sealed one-off MLS scheduling delivery:
// WelcomeBytes (the tagged-Scheduling MLS Welcome → welcome.deliver),
// ChannelIdHex (the one-off channel id, hex → both welcome.deliver and
// channel.send), AppEnvelope (the first + only application message, the sealed
// iMIP → channel.send).
type SealedSchedulingDelivery = faunaMail.SealedSchedulingDelivery

// BuildSchedulingDelivery seals an iMIP scheduling message (`imipRfc5322`) to a
// mailbox-less recipient's key package as a one-off MLS Welcome + application
// message, via the single-sourced shared-Rust builder (the SAME sealer a Fauna
// app uses, priority #2). It mints a fresh EPHEMERAL MLS signer — the MDA
// never holds the organizer's Ed25519 secret (CalDAV auth is password→capability)
// — so nest only ever sees ciphertext; soundness rests on the recipient
// authenticating the invite by the iMIP ORGANIZER, not the one-off MLS creator
// credential (which it ignores). `senderActorID` is the organizer's 32-byte
// actor id (stamped as the app-level sender). Errors when the key package or
// sender id is malformed.
func BuildSchedulingDelivery(recipientKp, senderActorID, imipRfc5322 []byte) (SealedSchedulingDelivery, error) {
	return faunaMail.BuildSchedulingDelivery(recipientKp, senderActorID, imipRfc5322)
}

// AttendeeTransport is the classified rail one off-box (cross-nest) attendee's
// iMIP must ride — the typed Go view of fauna-ffi's classify_attendee_transport
// flat [rail, actor_id, nest_url].
type AttendeeTransport struct {
	// Rail is "sealed" (a mailbox-less Fauna attendee on an email-disabled peer
	// nest → the WS-RPC sealed MLS scheduling rail) or "email" (every other case —
	// an external address, or a mail-enabled Fauna handle → the bridge MTA).
	Rail string
	// ActorIDHex + NestURL are set only for the "sealed" rail: the resolved
	// attendee actor (64-hex) and its nest base URL — the inputs the caller's
	// cross-nest keypackage-fetch (nest_url) + sealed delivery (peer domain) need.
	ActorIDHex string
	NestURL    string
}

// ClassifyAttendeeTransport classifies ONE off-box (cross-nest) attendee
// CAL-ADDRESS into the rail its server-side auto-schedule iMIP must ride, via the
// shared resolver's anonymous cross-nest discovery (faunaFfi.ClassifyAttendeeTransport
// → fauna_client_caldav::resolve_attendee_transport with the production
// AnonAttendeeDiscovery: anon fauna.actor.by_handle on the attendee's nest → that
// nest's fauna.setup.status.email_enabled). The SAME decision the native app
// send path makes (priority #2/#4) — the Go MDA gateway's off-box-domain branch
// reaches it here instead of re-coding the anon by_handle→setup.status decision in
// Go. caldav-server.md § Server-side auto-schedule. The cgo call does real network
// (its own tokio runtime, like VerifyInbound); the caller treats any error as
// "degrade to the email rail".
func ClassifyAttendeeTransport(addr string) (AttendeeTransport, error) {
	parts, err := faunaFfi.ClassifyAttendeeTransport(addr)
	if err != nil {
		return AttendeeTransport{}, err
	}
	var t AttendeeTransport
	if len(parts) > 0 {
		t.Rail = parts[0]
	}
	if len(parts) > 1 {
		t.ActorIDHex = parts[1]
	}
	if len(parts) > 2 {
		t.NestURL = parts[2]
	}
	return t, nil
}
