package imap

import (
	"log/slog"
	"sync"
	"time"

	"github.com/emersion/go-imap/v2"
	"github.com/faunasocial/fauna/bins/fauna-bridges/internal/dagcbor"
	"github.com/faunasocial/fauna/bins/fauna-bridges/internal/wsrpc"
)

// liveSessions indexes the Backend's AUTH'd Sessions by actor, so a
// `fauna.bridges.account_revoked` push can end every session of an account
// the moment it stops standing (`docs/goal/architecture/transport-connection.md`
// § Revocation teardown, the bridge-held sessions paragraph).
//
// The nest refuses every serving call for such an account, so a session that
// makes a call meets the refusal and ends with a BYE (revoked.go). The push is
// for the sessions that make no call: one in IDLE, or one sitting between
// commands — each holds the account's unwrapped MLS capability in this
// process's memory until it ends.
//
// A Session joins at its AUTH success and leaves at Close. Lock order: a
// Session's own mu, then this mu (Close); never this mu while taking a
// Session's (endActor copies the set out first).
type liveSessions struct {
	mu      sync.Mutex
	byActor map[string]map[*Session]struct{}
}

func newLiveSessions() *liveSessions {
	return &liveSessions{byActor: make(map[string]map[*Session]struct{})}
}

func (l *liveSessions) add(actorID []byte, s *Session) {
	l.mu.Lock()
	defer l.mu.Unlock()
	set := l.byActor[string(actorID)]
	if set == nil {
		set = make(map[*Session]struct{})
		l.byActor[string(actorID)] = set
	}
	set[s] = struct{}{}
}

func (l *liveSessions) remove(actorID []byte, s *Session) {
	l.mu.Lock()
	defer l.mu.Unlock()
	set := l.byActor[string(actorID)]
	delete(set, s)
	if len(set) == 0 {
		delete(l.byActor, string(actorID))
	}
}

func (l *liveSessions) of(actorID []byte) []*Session {
	l.mu.Lock()
	defer l.mu.Unlock()
	set := l.byActor[string(actorID)]
	out := make([]*Session, 0, len(set))
	for s := range set {
		out = append(out, s)
	}
	return out
}

// endActor ends every live session AUTH'd as actorID; returns how many.
func (l *liveSessions) endActor(actorID []byte) int {
	sessions := l.of(actorID)
	for _, s := range sessions {
		s.endForRevocation()
	}
	return len(sessions)
}

// accountRevokedHandler is the wsrpc.PushHandler for PushKindAccountRevoked.
// It runs on the wsrpc reader goroutine, so it only decodes and hands the
// teardown to its own goroutine: ending a session takes the Session's mu, and
// Session.Close holds that mu across a nest round-trip whose reply this very
// reader goroutine delivers.
func accountRevokedHandler(live *liveSessions, logger *slog.Logger) wsrpc.PushHandler {
	return func(kind string, payload []byte, seq uint64) {
		if kind != wsrpc.PushKindAccountRevoked {
			return
		}
		push, err := dagcbor.Unmarshal[wsrpc.BridgeAccountRevokedPush](payload)
		if err != nil || len(push.ActorID) == 0 {
			logger.Warn("imap: account_revoked push undecodable", "seq", seq, "err", err)
			return
		}
		actorID := append([]byte(nil), push.ActorID...)
		go func() {
			if n := live.endActor(actorID); n > 0 {
				logger.Info("imap: ended the sessions of an account that no longer stands",
					"sessions", n, "seq", seq)
			}
		}()
	}
}

// revokedChLocked returns the channel closed when this session's account stops
// standing, creating it on first use (a hand-built test Session has none).
// Callers hold s.mu.
func (s *Session) revokedChLocked() chan struct{} {
	if s.revoked == nil {
		s.revoked = make(chan struct{})
	}
	return s.revoked
}

// endForRevocation ends this session because its account stopped standing.
// A session in IDLE is woken and answers `* BYE` (the IDLE loop's revoked
// arm, wakeIdleRead); any other is disconnected at once by closing its
// connection — whatever command it is running fails, and the server's
// teardown runs Close, which zeroizes the capability. Idempotent.
func (s *Session) endForRevocation() {
	s.mu.Lock()
	ch := s.revokedChLocked()
	if !s.revokedClosed {
		s.revokedClosed = true
		close(ch)
	}
	idling := s.idling
	conn := s.conn
	s.mu.Unlock()
	if idling {
		return
	}
	if conn != nil {
		if nc := conn.NetConn(); nc != nil {
			_ = nc.Close()
		}
	}
}

// idleWakeRetry / idleWakeGiveUp bound wakeIdleRead's retries.
const (
	idleWakeRetry  = 20 * time.Millisecond
	idleWakeGiveUp = 2 * time.Second
)

// wakeIdleRead ends an IDLE from the server side. While a session idles the
// IMAP server is blocked reading the client's `DONE`, and the result of a
// returned Idle is written only after that read returns — so the IDLE loop
// returning alone says nothing until the client speaks. The vendored server
// ends an IDLE whose read deadline expires with `* BYE` and a close (the
// per-server IDLE timeout, third_party/go-imap/FORK.md row 19), so this moves
// that deadline to now. The server sets its own deadline when the IDLE
// starts, which may land after ours, so the move is repeated until the read
// returns (stop closes); past idleWakeGiveUp the connection is closed outright.
func (s *Session) wakeIdleRead(stop <-chan struct{}) {
	s.mu.Lock()
	conn := s.conn
	s.mu.Unlock()
	if conn == nil {
		return
	}
	nc := conn.NetConn()
	if nc == nil {
		return
	}
	giveUp := time.Now().Add(idleWakeGiveUp)
	for {
		_ = nc.SetReadDeadline(time.Now())
		select {
		case <-stop:
			return
		case <-time.After(idleWakeRetry):
		}
		if time.Now().After(giveUp) {
			_ = nc.Close()
			return
		}
	}
}

// errSessionRevoked is what the IDLE loop returns when the account stops
// standing mid-IDLE: an IMAP `BYE`, on which the server ends the connection
// (the same shape revoked.go's refusal takes).
var errSessionRevoked = &imap.Error{
	Type: imap.StatusResponseTypeBye,
	Text: "This account is locked or suspended",
}
