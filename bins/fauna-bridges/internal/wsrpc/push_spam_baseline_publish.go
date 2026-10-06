package wsrpc

import (
	"log/slog"

	"github.com/faunasocial/fauna/bins/fauna-bridges/internal/dagcbor"
)

// PushKindSpamBaselinePublish is the nest→bridge push kind that prompts the
// aggregation holder to drain one spam-baseline publish run. Mirrors
// fauna-protocol's PUSH_KIND_BRIDGE_SPAM_BASELINE_PUBLISH (bridge_routing.rs)
// and mail-spam.md § Encrypted-mode interaction (ratified 2026-07-13). The
// third holder-pull drain trigger beside `rescore_ready` — but unlike that
// content-free "drain now" nudge this push carries the 16-byte run_id the
// holder must echo on its `spam_baseline_worklist` pull and `submit_spam_baseline`,
// scoping both RPCs to exactly this publish window.
const PushKindSpamBaselinePublish = "fauna.bridges.spam_baseline_publish"

// BridgeSpamBaselinePublishPush mirrors fauna-protocol's
// BridgeSpamBaselinePublishPush — the push payload carries only the run id.
type BridgeSpamBaselinePublishPush struct {
	RunID []byte `cbor:"run_id"`
}

// SpamBaselinePublishHandler is the [PushHandler] for PushKindSpamBaselinePublish,
// installed on the MDA's PushDispatcher. On the push it decodes the run_id and
// hands it to the spam-baseline drain's coalescing poke so the publish drains
// against exactly the run the nest is awaiting. Best-effort: the nest's publish
// handler bounds its wait, so a missed/undecodable push (or a full poke queue)
// just proceeds from the readable half with an honest skipped count — no
// obligation is lost. Only a process with a live capability holder (→ a drain)
// registers this.
//
// The handler runs on the wsrpc reader goroutine (the PushHandler "fast and
// non-blocking" contract), so it does NO RPC itself — the worklist pull +
// submit run on the drain's own goroutine, exactly as config_changed defers its
// re-fetch off the reader goroutine to avoid a reply-read deadlock.
type SpamBaselinePublishHandler struct {
	poke   func(runID []byte)
	logger *slog.Logger
}

// NewSpamBaselinePublishHandler wraps the drain's poke. A nil logger defaults to
// slog.Default().
func NewSpamBaselinePublishHandler(poke func(runID []byte), logger *slog.Logger) *SpamBaselinePublishHandler {
	if logger == nil {
		logger = slog.Default()
	}
	return &SpamBaselinePublishHandler{poke: poke, logger: logger}
}

// Handle guards the kind (so it is safe installed directly), decodes the run
// id, logs the nudge, and fires the coalescing poke. A malformed/empty payload
// is dropped (the run simply times out nest-side → honest skip). Non-blocking.
func (h *SpamBaselinePublishHandler) Handle(kind string, payload []byte, seq uint64) {
	if kind != PushKindSpamBaselinePublish {
		return
	}
	push, err := dagcbor.Unmarshal[BridgeSpamBaselinePublishPush](payload)
	if err != nil || len(push.RunID) == 0 {
		h.logger.Warn("spam_baseline_publish push with no decodable run_id; dropping", "seq", seq, "err", err)
		return
	}
	h.logger.Info("spam_baseline_publish push received; poking spam-baseline drain", "seq", seq)
	if h.poke != nil {
		h.poke(push.RunID)
	}
}
