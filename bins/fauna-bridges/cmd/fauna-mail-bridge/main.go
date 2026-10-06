// Command fauna-mail-bridge is the Go MTA/MDA bridge process.
//
// The binary's role (MTA or MDA) is determined by the service-user
// keypair it presents to nest at startup — nest holds the role and the
// configuration. There is no --mode CLI flag — a product invariant: "a
// bridge process discovers its role from its nest service-user
// enrollment, not from --mode=...".
//
// CLI args are limited to deployment-topology values that nest cannot
// provide (the keypair file path, the nest WS endpoint to dial, the log
// level, the data directory). Everything operator-facing — bind
// addresses, TLS keys, spam thresholds, policy knobs — lives in
// nest state, fetched by the bridge over WS-RPC after authentication.
//
// Startup sequence (Phase B.8):
//
//  1. Parse argv; validate.
//  2. Load operator-hatch TOML if --operator-hatch is set or
//     <data-dir>/operator-hatch.toml exists. Apply overrides
//     (metrics_bind_addr only today; data_dir is honored via argv).
//  3. Init structured logger with role="unresolved".
//  4. Load (or generate-with-placeholder-role) the service-user
//     keyfile. New keyfiles get "unresolved"/"unresolved-bridge"
//     placeholders; we'll write the resolved values back after
//     Whoami succeeds.
//  5. Build the AuthClient + Dial WS-RPC.
//  6. Call fauna.bridges.whoami → resolved role + bridge_id + domain.
//     Re-init the logger with the resolved role attribute and
//     persist the resolved role+bridge_id back to the keyfile.
//  7. Call fauna.bridges.fetch_config → ConfigSnapshot.
//  8. If WhoamiReply.Domain is non-empty, construct + Refresh +
//     Start the *tls.Provider. Empty domain → defer to a follow-up
//     phase (B.8's scope-reduction; see
//     the WhoamiReply doc comment).
//  9. Spawn the localhost-only /metrics HTTP listener on the bind
//     address from the operator-hatch (default 127.0.0.1:9090).
//  10. Dispatch to mta.Run or mda.Run (each blocks until ctx is
//     cancelled or its listener fails).
//  11. Wait for SIGINT/SIGTERM, cancel ctx, wait for the role
//     goroutine to return cleanly.
package main

import (
	"context"
	"errors"
	"flag"
	"fmt"
	"io"
	"log/slog"
	"net"
	"net/http"
	"os"
	"os/signal"
	"path/filepath"
	"runtime"
	"strings"
	"sync"
	"sync/atomic"
	"syscall"
	"time"

	"github.com/faunasocial/fauna/bins/fauna-bridges/internal/bridgeshutdown"
	"github.com/faunasocial/fauna/bins/fauna-bridges/internal/config"
	"github.com/faunasocial/fauna/bins/fauna-bridges/internal/confinement"
	"github.com/faunasocial/fauna/bins/fauna-bridges/internal/keypair"
	"github.com/faunasocial/fauna/bins/fauna-bridges/internal/logging"
	"github.com/faunasocial/fauna/bins/fauna-bridges/internal/logplane"
	"github.com/faunasocial/fauna/bins/fauna-bridges/internal/mailfauna"
	"github.com/faunasocial/fauna/bins/fauna-bridges/internal/mda"
	"github.com/faunasocial/fauna/bins/fauna-bridges/internal/metrics"
	"github.com/faunasocial/fauna/bins/fauna-bridges/internal/mta"
	"github.com/faunasocial/fauna/bins/fauna-bridges/internal/proxyproto"
	"github.com/faunasocial/fauna/bins/fauna-bridges/internal/scan"
	bridgetls "github.com/faunasocial/fauna/bins/fauna-bridges/internal/tls"
	"github.com/faunasocial/fauna/bins/fauna-bridges/internal/wsrpc"
)

// defaultMetricsBindAddr is the MTA role's /metrics bind address when
// no operator-hatch override is supplied. Localhost-only by design —
// /metrics is for the local Prometheus / s6-log-watcher / systemd-
// journald pipeline, NOT for cross-host scraping.
const defaultMetricsBindAddr = "127.0.0.1:9090"

// defaultMetricsBindAddrMDA is the MDA role's /metrics default. The MTA
// and MDA run as separate processes on the same host (one process per
// role — mail-bridge-lifecycle.md § Multi-role multi-process), so they
// need distinct metrics ports or the second binder collides on :9090.
const defaultMetricsBindAddrMDA = "127.0.0.1:9091"

// defaultMetricsBindAddrForRole returns the role-derived default
// /metrics bind address. Because the bridge learns its role from nest
// (fauna.bridges.whoami) rather than argv, the per-role metrics port is
// derived here instead of diverging the two s6 run-scripts — an
// operator-hatch metrics_bind_addr still overrides it. An unresolved /
// unknown role falls back to the MTA default (defensive; whoami rejects
// any role other than mta/mda before this is reached in production).
func defaultMetricsBindAddrForRole(role string) string {
	if role == "mda" {
		return defaultMetricsBindAddrMDA
	}
	return defaultMetricsBindAddr
}

// mdaListenAddrs holds the resolved MDA IMAP/CalDAV listener bind addresses.
type mdaListenAddrs struct {
	IMAPImplicitTLS string
	IMAPStartTLS    string
	CalDAVHTTPS     string
}

