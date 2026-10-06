// Package atprotofirehose serves com.atproto.sync.subscribeRepos — the live
// event stream a relay consumes to learn that a projected repo changed
// (atproto-pds-bridge.md § Architecture; Sync v1.1 per atproto-pds-full.md
// § Ecosystem reality).
//
// Shape: the commit funnel persists each already-serialized frame to the
// firehose_events outbox in the same txn as the repo head (atprotorepo, C1), and
// exactly ONE emitter goroutine here drains that outbox in seq order and fans
// frames out to connected subscribers. Nothing re-encodes a frame and nothing
// broadcasts out of order, because the outbox is the only ordering authority.
//
// The route registers on F1's single XRPC route table as Public /
// ClassPublicRead (C5 — never a second table); the WS upgrade happens INSIDE the
// handler, so the frame's per-IP rate limit runs before any upgrade.
//
// A subscriber that cannot keep up is disconnected rather than buffered without
// bound — the standard firehose contract: reconnect with a cursor and replay.
//
// The broadcaster is frame-TYPE agnostic: it forwards whatever the outbox holds,
// so #commit today and #identity (task 8's rename hooks) or #account (S4's
// layer-2 disable ratification) need no change here — only a producer that
// writes the row.
package atprotofirehose

import (
	"context"
	"errors"
	"log/slog"
	"sync"
	"time"

	"github.com/faunasocial/fauna/bins/fauna-bridges/internal/atprotorepo"
)

const (
	// pollInterval is the backstop drain tick. The funnel notifies the
	// broadcaster in-process after every commit (Funnel.SetOnCommit), so this
	// only covers a dropped notification.
	pollInterval = 10 * time.Second
	// pruneInterval is how often aged-out outbox rows are swept.
	pruneInterval = time.Hour
	// drainBatch bounds one outbox read.
	drainBatch = 512
	// subscriberBuffer is how many frames a single subscriber may fall behind
	// before it is dropped as too slow.
	subscriberBuffer = 256
	// maxSubscribers caps CONCURRENT subscribers across the whole PDS, and
	// maxSubscribersPerIP caps them per client IP.
	//
	// The route's ClassPublicRead limiter bounds how fast connections are
	// ESTABLISHED, not how many are held — and a firehose connection is held
	// open indefinitely by design, so without a count cap one IP ramps to
	// thousands of live goroutines/fds. The too-slow drop does not help: on a
	// quiet PDS the 256-frame buffer never fills, so a silent connection is
	// never dropped.
	//
	// Hard-coded, not configurable: nobody chooses these (§ Product invariants
	// — the only configuration surface is the apps). Sized well above any
	// legitimate consumer set (a handful of relays, mirrors and debugging
	// clients) and well below the resource wall.
	maxSubscribers      = 256
	maxSubscribersPerIP = 8
)

// ErrTooManySubscribers is returned by subscribe when admitting the connection
// would exceed the global or per-IP concurrent-subscriber cap. The handler
// turns it into an XRPC 429 BEFORE the WebSocket upgrade, so a refused consumer
// costs one cheap HTTP reply rather than a held connection.
var ErrTooManySubscribers = errors.New("too many concurrent firehose subscribers")

// Broadcaster drains the firehose outbox and fans frames out to subscribers.
// Construct with New, run exactly one Run per process.
type Broadcaster struct {
	store  *atprotorepo.Store
	logger *slog.Logger

	nudge chan struct{}

	mu      sync.Mutex
	subs    map[*subscriber]struct{}
	perIP   map[string]int // live subscriber count keyed by client IP
	lastSeq int64          // highest seq broadcast so far
	seeded  bool           // Run has seeded lastSeq from the outbox head
}

type subscriber struct {
	// ip is the client IP this subscriber was admitted under, so unsubscribe
	// releases the same per-IP slot subscribe took.
	ip string
	ch chan atprotorepo.FirehoseEvent
	// dropped is closed when the subscriber fell too far behind; the handler
	// sees it and closes the connection with a too-slow status.
	dropped chan struct{}
	once    sync.Once
}

// New builds a broadcaster over store.
func New(store *atprotorepo.Store, logger *slog.Logger) *Broadcaster {
	if logger == nil {
		logger = slog.Default()
	}
	return &Broadcaster{
		store:  store,
		logger: logger,
		nudge:  make(chan struct{}, 1),
		subs:   make(map[*subscriber]struct{}),
		perIP:  make(map[string]int),
	}
}

// Notify wakes the emitter. Non-blocking and lossless in effect: the emitter
// always re-reads the outbox from its own cursor, so a coalesced notification
// still delivers every frame.
func (b *Broadcaster) Notify() {
	select {
	case b.nudge <- struct{}{}:
	default:
	}
}

