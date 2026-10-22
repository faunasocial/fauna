package mta

import (
	"errors"
	"io"
	"net"
	"sync"
	"time"
)

// maxInFlightDataBytes bounds the DATA bytes every port-25 session together may
// hold in memory at once (smtp-server.md § Connection-time limits). Each DATA
// is read whole before parse, verify and scan, and copied again on the way to
// the nest, so without it the 100-connection cap times the per-message size
// cap (50 MB, 250 MB at the admin maximum) is 5–25 GB before the 64-scan cap
// applies. Past it a message is told to retry later (451 4.3.1, not
// tarpitted): the sending MTA's retry curve absorbs that as it absorbs a
// scanner outage. 256 MiB keeps a single message at the admin maximum
// admissible and holds the raw buffers, whose downstream copies are a small
// multiple of them, well inside a small VPS. Hard-coded: internal resource
// tuning, not a human-chosen knob.
const maxInFlightDataBytes = 256 << 20

// dataSourceShareDivisor sets one source's share of the budget (limit/8, 32
// MiB in production): a source — an IPv4 address or an IPv6 /64, loopback
// exempt — holds at most that much across its concurrent DATA transactions,
// so one stranger fanning trickled DATA over its 50 connections cannot defer
// every other sender's mail. A source's sole transaction may pass
// the share, so one message at the size cap stays admissible; while it is
// past the share the source's other transactions get nothing. Hard-coded:
// internal anti-abuse tuning, not a human-chosen knob.
const dataSourceShareDivisor = 8

// dataDeadlineGrace and dataMinBytesPerSecond bound a whole DATA transfer:
// after the grace a transfer must have averaged the minimum rate since DATA
// began, or it is dropped (421 4.4.2) and its charge returned. The per-read
// timeout alone let a byte every < 30 s hold a reservation for as long as the
// sender liked. 16 KiB/s is far below any mail server's link, and
// the check runs after each read, which the per-read timeout returns within
// 30 s. Hard-coded: internal anti-abuse tuning, not a human-chosen knob.
const (
	dataDeadlineGrace     = 2 * time.Minute
	dataMinBytesPerSecond = 16 << 10
)

// dataReadChunk is the smallest step readDataWithinBudget grows its buffer by,
// so a message is charged as it arrives rather than at the size cap.
const dataReadChunk = 64 << 10

var (
	// errDataBudgetFull: the process-wide budget has no room (451 4.3.1).
	errDataBudgetFull = errors.New("in-flight DATA byte budget full")
	// errDataSourceShareFull: this source holds its share already (451 4.3.1).
	errDataSourceShareFull = errors.New("source's share of the in-flight DATA byte budget full")
	// errDataTooSlow: the transfer fell behind the whole-DATA deadline (421 4.4.2).
	errDataTooSlow = errors.New("DATA transfer below the minimum sustained rate")
)

// dataBudget is the process-wide in-flight DATA byte budget, constructed once
// on inboundBackend and shared by pointer into every session, like scanGate.
// A nil *dataBudget is valid and means "unbudgeted" (unit tests that build a
// bare session).
type dataBudget struct {
	mu      sync.Mutex
	limit   int64
	share   int64
	used    int64
	sources map[string]*sourceCharges
	// now is the whole-DATA deadline's clock; tests swap in a fake.
	now func() time.Time
}

// sourceCharges is one source's bytes held and DATA transactions open; the
// entry goes when its last transaction closes, so the map is bounded by the
// connection caps.
type sourceCharges struct {
	held int64
	open int
}

func newDataBudget(limit int64) *dataBudget {
	return &dataBudget{
		limit:   limit,
		share:   limit / dataSourceShareDivisor,
		sources: make(map[string]*sourceCharges),
		now:     time.Now,
	}
}

// dataCharge is one DATA transaction's hold on the budget, from open to close.
type dataCharge struct {
	b      *dataBudget
	source string
	held   int64
	start  time.Time
	closed bool
}

// open starts a transaction for source (dataShareKey; "" is exempt from the
// share). On a nil budget the charge is unbudgeted and has no deadline.
func (b *dataBudget) open(source string) *dataCharge {
	if b == nil {
		return &dataCharge{}
	}
	b.mu.Lock()
	defer b.mu.Unlock()
	if source != "" {
		sc := b.sources[source]
		if sc == nil {
			sc = &sourceCharges{}
			b.sources[source] = sc
		}
		sc.open++
	}
	return &dataCharge{b: b, source: source, start: b.now()}
}

