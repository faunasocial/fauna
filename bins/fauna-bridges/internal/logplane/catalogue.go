package logplane

import (
	"fmt"
	"net"
	"strconv"

	"github.com/faunasocial/fauna/bins/fauna-bridges/internal/confinement"
)

// This file is the mail bridge's **compile-time event catalogue** for the
// sidecar log plane — the complete set of events this binary may report to
// nest's admin Logs surface, auditable in one read. The queue/flush/disable
// machinery is in logplane.go; this file is only the catalogue and its message
// templates.
//
// # Why these five
//
// They are the events an admin *acts on*: the bridge is up or it is not, it
// can serve TLS or it cannot, it got its ports or something else holds them,
// its link to nest is stable or flapping, and it cut live sessions on the way
// down. Per-transaction SMTP/IMAP chatter is deliberately absent — a delivery
// verdict is `mail-observability.md`'s (separate, unbuilt) surface, not this
// one, and routing it here would both flood the ring and drag recipient
// addresses toward a page that must never carry them.
//
// # Deliverability — the rule that decides what may be catalogued at all
//
// The plane reports *to nest, over the bridge's authenticated WS*. So a bridge
// can only report what it can report **while that link works**, and an event
// emitted when the link is permanently gone is not "best-effort", it is dead
// code wearing a catalogue entry's clothes — worse than absent, because the
// next reader takes it for coverage.
//
// Two events were deliberately NOT catalogued for exactly this reason:
//
//   - *enrollment rejected* — happens before approval, i.e. before the bridge
//     has an authenticated WS at all.
//   - *service user revoked* — by the time the bridge knows, nest's capability
//     gate is refusing every kind and the reconnector has torn the connection
//     down (ReconnectingClient.Call returns ErrReconnecting once cur is nil).
//
// Neither is a gap: in both cases **nest is the party that made the decision**,
// so nest already has the fact and logs it on its own ring. The bridge telling
// nest what nest just did would add nothing. The same reasoning is why the
// Rust relay's catalogue flushes on each successful handshake rather than
// pretending it can report while unreachable — see
// `libs/fauna-sidecar-client/src/log_plane.rs`.
//
// Before adding an event here, ask: *would the bridge still have a working
// nest link at the moment this fires?* If not, it belongs in the bridge's slog
// stream only.
//
// # The interpolation rule, concretely
//
// Every template below interpolates only a **role or listener name** (a fixed
// Fauna service/component name, constrained to a known set here rather than
// trusted), a **port**, or a **count**. In particular none of them
// interpolates the `error` its call site has in hand: a cert-fetch or bind
// failure's error string can carry upstream or remote text, and the plane's
// whole point is that it never becomes a log-injection surface into an admin
// page. The full error still goes to the bridge's own slog stream at the same
// site — losing nothing, leaking nothing.

// CatalogueEventIDs is every event id this binary may emit. It exists so the
// tests can sweep the catalogue for admission-charset and message-length
// violations; keep it in step when adding an event (the sweep test fails if you
// do not).
var CatalogueEventIDs = []string{
	"ready",
	"tls_cert_fetch_failed",
	"listener_bind_failed",
	"nest_reconnected",
	"shutdown_forced",
	"confinement_degraded",
}

// knownConfinementStates is the closed set of confinement tokens that may be
// interpolated — same bounded-class discipline as knownRoles/knownListeners.
// The tokens originate in internal/confinement, which already bounds them, so
// this is belt-and-braces; it costs nothing and keeps the claim structural.
//
// It is now *derived* from that package rather than restated (2026-08-23). The
// old hand-written copy was missing nothing, but a fourth confinement state
// would have landed there without landing here, and the failure is silent: the
// new token renders as "unknown" on the admin Logs surface, which is
// indistinguishable from the state that genuinely means "we could not tell".
var knownConfinementStates = func() map[string]bool {
	m := make(map[string]bool, len(confinement.ConfinementStates()))
	for _, s := range confinement.ConfinementStates() {
		m[s] = true
	}
	return m
}()

func safeConfinementState(s string) string {
	if knownConfinementStates[s] {
		return s
	}
	return "unknown"
}

// knownRoles is the closed set of role names that may be interpolated. A role
// arrives from nest's whoami reply, so it is not remote-controlled — but
// pinning it to a literal set is what makes the bounded-class claim structural
// instead of a trust assumption, and costs nothing.
var knownRoles = map[string]bool{"mta": true, "mda": true}

