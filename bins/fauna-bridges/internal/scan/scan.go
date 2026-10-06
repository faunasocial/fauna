// Package scan — the co-resident content-scanner dialers (clamd INSTREAM +
// rspamd /checkv2), shared by every bridge role.
//
// Extracted from internal/mta (the T1.4 perimeter scan gate) when the MDA's
// capability drain gained a second caller (capability-mediated-content-
// processing design § 2.5: the drain re-runs the co-resident scanners on
// sealed content it transiently unseals under a user-minted grant). clamd and
// rspamd are loopback co-resident sidecars reachable from any role on the box
// — the dialers are pure I/O with no role coupling, so they live here; the
// perimeter-only concerns (circuit breakers, in-flight cap, delivery-action
// mapping) stay in internal/mta's scan gate.
//
// Fail-closed contract: any dial / timeout / malformed-reply error is returned
// to the caller — the MTA maps it to Tempfail (never allow-without-scan,
// mail-content-scanning.md § Don't do these); the drain skips the unit (the
// obligation stays open and is retried on a later drain).
package scan

import (
	"bufio"
	"bytes"
	"context"
	"encoding/binary"
	"fmt"
	"io"
	"net"
	"net/http"
	"strings"
	"time"

	"github.com/faunasocial/fauna/bins/fauna-bridges/internal/mailfauna"
)

// Config is the bridge deployment-topology + policy bundle for the co-resident
// scanners. The clamd/rspamd **addresses** are OS-deployment topology (where
// the co-located daemons listen on this box) and come from the operator-hatch
// (mail-content-scanning.md § Compile-time decisions). The **Policy** (enabled
// flags, action-on-infected, scaling) is admin-tier nest config; today it sits
// at the compile-time default (PolicyDefault) until a future track projects it
// from the fetch_config snapshot — mirroring SpamPolicyFromSnapshot.
type Config struct {
	// ClamdAddr is a unix-socket path (leading "/") or a host:port; the
	// production default is /var/run/clamav/clamd.ctl (Linux/macOS) or
	// localhost:3310 (Windows). Dialed only when Policy.ClamavEnabled.
	ClamdAddr string
	// RspamdURL is the rspamd base URL (e.g. http://localhost:11333);
	// /checkv2 is appended. Used only when Policy.RspamdEnabled.
	RspamdURL string
	// Policy is the scan policy (PolicyDefault today).
	Policy mailfauna.ScanPolicy
	// HTTPClient dials rspamd; nil ⇒ a default client with Timeout. The
	// field exists so tests can inject an httptest.Server-backed client.
	HTTPClient *http.Client
}

const (
	// Timeout bounds each clamd / rspamd round-trip.
	//
	// Derived from the product ceiling, not from the old 50 MiB assumption: the
	// gate now scans everything the perimeter accepts, and the perimeter accepts
	// up to MAX_MESSAGE_BYTES_CEILING (mail-content-scanning.md § Oversize
	// messages — "the scan round-trip timeout is sized for that ceiling, not for
	// 50 MiB"). A timeout sized for 50 MiB would turn a legitimate large message
	// into a fail-closed 451 loop on a deployment that raised its ceiling — the
	// exact silent-failure shape the ruling exists to prevent, only one door over.
	//
	// The budget is a floor plus a per-byte allowance. The floor covers connect +
	// handshake + a wedged-but-alive daemon's first byte; the per-byte term
	// covers streaming the body and clamd's scan of it. scanBytesPerSec is
	// deliberately pessimistic (well under real clamd INSTREAM throughput on a
	// small VPS), so the timeout stays an upper bound on pathology rather than a
	// performance target: 30s + 250,000,000/5 MiB-per-s = ~78s.
	//
	// The budget is a CONSTANT, derived from the ceiling rather than from the
	// deployment's current max_message_bytes — a deployment that has not raised
	// its ceiling still gets ~78s where it used to get 30s. That is the intended
	// trade: the two guards that actually bound a wedged daemon are the per-
	// scanner circuit breaker (which stops dialling a dead socket after a few
	// failures, so the long timeout is paid once, not per message) and the 64-slot
	// in-flight cap — not this deadline. Sizing the deadline per-message off the
	// live snapshot instead would buy a shorter tail on small deployments at the
	// cost of threading a per-call value through scan.Clamd/scan.Rspamd, and the
	// ruling asks for "sized for that ceiling", not for the current setting.
	scanFloor       = 30 * time.Second
	scanBytesPerSec = 5 << 20 // 5 MiB/s
	// clamdChunkSize is the INSTREAM frame payload size.
	clamdChunkSize = 2048

	// maxClamdReplyBytes / maxRspamdReplyBytes bound the scanner reply readers
	// (previously unbounded io.ReadAll). clamd answers a single short status
	// line; rspamd a /checkv2 JSON object. A wedged/compromised daemon could
	// otherwise stream unbounded bytes → OOM. Generous caps that never clip a
	// legitimate reply; over-cap ⇒ a read error ⇒ the caller fails closed.
	maxClamdReplyBytes  = 4 << 10 // 4 KiB
	maxRspamdReplyBytes = 1 << 20 // 1 MiB
)