// resolveMDAListenAddrs picks the MDA IMAP/CalDAV listener bind addresses,
// applying the private-NAT-axis safe default (loopback) whenever the
// operator-hatch leaves a listener unset.
//
// nodeMode is the nest's NAT axis from fauna.bridges.whoami ("private" /
// "public"); the nest always sends it, and an empty value is simply "not
// private" (no distinct arm) — the all-interfaces behavior public
// deployments rely on. On the private axis a fresh deployment must not
// silently expose plaintext IMAP/CalDAV on a public interface
// (deployment-home-with-public-relay.md § Plaintext-mode behavior +
// § Don't do these: "Don't bind the private nest's IMAP/CalDAV to all
// interfaces"), so an unset listener defaults to 127.0.0.1 rather than the
// canonical :993/:143/:443 wildcards.
//
// An explicit operator-hatch bind always wins — that is the sanctioned
// OS-deployment-topology opt-out (the installer's real LAN IP; bind address
// is operator-set topology, never nest state). When the axis is private and
// such an explicit bind is itself an all-interfaces wildcard, the deployment
// is deliberately widening exposure: it is honored, but reported in the
// returned warnings so the operator gets a loud log line.
//
// caldavPort is the admin-set CalDAV listener port (FetchConfigReply.caldav_port,
// default 8443). Unlike IMAP's canonical :993/:143, the CalDAV no-hatch default
// is this admin choice, not a constant — a bare-IP / domainless box (no SNI
// router, no operator-hatch) serves CalDAV directly at <resolved-host>:<port>,
// the admin can change the port from any client, and changing it triggers a
// supervisor rebind (caldav-server.md § Network exposure). The operator-hatch
// caldav_listen_https still wins when present (the loopback IPC port a domain
// box's SNI router targets, or a desktop supervisor's <iface>:<port>) — the
// admin port drives only the no-hatch direct listener, so this is purely
// additive (today's only no-hatch production shape is bare-IP, the case we fix).
// Between those two, caldav_bind_host carries ONLY the LAN/bare-IP interface
// (the home2 / home-relay shape, set from FAUNA_LAN_BIND_IP) while the admin
// port still supplies the number: CalDAV binds <caldav_bind_host>:<admin-port>
// directly, router-bypassing and reachable by bare IP, and an admin port change
// still rebinds (no pin). It is the exact CalDAV analogue of the IMAP LAN bind.
// A 0 (a non-conforming snapshot that would bind port 0) normalizes to the canonical default
// via wsrpc.EffectiveCalDAVPort so the listener never binds port 0.
func resolveMDAListenAddrs(hatch *config.OperatorHatch, nodeMode string, caldavPort uint16) (mdaListenAddrs, []string) {
	private := nodeMode == "private"
	defaultHost := ""
	if private {
		defaultHost = "127.0.0.1"
	}
	var warns []string
	resolve := func(override, defaultPort, label string) string {
		if override != "" {
			if private && isAllInterfacesBind(override) {
				warns = append(warns, fmt.Sprintf(
					"MDA %s listener explicitly bound to all interfaces (%q) on a private nest; "+
						"plaintext IMAP/CalDAV may be reachable beyond the LAN — bind a specific "+
						"LAN address via the operator-hatch instead", label, override))
			}
			return override
		}
		return defaultHost + defaultPort
	}
	var imapImplicit, imapStartTLS, caldav, caldavBindHost string
	if hatch != nil {
		imapImplicit = hatch.IMAPListenImplicitTLS
		imapStartTLS = hatch.IMAPListenStartTLS
		caldav = hatch.CalDAVListenHTTPS
		caldavBindHost = hatch.CalDAVBindHost
	}
	caldavDefaultPort := fmt.Sprintf(":%d", wsrpc.EffectiveCalDAVPort(caldavPort))
	imapImplicitAddr := resolve(imapImplicit, defaultIMAPListenImplicitTLS, "IMAPS")
	imapStartTLSAddr := resolve(imapStartTLS, defaultIMAPListenStartTLS, "IMAP")
	// CalDAV resolves three ways, all at the admin-set port unless a full pin
	// overrides it: (1) a full caldav_listen_https pin wins outright (the SNI-
	// router loopback IPC port, or a desktop supervisor's <iface>:<port>);
	// (2) else a caldav_bind_host carries only the LAN/bare-IP interface and the
	// admin port supplies the number — the home2 shape, router-bypassing and
	// reachable by bare IP, with the admin still free to change the port (no
	// pin); (3) else the NAT-axis default host (loopback private, all-interfaces
	// public) + admin port.
	var caldavAddr string
	switch {
	case caldav != "":
		caldavAddr = resolve(caldav, caldavDefaultPort, "CalDAV")
	case caldavBindHost != "":
		caldavAddr = caldavBindHost + caldavDefaultPort
		if private && isAllInterfacesBind(caldavAddr) {
			warns = append(warns, fmt.Sprintf(
				"MDA CalDAV listener explicitly bound to all interfaces (%q) on a private nest; "+
					"plaintext CalDAV may be reachable beyond the LAN — bind a specific "+
					"LAN address via caldav_bind_host instead", caldavAddr))
		}
	default:
		caldavAddr = resolve("", caldavDefaultPort, "CalDAV")
	}
	return mdaListenAddrs{
		IMAPImplicitTLS: imapImplicitAddr,
		IMAPStartTLS:    imapStartTLSAddr,
		CalDAVHTTPS:     caldavAddr,
	}, warns
}

// The TLS-cert fetch-key logic (primary domain, or the floor sentinel when
// domainless) is shared with the ATProto PDS bridge — see
// bridgetls.CertFetchDomain / bridgetls.FloorCertFetchDomain in internal/tls.

// isAllInterfacesBind reports whether addr is an all-interfaces wildcard
// (no host, 0.0.0.0, or ::) as opposed to a specific address. Used to flag a
// private-axis MDA bind that would expose plaintext mail beyond the LAN.
func isAllInterfacesBind(addr string) bool {
	host, _, err := net.SplitHostPort(addr)
	if err != nil {
		return false
	}
	return host == "" || host == "0.0.0.0" || host == "::"
}

// defaultMTABindAddr is what the MTA-role SMTP MX listener binds to.
// Privileged port 25 — the production deployment grants this via
// CAP_NET_BIND_SERVICE / authbind / systemd socket activation. Phase
// E lets operators override via the operator-hatch when the topology
// uses a non-privileged bind + port-forward instead.
const defaultMTABindAddr = ":25"

// Default bind addresses for the MTA-role submission listeners (Phase D.1).
// 465 is implicit TLS; 587 is STARTTLS-required. Same operator-hatch
// override semantics as defaultMTABindAddr — production grants these
// privileged ports via CAP_NET_BIND_SERVICE / authbind / systemd socket
// activation; non-privileged deployments rebind via the hatch.
const (
	defaultMTABindAddr465 = ":465"
	defaultMTABindAddr587 = ":587"
)

// Default bind addresses for the MDA-role IMAP listeners (Phase C onward).
// The MDA arm picks these up when the operator-hatch leaves them empty; they
// are the canonical IMAP/IMAPS ports per imap-server.md § Process topology.
// CalDAV has no fixed default port — its no-hatch default is the admin-set
// caldav_port (FetchConfigReply.caldav_port, default 8443), resolved in
// resolveMDAListenAddrs (caldav-server.md § Network exposure).
const (
	defaultIMAPListenImplicitTLS = ":993"
	defaultIMAPListenStartTLS    = ":143"
)

// Default co-located content-scan daemon addresses (T1.4). These are
// OS-deployment topology (where clamd / rspamd listen on this box), not nest
// policy — overridable via the operator-hatch (clamd_addr / rspamd_url) per
// mail-content-scanning.md § Compile-time decisions. clamd defaults to its
// unix control socket on Unix and a TCP loopback on Windows; rspamd to its
// loopback HTTP worker.
const (
	defaultClamdAddrUnix = "/var/run/clamav/clamd.ctl"
	defaultClamdAddrTCP  = "localhost:3310"
	defaultRspamdURL     = "http://localhost:11333"
)

// rpcDeadline is the per-call timeout for the bootstrap RPCs
// (Whoami, FetchConfig, FetchTLSCertBlob). 30s is comfortably above
// the loopback round-trip and small enough that a stuck nest does
// not silently hang startup.
const rpcDeadline = 30 * time.Second

func main() {
	ctx, stop := signal.NotifyContext(context.Background(), syscall.SIGINT, syscall.SIGTERM)
	defer stop()
	if err := run(ctx, os.Args, os.Stdout); err != nil {
		// A forced graceful shutdown (grace window expired) is already logged
		// structurally by run(); exit 1 per mail-bridge-lifecycle.md
		// § Shutting down step 6 without re-dumping it to stderr as a crash.
		if !errors.Is(err, bridgeshutdown.ErrShutdownForced) {
			fmt.Fprintln(os.Stderr, err)
		}
		os.Exit(1)
	}
}

