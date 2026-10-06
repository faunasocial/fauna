package imap

import (
	"context"
	"errors"
	"time"

	"github.com/faunasocial/fauna/bins/fauna-bridges/internal/wsrpc"
)

// subscribeRPCTimeout caps SUBSCRIBE / UNSUBSCRIBE round-trips. Each
// is a single-row write against `bridge_imap_subscriptions`; 5 s is
// generous.
const subscribeRPCTimeout = 5 * time.Second

// Subscribe implements emersion/go-imap's Session.Subscribe. Maps to
// `fauna.bridges.subscribe_mailbox`. Idempotent and tolerant of a
// not-yet-existing mailbox per RFC 9051 §6.3.7 — no special outcome
// branching, success at the wire is success at IMAP.
func (s *Session) Subscribe(mailbox string) error {
	s.mu.Lock()
	actorID := s.actorID
	s.mu.Unlock()
	if actorID == nil {
		return errors.New("imap: SUBSCRIBE requires an authenticated session")
	}
	ctx, cancel := context.WithTimeout(context.Background(), subscribeRPCTimeout)
	defer cancel()
	return wsrpc.SubscribeMailbox(ctx, s.client, actorID, mailbox)
}

// Unsubscribe implements emersion/go-imap's Session.Unsubscribe. Maps
// to `fauna.bridges.unsubscribe_mailbox`. Idempotent per RFC 9051
// §6.3.8 — UNSUBSCRIBE on a not-subscribed mailbox succeeds.
func (s *Session) Unsubscribe(mailbox string) error {
	s.mu.Lock()
	actorID := s.actorID
	s.mu.Unlock()
	if actorID == nil {
		return errors.New("imap: UNSUBSCRIBE requires an authenticated session")
	}
	ctx, cancel := context.WithTimeout(context.Background(), subscribeRPCTimeout)
	defer cancel()
	return wsrpc.UnsubscribeMailbox(ctx, s.client, actorID, mailbox)
}
