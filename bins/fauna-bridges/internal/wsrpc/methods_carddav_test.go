// Tests for the CardDAV typed WS-RPC method wrappers (the `bridge_carddav_*`
// block in methods.go). These mirror the CalDAV wrapper tests in
// methods_test.go one-for-one (calendar→addressbook, event→card): each subtest
// substitutes a *recordingCaller for the underlying *Client, captures the
// method name + encoded body the wrapper wrote, and asserts
//  1. the method string equals `fauna.bridges.<name>` exactly,
//  2. the request body decodes into a struct mirroring the Rust wire shape
//     (field names, types, and the Option→pointer nullability discipline),
//  3. the wrapper unpacks each `#[serde(tag = "outcome")]` reply variant.
//
// Shared helpers (recordingCaller, bytes32, bytesEq, assertWireFieldFirstByte,
// assertWireFieldAbsent) live in methods_test.go — same package.
package wsrpc

import (
	"context"
	"errors"
	"os"
	"path/filepath"
	"strings"
	"testing"

	"github.com/fxamacker/cbor/v2"

	"github.com/faunasocial/fauna/bins/fauna-bridges/internal/dagcbor"
)

func TestCardDAVMethodWrappers(t *testing.T) {
	ctx := context.Background()

	// ── provision_addressbook ───────────────────────────────────────

	t.Run("ProvisionAddressbook_MkcolCreatedRoundTrip", func(t *testing.T) {
		caller := &recordingCaller{
			replyBody: provisionAddressbookReply{Outcome: string(ProvisionAddressbookCreated)},
		}
		actor := bytes32(0xAA)
		ab := bytes32(0xBB)
		meta := []byte("SEALED-METADATA")
		got, err := ProvisionAddressbook(ctx, caller, actor[:], ab[:], meta, false)
		if err != nil {
			t.Fatalf("ProvisionAddressbook: %v", err)
		}
		if caller.gotMethod != MethodProvisionAddressbook {
			t.Errorf("method: got %q, want %s", caller.gotMethod, MethodProvisionAddressbook)
		}
		var body struct {
			ActorID           []byte `cbor:"actor_id"`
			AddressbookID     []byte `cbor:"addressbook_id"`
			EncryptedMetadata []byte `cbor:"encrypted_metadata"`
			UpdateMetadata    bool   `cbor:"update_metadata"`
		}
		if err := cbor.Unmarshal(caller.gotBody, &body); err != nil {
			t.Fatalf("decode body: %v", err)
		}
		if !bytesEq(body.ActorID, actor[:]) || !bytesEq(body.AddressbookID, ab[:]) {
			t.Errorf("ids: got actor=%x ab=%x", body.ActorID, body.AddressbookID)
		}
		if !bytesEq(body.EncryptedMetadata, meta) {
			t.Errorf("encrypted_metadata: got %x, want %x", body.EncryptedMetadata, meta)
		}
		if body.UpdateMetadata {
			t.Errorf("update_metadata: got true, want false on MKCOL path")
		}
		if got != ProvisionAddressbookCreated {
			t.Errorf("outcome: got %q, want %q", got, ProvisionAddressbookCreated)
		}
	})

	t.Run("ProvisionAddressbook_PropPatchUpdatedRoundTrip", func(t *testing.T) {
		caller := &recordingCaller{
			replyBody: provisionAddressbookReply{Outcome: string(ProvisionAddressbookUpdated)},
		}
		actor := bytes32(0xAA)
		ab := bytes32(0xBB)
		got, err := ProvisionAddressbook(ctx, caller, actor[:], ab[:], []byte("RESEALED"), true)
		if err != nil {
			t.Fatalf("ProvisionAddressbook: %v", err)
		}
		var body struct {
			UpdateMetadata bool `cbor:"update_metadata"`
		}
		if err := cbor.Unmarshal(caller.gotBody, &body); err != nil {
			t.Fatalf("decode body: %v", err)
		}
		if !body.UpdateMetadata {
			t.Errorf("update_metadata: got false, want true on PROPPATCH path")
		}
		if got != ProvisionAddressbookUpdated {
			t.Errorf("outcome: got %q, want %q", got, ProvisionAddressbookUpdated)
		}
	})

	t.Run("ProvisionAddressbook_AllOutcomesDecode", func(t *testing.T) {
		for _, oc := range []ProvisionAddressbookOutcome{
			ProvisionAddressbookCreated, ProvisionAddressbookAlreadyExists,
			ProvisionAddressbookConflict, ProvisionAddressbookUpdated,
			ProvisionAddressbookNotFound,
		} {
			caller := &recordingCaller{replyBody: provisionAddressbookReply{Outcome: string(oc)}}
			actor := bytes32(0xAA)
			got, err := ProvisionAddressbook(ctx, caller, actor[:], actor[:], []byte("m"), false)
			if err != nil {
				t.Fatalf("ProvisionAddressbook(%s): %v", oc, err)
			}
			if got != oc {
				t.Errorf("outcome: got %q, want %q", got, oc)
			}
		}
	})

	t.Run("ProvisionAddressbook_UnknownOutcomeReturnsError", func(t *testing.T) {
		caller := &recordingCaller{replyBody: provisionAddressbookReply{Outcome: "future-shape"}}
		actor := bytes32(0xAA)
		_, err := ProvisionAddressbook(ctx, caller, actor[:], actor[:], []byte("m"), false)
		if err == nil || !strings.Contains(err.Error(), "future-shape") {
			t.Fatalf("err: %v", err)
		}
	})

	t.Run("ProvisionAddressbook_PropagatesUnderlyingError", func(t *testing.T) {
		caller := &recordingCaller{returnErr: errors.New("synthetic-provision-fail")}
		actor := bytes32(0xAA)
		_, err := ProvisionAddressbook(ctx, caller, actor[:], actor[:], []byte("m"), true)
		if err == nil || !strings.Contains(err.Error(), "synthetic-provision-fail") {
			t.Fatalf("err: %v", err)
		}
	})

	// ── list_addressbooks ───────────────────────────────────────────

	t.Run("ListAddressbooks_RoundTrip", func(t *testing.T) {
		ab := bytes32(0xB1)
		caller := &recordingCaller{
			replyBody: listAddressbooksReply{
				Addressbooks: []AddressbookEntry{
					{
						AddressbookID:     ab[:],
						EncryptedMetadata: []byte("SEALED-META"),
						CTag:              12,
						HighestModseq:     99,
						CardCount:         7,
						CreatedAt:         1700000000,
					},
				},
			},
		}
		actor := bytes32(0xAA)
		got, err := ListAddressbooks(ctx, caller, actor[:])
		if err != nil {
			t.Fatalf("ListAddressbooks: %v", err)
		}
		if caller.gotMethod != MethodListAddressbooks {
			t.Errorf("method: got %q, want %s", caller.gotMethod, MethodListAddressbooks)
		}
		var body struct {
			ActorID []byte `cbor:"actor_id"`
		}
		if err := cbor.Unmarshal(caller.gotBody, &body); err != nil {
			t.Fatalf("decode body: %v", err)
		}
		if !bytesEq(body.ActorID, actor[:]) {
			t.Errorf("actor_id: got %x, want %x", body.ActorID, actor[:])
		}
		if len(got) != 1 {
			t.Fatalf("addressbooks: got %d, want 1", len(got))
		}
		e := got[0]
		if !bytesEq(e.AddressbookID, ab[:]) || !bytesEq(e.EncryptedMetadata, []byte("SEALED-META")) {
			t.Errorf("entry bytes: %+v", e)
		}
		if e.CTag != 12 || e.HighestModseq != 99 || e.CardCount != 7 || e.CreatedAt != 1700000000 {
			t.Errorf("entry scalars: %+v", e)
		}
	})

	t.Run("ListAddressbooks_EmptyIsNormal", func(t *testing.T) {
		caller := &recordingCaller{replyBody: listAddressbooksReply{}}
		actor := bytes32(0xAA)
		got, err := ListAddressbooks(ctx, caller, actor[:])
		if err != nil {
			t.Fatalf("ListAddressbooks: %v", err)
		}
		if len(got) != 0 {
			t.Errorf("empty reply must yield 0 entries, got %d", len(got))
		}
	})

	t.Run("ListAddressbooks_PropagatesUnderlyingError", func(t *testing.T) {
		caller := &recordingCaller{returnErr: errors.New("synthetic-list-fail")}
		actor := bytes32(0xAA)
		_, err := ListAddressbooks(ctx, caller, actor[:])
		if err == nil || !strings.Contains(err.Error(), "synthetic-list-fail") {
			t.Fatalf("err: %v", err)
		}
	})

	// ── put_card_ciphertext ─────────────────────────────────────────

	t.Run("PutCardCiphertext_CreatedRoundTrip", func(t *testing.T) {
		cardID := bytes32(0xC1)
		caller := &recordingCaller{
			replyBody: putCardCiphertextReply{
				Outcome: string(PutCardCreated),
				CardID:  cardID[:],
				ETag:    "etag-1",
				Modseq:  42,
			},
		}
		actor := bytes32(0xAA)
		ab := bytes32(0xBB)
		uidHash := bytes32(0xCC)
		body := []byte("CIPHERBODY")
		hint := []byte("CIPHERHINT")
		got, err := PutCardCiphertext(ctx, caller, actor[:], ab[:], uidHash[:], body, hint, 1700000000, uint32(len(body)), nil)
		if err != nil {
			t.Fatalf("PutCardCiphertext: %v", err)
		}
		if caller.gotMethod != MethodPutCardCiphertext {
			t.Errorf("method: got %q, want %s", caller.gotMethod, MethodPutCardCiphertext)
		}
		var body2 struct {
			ActorID            []byte  `cbor:"actor_id"`
			AddressbookID      []byte  `cbor:"addressbook_id"`
			UIDHash            []byte  `cbor:"uid_hash"`
			EncryptedBody      []byte  `cbor:"encrypted_body"`
			EncryptedIndexHint []byte  `cbor:"encrypted_index_hint"`
			Timestamp          int64   `cbor:"timestamp"`
			CiphertextSize     uint32  `cbor:"ciphertext_size"`
			IfMatch            *string `cbor:"if_match"`
		}
		if err := cbor.Unmarshal(caller.gotBody, &body2); err != nil {
			t.Fatalf("decode body: %v", err)
		}
		if !bytesEq(body2.ActorID, actor[:]) || !bytesEq(body2.AddressbookID, ab[:]) || !bytesEq(body2.UIDHash, uidHash[:]) {
			t.Errorf("ids: got actor=%x ab=%x uid=%x", body2.ActorID, body2.AddressbookID, body2.UIDHash)
		}
		if !bytesEq(body2.EncryptedBody, body) || !bytesEq(body2.EncryptedIndexHint, hint) {
			t.Errorf("payloads: got body=%x hint=%x", body2.EncryptedBody, body2.EncryptedIndexHint)
		}
		if body2.Timestamp != 1700000000 || body2.CiphertextSize != uint32(len(body)) {
			t.Errorf("meta: got ts=%d size=%d", body2.Timestamp, body2.CiphertextSize)
		}
		if body2.IfMatch != nil {
			t.Errorf("if_match: got %q, want absent (nil)", *body2.IfMatch)
		}
		// The MDA never writes a sidecar — encrypted_fauna_ext MUST be omitted.
		assertWireFieldAbsent(t, caller.gotBody, "encrypted_fauna_ext")
		if got.Outcome != PutCardCreated {
			t.Errorf("outcome: got %q, want %q", got.Outcome, PutCardCreated)
		}
		if !bytesEq(got.CardID, cardID[:]) || got.ETag != "etag-1" || got.Modseq != 42 {
			t.Errorf("result: got %+v", got)
		}
	})

	t.Run("PutCardCiphertext_IfMatchEncoded", func(t *testing.T) {
		cardID := bytes32(0xC2)
		caller := &recordingCaller{
			replyBody: putCardCiphertextReply{
				Outcome: string(PutCardUpdated),
				CardID:  cardID[:],
				ETag:    "etag-2",
				Modseq:  43,
			},
		}
		etag := "previous-etag"
		actor := bytes32(0xAA)
		got, err := PutCardCiphertext(ctx, caller, actor[:], actor[:], actor[:], []byte("b"), []byte("h"), 1, 1, &etag)
		if err != nil {
			t.Fatalf("PutCardCiphertext: %v", err)
		}
		var body2 struct {
			IfMatch *string `cbor:"if_match"`
		}
		if err := cbor.Unmarshal(caller.gotBody, &body2); err != nil {
			t.Fatalf("decode body: %v", err)
		}
		if body2.IfMatch == nil || *body2.IfMatch != etag {
			t.Errorf("if_match: got %v, want &%q", body2.IfMatch, etag)
		}
		if got.Outcome != PutCardUpdated {
			t.Errorf("outcome: got %q, want updated", got.Outcome)
		}
	})

	t.Run("PutCardCiphertext_PreconditionFailedCarriesCurrentETag", func(t *testing.T) {
		caller := &recordingCaller{
			replyBody: putCardCiphertextReply{
				Outcome:     string(PutCardPreconditionFailed),
				CurrentETag: "server-etag",
			},
		}
		actor := bytes32(0xAA)
		got, err := PutCardCiphertext(ctx, caller, actor[:], actor[:], actor[:], []byte("b"), []byte("h"), 1, 1, nil)
		if err != nil {
			t.Fatalf("PutCardCiphertext: %v", err)
		}
		if got.Outcome != PutCardPreconditionFailed {
			t.Errorf("outcome: got %q, want precondition_failed", got.Outcome)
		}
		if got.CurrentETag != "server-etag" {
			t.Errorf("current_etag: got %q, want server-etag", got.CurrentETag)
		}
		if got.CardID != nil || got.ETag != "" || got.Modseq != 0 {
			t.Errorf("precondition_failed must zero out success-only fields: got %+v", got)
		}
	})

	t.Run("PutCardCiphertext_AddressbookNotFound", func(t *testing.T) {
		caller := &recordingCaller{
			replyBody: putCardCiphertextReply{Outcome: string(PutCardAddressbookNotFound)},
		}
		actor := bytes32(0xAA)
		got, err := PutCardCiphertext(ctx, caller, actor[:], actor[:], actor[:], []byte("b"), []byte("h"), 1, 1, nil)
		if err != nil {
			t.Fatalf("PutCardCiphertext: %v", err)
		}
		if got.Outcome != PutCardAddressbookNotFound {
			t.Errorf("outcome: got %q, want addressbook_not_found", got.Outcome)
		}
	})

	t.Run("PutCardCiphertext_UnknownOutcomeReturnsError", func(t *testing.T) {
		caller := &recordingCaller{replyBody: putCardCiphertextReply{Outcome: "future-shape"}}
		actor := bytes32(0xAA)
		_, err := PutCardCiphertext(ctx, caller, actor[:], actor[:], actor[:], []byte("b"), []byte("h"), 1, 1, nil)
		if err == nil || !strings.Contains(err.Error(), "future-shape") {
			t.Fatalf("err: %v", err)
		}
	})

	t.Run("PutCardCiphertext_PropagatesUnderlyingError", func(t *testing.T) {
		caller := &recordingCaller{returnErr: errors.New("synthetic-put-fail")}
		actor := bytes32(0xAA)
		_, err := PutCardCiphertext(ctx, caller, actor[:], actor[:], actor[:], []byte("b"), []byte("h"), 1, 1, nil)
		if err == nil || !strings.Contains(err.Error(), "synthetic-put-fail") {
			t.Fatalf("err: %v", err)
		}
	})

	// ── delete_card ─────────────────────────────────────────────────

	t.Run("DeleteCard_DeletedRoundTrip", func(t *testing.T) {
		cardID := bytes32(0xC3)
		caller := &recordingCaller{
			replyBody: deleteCardReply{
				Outcome: string(DeleteCardDeleted),
				CardID:  cardID[:],
				Modseq:  44,
			},
		}
		actor := bytes32(0xAA)
		ab := bytes32(0xBB)
		uidHash := bytes32(0xCC)
		got, err := DeleteCard(ctx, caller, actor[:], ab[:], uidHash[:], nil)
		if err != nil {
			t.Fatalf("DeleteCard: %v", err)
		}
		if caller.gotMethod != MethodDeleteCard {
			t.Errorf("method: got %q", caller.gotMethod)
		}
		var body2 struct {
			ActorID       []byte  `cbor:"actor_id"`
			AddressbookID []byte  `cbor:"addressbook_id"`
			UIDHash       []byte  `cbor:"uid_hash"`
			IfMatch       *string `cbor:"if_match"`
		}
		if err := cbor.Unmarshal(caller.gotBody, &body2); err != nil {
			t.Fatalf("decode body: %v", err)
		}
		if !bytesEq(body2.ActorID, actor[:]) || !bytesEq(body2.AddressbookID, ab[:]) || !bytesEq(body2.UIDHash, uidHash[:]) {
			t.Errorf("ids: got actor=%x ab=%x uid=%x", body2.ActorID, body2.AddressbookID, body2.UIDHash)
		}
		if body2.IfMatch != nil {
			t.Errorf("if_match: got %q, want absent", *body2.IfMatch)
		}
		if got.Outcome != DeleteCardDeleted {
			t.Errorf("outcome: got %q, want deleted", got.Outcome)
		}
		if !bytesEq(got.CardID, cardID[:]) || got.Modseq != 44 {
			t.Errorf("result: got %+v", got)
		}
	})

	t.Run("DeleteCard_IfMatchEncoded", func(t *testing.T) {
		cardID := bytes32(0xC4)
		caller := &recordingCaller{
			replyBody: deleteCardReply{Outcome: string(DeleteCardDeleted), CardID: cardID[:], Modseq: 45},
		}
		etag := "del-etag"
		actor := bytes32(0xAA)
		_, err := DeleteCard(ctx, caller, actor[:], actor[:], actor[:], &etag)
		if err != nil {
			t.Fatalf("DeleteCard: %v", err)
		}
		var body2 struct {
			IfMatch *string `cbor:"if_match"`
		}
		if err := cbor.Unmarshal(caller.gotBody, &body2); err != nil {
			t.Fatalf("decode body: %v", err)
		}
		if body2.IfMatch == nil || *body2.IfMatch != etag {
			t.Errorf("if_match: got %v, want &%q", body2.IfMatch, etag)
		}
	})

	t.Run("DeleteCard_NotFound", func(t *testing.T) {
		caller := &recordingCaller{replyBody: deleteCardReply{Outcome: string(DeleteCardNotFound)}}
		actor := bytes32(0xAA)
		got, err := DeleteCard(ctx, caller, actor[:], actor[:], actor[:], nil)
		if err != nil {
			t.Fatalf("DeleteCard: %v", err)
		}
		if got.Outcome != DeleteCardNotFound {
			t.Errorf("outcome: got %q, want not_found", got.Outcome)
		}
	})

	t.Run("DeleteCard_PreconditionFailedCarriesCurrentETag", func(t *testing.T) {
		caller := &recordingCaller{
			replyBody: deleteCardReply{
				Outcome:     string(DeleteCardPreconditionFailed),
				CurrentETag: "server-etag",
			},
		}
		actor := bytes32(0xAA)
		got, err := DeleteCard(ctx, caller, actor[:], actor[:], actor[:], nil)
		if err != nil {
			t.Fatalf("DeleteCard: %v", err)
		}
		if got.Outcome != DeleteCardPreconditionFailed {
			t.Errorf("outcome: got %q, want precondition_failed", got.Outcome)
		}
		if got.CurrentETag != "server-etag" {
			t.Errorf("current_etag: got %q", got.CurrentETag)
		}
	})

	t.Run("DeleteCard_UnknownOutcomeReturnsError", func(t *testing.T) {
		caller := &recordingCaller{replyBody: deleteCardReply{Outcome: "future-shape"}}
		actor := bytes32(0xAA)
		_, err := DeleteCard(ctx, caller, actor[:], actor[:], actor[:], nil)
		if err == nil || !strings.Contains(err.Error(), "future-shape") {
			t.Fatalf("err: %v", err)
		}
	})

	t.Run("DeleteCard_PropagatesUnderlyingError", func(t *testing.T) {
		caller := &recordingCaller{returnErr: errors.New("synthetic-delete-fail")}
		actor := bytes32(0xAA)
		_, err := DeleteCard(ctx, caller, actor[:], actor[:], actor[:], nil)
		if err == nil || !strings.Contains(err.Error(), "synthetic-delete-fail") {
			t.Fatalf("err: %v", err)
		}
	})

	// ── delete_addressbook ──────────────────────────────────────────

	t.Run("DeleteAddressbook_DeletedRoundTrip", func(t *testing.T) {
		caller := &recordingCaller{
			replyBody: deleteAddressbookReply{
				Outcome:      string(DeleteAddressbookDeleted),
				CardsDeleted: 7,
			},
		}
		actor := bytes32(0xAA)
		ab := bytes32(0xBB)
		got, err := DeleteAddressbook(ctx, caller, actor[:], ab[:])
		if err != nil {
			t.Fatalf("DeleteAddressbook: %v", err)
		}
		if caller.gotMethod != MethodDeleteAddressbook {
			t.Errorf("method: got %q, want %s", caller.gotMethod, MethodDeleteAddressbook)
		}
		var body2 struct {
			ActorID       []byte `cbor:"actor_id"`
			AddressbookID []byte `cbor:"addressbook_id"`
		}
		if err := cbor.Unmarshal(caller.gotBody, &body2); err != nil {
			t.Fatalf("decode body: %v", err)
		}
		if !bytesEq(body2.ActorID, actor[:]) || !bytesEq(body2.AddressbookID, ab[:]) {
			t.Errorf("ids: got actor=%x ab=%x", body2.ActorID, body2.AddressbookID)
		}
		if got.Outcome != DeleteAddressbookDeleted || got.CardsDeleted != 7 {
			t.Errorf("result: got %+v", got)
		}
	})

	t.Run("DeleteAddressbook_NotFound", func(t *testing.T) {
		caller := &recordingCaller{replyBody: deleteAddressbookReply{Outcome: string(DeleteAddressbookNotFound)}}
		actor := bytes32(0xAA)
		got, err := DeleteAddressbook(ctx, caller, actor[:], actor[:])
		if err != nil {
			t.Fatalf("DeleteAddressbook: %v", err)
		}
		if got.Outcome != DeleteAddressbookNotFound || got.CardsDeleted != 0 {
			t.Errorf("result: got %+v", got)
		}
	})

	t.Run("DeleteAddressbook_UnknownOutcomeReturnsError", func(t *testing.T) {
		caller := &recordingCaller{replyBody: deleteAddressbookReply{Outcome: "future-shape"}}
		actor := bytes32(0xAA)
		_, err := DeleteAddressbook(ctx, caller, actor[:], actor[:])
		if err == nil || !strings.Contains(err.Error(), "future-shape") {
			t.Fatalf("err: %v", err)
		}
	})

	t.Run("DeleteAddressbook_PropagatesUnderlyingError", func(t *testing.T) {
		caller := &recordingCaller{returnErr: errors.New("synthetic-delete-ab-fail")}
		actor := bytes32(0xAA)
		_, err := DeleteAddressbook(ctx, caller, actor[:], actor[:])
		if err == nil || !strings.Contains(err.Error(), "synthetic-delete-ab-fail") {
			t.Fatalf("err: %v", err)
		}
	})

	// ── query_cards ─────────────────────────────────────────────────

	t.Run("QueryCards_OkRoundTrip", func(t *testing.T) {
		cardID := bytes32(0xC5)
		uidHash := bytes32(0xC6)
		ext := []byte("sealed-fauna-ext")
		caller := &recordingCaller{
			replyBody: queryCardsReply{
				Outcome: string(QueryCardsOk),
				Cards: []CardEntry{
					{
						CardID:             cardID[:],
						UIDHash:            uidHash[:],
						EncryptedBody:      []byte("CIPHERBODY"),
						EncryptedIndexHint: []byte("CIPHERHINT"),
						ETag:               "etag-q1",
						Modseq:             100,
						CiphertextSize:     10,
						InternalDate:       1700000000,
						EncryptedFaunaExt:  &ext,
					},
				},
				HighestModseq: 100,
				More:          false,
			},
		}
		actor := bytes32(0xAA)
		ab := bytes32(0xBB)
		got, err := QueryCards(ctx, caller, actor[:], ab[:], nil, nil, 0)
		if err != nil {
			t.Fatalf("QueryCards: %v", err)
		}
		if caller.gotMethod != MethodQueryCards {
			t.Errorf("method: got %q, want %s", caller.gotMethod, MethodQueryCards)
		}
		var body2 struct {
			ActorID       []byte `cbor:"actor_id"`
			AddressbookID []byte `cbor:"addressbook_id"`
			SinceModseq   *int64 `cbor:"since_modseq"`
			AfterCardID   []byte `cbor:"after_card_id"`
			Limit         uint32 `cbor:"limit"`
		}
		if err := cbor.Unmarshal(caller.gotBody, &body2); err != nil {
			t.Fatalf("decode body: %v", err)
		}
		if !bytesEq(body2.ActorID, actor[:]) || !bytesEq(body2.AddressbookID, ab[:]) {
			t.Errorf("ids: got actor=%x ab=%x", body2.ActorID, body2.AddressbookID)
		}
		if body2.SinceModseq != nil {
			t.Errorf("since_modseq: got %v, want absent (nil)", *body2.SinceModseq)
		}
		if body2.AfterCardID != nil {
			t.Errorf("after_card_id: got %x, want absent (nil)", body2.AfterCardID)
		}
		if body2.Limit != 0 {
			t.Errorf("limit: got %d, want 0", body2.Limit)
		}
		if got.Outcome != QueryCardsOk {
			t.Errorf("outcome: got %q, want ok", got.Outcome)
		}
		if len(got.Cards) != 1 {
			t.Fatalf("cards: got %d, want 1", len(got.Cards))
		}
		c := got.Cards[0]
		if !bytesEq(c.CardID, cardID[:]) || c.ETag != "etag-q1" || c.Modseq != 100 {
			t.Errorf("card: got %+v", c)
		}
		if c.EncryptedFaunaExt == nil || !bytesEq(*c.EncryptedFaunaExt, ext) {
			t.Errorf("encrypted_fauna_ext round-trip (Some): got %v", c.EncryptedFaunaExt)
		}
		if got.HighestModseq != 100 || got.More {
			t.Errorf("highestmodseq/more: got %d/%v", got.HighestModseq, got.More)
		}
	})

	t.Run("QueryCards_SincePresentResumeCursor", func(t *testing.T) {
		caller := &recordingCaller{
			replyBody: queryCardsReply{
				Outcome: string(QueryCardsOk),
				Cards:   []CardEntry{},
				More:    true,
			},
		}
		actor := bytes32(0xAA)
		ab := bytes32(0xBB)
		after := bytes32(0xCC)
		since := int64(42)
		_, err := QueryCards(ctx, caller, actor[:], ab[:], &since, after[:], 100)
		if err != nil {
			t.Fatalf("QueryCards: %v", err)
		}
		var body2 struct {
			SinceModseq *int64 `cbor:"since_modseq"`
			AfterCardID []byte `cbor:"after_card_id"`
			Limit       uint32 `cbor:"limit"`
		}
		if err := cbor.Unmarshal(caller.gotBody, &body2); err != nil {
			t.Fatalf("decode body: %v", err)
		}
		if body2.SinceModseq == nil || *body2.SinceModseq != 42 {
			t.Errorf("since_modseq: got %v, want &42", body2.SinceModseq)
		}
		if !bytesEq(body2.AfterCardID, after[:]) {
			t.Errorf("after_card_id: got %x, want %x", body2.AfterCardID, after[:])
		}
		if body2.Limit != 100 {
			t.Errorf("limit: got %d, want 100", body2.Limit)
		}
	})

	t.Run("QueryCards_AddressbookNotFound", func(t *testing.T) {
		caller := &recordingCaller{
			replyBody: queryCardsReply{Outcome: string(QueryCardsAddressbookNotFound)},
		}
		actor := bytes32(0xAA)
		got, err := QueryCards(ctx, caller, actor[:], actor[:], nil, nil, 0)
		if err != nil {
			t.Fatalf("QueryCards: %v", err)
		}
		if got.Outcome != QueryCardsAddressbookNotFound {
			t.Errorf("outcome: got %q, want addressbook_not_found", got.Outcome)
		}
		if len(got.Cards) != 0 || got.HighestModseq != 0 {
			t.Errorf("not-found should have zero payload: %+v", got)
		}
	})

	t.Run("QueryCards_UnknownOutcomeReturnsError", func(t *testing.T) {
		caller := &recordingCaller{replyBody: queryCardsReply{Outcome: "future-shape"}}
		actor := bytes32(0xAA)
		_, err := QueryCards(ctx, caller, actor[:], actor[:], nil, nil, 0)
		if err == nil || !strings.Contains(err.Error(), "future-shape") {
			t.Fatalf("err: %v", err)
		}
	})

	t.Run("QueryCards_PropagatesUnderlyingError", func(t *testing.T) {
		caller := &recordingCaller{returnErr: errors.New("synthetic-query-fail")}
		actor := bytes32(0xAA)
		_, err := QueryCards(ctx, caller, actor[:], actor[:], nil, nil, 0)
		if err == nil || !strings.Contains(err.Error(), "synthetic-query-fail") {
			t.Fatalf("err: %v", err)
		}
	})

	// ── sync_addressbook_since ──────────────────────────────────────

	t.Run("SyncAddressbookSince_OkRoundTrip", func(t *testing.T) {
		changedID := bytes32(0xC7)
		expungedID := bytes32(0xC8)
		expungedUID := bytes32(0xC9)
		caller := &recordingCaller{
			replyBody: syncAddressbookSinceReply{
				Outcome: string(SyncAddressbookSinceOk),
				Changed: []CardEntry{
					{CardID: changedID[:], ETag: "etag-c1", Modseq: 50},
				},
				Expunged: []ExpungedCardEntry{
					{CardID: expungedID[:], UIDHash: expungedUID[:], Modseq: 51},
				},
				NewSyncToken: "51",
				More:         false,
			},
		}
		actor := bytes32(0xAA)
		ab := bytes32(0xBB)
		got, err := SyncAddressbookSince(ctx, caller, actor[:], ab[:], "0", 0, "")
		if err != nil {
			t.Fatalf("SyncAddressbookSince: %v", err)
		}
		if caller.gotMethod != MethodSyncAddressbookSince {
			t.Errorf("method: got %q, want %s", caller.gotMethod, MethodSyncAddressbookSince)
		}
		var body2 struct {
			ActorID       []byte `cbor:"actor_id"`
			AddressbookID []byte `cbor:"addressbook_id"`
			SyncToken     string `cbor:"sync_token"`
			Limit         uint32 `cbor:"limit"`
		}
		if err := cbor.Unmarshal(caller.gotBody, &body2); err != nil {
			t.Fatalf("decode body: %v", err)
		}
		if body2.SyncToken != "0" {
			t.Errorf("sync_token: got %q, want %q", body2.SyncToken, "0")
		}
		// same-nest / no MUA ⇒ mua_id omitted.
		assertWireFieldAbsent(t, caller.gotBody, "mua_id")
		if got.Outcome != SyncAddressbookSinceOk {
			t.Errorf("outcome: got %q, want ok", got.Outcome)
		}
		if len(got.Changed) != 1 || got.Changed[0].ETag != "etag-c1" {
			t.Errorf("changed: got %+v", got.Changed)
		}
		if len(got.Expunged) != 1 || got.Expunged[0].Modseq != 51 || !bytesEq(got.Expunged[0].UIDHash, expungedUID[:]) {
			t.Errorf("expunged: got %+v", got.Expunged)
		}
		if got.NewSyncToken != "51" || got.More || got.Stale {
			t.Errorf("token/more/stale: got %q/%v/%v", got.NewSyncToken, got.More, got.Stale)
		}
	})

	t.Run("SyncAddressbookSince_OkStaleFlag", func(t *testing.T) {
		// The `Ok { stale: true }` case (token behind the tombstone-retention
		// window) — distinct from the top-level Stale outcome below.
		caller := &recordingCaller{
			replyBody: syncAddressbookSinceReply{
				Outcome:      string(SyncAddressbookSinceOk),
				NewSyncToken: "77",
				Stale:        true,
			},
		}
		actor := bytes32(0xAA)
		got, err := SyncAddressbookSince(ctx, caller, actor[:], actor[:], "3", 0, "")
		if err != nil {
			t.Fatalf("SyncAddressbookSince: %v", err)
		}
		if got.Outcome != SyncAddressbookSinceOk {
			t.Errorf("outcome: got %q, want ok", got.Outcome)
		}
		if !got.Stale {
			t.Errorf("Ok.stale flag: got false, want true")
		}
		if got.ServerModseq != 0 {
			t.Errorf("Ok must not carry server_modseq: got %d", got.ServerModseq)
		}
	})

	t.Run("SyncAddressbookSince_EmptyTokenNormalizesToZero", func(t *testing.T) {
		caller := &recordingCaller{
			replyBody: syncAddressbookSinceReply{
				Outcome:      string(SyncAddressbookSinceOk),
				NewSyncToken: "100",
			},
		}
		actor := bytes32(0xAA)
		_, err := SyncAddressbookSince(ctx, caller, actor[:], actor[:], "", 0, "")
		if err != nil {
			t.Fatalf("SyncAddressbookSince: %v", err)
		}
		var body2 struct {
			SyncToken string `cbor:"sync_token"`
		}
		if err := cbor.Unmarshal(caller.gotBody, &body2); err != nil {
			t.Fatalf("decode body: %v", err)
		}
		if body2.SyncToken != "0" {
			t.Errorf("empty token MUST normalize to %q on the wire; got %q", "0", body2.SyncToken)
		}
	})

	t.Run("SyncAddressbookSince_AddressbookNotFound", func(t *testing.T) {
		caller := &recordingCaller{
			replyBody: syncAddressbookSinceReply{Outcome: string(SyncAddressbookSinceAddressbookNotFound)},
		}
		actor := bytes32(0xAA)
		got, err := SyncAddressbookSince(ctx, caller, actor[:], actor[:], "0", 0, "")
		if err != nil {
			t.Fatalf("SyncAddressbookSince: %v", err)
		}
		if got.Outcome != SyncAddressbookSinceAddressbookNotFound {
			t.Errorf("outcome: got %q, want addressbook_not_found", got.Outcome)
		}
	})

	t.Run("SyncAddressbookSince_StaleForwardsServerModseqAndMuaID", func(t *testing.T) {
		// Top-level Stale outcome (MUA ahead) — distinct from Ok.stale. The
		// wrapper must surface Stale + ServerModseq and forward mua_id on the
		// wire so nest can log the divergence.
		caller := &recordingCaller{
			replyBody: syncAddressbookSinceReply{
				Outcome:      string(SyncAddressbookSinceStale),
				ServerModseq: 7,
			},
		}
		actor := bytes32(0xAA)
		got, err := SyncAddressbookSince(ctx, caller, actor[:], actor[:], "99", 0, "DAVx5/4.4")
		if err != nil {
			t.Fatalf("SyncAddressbookSince: %v", err)
		}
		if got.Outcome != SyncAddressbookSinceStale {
			t.Errorf("outcome: got %q, want stale", got.Outcome)
		}
		if got.ServerModseq != 7 {
			t.Errorf("ServerModseq: got %d, want 7", got.ServerModseq)
		}
		var body struct {
			MuaID *string `cbor:"mua_id"`
		}
		if err := cbor.Unmarshal(caller.gotBody, &body); err != nil {
			t.Fatalf("decode body: %v", err)
		}
		if body.MuaID == nil || *body.MuaID != "DAVx5/4.4" {
			t.Errorf("mua_id: got %v, want %q", body.MuaID, "DAVx5/4.4")
		}
	})

	t.Run("SyncAddressbookSince_UnknownOutcomeReturnsError", func(t *testing.T) {
		caller := &recordingCaller{replyBody: syncAddressbookSinceReply{Outcome: "future-shape"}}
		actor := bytes32(0xAA)
		_, err := SyncAddressbookSince(ctx, caller, actor[:], actor[:], "0", 0, "")
		if err == nil || !strings.Contains(err.Error(), "future-shape") {
			t.Fatalf("err: %v", err)
		}
	})

	t.Run("SyncAddressbookSince_PropagatesUnderlyingError", func(t *testing.T) {
		caller := &recordingCaller{returnErr: errors.New("synthetic-sync-fail")}
		actor := bytes32(0xAA)
		_, err := SyncAddressbookSince(ctx, caller, actor[:], actor[:], "0", 0, "")
		if err == nil || !strings.Contains(err.Error(), "synthetic-sync-fail") {
			t.Fatalf("err: %v", err)
		}
	})
}