func run(ctx context.Context, args []string, out io.Writer) error {
	fs := flag.NewFlagSet(args[0], flag.ContinueOnError)
	fs.SetOutput(io.Discard)
	keypairFile := fs.String("keypair-file", "", "Ed25519 service-user keypair file; nest holds the role + config for this keypair (required)")
	nestEndpoint := fs.String("nest-endpoint", "", "WS-RPC endpoint URL to dial (e.g., wss://nest.example/api/v1/ws); required outside --dry-run")
	logLevel := fs.String("log-level", "info", "structured-log level: debug, info, warn, error")
	dryRun := fs.Bool("dry-run", false, "print the startup banner and exit; do not connect to nest")
	printPubkey := fs.Bool("print-pubkey", false, "mint-if-absent the --keypair-file keyfile, print its hex Ed25519 pubkey to stdout, and exit (deployment-artifact blessed-registry provisioning); does not connect to nest")
	dataDir := fs.String("data-dir", "", "deployment-topology directory; if --operator-hatch is unset, <data-dir>/operator-hatch.toml is loaded when present")
	operatorHatch := fs.String("operator-hatch", "", "explicit path to a localhost-only operator-hatch TOML overriding deployment-topology values (data dir, metrics bind, MTA bind, IMAP/CalDAV listen addresses)")
	if err := fs.Parse(args[1:]); err != nil {
		return err
	}
	if *keypairFile == "" {
		return fmt.Errorf("--keypair-file is required (nest looks up the bridge's role + config by the corresponding pubkey)")
	}
	// Blessed-registry provisioning: the deployment artifact runs
	// this AS ROOT at entrypoint to mint each role's keypair (mint-if-absent, so
	// the enrolled identity is stable across reboots) and capture its blessed
	// Ed25519 pubkey for the registry the nest reads. Print only the hex pubkey +
	// newline to stdout (the caller pipes it to /data/keys/blessed/<role>.pub) and
	// exit before any nest dial — so --nest-endpoint is not required here. The
	// keyfile role is derived from the path basename (mta.key → "mta"), matching
	// the running bridge's roleHintFromKeyfile. security.md § Enrollment
	// proof-of-possession contract.
	if *printPubkey {
		kf, err := keypair.LoadOrCreate(*keypairFile, roleHintFromKeyfile(*keypairFile), "unresolved-bridge")
		if err != nil {
			return fmt.Errorf("mint/load keyfile for --print-pubkey: %w", err)
		}
		fmt.Fprintf(out, "%x\n", kf.Ed25519PublicKey())
		return nil
	}
	if !*dryRun && *nestEndpoint == "" {
		return fmt.Errorf("--nest-endpoint is required outside --dry-run")
	}
	fmt.Fprintf(out, "fauna-mail-bridge: keypair_file=%s nest_endpoint=%q log_level=%s role=<from-nest> dry_run=%t\n",
		*keypairFile, *nestEndpoint, *logLevel, *dryRun)
	if *dryRun {
		return nil
	}

	hatch, err := loadOperatorHatch(*operatorHatch, *dataDir)
	if err != nil {
		return err
	}
	// metricsBindAddr is resolved after whoami — its default is
	// role-derived (mta 9090 / mda 9091) so the two same-host role
	// processes don't collide on :9090 (Gap D; § Multi-role).
	mtaBindAddr := defaultMTABindAddr
	if hatch != nil && hatch.MTABindAddr != "" {
		mtaBindAddr = hatch.MTABindAddr
	}
	mtaBindAddr465 := defaultMTABindAddr465
	if hatch != nil && hatch.MTABindAddr465 != "" {
		mtaBindAddr465 = hatch.MTABindAddr465
	}
	mtaBindAddr587 := defaultMTABindAddr587
	if hatch != nil && hatch.MTABindAddr587 != "" {
		mtaBindAddr587 = hatch.MTABindAddr587
	}
	// Optional static outbound transport route (operator-hatch
	// `mta_mx_override`): when present, the MTA outbound worker resolves
	// the listed recipient domains straight to the mapped SMTP target
	// (split-horizon / air-gapped relay; test loopback stub MX) and falls
	// back to live DNS MX for everything else. Nil ⇒ pure DNS resolution.
	// Built as a function of the nest client rather than eagerly: the
	// un-overridden-domain fallback must be the DNSSEC-validating nest
	// resolver (outbound DANE may only bind to a name that came out of a
	// validated MX RRset — RFC 7672 §2.2), and the client does not exist
	// until enrollment completes further down.
	mtaMXResolverFor := func(c wsrpc.Caller) mta.MXResolver {
		if hatch == nil || len(hatch.MTAMXOverride) == 0 {
			// nil ⇒ NewOutboundWorker installs NestMXResolver.
			return nil
		}
		return mta.OverrideMXResolver{
			Override: hatch.MTAMXOverride,
			Fallback: mta.NestMXResolver{Client: c},
		}
	}
	// T1.4 content-scan deployment topology (clamd / rspamd addresses) + the
	// compile-time-default scan policy. The addresses are operator-hatch
	// overridable; the policy stays at ScanPolicyDefault until a future track
	// projects mail.scanning.* from the config snapshot.
	clamdAddr := defaultClamdAddrUnix
	if runtime.GOOS == "windows" {
		clamdAddr = defaultClamdAddrTCP
	}
	if hatch != nil && hatch.ClamdAddr != "" {
		clamdAddr = hatch.ClamdAddr
	}
	rspamdURL := defaultRspamdURL
	if hatch != nil && hatch.RspamdURL != "" {
		rspamdURL = hatch.RspamdURL
	}
	scanConfig := scan.Config{
		ClamdAddr: clamdAddr,
		RspamdURL: rspamdURL,
		Policy:    scan.PolicyDefault(),
	}
	// MDA IMAP/CalDAV listener binds are resolved later (in the `case "mda"`
	// dispatch), once whoami has reported the nest's NAT axis: a private nest
	// defaults its binds to loopback rather than all-interfaces. See
	// resolveMDAListenAddrs.
	if err := logging.Init(*logLevel, logging.RoleUnresolved); err != nil {
		return fmt.Errorf("init logger: %w", err)
	}
	logger := slog.Default()

	kf, err := keypair.LoadOrCreate(*keypairFile, logging.RoleUnresolved, "unresolved-bridge")
	if err != nil {
		return fmt.Errorf("load keyfile: %w", err)
	}
	logger.Info(
		"keyfile loaded",
		"path", *keypairFile,
		"ed25519_pubkey_hex_prefix", fmt.Sprintf("%x", kf.Ed25519PublicKey()[:4]),
		"role_in_keyfile", kf.Role,
		"bridge_id_in_keyfile", kf.BridgeID,
	)

	// ── Zero-touch self-enrollment: announce + poll until approved ──
	//
	// A freshly-generated keypair has no enrollment row in nest, so the
	// authenticated challenge/verify Dial below would 404 ("actor not
	// registered") and the process would exit → s6 restart loop. Instead,
	// announce the pubkey over the anonymous (pre-identity) WS via
	// fauna.bridges.request_enrollment and poll it on the 1/2/4/8/16/30 s
	// backoff until an admin approves the bridge from their Fauna app —
	// mail-bridge-lifecycle.md § Cold boot steps 3–4 / § Pending approval.
	// The kind is loopback-gated nest-side (the in-container bridge↔nest hop);
	// approval inserts the audit row, after which the auth Dial succeeds. The
	// role_hint is derived from the keyfile basename (mta.key → "mta"); nest
	// validates it and the admin confirms it at approval — the authoritative
	// operating role still comes from whoami below (no --mode flag — a
	// product invariant: role is discovered, never operator-set). Idempotent on re-enroll, so a restart is harmless.
	roleHint := roleHintFromKeyfile(*keypairFile)
	// Slice-2 proof-of-possession: sign the
	// enrollment so nest can require an enrolling bridge to BE the artifact-blessed
	// key for its role AND prove possession of it. The key file is UID-isolated
	// (slices 1+4), so a co-resident attacker that cannot read it cannot forge this
	// signature. On a binary-only/dev nest (or a content-processor/atproto role) with no blessed registry the fields are still
	// sent but ignored (lenient path). x25519Pub is bound set-once at enrollment by
	// the same signature, so the later register_service_user attestation only
	// confirms it. security.md § Enrollment proof-of-possession contract.
	newEnrollID := func() wsrpc.EnrollmentIdentity {
		enrollX25519 := kf.X25519PublicKey()
		enrollEd25519 := kf.Ed25519PublicKey()
		return wsrpc.EnrollmentIdentity{
			Ed25519Pub:    enrollEd25519,
			X25519Pub:     enrollX25519[:],
			EnrollmentSig: wsrpc.SignEnrollment(kf.SigningKey(), roleHint, enrollEd25519, enrollX25519[:]),
			RoleHint:      roleHint,
			BridgeID:      kf.BridgeID,
		}
	}
	// The bridge always reaches the nest over the container loopback. When the
	// nest serves TLS there (real deployment), the nest's cert is issued for its
	// public domain (and may be self-signed), so the loopback dial mismatches
	// the SNI hostname and may not chain to a trusted root — a verifying client
	// fails with `malformed HTTP response "\x15\x03..."` (a TLS alert) and the
	// bridge can never enroll. Skip verification for a loopback nest endpoint
	// only; the connection never leaves the host so there is nothing to MITM.
	nestHTTP := wsrpc.NestHTTPClient(*nestEndpoint, logger)
	// Service-user re-keying auto-regenerate (mail-bridge-lifecycle.md
	// § Service-user re-keying steps 6–7): a `revoked` reply at the enrollment
	// poll means an admin rotated this bridge's key — archive the revoked
	// keypair (0400, kept for audit), generate a fresh one at the original
	// path, and re-enroll in-process. At most ONE regenerate per process run: a
	// second `revoked` means the admin revoked the *fresh* key too (a
	// deliberate rejection, not a rotation) — stop cleanly instead of minting
	// keys in a loop.
	regenerated := false
	for {
		err := wsrpc.EnrollAndAwaitApproval(ctx, *nestEndpoint, newEnrollID(), logger, nestHTTP)
		if err == nil {
			break
		}
		switch {
		case errors.Is(err, wsrpc.ErrBridgeRevoked) && !regenerated:
			// Nothing is serving yet (no listeners open pre-approval), so the
			// rotation is a clean in-process restart of the enrollment flow.
			freshKf, archived, aerr := keypair.ArchiveAndRegenerate(*keypairFile, roleHint, kf.BridgeID)
			if aerr != nil {
				return fmt.Errorf("re-key after revoked enrollment: %w", aerr)
			}
			kf = freshKf
			regenerated = true
			logger.Warn("enrollment is revoked (admin key rotation): archived the old keypair and generated a fresh one; re-enrolling",
				"archived", archived,
				"new_ed25519_pubkey_hex_prefix", fmt.Sprintf("%x", kf.Ed25519PublicKey()[:4]),
			)
			continue
		case errors.Is(err, wsrpc.ErrBridgeRevoked):
			// The freshly-generated key was revoked too — a deliberate admin
			// rejection, not a rotation. A revoked row stays revoked; exit 0.
			logger.Warn("nest revoked the freshly-generated keypair as well; shutting down (deliberate admin rejection)")
			return nil
		case errors.Is(err, context.Canceled), errors.Is(err, context.DeadlineExceeded):
			logger.Info("shutdown signal received while awaiting bridge approval", "err", err)
			return nil
		default:
			if code, ok := wsrpc.RpcErrorCode(err); ok && code == "fauna.bridges.permission_denied" && regenerated {
				// Strict enrollment mode: the fresh key is not yet the
				// artifact-blessed key for this role — the root-owned blessed
				// registry is re-derived from the keyfile by the s6 run-script
				// at service start (deliberately not writable from this UID).
				// Exit 0 so the supervisor restarts us: the run-script
				// re-blesses the fresh key, then enrollment proceeds.
				logger.Info("fresh keypair awaits the artifact re-bless (strict enrollment); exiting for a supervisor restart")
				return nil
			}
			return fmt.Errorf("await bridge approval: %w", err)
		}
	}

	authClient := wsrpc.NewAuthClient(nestHTTP, *nestEndpoint, kf.Ed25519PublicKey(), kf.SigningKey())
	dialCtx, dialCancel := context.WithTimeout(ctx, rpcDeadline)
	defer dialCancel()
	client, err := wsrpc.Dial(dialCtx, wsrpc.ClientConfig{
		NestEndpoint: *nestEndpoint,
		AuthClient:   authClient,
		Logger:       logger,
		HTTPClient:   nestHTTP,
	})
	if err != nil {
		return fmt.Errorf("dial nest: %w", err)
	}
	// The raw client is wrapped in a *wsrpc.ReconnectingClient below (after
	// the bootstrap whoami/fetch_config), which then owns the connection
	// lifecycle — re-dialling on drops per mail-bridge-lifecycle.md
	// § Reconnecting. This single defer closes the reconnector once it
	// exists, or the raw client on a bootstrap-failure early return.
	var reconnector *wsrpc.ReconnectingClient
	defer func() {
		if reconnector != nil {
			_ = reconnector.Close()
			return
		}
		_ = client.Close()
	}()
	logger.Info("ws-rpc dial complete", "endpoint", *nestEndpoint)

	// ── Whoami: resolve role + bridge_id + domain from nest ──
	whoamiCtx, whoamiCancel := context.WithTimeout(ctx, rpcDeadline)
	whoami, err := wsrpc.Whoami(whoamiCtx, client)
	whoamiCancel()
	if err != nil {
		return fmt.Errorf("whoami: %w", err)
	}
	if whoami.Role != "mta" && whoami.Role != "mda" {
		return fmt.Errorf("whoami: nest returned unknown role %q (want mta or mda)", whoami.Role)
	}
	if whoami.Status != "approved" {
		return fmt.Errorf("whoami: bridge service user is %q, not approved; have the admin approve this bridge in their Fauna app", whoami.Status)
	}
	// Re-init the logger with the resolved role attribute. The role
	// is on every record from here on (logging package's WithAttrs
	// path). Update the WS-RPC client's logger too — Dial captured
	// the role=unresolved logger and would otherwise keep emitting
	// records under that placeholder for the lifetime of the process.
	if err := logging.Init(*logLevel, whoami.Role); err != nil {
		return fmt.Errorf("re-init logger with resolved role: %w", err)
	}
	logger = slog.Default()
	client.SetLogger(logger)
	logger.Info(
		"whoami resolved",
		"role", whoami.Role,
		"bridge_id", whoami.BridgeID,
		"status", whoami.Status,
	)
	// Surface the resolved role for dashboards / fleet-level sums.
	// Whoami already rejected anything that wasn't "mta" or "mda" above.
	metrics.BridgeRole.WithLabelValues(whoami.Role).Set(1)
	metrics.BridgeRole.WithLabelValues(otherRole(whoami.Role)).Set(0)

	// Persist the resolved role + bridge_id back to the keyfile so
	// a future admin inspecting it sees nest's view, not the
	// placeholder. Idempotent — UpdateEnrollment is a no-op if
	// the keyfile already matches.
	if err := kf.UpdateEnrollment(*keypairFile, whoami.Role, whoami.BridgeID); err != nil {
		// Soft failure: the seeds are still good, the resolved role
		// lives in memory for this run. Log + continue rather than
		// terminate the bridge — the worst case is the keyfile's
		// `role` field stays "unresolved" until the next clean
		// shutdown.
		logger.Warn("persist resolved role to keyfile failed (continuing)", "err", err)
	}

	// ── Register service user: attest the x25519 pubkey ──
	// nest seals the TLS cert blob to the bridge's per-instance
	// x25519 pubkey (storage-modes.md rule 6; mail-bridge-lifecycle.md
	// § Cold boot). Without this attestation bridge_service_users
	// .x25519_pubkey stays NULL, so the cert fan-out skips this
	// bridge (bridges_skipped_no_x25519) → 465/993 never get a cert.
	// The nest handler requires an enrollment
	// row, which whoami==approved above confirms; the upsert is
	// idempotent and the row is persisted, so attesting once at cold
	// boot suffices (a reconnect does not lose it). Fatal on failure,
	// consistent with the other cold-boot RPCs — the supervisor restart
	// retries.
	x25519Pub := kf.X25519PublicKey()
	// PQ-CAP-2: derive the holder's ML-KEM keypair from the keyfile Ed25519 seed.
	// The `ek` is published now so the client mint can seal capability grants
	// X-Wing to this holder; the `dk` is threaded into the MDA capability holder
	// (below) to open hybrid wraps. Deterministic in the seed — re-derived each
	// boot, never persisted. Every bridge role publishes its ek (the MTA's is
	// reused by the paused S6 TLS hybrid).
	bridgeMlkemDk, bridgeMlkemEk, err := mailfauna.DeriveBridgeServiceUserMlkem(kf.Ed25519Seed)
	if err != nil {
		return fmt.Errorf("derive bridge ML-KEM keypair: %w", err)
	}
	// ── Confinement self-probe ──
	// Measure our own sandbox and ride the result along on the same call.
	// This code runs INSIDE the sandbox by construction — `fauna-sandbox`
	// applies its Landlock + seccomp profile and then execs this binary, so
	// there is no point in this process that is not already confined. That is
	// exactly why the probe lives here and not in the wrapper: the wrapper
	// would measure its own pre-restriction view and report a comforting lie
	// (security.md § Co-resident process trust boundary → Confinement
	// self-probe).
	//
	// Diagnostics only — nothing below branches on the result except the
	// warning. A compromised bridge can report whatever it likes; the value is
	// catching the honest misconfiguration on a box no one can SSH into
	// (testing.md § Gap 3).
	conf := confinement.Probe(*dataDir)
	logger.Info("confinement self-probe",
		"uid", conf.UID,
		"sealed_store", conf.SealedStore,
		"landlock", conf.Landlock,
		"seccomp", conf.Seccomp,
	)
	if !conf.Confined() {
		// Secondary surface: the admin's Logs page, so a misprovisioned box is
		// visible without anyone thinking to read a service-user row. The wire
		// field stays the primary, queryable record.
		logplane.ConfinementDegraded(conf.SealedStore, conf.Landlock)
		logger.Warn("bridge is NOT confined as expected — check the deployment "+
			"artifact (a compose bypassing fauna-sandbox, a kernel without "+
			"Landlock, or a seccomp policy blocking the landlock syscalls)",
			"sealed_store", conf.SealedStore, "landlock", conf.Landlock)
	}

	regCtx, regCancel := context.WithTimeout(ctx, rpcDeadline)
	enrollmentRequestID, err := wsrpc.RegisterServiceUser(
		regCtx, client,
		kf.Ed25519PublicKey(), x25519Pub[:], bridgeMlkemEk,
		whoami.Role, whoami.BridgeID, &conf,
	)
	regCancel()
	if err != nil {
		return fmt.Errorf("register_service_user (x25519 attestation): %w", err)
	}
	logger.Info("x25519 + ML-KEM ek attested to nest", "enrollment_request_id", enrollmentRequestID)

	// ── FetchConfig: pull the current config snapshot ──
	cfgCtx, cfgCancel := context.WithTimeout(ctx, rpcDeadline)
	snapshot, err := wsrpc.FetchConfig(cfgCtx, client, "all")
	cfgCancel()
	if err != nil {
		return fmt.Errorf("fetch_config: %w", err)
	}
	logger.Info(
		"config snapshot fetched",
		"spam_max_score_before_reject", snapshot.Spam.MaxScoreBeforeReject,
		"spam_fcrdns_mode", snapshot.Spam.FCrDNSMode,
		"spam_dnsbl_servers", snapshot.Spam.DNSBLServers,
		"spam_greylist_enabled", snapshot.Spam.GreylistEnabled,
		"auth_enforce_dmarc", snapshot.Auth.EnforceDmarc,
		"submission_max_per_day", snapshot.Submission.MaxPerDay,
		"imap_idle_timeout_secs", snapshot.IMAP.IdleTimeoutSecs,
		"local_domains_count", len(snapshot.LocalDomains),
		"primary_domain", snapshot.PrimaryDomain,
	)

	// roleConfigReloader is published by whichever role Run builds its
	// wsrpc.ConfigReloader (mta.Run and mda.Run both do — one role runs per
	// process). The reconnect OnConnect below routes its re-fetched snapshot
	// through the same Apply seam as the config_changed push, so a config that
	// changed *during* a WS gap hot-applies on reconnect. atomic.Pointer because
	// the role Run (its writer) and the reconnector's OnConnect (its reader) run
	// on different goroutines.
	var roleConfigReloader atomic.Pointer[wsrpc.ConfigReloader]

	// How many times the WS to nest has dropped and come back. Read by the
	// log-plane event in OnConnect below — a climbing count is what makes a
	// flapping link visible to an admin.
	var reconnectAttempts atomic.Int64

	// ── In-process reconnect: from here the bridge survives nest blips ──
	//
	// Wrap the live client so a dropped WS (nest restart, network blip, NAT
	// timeout, idle close) triggers an in-process reconnect-backoff
	// (1/2/4/8/16/30 s) instead of stranding the listeners on a dead
	// connection — mail-bridge-lifecycle.md § Reconnecting. The SMTP / IMAP
	// / CalDAV listeners, the TLS refresher, and the per-transaction
	// verdict RPCs all issue through this reconnector; calls made during a
	// reconnect gap return wsrpc.ErrReconnecting, which the MTA maps to a
	// transient 451. Each reconnect re-runs whoami (revoke → graceful
	// shutdown via Done()) and re-fetches config, hot-applying it through the
	// role's ConfigReloader.Apply seam when one is published (MDA today) —
	// § Reconnecting / § Implementation status today.
	//
	// The reconnect loop is rooted in context.Background(), NOT the process
	// ctx: on SIGTERM the WS must stay open while the role listeners drain
	// in-flight transactions (they still need nest), exactly as the previous
	// deferred client.Close() kept it. The loop is stopped solely by the
	// deferred reconnector.Close() — which runs after roleWG.Wait() below —
	// or by a revoke (Done() → graceful shutdown).
	reconnector = wsrpc.NewReconnectingClient(context.Background(), client, wsrpc.ReconnectConfig{
		Dial: func(c context.Context) (*wsrpc.Client, error) {
			dctx, dcancel := context.WithTimeout(c, rpcDeadline)
			defer dcancel()
			return wsrpc.Dial(dctx, wsrpc.ClientConfig{
				NestEndpoint: *nestEndpoint,
				AuthClient:   authClient,
				Logger:       logger,
			})
		},
		OnConnect: func(c context.Context, cc wsrpc.Caller) error {
			wctx, wcancel := context.WithTimeout(c, rpcDeadline)
			wh, werr := wsrpc.Whoami(wctx, cc)
			wcancel()
			// A revoked bridge never sees a status=revoked *reply* — nest's
			// capability gate denies it whoami before the handler runs — so
			// the denial itself is the revocation signal (same rule as the
			// in-band probe; wsrpc.WhoamiIndicatesRevoked). This reconnect is
			// exactly where a mid-run revoke lands: nest's revocation
			// teardown force-closes the old socket with 4401.
			if wsrpc.WhoamiIndicatesRevoked(wh, werr) {
				return wsrpc.ErrBridgeRevoked // → graceful shutdown
			}
			if werr != nil {
				return fmt.Errorf("reconnect whoami: %w", werr) // transient → retry
			}
			if wh.Status != "approved" {
				// pending/unknown mid-run is unexpected but recoverable; retry
				// rather than tear the bridge (and its live MUA sessions) down.
				return fmt.Errorf("reconnect whoami: status %q (want approved)", wh.Status)
			}
			fctx, fcancel := context.WithTimeout(c, rpcDeadline)
			snap, ferr := wsrpc.FetchConfig(fctx, cc, "all")
			fcancel()
			if ferr != nil {
				return fmt.Errorf("reconnect fetch_config: %w", ferr) // transient → retry
			}
			logger.Info("reconnect: re-fetched config snapshot",
				"local_domains_count", len(snap.LocalDomains),
				"primary_domain", snap.PrimaryDomain,
			)
			// Hot-apply the re-fetched snapshot through the same applier seam
			// as the config_changed push, so a config changed during the WS gap
			// takes effect on reconnect (not only on the next push). Both roles
			// publish a reloader; nil only before the role's Run has reached its
			// publish call.
			if r := roleConfigReloader.Load(); r != nil {
				r.Apply(snap)
			}
			// OnConnect fires only on a genuine re-dial (the initial client is
			// installed directly), so reaching here means the link dropped and
			// came back. Reported after the fact by construction — a bridge
			// with no link to nest cannot tell nest anything — which is why the
			// attempt count is the payload: it is how a flapping link becomes
			// visible at all. Queued now, shipped by the next flush over this
			// freshly-live connection.
			logplane.NestReconnected(reconnectAttempts.Add(1))
			return nil
		},
		Logger: logger,
	})

	// ── Sidecar log plane: ship catalogued events to nest's admin ring ──
	//
	// observability.md § The sidecar log plane. Only `fauna-nest` installs a
	// ring layer, so without this a bridge that cannot fetch a TLS cert or bind
	// a port is invisible on the admin Logs page. Strictly best-effort: the
	// queue is bounded and drop-oldest, and a flush failure never touches mail
	// serving.
	//
	// Rooted in context.Background() for the same reason the reconnect loop is:
	// the most valuable events happen during shutdown, so the plane must
	// outlive the process ctx. The defer below is registered AFTER the
	// reconnector's Close defer and therefore runs BEFORE it (LIFO) — that
	// ordering is what lets the final flush go out over a still-open
	// connection.
	planeCtx, planeCancel := context.WithCancel(context.Background())
	planeDone := make(chan struct{})
	go func() {
		logplane.Run(planeCtx, reconnector)
		close(planeDone)
	}()
	defer func() {
		planeCancel()
		<-planeDone
	}()

	// ── TLS provider. The MDA/MTA terminate their own TLS, so a bridge
	// always needs a serving cert. The fetch key is the deployment's primary
	// mail domain (`is_primary = true` mail_domains row) when one exists —
	// per-domain TLS is a separate track (docs/goal/behavior/mail-multidomain.md
	// § MTA-STS HTTPS cert); for now the single provider anchors on the primary
	// domain and serves the same cert across all listeners via SNI.
	//
	// When there is NO primary domain — a domainless / bare-IP / localhost nest
	// (the any-locator design, 2026-06-18-caldav-imap-any-locator-design.md
	// § 3) — the provider still builds, fetching under the floor sentinel so
	// nest seals its self-signed floor cert (CN "fauna-nest", SANs
	// localhost/127.0.0.1). Fauna apps key-bind whatever cert is served
	// (security.md Axis 1); third-party MUAs TOFU-accept the self-signed floor.
	// This is what lets the CalDAV/IMAP listeners serve TLS at all on a
	// domainless nest (pre-2026-06-18 the provider was skipped → no cert).
	// NOTE: Provider.domain is fixed at New(), so a domainless→domained
	// transition (adding the first mail domain to a running MDA) needs an MDA
	// restart to re-target TLS to mail.<domain> — out of alpha scope (spec § 6);
	// caldav-server.md § Implementation status today tracks it.
	var tlsProvider *bridgetls.Provider
	secret := kf.X25519Secret()
	tlsProvider, err = bridgetls.New(bridgetls.Config{
		Domain:       bridgetls.CertFetchDomain(snapshot.PrimaryDomain),
		Role:         whoami.Role,
		BridgeID:     whoami.BridgeID,
		X25519Secret: secret[:],
		Caller:       reconnector,
		Logger:       logger,
	})
	if err != nil {
		return fmt.Errorf("tls.New: %w", err)
	}
	refreshCtx, refreshCancel := context.WithTimeout(ctx, rpcDeadline)
	err = tlsProvider.Refresh(refreshCtx)
	refreshCancel()
	if err != nil {
		// First-refresh failure is soft — the refresh loop retries on backoff,
		// so the cert lands before any RCPT TO / CalDAV PROPFIND arrives (e.g.
		// the bridge is still pending approval, or its x25519 not yet on the
		// nest row).
		logger.Warn("initial tls refresh failed (will retry on backoff schedule)",
			"primary_domain", snapshot.PrimaryDomain, "err", err)
		// The admin-meaningful half of the same event. `err` deliberately does
		// NOT ride the plane — it can carry upstream text, and the plane must
		// never be a log-injection surface into an admin page. It stays in the
		// slog record above, which journald captures. (`primary_domain` would
		// be a permitted bounded class, but adds nothing an admin acts on here.)
		logplane.TLSCertFetchFailed()
	}
	tlsProvider.Start(ctx)

	// ── Capability holder secret: MDA-only ──
	//
	// The MDA becomes a capability holder (capability-mediated-content-
	// processing design § 2.4): it HPKE-Opens the content grants the user
	// sealed to its enrolled x25519 pubkey, so a re-score/re-index drain
	// (Slice 5A) can transiently wield a scoped content key on an untrusted
	// box. Same secret source as TLS (the bridge's enrolled service-user
	// X25519 — NOT the actor identity). Empty/short leaves the holder loop off
	// (localhost / pre-enrollment → no grants).
	var capabilityX25519Secret []byte
	if whoami.Role == "mda" {
		secret := kf.X25519Secret()
		capabilityX25519Secret = secret[:]
	}

	// ── Metrics listener: localhost-only /metrics ──
	// The default bind is role-derived (mta 9090 / mda 9091) so the two
	// same-host role processes don't collide on :9090 (Gap D); an
	// operator-hatch metrics_bind_addr still overrides it.
	metricsBindAddr := defaultMetricsBindAddrForRole(whoami.Role)
	if hatch != nil && hatch.MetricsBindAddr != "" {
		metricsBindAddr = hatch.MetricsBindAddr
	}
	metricsSrv := &http.Server{
		Addr:              metricsBindAddr,
		Handler:           metricsMux(),
		ReadHeaderTimeout: 5 * time.Second,
	}
	go func() {
		logger.Info("metrics listener starting", "addr", metricsBindAddr)
		err := metricsSrv.ListenAndServe()
		if err != nil && !errors.Is(err, http.ErrServerClosed) {
			// Observability-only: a metrics bind/serve failure (e.g. a
			// port collision with the sibling role) must NOT strand mail
			// serving. Log and continue — the role listeners are
			// unaffected (Gap D: non-fatal metrics bind).
			logger.Error("metrics listener failed (continuing; /metrics unavailable)", "addr", metricsBindAddr, "err", err)
			// Exactly the case the plane exists for: the bridge stays up and
			// merely serves less than it should, so nothing else tells the
			// admin. Only the port rides along — the bind host could be an
			// operator-hatch string, and `err` never travels.
			logplane.ListenerBindFailed("metrics", logplane.PortOf(metricsBindAddr))
		}
	}()

	// Graceful-shutdown drain budget (T2.6): nest's catalog value
	// (mail.bridge.shutdown_grace_seconds, default 30 s) governs how long
	// the role's listeners drain in-flight transactions on SIGTERM before
	// force-closing. This comes from nest state (a product invariant), never
	// argv.
	shutdownGrace := time.Duration(snapshot.Bridge.ShutdownGraceSeconds) * time.Second

	// MDA listener binds, resolved now that whoami has reported the nest's
	// NAT axis: a private nest defaults its IMAP/CalDAV binds to loopback so a
	// fresh private-paired plaintext deployment never silently exposes mail on
	// a public interface (deployment-home-with-public-relay.md § Plaintext-mode
	// behavior). An explicit operator-hatch bind (the installer's LAN IP) wins;
	// a private + all-interfaces explicit bind is honored but warned about.
	// Computed only for the MDA role — the MTA serves no IMAP/CalDAV.
	var mdaAddrs mdaListenAddrs
	// caldavBindIsAdminPort: the CalDAV listener took the admin-set port (no
	// operator-hatch pinned it), so a later set_caldav_port must trigger a
	// supervisor rebind; a hatch-pinned box ignores the admin port (see mda.Deps).
	var caldavBindIsAdminPort bool
	// routerProxyAuth: the secret the DAV listener requires in a PROXY-v2
	// header's router-auth TLV before honouring the source it conveys. The DAV
	// port is a loopback bind on a router-fronted box, so without this a
	// compromised co-resident bridge UID could forge a header and spoof the
	// source IP the AUTH lockout + report_auth_event audit key on (security.md
	// § Co-resident process trust boundary). Nil on a non-router box ⇒
	// trust-any-loopback. MDA-only: the MTA's listeners are published directly
	// and take no PROXY header.
	var routerProxyAuth []byte
	if whoami.Role == "mda" {
		var mdaBindWarnings []string
		routerProxyAuth = proxyproto.RouterAuthFromEnv(logger)
		// The CalDAV no-hatch default is the admin-set port (snapshot.CalDAVPort,
		// default 8443) — a bare-IP / domainless box binds it directly; an
		// operator-hatch caldav_listen_https (a domain box's loopback IPC port, a
		// desktop supervisor's <iface>:<port>) still wins. See resolveMDAListenAddrs.
		caldavBindIsAdminPort = hatch == nil || hatch.CalDAVListenHTTPS == ""
		mdaAddrs, mdaBindWarnings = resolveMDAListenAddrs(hatch, whoami.NodeMode, snapshot.CalDAVPort)
		for _, w := range mdaBindWarnings {
			logger.Warn(w)
		}
		logger.Info("mda listener binds resolved",
			"node_mode", whoami.NodeMode,
			"imaps", mdaAddrs.IMAPImplicitTLS,
			"imap", mdaAddrs.IMAPStartTLS,
			"caldav", mdaAddrs.CalDAVHTTPS,
			"proxy_header_authenticated", len(routerProxyAuth) > 0,
		)
	}

	// ── Role dispatch ──
	roleCtx, roleCancel := context.WithCancel(ctx)
	defer roleCancel()
	roleErrCh := make(chan error, 1)
	var roleWG sync.WaitGroup
	roleWG.Add(1)
	go func() {
		defer roleWG.Done()
		var rerr error
		switch whoami.Role {
		case "mta":
			rerr = mta.Run(roleCtx, mta.Deps{
				Snapshot:       snapshot,
				TLSProvider:    tlsProvider,
				Client:         reconnector,
				Logger:         logger,
				BridgeID:       whoami.BridgeID,
				MTABindAddr:    mtaBindAddr,
				MTABindAddr465: mtaBindAddr465,
				MTABindAddr587: mtaBindAddr587,
				// The bulk-byte plane, where an over-frame sealed body is staged
				// before the ingest RPC carries only its chunk hashes
				// (smtp-server.md § Message size limits). Same endpoint + client
				// the MDA is given below.
				NestBaseURL:           *nestEndpoint,
				NestHTTPClient:        nestHTTP,
				MXResolver:            mtaMXResolverFor(reconnector),
				ScanConfig:            scanConfig,
				ShutdownGrace:         shutdownGrace,
				PublishConfigReloader: roleConfigReloader.Store,
			})
		case "mda":
			rerr = mda.Run(roleCtx, mda.Deps{
				Snapshot:               snapshot,
				TLSProvider:            tlsProvider,
				Client:                 reconnector,
				Logger:                 logger,
				BridgeID:               whoami.BridgeID,
				CapabilityX25519Secret: capabilityX25519Secret,
				CapabilityMlkemDk:      bridgeMlkemDk,
				ScanConfig:             scanConfig,
				IMAPListenImplicitTLS:  mdaAddrs.IMAPImplicitTLS,
				IMAPListenStartTLS:     mdaAddrs.IMAPStartTLS,
				CalDAVListenHTTPS:      mdaAddrs.CalDAVHTTPS,
				CalDAVBindIsAdminPort:  caldavBindIsAdminPort,
				RouterProxyAuth:        routerProxyAuth,
				NestBaseURL:            *nestEndpoint,
				NestHTTPClient:         nestHTTP,
				ShutdownGrace:          shutdownGrace,
				PublishConfigReloader:  roleConfigReloader.Store,
			})
		}
		roleErrCh <- rerr
	}()

	logger.Info("fauna-mail-bridge ready", "role", whoami.Role, "bridge_id", whoami.BridgeID)
	logplane.Ready(whoami.Role)

	// ── Wait for signal or unexpected failure ──
	select {
	case <-ctx.Done():
		logger.Info("shutdown signal received", "err", ctx.Err())
	case <-reconnector.Done():
		// The reconnect loop stopped permanently. The only non-Close reason
		// is a revoke (whoami status=revoked on a reconnect) — treat it like
		// any other shutdown trigger (mail-bridge-lifecycle.md § Shutting
		// down) and fall through to the graceful drain below.
		if err := reconnector.Err(); errors.Is(err, wsrpc.ErrBridgeRevoked) {
			// Deliberately NOT a log-plane event: by the time we know, nest's
			// capability gate is refusing every kind and the reconnector has
			// torn the connection down, so it could never be delivered — and
			// nest, having done the revoking, already has the fact. See
			// internal/logplane/catalogue.go § Deliverability.
			logger.Warn("nest revoked this bridge's service user; shutting down")
		} else {
			logger.Info("ws-rpc connection permanently closed; shutting down", "err", err)
		}
	case err := <-roleErrCh:
		if err != nil {
			logger.Error("role.Run returned an error", "role", whoami.Role, "err", err)
			roleCancel()
			roleWG.Wait()
			shutdownMetrics(metricsSrv, logger)
			return fmt.Errorf("%s.Run: %w", whoami.Role, err)
		}
		// Role.Run returned nil unexpectedly — shouldn't happen (they
		// block on ctx.Done), but be defensive.
		logger.Warn("role.Run returned nil unexpectedly; treating as clean shutdown", "role", whoami.Role)
	}

	// Graceful shutdown: cancel the role context (triggering the listeners'
	// drain sequence — stop accepting, refuse new work [MTA: 421 on new MAIL
	// FROM; MDA: BYE on new IMAP command], drain up to the grace window, then
	// force-close), wait for the role goroutine, then stop the metrics server.
	roleCancel()
	roleWG.Wait()
	shutdownMetrics(metricsSrv, logger)

	// Learn whether the drain completed cleanly or had to force-close. In
	// the ctx.Done() path the role goroutine's result is buffered on
	// roleErrCh; in the role-returned-nil path it was already consumed (the
	// non-blocking read then takes the default branch → treated as clean).
	var roleResult error
	select {
	case roleResult = <-roleErrCh:
	default:
	}
	switch {
	case errors.Is(roleResult, bridgeshutdown.ErrShutdownForced):
		// Expected outcome of a busy bridge being stopped: the drain window
		// expired with transactions still in flight (MTA or MDA role). Exit 1
		// per mail-bridge-lifecycle.md § Shutting down step 6 — main()
		// recognises this sentinel and exits 1 without a stderr crash dump.
		logger.Warn("fauna-mail-bridge force-closed in-flight connections (grace window expired)")
		logplane.ShutdownForced()
		return roleResult
	case roleResult != nil:
		logger.Error("role returned an error during shutdown", "role", whoami.Role, "err", roleResult)
		return fmt.Errorf("%s.Run: %w", whoami.Role, roleResult)
	}
	logger.Info("fauna-mail-bridge stopped cleanly")
	return nil
}

