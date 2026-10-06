// enroll.go — zero-touch bridge self-enrollment poll loop (cold-boot phase).
//
// A freshly-generated keypair has no enrollment row in nest, so the standard
// challenge/verify auth path (auth.go / Dial) would 404 ("actor not
// registered") and the process would exit → s6 restart loop. Instead, at cold
// boot the bridge announces its pubkey over the anonymous (pre-identity) WS
// (DialAnonymous → fauna.bridges.request_enrollment) and *polls* it on the
// 1/2/4/8/16/30 s backoff until an admin approves the bridge from their Fauna
// app. Approval inserts the audit `users` row, after which the normal
// challenge/verify → Whoami → register → fetch_config running flow proceeds.
//
// The bridge must NOT crash-loop on the pre-approval *state* (status=pending) —
// it waits in-process on the anonymous WS. It does still exit on a genuine
// connection/RPC error (the anonymous WS dropping mid-wait): s6 restarts it and
// it re-enrolls idempotently. Per mail-bridge-lifecycle.md § Cold boot steps
// 3–4 / § Pending approval, the poll re-uses the same anonymous connection (no
// in-poll reconnect, no token).
package wsrpc

import (
	"context"
	"crypto/ed25519"
	"fmt"
	"log/slog"
	"net/http"
	"time"
)

// EnrollmentIdentity is the material a bridge presents at zero-touch
// self-enrollment (fauna.bridges.request_enrollment): its Ed25519 pubkey + the
// advisory role/bridge-id, plus the slice-2 proof-of-possession fields — its
// x25519 pubkey and a static Ed25519 signature over EnrollmentSignedMessage.
//
// The PoP fields (X25519Pub / EnrollmentSig) are non-nil only on the image path,
// where the deployment artifact minted a blessed keypair and nest has the role's
// blessed pubkey on record (FAUNA_BLESSED_{MTA,MDA}_PUBKEY) — there nest requires
// the enroller to BE that key and prove possession of it. A dev/binary-only bridge
// with no artifact keypair (and tests) leaves them nil and nest takes the lenient registry-absent path. Bundling them
// here keeps the 3-layer enrollment call chain (EnrollAndAwaitApproval →
// PollEnrollmentUntilApproved → RequestEnrollment) from threading five loose
// arguments. security.md § Enrollment proof-of-possession contract.
type EnrollmentIdentity struct {
	Ed25519Pub    []byte
	X25519Pub     []byte // nil on a dev/binary-only bridge
	EnrollmentSig []byte // nil on a dev/binary-only bridge
	RoleHint      string
	BridgeID      string
}

// EnrollmentSignedMessage mirrors fauna_protocol::wrapped_blob::
// enrollment_signed_message — THE single source of the slice-2 PoP signed-message
// contract, so nest's verifier (bridge_blob_handlers::check_enrollment_authorization)
// and this signer cannot drift (mirrors auth.handshakeSignedMessage / the Rust
// auth::handshake_signed_message). The bytes: a fixed domain tag, then
// ed25519Pub(32) ‖ x25519Pub(32) ‖ role (role ∈ "mta"/"mda", the canonical
// strings). A static, context-bound signature (no nonce, no second round) is
// deliberate: replay of (pubkey, sig) only re-asserts the same artifact-blessed
// identity, whose private half stays UID-isolated on disk, and the domain tag +
// role bind it to this purpose. Pure byte assembly — crypto-free, matching the
// Rust single source.
func EnrollmentSignedMessage(role string, ed25519Pub, x25519Pub []byte) []byte {
	const domain = "fauna.bridges.enroll.v1"
	msg := make([]byte, 0, len(domain)+len(ed25519Pub)+len(x25519Pub)+len(role))
	msg = append(msg, domain...)
	msg = append(msg, ed25519Pub...)
	msg = append(msg, x25519Pub...)
	msg = append(msg, role...)
	return msg
}

// SignEnrollment produces the slice-2 proof-of-possession signature an enrolling
// bridge presents as `enrollment_sig`: the Ed25519 signature, under the bridge's
// keyfile signing key, over EnrollmentSignedMessage(role, ed25519Pub, x25519Pub).
// ed25519Pub must be signKey's public half (the artifact-blessed key nest has on
// record for the role). Kept here beside the message assembly so the "what bytes
// + which key" pair lives in one place, like auth.go owns its challenge signing.
func SignEnrollment(signKey ed25519.PrivateKey, role string, ed25519Pub, x25519Pub []byte) []byte {
	return ed25519.Sign(signKey, EnrollmentSignedMessage(role, ed25519Pub, x25519Pub))
}

// Bridge enrollment / lifecycle status values, as returned by
// RequestEnrollment and WhoamiReply.Status (mail-bridge-lifecycle.md
// § Lifecycle phases). nest may also return "unknown" for a never-seen pubkey
// on some surfaces; the poll loop treats any non-approved/non-revoked value as
// "keep waiting".
const (
	StatusPending  = "pending"
	StatusApproved = "approved"
	StatusRevoked  = "revoked"
)

