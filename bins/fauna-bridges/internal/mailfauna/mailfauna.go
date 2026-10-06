// Package mailfauna is the single import surface for the fauna-mail
// UniFFI Go bindings (and, in later phases, the other fauna_* namespaces
// that fauna-ffi aggregates). Centralizing the import here keeps the
// generated package names ("fauna_mail", "fauna_core", …) off the rest
// of the codebase and gives us one place to adjust if regen renames a
// symbol.
package mailfauna

import (
	"bytes"
	"crypto/rand"
	"encoding/hex"
	"fmt"
	"log/slog"

	"github.com/faunasocial/fauna/bins/fauna-bridges/internal/wsrpc"
	faunaCore "github.com/faunasocial/fauna/libs/fauna-mail-go/fauna_core"
	faunaFfi "github.com/faunasocial/fauna/libs/fauna-mail-go/fauna_ffi"
	faunaMail "github.com/faunasocial/fauna/libs/fauna-mail-go/fauna_mail"
)

// ParsedMessage is the Go-facing view of an RFC 5322 / MIME parse
// result. Optional fields are exposed as empty strings (and zero
// DateUnixSeconds) to keep call sites simple; nil-vs-empty distinctions
// are not load-bearing for the MTA path.
type ParsedMessage struct {
	From            string
	To              []string
	Cc              []string
	Bcc             []string
	Subject         string
	MessageID       string
	DateUnixSeconds int64
	BodyText        string
	BodyHTML        string
	Headers         []ParsedHeader
	MimeParts       []ParsedMimePart
}

type ParsedHeader struct {
	Name  string
	Value string
}

// ParsedMimePart mirrors the Rust ParsedMimePart fields. Disposition
// and Filename are empty when the underlying MIME part lacks them
// (e.g. an inline text/plain body part has neither). ContentType
// defaults to "application/octet-stream" when the underlying part has
// no Content-Type header, matching the Rust-side fallback.
type ParsedMimePart struct {
	ContentType string
	Disposition string
	Filename    string
	SizeBytes   uint64
}

// ParseRFC5322 parses an on-the-wire RFC 5322 / MIME message via the
// shared Rust parser (libs/fauna-mail/src/parser.rs).
func ParseRFC5322(b []byte) (ParsedMessage, error) {
	raw, err := faunaMail.ParseRfc5322(b)
	if err != nil {
		return ParsedMessage{}, err
	}
	out := ParsedMessage{
		From:            derefString(raw.From),
		To:              raw.To,
		Cc:              raw.Cc,
		Bcc:             raw.Bcc,
		Subject:         derefString(raw.Subject),
		MessageID:       derefString(raw.MessageId),
		DateUnixSeconds: derefInt64(raw.DateUnixSeconds),
		BodyText:        raw.BodyText,
		BodyHTML:        derefString(raw.BodyHtml),
	}
	for _, h := range raw.Headers {
		out.Headers = append(out.Headers, ParsedHeader{Name: h.Name, Value: h.Value})
	}
	for _, p := range raw.MimeParts {
		out.MimeParts = append(out.MimeParts, ParsedMimePart{
			ContentType: p.ContentType,
			Disposition: derefString(p.Disposition),
			Filename:    derefString(p.Filename),
			SizeBytes:   p.SizeBytes,
		})
	}
	return out, nil
}

// SenderDomainWithEnvelopeFallback returns the lowercased domain of the
// message's From header — `public_metadata.sender_domain` per
// `bridge_imap_messages.from_norm` (IMAP SEARCH FROM, the SPF-audit path) —
// falling back to `mailFrom`'s domain (the SMTP MAIL FROM:<...> reverse
// path) when the header yields none. Callers without an envelope (IMAP
// APPEND, the DKIM d= anchor) pass mailFrom="". The rule lives in shared
// Rust (`fauna_mail::envelope::sender_domain_with_envelope_fallback` — the
// 2026-07-12 lift that retired the Go ExtractSenderDomain copy; see its doc
// for the two deliberate semantic deltas, pinned by
// TestSenderDomainGoldenCorpus against the Rust golden corpus).
func SenderDomainWithEnvelopeFallback(raw []byte, mailFrom string) string {
	return faunaMail.SenderDomainWithEnvelopeFallback(raw, mailFrom)
}

// FromFieldCount returns how many From header fields raw's header section
// carries (`fauna_mail::from_field::from_field_count`). The inbound MX DATA
// stage and submission accept a message only when it is exactly one: given
// two, DMARC (mail-auth) aligns against the first while the apps and
// SenderDomainWithEnvelopeFallback read the last (smtp-server.md
// § Architectural rules). The count is lexical and generous — LF-only lines,
// bare CR, any case, whitespace before the colon — so it never undercounts
// what either parser sees.
func FromFieldCount(raw []byte) uint32 {
	return faunaMail.FromFieldCount(raw)
}

func derefString(p *string) string {
	if p == nil {
		return ""
	}
	return *p
}

func derefInt64(p *int64) int64 {
	if p == nil {
		return 0
	}
	return *p
}

// ── verify_inbound (Phase C.5) ────────────────────────────────────
//
// Type aliases re-export the UniFFI-generated verdict types under the
// `mailfauna` namespace so call sites don't have to import the
// generated package directly. They come from `fauna_core`, not
// `fauna_mail`: since 2026-08-17 the six verdict
// types have ONE Rust definition, `fauna_core::mail_auth`, which
// `fauna_mail::auth` and `fauna_protocol::bridge_routing` both
// re-export — and UniFFI attributes a type to its *defining* crate,
// so the generated Go lands in `fauna_core.go`. The UniFFI binding
// exposes each verdict's enum-with-payload as a Go interface plus
// per-variant structs (e.g. `DkimVerdictPass{}`, `DkimVerdictFail{
// Reason: ...}`); the per-variant constants (`SpfVerdictPass` etc.)
// are integer enum values for the all-unit variants.

// AuthVerdicts is the UniFFI-side mirror of the
// `fauna_core::mail_auth::AuthVerdicts` record. The four field types
// are tagged-union Go interfaces (or integer enums for all-unit
// variants); the wire-shape mirror in
// `internal/wsrpc/methods.go::AuthVerdicts` is what actually rides on
// the WS-RPC wire — use `AuthVerdictsToWire` for the conversion.
type AuthVerdicts = faunaCore.AuthVerdicts

// DkimVerdict / SpfVerdict / DmarcVerdict / ArcVerdict / DmarcPolicy
// re-export the UniFFI binding types. DkimVerdict + DmarcVerdict are
// Go interfaces (with per-variant struct implementations
// `DkimVerdictPass`, `DkimVerdictFail{Reason: ...}`, etc.);
// SpfVerdict + ArcVerdict + DmarcPolicy are integer enum constants
// (`SpfVerdictPass`, …).
type (
	DkimVerdict  = faunaCore.DkimVerdict
	SpfVerdict   = faunaCore.SpfVerdict
	DmarcVerdict = faunaCore.DmarcVerdict
	ArcVerdict   = faunaCore.ArcVerdict
	DmarcPolicy  = faunaCore.DmarcPolicy
)

// AuthError re-exports the UniFFI-generated error type. Use
// `errors.Is(err, mailfauna.ErrAuthErrorUnparseable)` to detect the
// unparseable case (Session.Data answers `554 5.6.0` on that).
type AuthError = faunaMail.AuthError

// ErrAuthErrorUnparseable is the sentinel returned (via wrapping)
// when verify_inbound cannot parse the raw bytes.
var ErrAuthErrorUnparseable = faunaMail.ErrAuthErrorUnparseable

// VerifyInbound runs the SPF/DKIM/DMARC/ARC verification pipeline
// over the raw RFC 5322 bytes, returning the four verdicts. The
// shared Rust impl in `libs/fauna-mail/src/auth.rs::verify_inbound`
// is async (it queries DNS via hickory-resolver); the UniFFI Go
// binding handles the async-tokio bridge transparently — this
// wrapper is a synchronous call from Go's perspective.
//
// On parser failure (unparseable input), the returned error wraps
// `ErrAuthErrorUnparseable`; the bridge's SMTP `Session.Data`
// translates that to `554 5.6.0` on the wire.
//
//   - raw: original on-the-wire RFC 5322 bytes (re-serialized
//     `ParsedMessage` bytes will NOT verify — DKIM is cryptographically
//     bound to the canonical original).
//   - mailFrom: SMTP envelope sender (the MAIL FROM:<...> value).
//   - clientIP: connecting MTA's IP as a string (an unparseable
//     value is mapped to 127.0.0.1 inside the Rust impl).
//   - clientHelo: HELO/EHLO hostname the peer announced.
func VerifyInbound(raw []byte, mailFrom, clientIP, clientHelo string) (AuthVerdicts, error) {
	return faunaMail.VerifyInbound(raw, mailFrom, clientIP, clientHelo)
}

// AuthVerdictsToWire translates the UniFFI-side `AuthVerdicts`
// (tagged-union Go) into the WS-RPC wire-shape mirror declared in
// `internal/wsrpc/methods.go`. The wire shape uses adjacently-tagged
// maps (`{"kind":...}` / `{"kind":..., "data":...}`) so it round-
// trips byte-for-byte with `libs/fauna-protocol::bridge_routing::
// AuthVerdicts` via DAG-CBOR.
//
// Pure (no I/O); safe to call before constructing
// `wsrpc.IngestInboundMailParams.Verdicts`.
func AuthVerdictsToWire(v AuthVerdicts) wsrpc.AuthVerdicts {
	return wsrpc.AuthVerdicts{
		Dkim:  dkimToWire(v.Dkim),
		Spf:   spfToWire(v.Spf),
		Dmarc: dmarcToWire(v.Dmarc),
		Arc:   arcToWire(v.Arc),
	}
}

