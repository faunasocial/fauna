// Package config implements the bridge's localhost-only **operator-hatch**
// TOML reader.
//
// # Context
//
// A product invariant: nest configuration — every bridge,
// every key, every policy — is set from a Fauna app UI, propagated via
// nest state, and consumed by the bridge over WS-RPC (see
// internal/wsrpc.FetchConfig and wsrpc.ConfigSnapshot for the actual
// nest-side surface, which is what carries DKIM keys, spam thresholds,
// per-account opt-in, ACME details, etc.). The bridge does not read those
// from a file.
//
// However, a small set of **deployment-topology** values genuinely can't
// come from nest, because they're consumed *before* the bridge has dialed
// nest: the data directory containing the service-user keyfile, the
// loopback address the /metrics listener binds to, where structured logs
// go. Those legitimately belong to the operator running `docker-compose
// up` / `systemctl start fauna-mail-bridge`, and they may be overridden
// via this **operator-hatch** TOML file.
//
// # Allow-list discipline
//
// The operator-hatch is a deliberate allow-list. Only the named fields are
// accepted; anything else returns ErrForbiddenField pointing the operator
// at the Fauna admin UI. The product invariant — "nest configuration from
// clients, not config files" — is enforced at the parse boundary, not by
// convention: an operator who tries to pin a DKIM selector here gets an
// immediate, actionable error.
//
// Operators who want to change anything not in the allow-list (spam
// thresholds, DKIM keys, per-account fields, the bridge's role, …) do so
// from the Fauna admin UI; that change flows into nest state and reaches
// the bridge via wsrpc.FetchConfig + the subscribe-and-hot-reload loop
// (B.8 scope; main.go owns the WS connection).
//
// # Lifecycle
//
// The operator-hatch is OPTIONAL. The normal case for a fresh deployment
// is no operator-hatch at all — the bridge runs with its compiled-in
// defaults. Load("") returns (nil, nil) cleanly to support that.
package config

import (
	"bytes"
	"fmt"
	"io"
	"os"
	"strings"

	"github.com/BurntSushi/toml"
)