// TestCardDAVNilContainersWireCorrectness pins the Option→pointer nullability
// discipline (see internal/dagcbor/codec.go NilContainersAsEmpty) for the
// CardDAV request cursors: an absent Option<ByteBuf>/Option<String> MUST hit
// the wire as CBOR null (0xf6) or be omitted, never an empty byte string
// (0x40 = Some(empty)) that Rust would decode as Some.
func TestCardDAVNilContainersWireCorrectness(t *testing.T) {
	ctx := context.Background()

	t.Run("QueryCards_NilAfterCardIDStaysNullNotEmptyBytes", func(t *testing.T) {
		caller := &recordingCaller{replyBody: queryCardsReply{Outcome: string(QueryCardsOk)}}
		actor := bytes32(0xA1)
		ab := bytes32(0xB1)
		_, err := QueryCards(ctx, caller, actor[:], ab[:], nil, nil, 0)
		if err != nil {
			t.Fatalf("QueryCards: %v", err)
		}
		assertWireFieldFirstByte(t, caller.gotBody, "after_card_id", 0xf6) // null, not 0x40
	})

	t.Run("QueryCards_NilSinceModseqStaysNull", func(t *testing.T) {
		caller := &recordingCaller{replyBody: queryCardsReply{Outcome: string(QueryCardsOk)}}
		actor := bytes32(0xA2)
		ab := bytes32(0xB2)
		_, err := QueryCards(ctx, caller, actor[:], ab[:], nil, nil, 0)
		if err != nil {
			t.Fatalf("QueryCards: %v", err)
		}
		assertWireFieldFirstByte(t, caller.gotBody, "since_modseq", 0xf6) // null
	})

	t.Run("SyncAddressbookSince_EmptyMuaIDOmitted", func(t *testing.T) {
		caller := &recordingCaller{replyBody: syncAddressbookSinceReply{Outcome: string(SyncAddressbookSinceOk)}}
		actor := bytes32(0xA3)
		ab := bytes32(0xB3)
		_, err := SyncAddressbookSince(ctx, caller, actor[:], ab[:], "0", 0, "")
		if err != nil {
			t.Fatalf("SyncAddressbookSince: %v", err)
		}
		assertWireFieldAbsent(t, caller.gotBody, "mua_id")
	})

	t.Run("PutCardCiphertext_NilIfMatchStaysNull", func(t *testing.T) {
		caller := &recordingCaller{replyBody: putCardCiphertextReply{Outcome: string(PutCardCreated)}}
		actor := bytes32(0xA4)
		_, err := PutCardCiphertext(ctx, caller, actor[:], actor[:], actor[:], []byte("b"), []byte("h"), 1, 1, nil)
		if err != nil {
			t.Fatalf("PutCardCiphertext: %v", err)
		}
		assertWireFieldFirstByte(t, caller.gotBody, "if_match", 0xf6) // null
	})
}

