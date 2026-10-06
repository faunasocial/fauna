// Package mda is the MDA role's main loop.
//
// This is the role the bridge runs when `fauna.bridges.whoami` resolves
// its role to `"mda"`: the user-facing read surface for encrypted mail
// (IMAP) and calendars (CalDAV). The MDA fetches per-actor sealed rows
// from nest and serves them to MUAs over the IMAP / CalDAV wire,
// decrypting in-session via the AUTH'd actor's wrapped-blob keychain.
//
// Run binds up to three listener sockets: the shared DAV `:443` (CalDAV +
// CardDAV, terminated once and path-muxed — internal/mda/dav), IMAPS `:993`
// implicit-TLS, and IMAP `:143` STARTTLS. They share one nest WS-RPC client,
// one TLS cert provider, and (for the two IMAP variants) one Backend +
// BODYSTRUCTURE cache. Email (IMAP), CalDAV, and CardDAV gate **independently**:
// IMAP binds iff `Snapshot.MailEnabled`, CalDAV iff `Snapshot.CalDAVEnabled`,
// CardDAV iff `Snapshot.CardDAVEnabled` (caldav-server.md § Independent
// enablement; the CardDAV twin rides the same 443 listener — bridge_routing.rs,
// "CardDAV rides the existing caldav_port, no separate port"). One MDA process
// can serve any subset — calendar without email, contacts without calendar,
// etc.; with all three off it idles. A non-empty `Snapshot.LocalDomains` is not
// required (the any-locator floor-cert path serves DAV/IMAP on a bare-IP nest).
// When an admin flips any toggle, nest's `config_changed` push makes the
// process exit cleanly so s6 restarts it bound to the new listener set (the
// in-process set can't be re-bound live).
package mda

import (
	"context"
	"crypto/tls"
	"errors"
	"fmt"
	"log/slog"
	"net"
	"net/http"
	"sync"
	"time"

	"github.com/faunasocial/fauna/bins/fauna-bridges/internal/bridgeshutdown"
	"github.com/faunasocial/fauna/bins/fauna-bridges/internal/byteplane"
	"github.com/faunasocial/fauna/bins/fauna-bridges/internal/connlimit"
	"github.com/faunasocial/fauna/bins/fauna-bridges/internal/logplane"
	"github.com/faunasocial/fauna/bins/fauna-bridges/internal/mailfauna"
	"github.com/faunasocial/fauna/bins/fauna-bridges/internal/mda/caldav"
	"github.com/faunasocial/fauna/bins/fauna-bridges/internal/mda/carddav"
	"github.com/faunasocial/fauna/bins/fauna-bridges/internal/mda/dav"
	"github.com/faunasocial/fauna/bins/fauna-bridges/internal/mda/imap"
	"github.com/faunasocial/fauna/bins/fauna-bridges/internal/mda/webdav"
	"github.com/faunasocial/fauna/bins/fauna-bridges/internal/metrics"
	"github.com/faunasocial/fauna/bins/fauna-bridges/internal/proxyproto"
	"github.com/faunasocial/fauna/bins/fauna-bridges/internal/scan"
	bridgetls "github.com/faunasocial/fauna/bins/fauna-bridges/internal/tls"
	"github.com/faunasocial/fauna/bins/fauna-bridges/internal/wsrpc"
)

// configReloadFetchTimeout bounds the fetch_config round-trip the config
// reloader issues when a `fauna.bridges.config_changed` push (or a
// reconnect) prompts a re-fetch. Generous — a slow re-fetch keeps the
// last-good config until it lands, no worse than before.
const configReloadFetchTimeout = 30 * time.Second