// ScanTimeout is the per-round-trip budget, sized for the product ceiling
// rather than a fixed 50 MiB assumption (see scanFloor / scanBytesPerSec).
//
// It reads MAX_MESSAGE_BYTES_CEILING — the upper bound on `max_message_bytes`,
// not the deployment's current setting — deliberately: the value must bound the
// largest message this build can ever be asked to scan, and it must not move
// under a `config_changed` reload while a scan is in flight.
func ScanTimeout() time.Duration {
	ceiling := time.Duration(mailfauna.MaxMessageBytesCeiling()) * time.Second /
		time.Duration(scanBytesPerSec)
	return scanFloor + ceiling
}

// PolicyDefault is the compile-time default scan policy
// (libs/fauna-mail/src/scan/mod.rs ScanPolicy::default): ClamAV enabled,
// action `reject`, rspamd enabled, scaling 0.5 (per-mille 500). Both roles run
// at this default until the admin snapshot projection lands.
func PolicyDefault() mailfauna.ScanPolicy {
	return mailfauna.ScanPolicy{
		ClamavEnabled:              true,
		ClamavActionOnInfected:     mailfauna.ClamavActionReject,
		RspamdEnabled:              true,
		RspamdScoreScalingPerMille: 500,
	}
}

// Clamd dials clamd and runs an INSTREAM scan of `raw`, returning the raw
// (null-trimmed) reply line. Unix-socket path (leading "/") or host:port TCP.
// The INSTREAM wire is: "zINSTREAM\0", then repeated <uint32-BE len><chunk>,
// then a <uint32-BE 0> terminator; clamd replies with a null-terminated line.
func Clamd(ctx context.Context, addr string, raw []byte) (string, error) {
	if addr == "" {
		return "", fmt.Errorf("no clamd address configured")
	}
	network := "tcp"
	if strings.HasPrefix(addr, "/") {
		network = "unix"
	}
	dialer := net.Dialer{}
	conn, err := dialer.DialContext(ctx, network, addr)
	if err != nil {
		return "", fmt.Errorf("dial %s %s: %w", network, addr, err)
	}
	defer func() { _ = conn.Close() }()
	if err := conn.SetDeadline(time.Now().Add(ScanTimeout())); err != nil {
		return "", fmt.Errorf("set deadline: %w", err)
	}

	w := bufio.NewWriter(conn)
	if _, err := w.WriteString("zINSTREAM\x00"); err != nil {
		return "", fmt.Errorf("write command: %w", err)
	}
	var lenbuf [4]byte
	for off := 0; off < len(raw); off += clamdChunkSize {
		end := off + clamdChunkSize
		if end > len(raw) {
			end = len(raw)
		}
		chunk := raw[off:end]
		binary.BigEndian.PutUint32(lenbuf[:], uint32(len(chunk)))
		if _, err := w.Write(lenbuf[:]); err != nil {
			return "", fmt.Errorf("write chunk length: %w", err)
		}
		if _, err := w.Write(chunk); err != nil {
			return "", fmt.Errorf("write chunk: %w", err)
		}
	}
	// Zero-length frame terminates the stream.
	binary.BigEndian.PutUint32(lenbuf[:], 0)
	if _, err := w.Write(lenbuf[:]); err != nil {
		return "", fmt.Errorf("write terminator: %w", err)
	}
	if err := w.Flush(); err != nil {
		return "", fmt.Errorf("flush: %w", err)
	}

	// clamd sends a single short null-terminated reply line, then closes.
	// Bound the read (D7): a wedged/compromised daemon must not stream
	// unbounded bytes into io.ReadAll. Read one byte past the cap so an
	// at-cap reply is detectable; over the cap ⇒ error ⇒ caller fails closed.
	reply, err := io.ReadAll(io.LimitReader(conn, maxClamdReplyBytes+1))
	if err != nil {
		return "", fmt.Errorf("read reply: %w", err)
	}
	if len(reply) > maxClamdReplyBytes {
		return "", fmt.Errorf("clamd reply exceeds %d-byte cap", maxClamdReplyBytes)
	}
	if len(reply) == 0 {
		return "", fmt.Errorf("empty clamd reply")
	}
	return string(reply), nil
}

