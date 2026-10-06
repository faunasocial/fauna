package logplane

import (
	"strings"
	"testing"
)

// The catalogue's event ids must satisfy nest's admission charset
// (`[a-z0-9_.-]`, <= MaxEventBytes) — a violation is *rejected* at admission,
// i.e. a silently missing event. Cheaper to catch here than in a tier_4 run.
func TestEveryCataloguedEventIDPassesNestAdmission(t *testing.T) {
	for _, id := range CatalogueEventIDs {
		if id == "" {
			t.Error("empty event id")
			continue
		}
		if len(id) > MaxEventBytes {
			t.Errorf("event id %q is %d bytes, over the %d-byte cap", id, len(id), MaxEventBytes)
		}
		for _, r := range id {
			ok := (r >= 'a' && r <= 'z') || (r >= '0' && r <= '9') || r == '_' || r == '.' || r == '-'
			if !ok {
				t.Errorf("event id %q contains %q — nest admission would reject it", id, r)
				break
			}
		}
	}
}

// Messages must stay under the admission cap so nest never truncates one — a
// truncated message is a message an admin half-reads. Drive every catalogue
// function with worst-case interpolations and measure what actually queued.
func TestCataloguedMessagesFitUnderTheAdmissionCap(t *testing.T) {
	setup(t)
	emitEveryCatalogueEventWorstCase()
	batch, _, ok := takeBatch()
	if !ok {
		t.Fatal("expected the catalogue sweep to queue events")
	}
	if len(batch) != len(CatalogueEventIDs) {
		t.Fatalf("queued %d events, but the catalogue declares %d — the sweep and CatalogueEventIDs have drifted",
			len(batch), len(CatalogueEventIDs))
	}
	for _, ev := range batch {
		if len(ev.Message) > MaxMessageBytes {
			t.Errorf("event %q renders %d bytes, over the %d-byte cap: %q",
				ev.Event, len(ev.Message), MaxMessageBytes, ev.Message)
		}
		if ev.Level != string(LevelError) && ev.Level != string(LevelWarn) && ev.Level != string(LevelInfo) {
			t.Errorf("event %q has level %q, outside {error,warn,info} — nest drops it", ev.Event, ev.Level)
		}
	}
}

// Every id the sweep emits must be declared, and vice versa. Without this, a
// new catalogue function silently escapes both checks above.
func TestTheSweepCoversExactlyTheDeclaredCatalogue(t *testing.T) {
	setup(t)
	emitEveryCatalogueEventWorstCase()
	batch, _, _ := takeBatch()
	seen := map[string]bool{}
	for _, ev := range batch {
		seen[ev.Event] = true
	}
	for _, id := range CatalogueEventIDs {
		if !seen[id] {
			t.Errorf("catalogue declares %q but the sweep never emits it", id)
		}
	}
	for id := range seen {
		found := false
		for _, d := range CatalogueEventIDs {
			if d == id {
				found = true
				break
			}
		}
		if !found {
			t.Errorf("sweep emits %q but CatalogueEventIDs does not declare it", id)
		}
	}
}

// The load-bearing privacy property. The bridge's slog stream correctly logs
// recipient addresses and actor IDs for journald; the plane must never carry
// them. This asserts the *rendered* messages are clean even when the call site
// is handed hostile input.
func TestNoCatalogueMessageCarriesCallerSuppliedFreeText(t *testing.T) {
	setup(t)
	emitEveryCatalogueEventWorstCase()
	batch, _, _ := takeBatch()
	// The worst-case sweep feeds these markers wherever a call site could be
	// tempted to interpolate an address, an actor id, or an upstream error.
	forbidden := []string{"@", "victim", "did:", "actor", "550 ", "\n", "\r"}
	for _, ev := range batch {
		for _, bad := range forbidden {
			if strings.Contains(ev.Message, bad) {
				t.Errorf("event %q leaked %q into the plane: %q", ev.Event, bad, ev.Message)
			}
		}
	}
}

// A role string reaches ready() from nest's whoami reply. Constraining it to
// the known set makes the "Fauna service name" bounded-class claim true by
// construction rather than by trust.
func TestReadyConstrainsTheRoleToKnownServiceNames(t *testing.T) {
	setup(t)
	Ready("mta")
	Ready("mda")
	Ready("evil@example.com role")
	batch, _, ok := takeBatch()
	if !ok || len(batch) != 3 {
		t.Fatalf("expected 3 events, got ok=%v len=%d", ok, len(batch))
	}
	if !strings.Contains(batch[0].Message, "mta") {
		t.Errorf("mta role not rendered: %q", batch[0].Message)
	}
	if !strings.Contains(batch[1].Message, "mda") {
		t.Errorf("mda role not rendered: %q", batch[1].Message)
	}
	if strings.Contains(batch[2].Message, "@") {
		t.Errorf("an unknown role must not be interpolated verbatim: %q", batch[2].Message)
	}
}

// PortOf is the guard that keeps a bind *host* — which can be an arbitrary
// operator-hatch string — off the wire while still reporting the port an admin
// needs. The hostile rows are the ones that matter.
func TestPortOfExtractsOnlyThePort(t *testing.T) {
	for _, tc := range []struct {
		addr string
		want uint16
	}{
		{"127.0.0.1:9090", 9090},
		{"0.0.0.0:25", 25},
		{"[::1]:993", 993},
		{":587", 587},
		{"mail.example.com:465", 465},
		{"", 0},
		{"no-port", 0},
		{"host:not-a-number", 0},
		{"host:99999", 0}, // out of uint16 range
	} {
		if got := PortOf(tc.addr); got != tc.want {
			t.Errorf("PortOf(%q) = %d, want %d", tc.addr, got, tc.want)
		}
	}
}

// The rendered event must never contain the host half, even when the bind
// address is a hostname an admin typed into an operator hatch.
func TestListenerBindFailedCarriesNoBindHost(t *testing.T) {
	setup(t)
	ListenerBindFailed("smtp", PortOf("mail.victim@example.com:25"))
	batch, _, ok := takeBatch()
	if !ok || len(batch) != 1 {
		t.Fatal("expected one event")
	}
	if strings.Contains(batch[0].Message, "example.com") || strings.Contains(batch[0].Message, "@") {
		t.Errorf("bind host leaked into the plane: %q", batch[0].Message)
	}
}
