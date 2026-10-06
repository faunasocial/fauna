package mta

import (
	"context"
	"encoding/hex"
	"fmt"
	"log/slog"

	"github.com/faunasocial/fauna/bins/fauna-bridges/internal/mailfauna"
	"github.com/faunasocial/fauna/bins/fauna-bridges/internal/wsrpc"
	"lukechampine.com/blake3"
)

// inviteFromMail is the shared-Rust invitation reader; a variable only so a
// test can make it panic (TestInviteStagePanicKeepsDeliveredMail).
var inviteFromMail = mailfauna.InviteFromMail

// placeInboundInvite puts an invitation that arrived by email on the local
// recipient's calendar — the inbound half of caldav-server.md § Server-side
// auto-schedule ("the MTA receives the iMIP REQUEST in cleartext and seals it
// to the recipient … and drops it on their calendar"). Called once per local
// recipient, strictly AFTER that recipient's mail copy is ingested, and only
// for a copy that went to the inbox (an invitation filed as spam
// never reaches the calendar).
//
// The shared-Rust reader (mailfauna.InviteFromMail) decides whether the message
// is an invitation and renders the body the calendar stores — the same renderer
// a Fauna app's own PUT uses. The MTA seals it exactly as the MDA seals a
// CalDAV PUT (body to the recipient's calendar key, index hint to their index
// key) and hands nest only ciphertext: encrypt-only, so placing an invitation
// grants the MTA no read of anyone's calendar. nest keeps it create-only and
// re-derives the guardian mail verdict from `senderAddress`.
//
// Best-effort, like the forward and the auto-reply beside it: the mail is
// already delivered, so a failure is logged and the calendar stays as it was.
func placeInboundInvite(
	ctx context.Context,
	caller wsrpc.Caller,
	logger *slog.Logger,
	actorID, raw []byte,
	senderAddress string,
	timestamp int64,
) {
	if logger == nil {
		logger = slog.Default()
	}
	actor := hex.EncodeToString(actorID)
	// A panic crossing the UniFFI boundary from the shared-Rust reader (a
	// hostile invitation) would otherwise unwind into go-smtp's
	// connection-level recover, turn the delivered message into a 421, and make
	// every sender retry a duplicate delivery — the same post-delivery guard the
	// auto-reply stage carries. Swallow it: only the calendar entry is lost.
	defer func() {
		if r := recover(); r != nil {
			logger.Error("smtp: invitation stage panicked (mail delivered; calendar untouched)",
				"actor", actor, "panic", fmt.Sprintf("%v", r), "verdict", "invite_panic")
		}
	}()
	invite := inviteFromMail(raw, timestamp)
	if invite == nil {
		return
	}
	fail := func(step string, err error) {
		logger.Warn("smtp: an emailed invitation was delivered but not placed on the calendar",
			"actor", actor, "step", step, "err", err)
	}
	// The calendar key is the actor's non-epoch recipient key — the one the
	// MDA seals a CalDAV PUT to — not the mail-ingest epoch key.
	pubkey, mlkemEk, _, err := wsrpc.FetchRecipientMLSPubkeyHybrid(ctx, caller, actorID, false)
	if err != nil || pubkey == nil {
		fail("fetch calendar key", err)
		return
	}
	indexKey, err := wsrpc.FetchRecipientIndexKey(ctx, caller, actorID)
	if err != nil {
		fail("fetch index key", err)
		return
	}
	if indexKey == nil {
		indexKey = pubkey
	}
	body, err := mailfauna.EncryptToRecipientHybrid([]byte(invite.Ics), pubkey, mlkemEk)
	if err != nil {
		fail("seal body", err)
		return
	}
	hint, err := mailfauna.EncryptToRecipientHybrid(
		mailfauna.Tokenize(invite.Ics).CanonicalBytes, indexKey,
		mailfauna.IndexHintMlkemEk(indexKey, pubkey, mlkemEk))
	if err != nil {
		fail("seal index hint", err)
		return
	}
	uidHash := blake3.Sum256([]byte(invite.Uid))
	outcome, err := wsrpc.PlaceInboundInvite(
		ctx, caller, actorID, uidHash[:], body, hint, timestamp, senderAddress)
	if err != nil {
		fail("place", err)
		return
	}
	logger.Info("smtp: emailed invitation handled", "actor", actor, "outcome", outcome)
}