// Rspamd POSTs `raw` to rspamd's /checkv2 and parses the JSON response into a
// scaled RspamdScore. The rspamd envelope context (IP / From / Rcpt) rides as
// HTTP headers per the checkv2 protocol; the message bytes are the body — a
// background re-score has no connection-time envelope and passes them empty
// (rspamd then scores content rules only). A non-200 or malformed-JSON
// response is an error (caller fails closed).
func Rspamd(
	ctx context.Context,
	cfg Config,
	raw []byte,
	clientIP, from string,
	rcpts []string,
) (mailfauna.RspamdScore, error) {
	url := strings.TrimRight(cfg.RspamdURL, "/") + "/checkv2"
	req, err := http.NewRequestWithContext(ctx, http.MethodPost, url, bytes.NewReader(raw))
	if err != nil {
		return mailfauna.RspamdScore{}, fmt.Errorf("build request: %w", err)
	}
	req.Header.Set("Content-Type", "message/rfc822")
	if clientIP != "" {
		req.Header.Set("IP", clientIP)
	}
	if from != "" {
		req.Header.Set("From", from)
	}
	for _, rcpt := range rcpts {
		req.Header.Add("Rcpt", rcpt)
	}
	client := cfg.HTTPClient
	if client == nil {
		client = &http.Client{Timeout: ScanTimeout()}
	}
	resp, err := client.Do(req)
	if err != nil {
		return mailfauna.RspamdScore{}, fmt.Errorf("post %s: %w", url, err)
	}
	defer func() { _ = resp.Body.Close() }()
	// Bound the read (D7): rspamd's /checkv2 JSON is bounded in practice; a
	// wedged/compromised daemon must not stream unbounded bytes into io.ReadAll.
	body, err := io.ReadAll(io.LimitReader(resp.Body, maxRspamdReplyBytes+1))
	if err != nil {
		return mailfauna.RspamdScore{}, fmt.Errorf("read response: %w", err)
	}
	if len(body) > maxRspamdReplyBytes {
		return mailfauna.RspamdScore{}, fmt.Errorf("rspamd reply exceeds %d-byte cap", maxRspamdReplyBytes)
	}
	if resp.StatusCode != http.StatusOK {
		return mailfauna.RspamdScore{}, fmt.Errorf("rspamd status %d: %s", resp.StatusCode, strings.TrimSpace(string(body)))
	}
	score, err := mailfauna.RspamdParseReply(string(body), cfg.Policy.RspamdScoreScalingPerMille)
	if err != nil {
		return mailfauna.RspamdScore{}, fmt.Errorf("parse rspamd reply: %w", err)
	}
	return score, nil
}
