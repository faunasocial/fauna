// Package atprotocrawl announces this PDS to the network's relays with
// com.atproto.sync.requestCrawl — the call that makes a self-hosted PDS visible
// at all: a relay that has never heard of a host never subscribes to its
// firehose, so nothing the repo serves reaches an AppView (atproto-pds-bridge.md
// § Goal, § Where logic lives — `requestCrawl` is the Go bridge's;
// atproto-pds-full.md § Ecosystem reality — relay crawling is open, no
// allowlist).
//
// When: after a commit lands, never at an arbitrary time. Every producer that
// appends to the firehose outbox passes the first-emit gate
// (atprotorepo.ErrFirstEmitGated), so a host is announced only once one of its
// identities resolves ecosystem-side — announcing earlier would invite the
// first-impression trap the gate exists to prevent. The bridge wires
// [Announcer.Notify] into the funnel's post-commit hook, beside the firehose
// broadcaster's own wake-up, and calls it once at boot when the store already
// holds repos (a relay may have dropped a host that was down).
//
// How often: once per relay per [MinInterval] at most, with one trailing
// announce when commits kept landing inside the window — the relay, once
// subscribed, follows the firehose itself, so a re-announce is only a "still
// here". A refused or failed call retries after [RetryInterval] on its own,
// without waiting for the next commit.
//
// Which relays: [DefaultRelays], a hard-coded product constant — nobody chooses
// it (§ Product invariants: the only configuration surface is the apps). The
// e2e flavor redirects it to a fake relay through the bridge's fixture seam.
package atprotocrawl

import (
	"bytes"
	"context"
	"encoding/json"
	"fmt"
	"io"
	"log/slog"
	"net/http"
	"strings"
	"time"
)

// DefaultRelays are the relays a production bridge announces to: Bluesky's
// public relay, which feeds the AppView every Bluesky app reads.
var DefaultRelays = []string{"https://bsky.network"}

const (
	// MinInterval spaces successful announces to one relay.
	MinInterval = 20 * time.Minute
	// RetryInterval is the wait after a failed announce before trying again.
	RetryInterval = time.Minute
	// requestTimeout bounds one requestCrawl call.
	requestTimeout = 30 * time.Second
	// maxErrorBody caps how much of a refusal's body reaches the log.
	maxErrorBody = 512
)

// Announcer sends requestCrawl for one hostname to a fixed set of relays.
type Announcer struct {
	hostname string
	relays   []string
	client   *http.Client
	logger   *slog.Logger
	wake     chan struct{}

	// Overridable by tests in this package.
	now           func() time.Time
	minInterval   time.Duration
	retryInterval time.Duration
}

// New builds an announcer for hostname (the PDS host, no scheme or port). A nil
// client gets a plain one with a request timeout. With no relays it is inert:
// Notify and Run do nothing.
func New(hostname string, relays []string, client *http.Client, logger *slog.Logger) *Announcer {
	if client == nil {
		client = &http.Client{Timeout: requestTimeout}
	}
	if logger == nil {
		logger = slog.Default()
	}
	trimmed := make([]string, 0, len(relays))
	for _, r := range relays {
		if r = strings.TrimRight(strings.TrimSpace(r), "/"); r != "" {
			trimmed = append(trimmed, r)
		}
	}
	return &Announcer{
		hostname:      hostname,
		relays:        trimmed,
		client:        client,
		logger:        logger,
		wake:          make(chan struct{}, 1),
		now:           time.Now,
		minInterval:   MinInterval,
		retryInterval: RetryInterval,
	}
}

// Notify asks for an announce. It never blocks; notifications that arrive
// while one is already pending collapse into it.
func (a *Announcer) Notify() {
	select {
	case a.wake <- struct{}{}:
	default:
	}
}

// Run serves notifications until ctx ends.
func (a *Announcer) Run(ctx context.Context) {
	if len(a.relays) == 0 || a.hostname == "" {
		return
	}
	pending := make(map[string]bool, len(a.relays))
	next := make(map[string]time.Time, len(a.relays))
	var timer *time.Timer
	var fire <-chan time.Time
	for {
		select {
		case <-ctx.Done():
			if timer != nil {
				timer.Stop()
			}
			return
		case <-a.wake:
			for _, r := range a.relays {
				pending[r] = true
			}
		case <-fire:
		}
		now := a.now()
		var soonest time.Time
		for _, r := range a.relays {
			if !pending[r] {
				continue
			}
			if now.Before(next[r]) {
				if soonest.IsZero() || next[r].Before(soonest) {
					soonest = next[r]
				}
				continue
			}
			if err := a.announce(ctx, r); err != nil {
				a.logger.Warn("atproto requestCrawl failed; retrying",
					"relay", r, "hostname", a.hostname, "retry_in", a.retryInterval, "err", err)
				next[r] = now.Add(a.retryInterval)
				if soonest.IsZero() || next[r].Before(soonest) {
					soonest = next[r]
				}
				continue
			}
			a.logger.Info("atproto requestCrawl sent", "relay", r, "hostname", a.hostname)
			pending[r] = false
			next[r] = now.Add(a.minInterval)
		}
		if timer != nil {
			timer.Stop()
			timer, fire = nil, nil
		}
		if !soonest.IsZero() {
			timer = time.NewTimer(soonest.Sub(now))
			fire = timer.C
		}
	}
}

// announce sends one com.atproto.sync.requestCrawl to relay.
func (a *Announcer) announce(ctx context.Context, relay string) error {
	body, err := json.Marshal(map[string]string{"hostname": a.hostname})
	if err != nil {
		return err
	}
	ctx, cancel := context.WithTimeout(ctx, requestTimeout)
	defer cancel()
	req, err := http.NewRequestWithContext(ctx, http.MethodPost,
		relay+"/xrpc/com.atproto.sync.requestCrawl", bytes.NewReader(body))
	if err != nil {
		return err
	}
	req.Header.Set("Content-Type", "application/json")
	resp, err := a.client.Do(req)
	if err != nil {
		return err
	}
	defer resp.Body.Close()
	if resp.StatusCode/100 != 2 {
		snippet, _ := io.ReadAll(io.LimitReader(resp.Body, maxErrorBody))
		return fmt.Errorf("relay answered %d: %s", resp.StatusCode, strings.TrimSpace(string(snippet)))
	}
	_, _ = io.Copy(io.Discard, io.LimitReader(resp.Body, maxErrorBody))
	return nil
}