// enrollCallTimeout bounds a single request_enrollment poll RPC so a hung nest
// mid-wait surfaces as an error (→ exit → s6 restart → idempotent re-enroll)
// rather than blocking the poll until process shutdown. Matches the cold-boot
// rpcDeadline the other bootstrap RPCs use.
const enrollCallTimeout = 30 * time.Second

// anonymousDialTimeout bounds the initial anonymous-WS dial in
// EnrollAndAwaitApproval (the long-lived poll uses the caller's ctx).
const anonymousDialTimeout = 30 * time.Second

// EnrollPollOptions configures PollEnrollmentUntilApproved. The zero value is
// production-ready: the BackoffSchedule curve, a ctx-aware time sleep, and
// slog.Default(). Tests inject Backoff/sleep to drive the state machine without
// real time.
type EnrollPollOptions struct {
	// Backoff returns the n-th (0-based) inter-poll delay. nil → BackoffSchedule.
	Backoff func(n int) time.Duration
	// sleep waits for d or until ctx is cancelled, returning ctx.Err() on
	// cancel. nil → a ctx-aware timer sleep. Unexported: only same-package
	// tests inject a fake clock.
	sleep func(ctx context.Context, d time.Duration) error
	// Logger for per-poll status lines; nil → slog.Default().
	Logger *slog.Logger
}

// PollEnrollmentUntilApproved repeatedly calls RequestEnrollment over c on the
// configured backoff until the enrollment status reaches a terminal outcome:
//
//   - StatusApproved → returns nil (proceed to the authenticated flow).
//   - StatusRevoked  → returns ErrBridgeRevoked (admin rejected/revoked; the
//     caller shuts down — there are no listeners to drain pre-approval).
//   - ctx cancelled  → returns ctx.Err() (SIGTERM while waiting → clean exit).
//   - RPC/connection error → returns it (fatal; s6 restart re-enrolls).
//
// StatusPending (or any unrecognized status) keeps polling. The loop never
// re-dials — it issues every poll over the same Caller (the anonymous
// connection from DialAnonymous), per § Pending approval.
func PollEnrollmentUntilApproved(ctx context.Context, c Caller, id EnrollmentIdentity, opts EnrollPollOptions) error {
	backoff := opts.Backoff
	if backoff == nil {
		backoff = BackoffSchedule
	}
	sleep := opts.sleep
	if sleep == nil {
		sleep = sleepCtx
	}
	logger := opts.Logger
	if logger == nil {
		logger = slog.Default()
	}

	for attempt := 0; ; attempt++ {
		callCtx, cancel := context.WithTimeout(ctx, enrollCallTimeout)
		status, err := RequestEnrollment(callCtx, c, id)
		cancel()
		if err != nil {
			return fmt.Errorf("enrollment poll: %w", err)
		}
		switch status {
		case StatusApproved:
			logger.Info("bridge enrollment approved by admin")
			return nil
		case StatusRevoked:
			return ErrBridgeRevoked
		case StatusPending:
			// Expected pre-approval state — keep waiting.
		default:
			// Forward-compatible: an unrecognized status ("unknown" etc.) is
			// not a terminal outcome, so keep polling rather than exit.
			logger.Warn("request_enrollment returned unrecognized status; treating as pending", "status", status)
		}
		d := backoff(attempt)
		logger.Info("bridge enrollment pending; awaiting admin approval in a Fauna app",
			"role_hint", id.RoleHint,
			"next_poll_in", d.String(),
			"poll_attempt", attempt+1,
		)
		if err := sleep(ctx, d); err != nil {
			return err // ctx cancelled → clean exit upstream
		}
	}
}

// EnrollAndAwaitApproval is the production cold-boot glue: it opens the
// anonymous WS, self-enrolls, and blocks (polling) until the bridge is approved
// — returning nil on approval, ErrBridgeRevoked on revoke, or ctx.Err() on a
// shutdown signal received while waiting. The anonymous connection is closed
// before returning; the caller then runs the authenticated Dial flow.
func EnrollAndAwaitApproval(ctx context.Context, nestEndpoint string, id EnrollmentIdentity, logger *slog.Logger, httpClient *http.Client) error {
	if logger == nil {
		logger = slog.Default()
	}
	dialCtx, dialCancel := context.WithTimeout(ctx, anonymousDialTimeout)
	anon, err := DialAnonymous(dialCtx, AnonymousClientConfig{
		NestEndpoint: nestEndpoint,
		Logger:       logger,
		HTTPClient:   httpClient,
	})
	dialCancel()
	if err != nil {
		return fmt.Errorf("dial anonymous ws for enrollment: %w", err)
	}
	defer func() { _ = anon.Close() }()
	logger.Info("self-enrolling over anonymous WS", "role_hint", id.RoleHint)
	return PollEnrollmentUntilApproved(ctx, anon, id, EnrollPollOptions{Logger: logger})
}

// sleepCtx waits for d or until ctx is cancelled, returning ctx.Err() on
// cancel. The production EnrollPollOptions.sleep.
func sleepCtx(ctx context.Context, d time.Duration) error {
	t := time.NewTimer(d)
	defer t.Stop()
	select {
	case <-ctx.Done():
		return ctx.Err()
	case <-t.C:
		return nil
	}
}