// loadOperatorHatch resolves the operator-hatch path from the two
// argv flags and reads it if present. Returns (nil, nil) when no
// operator-hatch is configured — the no-overrides case is normal
// for a fresh deployment.
//
// Precedence: --operator-hatch wins if non-empty. Otherwise, if
// --data-dir is set, we look for <data-dir>/operator-hatch.toml and
// load it ONLY if the file exists (missing-file is silently OK; an
// unreadable file is a hard error).
func loadOperatorHatch(operatorHatch, dataDir string) (*config.OperatorHatch, error) {
	switch {
	case operatorHatch != "":
		return config.Load(operatorHatch)
	case dataDir != "":
		path := filepath.Join(dataDir, "operator-hatch.toml")
		_, err := os.Stat(path)
		switch {
		case err == nil:
			return config.Load(path)
		case errors.Is(err, os.ErrNotExist):
			return nil, nil
		default:
			return nil, fmt.Errorf("stat %s: %w", path, err)
		}
	default:
		return nil, nil
	}
}

// metricsMux returns the localhost-only mux mounting /metrics +
// /healthz. /healthz returns 200 OK with the static body "ok\n" — a
// minimal liveness probe for s6/systemd watchdogs.
func metricsMux() http.Handler {
	mux := http.NewServeMux()
	mux.Handle("/metrics", metrics.Handler())
	mux.HandleFunc("/healthz", func(w http.ResponseWriter, _ *http.Request) {
		w.Header().Set("Content-Type", "text/plain; charset=utf-8")
		_, _ = io.WriteString(w, "ok\n")
	})
	return mux
}