func dkimToWire(v DkimVerdict) wsrpc.DkimVerdict {
	switch d := v.(type) {
	case faunaCore.DkimVerdictNone:
		return wsrpc.DkimVerdict{Kind: "none"}
	case faunaCore.DkimVerdictPass:
		return wsrpc.DkimVerdict{Kind: "pass"}
	case faunaCore.DkimVerdictFail:
		return wsrpc.DkimVerdict{
			Kind: "fail",
			Data: &wsrpc.DkimVerdictFail{Reason: d.Reason},
		}
	case faunaCore.DkimVerdictNeutral:
		return wsrpc.DkimVerdict{Kind: "neutral"}
	case faunaCore.DkimVerdictPermError:
		return wsrpc.DkimVerdict{Kind: "perm_error"}
	case faunaCore.DkimVerdictTempError:
		return wsrpc.DkimVerdict{Kind: "temp_error"}
	default:
		// Defensive: UniFFI regen could add a variant before this switch is
		// updated. The wire shape must stay valid (nest's
		// deny_unknown_fields fires on a new TOP-LEVEL field, not on a new
		// sub-variant kind) — but VALIDITY was the only thing the original
		// comment here considered, and it picked the wrong point of the
		// lattice.
		//
		// On an AUTHENTICATION verdict, `none` is the most permissive value
		// there is: RFC 7208/6376/7489 all read it as "no policy published /
		// no determination made". Emitting it for a variant Go could not map
		// asserts something FALSE and permissive. `temp_error` says "could
		// not determine", which is exactly what happened, and is the
		// deferring direction if a consumer ever acts on these.
		//
		// Ruled 2026-08-18 after tracing every
		// consumer: nest flattens these to RFC strings and RECORDS them
		// (bridge_routing_handlers.rs:5633 → InboundMailFields →
		// insert_inbound_mail) — no refusal decision rides on the wire
		// mirror; the SMTP-perimeter gates read the UniFFI AuthVerdicts
		// directly. So this is about recording the truth, and `temperror` is
		// the truth. Pinned by TestToWireDefaultArmsAreFailSafe.
		return wsrpc.DkimVerdict{Kind: "temp_error"}
	}
}

func spfToWire(v SpfVerdict) wsrpc.SpfVerdict {
	switch v {
	case faunaCore.SpfVerdictNone:
		return wsrpc.SpfVerdict{Kind: "none"}
	case faunaCore.SpfVerdictPass:
		return wsrpc.SpfVerdict{Kind: "pass"}
	case faunaCore.SpfVerdictFail:
		return wsrpc.SpfVerdict{Kind: "fail"}
	case faunaCore.SpfVerdictSoftFail:
		return wsrpc.SpfVerdict{Kind: "soft_fail"}
	case faunaCore.SpfVerdictNeutral:
		return wsrpc.SpfVerdict{Kind: "neutral"}
	case faunaCore.SpfVerdictPermError:
		return wsrpc.SpfVerdict{Kind: "perm_error"}
	case faunaCore.SpfVerdictTempError:
		return wsrpc.SpfVerdict{Kind: "temp_error"}
	default:
		// See dkimToWire's default arm for the ruling: `temp_error` ("could
		// not determine"), never `none` ("no policy published").
		return wsrpc.SpfVerdict{Kind: "temp_error"}
	}
}

func dmarcToWire(v DmarcVerdict) wsrpc.DmarcVerdict {
	switch d := v.(type) {
	case faunaCore.DmarcVerdictNone:
		return wsrpc.DmarcVerdict{Kind: "none"}
	case faunaCore.DmarcVerdictPass:
		return wsrpc.DmarcVerdict{Kind: "pass"}
	case faunaCore.DmarcVerdictFail:
		return wsrpc.DmarcVerdict{
			Kind: "fail",
			Data: &wsrpc.DmarcVerdictFail{Policy: dmarcPolicyToWire(d.Policy)},
		}
	case faunaCore.DmarcVerdictPermError:
		return wsrpc.DmarcVerdict{Kind: "perm_error"}
	case faunaCore.DmarcVerdictTempError:
		return wsrpc.DmarcVerdict{Kind: "temp_error"}
	default:
		// See dkimToWire's default arm for the ruling.
		return wsrpc.DmarcVerdict{Kind: "temp_error"}
	}
}

func dmarcPolicyToWire(p DmarcPolicy) string {
	switch p {
	case faunaCore.DmarcPolicyNone:
		return "none"
	case faunaCore.DmarcPolicyQuarantine:
		return "quarantine"
	case faunaCore.DmarcPolicyReject:
		return "reject"
	default:
		// `none` is RIGHT here, and for a different reason than it was WRONG
		// in the verdict switches above — the asymmetry is deliberate, so do
		// not "fix" this arm to match them.
		//
		// DmarcPolicy is not a determination, it is what the domain owner
		// PUBLISHED, and RFC 7489 fixes its set at exactly none/quarantine/
		// reject. There is no error member to defer to, and the two
		// alternatives are both stricter: synthesising `quarantine` or
		// `reject` for a policy the domain never published is the harm
		// direction for a policy field. `none` under-claims, which is the
		// safe way to be wrong about someone else's published intent.
		//
		// Reached only from inside DmarcVerdictFail, so the verdict already
		// says `fail`; this only qualifies how the owner asked failures to be
		// treated. Ruled 2026-08-18.
		return "none"
	}
}

func arcToWire(v ArcVerdict) wsrpc.ArcVerdict {
	switch v {
	case faunaCore.ArcVerdictNone:
		return wsrpc.ArcVerdict{Kind: "none"}
	case faunaCore.ArcVerdictPass:
		return wsrpc.ArcVerdict{Kind: "pass"}
	case faunaCore.ArcVerdictFail:
		return wsrpc.ArcVerdict{Kind: "fail"}
	case faunaCore.ArcVerdictPermError:
		return wsrpc.ArcVerdict{Kind: "perm_error"}
	case faunaCore.ArcVerdictTempError:
		return wsrpc.ArcVerdict{Kind: "temp_error"}
	default:
		// See dkimToWire's default arm for the ruling.
		return wsrpc.ArcVerdict{Kind: "temp_error"}
	}
}

// ── combined spam score + disposition (T3.1) ─────────────────────
//
// Pass-throughs for libs/fauna-mail/src/spam.rs's two pure functions
// (`combined_spam_score_milli`, `decide_spam_disposition`) plus a
// one-line mapper from the wire-shape SpamPolicyThresholds + AuthPolicy
// onto the fauna_mail SpamPolicy the Rust scorer consumes. Kept as thin
// wrappers because every business decision (the combined-score formula,
// the `0 = disabled` tier semantics, the DMARC short-circuit) already
// lives in shared Rust. rspamd is the sole deployment-wide content
// scorer; the legacy auth/DNSBL heuristic was removed at T3.1.

// SpamPolicy, SpamDisposition re-export the UniFFI-generated types under
// the mailfauna namespace so call sites don't have to import the
// generated fauna_mail package directly.
type (
	SpamPolicy      = faunaMail.SpamPolicy
	SpamDisposition = faunaMail.SpamDisposition
)

// Per-variant constants for the SpamDisposition enum, mirroring the
// generated `SpamDispositionAccept`/etc. Re-exported so call sites
// can switch on disposition without importing fauna_mail directly.
const (
	SpamDispositionAccept             = faunaMail.SpamDispositionAccept
	SpamDispositionAcceptToSpamFolder = faunaMail.SpamDispositionAcceptToSpamFolder
	SpamDispositionPolicyJunk         = faunaMail.SpamDispositionPolicyJunk
	SpamDispositionReject             = faunaMail.SpamDispositionReject
)

// CombinedSpamScoreMilli combines the deployment-wide rspamd score with
// the per-user Bayesian score: max(rspamd_scaled, weighted_bayesian),
// floored at 0, in milli-units of the 0–15 scale (dag-cbor forbids
// floats). Cold-start (no per-user model yet) passes
// weightedBayesianMilli=0, so the combined score is just rspamd's
// scaled score; rspamd disabled/absent ⇒ rspamdScaledMilli=0 ⇒ no
// scoring ⇒ INBOX. The Bayesian weighting lands with the deferred
// per-user model track.
func CombinedSpamScoreMilli(rspamdScaledMilli, weightedBayesianMilli int32) int32 {
	return faunaMail.CombinedSpamScoreMilli(rspamdScaledMilli, weightedBayesianMilli)
}

// ApplyUnlistedRecipientPenaltyMilli adds the deployment-wide unlisted-recipient
// penalty (points × 1000) to a combined milli-score, the canonical arithmetic
// the MTA per-recipient loop applies for a catch-all (not-on-whitelist)
// recipient (`mail-spam.md` § Unlisted-recipient penalty). Thin pass-through to
// the shared Rust helper so nest and bridge compute it identically.
func ApplyUnlistedRecipientPenaltyMilli(combinedMilli int32, penaltyPoints uint32) int32 {
	return faunaMail.ApplyUnlistedRecipientPenaltyMilli(combinedMilli, penaltyPoints)
}

// BayesianKnobs re-exports the UniFFI-generated confidence-ramp / weight
// record (bayesian_weight_milli, min_samples, full_confidence_samples)
// so the per-user-scorer call sites don't import fauna_mail directly.
type BayesianKnobs = faunaMail.BayesianKnobs

// DefaultBayesianKnobs returns the catalog defaults (weight 0.7,
// min_samples 50, full_confidence_samples 200 — mail-spam.md
// § Combined-score formula). Used as the unseeded fallback; production seeds
// the admin-effective knobs via BayesianKnobsFromSnapshot.
func DefaultBayesianKnobs() BayesianKnobs {
	return faunaMail.DefaultBayesianKnobs()
}

// BayesianKnobsFromSnapshot maps the Tier-2 per-user `mail.spam.bayesian_*`
// knobs off the wire SpamPolicyThresholds onto the BayesianKnobs the shared
// scorer consumes (weight / cold-start floor / confidence-ramp + baseline-fade
// horizon). The MDA SELECT-time scoring pass reads these instead of the
// hard-coded defaults so an admin's `put_spam_policy` override reaches the
// per-user scorer (mail-policy-config.md § Spam; mail-spam.md § Combined-score
// formula). `TrainingHistoryRetentionDays` is nest-consumed only and not mapped.
func BayesianKnobsFromSnapshot(spam wsrpc.SpamPolicyThresholds) BayesianKnobs {
	return BayesianKnobs{
		BayesianWeightMilli:   spam.BayesianWeightMilli,
		MinSamples:            spam.BayesianMinSamples,
		FullConfidenceSamples: spam.BayesianFullConfidenceSamples,
	}
}