// safeRole renders a role for interpolation, collapsing anything unexpected to
// a constant rather than passing it through.
func safeRole(role string) string {
	if knownRoles[role] {
		return role
	}
	return "unknown"
}

// knownListeners is the closed set of listener component names, same rationale
// as knownRoles. These are the binds the bridge actually makes.
var knownListeners = map[string]bool{
	"smtp": true, "submission": true, "imaps": true,
	"imap": true, "caldav": true, "metrics": true,
}

func safeListener(name string) string {
	if knownListeners[name] {
		return name
	}
	return "unknown"
}

// PortOf extracts the port from a "host:port" bind address so a call site can
// report it without carrying the host half — a port is a permitted bounded
// value class, while a bind host can be an arbitrary operator-hatch string.
// It lives here, beside the rule it exists to enforce, so every call site
// renders addresses the same way.
//
// Returns 0 when the address does not parse: the event is still worth sending,
// and 0 reads as "could not tell" rather than inventing a port.
func PortOf(addr string) uint16 {
	_, portStr, err := net.SplitHostPort(addr)
	if err != nil {
		return 0
	}
	p, err := strconv.ParseUint(portStr, 10, 16)
	if err != nil {
		return 0
	}
	return uint16(p)
}

// Ready — the bridge finished enrolling and its role listeners are up. The
// "it came back" signal after a restart, and the only routine info-level event
// in the catalogue.
func Ready(role string) {
	Emit(LevelInfo, "ready", fmt.Sprintf("mail bridge ready (role %s)", safeRole(role)))
}

// TLSCertFetchFailed — a cert fetch or refresh from nest failed. The bridge
// keeps retrying on its backoff schedule, so this is the early warning for a
// silent outage: a listener that cannot serve TLS is a mailbox no MUA can
// reach.
func TLSCertFetchFailed() {
	Emit(LevelWarn, "tls_cert_fetch_failed",
		"TLS cert fetch from nest failed; retrying on the backoff schedule")
}

// ListenerBindFailed — a listener could not take its port. Almost always a
// port collision or a permissions problem, and invisible to the admin
// otherwise: the bridge stays up and merely serves less than it should.
func ListenerBindFailed(listener string, port uint16) {
	Emit(LevelError, "listener_bind_failed",
		fmt.Sprintf("%s listener failed to bind port %d", safeListener(listener), port))
}

// NestReconnected — the WS to nest dropped and came back; count is how many
// times that has happened in this process's life.
//
// Reported *after* the fact by construction (a bridge with no link to nest
// cannot tell nest anything), which is exactly why the running count is the
// payload rather than the gap's duration: one reconnect is a blip and needs no
// attention, but a count climbing through the day is a flapping link, and that
// is the thing an admin acts on.
func NestReconnected(count int64) {
	Emit(LevelWarn, "nest_reconnected",
		fmt.Sprintf("reconnected to nest (reconnect #%d this session)", count))
}

// ShutdownForced — the graceful drain window expired with transactions still
// in flight, so the bridge force-closed them. Means someone's mail session was
// cut; if it recurs, the grace window is too short for this deployment.
func ShutdownForced() {
	Emit(LevelWarn, "shutdown_forced",
		"force-closed in-flight connections (drain grace window expired)")
}

// ConfinementDegraded — the startup self-probe found this bridge is not
// confined the way the deployment artifact is supposed to confine it: either it
// can reach nest's sealed store, or nothing attributes its inability to the
// kernel LSM (security.md § Co-resident process trust boundary → Confinement
// self-probe).
//
// This is the *secondary* surface, deliberately. The durable record is the
// additive `confinement` field on the bridge's service-user row, because that
// is queryable state an admin view and a live e2e can assert on, while the
// plane is a bounded ring someone has to be looking at. The event exists
// because a misprovisioned box should be visible to an admin who never thinks
// to open a service-user row.
//
// Deliverable by the § Deliverability rule: the probe fires at startup, at
// which point the bridge holds a working authenticated link to nest — unlike
// the enrollment events that rule excluded.
func ConfinementDegraded(sealedStore, landlock string) {
	Emit(LevelWarn, "confinement_degraded",
		fmt.Sprintf("sandbox self-probe degraded (sealed store %s, landlock %s) — "+
			"check the deployment artifact",
			safeConfinementState(sealedStore), safeConfinementState(landlock)))
}
