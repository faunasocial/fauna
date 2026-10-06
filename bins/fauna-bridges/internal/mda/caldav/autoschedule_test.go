package caldav

import (
	"bytes"
	"context"
	"errors"
	"io"
	"log/slog"
	"net/http"
	"strings"
	"sync/atomic"
	"testing"

	"github.com/faunasocial/fauna/bins/fauna-bridges/internal/mailfauna"
	"github.com/faunasocial/fauna/bins/fauna-bridges/internal/mda/davauth"
	"github.com/faunasocial/fauna/bins/fauna-bridges/internal/wsrpc"
	"github.com/fxamacker/cbor/v2"
)

// An event the AUTH'd fixture user (alice@example.com) ORGANIZES, inviting an
// external attendee (bob) plus themself. The server-side auto-schedule gateway
// should fan an iMIP REQUEST out to bob (the organizer is excluded).
const testOrganizerEvent = "BEGIN:VCALENDAR\r\n" +
	"VERSION:2.0\r\n" +
	"PRODID:-//fauna//test//EN\r\n" +
	"BEGIN:VEVENT\r\n" +
	"UID:autosched-uid-001\r\n" +
	"DTSTAMP:20260516T120000Z\r\n" +
	"DTSTART:20260601T100000Z\r\n" +
	"DTEND:20260601T110000Z\r\n" +
	"SUMMARY:Design review\r\n" +
	"ORGANIZER:mailto:alice@example.com\r\n" +
	"ATTENDEE;PARTSTAT=NEEDS-ACTION:mailto:bob@example.com\r\n" +
	"ATTENDEE;PARTSTAT=ACCEPTED:mailto:alice@example.com\r\n" +
	"END:VEVENT\r\n" +
	"END:VCALENDAR\r\n"

// An event the AUTH'd fixture user (alice) is only an ATTENDEE of — the
// ORGANIZER is someone else (carol). A non-organizer PUT (e.g. alice syncing
// her own copy of a shared event) must NOT fan out.
const testAttendeeOnlyEvent = "BEGIN:VCALENDAR\r\n" +
	"VERSION:2.0\r\n" +
	"PRODID:-//fauna//test//EN\r\n" +
	"BEGIN:VEVENT\r\n" +
	"UID:autosched-uid-002\r\n" +
	"DTSTAMP:20260516T120000Z\r\n" +
	"DTSTART:20260601T100000Z\r\n" +
	"DTEND:20260601T110000Z\r\n" +
	"SUMMARY:Someone else's event\r\n" +
	"ORGANIZER:mailto:carol@example.com\r\n" +
	"ATTENDEE;PARTSTAT=NEEDS-ACTION:mailto:alice@example.com\r\n" +
	"END:VEVENT\r\n" +
	"END:VCALENDAR\r\n"

// enqueueOutboundDecoded mirrors the wire fields the gateway sends, for
// asserting the recorded enqueue_outbound_mail call body.
type enqueueOutboundDecoded struct {
	OriginalMsgID   string   `cbor:"original_msgid"`
	OriginalSender  string   `cbor:"original_sender"`
	Recipients      []string `cbor:"recipients"`
	RawMessage      []byte   `cbor:"raw_message"`
	OnBehalfOfActor *[]byte  `cbor:"on_behalf_of_actor"`
}

