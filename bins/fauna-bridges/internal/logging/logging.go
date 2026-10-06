// Package logging configures the bridge's structured-logging surface.
//
// The bridge emits JSON log records to stderr via log/slog (Go 1.24 stdlib)
// with two static attributes on every record:
//
//   - service: always "fauna-mail-bridge"
//   - role:    "mta" | "mda" | "unresolved" — the role nest assigns this
//     bridge, resolved over WS-RPC in B.8 via the Whoami RPC. Until B.8
//     wires that call, the role is "unresolved"; B.8 calls Init a second
//     time with the resolved role and replaces the default logger.
//
// Wire shape is one JSON object per record on stderr, one record per line.
// That's the Docker / s6 / systemd-journald convention: collectors scrape
// stderr line-by-line. No file handles, no rotation — the supervisor owns
// log retention.
package logging

import (
	"fmt"
	"io"
	"log/slog"
	"os"
	"strings"
)

// Service is the static service= attribute on every record.
const Service = "fauna-mail-bridge"

// RoleUnresolved is the sentinel role used before the Whoami RPC has
// resolved this bridge's actual role (mta/mda).
const RoleUnresolved = "unresolved"

// New returns a *slog.Logger emitting JSON to w with the given level and
// role pre-applied as record attributes. The level string is parsed
// case-insensitively as "debug"/"info"/"warn"/"error"; an empty string
// defaults to "info"; anything else returns an error.
//
// New is the test seam — pass a *bytes.Buffer for capture. Init wraps
// New with w=os.Stderr and installs the result as slog.Default.
func New(level, role string, w io.Writer) (*slog.Logger, error) {
	if w == nil {
		return nil, fmt.Errorf("logging.New: writer must not be nil")
	}
	lvl, err := parseLevel(level)
	if err != nil {
		return nil, err
	}
	handler := slog.NewJSONHandler(w, &slog.HandlerOptions{Level: lvl})
	withAttrs := handler.WithAttrs([]slog.Attr{
		slog.String("service", Service),
		slog.String("role", role),
	})
	return slog.New(withAttrs), nil
}

// Init configures the global slog default logger to emit JSON to stderr
// at the given level with service= and role= pre-applied. Call once at
// process startup, after argv is parsed; B.8 calls it a second time with
// the role nest resolved via Whoami.
func Init(level, role string) error {
	logger, err := New(level, role, os.Stderr)
	if err != nil {
		return err
	}
	slog.SetDefault(logger)
	return nil
}

// parseLevel maps the human-readable level string to slog.Level. Empty
// defaults to info; case-insensitive; unknown returns an error.
func parseLevel(s string) (slog.Level, error) {
	switch strings.ToLower(strings.TrimSpace(s)) {
	case "", "info":
		return slog.LevelInfo, nil
	case "debug":
		return slog.LevelDebug, nil
	case "warn", "warning":
		return slog.LevelWarn, nil
	case "error":
		return slog.LevelError, nil
	default:
		return 0, fmt.Errorf("logging: unknown level %q (want debug|info|warn|error)", s)
	}
}