// roleHintFromKeyfile derives the advisory enrollment role_hint from the
// keypair file's basename: "/data/keys/mta.key" → "mta". nest validates it is
// "mta"/"mda" (rejecting anything else) and the admin confirms it at approval;
// the bridge's authoritative operating role still comes from whoami
// post-approval (mail-bridge-lifecycle.md § Cold boot step 3 — the keyfile path
// is a deployment-topology argv slot, not a forbidden --mode flag).
func roleHintFromKeyfile(keypairFile string) string {
	base := filepath.Base(keypairFile)
	return strings.TrimSuffix(base, filepath.Ext(base))
}

// otherRole returns the role this bridge is NOT running. Used to zero
// out the `bridge_role` gauge for the role this process didn't take so
// fleet-level sums are well-defined ("how many MTAs are up" =
// `sum(bridge_role{role="mta"})`). Whoami validates the input role
// upstream, so the panic branch should never trip.
func otherRole(r string) string {
	switch r {
	case "mta":
		return "mda"
	case "mda":
		return "mta"
	default:
		// Defensive — Whoami already rejected anything else, but if
		// a future role variant lands without updating this helper
		// we want to know at startup, not silently miscount.
		panic(fmt.Sprintf("otherRole: unknown role %q (only \"mta\" / \"mda\" supported)", r))
	}
}

// shutdownMetrics gracefully stops the metrics HTTP server. Best-
// effort: a 5s deadline; ignore the error type if it's the
// expected http.ErrServerClosed.
func shutdownMetrics(srv *http.Server, logger *slog.Logger) {
	shutdownCtx, cancel := context.WithTimeout(context.Background(), 5*time.Second)
	defer cancel()
	if err := srv.Shutdown(shutdownCtx); err != nil && !errors.Is(err, http.ErrServerClosed) {
		logger.Warn("metrics server shutdown error", "err", err)
	}
}