// TestClassifyAutoScheduleRecipients pins the C4 classifier decision
// (caldav-server.md § Server-side auto-schedule): which rail each attendee
// rides. It calls the classifier directly (no real MLS seal — that's the
// tier_3 e2e's job) with a mockCaller answering the resolve_recipient →
// actor.by_handle → keypackage.fetch chain. LocalDomains = {fauna.test}.
func TestClassifyAutoScheduleRecipients(t *testing.T) {
	ctx := context.Background()
	localDomains := &atomic.Pointer[[]string]{}
	ld := []string{"fauna.test"}
	localDomains.Store(&ld)

	newSess := func(caller wsrpc.Caller) *davauth.Session {
		return davauth.NewSession(caller, slog.Default(), fixtureActorID, "alice", "fauna.test")
	}
	classify := func(caller *mockCaller, recipients ...string) ([]string, []mailboxLessRecipient) {
		b := NewBackend(slog.Default(), localDomains)
		// This same-nest suite's off-box addresses are genuinely external
		// (external.com); stub the cross-nest discovery to "email" so these tests
		// stay hermetic (no real anon-TLS probe). The dedicated cross-nest sealed
		// routing is exercised by TestClassifyAutoScheduleRecipientsCrossNest.
		b.classifyTransport = func(string) (mailfauna.AttendeeTransport, error) {
			return mailfauna.AttendeeTransport{Rail: "email"}, nil
		}
		return b.classifyAutoScheduleRecipients(ctx, newSess(caller), recipients)
	}

	t.Run("externalDomainRidesEmailWithNoNestRPC", func(t *testing.T) {
		// An off-box domain the cross-nest resolver classifies "email" (here
		// external.com — not a Fauna nest) rides the email rail and fires NO nest
		// RPC: the off-box branch makes the email-vs-sealed decision via the shared
		// resolver (stubbed), never the same-nest resolve_recipient / actor.by_handle
		// reads (those are the local-domain branch).
		caller := &mockCaller{}
		email, mbl := classify(caller, "bob@external.com")
		if len(mbl) != 0 || len(email) != 1 || email[0] != "bob@external.com" {
			t.Fatalf("external: email=%v mailboxLess=%v", email, mbl)
		}
		if n := len(caller.callsOf(wsrpc.MethodResolveRecipient)); n != 0 {
			t.Errorf("resolve_recipient fired %d times for an external attendee, want 0", n)
		}
		if n := len(caller.callsOf(wsrpc.MethodActorByHandle)); n != 0 {
			t.Errorf("actor.by_handle fired %d times for an external attendee, want 0", n)
		}
	})

	t.Run("mailEnabledLocalToEmail", func(t *testing.T) {
		// A local attendee with a mailbox (resolve_recipient Resolved) stays on
		// the email rail (in-domain seal) — and Resolved short-circuits, so no
		// by_handle/keypackage reads.
		caller := &mockCaller{resolveRecipientOutcome: "resolved"}
		email, mbl := classify(caller, "carol@fauna.test")
		if len(mbl) != 0 || len(email) != 1 || email[0] != "carol@fauna.test" {
			t.Fatalf("mail-enabled: email=%v mailboxLess=%v", email, mbl)
		}
		if n := len(caller.callsOf(wsrpc.MethodActorByHandle)); n != 0 {
			t.Errorf("actor.by_handle fired %d times for a mail-enabled attendee, want 0", n)
		}
	})

	t.Run("forwardedLocalToEmail", func(t *testing.T) {
		// An admin forwarder (resolve_recipient Forward) is email-reachable too.
		caller := &mockCaller{resolveRecipientOutcome: "forward"}
		email, mbl := classify(caller, "team@fauna.test")
		if len(mbl) != 0 || len(email) != 1 {
			t.Fatalf("forward: email=%v mailboxLess=%v", email, mbl)
		}
	})

	t.Run("mailboxLessLocalToSealedRail", func(t *testing.T) {
		// A local attendee with no mail route (Reject) that resolves to a Fauna
		// actor with a fetchable key package → the sealed rail.
		caller := &mockCaller{
			resolveRecipientOutcome: "reject",
			actorByHandleActorIDHex: strings.Repeat("bc", 32),
			keypackageBytes:         []byte("fake-kp-bytes"),
		}
		email, mbl := classify(caller, "dave@fauna.test")
		if len(email) != 0 {
			t.Fatalf("mailbox-less must not ride the email rail: %v", email)
		}
		if len(mbl) != 1 ||
			mbl[0].address != "dave@fauna.test" ||
			mbl[0].actorIDHex != strings.Repeat("bc", 32) ||
			!bytes.Equal(mbl[0].keyPackage, []byte("fake-kp-bytes")) {
			t.Fatalf("mailbox-less classification: %+v", mbl)
		}
	})

	t.Run("unknownLocalHandleToEmail", func(t *testing.T) {
		// A local part with no mailbox AND no Fauna handle (by_handle not_found)
		// falls back to the email rail rather than being dropped.
		caller := &mockCaller{resolveRecipientOutcome: "reject", actorByHandleActorIDHex: ""}
		email, mbl := classify(caller, "ghost@fauna.test")
		if len(mbl) != 0 || len(email) != 1 || email[0] != "ghost@fauna.test" {
			t.Fatalf("unknown local: email=%v mailboxLess=%v", email, mbl)
		}
	})

	t.Run("noKeyPackageToEmail", func(t *testing.T) {
		// A Fauna actor with no fetchable key package (not reachable over MLS)
		// falls back to the email rail.
		caller := &mockCaller{
			resolveRecipientOutcome: "reject",
			actorByHandleActorIDHex: strings.Repeat("bc", 32),
			keypackageBytes:         nil,
		}
		email, mbl := classify(caller, "frank@fauna.test")
		if len(mbl) != 0 || len(email) != 1 || email[0] != "frank@fauna.test" {
			t.Fatalf("no-KP: email=%v mailboxLess=%v", email, mbl)
		}
	})

	t.Run("mixedRosterSplits", func(t *testing.T) {
		// The core split: an external attendee and a local mailbox-less attendee
		// in one roster land on their respective rails.
		caller := &mockCaller{
			resolveRecipientOutcome: "reject",
			actorByHandleActorIDHex: strings.Repeat("bc", 32),
			keypackageBytes:         []byte("kp"),
		}
		email, mbl := classify(caller, "bob@external.com", "dave@fauna.test")
		if len(email) != 1 || email[0] != "bob@external.com" {
			t.Fatalf("mixed email rail: %v", email)
		}
		if len(mbl) != 1 || mbl[0].address != "dave@fauna.test" {
			t.Fatalf("mixed sealed rail: %+v", mbl)
		}
	})

	t.Run("emailDisabledNestLocalToSealedRail", func(t *testing.T) {
		// On an email-DISABLED nest (a CalDAV-only deployment) a local attendee's
		// canonical <handle>@<domain> alias still resolves (resolve_recipient
		// Resolved — `ensure_canonical_handle_alias` fires on the recipient-pubkey
		// provision regardless of email), but no MTA/INBOX delivers to it. So the
		// classifier must SKIP the resolve_recipient short-circuit and route a
		// reachable Fauna actor onto the sealed rail. RED before the fix: the alias
		// → email rail — the bug a real CalDAV-only user hits, since enabling CalDAV
		// (minting the MSEK / recipient pubkey) creates that canonical alias.
		mailEnabled := &atomic.Bool{} // false ⇒ email-disabled nest
		b := NewBackend(slog.Default(), localDomains)
		b.mailEnabled = mailEnabled
		caller := &mockCaller{
			resolveRecipientOutcome: "resolved", // the canonical alias WOULD Resolve…
			actorByHandleActorIDHex: strings.Repeat("bc", 32),
			keypackageBytes:         []byte("kp"),
		}
		email, mbl := b.classifyAutoScheduleRecipients(ctx, newSess(caller), []string{"carol@fauna.test"})
		if len(email) != 0 {
			t.Fatalf("email-disabled nest: a local Fauna attendee must not ride the email rail: %v", email)
		}
		if len(mbl) != 1 || mbl[0].address != "carol@fauna.test" {
			t.Fatalf("email-disabled nest: want the sealed rail, got %+v", mbl)
		}
		// …and resolve_recipient is skipped entirely (the canonical alias is an
		// AUTH identity on an email-disabled nest, not a routing signal).
		if n := len(caller.callsOf(wsrpc.MethodResolveRecipient)); n != 0 {
			t.Errorf("resolve_recipient fired %d times on an email-disabled nest, want 0", n)
		}
	})
}

