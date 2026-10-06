// Command fauna-atproto-bridge is the out-of-process ATProto PDS bridge
// process (role "atproto.pds"). It publishes a Fauna user's *public* posts
// into the ATProto network by hosting an ATProto Personal Data Server
// — mint a DID, hold the user's repo, sign commits, serve the
// com.atproto.sync.* / repo.* read surface, and emit a subscribeRepos firehose
// (docs/goal/behavior/atproto-pds-bridge.md § Architecture).
//
// S1 landed the enrollment + lifecycle handshake it inherits verbatim from
// the mail bridge (docs/goal/behavior/mail-bridge-lifecycle.md § Cold boot /
// § Reconnecting), under the role "atproto.pds". S2 (this slice) adds the
// IDENTITY surface: the DID mint loop (mint.go) polls nest's per-user roster
// and mints a did:plc (PLC genesis op, user rotation key senior) or did:web
// per pending identity. Repo/MST/firehose translation is S3.
//
// Since S2 this binary is CGO (like the mail MTA/MDA cmd): it imports
// internal/mailfauna to HPKE-Open the sealed per-user identity-key blobs over
// the fauna-mail-go FFI, so it links libfauna_ffi.so. S1's CGO-free build was
// a skeleton property, not a contract — the accepted S2 design decision. The
// --print-pubkey / keygen / --dry-run paths never touch the FFI.
//
// F1 (this slice) adds the auth surface: --xrpc-listen brings up the
// com.atproto.server.* XRPC listener (internal/{xrpc,atprotopds,auth,authlock})
// alongside the S2 mint loop. Those packages are themselves CGO-free — the HS256
// session-token secret is minted by nest on first fetch and sealed to this
// bridge, so it is durable across restarts (atproto-pds-full.md
// § Implementation status today).
//
// The role is discovered from nest (fauna.bridges.whoami), never from a
// --mode flag — the same product invariant the mail bridge honours ("a bridge
// process discovers its role from its nest service-user enrollment"). CLI args
// are limited to deployment-topology values nest cannot provide (keypair file,
// nest WS endpoint, log level, data dir).
package main

import (
	"context"
	"crypto/tls"
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
	"syscall"
	"time"

	"github.com/fxamacker/cbor/v2"

	"github.com/faunasocial/fauna/bins/fauna-bridges/internal/atprotofirehose"
	"github.com/faunasocial/fauna/bins/fauna-bridges/internal/atprotoid"
	"github.com/faunasocial/fauna/bins/fauna-bridges/internal/atprotopds"
	"github.com/faunasocial/fauna/bins/fauna-bridges/internal/atprotoread"
	"github.com/faunasocial/fauna/bins/fauna-bridges/internal/atprotorepo"
	"github.com/faunasocial/fauna/bins/fauna-bridges/internal/confinement"
	"github.com/faunasocial/fauna/bins/fauna-bridges/internal/keypair"
	"github.com/faunasocial/fauna/bins/fauna-bridges/internal/logging"
	"github.com/faunasocial/fauna/bins/fauna-bridges/internal/mailfauna"
	"github.com/faunasocial/fauna/bins/fauna-bridges/internal/proxyproto"
	bridgetls "github.com/faunasocial/fauna/bins/fauna-bridges/internal/tls"
	"github.com/faunasocial/fauna/bins/fauna-bridges/internal/wsrpc"
	"github.com/faunasocial/fauna/bins/fauna-bridges/internal/xrpc"
	faunaAtproto "github.com/faunasocial/fauna/libs/fauna-mail-go/fauna_bridge_atproto"
)

// roleAtprotoPds is the fixed enrollment role this binary requests and the
// authoritative role nest must echo back at whoami. Unlike the mail bridge —
// where the same binary is MTA or MDA depending on which keyfile it presents,
// so its role_hint is derived from the keyfile basename — the atproto bridge
// serves exactly one role, so the hint is a compile-time constant. nest gates
// this role to MANUAL admin approval (never auto-approved by the mail/DAV
// toggles — mail-bridge-lifecycle.md § Onboarding auto-approval, Scope).
const roleAtprotoPds = "atproto.pds"

// rpcDeadline is the per-call timeout for the bootstrap RPCs (Whoami,
// RegisterServiceUser, FetchConfig). 30s is comfortably above the loopback
// round-trip and small enough that a stuck nest does not silently hang startup.
const rpcDeadline = 30 * time.Second

func main() {
	ctx, stop := signal.NotifyContext(context.Background(), syscall.SIGINT, syscall.SIGTERM)
	defer stop()
	if err := run(ctx, os.Args, os.Stdout); err != nil {
		fmt.Fprintln(os.Stderr, err)
		os.Exit(1)
	}
}