// FoldSpamModelBaseline folds the published deployment baseline into a
// just-unwrapped per-user model at the SCORING agent — the MDA leg of the
// read-time faded prior (mail-spam.md § Cold start Path 2 step 4). The
// baseline rides the fetch_spam_model reply only for a stored (sealed)
// model (the cold-start seed is folded nest-side on read — the no-double-fold
// rule, § Encrypted-mode interaction). One shared fade implementation
// (SpamModel::fold_baseline_faded), so scores stay byte-identical across
// positions. Tolerant: empty/unparseable inputs, a model at/above the
// confidence horizon, or a 0 horizon return the model bytes unchanged.
// Score-time only — never persist a folded model.
func FoldSpamModelBaseline(modelBytes, baselineBytes []byte, fullConfidenceSamples uint32) []byte {
	return faunaMail.FoldSpamModelBaseline(modelBytes, baselineBytes, fullConfidenceSamples)
}

// WeightedBayesianMilliForModel runs the shared per-user Bayesian scorer
// over `text` against the actor's opaque model bytes (the
// `SpamModel::to_bytes` serde_json an AUTH'd agent fetches via
// fetch_spam_model and unwraps under its session MLS capability),
// returning the **already-weighted** Bayesian contribution in milli-units
// of the 0–15 scale — the second argument CombinedSpamScoreMilli takes.
// Unreadable / cold-start model bytes ⇒ 0 (no contribution). The scorer
// is byte-identical at nest / MDA / client over the shared tokenizer
// (mail-spam.md § Scoring placement).
func WeightedBayesianMilliForModel(modelBytes []byte, text string, knobs BayesianKnobs) int32 {
	return faunaMail.WeightedBayesianMilliForModel(modelBytes, text, knobs)
}

// SpamTrainingMutation re-exports the FFI mutation result (the mutated
// model to re-seal + the forward n-gram delta to seal into the training-
// history row) so callers stay off the generated package.
type SpamTrainingMutation = faunaMail.SpamTrainingMutation

// ApplySpamTraining applies one training event to (a copy of) the opened
// model bytes — the leg-2 agent-side mutation the MDA runs when the
// stored model is sealed at rest (nest-opaque). Pure — no crypto, no I/O;
// byte-identical with the nest's train_spam/train_ham and the client
// `ModelWriteOp::Train` (one shared `SpamModel`). The caller re-seals
// `NewModelBytes` to the actor's own recipient key and ships it (plus the
// sealed `DeltaJson`) via `put_spam_model`. Unreadable/empty model bytes
// train onto a fresh model, matching the nest's `load_or_create`.
func ApplySpamTraining(modelBytes []byte, text string, isSpam bool) SpamTrainingMutation {
	return faunaMail.ApplySpamTraining(modelBytes, text, isSpam)
}

// DecideSpamDisposition picks the disposition (Accept /
// AcceptToSpamFolder / PolicyJunk / Reject) given the combined spam
// score (milli-units), verdicts and policy. A DMARC p=quarantine failure
// short-circuits to PolicyJunk (the recipient's Junk) per
// `policy.HonorDmarcQuarantine`, overriding score-based routing;
// DMARC *reject* is enforced upstream by the auth-enforce gate
// (mta/auth_enforce.go, 550 5.7.1) and never reaches the scorer.
//
// The default policy is permissive auto-Junk (spam_folder=5,
// reject=0; `0 = disabled`) — high-scoring mail lands in Junk, never
// 550-rejected unless an admin sets a non-zero reject tier. The Rust impl
// debug_asserts spam_folder < reject when both are non-zero; nest
// validates at config-write time so a malformed snapshot can't reach
// here.
func DecideSpamDisposition(combinedScoreMilli int32, verdicts AuthVerdicts, policy SpamPolicy) SpamDisposition {
	return faunaMail.DecideSpamDisposition(combinedScoreMilli, verdicts, policy)
}

// SpamPolicyFromSnapshot maps the wire SpamPolicyThresholds (two
// score-tier knobs) + AuthPolicy (the DMARC quarantine honor bit) onto
// the fauna_mail SpamPolicy the scorer consumes. DMARC *reject*
// (`auth.EnforceDmarc`) is consumed by the auth-enforce gate
// (mta/auth_enforce.go), not the scorer, so it is not mapped here.
func SpamPolicyFromSnapshot(spam wsrpc.SpamPolicyThresholds, auth wsrpc.AuthPolicy) SpamPolicy {
	return SpamPolicy{
		SpamFolderThreshold:  spam.MaxScoreBeforeSpamFolder,
		RejectThreshold:      spam.MaxScoreBeforeReject,
		HonorDmarcQuarantine: auth.EnforceDmarcQuarantine,
	}
}

// SpamDispositionToWire maps the Rust SpamDisposition variant onto
// the IngestInboundMailParams.SpamDisposition wire string set. The
// Reject variant returns ("", true) — the bridge 5xx's at SMTP DATA
// instead of calling ingest, so reject never reaches the wire; the
// boolean signals the caller to short-circuit. The other three
// variants return their canonical snake_case token + false.
func SpamDispositionToWire(d SpamDisposition) (wire string, isReject bool) {
	switch d {
	case SpamDispositionAccept:
		return "accept", false
	case SpamDispositionAcceptToSpamFolder:
		return "accept_to_spam_folder", false
	case SpamDispositionPolicyJunk:
		return "policy_junk", false
	case SpamDispositionReject:
		return "", true
	default:
		// Defensive: UniFFI regen could add a variant before this
		// switch is updated. Surface as ("", true) so the bridge 5xx's
		// rather than ingest under an unknown disposition.
		return "", true
	}
}

// ── content scan: ClamAV verdict + rspamd score (T1.4) ───────────
//
// Pass-throughs for libs/fauna-mail/src/scan/mod.rs's three pure
// functions (`clamd_parse_reply`, `rspamd_parse_reply`,
// `decide_scan_action`) plus mappers from the UniFFI verdict types onto
// the wsrpc cbor wire shapes (`ClamavVerdictToWire`, `RspamdScoreToWire`).
// The network I/O (dial clamd, POST rspamd) is Go-side in mta/scan_gate.go,
// mirroring the spam gate; this package only frames-free-parses the replies
// and maps the pure-fn outputs onto the wire — every business decision lives
// in shared Rust.

// ClamavVerdict, RspamdScore, RspamdRuleContribution, ScanPolicy, ScanAction,
// ClamavAction re-export the UniFFI-generated types under the mailfauna
// namespace so call sites don't import the generated packages.
//
// ⚠ The split between the two generated packages is not arbitrary and is not
// ours to choose: UniFFI attributes a type to the crate that DEFINES it. The
// three scan RESULT types moved to `fauna_core::mail_scan` on 2026-08-18,
// so they arrive from `fauna_core`; the POLICY and ACTION types are still
// declared in `fauna-mail` and arrive from `fauna_mail`. These aliases are
// exactly why that regen was a one-file change for every call site.
type (
	ClamavVerdict          = faunaCore.ClamavVerdict
	RspamdScore            = faunaCore.RspamdScore
	RspamdRuleContribution = faunaCore.RspamdRuleContribution
	ScanPolicy             = faunaMail.ScanPolicy
	ScanAction             = faunaMail.ScanAction
	ClamavAction           = faunaMail.ClamavAction
	ScanError              = faunaMail.ScanError
)

// Concrete ClamavVerdict / ScanAction variant structs re-exported under the
// mailfauna namespace so call sites (mta/scan_gate.go) can construct + switch
// on them without importing the generated packages directly.
type (
	ClamavVerdictClean            = faunaCore.ClamavVerdictClean
	ClamavVerdictInfected         = faunaCore.ClamavVerdictInfected
	ClamavVerdictError            = faunaCore.ClamavVerdictError
	ClamavVerdictBypassedOversize = faunaCore.ClamavVerdictBypassedOversize
	ClamavVerdictNotScanned       = faunaCore.ClamavVerdictNotScanned

	ScanActionDeliver       = faunaMail.ScanActionDeliver
	ScanActionRejectMalware = faunaMail.ScanActionRejectMalware
	ScanActionJunk          = faunaMail.ScanActionJunk
	ScanActionTag           = faunaMail.ScanActionTag
	ScanActionTempfail      = faunaMail.ScanActionTempfail
)

// Per-variant constants for the ClamavAction enum, re-exported so call
// sites can build a ScanPolicy without importing fauna_mail directly.
const (
	ClamavActionReject = faunaMail.ClamavActionReject
	ClamavActionJunk   = faunaMail.ClamavActionJunk
	ClamavActionTag    = faunaMail.ClamavActionTag
)

// ClamdParseReply parses a clamd zINSTREAM reply into a verdict (pure).
// Anything unrecognized — including an `ERROR` line — becomes
// ClamavVerdictError (never silently Clean; that was the legacy clamd.rs
// fail-open bug). The Go socket writer does the INSTREAM framing inline.
func ClamdParseReply(reply string) ClamavVerdict {
	return faunaMail.ClamdParseReply(reply)
}

// RspamdParseReply parses an rspamd /checkv2 JSON response, applying
// scalingPerMille (default 500 = 0.5). A malformed 200 is an error (the
// caller tempfails — never allow-without-score).
func RspamdParseReply(json string, scalingPerMille uint16) (RspamdScore, error) {
	return faunaMail.RspamdParseReply(json, scalingPerMille)
}

// DecideScanAction maps the ClamAV verdict + policy onto the delivery action
// (Deliver / RejectMalware / Junk / Tag / Tempfail). Centralizes the
// never-allow-without-scan rule: an Error verdict becomes a Tempfail.
func DecideScanAction(clamav ClamavVerdict, policy ScanPolicy) ScanAction {
	return faunaMail.DecideScanAction(clamav, policy)
}