// TestClassifyAutoScheduleRecipientsCrossNest pins the CROSS-NEST classifier
// decision (caldav-server.md § Server-side auto-schedule): an off-box-domain
// attendee is no longer routed unconditionally to email — a mailbox-less Fauna
// attendee on a different, email-disabled nest rides the SEALED rail with
// peerDomain = that foreign domain (so deliver_sealed_scheduling relays the
// welcome cross-nest), while every other off-box outcome degrades to email. The
// cross-nest discovery (the real cgo anon-TLS call) is stubbed via the injectable
// classifyTransport seam; the KP comes from the own-nest federation relay
// (KeypackageFetch w/ a non-empty nest_url), answered by the mock.
func TestClassifyAutoScheduleRecipientsCrossNest(t *testing.T) {
	ctx := context.Background()
	localDomains := &atomic.Pointer[[]string]{}
	ld := []string{"fauna.test"}
	localDomains.Store(&ld)

	newSess := func(caller wsrpc.Caller) *davauth.Session {
		return davauth.NewSession(caller, slog.Default(), fixtureActorID, "alice", "fauna.test")
	}
	withStub := func(
		stub func(addr string) (mailfauna.AttendeeTransport, error),
		caller *mockCaller, recipients ...string,
	) ([]string, []mailboxLessRecipient) {
		b := NewBackend(slog.Default(), localDomains)
		b.classifyTransport = stub
		return b.classifyAutoScheduleRecipients(ctx, newSess(caller), recipients)
	}

	t.Run("crossNestMailboxLessToSealedRail", func(t *testing.T) {
		// A mailbox-less Fauna attendee on a different, email-disabled nest → sealed
		// rail, peerDomain = the foreign domain, KP from the own-nest relay.
		actorHex := strings.Repeat("ab", 32)
		stub := func(string) (mailfauna.AttendeeTransport, error) {
			return mailfauna.AttendeeTransport{Rail: "sealed", ActorIDHex: actorHex, NestURL: "https://peer.example"}, nil
		}
		caller := &mockCaller{keypackageBytes: []byte("peer-kp-bytes")}
		email, mbl := withStub(stub, caller, "eve@peer.example")
		if len(email) != 0 {
			t.Fatalf("cross-nest mailbox-less must not ride the email rail: %v", email)
		}
		if len(mbl) != 1 ||
			mbl[0].address != "eve@peer.example" ||
			mbl[0].actorIDHex != actorHex ||
			mbl[0].peerDomain != "peer.example" ||
			!bytes.Equal(mbl[0].keyPackage, []byte("peer-kp-bytes")) {
			t.Fatalf("cross-nest classification: %+v", mbl)
		}
	})

	t.Run("crossNestMailReachableToEmail", func(t *testing.T) {
		// An off-box attendee the resolver classifies "email" (external, or a
		// mail-enabled Fauna handle on the peer nest) stays on the email rail.
		stub := func(string) (mailfauna.AttendeeTransport, error) {
			return mailfauna.AttendeeTransport{Rail: "email"}, nil
		}
		email, mbl := withStub(stub, &mockCaller{}, "ext@peer.example")
		if len(mbl) != 0 || len(email) != 1 || email[0] != "ext@peer.example" {
			t.Fatalf("cross-nest email: email=%v mailboxLess=%v", email, mbl)
		}
	})

	t.Run("crossNestDiscoveryErrorDegradesToEmail", func(t *testing.T) {
		// A discovery fault (the resolver Err) degrades to email rather than
		// dropping the attendee — backward-compatible best-effort.
		stub := func(string) (mailfauna.AttendeeTransport, error) {
			return mailfauna.AttendeeTransport{}, errors.New("discovery boom")
		}
		email, mbl := withStub(stub, &mockCaller{}, "who@peer.example")
		if len(mbl) != 0 || len(email) != 1 || email[0] != "who@peer.example" {
			t.Fatalf("cross-nest discovery error: email=%v mailboxLess=%v", email, mbl)
		}
	})

	t.Run("crossNestSealedButNoKeyPackageToEmail", func(t *testing.T) {
		// A "sealed" verdict whose recipient has no fetchable key package (not
		// reachable over MLS) degrades to the email rail.
		stub := func(string) (mailfauna.AttendeeTransport, error) {
			return mailfauna.AttendeeTransport{Rail: "sealed", ActorIDHex: strings.Repeat("cd", 32), NestURL: "https://peer.example"}, nil
		}
		caller := &mockCaller{keypackageBytes: nil}
		email, mbl := withStub(stub, caller, "nokp@peer.example")
		if len(mbl) != 0 || len(email) != 1 || email[0] != "nokp@peer.example" {
			t.Fatalf("cross-nest no-KP: email=%v mailboxLess=%v", email, mbl)
		}
	})

	t.Run("allExternalRosterFiresNoDiscovery", func(t *testing.T) {
		// A genuinely external attendee still rides email — but the discovery DOES
		// fire for it (an off-box domain may host a mailbox-less Fauna nest). The
		// "email" verdict keeps it on the email rail; assert no KP fetch happened.
		var calls int
		stub := func(string) (mailfauna.AttendeeTransport, error) {
			calls++
			return mailfauna.AttendeeTransport{Rail: "email"}, nil
		}
		caller := &mockCaller{}
		email, mbl := withStub(stub, caller, "bob@example.net")
		if len(mbl) != 0 || len(email) != 1 {
			t.Fatalf("external: email=%v mailboxLess=%v", email, mbl)
		}
		if calls != 1 {
			t.Errorf("classifyTransport fired %d times, want 1", calls)
		}
		if n := len(caller.callsOf(wsrpc.MethodKeypackageFetch)); n != 0 {
			t.Errorf("keypackage.fetch fired %d times for an email-rail attendee, want 0", n)
		}
	})
}

