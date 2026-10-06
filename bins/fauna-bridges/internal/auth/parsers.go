// Package auth holds the protocol-neutral pieces of the
// AEAD-unwrap-as-auth flow shared by the IMAP MDA (Phase C.3) and the
// SMTP submission MTA (Phase D.2): SASL payload parsers, the email
// splitter, and the audit-log redaction helper.
//
// What does NOT live here: the per-protocol "what happens after the
// AEAD-unwrap succeeds" glue (IMAP stashes an MLSCapability for body
// decrypt; SMTP stashes a SubmissionToken for quota enforcement). Those
// have different result types and live in their respective package.
//
// Both consumers wrap go-sasl's PLAIN / OAUTHBEARER server in identical
// shape: the SASL authenticator closure rebuilds the canonical payload
// via BuildPlainPayload / BuildOAuthBearerPayload and dispatches to a
// per-protocol workhorse that re-parses via the helpers here. The
// rebuild-then-reparse pattern keeps tests able to drive the workhorse
// directly with a raw payload string without spinning up the full SASL
// pipeline (mirrors the IMAP test path at
// `bins/fauna-bridges/internal/mda/imap/auth_test.go`).
package auth

import (
	"errors"
	"strings"
	"time"
)

// RPCTimeout caps every wsrpc round-trip during AUTH so a stuck nest
// doesn't hang the bridge connection. Shared by IMAP MDA (per
// imap-server.md § Authentication — "AUTH failures (including timeout)
// must surface as IMAP NO Authentication failed") and SMTP submission
// (per smtp-server.md § Auth on each port — same timeout discipline,
// 535 5.7.8 on timeout). The 30-second budget covers a slow Argon2id
// derive on a contended bridge plus the validate_recipient and
// fetch_wrapped_*_blob round-trips.
const RPCTimeout = 30 * time.Second

// ReasonRedacted is the audit-log reason for any AUTH failure path
// where the underlying SDK error could otherwise leak credential
// bytes. Per imap-server.md / smtp-server.md § Authentication: AUTH
// failures are reported to nest with a coarse category; the
// underlying SDK error string never rides over the WS-RPC channel.
const ReasonRedacted = "auth failed"

// ── PLAIN (RFC 4616) ──────────────────────────────────────────────

// BuildPlainPayload rebuilds the canonical RFC 4616 PLAIN payload from
// the (authzID, authcID, password) triple the go-sasl PLAIN authenticator
// closure receives. The wire shape is `<authzid> NUL <authcid> NUL
// <password>`; an empty authzid is the common "no separate authorization
// identity" case.
func BuildPlainPayload(authzID, authcID, password string) string {
	return authzID + "\x00" + authcID + "\x00" + password
}

// ParsePlainPayload splits a PLAIN payload into (authcid, password).
// The authzid (if present) must be empty or equal to authcid per RFC
// 4616; mismatch returns ErrAuthzIDMismatch. Caller should treat any
// error as "AUTH failed" without leaking the specific reason on the
// wire — the redacted reason rides into the audit log via
// RedactedFailReason.
func ParsePlainPayload(payload string) (authcid, password string, err error) {
	parts := strings.SplitN(payload, "\x00", 3)
	if len(parts) != 3 {
		return "", "", ErrPlainMalformed
	}
	authzid, user, pass := parts[0], parts[1], parts[2]
	if authzid != "" && authzid != user {
		return "", "", ErrAuthzIDMismatch
	}
	return user, pass, nil
}

// ── OAUTHBEARER (RFC 7628) ────────────────────────────────────────

// BuildOAuthBearerPayload rebuilds the canonical GS2 envelope from the
// (username, token) pair the go-sasl OAUTHBEARER server hands the
// authenticator closure. Wire shape:
//
//	"n,a=<username>,\x01auth=Bearer <token>\x01\x01"
//
// `n,` is the GS2 no-channel-binding header; host/port kvps are
// advisory and trusted at the TLS layer above, so the builder omits
// them. The trailing `\x01\x01` is the RFC 7628 end-of-message marker.
func BuildOAuthBearerPayload(username, token string) string {
	return "n,a=" + username + ",\x01auth=Bearer " + token + "\x01\x01"
}