func run(ctx context.Context, args []string, out io.Writer) error {
	fs := flag.NewFlagSet(args[0], flag.ContinueOnError)
	fs.SetOutput(io.Discard)
	keypairFile := fs.String("keypair-file", "", "Ed25519 service-user keypair file; nest holds the role + config for this keypair (required)")
	nestEndpoint := fs.String("nest-endpoint", "", "WS-RPC endpoint URL to dial (e.g., https://127.0.0.1:8443); required outside --dry-run")
	xrpcListen := fs.String("xrpc-listen", "", "loopback host:port for the XRPC listener (F1 auth surface); empty = no listener. Deployment-topology wiring: the L4 SNI router forwards `pds.<domain>` here with a PROXY-v2 header; the bridge peels it and terminates `pds.<domain>` TLS itself via the sealed cert blob")
	logLevel := fs.String("log-level", "info", "structured-log level: debug, info, warn, error")
	dataDir := fs.String("data-dir", "", "deployment-topology directory (reserved: S2+ store the per-user DID / repo / carstore here)")
	dryRun := fs.Bool("dry-run", false, "print the startup banner and exit; do not connect to nest")
	printPubkey := fs.Bool("print-pubkey", false, "mint-if-absent the --keypair-file keyfile, print its hex Ed25519 pubkey to stdout, and exit (deployment-artifact blessed-registry provisioning); does not connect to nest")
	// E2E-only one-shot modes, compiled in only under `-tags fauna_e2e_seize`
	// (seize.go / seize_absent.go). A production build registers nothing here,
	// so the flags below are unknown flags it refuses to parse.
	registerSeizeFlags(fs)
	if err := fs.Parse(args[1:]); err != nil {
		return err
	}
	if *keypairFile == "" {
		return fmt.Errorf("--keypair-file is required (nest looks up the bridge's role + config by the corresponding pubkey)")
	}
	// Blessed-registry provisioning: the deployment artifact runs this AS ROOT
	// at entrypoint to mint the atproto.pds keypair (mint-if-absent, so the
	// enrolled identity is stable across reboots) and capture its blessed
	// Ed25519 pubkey for the registry nest reads. Print only the hex pubkey +
	// newline to stdout and exit before any nest dial — so --nest-endpoint is
	// not required here. The keyfile role is the fixed atproto.pds role.
	// security.md § Enrollment proof-of-possession contract.
	if *printPubkey {
		kf, err := keypair.LoadOrCreate(*keypairFile, roleAtprotoPds, "unresolved-bridge")
		if err != nil {
			return fmt.Errorf("mint/load keyfile for --print-pubkey: %w", err)
		}
		fmt.Fprintf(out, "%x\n", kf.Ed25519PublicKey())
		return nil
	}
	if !*dryRun && *nestEndpoint == "" {
		return fmt.Errorf("--nest-endpoint is required outside --dry-run")
	}
	// One-shot e2e modes run here, before the banner and the long-lived boot:
	// they reuse the enrolled keyfile of the bridge already running beside them,
	// so they need the dial and nothing else.
	if handled, err := maybeRunSeize(ctx, *keypairFile, *nestEndpoint, *logLevel, out); handled {
		return err
	}
	fmt.Fprintf(out, "fauna-atproto-bridge: keypair_file=%s nest_endpoint=%q log_level=%s data_dir=%q role=%s dry_run=%t\n",
		*keypairFile, *nestEndpoint, *logLevel, *dataDir, roleAtprotoPds, *dryRun)
	if *dryRun {
		return nil
	}

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
	// fauna.bridges.request_enrollment with role_hint="atproto.pds" and poll it
	// on the 1/2/4/8/16/30 s backoff until an admin approves the bridge from
	// their Fauna app — mail-bridge-lifecycle.md § Cold boot steps 3–4 /
	// § Pending approval. The kind is loopback-gated nest-side (the in-container
	// bridge↔nest hop); approval inserts the audit row, after which the auth
	// Dial succeeds. Unlike mail, this role is NEVER auto-approved (it always
	// takes the manual approval card — § Onboarding auto-approval, Scope). The
	// authoritative operating role still comes from whoami below. Idempotent on
	// re-enroll, so a restart is harmless.
	roleHint := roleAtprotoPds
	// Proof-of-possession: sign the
	// enrollment so nest can, in strict mode, require the enrolling bridge to BE
	// the artifact-blessed key for this role AND prove possession of it. In S1
	// the atproto role is provisioned WITHOUT a blessed key (nest's
	// blessed_bridge_pubkey → AtprotoPds is None), so the lenient loopback path
	// applies and these fields are sent-but-unverified; the signature still
	// binds x25519 set-once at enrollment, which the later
	// register_service_user attestation only confirms.
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
	// The bridge always reaches the nest over the container loopback, where the
	// nest's TLS cert is issued for its public domain (and may be self-signed),
	// so verification is skipped for a loopback endpoint only. This is the
	// module-shared decision, single-sourced in wsrpc.NestHTTPClient — a drifted
	// copy that skipped verify for a non-loopback host would be a MITM hole.
	nestHTTP := wsrpc.NestHTTPClient(*nestEndpoint, logger)
	// Service-user re-keying auto-regenerate (mail-bridge-lifecycle.md
	// § Service-user re-keying): a `revoked` reply at the enrollment poll means
	// an admin rotated this bridge's key — archive the revoked keypair (0400,
	// kept for audit), generate a fresh one at the original path, and re-enroll
	// in-process. At most ONE regenerate per process run: a second `revoked`
	// means the admin revoked the *fresh* key too (a deliberate rejection, not a
	// rotation) — stop cleanly. No listeners are open pre-approval, so the
	// rotation is a clean in-process restart of the enrollment flow.
	regenerated := false
	for {
		err := wsrpc.EnrollAndAwaitApproval(ctx, *nestEndpoint, newEnrollID(), logger, nestHTTP)
		if err == nil {
			break
		}
		switch {
		case errors.Is(err, wsrpc.ErrBridgeRevoked) && !regenerated:
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
	// The raw client is wrapped in a *wsrpc.ReconnectingClient below (after the
	// bootstrap whoami/register/fetch_config), which then owns the connection
	// lifecycle — re-dialling on drops (mail-bridge-lifecycle.md § Reconnecting).
	// This single defer closes the reconnector once it exists, or the raw client
	// on a bootstrap-failure early return.
	var reconnector *wsrpc.ReconnectingClient
	defer func() {
		if reconnector != nil {
			_ = reconnector.Close()
			return
		}
		_ = client.Close()
	}()
	logger.Info("ws-rpc dial complete", "endpoint", *nestEndpoint)

	// ── Whoami: resolve the authoritative role + bridge_id from nest ──
	whoamiCtx, whoamiCancel := context.WithTimeout(ctx, rpcDeadline)
	whoami, err := wsrpc.Whoami(whoamiCtx, client)
	whoamiCancel()
	if err != nil {
		return fmt.Errorf("whoami: %w", err)
	}
	if whoami.Role != roleAtprotoPds {
		return fmt.Errorf("whoami: nest returned unexpected role %q (want %q)", whoami.Role, roleAtprotoPds)
	}
	if whoami.Status != "approved" {
		return fmt.Errorf("whoami: bridge service user is %q, not approved; have the admin approve this bridge in their Fauna app", whoami.Status)
	}
	// Re-init the logger with the resolved role attribute, then swap it onto the
	// WS-RPC client (Dial captured the role=unresolved logger and would keep
	// emitting under that placeholder otherwise).
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

	// Persist the resolved role + bridge_id back to the keyfile so a future
	// admin inspecting it sees nest's view, not the placeholder. Idempotent —
	// UpdateEnrollment is a no-op if the keyfile already matches. Soft failure:
	// the seeds are still good and the resolved role lives in memory for the run.
	if err := kf.UpdateEnrollment(*keypairFile, whoami.Role, whoami.BridgeID); err != nil {
		logger.Warn("persist resolved role to keyfile failed (continuing)", "err", err)
	}

	// ── Register service user: attest the x25519 pubkey ──
	//
	// nest seals per-bridge wrapped blobs to the bridge's attested x25519
	// pubkey (mail-bridge-lifecycle.md § Cold boot step 6). For the atproto
	// bridge those blobs are the per-user DID signing/rotation key material —
	// but that sealing is S2, so S1 lands only the attestation itself. Without
	// it bridge_service_users.x25519_pubkey stays NULL and S2's key-seal
	// fan-out would skip this bridge. Fatal on failure, consistent with the
	// other cold-boot RPCs — the supervisor restart retries.
	//
	// mlkemEk is nil: the atproto bridge is classical-x25519-only in S1. The
	// mail bridge derives an ML-KEM ek from its seed via the fauna-mail-go FFI;
	// that is exactly the cgo dependency this binary avoids, and S1 has no
	// hybrid-sealed blobs to open. nest accepts a NULL mlkem_ek (the Section-A
	// data-preserving CHECK-widen migration keeps the column nullable).
	x25519Pub := kf.X25519PublicKey()
	// Confinement self-probe, same shape as the mail bridge's (security.md
	// § Co-resident process trust boundary → Confinement self-probe). This
	// binary is not wrapped in `fauna-sandbox` today, so it will report
	// `landlock: unknown` — which is the point: the report says what is
	// actually true of the running process rather than what the design intends,
	// and it flips to `partial`/`fully` for free on the day this role gains a
	// profile. Diagnostics only; nothing branches on it.
	conf := confinement.Probe(*dataDir)
	logger.Info("confinement self-probe",
		"uid", conf.UID,
		"sealed_store", conf.SealedStore,
		"landlock", conf.Landlock,
		"seccomp", conf.Seccomp,
	)
	regCtx, regCancel := context.WithTimeout(ctx, rpcDeadline)
	enrollmentRequestID, err := wsrpc.RegisterServiceUser(
		regCtx, client,
		kf.Ed25519PublicKey(), x25519Pub[:], nil,
		whoami.Role, whoami.BridgeID, &conf,
	)
	regCancel()
	if err != nil {
		return fmt.Errorf("register_service_user (x25519 attestation): %w", err)
	}
	logger.Info("x25519 attested to nest", "enrollment_request_id", enrollmentRequestID)

	// ── FetchConfig: pull the current config snapshot ──
	//
	// The snapshot carries `PrimaryDomain` — the anchor the F1 XRPC listener's
	// TLS provider fetches its `pds.<domain>` cert under (below). The rest of
	// the snapshot is mail-shaped and irrelevant here; S2+ read any
	// atproto-specific config once nest adds it.
	cfgCtx, cfgCancel := context.WithTimeout(ctx, rpcDeadline)
	snapshot, err := wsrpc.FetchConfig(cfgCtx, client, "all")
	cfgCancel()
	if err != nil {
		return fmt.Errorf("fetch_config: %w", err)
	}
	logger.Info("config snapshot fetched", "primary_domain", snapshot.PrimaryDomain)

	// ── In-process reconnect: from here the bridge survives nest blips ──
	//
	// Wrap the live client so a dropped WS (nest restart, network blip, NAT
	// timeout, idle close) triggers an in-process reconnect-backoff
	// (1/2/4/8/16/30 s) — mail-bridge-lifecycle.md § Reconnecting. Each
	// reconnect re-runs whoami (a revoke → graceful shutdown via Done()) and
	// re-fetches config. There is no role ConfigReloader yet (nothing to
	// hot-apply). The loop is rooted in context.Background(), NOT the process
	// ctx: it is stopped solely by the deferred reconnector.Close() or by a
	// revoke — so a SIGTERM does not race the loop into a spurious re-dial while
	// the process is already tearing down.
	//
	// mintNudge wakes the mint loop below immediately after a reconnect (a
	// nest restart may carry new pending identities); buffered size 1 so a
	// nudge is never dropped and never blocks OnConnect.
	mintNudge := make(chan struct{}, 1)
	// issuerKeyNudge wakes the NEST issuer key-set refresh loop (TP5): fed by
	// the issuer_key_rotated push, by reconnect (an admin rotation during an
	// outage is only visible by re-fetching), and by the verify path on an
	// unknown `kid`. Same buffered-1 discipline.
	issuerKeyNudge := make(chan struct{}, 1)
	reconnector = wsrpc.NewReconnectingClient(context.Background(), client, wsrpc.ReconnectConfig{
		Dial: func(c context.Context) (*wsrpc.Client, error) {
			dctx, dcancel := context.WithTimeout(c, rpcDeadline)
			defer dcancel()
			return wsrpc.Dial(dctx, wsrpc.ClientConfig{
				NestEndpoint: *nestEndpoint,
				AuthClient:   authClient,
				Logger:       logger,
				HTTPClient:   nestHTTP,
			})
		},
		OnConnect: func(c context.Context, cc wsrpc.Caller) error {
			wctx, wcancel := context.WithTimeout(c, rpcDeadline)
			wh, werr := wsrpc.Whoami(wctx, cc)
			wcancel()
			// A revoked bridge never sees a status=revoked *reply* — nest's
			// capability gate denies it whoami before the handler runs — so the
			// denial itself is the revocation signal (wsrpc.WhoamiIndicatesRevoked).
			if wsrpc.WhoamiIndicatesRevoked(wh, werr) {
				return wsrpc.ErrBridgeRevoked // → graceful shutdown
			}
			if werr != nil {
				return fmt.Errorf("reconnect whoami: %w", werr) // transient → retry
			}
			if wh.Status != "approved" {
				return fmt.Errorf("reconnect whoami: status %q (want approved)", wh.Status)
			}
			fctx, fcancel := context.WithTimeout(c, rpcDeadline)
			_, ferr := wsrpc.FetchConfig(fctx, cc, "all")
			fcancel()
			if ferr != nil {
				return fmt.Errorf("reconnect fetch_config: %w", ferr) // transient → retry
			}
			logger.Info("reconnect: re-verified whoami + re-fetched config")
			select {
			case mintNudge <- struct{}{}:
			default:
			}
			select {
			case issuerKeyNudge <- struct{}{}:
			default:
			}
			return nil
		},
		Logger: logger,
	})

	// ── F1 XRPC auth surface (atproto-pds-full.md § F1 detail) ──
	//
	// When --xrpc-listen is set, serve the com.atproto.server.* auth core on
	// a loopback listener ALONGSIDE the S2 mint loop below (both are
	// event-driven; the listener runs in its own goroutine). The listener
	// terminates `pds.<domain>` TLS itself (below): the L4 SNI router forwards
	// `pds.<domain>` here with a PROXY-v2 header, the bridge peels it and
	// completes the TLS handshake with the sealed cert blob. A tier_3 e2e dials
	// the loopback directly over HTTPS (skip-verify against the floor cert).
	//
	// The HS256 signing secret is durable: nest mints it on the first fetch
	// (provision-on-read, sealed to this bridge's attested x25519 — the
	// TLS blob discipline) and serves the same ciphertext forever after,
	// so minted tokens survive bridge restarts. Fetched once at boot; it
	// never rotates within F1 (atproto-pds-full.md § Key material inventory).
	xrpcListenErr := make(chan error, 1)
	if *xrpcListen != "" {
		secretCtx, secretCancel := context.WithTimeout(ctx, rpcDeadline)
		sealed, ferr := wsrpc.FetchAtprotoSessionSecretBlob(secretCtx, reconnector)
		secretCancel()
		if ferr != nil {
			return fmt.Errorf("fetch sealed session-token secret: %w", ferr)
		}
		bootX25519 := kf.X25519Secret()
		secret, uerr := mailfauna.UnsealAtprotoSessionSecretBlob(sealed, bootX25519[:])
		if uerr != nil {
			return fmt.Errorf("unseal session-token secret: %w", uerr)
		}
		// The PDS serves at the dedicated `pds.<domain>` subdomain (the same
		// hostname nest returns as `pds_endpoint` and the SNI router routes here
		// — atproto-pds-full.md § Wire & process topology, F1 packaging
		// resolution). The host and the service DID `did:web:pds.<domain>` are
		// read from shared Rust (`oauth_metadata`), their one owner: the nest
		// mints every OAuth access token with that DID as `aud`, so the two
		// sides must agree on it byte for byte. A loopback fallback keeps a
		// domainless/dev listener constructible for the app plane. (The
		// per-user DID is a separate concern: it is the account's real did:plc,
		// resolved nest-side and carried in the session token since slice 4d.)
		pdsHost := *xrpcListen
		serviceDID := "did:web:" + pdsHost
		if snapshot.PrimaryDomain != "" {
			pdsHost = faunaAtproto.AtprotoPdsHost(snapshot.PrimaryDomain)
			serviceDID = faunaAtproto.AtprotoPdsServiceDid(snapshot.PrimaryDomain)
		}
		minter := atprotopds.NewTokenMinter(atprotopds.StaticSecret(secret), serviceDID, nil)
		// Service auth mints with each ACCOUNT's own repo signing key, unsealed
		// per call from its nest-held blob (serviceauth_signers.go) — the same
		// key the projection loop signs repo commits with, so getServiceAuth
		// widens no custody (C7).
		// Cached behind the seam because the proxy path below mints on every
		// forwarded request (serviceauth_signers.go, repoSignerCacheTTL).
		signers := newCachedRepoSigners(sealedRepoSigners{
			nest:         reconnector,
			unseal:       mailfauna.UnsealAtprotoIdentityBlob,
			x25519Secret: bootX25519[:],
		})
		pds := atprotopds.NewServer(reconnector, minter, ffiAuthorizer{}, signers, logger)
		// The OAuth plane's resource server (`authorization-server.md` § The
		// issuer): the nest is the authorization server, this PDS verifies the
		// DPoP-bound access tokens it mints. Wired only with a claimed domain —
		// a domainless nest has no issuer, so there is nothing to honour, and
		// every OAuth token is refused either way. The origin a proof's `htu`
		// is compared against comes from the same shared-Rust builder that
		// spells the PDS host above, never from a request's Host header.
		if snapshot.PrimaryDomain != "" {
			pds.EnableOAuthResourceServer(ffiDPoPPolicy{}, faunaAtproto.OauthIssuer(pdsHost))
		}
		// F4 permission sets: the resolution chain the nest's own `/oauth/par`
		// asks this bridge to run for an `include:` scope (§ F4 detail →
		// *Permission sets*), answered over deliver_permission_set.
		pds.EnablePermissionSets(permissionSetResolver())
		// One limiter instance for the whole pre-auth surface: every
		// ClassAuth route shares one budget per peer, which is what
		// internal/xrpc/ratelimit.go's table says they are.
		ipLimiter := xrpc.NewIPLimiter(nil)
		// F4 slice 7: the OAuth plane's verifier fills the seam that has been
		// nil since F1. `Session`-class routes now accept EITHER an app
		// credential's `Bearer` token or an OAuth grant's DPoP-bound one, and
		// the scheme is what selects the plane — see xrpc.authenticate for why
		// that dispatch is the security property rather than a convenience.
		xrpcServer := xrpc.NewServer(pds, pds.OAuthTokenVerifier(), pds.AuthzHook, ipLimiter, logger)
		pds.RegisterRoutes(xrpcServer)
		// F3 service proxying: unregistered NSIDs with a proxy target —
		// explicit `atproto-proxy` header, or the headerless `app.bsky.*`
		// AppView default — forward through resolution + guard + per-request
		// service-auth mint (internal/atprotopds/proxy.go).
		pds.EnableProxy(proxyConfig())
		xrpcServer.SetFallback(pds.ProxyFallback)

		// ── S3 repo store + projection loop (atproto-pds-bridge.md § Projection) ──
		//
		// The store holds every projected user's MST repo; it lives beside the
		// XRPC listener because the two are one unit — projecting into a store no
		// listener serves has no value, and production (s6) always sets both
		// --xrpc-listen and --data-dir. Everything the store holds is re-derivable
		// from nest state (C6), so a file under --data-dir is a cache, not
		// precious; a domainless dev listener falls back to :memory:.
		storePath := ":memory:"
		if *dataDir != "" {
			// Under `/data/atproto/`, NOT directly under --data-dir: in the deploy
			// image /data is fauna:fauna 0711, so this bridge's non-root UID can
			// only traverse it, never create a file in it (SQLITE_CANTOPEN(14) →
			// boot crash-loop). entrypoint.sh mints /data/atproto owned by
			// fauna-atproto 0700; WAL needs directory write for -wal/-shm anyway.
			storeDir := filepath.Join(*dataDir, "atproto")
			// No-op in the deploy image (entrypoint.sh already minted it with the
			// right owner); this is what makes a plain dev `--data-dir` work too,
			// since the bridge cannot rely on an artifact having run.
			if mkerr := os.MkdirAll(storeDir, 0o700); mkerr != nil {
				return fmt.Errorf("create atproto store dir %q: %w", storeDir, mkerr)
			}
			storePath = filepath.Join(storeDir, "repos.db")
		}
		store, serr := atprotorepo.Open(storePath)
		if serr != nil {
			return fmt.Errorf("open atproto repo store %q: %w", storePath, serr)
		}
		defer store.Close()
		funnel, ferr := atprotorepo.NewFunnel(ctx, store, nil)
		if ferr != nil {
			return fmt.Errorf("build repo commit funnel: %w", ferr)
		}
		// The blob source reuses the SAME nest HTTP client the enroll/auth path
		// built (loopback-aware TLS, single-sourced in wsrpc.NestHTTPClient) —
		// a second client here would be a second place for that decision to
		// drift.
		blobSource := nestBlobSource{client: nestHTTP, endpoint: *nestEndpoint}
		projector := atprotorepo.NewProjector(store, funnel, ffiTranslator{}, blobSource, logger)
		// F2: the authed write surface commits through the SAME funnel the
		// projection loop uses (C1 — one writer per repo, never a second) with
		// the same per-account signing key.
		// The projector goes in so a write to a projection-OWNED collection (the
		// profile singleton) commits the record a projection pass would — one
		// renderer, not two (F2.4 slice 3).
		pds.EnableWrites(newFunnelRepoWriter(funnel, store, signers, projector, reconnector))
		// F2.4: uploadBlob lands bytes in the Fauna media path over the same
		// bulk-binary carve-out the outbound blob source reads from, and in the
		// SAME blob store com.atproto.sync.getBlob serves — so an inbound blob is
		// fetchable by the ref the caller was just handed.
		pds.EnableBlobUploads(newNestBlobIngester(
			nestHTTP, *nestEndpoint, authClient.Token, reconnector, store))
		projDeps := newProjDeps(bootX25519[:], store, funnel, projector)
		projNudge := make(chan struct{}, 1)

		// Public com.atproto.sync.*/repo.* read surface on F1's route table (C5:
		// one table, never a second). It serves the repos the projection loop
		// fills; subscribeRepos joins the same table just below, upgrading to a
		// WebSocket inside its handler so this frame's rate limit runs first.
		atprotoread.Register(xrpcServer, store, logger)

		// The live firehose: one emitter drains the commit funnel's outbox and
		// fans frames out to subscribed relays. SetOnCommit wakes it the instant
		// a commit lands; its own poll tick is the backstop.
		firehose := atprotofirehose.New(store, logger)
		funnel.SetOnCommit(firehose.Notify)
		atprotofirehose.Register(xrpcServer, firehose, logger)
		go firehose.Run(ctx)

		// Boot self-check: reload every persisted repo through indigo's own
		// loader before serving it, so storage that no longer parses is a loud
		// log line here rather than a silent relay rejection later. Structure
		// only — the per-account signing key is unsealed in the projection pass,
		// which runs the signature half once per boot.
		if repos, lerr := store.ListRepos(ctx); lerr != nil {
			logger.Warn("atproto self-check: cannot list repos", "err", lerr)
		} else {
			for _, rp := range repos {
				if verr := store.VerifyRepo(ctx, rp.DID, nil); verr != nil {
					logger.Error("atproto self-check FAILED: persisted repo does not reload",
						"did", rp.DID, "rev", rp.Rev, "err", verr)
					continue
				}
				logger.Info("atproto self-check ok", "did", rp.DID, "rev", rp.Rev)
			}
		}

		// One push handler for both S3 (projection) and F1 (kill-switch) kinds:
		// SetOnPush has a single slot, so the two chains dispatch on `kind` here.
		reconnector.SetOnPush(func(kind string, payload []byte, _ uint64) {
			switch kind {
			case "fauna.bridges.atproto.projection_ready":
				// Wake the projection loop; the poll ticker is the backstop, so a
				// missed or hint-less nudge only delays by one poll interval.
				select {
				case projNudge <- struct{}{}:
				default:
				}
			case "fauna.bridges.atproto.sessions_changed":
				// F1: keep the per-account external-apps kill-switch cache immediate.
				var push struct {
					ActorID             []byte `cbor:"actor_id"`
					ExternalAppsEnabled *bool  `cbor:"external_apps_enabled"`
				}
				if err := cbor.Unmarshal(payload, &push); err != nil {
					logger.Warn("sessions_changed push: undecodable payload", "err", err)
					return
				}
				pds.HandleSessionsChanged(push.ActorID, push.ExternalAppsEnabled)
			case "fauna.bridges.atproto.issuer_key_rotated":
				// TP5: the admin rotated the NEST's issuer key set — wake the
				// refresh loop to re-read it. Hint-less by design, but
				// load-bearing for the forced arm: the dropped `kid`s are
				// still in the set this process holds, and only a re-read
				// removes them.
				select {
				case issuerKeyNudge <- struct{}{}:
				default:
				}
			case "fauna.bridges.atproto.permission_set_requested":
				// The nest's own /oauth/par asking this bridge to resolve an
				// `include:<NSID>` through the chain only this bridge runs
				// (atproto-oauth-provider.md § Implementation status today, the
				// 2026-09-25 bullet). A REQUEST, not a nudge: answered over
				// deliver_permission_set, off this read loop, and the nest's
				// own deadline is the fallback for a push that never lands.
				var push struct {
					RequestID []byte `cbor:"request_id"`
					NSID      string `cbor:"nsid"`
				}
				if err := cbor.Unmarshal(payload, &push); err != nil {
					logger.Warn("permission_set_requested push: undecodable payload", "err", err)
					return
				}
				pds.HandlePermissionSetRequested(ctx, push.RequestID, push.NSID)
			}
		})

		// Drive the projection loop concurrently with the mint loop + XRPC
		// listener, rooted in the process ctx.
		go runProjectionLoop(ctx, reconnector, projDeps, projNudge, logger)

		// Keep the NEST's issuer key set current, so this resource server
		// honours the tokens the nest's authorization server mints across its
		// rotations (TP5): push-nudged, reconnect-nudged, unknown-`kid`-nudged,
		// slow-ticker backstop.
		pds.SetIssuerKeyNudge(issuerKeyNudge)
		go runIssuerKeyRefreshLoop(ctx, reconnector, pds, issuerKeyNudge, logger)

		// ── TLS termination (atproto-pds-full.md § Wire & process topology, F1
		// packaging resolution). The SNI router is pure L4 pass-through, so the
		// bridge terminates `pds.<domain>` TLS ITSELF: it fetches the sealed
		// TLS-cert blob from nest (fauna.bridges.fetch_tls_cert_blob, seal-on-read
		// bound to this bridge's attested x25519 + the fetched domain) and serves
		// it. Same fetch→unseal→cache Provider the mail MDA uses for CalDAV/IMAP
		// TLS (internal/tls); the on-disk cert's SANs cover `pds.<domain>`.
		tlsProvider, terr := bridgetls.New(bridgetls.Config{
			Domain:       bridgetls.CertFetchDomain(snapshot.PrimaryDomain),
			Role:         whoami.Role,
			BridgeID:     whoami.BridgeID,
			X25519Secret: bootX25519[:],
			Caller:       reconnector,
			Logger:       logger,
		})
		if terr != nil {
			return fmt.Errorf("build pds tls provider: %w", terr)
		}
		// First refresh is soft — the Start() loop retries on backoff, so the
		// cert lands before any XRPC request even if it isn't ready at boot (the
		// bridge just approved, or its x25519 not yet sealed-to). Mirrors the MDA.
		refreshCtx, refreshCancel := context.WithTimeout(ctx, rpcDeadline)
		if rerr := tlsProvider.Refresh(refreshCtx); rerr != nil {
			logger.Warn("initial pds tls refresh failed (will retry on backoff schedule)",
				"primary_domain", snapshot.PrimaryDomain, "err", rerr)
		}
		refreshCancel()
		tlsProvider.Start(ctx)
		tlsConfig := &tls.Config{
			GetCertificate: tlsProvider.GetCertificate,
			MinVersion:     tls.VersionTLS12,
		}

		// Layering, innermost → outermost: proxyproto → TLS. proxyproto is
		// INNERMOST — the PROXY-v2 header the SNI router prepends arrives on the
		// raw socket BEFORE the TLS ClientHello, so peeling it first makes the
		// real client IP the conn's RemoteAddr (so `xrpc.clientIP` / the IPLimiter
		// key on the real client, not the router's loopback — the mda.go dav-443
		// stack). A direct headerless dial (tier_3 via HTTPS skip-verify) has no
		// header, so proxyproto passes it through untouched.
		//
		// WithRouterAuth exists for the same reason the dav-443 stack takes it:
		// 8447 is a LOOPBACK bind, so a compromised co-resident bridge UID can
		// dial it and forge a header to spoof the very client IP the per-IP XRPC
		// rate limits and the audit key on. With the artifact-provisioned secret
		// only the router's header may rewrite the source (security.md
		// § Co-resident process trust boundary); nil ⇒ trust-any-loopback.
		rawXrpc, lerr := net.Listen("tcp", *xrpcListen)
		if lerr != nil {
			return fmt.Errorf("xrpc listen on %s: %w", *xrpcListen, lerr)
		}
		routerProxyAuth := proxyproto.RouterAuthFromEnv(logger)
		logger.Info("xrpc proxy-header trust resolved", "authenticated", len(routerProxyAuth) > 0)
		xrpcListener := tls.NewListener(
			proxyproto.New(rawXrpc, proxyproto.WithRouterAuth(routerProxyAuth)), tlsConfig)

		mux, merr := newPDSMux(xrpcServer, pdsHost, snapshot.PrimaryDomain)
		if merr != nil {
			return merr
		}
		httpServer := newPDSServer(mux)
		go func() {
			logger.Info("xrpc listener up", "addr", *xrpcListen, "pds_host", pdsHost)
			if serr := httpServer.Serve(xrpcListener); serr != nil && !errors.Is(serr, http.ErrServerClosed) {
				xrpcListenErr <- serr
			}
		}()
		defer func() {
			shutdownCtx, shutdownCancel := context.WithTimeout(context.Background(), 5*time.Second)
			defer shutdownCancel()
			_ = httpServer.Shutdown(shutdownCtx)
			tlsProvider.Close()
		}()
	}

	// ── S2 mint loop: poll the identity roster and mint pending DIDs ──
	//
	// One pass immediately, then every mintPollInterval, plus a nudge right
	// after each reconnect. Repo/MST/firehose translation is S3; the F1 XRPC
	// listener (above, when enabled) runs concurrently. Besides minting and
	// serving, the bridge holds the authenticated, self-healing WS open and
	// waits for a shutdown signal, an admin revoke, or an XRPC listener
	// failure.
	x25519Secret := kf.X25519Secret()
	deps := newMintDeps(x25519Secret[:])
	logger.Info("fauna-atproto-bridge enrolled; mint loop starting (S2: identity surface)",
		"poll_interval", mintPollInterval.String(),
		"plc_directory", deps.directoryBaseURL,
		"xrpc_listen", *xrpcListen,
	)
	ticker := time.NewTicker(mintPollInterval)
	defer ticker.Stop()
	var loopErr error
loop:
	for {
		runMintPass(ctx, reconnector, deps, logger)
		select {
		case <-ctx.Done():
			logger.Info("shutdown signal received; closing")
			break loop
		case <-reconnector.Done():
			// The reconnect loop stopped permanently. The only non-Close reason is a
			// revoke (whoami denied on a reconnect) — treat it as a shutdown trigger
			// (mail-bridge-lifecycle.md § Shutting down).
			if rerr := reconnector.Err(); errors.Is(rerr, wsrpc.ErrBridgeRevoked) {
				logger.Warn("nest revoked this bridge's service user; shutting down")
			} else {
				logger.Info("ws-rpc connection permanently closed; shutting down", "err", rerr)
			}
			break loop
		case err := <-xrpcListenErr:
			logger.Error("xrpc listener failed; shutting down", "err", err)
			loopErr = fmt.Errorf("xrpc listener: %w", err)
			break loop
		case <-ticker.C:
		case <-mintNudge:
		}
	}
	// The deferred reconnector.Close() (and the XRPC httpServer.Shutdown, when
	// enabled) tear down the listeners.
	logger.Info("fauna-atproto-bridge stopped cleanly")
	return loopErr
}

// newPDSMux builds the PDS listener's HTTP surface: everything under /xrpc/ is
// F1's route table (C5 — one table), and two plain routes sit beside it —
// /.well-known/did.json and the OAuth protected-resource document — because
// neither a DID document nor discovery metadata is an XRPC method.
//
// The DID doc served is the PDS *service* DID (`did:web:pds.<domain>`) — the
// identifier nest publishes as pds_endpoint and F1's session tokens carry — not
// any user's. Per-user did:web documents live on the user's own handle domain
// (internal/atprotoid resolve.go), which this host is not.
//
// ⚠ **This PDS is a resource server only; the authorization server is the
// nest's**, on the apex domain (`authorization-server.md` § The issuer). So the
// protected-resource document names `https://<apex>` in
// `authorization_servers`, and NONE of the authorization server's own routes —
// /oauth/{par,authorize,authorize/poll,token,revoke}, /oauth/jwks, or
// /.well-known/oauth-authorization-server — is mounted here. The bridge-hosted
// authorization server that once answered them retired in the same change that
// re-pointed this document, because leaving its routes up while the document
// named the nest would have made a sign-out at the nest's /oauth/revoke a
// silent no-op for a bridge-minted token (§ The issuer → *The re-point, the
// teaching, and the bridge AS's retirement are ONE change*). pdsmux_test.go pins
// both halves against the real mux.
func newPDSMux(xrpcServer http.Handler, pdsHost, apexDomain string) (*http.ServeMux, error) {
	serviceDIDDoc, err := atprotoid.BuildServiceDIDWebDoc(pdsHost)
	if err != nil {
		return nil, fmt.Errorf("build service did:web doc: %w", err)
	}
	mux := http.NewServeMux()
	mux.Handle("/xrpc/", xrpcServer)
	mux.HandleFunc("/.well-known/did.json", func(w http.ResponseWriter, r *http.Request) {
		if r.Method != http.MethodGet {
			w.Header().Set("Allow", http.MethodGet)
			http.Error(w, "method not allowed", http.StatusMethodNotAllowed)
			return
		}
		w.Header().Set("Content-Type", "application/did+ld+json")
		_, _ = w.Write(serviceDIDDoc)
	})
	mountProtectedResourceDocument(mux, apexDomain)
	return mux, nil
}

// protectedResourcePath is RFC 9728's well-known path for the document.
const protectedResourcePath = "/.well-known/oauth-protected-resource"

// mountProtectedResourceDocument serves `/.well-known/oauth-protected-resource`.
//
// The body is rendered in shared Rust (every field name in it is wire
// vocabulary a client branches on), so this route only writes bytes and a
// content type. It is GET-only and public by definition — RFC 9728 has clients
// fetch it unauthenticated, and everything in it is already public.
//
// Rendered once at mux construction rather than per request: it is a pure
// function of the apex domain, which this process reads once at boot.
//
// ⚠ **A deployment with no claimed domain has no authorization server to
// name**: the issuer identifier IS the nest's apex, so there is no issuer until
// one is claimed. The route answers `503` then, the same shape the nest's own
// issuer surfaces take for that state (`oauth_issuer_routes.rs::no_issuer_yet`)
// — the surface is real, its prerequisite is not here yet. Inventing an
// authorization server (this host, an IP) would point clients at an issuer
// whose tokens stop verifying the moment a real domain is claimed.
//
// Open to every origin, with no credentials (`Access-Control-Allow-Origin: *`):
// a browser-based ATProto client reads this document first, from its own
// origin, before it can even find the issuer — the same posture the nest's
// issuer plane takes on the documents it then reads there
// (`authorization-server.md` § The issuer → *Cross-origin access*). The header
// goes on every answer, the `503` and the `405` included, so a browser can
// read the refusal rather than see an opaque network error.
func mountProtectedResourceDocument(mux *http.ServeMux, apexDomain string) {
	var payload []byte
	if apexDomain != "" {
		payload = []byte(faunaAtproto.OauthProtectedResourceDocument(apexDomain))
	}
	mux.HandleFunc(protectedResourcePath, func(w http.ResponseWriter, r *http.Request) {
		w.Header().Set("Access-Control-Allow-Origin", "*")
		if r.Method != http.MethodGet {
			w.Header().Set("Allow", http.MethodGet)
			http.Error(w, "method not allowed", http.StatusMethodNotAllowed)
			return
		}
		if payload == nil {
			http.Error(w, "this deployment has not claimed a domain yet, so it has no "+
				"authorization server to name", http.StatusServiceUnavailable)
			return
		}
		w.Header().Set("Content-Type", "application/json")
		_, _ = w.Write(payload)
	})
}

// Listener bounds for the public PDS surface. This listener is reachable by any
// IP on the internet with no token, so an unbounded phase of the request cycle
// is a resource one anonymous peer can hold for free.
//
//	pdsReadHeaderTimeout — a peer must finish its request HEADERS in this long.
//	  The classic slow-header hold: without it a connection that dribbles bytes
//	  keeps a goroutine and an fd indefinitely, before any route, rate limit or
//	  auth class has even been resolved.
//	pdsIdleTimeout — how long an established keep-alive connection may sit
//	  between requests. Set explicitly rather than inherited from ReadTimeout
//	  (Go's fallback), so it stays a stated bound instead of a side effect.
const (
	pdsReadHeaderTimeout = 15 * time.Second
	pdsIdleTimeout       = 2 * time.Minute
)

// newPDSServer wraps the PDS mux in the listener bounds above.
//
// ⚠ ReadTimeout and WriteTimeout are deliberately LEFT ZERO, and that is a
// correctness requirement, not an omission: com.atproto.sync.subscribeRepos is a
// WebSocket on this same server, held open for hours by design and legitimately
// silent for all of it, so a server-wide read or write deadline would evict
// exactly the healthy relays the firehose exists to feed. The per-request bound
// lives one layer in, where it can tell a stream from a reply — the xrpc frame
// arms a re-armable response-stall deadline on every route that is not
// Route.LongLived (internal/xrpc), and the firehose bounds its own frame writes.
// Setting them here would be a silent availability regression.
func newPDSServer(mux http.Handler) *http.Server {
	return &http.Server{
		Handler:           mux,
		ReadHeaderTimeout: pdsReadHeaderTimeout,
		IdleTimeout:       pdsIdleTimeout,
	}
}

// issuerKeyRefreshBackstop is the issuer-key refresh loop's ticker cadence —
// the correctness backstop for an issuer_key_rotated push lost on a live
// connection.
//
// Ten minutes, and deliberately not shorter: the pickup rule this loop implements is that a verifier re-reads on
// an unknown `kid` and on a nudge, "never on a fixed long TTL alone"
// (authorization-server.md § The issuer → Two rotation arms). The ticker is the
// third path, not the first, so shortening it would buy a worse trade — more
// unprompted load for a window the push already closes in seconds.
const issuerKeyRefreshBackstop = 10 * time.Minute

// runIssuerKeyRefreshLoop keeps the NEST's issuer key set current: re-fetch and
// REPLACE on the issuer_key_rotated push, on reconnect (a rotation during an
// outage is only visible by re-fetching), on an unknown `kid` at the verify
// path, and on the ticker backstop.
//
// Never fails the process: a failed refresh keeps the set already held and
// retries on the next wake. That is the right failure direction here — the
// alternative, dropping the set, would refuse every honest nest-minted token
// because the nest was briefly unreachable.
func runIssuerKeyRefreshLoop(
	ctx context.Context,
	c wsrpc.Caller,
	pds *atprotopds.Server,
	nudge <-chan struct{},
	logger *slog.Logger,
) {
	ticker := time.NewTicker(issuerKeyRefreshBackstop)
	defer ticker.Stop()
	for {
		fctx, cancel := context.WithTimeout(ctx, rpcDeadline)
		set, err := wsrpc.FetchAtprotoIssuerJWKS(fctx, c)
		cancel()
		switch {
		case err != nil:
			logger.Warn("nest issuer key refresh failed; keeping the current set", "err", err)
		default:
			beforeIssuer := pds.NestIssuerURL()
			if serr := pds.SetNestIssuerKeys(set.Issuer, toIssuerJWKs(set.Keys)); serr != nil {
				logger.Warn("nest issuer key set refused; keeping the current set", "err", serr)
			} else if set.Issuer != beforeIssuer {
				// The issuer this resource server honours changed — the
				// re-point itself, or a deployment that has not claimed a
				// domain yet. Public information, and the line that tells an
				// admin the flip actually reached the serving process.
				logger.Info("nest issuer honoured by this resource server changed",
					"old_issuer", beforeIssuer, "new_issuer", set.Issuer,
					"keys", len(set.Keys))
			}
		}
		select {
		case <-ctx.Done():
			return
		case <-nudge:
		case <-ticker.C:
		}
	}
}

// toIssuerJWKs converts the WS-RPC reply's keys to the resource server's own
// shape. The conversion exists so atprotopds keeps no dependency on how the set
// arrived — it is fed the same way from a test as from the wire.
func toIssuerJWKs(keys []wsrpc.IssuerKey) []atprotopds.NestIssuerJWK {
	out := make([]atprotopds.NestIssuerJWK, 0, len(keys))
	for _, k := range keys {
		out = append(out, atprotopds.NestIssuerJWK{Kid: k.Kid, X: k.X, Y: k.Y})
	}
	return out
}