func putValidEventOK(t *testing.T) *mockCaller {
	t.Helper()
	caller := putAuthedCaller(t)
	caller.putEventOutcome = wsrpc.PutEventCreated
	caller.putEventETag = "etag-autosched"
	caller.putEventID = []byte("event-id-autosched-pad-32-byte00")
	return caller
}

// TestPutOrganizerFansOutImipRequest pins the gateway happy path: an organizer
// PUT with an email-reachable attendee enqueues exactly one iMIP REQUEST, AS the
// organizer (caller-scoped on_behalf_of_actor), excluding the organizer from the
// recipients (caldav-server.md § Server-side auto-schedule).
func TestPutOrganizerFansOutImipRequest(t *testing.T) {
	caller := putValidEventOK(t)
	baseURL, stop := startServer(t, caller)
	defer stop()

	req := putRequest(t, eventURL(baseURL, testCalendarID, "organizer-event"), testOrganizerEvent, "")
	resp, err := httpsClient().Do(req)
	if err != nil {
		t.Fatalf("Do: %v", err)
	}
	defer resp.Body.Close()
	io.Copy(io.Discard, resp.Body)
	if resp.StatusCode != http.StatusCreated {
		t.Fatalf("status = %d, want %d", resp.StatusCode, http.StatusCreated)
	}

	enqueues := caller.callsOf(wsrpc.MethodEnqueueOutboundMail)
	if len(enqueues) != 1 {
		t.Fatalf("enqueue_outbound_mail fired %d times, want 1", len(enqueues))
	}
	var got enqueueOutboundDecoded
	if err := cbor.Unmarshal(enqueues[0].body, &got); err != nil {
		t.Fatalf("decode enqueue body: %v", err)
	}
	if got.OriginalSender != "alice@example.com" {
		t.Errorf("original_sender = %q, want alice@example.com", got.OriginalSender)
	}
	if len(got.Recipients) != 1 || got.Recipients[0] != "bob@example.com" {
		t.Errorf("recipients = %v, want [bob@example.com] (organizer excluded)", got.Recipients)
	}
	if got.OnBehalfOfActor == nil || !bytes.Equal(*got.OnBehalfOfActor, fixtureActorID) {
		t.Errorf("on_behalf_of_actor = %x, want %x (the AUTH'd organizer)", got.OnBehalfOfActor, fixtureActorID)
	}
	if !bytes.Contains(got.RawMessage, []byte("METHOD:REQUEST")) {
		t.Errorf("raw_message is not an iMIP REQUEST: %q", got.RawMessage)
	}
}