// ParseOAuthBearerPayload extracts (username, token) from the GS2-framed
// OAUTHBEARER client-first message. Mirrors the parse in go-sasl's
// oauthbearer server; redone here so the workhorse can be called
// directly from tests without spinning up the SASL pipeline. Permissive
// about kvp ordering — only `auth=Bearer <token>` is required after the
// authzid, anything else (host=, port=) is ignored.
//
// Returns ok=false for any malformed shape (missing GS2 header, missing
// authzid, missing auth= kvp, missing "Bearer " prefix, empty token).
func ParseOAuthBearerPayload(payload string) (username, token string, ok bool) {
	const flag = "n,"
	if !strings.HasPrefix(payload, flag) {
		return "", "", false
	}
	rest := payload[len(flag):]
	comma := strings.IndexByte(rest, ',')
	if comma < 0 {
		return "", "", false
	}
	authzid := rest[:comma]
	if strings.HasPrefix(authzid, "a=") {
		username = authzid[len("a="):]
	}
	kvpStr := rest[comma+1:]
	for _, p := range strings.Split(kvpStr, "\x01") {
		if p == "" {
			continue
		}
		if eq := strings.IndexByte(p, '='); eq > 0 {
			k, v := p[:eq], p[eq+1:]
			if k == "auth" {
				const prefix = "bearer "
				if strings.HasPrefix(strings.ToLower(v), prefix) {
					token = v[len(prefix):]
				}
			}
		}
	}
	if username == "" || token == "" {
		return "", "", false
	}
	return username, token, true
}

// ── Email splitter ────────────────────────────────────────────────

// SplitEmail splits "user@domain" into (local, domain, true). The
// trivial validator is intentionally loose — nest is the authority on
// whether the recipient resolves; the bridge just needs to extract the
// two halves for the validate_recipient RPC. An at-most-one '@' is not
// enforced (we use the LAST '@' so addresses with quoted local parts
// containing '@' Just Work to the extent nest accepts them).
func SplitEmail(addr string) (local, domain string, ok bool) {
	i := strings.LastIndexByte(addr, '@')
	if i <= 0 || i == len(addr)-1 {
		return "", "", false
	}
	return addr[:i], addr[i+1:], true
}

// SplitEmailDefault is SplitEmail with a fallback: when addr carries no
// domain at all (a bare username with no '@'), it resolves the whole addr
// as the local part. This lets every bridge AUTH surface accept the bare
// username many MUAs send — notably macOS Calendar.app, which parses a
// configured `user@domain`, uses the domain for server discovery, and sends
// only the bare local part as the CalDAV Basic-auth username (so a strict
// `user@domain` requirement 401s a correctly-configured Apple Calendar
// before discovery even starts).
//
// defaultDomain is the box's PrimaryDomain (the single-domain TLS/EHLO
// anchor; mail-multidomain.md § The primary domain). A bare username is
// resolved under defaultDomain when one exists; when it is empty — the
// domainless / bare-IP nest reached by any locator (the any-locator design;
// tracked internally) —
// the bare username flows through with an EMPTY domain so it still reaches the
// validate_recipient RPC, where nest now resolves it against the unique
// handle→actor store (an empty / unregistered domain takes nest's handle
// fallback). This is the locator-honoring relaxation: `test`, `test@<IP>`,
// `test@<any-name>` all authenticate to the actor whose handle is the
// local-part, instead of the old "empty PrimaryDomain ⇒ strict user@domain"
// 401-before-discovery behavior.
//
// Only a genuine no-'@' bare username is rescued — the other SplitEmail
// rejections (`@domain`, `user@`) stay malformed, since they are corrupt
// rather than domain-less.
func SplitEmailDefault(addr, defaultDomain string) (local, domain string, ok bool) {
	if local, domain, ok = SplitEmail(addr); ok {
		return local, domain, true
	}
	if addr != "" && strings.IndexByte(addr, '@') < 0 {
		// Bare username (no '@'): default the domain to the box's
		// PrimaryDomain when one exists, else pass through domainless so
		// nest's handle-keyed fallback resolves it (the bare-IP nest).
		return addr, defaultDomain, true
	}
	return "", "", false
}

// ── Credential sub-addressing ─────────────────────────────────────