// ClamavVerdictToWire maps the UniFFI ClamavVerdict enum onto the wsrpc cbor
// wire shape (adjacently-tagged snake_case), mirroring AuthVerdictsToWire.
func ClamavVerdictToWire(v ClamavVerdict) wsrpc.ClamavVerdict {
	switch d := v.(type) {
	case faunaCore.ClamavVerdictClean:
		return wsrpc.ClamavVerdict{Kind: "clean"}
	case faunaCore.ClamavVerdictInfected:
		return wsrpc.ClamavVerdict{
			Kind: "infected",
			Data: &wsrpc.ClamavVerdictData{Signature: d.Signature},
		}
	case faunaCore.ClamavVerdictError:
		return wsrpc.ClamavVerdict{
			Kind: "error",
			Data: &wsrpc.ClamavVerdictData{Detail: d.Detail},
		}
	case faunaCore.ClamavVerdictBypassedOversize:
		return wsrpc.ClamavVerdict{Kind: "bypassed_oversize"}
	case faunaCore.ClamavVerdictNotScanned:
		return wsrpc.ClamavVerdict{Kind: "not_scanned"}
	default:
		// Defensive: a UniFFI regen could add a variant before this switch is
		// updated. The old comment called this branch "unreachable for the four
		// variants above" and landed on "clean" to keep the wire valid — but
		// unreachability is exactly what a new variant removes, and on a MALWARE
		// verdict `clean` is the most permissive value there is. "No signature
		// matched" is a claim, and it is not one we can make about a variant we
		// could not map.
		//
		// `error` is the truthful and fail-safe landing point, and this file
		// already says so at the variant itself: clamd errors "must NOT be
		// treated as clean". Same ruling as the auth verdicts' default arms
		// one family over; same containment, too — the delivery
		// DECISION is made Go-side by DecideScanAction on the UniFFI value
		// before it ever reaches this mapper, so what the fallback reaches is
		// nest's recorded scan result (bridge_routing_handlers.rs →
		// message_scan_results), not an accept/reject.
		return wsrpc.ClamavVerdict{
			Kind: "error",
			Data: &wsrpc.ClamavVerdictData{
				Detail: "unmappable ClamavVerdict variant — the Go wire mapper is behind the Rust enum",
			},
		}
	}
}

// RspamdScoreToWire maps the UniFFI RspamdScore onto the wsrpc cbor wire
// shape (field-for-field; all milli-ints).
func RspamdScoreToWire(s RspamdScore) wsrpc.RspamdScore {
	breakdown := make([]wsrpc.RspamdRuleContribution, len(s.Breakdown))
	for i, c := range s.Breakdown {
		breakdown[i] = wsrpc.RspamdRuleContribution{
			Rule:       c.Rule,
			ScoreMilli: c.ScoreMilli,
		}
	}
	return wsrpc.RspamdScore{
		RawMilli:     s.RawMilli,
		ScaledMilli:  s.ScaledMilli,
		FlaggedRules: s.FlaggedRules,
		Breakdown:    breakdown,
	}
}

// ── the scoring-metadata bus rows (the contract phase) ───────────
//
// The perimeter mints its own bus rows: the ONE per-kind verdict → row
// mapping is `fauna_core::scoring::perimeter_mail_score_rows` (shared Rust,
// over UniFFI — never a Go re-implementation), and the nest stores what it is
// sent and derives nothing (content-scoring.md § The scoring-metadata bus).

// ScoreEntry re-exports the UniFFI-generated bus row (mirrors
// `fauna_core::scoring::ScoreEntry`) so call sites don't import the
// generated package; `wsrpc.ScoreEntry` is its cbor wire twin.
type ScoreEntry = faunaCore.ScoreEntry

// PerimeterMailScoreRows runs the shared mapping over the verdicts this
// delivery's per-kind fields carry and returns the rows in the wsrpc wire
// shape, ready for IngestInboundMailParams.Scores. Called once per recipient
// at the ingest edge — the spam score is per-recipient once the
// unlisted-recipient penalty applies. spamScoreMilli is the combined score in
// milli-points (CombinedSpamScoreMilli, penalty applied), never the floored
// 0–15 points the per-kind SpamScore field carries; rspamd is nil when rspamd
// did not run. A scorer that did not run emits no row.
func PerimeterMailScoreRows(spamScoreMilli int32, clamav ClamavVerdict, rspamd *RspamdScore, verdicts AuthVerdicts) []wsrpc.ScoreEntry {
	return ScoreEntriesToWire(faunaCore.PerimeterMailScoreRows(spamScoreMilli, clamav, rspamd, verdicts))
}

// ScoreEntriesToWire maps UniFFI bus rows onto the wsrpc cbor wire shape
// (field-for-field).
func ScoreEntriesToWire(rows []ScoreEntry) []wsrpc.ScoreEntry {
	wire := make([]wsrpc.ScoreEntry, len(rows))
	for i, r := range rows {
		wire[i] = wsrpc.ScoreEntry{
			Factor:        r.Factor,
			Score:         r.Score,
			Tier:          r.Tier,
			ScorerVersion: r.ScorerVersion,
		}
	}
	return wire
}

// ── tokenize (Phase C.8) ──────────────────────────────────────────
//
// Pass-through for libs/fauna-mail/src/tokenizer.rs::tokenize. The
// CanonicalTokenSet shape (sorted-deduped Tokens + length-prefixed
// CanonicalBytes) is the deterministic encrypted-search input —
// every platform produces byte-identical output for a given input.
// The bridge tokenizes `Subject + " " + BodyText` on inbound (per
// the C.8 wiring in Session.Data) and encrypts CanonicalBytes to
// the recipient's index key for IngestInboundMail's
// encrypted_index_hint.

// CanonicalTokenSet re-exports the UniFFI-generated type under the
// mailfauna namespace; same Tokens / CanonicalBytes fields as the
// fauna_mail::CanonicalTokenSet Rust shape.
type CanonicalTokenSet = faunaMail.CanonicalTokenSet

// Tokenize runs the shared Rust tokenizer (NFKC normalize → word-
// segment per UAX#29 → Unicode case-fold → filter <2 chars / no
// alphanumerics → sort + dedupe → length-prefixed canonical bytes).
// Pure function; no I/O.
func Tokenize(input string) CanonicalTokenSet {
	return faunaMail.Tokenize(input)
}

// ReportHash runs the shared Rust canonical report-hash — the cross-user,
// cross-nest content-equality key for distributed report sharing
// (report-sharing.md § Content identity): blake3 over a domain-separated
// canonicalization (NFC → lowercase → collapse whitespace → trim) of
// `Subject` + "\n" + `BodyText` — the same parsed fields Tokenize consumes.
// Computed once per message at DATA, pre-seal; identical for every recipient
// of the same message on every nest. Pure function; no I/O.
func ReportHash(subject, bodyText string) []byte {
	return faunaMail.ReportHash(subject, bodyText)
}

// MailDedupKeyPair is the (dedup key, envelope key) pair one message is
// recorded under — the shared Rust record, re-exported so producers name it
// without importing the binding.
type MailDedupKeyPair = faunaMail.MailDedupKeyPair

// MailDedupKeys runs the shared Rust canonical per-actor mail dedup keys
// (mailbox-migration.md § Key format): DedupKey is the normalized Message-ID
// when the message carries one, else a SHA-256 over the canonical envelope +
// body; EnvelopeKey is that envelope SHA-256, always. The nest skips an import
// on a DedupKey hit only when the EnvelopeKeys agree (§ The envelope key
// confirms a Message-ID hit), so a producer sends both, never one.
//
// Its only input is the whole raw RFC 5322 message, deliberately: the MDA (at
// APPEND), the MTA (at DATA, pre-seal), the user's client (at import) and the
// nest (at its own in-domain delivery) all compute these keys, and they are
// worthless unless all four agree byte for byte. The parse therefore lives inside the shared function — never assemble
// the envelope Go-side. Pure function; no I/O; never fails (a garbage message
// still yields stable keys, so delivery is never dropped over a dedup detail).
func MailDedupKeys(rawMessage []byte) MailDedupKeyPair {
	return faunaMail.MailDedupKeys(rawMessage)
}

// NewFaunaMsgidLocal mints the 32-lowercase-hex Message-ID local part over
// the shared self-describing Fauna mint (`fauna_mail::msgid`): 96 random
// bits plus a 32-bit provenance tag the nest's guardian mail gate *verifies*
// before seeding its DSN correlation (`family-safety.md` § The mail gate).
// One construction with the native compose path's
// `fauna_conversations::rfc5322::new_message_id` — never reimplement it
// Go-side: a plain random token wears the right shape but fails the tag, so
// it would silently stop seeding and every bounce of that mail would hold.
func NewFaunaMsgidLocal() string {
	return faunaMail.NewFaunaMsgidLocal()
}

// ── seal_to_recipient (Phase C.9) ────────────────────────────────
//
// Thin pass-through over the UniFFI `SealToRecipient` (HPKE-Seal). The
// MTA bridge's Session.Data calls this twice per inbound message:
// once with `raw` (RFC 5322 bytes) targeted at the recipient's MLS
// pubkey, producing `encrypted_body`; once with `indexHint.CanonicalBytes`
// targeted at the recipient's index pubkey, producing
// `encrypted_index_hint`. Both ciphertexts ride on
// `IngestInboundMailParams` to `fauna.bridges.ingest_inbound_mail`.
//
// `recipientPubkey` must be exactly 32 bytes; mismatched length surfaces
// as a wrapped `FfiError::General`.

// EncryptToRecipient HPKE-seals `plaintext` to `recipientPubkey` and
// returns the canonical DAG-CBOR `MailRecordEnvelope` bytes ready for
// the wire. The MTA bridge encrypts the *raw on-the-wire RFC 5322
// bytes* (not the re-serialised parsed form) so future replay
// verification can succeed (DKIM is cryptographically bound to the
// original canonical bytes).
func EncryptToRecipient(plaintext, recipientPubkey []byte) ([]byte, error) {
	return faunaFfi.SealToRecipient(plaintext, recipientPubkey)
}

// mlkem768EncapsKeyLen is the published ML-KEM-768 encapsulation-key length
// (fauna_pq_kem::MLKEM768_ENCAPS_KEY_LEN). The X-Wing public key is this ek
// (1184 B) ∥ the X25519 mls_pubkey (32 B) = 1216 B.
const mlkem768EncapsKeyLen = 1184