// OperatorHatch carries the small set of deployment-topology overrides
// nest cannot provide (because they're consumed pre-dial). Everything
// else lives in nest state and arrives via wsrpc.FetchConfig.
//
// Adding a field here is a product-invariant decision, not a routine
// change: any new field must be defensibly "deployment topology nest
// can't know about," not "knob the operator finds convenient." When in
// doubt the answer is "from the Fauna admin UI."
type OperatorHatch struct {
	// DataDir overrides the default data directory used to find the
	// service-user keyfile and persist refresh-state. Defaults to
	// /var/lib/fauna-mail-bridge.
	DataDir string `toml:"data_dir"`

	// MetricsBindAddr overrides the loopback address the /metrics
	// listener binds to. Defaults to 127.0.0.1:9090.
	MetricsBindAddr string `toml:"metrics_bind_addr"`

	// MTABindAddr overrides the address the MTA-role SMTP MX
	// listener binds to. Defaults to :25 (privileged; production
	// deployments grant CAP_NET_BIND_SERVICE / use authbind /
	// systemd socket activation). Topologies that prefer a non-
	// privileged bind + port-forward use this override. Only the
	// MTA role consults this.
	MTABindAddr string `toml:"mta_bind_addr"`

	// MTABindAddr465 overrides the bind address for the MTA-role
	// implicit-TLS submission listener (Phase D.1). Defaults to
	// ":465". Same topology-only justification as MTABindAddr (port
	// 465 is privileged; non-privileged deployments rebind here).
	MTABindAddr465 string `toml:"mta_bind_addr_465"`

	// MTABindAddr587 overrides the bind address for the MTA-role
	// STARTTLS-required submission listener (Phase D.1). Defaults
	// to ":587". Same topology-only justification as MTABindAddr.
	MTABindAddr587 string `toml:"mta_bind_addr_587"`

	// IMAPListenImplicitTLS overrides the public bind address for the
	// IMAPS (implicit-TLS) listener. Defaults to ":993". MDA-role only;
	// ignored on the MTA arm.
	IMAPListenImplicitTLS string `toml:"imap_listen_implicit_tls"`

	// IMAPListenStartTLS overrides the public bind address for the
	// IMAP (STARTTLS) listener. Defaults to ":143". MDA-role only.
	IMAPListenStartTLS string `toml:"imap_listen_starttls"`

	// CalDAVListenHTTPS overrides the public bind address for the
	// CalDAV-over-HTTPS listener. Defaults to ":443". MDA-role only;
	// CalDAV serves at the path `/caldav/{user}/` under this address.
	CalDAVListenHTTPS string `toml:"caldav_listen_https"`

	// CalDAVBindHost is the topology bind HOST (no port) for the CalDAV
	// listener on a LAN / bare-IP box (the home2 / home-relay shape). Unlike
	// CalDAVListenHTTPS — a full `host:port` that PINS the port and wins
	// outright — this carries only the interface: the port still comes from
	// the admin-set `caldav_port` (FetchConfigReply.caldav_port), so the box
	// serves CalDAV directly at `<CalDAVBindHost>:<admin-port>`, router-
	// bypassing and reachable by bare IP, while the admin can still change the
	// port from any client (the rebind path stays live). It is the exact CalDAV
	// analogue of how IMAPListen* already bind `FAUNA_LAN_BIND_IP` on a home box
	// — interface is OS-deployment topology, the port is nest state. Consulted
	// only when CalDAVListenHTTPS is empty (a full pin always wins). MDA-role
	// only. See docs/goal/behavior/caldav-server.md § Network exposure.
	CalDAVBindHost string `toml:"caldav_bind_host"`

	// MTAMXOverride is a static outbound transport route: a map from
	// recipient domain to the SMTP target `host` or `host:port` the
	// outbound worker delivers to, bypassing live DNS MX resolution for
	// that domain. Empty (the default) means "always resolve MX via DNS".
	// MTA-role only.
	//
	// This is deployment-topology, not nest-managed policy: it describes
	// how *this box's network* reaches a domain, which nest cannot know —
	// the classic cases are a split-horizon / air-gapped deployment whose
	// internal resolver can't answer public MX queries (route through an
	// internal relay), and a test harness pointing an `external.test`
	// domain at a loopback stub MX on an ephemeral port. It is the direct
	// analogue of Postfix `transport_maps` / `relayhost` (which likewise
	// express targets as `[host]:port`), and like the bind-address fields
	// above it is consumed before the bridge has any per-domain nest
	// policy to consult. A `host:port` value lets the target use a
	// non-25 port; a bare `host` implies port 25.
	//
	// TOML shape:
	//   [mta_mx_override]
	//   "external.test"   = "127.0.0.1:2526"
	//   "internal.corp"   = "relay.corp"
	MTAMXOverride map[string]string `toml:"mta_mx_override"`

	// ClamdAddr is the address of the co-located ClamAV daemon (clamd) the
	// MTA dials for the T1.4 content scan. A leading-slash value is a unix
	// socket path (Linux/macOS default /var/run/clamav/clamd.ctl); otherwise
	// a host:port (Windows default localhost:3310). Empty ⇒ main.go's
	// compiled-in default. MTA-role only.
	//
	// Deployment topology, not nest policy: it describes where *this box*
	// runs clamd, which nest cannot know (mail-content-scanning.md
	// § Compile-time decisions). The scan *policy* (enabled / action /
	// scaling) is nest-managed and (in a future track) arrives via the
	// config snapshot, mirroring the spam thresholds.
	ClamdAddr string `toml:"clamd_addr"`

	// RspamdURL is the base URL of the co-located rspamd daemon the MTA POSTs
	// to for the T1.4 content score (/checkv2 is appended). Default
	// http://localhost:11333. Empty ⇒ main.go's compiled-in default.
	// MTA-role only. Same deployment-topology-not-policy rationale as
	// ClamdAddr.
	RspamdURL string `toml:"rspamd_url"`
}

// ErrForbiddenField is returned when the operator-hatch file contains
// any key outside the Tier-1 deployment-topology allow-list. The Hint
// always points operators at the Fauna admin UI.
type ErrForbiddenField struct {
	// Key is the offending dotted TOML key (e.g. "dkim_selector",
	// "accounts.alice.opt_in").
	Key string
	// Hint is a short, actionable redirect to the right configuration
	// surface (always references the Fauna admin UI).
	Hint string
}