// Deps is the dependency-injection bundle main passes to Run. Names
// every dependency the MDA listeners need; gate fields below the
// startup-gate comment may be empty when the listener is intentionally
// idle (mail disabled or domain not yet ratified by nest).
type Deps struct {
	// Snapshot is the bridge's current config snapshot. `MailEnabled`
	// gates listener startup; on `false` Run idles until ctx is cancelled.
	// `Snapshot.IMAP.BodyStructureCacheMax` sizes the IMAP
	// BODYSTRUCTURE LRU shared by both 993 + 143 listeners.
	Snapshot wsrpc.ConfigSnapshot
	// TLSProvider serves the live TLS cert via GetCertificate. Required
	// once `MailEnabled && len(Snapshot.LocalDomains) > 0` — all three
	// listeners terminate TLS in-process via this provider (CalDAV
	// 443 via tls.NewListener, IMAPS 993 via tls.NewListener, IMAP 143
	// via in-band STARTTLS that the emersion/go-imap library upgrades
	// using the same tls.Config).
	TLSProvider *bridgetls.Provider
	// Client is the WS-RPC caller to nest. Every per-session CalDAV /
	// IMAP backend instance pins it for the duration of the AUTH'd session
	// (the auth middleware stashes it in request context for CalDAV; the
	// imap.Session stashes it as a field). Typed as the Caller interface so
	// the production process passes a *wsrpc.ReconnectingClient (survives
	// nest blips, mail-bridge-lifecycle.md § Reconnecting) — the IMAP
	// backend's push-handler install asserts the SetOnPush capability,
	// which the reconnector also satisfies; a plain *wsrpc.Client works too.
	Client wsrpc.Caller
	// Logger is the per-role structured logger (role="mda" attribute
	// already attached).
	Logger *slog.Logger
	// BridgeID is the deployment-time bridge identity nest resolved
	// via Whoami.
	BridgeID string

	// CapabilityX25519Secret is the bridge's enrolled service-user X25519
	// private key (the same 32-byte secret the TLS blob uses — NOT the actor
	// identity), used to HPKE-Open the capability grants the user minted to
	// this holder. When present (32 bytes) the MDA keeps its legacy mail-serve
	// role AND becomes a capability holder (capability-mediated-content-
	// processing design § 2.4), so a re-score/re-index drain (Slice 5A) can
	// transiently wield a scoped content key on an untrusted box. Absent/short
	// (localhost / pre-enrollment) → the holder loop is a no-op. main.go
	// supplies it for the MDA role only.
	CapabilityX25519Secret []byte

	// CapabilityMlkemDk is the holder's 2400-byte ML-KEM-768 decapsulation key
	// (PQ-CAP-2), derived from the bridge's Ed25519 keyfile seed, so the holder
	// opens BOTH classical and hybrid (X-Wing) capability-grant wraps with one
	// path. nil only in tests (classical grants only); main.go always
	// supplies it for the MDA role alongside CapabilityX25519Secret.
	CapabilityMlkemDk []byte

	// ScanConfig carries the co-resident clamd/rspamd addresses + policy —
	// the same deployment topology main.go resolves for the MTA role. The MDA
	// wields it in the re-score drain (design § 2.5: the capability holder
	// re-runs the co-resident scanners on content it transiently unseals
	// under a user-minted grant; the sidecars are loopback-reachable from any
	// role on the box).
	ScanConfig scan.Config

	// IMAPListenImplicitTLS is the bind address for the IMAPS
	// (implicit-TLS) listener. Defaults to ":993" when the
	// operator-hatch leaves it empty. main.go applies the default
	// before calling Run; an empty value here is a wiring bug.
	IMAPListenImplicitTLS string
	// IMAPListenStartTLS is the bind address for the IMAP (STARTTLS)
	// listener. Defaults to ":143". See IMAPListenImplicitTLS above.
	IMAPListenStartTLS string
	// CalDAVListenHTTPS is the bind address for the shared DAV-over-HTTPS
	// listener (443). main.go resolves it from the operator-hatch (when set) or,
	// when no hatch pins it, the admin-set caldav_port (default 8443) — see
	// resolveMDAListenAddrs. Both DAV protocols ride this one addr: CalDAV serves
	// at `/caldav/{user}/` (and the root principal `/{user}/`), CardDAV at
	// `/carddav/{user}/` — path-muxed by internal/mda/dav (bridge_routing.rs:
	// "CardDAV rides the existing caldav_port, no separate port"). The field name
	// is retained (CalDAV predates CardDAV) rather than renamed to avoid churning
	// the main.go wiring for a purely cosmetic rename.
	CalDAVListenHTTPS string
	// CalDAVBindIsAdminPort is true when the CalDAV listener address came from
	// the admin-set caldav_port (no operator-hatch caldav_listen_https). Only
	// then does a `set_caldav_port` config_changed trigger a supervisor rebind:
	// on a hatch-pinned box (a domain box's loopback IPC port, a desktop
	// supervisor's <iface>:<port>) the admin port doesn't drive the bind, so a
	// port change is a no-op and must NOT pointlessly restart the MDA. main.go
	// sets this for the MDA role from whether the hatch pins the CalDAV listener.
	CalDAVBindIsAdminPort bool
	// RouterProxyAuth is the router-auth secret a PROXY-v2 header must carry
	// (as its router-auth TLV) before the DAV listener honours the source address
	// it conveys — see internal/proxyproto's trust model and
	// docs/goal/architecture/security.md § Co-resident process trust boundary.
	// Provisioned by the artifact (/data/keys/router/proxy-secret, read as root by
	// the MDA run-script and env-passed); empty on a non-router box or a
	// provisioning gap, which keeps the trust-any-loopback behaviour.
	RouterProxyAuth []byte

	// NestBaseURL is nest's external base URL (`https://host`, no /api/v1 suffix)
	// — the same value the WS-RPC dial uses. The WebDAV terminator's byte-route
	// client (chunk/manifest upload+download) hits it; empty on non-WebDAV boxes.
	NestBaseURL string
	// NestHTTPClient is the shared HTTP client for the WebDAV byte routes (reuses
	// the WS-RPC dial's client so the in-container loopback nest's TLS-skip
	// behaviour is consistent). main.go supplies it for the MDA role.
	NestHTTPClient *http.Client

	// ShutdownGrace is the graceful-shutdown drain budget (T2.6), sourced
	// from snapshot.Bridge.ShutdownGraceSeconds. main.go threads it in
	// uniformly with the MTA role; on ctx cancel the listener runners honor it
	// (shared DAV 443: http drain; IMAP: `* BYE` new commands + drain in-flight
	// up to grace, then force-close) — see runIMAPListener / runDAVListener.
	ShutdownGrace time.Duration

	// PublishConfigReloader, if non-nil, is called once with the
	// wsrpc.ConfigReloader Run builds, so main.go can route its reconnect
	// re-fetch through the same Apply seam as the config_changed push (a
	// config changed during a WS gap then hot-applies on reconnect, not only
	// on the next push). Optional — nil in tests / standalone. The reloader is
	// goroutine-safe (Apply takes the RWMutex), so the cross-goroutine handoff
	// is sound; main.go holds it in an atomic.Pointer.
	PublishConfigReloader func(*wsrpc.ConfigReloader)
}

// caldavPortRebindNeeded reports whether a config_changed snapshot's CalDAV port
// requires a listener rebind (exit 0 → supervisor restart). It fires only when
// the bind is admin-port-driven (adminDriven) AND the effective new port differs
// from the effective port the listener bound at startup. startPortEffective is
// the already-normalized startup port; the new port is normalized here too
// (a 0 → DefaultCalDAVPort defensive clamp; the current nest always sends a
// non-zero port) so it never reads as a
// change against a defaulted startup port (and so a hatch-pinned box, where the
// admin port doesn't drive the bind, never restarts on a port change).
func caldavPortRebindNeeded(adminDriven bool, startPortEffective, newPort uint16) bool {
	return adminDriven && wsrpc.EffectiveCalDAVPort(newPort) != startPortEffective
}

// Bindable reports whether a snapshot enables at least one of the MDA's
// protocols — i.e. whether an MDA cold-booting on it would bind a listener
// rather than idle.
//
// It is the single source of truth for Run's all-off idle gate: the branch that
// decides to idle and the idle watcher's "have my gates opened?" applier are the
// same predicate read from opposite sides, so the MDA can never idle on a
// snapshot it would also wake for, nor wake for one it would then idle on (a
// restart loop). The serving path's gating watcher is deliberately NOT this
// predicate — it fires on any change to the four-flag TUPLE, including one that
// merely swaps which subset is on.
func Bindable(s wsrpc.ConfigSnapshot) bool {
	return s.MailEnabled || s.CalDAVEnabled || s.CardDAVEnabled || s.WebDAVEnabled
}