// EncryptToRecipientHybrid is the single seal-suite decision for every MTA/MDA
// mail/calendar seal — the message/event *body* AND the companion index hint
// (PQ-6 brought the hint under the same hybrid surface; priority #2). It seals
// `plaintext` to `recipientPubkey`, selecting the post-quantum X-Wing suite iff
// the caller passes an ML-KEM ek (`mlkemEk`, 1184 B) that pairs with
// `recipientPubkey` — the ek's presence IS the gate (`architecture/security/post-quantum.md`
// § Capability negotiation; no capability token). A recipient's own seal key
// always carries one: the provision request requires both halves and the
// bridge's fetch (wsrpc.FetchRecipientMLSPubkeyHybrid) refuses a key missing
// either, so no body seal arrives here without an ek.
// `mlkemEk` nil/empty → the classical seal. Its one producer is the index
// hint sealed to a key that is not the recipient's MLS pubkey
// ([IndexHintMlkemEk] returns nil there: no ek pairs with that key). The
// in-image bridge's X-Wing capability is
// build-identical to the nest's, so a present ek always means this build can seal
// to it. The body passes the recipient's MLS pubkey + its ek; the index hint
// passes its index pubkey + an ek from [IndexHintMlkemEk] (the ek only when the
// index key is the MLS-pubkey fallback, so the ek pairs with the sealed-to key).
//
// PQ-4(b) — degrade-to-classical on X-Wing seal *error*, never fail closed:
// the published ek is length-gated (1184 B) but only validated at encaps (FIPS
// 203 input validation is deferred), so a right-length-but-invalid ek makes
// SealToRecipientXwing error. Falling back to the classical seal avoids a
// self-DoS of the recipient's inbound mail (publish is authz-bound to self, so
// there is no cross-user DoS). The fallback is warn-logged so a *systemic*
// "PQ silently off" regression stays observable rather than degrading silently
// fleet-wide.
func EncryptToRecipientHybrid(plaintext, recipientPubkey, mlkemEk []byte) ([]byte, error) {
	if len(mlkemEk) == mlkem768EncapsKeyLen {
		xwingPubkey := make([]byte, 0, mlkem768EncapsKeyLen+len(recipientPubkey))
		xwingPubkey = append(xwingPubkey, mlkemEk...)
		xwingPubkey = append(xwingPubkey, recipientPubkey...)
		sealed, err := faunaFfi.SealToRecipientXwing(plaintext, xwingPubkey)
		if err == nil {
			return sealed, nil
		}
		slog.Warn("X-Wing seal failed; degrading to classical X25519 seal (PQ-4b)",
			"error", err)
	}
	return faunaFfi.SealToRecipient(plaintext, recipientPubkey)
}

// IndexHintMlkemEk returns the ML-KEM ek to use for the X-Wing index-hint seal:
// the recipient's body ek (`mlkemEk`) when the index key is the MLS-pubkey
// fallback (`indexPubkey == mlsPubkey`), so the ek pairs with the key the hint
// is sealed to; otherwise nil (classical). PQ-6 seals the hint hybrid on the
// recipient's standing key while the dedicated index key is unprovisioned.
// **Phase E:** when `provision_recipient_index_key` ships a *dedicated* index
// X25519 key, it MUST also publish a sibling index ML-KEM ek and re-point this
// gate to it — otherwise a dedicated index key (≠ mlsPubkey) seals the hint
// classical here, which is the correct safe default but must not become the
// permanent state (`content-index.md` Plan 5b / storage-mode-redesign Phase E2).
func IndexHintMlkemEk(indexPubkey, mlsPubkey, mlkemEk []byte) []byte {
	if bytes.Equal(indexPubkey, mlsPubkey) {
		return mlkemEk
	}
	return nil
}

// OpenMailRecordWithKey HPKE-opens a `MailRecordEnvelope` with a RAW recipient
// content key — the capability-holder counterpart of OpenMailRecord, for the
// BACKGROUND case with no AUTH'd session (and hence no MLS snapshot): the
// re-score drain opens a sealed record with the minimal derived key a
// user-minted `content.read{mail}` grant carried (a CapabilityScopeKey.Key from
// UnsealCapabilityGrant). The key self-describes its reach by length: 32 bytes
// (x25519 secret) opens classical records only; 32+2400 bytes
// (x25519∥mlkem_dk) opens both classical and hybrid (X-Wing) records. A wrong
// or insufficient key fails AEAD-closed — never a mis-decrypt.
func OpenMailRecordWithKey(envelope, key []byte) ([]byte, error) {
	pt, err := faunaFfi.OpenMailRecordWithKey(envelope, key)
	if err != nil {
		return nil, fmt.Errorf("OpenMailRecordWithKey: %w", err)
	}
	return pt, nil
}

// MailRecordOpener is the per-connection F2 opener (Phase-3 S2): the AUTH'd
// session's MLS-snapshot plaintext is parsed ONCE at construction (the leaf
// keypairs stay Zeroizing Rust-side), and each per-record Open pays one
// envelope decode + HPKE open — not the per-open snapshot marshal + re-parse
// MlsCapability.OpenMailRecord paid (~18 ms/FETCH at 1k messages). Construct
// at AUTH next to the capability; Zeroize on session close alongside it.
type MailRecordOpener = faunaFfi.MailRecordOpener

// NewMailRecordOpener parses `snapshotPlaintext` (canonical DAG-CBOR
// MlsSnapshotPlaintext, already AEAD-unwrapped under MSEK at AUTH) into a
// per-connection opener. Errors on malformed snapshots or an empty leaf-
// keypair list — a session with no snapshot on file passes nil around and
// surfaces the missing-snapshot error at first sealed-record open, exactly
// as before.
//
// This opener carries no MSEK, so its epoch-aware sibling method (OpenMail)
// falls straight through to the standing/grace chain — the right shape for
// CalDAV/CardDAV sessions, whose content is never epoch-sealed (
// site-scoping precedent). The IMAP MDA's genuine mail-new-ingest opener
// uses NewEpochAwareMailRecordOpener instead.
func NewMailRecordOpener(snapshotPlaintext []byte) (*MailRecordOpener, error) {
	opener, err := faunaFfi.NewMailRecordOpener(snapshotPlaintext)
	if err != nil {
		return nil, fmt.Errorf("NewMailRecordOpener: %w", err)
	}
	return opener, nil
}

// NewEpochAwareMailRecordOpener builds the IMAP MDA's per-connection opener
// from `cap` (the AUTH'd session's MlsCapability, still holding its
// unwrapped MSEK) plus the already-decrypted snapshot plaintext —
// content-sealing-epochs design § 4's MSEK-holder opener chain. Unlike
// NewMailRecordOpener, the returned opener's OpenMail additionally tries
// the record's candidate epoch keys (derived on demand from the
// capability's MSEK, which never crosses the FFI boundary) before falling
// through to the standing/grace chain.
func NewEpochAwareMailRecordOpener(cap *MLSCapability, snapshotPlaintext []byte) (*MailRecordOpener, error) {
	opener, err := cap.NewEpochAwareMailRecordOpener(snapshotPlaintext)
	if err != nil {
		return nil, fmt.Errorf("NewEpochAwareMailRecordOpener: %w", err)
	}
	return opener, nil
}

// IsSealedMailRecord reports whether `stored` is a sealed MailRecordEnvelope
// (strict canonical DAG-CBOR, kind "mail-record") — as opposed to raw
// plaintext (RFC 5322 / iCalendar / canonical index-hint token bytes). Every
// mail-plane record rests sealed, so no serve path branches on this; tests
// use it to prove a payload handed to the nest is sealed.
func IsSealedMailRecord(stored []byte) bool {
	return faunaFfi.IsSealedMailRecord(stored)
}

// MailSealingEpochOf returns the wall-clock mail content-sealing epoch index
// for unixSecs — floor(unixSecs / MAIL_SEALING_EPOCH_SECS) (content-sealing-
// epochs design § 1). The re-score drain uses it to classify which epoch a
// record was sealed under from its SEAL INSTANT
// (FetchedCiphertext.SealEpochBasisUnix() — stored_at, not InternalDate; the two
// diverge for imported mail), feeding
// capability.GrantSet.KeyForMailEpoch's candidate-chain opener selection
// (design § 4) — the constant itself stays in shared Rust so Go never duplicates it.
func MailSealingEpochOf(unixSecs uint64) uint64 {
	return faunaFfi.MailSealingEpochOf(unixSecs)
}

// ── spam-baseline holder aggregation ─────────────────────────────
//
// The holder-side compute of the keyless `content.read{spam-model}` shape
// (mail-spam.md § Encrypted-mode interaction, ratified 2026-07-13). The whole
// unseal→decode→merge loop lives in shared Rust so the Go spam-baseline drain
// worker is a thin transport shim (priority #2): it pulls the run's sealed-copy
// worklist, calls AggregateSpamModelCopies with the holder's OWN service-user
// key halves, and submits the merged half back. No key of any *user's* is
// involved.

// SpamBaselineCopyInput, SpamBaselineAggregate re-export the UniFFI-generated
// records under the mailfauna namespace so the mda drain doesn't import
// fauna_ffi directly.
type (
	SpamBaselineCopyInput = faunaFfi.SpamBaselineCopyInput
	SpamBaselineAggregate = faunaFfi.SpamBaselineAggregate
)

// AggregateSpamModelCopies opens + additively merges a publish-run worklist of
// sealed-to-holder spam-model copies with the holder's OWN service-user key
// halves — the same halves the re-score drain feeds `unseal_capability_grant`.
// `holderMlkemDk` is the optional 2400-byte ML-KEM-768 decapsulation key: an
// empty/nil slice means a classical-only holder (mapped to the FFI's None), and
// AggregateSpamModelCopies errors if a non-empty dk is not exactly 2400 bytes.
// Per-copy failures (wrong suite half, tampered bytes, owner mismatch,
// undecodable plaintext) count `unreadable` and never fail the call — one bad
// copy must not block the deployment's baseline.
func AggregateSpamModelCopies(copies []SpamBaselineCopyInput, holderX25519Secret, holderMlkemDk []byte) (SpamBaselineAggregate, error) {
	var dk *[]byte
	if len(holderMlkemDk) > 0 {
		dk = &holderMlkemDk
	}
	agg, err := faunaFfi.AggregateSpamModelCopies(copies, holderX25519Secret, dk)
	if err != nil {
		return SpamBaselineAggregate{}, fmt.Errorf("AggregateSpamModelCopies: %w", err)
	}
	return agg, nil
}

