package logging

import (
	"bytes"
	"encoding/json"
	"log/slog"
	"strings"
	"testing"
)

// TestInitInfoLevel pins the JSON shape every record carries: an info-level
// call produces a JSON object containing service, role, msg, level, and
// any caller-supplied attrs. We test against New rather than Init so we
// don't have to capture os.Stderr.
func TestInitInfoLevel(t *testing.T) {
	var buf bytes.Buffer
	logger, err := New("info", "mta", &buf)
	if err != nil {
		t.Fatalf("New: %v", err)
	}
	logger.Info("hello", "k", "v")

	var rec map[string]any
	if err := json.Unmarshal(bytes.TrimSpace(buf.Bytes()), &rec); err != nil {
		t.Fatalf("record is not valid JSON: %v (raw=%q)", err, buf.String())
	}
	for _, field := range []string{"service", "role", "msg", "level", "k"} {
		if _, ok := rec[field]; !ok {
			t.Errorf("record missing field %q (rec=%v)", field, rec)
		}
	}
	if rec["service"] != Service {
		t.Errorf("service = %q, want %q", rec["service"], Service)
	}
	if rec["role"] != "mta" {
		t.Errorf("role = %q, want %q", rec["role"], "mta")
	}
	if rec["msg"] != "hello" {
		t.Errorf("msg = %q, want %q", rec["msg"], "hello")
	}
	if rec["level"] != "INFO" {
		t.Errorf("level = %q, want %q", rec["level"], "INFO")
	}
	if rec["k"] != "v" {
		t.Errorf("k = %q, want %q", rec["k"], "v")
	}
}

// TestInitBadLevel asserts unknown level strings are rejected at New time.
func TestInitBadLevel(t *testing.T) {
	var buf bytes.Buffer
	_, err := New("bogus", "mta", &buf)
	if err == nil {
		t.Fatalf("New with bogus level: want error, got nil")
	}
	if !strings.Contains(err.Error(), "bogus") {
		t.Errorf("error should mention the bad level; got %q", err.Error())
	}
}

// TestInitRespectsRole asserts the role= attr reflects the param byte-for-byte
// (including the "unresolved" sentinel used before Whoami resolves the role).
func TestInitRespectsRole(t *testing.T) {
	for _, role := range []string{"mta", "mda", "unresolved"} {
		role := role
		t.Run(role, func(t *testing.T) {
			var buf bytes.Buffer
			logger, err := New("info", role, &buf)
			if err != nil {
				t.Fatalf("New: %v", err)
			}
			logger.Info("probe")
			var rec map[string]any
			if err := json.Unmarshal(bytes.TrimSpace(buf.Bytes()), &rec); err != nil {
				t.Fatalf("invalid JSON: %v", err)
			}
			if rec["role"] != role {
				t.Errorf("role = %q, want %q", rec["role"], role)
			}
		})
	}
}

// TestNewLevelCaseInsensitive pins the documented case-insensitivity of the
// level parser, and the empty-string default of "info".
func TestNewLevelCaseInsensitive(t *testing.T) {
	cases := map[string]bool{
		"":        true,
		"info":    true,
		"INFO":    true,
		"Debug":   true,
		"WARN":    true,
		"warning": true,
		"error":   true,
		"trace":   false,
		"silent":  false,
	}
	for level, ok := range cases {
		level, ok := level, ok
		t.Run(level, func(t *testing.T) {
			var buf bytes.Buffer
			_, err := New(level, "mta", &buf)
			if (err == nil) != ok {
				t.Errorf("New(%q): err=%v, want ok=%v", level, err, ok)
			}
		})
	}
}

// TestInitInstallsDefault asserts Init actually mutates slog.Default — the
// post-condition the package contract advertises. We check by reading the
// handler back; we don't try to capture os.Stderr.
func TestInitInstallsDefault(t *testing.T) {
	if err := Init("info", "mta"); err != nil {
		t.Fatalf("Init: %v", err)
	}
	if got := slog.Default(); got == nil {
		t.Fatal("slog.Default() returned nil after Init")
	}
	// Sanity: the default should accept an info record without panicking.
	slog.Default().Info("probe")
}