// Run is the MDA role's main loop.
//
// Startup decision tree (mirrors mta.Run for symmetry):
//
//  1. every protocol disabled (`!Bindable`) → idle, but STAY SUBSCRIBED
//     (wsrpc.IdleUntilGatesOpen): a `config_changed` push (or reconnect
//     re-fetch) that enables any protocol ends the idle and Run returns nil →
//     exit 0 → the supervisor rebinds a process that cold-boots bound. This is
//     the same exit-for-rebind step 5's gating watcher performs while serving;
//     before it existed, an all-off MDA was the one state from which an admin's
//     enable was invisible until something else restarted the process.
//  2. `len(Snapshot.LocalDomains) == 0` → DON'T idle: a domainless / bare-IP /
//     localhost nest still serves CalDAV (and local IMAP) under the self-signed
//     floor cert + handle-keyed auth (the any-locator design; tracked
//     internally). Only
//     the MTA role keeps its own no-domain idle gate (external mail needs a
//     domain). So this case logs and falls through to bind the enabled subset.
//  3. An enabled protocol's listen addr empty → error (wiring bug at
//     main.go; a misconfigured listener should fail loudly at startup
//     rather than silently swallowing traffic to that surface).
//  4. `TLSProvider == nil` → error (every listener requires terminated
//     TLS; a nil provider at this point is a wiring bug).
//  5. Otherwise → bind the enabled subset (IMAP iff MailEnabled; the shared DAV
//     443 listener iff CalDAVEnabled || CardDAVEnabled, muxing /caldav + /carddav
//     by which flags are on), serve until ctx cancel or first listener error. On
//     ctx cancel each listener grace-drains (DAV: http drain; IMAP: `* BYE` new
//     commands + drain in-flight up to deps.ShutdownGrace, then force-close);
//     on first serve error the shared listenerCtx is cancelled so siblings tear
//     down too. A `config_changed` push that flips the gating tuple also
//     cancels listenerCtx → Run returns nil → exit 0 → s6 rebind. A forced
//     drain surfaces as bridgeshutdown.ErrShutdownForced (→ main exit 1).
func Run(ctx context.Context, deps Deps) error {
	logger := deps.Logger
	if logger == nil {
		logger = slog.Default()
	}
	// Per-protocol enablement: email (IMAP 993/143) gates on MailEnabled; CalDAV
	// and CardDAV each gate on their own flag, independently (caldav-server.md
	// § Independent enablement). CalDAV + CardDAV share the one 443 DAV listener
	// (bridge_routing.rs: "CardDAV rides the existing caldav_port, no separate
	// port"), path-muxed by internal/mda/dav. The MDA serves whichever subset is
	// on; with all three off it idles (the supervisor will eventually down the
	// process, but a mid-transition snapshot must not crash).
	mailOn := deps.Snapshot.MailEnabled
	caldavOn := deps.Snapshot.CalDAVEnabled
	carddavOn := deps.Snapshot.CardDAVEnabled
	webdavOn := deps.Snapshot.WebDAVEnabled
	if !mailOn && !caldavOn && !carddavOn && !webdavOn {
		logger.Info("mda.Run idling: mail, CalDAV, CardDAV, and WebDAV all disabled by admin config",
			"local_domains_count", len(deps.Snapshot.LocalDomains),
			"primary_domain", deps.Snapshot.PrimaryDomain,
			"bridge_id", deps.BridgeID,
		)
		// Stay subscribed while idling and exit for rebind the moment any
		// protocol is enabled — the same exit-for-rebind the gating watcher
		// below arms once this Run is serving, now reachable from the one
		// state that previously observed no config change at all.
		opened := wsrpc.IdleUntilGatesOpen(
			ctx,
			deps.Client,
			deps.PublishConfigReloader,
			configReloadFetchTimeout,
			logger.With("component", "idle_gate_watch"),
			Bindable,
		)
		if opened {
			logger.Info("mda.Run shutting down", "reason", "a protocol was enabled while idling; exiting for supervisor rebind")
			return nil
		}
		logger.Info("mda.Run shutting down", "reason", ctx.Err())
		return nil
	}
	if len(deps.Snapshot.LocalDomains) == 0 {
		// Domainless / bare-IP / localhost nest (the any-locator design;
		// tracked internally):
		// there is no registered mail-domain, but CalDAV (and local IMAP) still
		// serve. The MDA terminates its own TLS with nest's self-signed FLOOR cert
		// (main.go builds the TLS provider even with an empty PrimaryDomain,
		// fetching under a floor sentinel) and local login resolves the bare handle
		// via the handle→actor store (Change A, validate_recipient fallback). So
		// DON'T idle — fall through and bind the enabled protocols. (External email
		// genuinely needs a domain, but that is the separate MTA role, which keeps
		// its own no-domain idle gate; this MDA path is local CalDAV/IMAP serving.)
		logger.Info("mda.Run: no mail_domains rows — serving any-locator (floor cert + handle-keyed auth)",
			"bridge_id", deps.BridgeID,
			"mail_enabled", mailOn,
			"caldav_enabled", caldavOn,
			"carddav_enabled", carddavOn,
			"webdav_enabled", webdavOn,
		)
	}
	// Wiring-bug checks scoped to the surfaces we will actually bind: main.go
	// defaults all addrs, so an empty one for an *enabled* protocol is a wiring
	// bug that should fail loudly rather than silently swallow traffic. The
	// shared DAV 443 addr is needed whenever CalDAV *or* CardDAV is on (both ride
	// it), so a contacts-only deployment must have it resolved too.
	if (caldavOn || carddavOn || webdavOn) && deps.CalDAVListenHTTPS == "" {
		return fmt.Errorf("mda.Run: CalDAVListenHTTPS is empty (main.go resolveMDAListenAddrs should resolve the shared DAV 443 addr from the hatch or the admin caldav_port)")
	}
	if mailOn && deps.IMAPListenImplicitTLS == "" {
		return fmt.Errorf("mda.Run: IMAPListenImplicitTLS is empty (main.go should default to :993)")
	}
	if mailOn && deps.IMAPListenStartTLS == "" {
		return fmt.Errorf("mda.Run: IMAPListenStartTLS is empty (main.go should default to :143)")
	}
	if deps.TLSProvider == nil {
		return fmt.Errorf("mda.Run: TLSProvider is nil (CalDAV + IMAP listeners require terminated TLS)")
	}

	tlsConfig := &tls.Config{
		GetCertificate: deps.TLSProvider.GetCertificate,
		MinVersion:     tls.VersionTLS12,
	}

	// listenerCtx cancels every bound listener together — a sibling serve
	// error, the parent ctx, or a rebind-on-toggle (the config reloader's
	// gating watcher below). Created up front so that watcher can capture it.
	listenerCtx, cancel := context.WithCancel(ctx)
	defer cancel()

	// One serve runner per bound listener. We bind only the enabled surfaces
	// (IMAP iff mailOn, CalDAV iff caldavOn); a partial bind failure closes
	// what we already grabbed so we don't leak file descriptors.
	type boundListener struct {
		name string
		ln   net.Listener
		run  func(context.Context) error
	}
	var bound []boundListener
	closeBound := func() {
		for _, b := range bound {
			_ = b.ln.Close()
		}
	}

	// The role's one bulk-byte-plane client, shared by every read path that can
	// meet a body too large for the 2 MiB WS-RPC frame (IMAP serve + the
	// re-score drain; the WebDAV terminator builds its own from the same two
	// deps). Chunk downloads are open routes, so it carries no token. Nil on a
	// box with no nest base URL wired — a by-reference body then fails closed
	// rather than silently unsealing zero bytes.
	var bytePlane *byteplane.Client
	if deps.NestBaseURL != "" {
		bytePlane = byteplane.New(deps.NestBaseURL, deps.NestHTTPClient)
	}

	// Push handling: build the dispatcher + config reloader regardless of which
	// protocols are enabled. The reloader carries (a) the gating watcher that
	// rebinds the listener set on a set_{mail,caldav}_enabled toggle, and (b)
	// when IMAP is enabled, the backend's IMAP-IDLE-timeout hot-reload. A
	// kind-routed dispatcher is the single installed push handler.
	pushDispatcher := wsrpc.NewPushDispatcher(logger.With("component", "push_dispatcher"))
	configReloader := wsrpc.NewConfigReloader(deps.Client, configReloadFetchTimeout, logger.With("component", "config_reloader"))

	// Prompt-refresh-on-provision (mail-bridge-lifecycle.md § TLS provisioning):
	// TLS cert blobs are out of fetch_config's scope and the provider otherwise
	// re-fetches only on its 12 h timer, so a cert that lands on the nest while
	// this MDA is running (a domain added post-claim → ACME issues a cert
	// covering mail.<domain>; an admin provision_self_signed_cert) would not be
	// served until that timer / a restart. Nest fires a `config_changed` push
	// (reason "tls") when a cert lands; re-fetch TLS on ANY config_changed rather
	// than parse the reason token — TriggerRefresh is a
	// cheap, coalesced async poke, and seal-on-read hands back the live disk
	// cert regardless of the provider's fetch domain.
	if deps.TLSProvider != nil {
		configReloader.Register(func(_ wsrpc.ConfigSnapshot) {
			deps.TLSProvider.TriggerRefresh()
		})
	}

	// Capability holder (capability-mediated-content-processing design § 2.4):
	// the MDA keeps its legacy mail-serve role AND becomes a capability holder,
	// fetching + HPKE-Opening the content grants the user minted to this
	// bridge's enrolled x25519 pubkey. Started here from the MDA role's Run
	// so Slice 5A's re-score/re-index
	// drain can transiently wield a scoped content key on an untrusted box.
	// No-op (nil) when the holder secret is absent/short (localhost /
	// pre-enrollment). Runs under its own child of ctx so teardown is ordered:
	// cancel the loop (holderCancel) THEN zeroize (Close) — Close's contract
	// requires the loop to have exited first.
	holderCtx, holderCancel := context.WithCancel(ctx)
	capHolder, err := startCapabilityHolder(holderCtx, capHolderDeps{
		caller:   deps.Client,
		secret:   deps.CapabilityX25519Secret,
		mlkemDk:  deps.CapabilityMlkemDk,
		logger:   logger.With("component", "capability_holder"),
		reloader: configReloader,
	})
	if err != nil {
		holderCancel()
		return fmt.Errorf("mda.Run: %w", err)
	}

	// The re-score drain worker (design § 2.5 step 4) rides the holder: only
	// a live capability holder can unseal, so no holder → no drain (localhost /
	// pre-enrollment serves mail without one, exactly as before). Runs are
	// triggered at startup, on every config_changed push (a nest-side
	// mint/revoke, or the reconnect re-fetch after a nest deploy — exactly when
	// a built-in model version bumps), and by the 12 h backstop. Teardown is
	// ordered: cancel holderCtx (stopping loop + drain) → wait for the drain
	// goroutine to exit → Close() (which itself joins the holder loop before
	// zeroizing), so no drain Refresh/KeyFor races the wipe.
	var drainDone <-chan struct{}
	var spamBaselineDrainDone <-chan struct{}
	if capHolder != nil {
		drain := newRescoreDrain(rescoreDrainDeps{
			registry: capHolder,
			caller:   deps.Client,
			plane:    bytePlane,
			scanCfg:  deps.ScanConfig,
			logger:   logger.With("component", "rescore_drain"),
		})
		configReloader.Register(func(_ wsrpc.ConfigSnapshot) {
			drain.poke()
		})
		// The ingest fast path (S5 Arm 1): nest emits a `rescore_ready` push
		// the moment a delivery seeds a new per-user obligation, so the drain
		// runs moments after delivery rather than waiting for the next
		// config_changed / 12 h trigger. Registered only when a holder (→ a
		// drain) exists; the drain's poke is the non-blocking coalescing send.
		pushDispatcher.Register(
			wsrpc.PushKindRescoreReady,
			wsrpc.NewRescoreReadyHandler(drain.poke, logger.With("component", "rescore_ready")).Handle,
		)
		drain.start(holderCtx)
		drainDone = drain.done

		// The spam-baseline publish drain (mail-spam.md § Encrypted-mode
		// interaction, ratified 2026-07-13) — the third holder-pull drain
		// instance. An admin `publish_spam_baseline` pokes this holder with a
		// run_id; the worker pulls the run's grant-gated sealed-copy worklist,
		// unseal-merges it OFF-BOX with the holder's OWN service-user key halves
		// (the same halves the re-score drain wields), and submits the merged
		// half back. Purely push-driven — no config_changed/backstop trigger, no
		// standing obligation between publish windows.
		spamDrain := newSpamBaselineDrain(spamBaselineDrainDeps{
			caller:             deps.Client,
			holderX25519Secret: deps.CapabilityX25519Secret,
			holderMlkemDk:      deps.CapabilityMlkemDk,
			logger:             logger.With("component", "spam_baseline_drain"),
		})
		pushDispatcher.Register(
			wsrpc.PushKindSpamBaselinePublish,
			wsrpc.NewSpamBaselinePublishHandler(spamDrain.poke, logger.With("component", "spam_baseline_publish")).Handle,
		)
		spamDrain.start(holderCtx)
		spamBaselineDrainDone = spamDrain.done
	}
	defer func() {
		holderCancel()
		if drainDone != nil {
			<-drainDone
		}
		if spamBaselineDrainDone != nil {
			<-spamBaselineDrainDone
		}
		if capHolder != nil {
			capHolder.Close()
		}
	}()

	// Rebind-on-toggle: the MDA binds its listener set once, at startup. When an
	// admin flips set_mail_enabled / set_caldav_enabled / set_carddav_enabled,
	// nest sends a `config_changed` push; if the new gating tuple differs from
	// what we bound, cancel the listeners so Run returns nil → the process exits
	// 0 and s6 restarts it bound to the new protocol set (the in-process listener
	// set can't be re-bound live; a fresh process is the simplest correct rebind,
	// caldav-server.md § Independent enablement). CardDAV rides the same 443
	// listener as CalDAV, but toggling it on/off still changes which handler is
	// mounted on that listener, so it needs a rebind too (an enable adds the
	// /carddav mount; a disable removes it). A change that leaves gating untouched
	// (e.g. a policy edit) does not restart.
	// WebDAV rides the same 443 listener; toggling it on/off changes which handler
	// is mounted (an enable adds the /webdav mount, a disable removes it), so it
	// needs the same exit-for-rebind as CalDAV/CardDAV.
	startMail, startCalDAV, startCardDAV, startWebDAV := mailOn, caldavOn, carddavOn, webdavOn
	configReloader.Register(func(snap wsrpc.ConfigSnapshot) {
		if snap.MailEnabled != startMail || snap.CalDAVEnabled != startCalDAV || snap.CardDAVEnabled != startCardDAV || snap.WebDAVEnabled != startWebDAV {
			logger.Info("mda listener gating changed; exiting for s6 rebind",
				"mail_was", startMail, "mail_now", snap.MailEnabled,
				"caldav_was", startCalDAV, "caldav_now", snap.CalDAVEnabled,
				"carddav_was", startCardDAV, "carddav_now", snap.CardDAVEnabled,
				"webdav_was", startWebDAV, "webdav_now", snap.WebDAVEnabled,
			)
			cancel()
		}
	})

	// Rebind-on-port-change: the admin can move the shared DAV listener port from
	// any client (set_caldav_port → caldav_port nest state → config_changed);
	// CardDAV rides this same port, so a port move rebinds the CardDAV mount too.
	// The in-process listener can't be re-bound live, so — same mechanism as the
	// gating watcher above — cancel the listeners → Run returns nil → exit 0 →
	// the supervisor restarts the MDA bound to the new port (caldav-server.md
	// § Network exposure). Only fires when the bind is admin-port-driven: on a
	// hatch-pinned box (a domain box's loopback IPC port the SNI router targets,
	// a desktop supervisor's <iface>:<port>) the admin port doesn't drive the
	// bind, so a port change is a no-op there and must not restart it.
	startCalDAVPort := wsrpc.EffectiveCalDAVPort(deps.Snapshot.CalDAVPort)
	configReloader.Register(func(snap wsrpc.ConfigSnapshot) {
		if caldavPortRebindNeeded(deps.CalDAVBindIsAdminPort, startCalDAVPort, snap.CalDAVPort) {
			logger.Info("mda caldav admin port changed; exiting for supervisor rebind",
				"caldav_port_was", startCalDAVPort,
				"caldav_port_now", wsrpc.EffectiveCalDAVPort(snap.CalDAVPort),
			)
			cancel()
		}
	})

	// One per-IP concurrent-connection limiter shared by every authenticated MDA
	// listener (IMAP 993/143 + the shared DAV 443), so a source's total
	// simultaneous connections across them are counted together (the catalog default's
	// "several devices × persistent IMAP IDLE" rationale). Fed from
	// AuthPolicy.max_conn_per_ip and hot-reloaded on config_changed — the Go
	// analogue of the nest TLS loop's fauna_conn_limit::PerIpConnLimit
	// (smtp-server.md § Connection-time limits; 0 = disabled, loopback exempt).
	perIPLimiter := connlimit.NewPerIPLimiter(deps.Snapshot.Auth.MaxConnPerIP)
	configReloader.Register(func(snap wsrpc.ConfigSnapshot) {
		perIPLimiter.SetMax(snap.Auth.MaxConnPerIP)
	})

	// Bind the shared DAV (443) surface iff CalDAV or CardDAV is enabled. Both
	// protocols terminate TLS once on this one listener and are path-muxed by
	// internal/mda/dav: `/caldav/…` + the root principal `/{user}/` → the CalDAV
	// chain, `/carddav/…` → the CardDAV chain (bridge_routing.rs: "CardDAV rides
	// the existing caldav_port, no separate port").
	if caldavOn || carddavOn || webdavOn {
		rawDAV, err := net.Listen("tcp", deps.CalDAVListenHTTPS)
		if err != nil {
			closeBound()
			logplane.ListenerBindFailed("caldav", logplane.PortOf(deps.CalDAVListenHTTPS))
			return fmt.Errorf("dav(443) listen %s: %w", deps.CalDAVListenHTTPS, err)
		}

		var caldavHandler, carddavHandler, webdavHandler http.Handler
		if caldavOn {
			caldavSrv := caldav.NewServer(caldav.ServerConfig{
				TLSConfig:                tlsConfig,
				Logger:                   logger.With("listener", "dav-443"),
				MaxAuthFailuresPerMinute: deps.Snapshot.Auth.MaxAuthFailuresPerMinute,
				PrimaryDomain:            deps.Snapshot.PrimaryDomain,
				LocalDomains:             deps.Snapshot.LocalDomains,
				MailEnabled:              deps.Snapshot.MailEnabled,
				// Unified root principal: when CardDAV is also on, this CalDAV chain
				// owns the mux catch-all `/` (davMounts below), so the shared
				// `/{user}/` principal must ALSO advertise addressbook-home-set for
				// host-only CardDAV autodiscovery. A carddav_enabled toggle triggers a
				// full 443-rebind (this block re-runs), so the value stays live.
				CardDAVEnabled: carddavOn,
			}, deps.Client)
			// Hot-reload the CalDAV AUTH-failure ceiling on a config_changed
			// re-fetch (same seam as the IMAP backend's ApplyConfig).
			configReloader.Register(caldavSrv.ApplyConfig)
			caldavHandler = caldavSrv.Handler()
		}
		if carddavOn {
			carddavSrv := carddav.NewServer(carddav.ServerConfig{
				TLSConfig:                tlsConfig,
				Logger:                   logger.With("listener", "dav-443"),
				MaxAuthFailuresPerMinute: deps.Snapshot.Auth.MaxAuthFailuresPerMinute,
				PrimaryDomain:            deps.Snapshot.PrimaryDomain,
			}, deps.Client)
			// Hot-reload the CardDAV AUTH-failure ceiling + primary domain on a
			// config_changed re-fetch (seal-always; no storage-mode plumbing).
			configReloader.Register(carddavSrv.ApplyConfig)
			carddavHandler = carddavSrv.Handler()
		}
		if webdavOn {
			webdavSrv := webdav.NewServer(webdav.ServerConfig{
				TLSConfig:                tlsConfig,
				Logger:                   logger.With("listener", "dav-443"),
				MaxAuthFailuresPerMinute: deps.Snapshot.Auth.MaxAuthFailuresPerMinute,
				PrimaryDomain:            deps.Snapshot.PrimaryDomain,
				NestBaseURL:              deps.NestBaseURL,
				NestHTTPClient:           deps.NestHTTPClient,
			}, deps.Client)
			// Hot-reload the WebDAV AUTH-failure ceiling + primary domain on a
			// config_changed re-fetch (same seam as the CardDAV backend).
			configReloader.Register(webdavSrv.ApplyConfig)
			webdavHandler = webdavSrv.Handler()
		}

		// One http.Server + mux over the enabled handlers. davMounts decides the
		// mount set from the gating tuple (contacts-only serves CardDAV at the root
		// too so its principal is reachable without a CalDAV catch-all; WebDAV
		// always mounts only under /webdav/).
		davSrv := dav.NewServer(dav.ServerConfig{
			TLSConfig: tlsConfig,
			Logger:    logger.With("listener", "dav-443"),
		}, davMounts(caldavOn, carddavOn, webdavOn, caldavHandler, carddavHandler, webdavHandler)...)

		// Layering, innermost → outermost: proxyproto → per-IP cap → global cap
		// → TLS. proxyproto is INNERMOST — the PROXY-v2 header arrives on the raw
		// socket before the TLS ClientHello, and peeling it makes the real client
		// IP the conn's RemoteAddr (caldav-server.md § Network exposure —
		// Client-IP propagation). The per-IP cap sits ABOVE proxyproto so its
		// RemoteAddr() read resolves that real client IP (not the router's
		// loopback) — the only MDA listener that needs the peel; for the trusted
		// in-container router the header is written immediately, so this doesn't
		// stall the accept loop. The per-IP cap sits BELOW the global cap,
		// mirroring nest serve_tls (global semaphore first, then per-IP). Both
		// caps' conns are non-*tls.Conn, so http.Server still type-asserts
		// *tls.Conn and populates r.TLS.
		//
		// WithRouterAuth matters here: this port is a LOOPBACK bind, so a compromised
		// co-resident bridge UID can dial it and forge a header to spoof the
		// source IP the AUTH lockout + report_auth_event audit key on. With the
		// artifact-provisioned secret, only the router's header may rewrite the
		// source (security.md § Co-resident process trust boundary); empty ⇒
		// trust-any-loopback.
		tlsDAV := tls.NewListener(capListener(perIPMDA(
			proxyproto.New(rawDAV, proxyproto.WithRouterAuth(deps.RouterProxyAuth)),
			perIPLimiter, "dav-443"), "dav-443"), tlsConfig)
		bound = append(bound, boundListener{
			name: "dav-443",
			ln:   tlsDAV,
			run: func(c context.Context) error {
				return runDAVListener(c, davSrv, tlsDAV, deps.ShutdownGrace, logger.With("listener", "dav-443"))
			},
		})
	}

	// Bind the IMAP (993 implicit-TLS + 143 STARTTLS) surfaces iff email is
	// enabled. Both share one Backend (the BODYSTRUCTURE LRU lives on it) and
	// the IMAP IDLE-timeout hot-reload registers on the reloader.
	if mailOn {
		rawIMAPStartTLS, err := net.Listen("tcp", deps.IMAPListenStartTLS)
		if err != nil {
			closeBound()
			logplane.ListenerBindFailed("imap", logplane.PortOf(deps.IMAPListenStartTLS))
			return fmt.Errorf("imap(143) listen %s: %w", deps.IMAPListenStartTLS, err)
		}
		rawIMAPS, err := net.Listen("tcp", deps.IMAPListenImplicitTLS)
		if err != nil {
			_ = rawIMAPStartTLS.Close()
			closeBound()
			logplane.ListenerBindFailed("imaps", logplane.PortOf(deps.IMAPListenImplicitTLS))
			return fmt.Errorf("imap(993) listen %s: %w", deps.IMAPListenImplicitTLS, err)
		}
		// Cap connections on each raw socket: per-IP shed below the global cap
		// (mirroring nest serve_tls), then for 993 (implicit TLS) wrap BELOW
		// tls.NewListener so go-imap still sees a *tls.Conn; 143 (STARTTLS) wraps
		// the raw listener directly. 993/143 are published directly (no PROXY-v2
		// front), so the per-IP RemoteAddr() is already the real client IP.
		tlsIMAPS := tls.NewListener(capListener(perIPMDA(rawIMAPS, perIPLimiter, "imap-993"), "imap-993"), tlsConfig)
		cappedIMAP143 := capListener(perIPMDA(rawIMAPStartTLS, perIPLimiter, "imap-143"), "imap-143")

		imapBackend := imap.NewBackend(
			deps.Client,
			logger.With("server", "imap"),
			int(deps.Snapshot.IMAP.BodyStructureCacheMax),
			time.Duration(deps.Snapshot.IMAP.IdleTimeoutSecs)*time.Second,
			deps.Snapshot.Auth.MaxAuthFailuresPerMinute,
			pushDispatcher,
			bytePlane,
		)
		// Mail auth (IMAP + CalDAV) is about to serve: mint the dummy-KDF blob
		// now so the first unknown-user probe does not pay a one-time seal cost
		// (network-exposure.md § Rulings F3 timing equalizer). Async — the
		// sync.Once serializes against any auth that races it.
		go mailfauna.PrewarmDummyKDF()
		if applier, ok := imapBackend.(interface {
			ApplyConfig(wsrpc.ConfigSnapshot)
		}); ok {
			configReloader.Register(applier.ApplyConfig)
		}
		// Seed the IMAP backend's primary domain from the boot snapshot so a
		// bare username resolves under it before the first config_changed (the
		// CalDAV server seeds the same value via ServerConfig.PrimaryDomain).
		if pdSetter, ok := imapBackend.(interface{ SetPrimaryDomain(string) }); ok {
			pdSetter.SetPrimaryDomain(deps.Snapshot.PrimaryDomain)
		}
		// Seed the per-user spam-scorer policy from the boot snapshot so the
		// SELECT-time scoring pass (spam_score.go) routes INBOX→Junk against
		// the admin-effective `spam_folder` threshold before the first
		// config_changed — the same boot-seed shape as the MTA's
		// config_holder.spamPolicy, so both scoring positions agree.
		if spSetter, ok := imapBackend.(interface {
			SetSpamPolicy(mailfauna.SpamPolicy)
		}); ok {
			spSetter.SetSpamPolicy(mailfauna.SpamPolicyFromSnapshot(deps.Snapshot.Spam, deps.Snapshot.Auth))
		}
		// Seed the Tier-2 per-user `mail.spam.bayesian_*` knobs from the same
		// boot snapshot so the SELECT-time scorer uses the admin-effective
		// weight / confidence ramp before the first config_changed (in lockstep
		// with the spam policy above).
		if bkSetter, ok := imapBackend.(interface {
			SetBayesianKnobs(mailfauna.BayesianKnobs)
		}); ok {
			bkSetter.SetBayesianKnobs(mailfauna.BayesianKnobsFromSnapshot(deps.Snapshot.Spam))
		}
		// Two Server instances (one per listener) sharing the Backend. TLSConfig
		// is set on both: the 143 server hands it to the library for in-band
		// STARTTLS upgrade; the 993 server's connections are already TLS (wrapped
		// by tls.NewListener above) so the library never re-handshakes.
		imapSrvImplicit := imap.NewServer(imap.ServerConfig{TLSConfig: tlsConfig}, imapBackend)
		imapSrvStartTLS := imap.NewServer(imap.ServerConfig{TLSConfig: tlsConfig}, imapBackend)
		bound = append(bound,
			boundListener{
				name: "imap-993",
				ln:   tlsIMAPS,
				run: func(c context.Context) error {
					return runIMAPListener(c, imapSrvImplicit, tlsIMAPS, deps.ShutdownGrace, logger.With("listener", "imap-993"))
				},
			},
			boundListener{
				name: "imap-143",
				ln:   cappedIMAP143,
				run: func(c context.Context) error {
					return runIMAPListener(c, imapSrvStartTLS, cappedIMAP143, deps.ShutdownGrace, logger.With("listener", "imap-143"))
				},
			},
		)
	}

	// Install config_changed handling now that all appliers are registered, and
	// publish the reloader so main.go's reconnect OnConnect re-applies the
	// re-fetched snapshot through this same applier seam (§ Reconnecting).
	pushDispatcher.Register(wsrpc.PushKindConfigChanged, configReloader.Handle)
	if deps.PublishConfigReloader != nil {
		deps.PublishConfigReloader(configReloader)
	}
	// Install the dispatcher as the process's single push handler. The
	// production deps.Client (a *wsrpc.ReconnectingClient) re-installs it on
	// every reconnect, so the push subscriptions survive nest blips.
	if inst, ok := deps.Client.(interface {
		SetOnPush(wsrpc.PushHandler)
	}); ok {
		inst.SetOnPush(pushDispatcher.Handle)
	}
	go configReloader.Run(ctx)

	logger.Info("mda.Run starting listeners",
		"mail_enabled", mailOn,
		"caldav_enabled", caldavOn,
		"carddav_enabled", carddavOn,
		"webdav_enabled", webdavOn,
		"listener_count", len(bound),
		"local_domains_count", len(deps.Snapshot.LocalDomains),
		"primary_domain", deps.Snapshot.PrimaryDomain,
		"bridge_id", deps.BridgeID,
	)

	// Serve every bound listener until ctx cancel or first listener error, then
	// grace-drain. A real Serve error cancels the siblings and becomes firstErr;
	// bridgeshutdown.ErrShutdownForced is the expected forced-drain outcome,
	// returned only if no real error preempted it.
	var wg sync.WaitGroup
	errCh := make(chan error, len(bound))
	wg.Add(len(bound))
	for _, b := range bound {
		b := b
		go func() {
			defer wg.Done()
			errCh <- b.run(listenerCtx)
		}()
	}

	var firstErr error
	forced := false
	for range bound {
		err := <-errCh
		switch {
		case err == nil:
		case errors.Is(err, bridgeshutdown.ErrShutdownForced):
			forced = true
		case firstErr == nil:
			firstErr = err
			cancel()
		}
	}
	wg.Wait()
	logger.Info("mda.Run shutting down", "reason", ctx.Err())
	if firstErr != nil {
		return firstErr
	}
	if forced {
		return bridgeshutdown.ErrShutdownForced
	}
	return nil
}