// Run is the single emitter goroutine: seed the cursor at the current head, then
// drain on every nudge/tick until ctx ends. Frames already in the outbox at
// startup are history — replayable by cursor, never re-broadcast to live
// subscribers (a reconnecting relay asks for them explicitly).
func (b *Broadcaster) Run(ctx context.Context) {
	_, max, ok, err := b.store.SeqRange(ctx)
	if err != nil {
		b.logger.Warn("firehose: seed cursor failed; starting from 0", "err", err)
	}
	b.mu.Lock()
	if err == nil && ok {
		b.lastSeq = max
	}
	b.seeded = true
	b.mu.Unlock()

	poll := time.NewTicker(pollInterval)
	defer poll.Stop()
	prune := time.NewTicker(pruneInterval)
	defer prune.Stop()

	for {
		select {
		case <-ctx.Done():
			b.closeAll()
			return
		case <-b.nudge:
			b.drain(ctx)
		case <-poll.C:
			b.drain(ctx)
		case <-prune.C:
			if n, err := b.store.PruneEvents(ctx, time.Now()); err != nil {
				b.logger.Warn("firehose: prune outbox failed", "err", err)
			} else if n > 0 {
				b.logger.Info("firehose: pruned aged-out frames", "count", n)
			}
		}
	}
}

// drain reads every outbox frame past the cursor and fans it out, in seq order.
// Called ONLY from Run, so the cursor advances under a single goroutine and no
// frame is ever broadcast twice.
func (b *Broadcaster) drain(ctx context.Context) {
	for {
		b.mu.Lock()
		from := b.lastSeq
		b.mu.Unlock()

		evs, err := b.store.EventsSince(ctx, from, drainBatch)
		if err != nil {
			b.logger.Warn("firehose: read outbox failed", "after_seq", from, "err", err)
			return
		}
		if len(evs) == 0 {
			return
		}
		for _, e := range evs {
			b.fanout(e)
		}
		if len(evs) < drainBatch {
			return
		}
	}
}

// fanout delivers one frame to every subscriber and advances the cursor. A
// subscriber whose buffer is full is dropped: buffering without bound would let
// one stalled consumer pin the whole outbox in memory.
func (b *Broadcaster) fanout(e atprotorepo.FirehoseEvent) {
	b.mu.Lock()
	defer b.mu.Unlock()
	for s := range b.subs {
		select {
		case s.ch <- e:
		default:
			b.removeLocked(s)
			s.drop()
			b.logger.Warn("firehose: dropping consumer that fell behind", "seq", e.Seq)
		}
	}
	b.lastSeq = e.Seq
}

func (b *Broadcaster) closeAll() {
	b.mu.Lock()
	defer b.mu.Unlock()
	for s := range b.subs {
		b.removeLocked(s)
		s.drop()
	}
}

// subscribe admits a consumer from ip and attaches it as a live subscriber,
// reporting the cursor it is live from: every frame with seq > liveFrom will
// arrive on the channel, so the caller replays history up to liveFrom and then
// switches over with no gap and no duplicate.
//
// Admission and attachment are ONE critical section: two racing subscribes must
// not both observe room and both take the last slot.
//
// Returns ErrTooManySubscribers when the global or per-IP cap is already full.
func (b *Broadcaster) subscribe(ip string) (*subscriber, int64, error) {
	b.mu.Lock()
	defer b.mu.Unlock()
	if len(b.subs) >= maxSubscribers {
		return nil, 0, ErrTooManySubscribers
	}
	if b.perIP[ip] >= maxSubscribersPerIP {
		return nil, 0, ErrTooManySubscribers
	}
	s := &subscriber{
		ip:      ip,
		ch:      make(chan atprotorepo.FirehoseEvent, subscriberBuffer),
		dropped: make(chan struct{}),
	}
	b.subs[s] = struct{}{}
	b.perIP[ip]++
	return s, b.lastSeq, nil
}

func (b *Broadcaster) unsubscribe(s *subscriber) {
	b.mu.Lock()
	defer b.mu.Unlock()
	b.removeLocked(s)
	s.drop()
}

// removeLocked drops s from the subscriber set and releases its per-IP slot.
// Idempotent — the too-slow path and the handler's deferred unsubscribe both
// reach it for the same subscriber, and a double decrement would leak capacity
// (an IP that could then hold more than the cap allows, forever).
func (b *Broadcaster) removeLocked(s *subscriber) {
	if _, live := b.subs[s]; !live {
		return
	}
	delete(b.subs, s)
	if n := b.perIP[s.ip]; n <= 1 {
		delete(b.perIP, s.ip)
	} else {
		b.perIP[s.ip] = n - 1
	}
}

// Seeded reports whether Run has seeded its cursor from the outbox head
// (diagnostics/tests). A subscriber attached before the seed races it: frames
// committed in that window are reclassified as pre-start history and never
// fanned out. Production is safe by construction — Run starts before the
// listener binds — but a test harness that launches Run in a goroutine must
// wait for this before attaching subscribers or posting.
func (b *Broadcaster) Seeded() bool {
	b.mu.Lock()
	defer b.mu.Unlock()
	return b.seeded
}

// CursorSeq reports the highest seq broadcast so far (diagnostics/tests).
func (b *Broadcaster) CursorSeq() int64 {
	b.mu.Lock()
	defer b.mu.Unlock()
	return b.lastSeq
}

// SubscriberCount reports how many consumers are attached (diagnostics/tests).
func (b *Broadcaster) SubscriberCount() int {
	b.mu.Lock()
	defer b.mu.Unlock()
	return len(b.subs)
}

func (s *subscriber) drop() { s.once.Do(func() { close(s.dropped) }) }