// SealSpamModelCopy seals a contributor's post-mutation plaintext spam model
// to the deployment's aggregation holder — the seal-side twin of
// AggregateSpamModelCopies, producing the `SpamModelCopyBlob` a `put_spam_model`
// write attaches as `holder_copy` (piece b4: the MDA `\Junk`-train re-seal
// path, mail-spam.md § Encrypted-mode interaction). `holderMlkemEk` is the
// optional 1184-byte ML-KEM-768 encapsulation key: an empty/nil slice selects
// the classical X25519 seal (mapped to the FFI's None); present selects the
// post-quantum X-Wing seal. The copy is owner-bound (`ownerActorID` folds into
// the AEAD AAD), so a re-attributed copy fails AEAD-open at the holder.
func SealSpamModelCopy(modelBytes, ownerActorID, holderX25519Pubkey, holderMlkemEk []byte) ([]byte, error) {
	var ek *[]byte
	if len(holderMlkemEk) > 0 {
		ek = &holderMlkemEk
	}
	copyBytes, err := faunaFfi.SealSpamModelCopy(modelBytes, ownerActorID, holderX25519Pubkey, ek)
	if err != nil {
		return nil, fmt.Errorf("SealSpamModelCopy: %w", err)
	}
	return copyBytes, nil
}

// RecordOpener is the per-record open seam OpenStoredRecord dispatches
// through — satisfied by *MailRecordOpener in production and by test stubs.
// Callers holding a possibly-nil *MailRecordOpener must convert through an
// explicit nil check (`var o RecordOpener; if concrete != nil { o = concrete }`)
// so the nil-interface guard below keeps working (the classic Go typed-nil
// trap; the imap fetch seam already follows this pattern).
type RecordOpener interface {
	Open(envelope []byte) ([]byte, error)
}

// OpenStoredRecord opens a stored content record through the ONE uniform
// serve path (Phase-3 D1): every mail-plane record rests sealed (the nest
// verifies the seal shape on every write), so it HPKE-opens via the session's
// per-connection opener. An unsealed payload is refused by the opener's
// envelope decode — never served verbatim — and a nil opener (no MLS snapshot
// on file) refuses every record.
func OpenStoredRecord(opener RecordOpener, stored []byte) ([]byte, error) {
	if opener == nil {
		return nil, fmt.Errorf("OpenStoredRecord: no MLS snapshot on file")
	}
	pt, err := opener.Open(stored)
	if err != nil {
		return nil, fmt.Errorf("OpenStoredRecord: %w", err)
	}
	return pt, nil
}

// epochAwareRecordOpener is OpenStoredRecordAt's optional upgrade seam —
// satisfied by *MailRecordOpener when it was built via
// NewEpochAwareMailRecordOpener (content-sealing-epochs design § 4). A test
// stub implementing only RecordOpener's plain Open falls through to
// OpenStoredRecord's exact behavior in OpenStoredRecordAt below.
type epochAwareRecordOpener interface {
	OpenMail(envelope []byte, recordUnixSecs uint64) ([]byte, error)
}

// OpenStoredRecordAt is OpenStoredRecord's epoch-aware sibling: the same
// refusals, but on an epoch-aware opener it calls OpenMail with the record's
// own seal instant (`recordUnixSecs`, the Go MDA's
// `FetchedCiphertext.SealEpochBasisUnix()`) so the MSEK-holder chain can try the
// record's candidate epoch keys (design § 4) before falling through to the
// standing/grace chain. A non-epoch-aware opener (built via
// NewMailRecordOpener, or a test stub) behaves exactly like
// OpenStoredRecord — `recordUnixSecs` is then unused.
//
// Confined to the genuine mail-new-ingest READ path — IMAP FETCH's body
// open (the literal success bar this construction path was built for).
// STORE's \Junk re-train, MOVE/COPY, body-search index hints, and the
// spam-model re-seal read still call OpenStoredRecord unchanged: each needs
// its own investigation into whether its content can ever be epoch-sealed
// and where its record timestamp comes from before it's safe to switch —
// captured as a follow-up, not
// silently assumed safe here.
func OpenStoredRecordAt(opener RecordOpener, stored []byte, recordUnixSecs uint64) ([]byte, error) {
	if opener == nil {
		return nil, fmt.Errorf("OpenStoredRecordAt: no MLS snapshot on file")
	}
	if eo, ok := opener.(epochAwareRecordOpener); ok {
		pt, err := eo.OpenMail(stored, recordUnixSecs)
		if err != nil {
			return nil, fmt.Errorf("OpenStoredRecordAt: %w", err)
		}
		return pt, nil
	}
	pt, err := opener.Open(stored)
	if err != nil {
		return nil, fmt.Errorf("OpenStoredRecordAt: %w", err)
	}
	return pt, nil
}

// SelectSigningDomain picks the DKIM `d=` signing domain for an outbound
// message from its From: header domain, per RFC 6376 §3.6 +
// mail-multidomain.md § Signing-key selection at outbound time. ONE shared
// selection rule across nest / clients / bridge (priority #2) — the Go MTA
// MUST NOT re-implement it. Returns (signingDomain, true) on a local exact or
// closest-parent-subdomain match, or ("", false) when fromDomain is not local
// (and not a subdomain of any local domain) — the caller then rejects
// submission with `550 5.7.7 From: domain not local`. The bridge only decides
// locality with it; the nest holds the keys and signs.
func SelectSigningDomain(fromDomain string, localDomains []string) (string, bool) {
	if d := faunaFfi.SelectSigningDomain(fromDomain, localDomains); d != nil {
		return *d, true
	}
	return "", false
}

// StripReceivedHeaders returns a copy of rawMessage with every `Received:`
// header removed (whole logical header, continuation lines included), the
// body copied verbatim. Outbound mail must not leak the submitter's IP or
// our internal hostnames (smtp-server.md § Outbound delivery); the
// submission listener calls this before the enqueue so the signature the
// nest adds at the outbound hand-out covers the stripped form.
//
// One impl lives in shared Rust at
// `libs/fauna-mail/src/outbound/received_strip.rs` and is crossed via the
// UniFFI `strip_received_headers` export. Case-insensitive field-name
// match, RFC 5322 §2.2.3 continuation-aware, substring-safe (`X-Received-By`
// / `Received-SPF` survive).
func StripReceivedHeaders(rawMessage []byte) []byte {
	return faunaFfi.StripReceivedHeaders(rawMessage)
}

// StripFaunaHeaders returns a copy of rawMessage with every reserved
// `X-Fauna-*` delivery-stamp header removed (`X-Fauna-Scan-*` /
// `X-Fauna-Address-*`) EXCEPT the inbound-consumed `X-Fauna-Forwarded-By`
// forward-loop trace (mail-forwarding.md § Loop detection — never stripped).
// The inbound MTA calls this at the DATA stage, before ParseRFC5322 + the
// genuine stamp prepend, so a sender-forged trust-stamp header reaches neither
// the per-recipient filter context nor the sealed copy a client reads
// (smtp-server.md § Architectural rules). Inbound sibling of
// StripReceivedHeaders.
//
// One impl lives in shared Rust at `libs/fauna-mail/src/received_header.rs`
// (`strip_fauna_headers`) and is crossed via the UniFFI `strip_fauna_headers`
// export. Case-insensitive `X-Fauna-` field-name prefix match, RFC 5322 §2.2.3
// continuation-aware, substring-safe (`X-Not-Fauna` survives).
func StripFaunaHeaders(rawMessage []byte) []byte {
	return faunaFfi.StripFaunaHeaders(rawMessage)
}

// ReadSpamThresholdStamp returns the `X-Fauna-Spam-Threshold` delivery-stamp on
// a decrypted message — that message's own `spam_folder` tier in whole points,
// folded nest-side at delivery from per-alias > per-account > admin default
// (mail-aliases.md § Spam-threshold override). A nil result means the message
// carries no stamp (delivered before the stamp shipped), and the caller falls
// back to its session policy.
//
// One impl lives in shared Rust at `libs/fauna-mail/src/aliases/mod.rs`
// (`read_spam_threshold_stamp`, beside `HEADER_SPAM_THRESHOLD` — the constant
// the nest resolver stamps with) and is crossed via the UniFFI
// `read_spam_threshold_stamp` export. Deliberately NOT a Go-side header scan
// against a Go-side copy of the field name: that shape is exactly how a
// cross-language contract goes silently green on a stale literal.
func ReadSpamThresholdStamp(rawMessage []byte) *uint32 {
	return faunaFfi.ReadSpamThresholdStamp(rawMessage)
}

// StampFilterAllow carries a fired `Allow` filter rule past delivery: it
// returns the recipient's stamped copy with every `X-Fauna-Spam-Threshold`
// header replaced by ONE leading `X-Fauna-Spam-Threshold: 0` — the disabled
// tier — so neither post-delivery scorer (the MDA's SELECT-time pass, the
// shared on-device INBOX scorer) re-files it to Junk (email-filters.md
// § Multi-action composition). It REPLACES nest's RCPT-time stamp rather than
// sitting beside it because ReadSpamThresholdStamp takes the first match.
//
// One impl lives in shared Rust beside the reader
// (`fauna_mail::aliases::stamp_filter_allow`), crossed via the UniFFI
// `stamp_filter_allow` export, for the reason ReadSpamThresholdStamp gives.
func StampFilterAllow(rawMessage []byte) []byte {
	return faunaFfi.StampFilterAllow(rawMessage)
}

// ReceivedHeaderOpts is the per-transaction context for BuildReceivedHeader.
// Re-exported from the shared Rust `fauna_mail::received_header` so the MTA call
// site uses `mailfauna.ReceivedHeaderOpts` without naming the generated package.
type ReceivedHeaderOpts = faunaMail.ReceivedHeaderOpts

// BuildReceivedHeader returns the single canonical `Received:` trace-header field
// the inbound MTA prepends to a message before sealing + ingest_inbound_mail
// (smtp-server.md § Architectural rules). The returned string is one CRLF-folded
// header field with no trailing CRLF, ready to hand to prependHeaders as one
// element. Sender-controlled fields (HELO, client IP) are sanitized inside the
// shared Rust impl — any CR / LF / non-printable byte becomes `unknown`, so a
// hostile EHLO can't forge a second header line.
//
// One impl lives in shared Rust at `libs/fauna-mail/src/received_header.rs`
// (sibling of the outbound `StripReceivedHeaders`); crossed via UniFFI. The
// caller supplies the I/O half — the clock (nowUnixSecs) and a fresh queue id
// from NewQueueID — so the Rust core stays a pure function of its inputs.
func BuildReceivedHeader(nowUnixSecs int64, opts ReceivedHeaderOpts) string {
	return faunaMail.BuildReceivedHeader(nowUnixSecs, opts)
}