// maxMDAConns caps simultaneously-served connections on each MDA listener
// (IMAP 993/143, CalDAV 443). The MDA serves authenticated clients that hold
// many long-lived sessions — IMAP especially, with one persistent IDLE
// connection per mailbox per device — so the cap is a generous OS-FD /
// goroutine backstop rather than a flood brake. Matched to the Rust nest TLS
// loop's global connection cap (4096) and the submission surface
// (mta.maxSubmissionConns), and far above port 25's deliberate 100
// (security.md / smtp-server.md § Connection-time limits). Each listener gets
// its own connlimit.Listener, so this is a per-listener ceiling, not a shared
// budget.
const maxMDAConns = 4096

// capListener wraps a raw listener in the per-MDA global connection cap
// (internal/connlimit), labelling its accepted/capped saturation on
// MDAConnectionsTotal{listener=name}. ⚠ Always wrap the *raw* socket below
// any tls.NewListener: the wrapper's slotConn is not a *tls.Conn, so the
// serving library (go-imap / http.Server) only detects TLS when tls.Conn is
// the outermost wrapper.
func capListener(ln net.Listener, name string) net.Listener {
	return connlimit.New(ln, maxMDAConns,
		func() { metrics.MDAConnectionsTotal.WithLabelValues(name, "accepted").Inc() },
		func() { metrics.MDAConnectionsTotal.WithLabelValues(name, "capped").Inc() },
	)
}