// DefaultCredentialID is the credential_id a bare username (no RFC 5233
// "+suffix") maps to — the first credential minted at enable-mail. Per
// `docs/goal/behavior/mail-credentials.md` § MUA-username convention: "for
// credential_id 'default' the suffix is omitted". All three bridge AUTH surfaces
// (IMAP/SMTP PLAIN MDA, CalDAV Basic MDA, SMTP submission MTA) default to it.
const DefaultCredentialID = "default"

// CredentialFromLocalPart splits an email local-part into the base mailbox and
// the credential_id per RFC 5233 sub-addressing (mail-credentials.md §
// MUA-username): "alice+phone" → ("alice", "phone"); "alice" → ("alice",
// "default"). The base is what validate_recipient resolves to the actor; the
// credential_id selects which per-credential wrapped-MSEK blob the bridge fetches
// and AEAD-unwraps. Splits on the FIRST '+' (matching nest's split_subaddress in
// libs/fauna-mail/src/aliases/mod.rs). An empty suffix ("alice+") carries no
// credential and falls back to the default.
func CredentialFromLocalPart(local string) (base, credentialID string) {
	if i := strings.IndexByte(local, '+'); i >= 0 {
		if suffix := local[i+1:]; suffix != "" {
			return local[:i], suffix
		}
		return local[:i], DefaultCredentialID
	}
	return local, DefaultCredentialID
}

// PrincipalKey reduces a resolved (base mailbox, domain) pair to the canonical
// identity the AUTH-failure lockout (internal/authlock) keys its fine and mid
// buckets on, so that *every presented form that resolves to the same credential
// shares one failure bucket*.
//
// The lockout previously keyed on the raw presented username, but
// CredentialFromLocalPart + SplitEmailDefault collapse several presented forms
// onto one credential: `alice`, `alice+`, and `alice+default` all map to (base
// "alice", credential "default"), and a bare `alice` resolves to the same actor
// as an explicit `alice@<primary-domain>`. Keyed raw, each of those forms opened
// a DISTINCT bucket and handed an attacker a fresh per-credential AUTH-failure
// allowance per form (2026-06-24 email-component-compromise review § B7). Keying
// the lockout on PrincipalKey(base, domain) closes that: the credential_id stays
// a separate key dimension (so genuinely different credentials keep distinct
// allowances — the deliberate per-credential design), while presentation
// variants of one credential now coincide.
//
// The domain is lower-cased (DNS is case-insensitive, so `alice@Example.COM` and
// `alice@example.com` are one account); the base local-part is left exactly as
// presented — RFC 5321 §2.4 makes local-parts case-sensitive and nest is the
// authority on their equivalence, so folding here could only wrongly *over*-merge
// two accounts. A domainless principal (empty domain — the bare-IP nest reached
// via SplitEmailDefault's pass-through) yields just the base.
func PrincipalKey(base, domain string) string {
	if domain == "" {
		return base
	}
	return base + "@" + strings.ToLower(domain)
}

// ── Audit-log redaction ───────────────────────────────────────────

// RedactedFailReason trims any error wording that could carry
// credential bytes from an AUTH failure, capping length so a
// maliciously-large blob error can't dominate the bridge's logs.
// The mailfauna errors today only carry shape/AEAD wording (no
// plaintext leakage), but defense in depth.
func RedactedFailReason(err error) string {
	if err == nil {
		return ""
	}
	msg := err.Error()
	const cap = 200
	if len(msg) > cap {
		msg = msg[:cap] + "…"
	}
	return msg
}

// ── Sentinel errors ───────────────────────────────────────────────

// ErrPlainMalformed is returned by ParsePlainPayload when the wire
// payload doesn't decompose into the (authzid, authcid, password)
// triple. Callers should treat this as an opaque AUTH failure on the
// wire and feed RedactedFailReason into the audit log.
var ErrPlainMalformed = errors.New("auth: PLAIN payload malformed")

// ErrAuthzIDMismatch is returned by ParsePlainPayload when the authzid
// is non-empty and differs from the authcid. Per RFC 4616, the
// authorization identity must be either empty or equal to the
// authentication identity; anything else is "authorization identity
// not supported" which the bridge surfaces as an auth-failure.
var ErrAuthzIDMismatch = errors.New("auth: PLAIN authzid does not match authcid")