// BuildAuthenticatedSenderStamp returns the full `X-Fauna-Authenticated-Sender:
// <addr-spec>` header line (no trailing CRLF, ready for prependHeaders) naming
// the sender address a filing door itself authenticated, lower-cased — or ""
// when addr cannot be stamped as one printable addr-spec line (empty, no single
// `@`, a display name / angle bracket / CR / LF / non-printable byte, over 254
// bytes). "" means prepend nothing: an unstamped copy reads downstream as
// unauthenticated, never as vouched-for (smtp-server.md § Architectural rules →
// The `X-Fauna-*` namespace, the authenticated-sender stamp).
//
// The MX door stamps the `From:` addr-spec only under a DMARC pass; the
// submission door stamps the envelope sender it validated as owned by the
// authenticated actor. One impl lives in shared Rust at
// `libs/fauna-mail/src/sender_auth.rs` (`build_authenticated_sender_stamp`),
// crossed via UniFFI.
func BuildAuthenticatedSenderStamp(addr string) string {
	return faunaMail.BuildAuthenticatedSenderStamp(addr)
}

// MaxInlineRawMessageBytes is the largest raw RFC 5322 message the perimeter
// accepts while mail bodies ride WS-RPC inline — the interim product ceiling
// the pre-parse LimitReader clamp enforces with 552 5.3.4
// (smtp-server.md § Message size limits).
func MaxInlineRawMessageBytes() uint32 {
	return faunaMail.MaxInlineRawMessageBytes()
}

// InlineMailRequestBudgetBytes is the byte budget for the sealed body +
// sealed index hint of one mail-carrying WS-RPC request; two ciphertexts
// over it cannot cross the 2 MiB frame, so a body over it rides the bulk-byte
// plane by reference instead (see MailBodyNeedsReference).
func InlineMailRequestBudgetBytes() uint32 {
	return faunaMail.InlineMailRequestBudgetBytes()
}

// EffectiveMaxRawMessageBytes is the product ceiling `max_message_bytes` (the
// admin knob) alone, since ceiling retirement (2026-07-18). The single source
// for both SMTP `Data` paths and the EHLO SIZE advertisement, so enforcement can
// never drift. A 0 knob does NOT mean uncapped — it falls back to the shipped
// product default (go-smtp treats a 0 MaxMessageBytes as unlimited).
func EffectiveMaxRawMessageBytes(maxMessageBytes uint32) uint32 {
	return faunaMail.EffectiveMaxRawMessageBytes(maxMessageBytes)
}

// MaxMessageBytesCeiling is the upper bound on the admin's `max_message_bytes`
// knob (250,000,000; mail-message-size.md, Message size limits, ruled
// 2026-08-26). The nest refuses a larger write, so the bridge never sees a
// ceiling above it.
//
// The bridge reads it for sizing rather than for enforcement: the scan
// round-trip budget must bound the largest message this build could ever be
// asked to scan (scan.ScanTimeout), and the scan sidecar's shipped
// stream/scan/file limits are pinned against this same number.
func MaxMessageBytesCeiling() uint32 {
	return faunaMail.MaxMessageBytesCeiling()
}

// MailBodyChunk is one staged chunk of a sealed mail body: the bytes, and the
// blake3 digest that keys them in the content-addressed store. Hex the Hash for
// the `X-Content-Hash` upload header; ship it raw in the RPC's body reference.
type MailBodyChunk = faunaMail.MailBodyChunk

// MailBodyNeedsReference is the single switchover predicate: must this sealed
// body cross on the bulk-byte plane rather than inline in the RPC?
//
// Both halves count. The index hint is input-dependent (a unique-word-dense body
// grows it toward the body's own size), so a body comfortably under the budget
// can still assemble a request the 2 MiB frame refuses.
func MailBodyNeedsReference(sealedBodyLen, sealedHintLen uint64) bool {
	return faunaMail.MailBodyNeedsReference(sealedBodyLen, sealedHintLen)
}

// SplitSealedMailBody splits a sealed body into content-addressed chunks, in
// order — the producer uploads each, then sends the hashes as the reference.
//
// Never re-derive this chunking in Go: the Go MTA that stages, the nest that
// rejoins, and the Go MDA that re-fetches all call this one Rust implementation
// over UniFFI (priority #2). A disagreement about where a chunk boundary falls,
// or about which hash keys a chunk, would corrupt mail silently.
func SplitSealedMailBody(body []byte) []MailBodyChunk {
	return faunaMail.SplitSealedMailBody(body)
}

// JoinSealedMailBody rejoins chunks fetched back off the byte plane, in the
// order the reference listed them. The caller must still pin the result against
// the reference's declared total — the chunk contents are self-verifying
// (content-addressed) but the chunk *list* is not.
func JoinSealedMailBody(chunks [][]byte) []byte {
	return faunaMail.JoinSealedMailBody(chunks)
}

// StagedSeal is the output of SealStagedBody: the one-shot random Key and the
// nonce-prefixed ciphertext (Sealed) that actually gets chunked and staged.
type StagedSeal = faunaMail.StagedSeal

// SealStagedBody AEAD-seals a PLAINTEXT-derived mail body under a fresh one-shot
// key for staging on the bulk-byte plane (the outbound-queue enqueue leg — the
// staged-envelope rule). The plaintext outbound legs cannot use the sealed-body
// (MailBodyRef) path: the open chunk-download route is safe only because
// everything in the store is ciphertext, so staging raw plaintext would disclose
// it to anyone holding the hash. The returned Sealed bytes ride the existing
// SplitSealedMailBody chunker; the returned Key travels inside the
// already-confidential WS-RPC. Never re-derive this in Go (priority #2).
func SealStagedBody(plain []byte) StagedSeal {
	return faunaMail.SealStagedBody(plain)
}

// OpenStagedBody reverses SealStagedBody: it AEAD-opens the rejoined sealed
// bytes with the one-shot key from the reference, recovering the original
// plaintext body. The AEAD tag authenticates the entire join, so a corrupt key
// or tampered ciphertext fails closed with an error — never a silent
// partial/empty body. The consumer must have already pinned the join against the
// reference's declared total before calling this.
func OpenStagedBody(sealed, key []byte) ([]byte, error) {
	return faunaMail.OpenStagedBody(sealed, key)
}

// NewQueueID returns a fresh 16-hex-char opaque transaction id for the
// `Received:` `id` clause, derived from 8 crypto/rand bytes. Uniqueness is
// per-process; grep it across the bridge + nest logs to correlate one inbound
// transaction. crypto/rand failure (never expected) falls back to all-zero
// rather than failing the SMTP DATA path.
func NewQueueID() string {
	var buf [8]byte
	if _, err := rand.Read(buf[:]); err != nil {
		return "0000000000000000"
	}
	return hex.EncodeToString(buf[:])
}

// ── Forward-loop detection (mail-forwarding N2 — MTA forward stage) ──
//
// Thin wrappers over the shared `fauna_mail::forward_loop` R2 (account-data-plane.md § The ratified decisions) core (crossed
// via UniFFI), so the bridge's post-delivery forward stage applies the exact
// same loop-suppression floors + stamp spelling as nest and the apps
// (priority #2). See `docs/goal/behavior/mail-forwarding.md` § Loop detection.

// HeaderForwardedBy is the loop-detection header field name each forward
// stamps (mail-forwarding.md:138). Mirrors the shared Rust const
// `fauna_mail::forward_loop::HEADER_FORWARDED_BY` (UniFFI cannot export a
// string const, so it is restated here; the value is an interop-stable
// header name).
const HeaderForwardedBy = "X-Fauna-Forwarded-By"

// ForwardReceivedChainExceeded reports whether an inbound message's Received:
// chain is too long to forward — the forward is suppressed but local delivery
// still completes (mail-forwarding.md:134, MAX_RECEIVED_HOPS=10). `count` is
// the number of Received: header fields on the inbound message.
func ForwardReceivedChainExceeded(count uint64) bool {
	return faunaFfi.ForwardReceivedChainExceeded(count)
}

// ForwardStampValue builds the X-Fauna-Forwarded-By header *value* (no field
// name): `actor=<id>; t=<unix>; rule=<rule-id|forward-all>`
// (mail-forwarding.md:138-141). Prepend `HeaderForwardedBy + ": "` to form
// the full header line.
func ForwardStampValue(actorID string, unixTime int64, rule string) string {
	return faunaFfi.ForwardStampValue(actorID, unixTime, rule)
}

// ForwardSelfAlreadyForwarded reports whether any X-Fauna-Forwarded-By value
// on the inbound message was stamped by our own forwarding actor — looping it
// again would tornado, so suppress the forward (mail-forwarding.md:146). A
// peer's different actor does not suppress.
func ForwardSelfAlreadyForwarded(forwardedByValues []string, ourActorID string) bool {
	return faunaFfi.ForwardSelfAlreadyForwarded(forwardedByValues, ourActorID)
}

// AtprotoIdentityKeyBundle re-exports the UniFFI-generated record carrying
// the plaintext side of an HPKE-Opened `AtprotoIdentityBlob` — the
// bridge-custodied half of the did:plc key-custody split (signing + junior
// rotation K-256 scalars with their `did:key` public halves).
type AtprotoIdentityKeyBundle = faunaFfi.AtprotoIdentityKeyBundle

// UnsealAtprotoIdentityBlob HPKE-Opens a sealed ATProto identity-key blob
// fetched via `fauna.bridges.atproto.fetch_identity_key_blob` and returns the
// plaintext key bundle. The atproto.pds bridge's mint loop calls this per
// pending identity right before building + signing the PLC genesis op.
//
//   - blobBytes: canonical DAG-CBOR `AtprotoIdentityBlob` (nil/empty surfaces
//     as the FFI's decode error).
//   - recipientX25519Secret: the bridge's 32-byte X25519 private key (from the
//     on-disk service-user keyfile).
//   - expectedSigningPubDIDKey / expectedRotationPubDIDKey: the identity's two
//     PUBLISHED keys, as the same fetch reply carries them beside the blob. The
//     shared Rust refuses the blob unless the keys inside are exactly these —
//     the check that stops one identity's whole blob opening where another's
//     was asked for. An empty expectation is refused, never "no expectation".
//
// The binding is to the keys, never to an actor id: the blob moves to a
// successor unchanged, so the returned bundle's ActorId names the actor the
// keys were minted for — after a succession, an ancestor of the account asked
// about. It is provenance; do not compare it.
//
// The returned bundle's SigningPriv / RotationPriv fields carry raw 32-byte
// K-256 scalars; the caller keeps their lifetime short and overwrites them
// after use (no on-disk persistence).
func UnsealAtprotoIdentityBlob(
	blobBytes []byte,
	recipientX25519Secret []byte,
	expectedSigningPubDIDKey string,
	expectedRotationPubDIDKey string,
) (*AtprotoIdentityKeyBundle, error) {
	bundle, err := faunaFfi.UnsealAtprotoIdentityBlob(
		blobBytes, recipientX25519Secret, expectedSigningPubDIDKey, expectedRotationPubDIDKey,
	)
	if err != nil {
		return nil, err
	}
	// UniFFI returns the Record by value; hand a pointer so callers can use
	// nil as a "no bundle" sentinel.
	return &bundle, nil
}