// perIPMDA wraps a listener in the shared per-IP concurrent-connection limiter
// (connlimit.PerIPLimiter, fed from AuthPolicy.max_conn_per_ip), labelling shed
// connections on MDAConnectionsTotal{listener=name,"per_ip_shed"}. Wrap it
// BELOW capListener (global cap outermost, mirroring nest serve_tls's
// global-then-per-IP order) and ABOVE proxyproto for CalDAV (so RemoteAddr()
// resolves the PROXY-v2-conveyed real client IP, not the router's loopback);
// for 993/143 — published directly — there is no proxyproto and RemoteAddr() is
// the raw peer. `limiter` is shared across all three MDA listeners. The MTA
// submission twin is mta.perIPSubmission.
func perIPMDA(ln net.Listener, limiter *connlimit.PerIPLimiter, name string) net.Listener {
	return connlimit.NewPerIPListener(ln, limiter,
		func() { metrics.MDAConnectionsTotal.WithLabelValues(name, "per_ip_shed").Inc() },
	)
}

// runIMAPListener serves one IMAP listener until ctx cancels or Serve fails,
// then grace-drains it: `* BYE` new commands, promptly evict idle/IDLE'ing
// sessions, drain in-flight commands up to grace, then force-close stragglers.
// Returns bridgeshutdown.ErrShutdownForced when the grace window expired with
// sessions still in flight, a real Serve error if Serve failed on its own, or
// nil on a clean drain. `ln` is cap-wrapped (connlimit.New) at construction;
// serving and draining it is unchanged. Mirrors mta.runListenerWithBackend.
func runIMAPListener(ctx context.Context, srv *imap.Server, ln net.Listener, grace time.Duration, logger *slog.Logger) error {
	serveErr := make(chan error, 1)
	go func() { serveErr <- srv.Serve(ln) }()
	select {
	case err := <-serveErr:
		// Serve returned on its own — a bind/accept failure. (A graceful close
		// only happens via Shutdown below, which we handle in the ctx branch.)
		return err
	case <-ctx.Done():
		graceCtx, gcancel := context.WithTimeout(context.Background(), grace)
		defer gcancel()
		forced, pending := srv.Shutdown(graceCtx)
		<-serveErr // Serve returns once Shutdown closes the listener
		if forced {
			// Goal-doc § Shutting down step 4 structured force-close line.
			logger.Warn("bridge_shutdown_force_close", "pending_count", pending)
			return bridgeshutdown.ErrShutdownForced
		}
		logger.Info("imap listener stopped", "drained_clean", true)
		return nil
	}
}