// grow charges n more bytes, or returns errDataBudgetFull /
// errDataSourceShareFull and charges nothing.
func (c *dataCharge) grow(n int64) error {
	b := c.b
	if b == nil {
		c.held += n
		return nil
	}
	b.mu.Lock()
	defer b.mu.Unlock()
	if b.used+n > b.limit {
		return errDataBudgetFull
	}
	var sc *sourceCharges
	if c.source != "" {
		sc = b.sources[c.source]
		// Past the share only as the source's sole transaction, or as the one
		// that already passed it so.
		if sc.held+n > b.share && sc.open > 1 && c.held <= b.share {
			return errDataSourceShareFull
		}
		sc.held += n
	}
	b.used += n
	c.held += n
	return nil
}

// tooSlow reports whether the transfer, holding received bytes, has fallen
// behind the whole-DATA deadline.
func (c *dataCharge) tooSlow(received int) bool {
	if c.b == nil {
		return false
	}
	allowed := dataDeadlineGrace + time.Duration(int64(received)*int64(time.Second)/dataMinBytesPerSecond)
	return c.b.now().Sub(c.start) > allowed
}

// close returns everything the transaction holds; it is idempotent.
func (c *dataCharge) close() {
	b := c.b
	if b == nil || c.closed {
		return
	}
	c.closed = true
	b.mu.Lock()
	defer b.mu.Unlock()
	b.used -= c.held
	if c.source != "" {
		sc := b.sources[c.source]
		sc.held -= c.held
		sc.open--
		if sc.open == 0 {
			delete(b.sources, c.source)
		}
	}
	c.held = 0
}

// inUse reports the bytes currently charged (tests and the budget's own log
// line).
func (b *dataBudget) inUse() int64 {
	if b == nil {
		return 0
	}
	b.mu.Lock()
	defer b.mu.Unlock()
	return b.used
}

// dataShareKey is the source a port-25 peer's DATA is charged to: the IPv4
// address, or the IPv6 /64 (one host is routinely handed a whole /64). Loopback
// — the in-container dials — is exempt ("") as for the per-IP connection caps;
// an address that does not parse is still charged, under its raw text.
func dataShareKey(clientIP string) string {
	ip := net.ParseIP(clientIP)
	if ip == nil {
		return "unparsed:" + clientIP
	}
	if ip.IsLoopback() {
		return ""
	}
	if v4 := ip.To4(); v4 != nil {
		return v4.String()
	}
	return ip.Mask(net.CIDRMask(64, 128)).String() + "/64"
}

// readDataWithinBudget reads r to EOF, or to limit+1 bytes when limit > 0 (the
// caller's size cap answers 552 past limit), into a buffer it grows itself —
// by half its size, at least dataReadChunk, never past limit+1 — charging each
// growth to charge before allocating it, so the charge is the memory the
// buffer really holds. It returns errDataBudgetFull or errDataSourceShareFull
// when a growth did not fit and errDataTooSlow when the transfer fell behind
// the whole-DATA deadline; the caller closes charge in every case.
func readDataWithinBudget(r io.Reader, limit int64, charge *dataCharge) ([]byte, error) {
	if limit > 0 {
		r = io.LimitReader(r, limit+1)
	}
	var raw []byte
	for {
		if len(raw) == cap(raw) {
			step := int64(max(cap(raw)/2, dataReadChunk))
			if limit > 0 {
				step = min(step, limit+1-int64(cap(raw)))
			}
			if step <= 0 {
				// limit+1 bytes held: the LimitReader has nothing left.
				return raw, nil
			}
			if err := charge.grow(step); err != nil {
				return nil, err
			}
			grown := make([]byte, len(raw), int64(cap(raw))+step)
			copy(grown, raw)
			raw = grown
		}
		n, rerr := r.Read(raw[len(raw):cap(raw)])
		raw = raw[:len(raw)+n]
		if rerr == io.EOF {
			return raw, nil
		}
		if rerr != nil {
			return nil, rerr
		}
		if charge.tooSlow(len(raw)) {
			return nil, errDataTooSlow
		}
	}
}