// TestPutNonOrganizerDoesNotFanOut confirms a PUT where the AUTH'd actor is a
// mere ATTENDEE (the ORGANIZER is someone else) does NOT enqueue an iMIP — only
// the organizer fans out, so syncing an attendee's own copy never re-invites.
func TestPutNonOrganizerDoesNotFanOut(t *testing.T) {
	caller := putValidEventOK(t)
	baseURL, stop := startServer(t, caller)
	defer stop()

	req := putRequest(t, eventURL(baseURL, testCalendarID, "attendee-event"), testAttendeeOnlyEvent, "")
	resp, err := httpsClient().Do(req)
	if err != nil {
		t.Fatalf("Do: %v", err)
	}
	defer resp.Body.Close()
	io.Copy(io.Discard, resp.Body)
	if resp.StatusCode != http.StatusCreated {
		t.Fatalf("status = %d, want %d", resp.StatusCode, http.StatusCreated)
	}

	if n := len(caller.callsOf(wsrpc.MethodEnqueueOutboundMail)); n != 0 {
		t.Errorf("enqueue_outbound_mail fired %d times for a non-organizer PUT, want 0", n)
	}
}

// TestPutAttendeeAnswerRepliesToOrganizer pins the "Responding" half of
// caldav-server.md § Server-side auto-schedule: an attendee's calendar app
// answers an invitation by re-storing the event with its own PARTSTAT set, and
// that PUT owes the organizer (carol) an iMIP REPLY, sent as the attendee and
// carrying only the attendee's own line.
func TestPutAttendeeAnswerRepliesToOrganizer(t *testing.T) {
	caller := putValidEventOK(t)
	baseURL, stop := startServer(t, caller)
	defer stop()

	answered := strings.Replace(testAttendeeOnlyEvent,
		"ATTENDEE;PARTSTAT=NEEDS-ACTION:mailto:alice@example.com",
		"ATTENDEE;PARTSTAT=ACCEPTED:mailto:alice@example.com", 1)
	req := putRequest(t, eventURL(baseURL, testCalendarID, "attendee-answer"), answered, "")
	resp, err := httpsClient().Do(req)
	if err != nil {
		t.Fatalf("Do: %v", err)
	}
	defer resp.Body.Close()
	io.Copy(io.Discard, resp.Body)
	if resp.StatusCode != http.StatusCreated {
		t.Fatalf("status = %d, want %d", resp.StatusCode, http.StatusCreated)
	}

	enqueues := caller.callsOf(wsrpc.MethodEnqueueOutboundMail)
	if len(enqueues) != 1 {
		t.Fatalf("enqueue_outbound_mail fired %d times, want 1 REPLY", len(enqueues))
	}
	var got enqueueOutboundDecoded
	if err := cbor.Unmarshal(enqueues[0].body, &got); err != nil {
		t.Fatalf("decode enqueue body: %v", err)
	}
	if got.OriginalSender != "alice@example.com" {
		t.Errorf("original_sender = %q, want the answering attendee alice@example.com", got.OriginalSender)
	}
	if len(got.Recipients) != 1 || got.Recipients[0] != "carol@example.com" {
		t.Errorf("recipients = %v, want [carol@example.com] (the organizer)", got.Recipients)
	}
	if got.OnBehalfOfActor == nil || !bytes.Equal(*got.OnBehalfOfActor, fixtureActorID) {
		t.Errorf("on_behalf_of_actor = %x, want %x (the AUTH'd attendee)", got.OnBehalfOfActor, fixtureActorID)
	}
	for _, want := range []string{"METHOD:REPLY", "PARTSTAT=ACCEPTED", "UID:autosched-uid-002"} {
		if !bytes.Contains(got.RawMessage, []byte(want)) {
			t.Errorf("raw_message lacks %q: %q", want, got.RawMessage)
		}
	}
}

// TestPutOrganizerNoAttendeesNoFanOut confirms an organizer PUT with no
// email-reachable attendee is a no-op (the gateway gets a nil iMIP message).
func TestPutOrganizerNoAttendeesNoFanOut(t *testing.T) {
	caller := putValidEventOK(t)
	baseURL, stop := startServer(t, caller)
	defer stop()

	// testPutValidEvent has neither ORGANIZER nor ATTENDEE.
	req := putRequest(t, eventURL(baseURL, testCalendarID, "no-roster"), testPutValidEvent, "")
	resp, err := httpsClient().Do(req)
	if err != nil {
		t.Fatalf("Do: %v", err)
	}
	defer resp.Body.Close()
	io.Copy(io.Discard, resp.Body)
	if resp.StatusCode != http.StatusCreated {
		t.Fatalf("status = %d, want %d", resp.StatusCode, http.StatusCreated)
	}

	if n := len(caller.callsOf(wsrpc.MethodEnqueueOutboundMail)); n != 0 {
		t.Errorf("enqueue_outbound_mail fired %d times for an event with no roster, want 0", n)
	}
}