// davMounts builds the internal/mda/dav mount list from the gating tuple. The
// CalDAV chain (when on) owns the `/` catch-all — the CalDAV principal lives at
// the root single-segment `/{user}/`, so it must own root to be reachable; the
// CardDAV chain (when on) owns `/carddav/` (longest-prefix, so it wins over `/`
// for its own paths). In a CONTACTS-ONLY deployment (CardDAV on, CalDAV off)
// there is no CalDAV catch-all, so CardDAV is ALSO mounted at `/` to keep its
// own root principal (`/{user}/`) and `propFindRoot` discovery reachable —
// otherwise a contacts-only box would 404 the principal a stock CardDAV client
// walks during RFC-6764 discovery.
//
// NOTE (known follow-up — unified principal): when BOTH are on, CalDAV owns `/`
// and the CardDAV root principal at `/{user}/` is shadowed by the CalDAV chain
// (which advertises only calendar-home-set). A CardDAV client pointed straight
// at its collection URL `/carddav/{user}/…` round-trips fully (a Depth:1
// PROPFIND of the home set self-enumerates the address books), but seamless
// principal→addressbook-home-set autodiscovery for a client that only knows the
// server host needs a UNIFIED root principal advertising both home-sets — a
// distinct, CalDAV-touching change tracked as a CardDAV follow-up (2e / a later
// slice), not this listener wiring.
func davMounts(caldavOn, carddavOn, webdavOn bool, caldavHandler, carddavHandler, webdavHandler http.Handler) []dav.Mount {
	var mounts []dav.Mount
	if caldavOn {
		mounts = append(mounts, dav.Mount{Pattern: "/", Handler: caldavHandler})
	}
	if carddavOn {
		mounts = append(mounts, dav.Mount{Pattern: "/carddav/", Handler: carddavHandler})
		if caldavOn {
			// CardDAV's service discovery must reach the CardDAV chain even
			// with CalDAV owning the root: the apex sends a contacts app to
			// this host's /.well-known/carddav, which the CalDAV chain would
			// answer with an empty multistatus (carddav-server.md § Network
			// exposure & discovery). An exact-path pattern, so it wins over "/".
			mounts = append(mounts, dav.Mount{Pattern: "/.well-known/carddav", Handler: carddavHandler})
		}
		if !caldavOn {
			// Contacts-only: no CalDAV catch-all, so serve the CardDAV principal
			// + propFindRoot at root too.
			mounts = append(mounts, dav.Mount{Pattern: "/", Handler: carddavHandler})
		}
	}
	if webdavOn {
		// WebDAV files always live under /webdav/{user}/… — it never owns the root
		// `/` (unlike CalDAV/CardDAV it has no RFC-6764 principal autodiscovery;
		// clients are configured with the full collection URL, and the apex
		// /.well-known/webdav redirect is nest-served). So no duplicate-`/` panic
		// regardless of the caldav/carddav gating.
		mounts = append(mounts, dav.Mount{Pattern: "/webdav/", Handler: webdavHandler})
	}
	return mounts
}

// runDAVListener is the shared-DAV analog of runIMAPListener: serve the muxed
// CalDAV/CardDAV http.Server until ctx cancel or Serve failure, then grace-drain
// via http.Server.Shutdown (force-close on grace expiry). Returns
// bridgeshutdown.ErrShutdownForced on a forced drain, a real Serve error, or nil
// on a clean drain. `ln` is cap-wrapped at construction.
func runDAVListener(ctx context.Context, srv *dav.Server, ln net.Listener, grace time.Duration, logger *slog.Logger) error {
	serveErr := make(chan error, 1)
	go func() { serveErr <- srv.Serve(ln) }()
	select {
	case err := <-serveErr:
		return err
	case <-ctx.Done():
		graceCtx, gcancel := context.WithTimeout(context.Background(), grace)
		defer gcancel()
		forced := srv.Shutdown(graceCtx)
		<-serveErr // Serve returns nil once Shutdown closes the listener
		if forced {
			logger.Warn("bridge_shutdown_force_close", "listener", "dav")
			return bridgeshutdown.ErrShutdownForced
		}
		logger.Info("dav listener stopped", "drained_clean", true)
		return nil
	}
}