func (e *ErrForbiddenField) Error() string {
	return fmt.Sprintf(
		"config: forbidden operator-hatch field %q — this is nest-managed "+
			"configuration; set it from the Fauna admin UI instead. %s",
		e.Key, e.Hint,
	)
}

// hintForKey returns a short, actionable hint pointing the operator at
// the right Fauna admin UI surface for the rejected key. The default
// hint is a generic redirect; specific prefixes get sharper hints.
func hintForKey(key string) string {
	switch {
	case strings.HasPrefix(key, "dkim"):
		return "DKIM keys are provisioned via the admin pane → Mail → DKIM in your Fauna app."
	case strings.HasPrefix(key, "spam"), strings.HasPrefix(key, "dnsbl"), strings.HasPrefix(key, "greylist"):
		return "Spam / DNSBL / greylist thresholds are set in the admin pane → Mail → Policy in your Fauna app."
	case strings.HasPrefix(key, "accounts"):
		return "Per-account mail opt-in is set by each user in their Fauna app → Settings → Mail."
	case key == "role":
		return "The bridge's role is resolved by nest from the enrolled service-user keypair (see fauna.bridges.whoami); enroll the bridge from the admin pane of your Fauna app."
	case strings.HasPrefix(key, "mta"), strings.HasPrefix(key, "submission"), strings.HasPrefix(key, "tls"), strings.HasPrefix(key, "acme"):
		return "MTA / submission / TLS / ACME settings are managed in the admin pane → Mail in your Fauna app."
	case strings.HasPrefix(key, "idle_timeout"),
		strings.HasPrefix(key, "tombstone_retention"),
		key == "delete_nonempty",
		strings.HasPrefix(key, "bodystructure_cache"):
		return "IMAP-policy knobs are nest-managed; set them in the admin pane → Mail → Policy in your Fauna app."
	default:
		return "Bridge configuration lives in nest state and is set from the admin pane of your Fauna app."
	}
}

// Load reads a TOML operator-hatch file from path.
//
// Returns (nil, nil) if path is empty — the no-operator-hatch case is
// normal for a fresh deployment; the caller should fall back to
// compiled-in defaults.
//
// Returns (*OperatorHatch, nil) on success; (nil, &ErrForbiddenField{…})
// if the file contains any key outside the allow-list (DataDir,
// MetricsBindAddr, MTABindAddr, MTABindAddr465, MTABindAddr587,
// IMAPListenImplicitTLS, IMAPListenStartTLS, CalDAVListenHTTPS,
// CalDAVBindHost, MTAMXOverride, ClamdAddr, RspamdURL).
//
// Other I/O or TOML-syntax errors are returned as plain errors with a
// "config: …" prefix.
func Load(path string) (*OperatorHatch, error) {
	if path == "" {
		return nil, nil
	}
	data, err := os.ReadFile(path)
	if err != nil {
		return nil, fmt.Errorf("config: reading operator-hatch %s: %w", path, err)
	}
	return LoadFromReader(bytes.NewReader(data))
}

// LoadFromReader parses an operator-hatch TOML document from r. Used by
// tests (so they need not write a tempfile per case) and by Load.
//
// An empty/whitespace-only document is valid and yields a zero-valued
// *OperatorHatch (no overrides — the bridge falls back to compiled-in
// defaults for every field).
func LoadFromReader(r io.Reader) (*OperatorHatch, error) {
	if r == nil {
		return nil, fmt.Errorf("config: reader must not be nil")
	}
	var hatch OperatorHatch
	meta, err := toml.NewDecoder(r).Decode(&hatch)
	if err != nil {
		return nil, fmt.Errorf("config: parsing operator-hatch: %w", err)
	}
	// Enforce the allow-list: any key in the TOML document not consumed
	// by the OperatorHatch struct is a forbidden field.
	if undec := meta.Undecoded(); len(undec) > 0 {
		// Report the first undecoded key — operators almost always set
		// one thing at a time; one targeted error is more useful than a
		// dump of every key. The key's String() joins hierarchical keys
		// with dots (e.g. "accounts.alice.opt_in").
		key := undec[0].String()
		return nil, &ErrForbiddenField{
			Key:  key,
			Hint: hintForKey(key),
		}
	}
	return &hatch, nil
}