// TestConfigSnapshotCardDAVEnabled decodes the committed reply-fetch-config.cbor
// fixture (Rust-produced, exhaustively populated) through the ConfigSnapshot Go
// mirror and asserts the CardDAV toggle round-trips. The fixture pins
// caldav_enabled=false / caldav_port=9443 / carddav_enabled=true — distinct
// values so a Go-mirror field swap between the three is caught here.
func TestConfigSnapshotCardDAVEnabled(t *testing.T) {
	b, err := os.ReadFile(filepath.Join("testdata", "reply-fetch-config.cbor"))
	if err != nil {
		t.Fatalf("read fixture: %v (run `cargo run -p fauna-protocol "+
			"--example regen_go_wsrpc_reply_fixtures`)", err)
	}
	snap, err := dagcbor.Unmarshal[ConfigSnapshot](b)
	if err != nil {
		t.Fatalf("decode ConfigSnapshot: %v", err)
	}
	if !snap.CardDAVEnabled {
		t.Errorf("CardDAVEnabled: got false, want true (fixture pins carddav_enabled=true)")
	}
	// Guard against a field swap with the two neighbouring CalDAV toggles.
	if snap.CalDAVEnabled {
		t.Errorf("CalDAVEnabled: got true, want false (fixture pins caldav_enabled=false)")
	}
	if snap.CalDAVPort != 9443 {
		t.Errorf("CalDAVPort: got %d, want 9443", snap.CalDAVPort)
	}
}
