package imap

import (
	"context"
	"errors"
	"log/slog"
	"testing"

	"github.com/fxamacker/cbor/v2"

	"github.com/faunasocial/fauna/bins/fauna-bridges/internal/dagcbor"
)

// subscribeCaller is the per-file fake covering SUBSCRIBE / UNSUBSCRIBE.
// By convention, the fake is local to this file — sibling
// tests' fakes are independent so drift on one doesn't propagate.
type subscribeCaller struct {
	subscribeOutcome   map[string]any
	unsubscribeOutcome map[string]any

	subscribeErr   error
	unsubscribeErr error

	subscribeCalls   int
	unsubscribeCalls int

	capturedSubscribe   capturedSubscribeReq
	capturedUnsubscribe capturedSubscribeReq // same shape
}

type capturedSubscribeReq struct {
	ActorID []byte `cbor:"actor_id"`
	Mailbox string `cbor:"mailbox"`
}

func (c *subscribeCaller) Call(_ context.Context, method string, body, reply any) error {
	enc, err := dagcbor.Marshal(body)
	if err != nil {
		return err
	}
	switch method {
	case "fauna.bridges.subscribe_mailbox":
		c.subscribeCalls++
		_ = cbor.Unmarshal(enc, &c.capturedSubscribe)
		if c.subscribeErr != nil {
			return c.subscribeErr
		}
		rep, err := dagcbor.Marshal(c.subscribeOutcome)
		if err != nil {
			return err
		}
		return cbor.Unmarshal(rep, reply)
	case "fauna.bridges.unsubscribe_mailbox":
		c.unsubscribeCalls++
		_ = cbor.Unmarshal(enc, &c.capturedUnsubscribe)
		if c.unsubscribeErr != nil {
			return c.unsubscribeErr
		}
		rep, err := dagcbor.Marshal(c.unsubscribeOutcome)
		if err != nil {
			return err
		}
		return cbor.Unmarshal(rep, reply)
	}
	return errors.New("subscribeCaller: unexpected method " + method)
}

// subscribeSession builds the minimal AUTH'd Session for the
// subscribe/unsubscribe paths.
func subscribeSession(caller *subscribeCaller) *Session {
	return &Session{
		client:       caller,
		actorID:      fixtureActorID,
		credentialID: fixtureCredentialID,
		logger:       slog.Default(),
	}
}

func TestSubscribeForwardsActorAndMailbox(t *testing.T) {
	caller := &subscribeCaller{
		subscribeOutcome: map[string]any{"outcome": "subscribed"},
	}
	s := subscribeSession(caller)
	if err := s.Subscribe("Saved Searches"); err != nil {
		t.Fatalf("Subscribe: %v", err)
	}
	if caller.subscribeCalls != 1 {
		t.Errorf("subscribe RPC calls: got %d, want 1", caller.subscribeCalls)
	}
	if !equalBytes(caller.capturedSubscribe.ActorID, fixtureActorID) {
		t.Errorf("actor_id: got %x, want %x", caller.capturedSubscribe.ActorID, fixtureActorID)
	}
	if caller.capturedSubscribe.Mailbox != "Saved Searches" {
		t.Errorf("mailbox: got %q", caller.capturedSubscribe.Mailbox)
	}
}

// SUBSCRIBE on a not-yet-existing mailbox is legal per RFC 9051
// §6.3.7. The session method's job is to forward the request; the
// outcome distinction (existed vs. didn't) doesn't surface IMAP-side.
func TestSubscribeUnknownMailboxIsNotAnError(t *testing.T) {
	caller := &subscribeCaller{
		subscribeOutcome: map[string]any{"outcome": "subscribed"},
	}
	s := subscribeSession(caller)
	if err := s.Subscribe("not-yet-created"); err != nil {
		t.Fatalf("Subscribe: %v", err)
	}
}

func TestSubscribePropagatesError(t *testing.T) {
	caller := &subscribeCaller{
		subscribeErr: errors.New("transport"),
	}
	s := subscribeSession(caller)
	if err := s.Subscribe("INBOX"); err == nil {
		t.Fatalf("expected transport error to propagate")
	}
}

func TestSubscribeRequiresAuthenticatedSession(t *testing.T) {
	// No actorID → unauthenticated.
	s := &Session{client: &subscribeCaller{}}
	if err := s.Subscribe("INBOX"); err == nil {
		t.Fatalf("expected error on unauthenticated session")
	}
}

func TestUnsubscribeForwardsActorAndMailbox(t *testing.T) {
	caller := &subscribeCaller{
		unsubscribeOutcome: map[string]any{"outcome": "unsubscribed"},
	}
	s := subscribeSession(caller)
	if err := s.Unsubscribe("Saved Searches"); err != nil {
		t.Fatalf("Unsubscribe: %v", err)
	}
	if caller.unsubscribeCalls != 1 {
		t.Errorf("unsubscribe RPC calls: got %d, want 1", caller.unsubscribeCalls)
	}
	if !equalBytes(caller.capturedUnsubscribe.ActorID, fixtureActorID) {
		t.Errorf("actor_id: got %x, want %x", caller.capturedUnsubscribe.ActorID, fixtureActorID)
	}
	if caller.capturedUnsubscribe.Mailbox != "Saved Searches" {
		t.Errorf("mailbox: got %q", caller.capturedUnsubscribe.Mailbox)
	}
}

// Idempotency: second UNSUBSCRIBE on an already-not-subscribed
// mailbox is a no-op success per RFC 9051 §6.3.8. The session method
// forwards twice and gets the same `unsubscribed` outcome both
// times; no error on either.
func TestUnsubscribeIsIdempotentFromSessionPerspective(t *testing.T) {
	caller := &subscribeCaller{
		unsubscribeOutcome: map[string]any{"outcome": "unsubscribed"},
	}
	s := subscribeSession(caller)
	if err := s.Unsubscribe("INBOX"); err != nil {
		t.Fatalf("first Unsubscribe: %v", err)
	}
	if err := s.Unsubscribe("INBOX"); err != nil {
		t.Fatalf("second Unsubscribe: %v", err)
	}
	if caller.unsubscribeCalls != 2 {
		t.Errorf("unsubscribe RPC calls: got %d, want 2", caller.unsubscribeCalls)
	}
}

func TestUnsubscribePropagatesError(t *testing.T) {
	caller := &subscribeCaller{
		unsubscribeErr: errors.New("transport"),
	}
	s := subscribeSession(caller)
	if err := s.Unsubscribe("INBOX"); err == nil {
		t.Fatalf("expected transport error to propagate")
	}
}

func TestUnsubscribeRequiresAuthenticatedSession(t *testing.T) {
	s := &Session{client: &subscribeCaller{}}
	if err := s.Unsubscribe("INBOX"); err == nil {
		t.Fatalf("expected error on unauthenticated session")
	}
}