// UnsealAtprotoSessionSecretBlob HPKE-Opens the bridge-wide sealed HS256
// session-token secret fetched via
// `fauna.bridges.atproto.fetch_session_secret_blob` and returns the raw
// 32-byte signing key. The atproto.pds bridge calls this once at boot; the
// secret then lives for the process lifetime as the XRPC token minter's key
// (`atproto-pds-full.md` § Key material inventory).
func UnsealAtprotoSessionSecretBlob(blobBytes []byte, recipientX25519Secret []byte) ([]byte, error) {
	bundle, err := faunaFfi.UnsealAtprotoSessionSecretBlob(blobBytes, recipientX25519Secret)
	if err != nil {
		return nil, err
	}
	return bundle.Secret, nil
}

// CapabilityGrant / CapabilityScopeKey re-export the UniFFI-generated records
// carrying a HPKE-Opened capability grant — the plaintext side of a
// fauna_mls::wrapped_blob::GrantBlob the nest serves via
// `fauna.capabilities.fetch`. Each CapabilityScopeKey.Key is a minimal derived
// content key (secret); the capability holder loop keeps its lifetime short and
// zeroizes on revoke (design § Phase 2 Step 2 § 2.3).
type (
	CapabilityGrant    = faunaFfi.UnsealedCapabilityGrant
	CapabilityScopeKey = faunaFfi.UnsealedScopeKey
)

// UnsealCapabilityGrant HPKE-Opens every wrapped scope key of a capability grant
// fetched via `fauna.capabilities.fetch` and returns the plaintext grant (owner
// / grant_id / window + one key per key-bearing scope tuple). The capability
// holder loop (`internal/capability`) calls this per grant on each refresh.
//
//   - blobBytes: canonical DAG-CBOR `GrantBlob` (one element of the fetch reply's
//     `grants` list; nil/empty surfaces as the FFI's decode error).
//   - holderX25519Secret: the bridge's 32-byte X25519 private key — the enrolled
//     service-user (holder) secret, NOT the actor identity.
//   - holderMlkemDk: the holder's 2400-byte ML-KEM-768 decapsulation key
//     (PQ-CAP-2), derived from the bridge's Ed25519 keyfile seed. When non-empty,
//     the FFI opens BOTH classical and hybrid (X-Wing) wraps; nil/empty opens
//     classical wraps only (test callers).
//
// The unseal is all-or-nothing: since every key is sealed to the one holder, a
// wrong secret / tampered blob fails the whole grant (an HPKE error), and the
// holder loop omits that grant from its refreshed set.
func UnsealCapabilityGrant(blobBytes []byte, holderX25519Secret []byte, holderMlkemDk []byte) (*CapabilityGrant, error) {
	// UniFFI maps the Rust `Option<Vec<u8>>` dk param to `*[]byte`; pass a
	// pointer only when the holder has a published ML-KEM key.
	var dkArg *[]byte
	if len(holderMlkemDk) > 0 {
		dk := holderMlkemDk
		dkArg = &dk
	}
	grant, err := faunaFfi.UnsealCapabilityGrant(blobBytes, holderX25519Secret, dkArg)
	if err != nil {
		return nil, err
	}
	// UniFFI returns the Record by value; hand a pointer so the holder loop can
	// use nil as a "no grant" sentinel.
	return &grant, nil
}

// DeriveBridgeServiceUserMlkem derives the holder's ML-KEM-768 keypair (PQ-CAP-2)
// from the bridge's 32-byte Ed25519 keyfile seed, domain-separated in shared Rust
// (`fauna.bridge.service-user-mlkem.v1`, distinct from the mail-recipient and
// subscription derivations). Returns (dk, ek): the 2400-byte decapsulation key the
// holder keeps to open hybrid (X-Wing) capability-grant wraps, and the 1184-byte
// encapsulation key it publishes at `register_service_user` so the client mint can
// seal grants X-Wing to `from_parts(ek, x25519_pubkey)`. Deterministic in the seed
// — re-derived each boot, never persisted (the keyfile carries only the Ed25519
// seed + X25519 key).
func DeriveBridgeServiceUserMlkem(ed25519Seed []byte) (dk, ek []byte, err error) {
	kp, err := faunaFfi.DeriveBridgeServiceUserMlkem768(ed25519Seed)
	if err != nil {
		return nil, nil, err
	}
	return kp.MlkemDk, kp.MlkemEk, nil
}

// ── Wrapped-MSEK AEAD-unwrap (Phase C.3 / IMAP MDA) ──
//
// MLSCapability wraps the generated UniFFI Object that owns the
// mlock'd MSEK. The bridge's IMAP Session stashes it after a
// successful AUTH and zeroizes it on Close. The UniFFI binding
// already runs Zeroize via the Drop/Destroy chain on garbage
// collection; calling Zeroize() explicitly from Session.Close
// reclaims the secret eagerly (no GC dependency) and makes the
// intent visible in code review.
//
// Aliased so the imap package can reference the type without
// pulling in the generated `fauna_ffi` import.
type MLSCapability = faunaFfi.MlsCapability

// KdfKind selects the credential-derived AEAD KDF arm. Argon2id
// for AUTH=PLAIN (low-entropy passwords); Hkdf for AUTH=OAUTHBEARER
// (high-entropy bearer tokens); see the wrapped-blob crypto design
// (tracked internally).
type KdfKind = faunaFfi.KdfKind

const (
	KdfKindArgon2id = faunaFfi.KdfKindArgon2id
	KdfKindHkdf     = faunaFfi.KdfKindHkdf
)

// UnwrapMLSBlob AEAD-unwraps a per-credential wrapped-MSEK blob and
// returns the mlock'd MLS-decryption capability. AEAD-success is
// the authentication-success signal; AEAD-failure (or any returned
// error) is the authentication-failure signal per
// docs/goal/behavior/imap-server.md § Authentication.
//
//   - blobBytes: canonical DAG-CBOR `WrappedMsekBlob` (from
//     fauna.bridges.fetch_wrapped_mls_blob).
//   - credential: the MUA-supplied secret (UTF-8 password for
//     Argon2id; high-entropy token bytes for Hkdf).
//   - actorID: the 32-byte bridge-resolved actor_id (from
//     fauna.bridges.validate_recipient → hex → bytes).
//   - credentialID: the credential identifier the blob is sealed
//     under; bound into the AAD for substitution-resistance.
//   - kind: which KDF arm to dispatch.
//
// The bridge MUST call MLSCapability.Zeroize() on session close;
// the generated finalizer also runs Zeroize on GC as a backstop.
func UnwrapMLSBlob(
	blobBytes []byte,
	credential []byte,
	actorID []byte,
	credentialID string,
	kind KdfKind,
) (*MLSCapability, error) {
	return faunaFfi.UnwrapMsekBlob(blobBytes, credential, actorID, credentialID, kind)
}

// ── Submission-token AEAD-unseal (Phase D.2 / MTA submission) ──
//
// SubmissionTokenFfi mirrors the FFI Record returned by the
// `unseal_submission_token_blob` UniFFI export: the public policy
// fields of a `fauna_mls::wrapped_blob::SubmissionToken` (quotas,
// expiry, actor_id, credential_id). `user_sig` is verified inside the
// FFI and not surfaced; AEAD success + inner-sig success is the
// authentication signal.
//
// Aliased so internal/mta and tests can reference the type without
// pulling in the generated `fauna_ffi` import.
type SubmissionTokenFfi = faunaFfi.SubmissionTokenFfi

// UnsealSubmissionTokenBlob AEAD-unseals a per-credential wrapped
// submission token and returns the plaintext policy fields. AEAD
// success AND inner Ed25519 signature verify are both required —
// either failure surfaces as a returned error and is the
// authentication-failure signal.
//
//   - blobBytes: canonical DAG-CBOR `WrappedSubmissionTokenBlob`
//     (from fauna.bridges.fetch_wrapped_submission_token).
//   - credential: MUA-supplied secret (UTF-8 password for Argon2id;
//     high-entropy bearer token bytes for Hkdf).
//   - actorID: the 32-byte bridge-resolved actor_id (from
//     fauna.bridges.validate_recipient). MUST be the user's Ed25519
//     verifying-key bytes — used both as the AAD binding parameter
//     and as the public key for the inner SubmissionToken signature
//     verify. The codebase invariant (`actor_id == Ed25519
//     verifying-key`, cf. bins/fauna-nest/src/registration.rs:184)
//     lets the FFI reconstruct the user's primary signing pubkey
//     from actor_id without a separate nest fetch.
//   - credentialID: cross-checked against blob.index and bound into
//     the AAD for substitution-resistance.
//   - kind: which KDF arm to dispatch (Argon2id for PLAIN, Hkdf for
//     OAUTHBEARER).
//
// Returns nil + error on any failure. The plaintext token carries no
// secret material (just signed policy data), so callers don't need to
// zeroize it on session close.
//
// See the mail-bridge rearchitecture design (tracked internally)
// § Outbound submission steps 1–2.
func UnsealSubmissionTokenBlob(
	blobBytes []byte,
	credential []byte,
	actorID []byte,
	credentialID string,
	kind KdfKind,
) (*SubmissionTokenFfi, error) {
	tok, err := faunaFfi.UnsealSubmissionTokenBlob(blobBytes, credential, actorID, credentialID, kind)
	if err != nil {
		return nil, err
	}
	// UniFFI's generated Record comes back by value; we hand a
	// pointer to the caller so a nil-stash signals "not authenticated"
	// in submissionSession's field. The struct contains no UniFFI
	// Object handles, so addressing it is safe (no Drop chain to
	// worry about).
	return &tok, nil
}
