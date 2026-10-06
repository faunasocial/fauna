package carddav

import (
	"bytes"
	"context"
	"encoding/hex"
	"errors"
	"fmt"
	"net/http"
	"strings"
	"time"

	"github.com/emersion/go-vcard"
	"github.com/emersion/go-webdav"
	"github.com/emersion/go-webdav/carddav"
	"github.com/faunasocial/fauna/bins/fauna-bridges/internal/mailfauna"
	"github.com/faunasocial/fauna/bins/fauna-bridges/internal/mda/dav"
	"github.com/faunasocial/fauna/bins/fauna-bridges/internal/mda/davauth"
	"github.com/faunasocial/fauna/bins/fauna-bridges/internal/wsrpc"
	"lukechampine.com/blake3"
)

// PutAddressObject lands the CardDAV PUT path (RFC 6352 §6.3.2). Flow:
//
//  1. Re-encode the parsed vcard.Card via emersion's encoder so PUT and the
//     eventual REPORT decrypt agree on byte-stable bodies. The encoder requires
//     a VERSION field and fails without it → 400.
//  2. Enforce FN + UID presence per RFC 6350 (§6.2.1 FN, §6.7.6 UID) — UID is
//     also the canonical filename slug. Missing either → 400.
//  3. uid_hash = blake3(UID)[:32] becomes the canonical filename slug; the
//     client's chosen filename is ignored — we rewrite Location.
//  4. Tokenize the FN/EMAIL/TEL values into a CanonicalTokenSet (encrypted-mode
//     SEARCH-axis index hint).
//  5. SEAL-ALWAYS: HPKE-seal raw body to actor's MLS pubkey; HPKE-seal hint
//     bytes to actor's index pubkey (falling back to MLS pubkey when the actor
//     has no index key yet). Wire bytes never see plaintext, in BOTH storage
//     modes — contacts have no plaintext-at-rest carve-out.
//  6. wsrpc.PutCardCiphertext → outcome dispatch:
//     - Created/Updated → 201 + ETag + Location (emersion hard-codes 201).
//     - PreconditionFailed → 412 with current ETag in body.
//     - AddressbookNotFound → 404.
//
// emersion's carddav.Handler.Put reads `opts.IfMatch` from the `If-Match`
// request header before dispatching here; we don't touch the header directly.
func (b *Backend) PutAddressObject(
	ctx context.Context,
	urlPath string,
	card vcard.Card,
	opts *carddav.PutAddressObjectOptions,
) (*carddav.AddressObject, error) {
	sess, err := b.session(ctx)
	if err != nil {
		return nil, err
	}
	abID, err := parseAddressbookID(sess, urlPath)
	if err != nil {
		return nil, err
	}

	rawBytes, err := encodeVCard(card)
	if err != nil {
		// A missing VERSION (the encoder's only hard requirement) lands here.
		return nil, webdav.NewHTTPError(http.StatusBadRequest,
			fmt.Errorf("re-encode vCard body: %w", err))
	}
	if len(rawBytes) > maxResourceSizeBytes {
		return nil, webdav.NewHTTPError(http.StatusRequestEntityTooLarge,
			fmt.Errorf("vCard body %d bytes exceeds %d cap", len(rawBytes), maxResourceSizeBytes))
	}

	uid, parseErr := extractRequiredCardFields(card)
	if parseErr != nil {
		return nil, parseErr
	}

	uidHashArr := blake3.Sum256([]byte(uid))
	uidHash := make([]byte, 32)
	copy(uidHash, uidHashArr[:])

	tokens := mailfauna.Tokenize(indexableText(card))

	mlsPubkey := sess.MLSPubkey()
	if mlsPubkey == nil {
		return nil, webdav.NewHTTPError(http.StatusInternalServerError,
			errors.New("session missing MLS pubkey"))
	}
	indexKey := sess.IndexKey()
	if indexKey == nil {
		// Actor has no index key provisioned yet — fall back to the MLS pubkey
		// so the recipient's MDA opens both shapes with the same key on REPORT
		// (mirrors the IMAP/CalDAV write path).
		indexKey = mlsPubkey
	}

	// SEAL-ALWAYS. The body seals X-Wing to the actor's two key halves; the
	// index hint does too while its key is the MLS-pubkey fallback
	// (IndexHintMlkemEk), and classically to a dedicated index key. Unlike the CalDAV terminator there
	// is NO plaintext-mode arm — the nest stores ciphertext in both modes.
	encryptedBody, err := mailfauna.EncryptToRecipientHybrid(rawBytes, mlsPubkey, sess.MlkemEk())
	if err != nil {
		return nil, fmt.Errorf("seal card body: %w", err)
	}
	encryptedHint, err := mailfauna.EncryptToRecipientHybrid(
		tokens.CanonicalBytes, indexKey, mailfauna.IndexHintMlkemEk(indexKey, mlsPubkey, sess.MlkemEk()))
	if err != nil {
		return nil, fmt.Errorf("seal card index hint: %w", err)
	}

	ifMatch, err := resolveIfMatch(opts)
	if err != nil {
		return nil, err
	}
	if err := b.refuseIfNoneMatch(ctx, sess, abID, uidHash, opts); err != nil {
		return nil, err
	}

	timestamp := time.Now().Unix()
	result, err := wsrpc.PutCardCiphertext(
		ctx, sess.Client(),
		sess.ActorID(), abID, uidHash,
		encryptedBody, encryptedHint,
		timestamp, uint32(len(encryptedBody)),
		ifMatch,
	)
	if err != nil {
		if dav.IsOverQuota(err) {
			// No room on the account: 507, answered with the
			// DAV:quota-not-exceeded body by dav.QuotaBody.
			return nil, webdav.NewHTTPError(http.StatusInsufficientStorage, err)
		}
		return nil, fmt.Errorf("put_card_ciphertext: %w", err)
	}

	canonicalPath := userBasePath(sess) +
		hex.EncodeToString(abID) + "/" +
		hex.EncodeToString(uidHash) + ".vcf"

	switch result.Outcome {
	case wsrpc.PutCardCreated, wsrpc.PutCardUpdated:
		// card_id / etag / modseq come FROM the reply — never derived locally.
		return &carddav.AddressObject{
			Path:          canonicalPath,
			ModTime:       time.Unix(timestamp, 0),
			ContentLength: int64(len(rawBytes)),
			ETag:          result.ETag,
			Card:          card,
		}, nil
	case wsrpc.PutCardPreconditionFailed:
		return nil, webdav.NewHTTPError(http.StatusPreconditionFailed,
			fmt.Errorf("If-Match mismatch; current etag=%s", result.CurrentETag))
	case wsrpc.PutCardAddressbookNotFound:
		return nil, webdav.NewHTTPError(http.StatusNotFound,
			fmt.Errorf("address book %s not found", hex.EncodeToString(abID)))
	default:
		return nil, fmt.Errorf("put_card_ciphertext: unexpected outcome %q", result.Outcome)
	}
}

// refuseIfNoneMatch honours `If-None-Match` on a card PUT (carddav-server.md §
// Address-book collection model — "Conditional card PUT/DELETE honor
// If-Match/If-None-Match ETags"): `*` refuses a PUT onto a card that already
// exists (the create-only write a contacts app uses so it never clobbers a card
// it has not seen), and an ETag refuses one whose current version carries it.
// Either refusal is 412. Checked here against the stored book because
// put_card_ciphertext carries If-Match only; the window between this read and
// the write is the same one a stock server without a transactional store has.
func (b *Backend) refuseIfNoneMatch(
	ctx context.Context, sess *davauth.Session, abID, uidHash []byte,
	opts *carddav.PutAddressObjectOptions,
) error {
	if opts == nil || !opts.IfNoneMatch.IsSet() {
		return nil
	}
	cards, err := b.fetchAllCards(ctx, sess, abID)
	if err != nil {
		return nil // no book yet ⇒ nothing exists to conflict with
	}
	for i := range cards {
		if !bytes.Equal(cards[i].UIDHash, uidHash) {
			continue
		}
		if opts.IfNoneMatch.IsWildcard() {
			return webdav.NewHTTPError(http.StatusPreconditionFailed,
				errors.New("If-None-Match: * — the card already exists"))
		}
		if etag, err := opts.IfNoneMatch.ETag(); err == nil && etag == cards[i].ETag {
			return webdav.NewHTTPError(http.StatusPreconditionFailed,
				fmt.Errorf("If-None-Match: the card is still at etag %s", etag))
		}
	}
	return nil
}

// encodeVCard re-encodes a parsed vcard.Card back to wire bytes. emersion's
// encoder/decoder pair round-trips byte-stably for the subset CardDAV MUAs
// produce; PUT and the eventual REPORT decrypt share the same encoder so the
// recipient sees byte-identical bodies. Fails if the card lacks a VERSION.
func encodeVCard(card vcard.Card) ([]byte, error) {
	var buf bytes.Buffer
	if err := vcard.NewEncoder(&buf).Encode(card); err != nil {
		return nil, err
	}
	return buf.Bytes(), nil
}

// extractRequiredCardFields enforces FN + UID presence per RFC 6350 (VERSION is
// enforced by the encoder). Returns the UID string on success; a 400 HTTPError
// listing the missing properties otherwise.
func extractRequiredCardFields(card vcard.Card) (string, error) {
	var missing []string
	if strings.TrimSpace(card.Value(vcard.FieldVersion)) == "" {
		missing = append(missing, "VERSION")
	}
	if strings.TrimSpace(card.PreferredValue(vcard.FieldFormattedName)) == "" {
		missing = append(missing, "FN")
	}
	uid := strings.TrimSpace(card.Value(vcard.FieldUID))
	if uid == "" {
		missing = append(missing, "UID")
	}
	if len(missing) > 0 {
		return "", webdav.NewHTTPError(http.StatusBadRequest,
			fmt.Errorf("vCard missing required properties: %s", strings.Join(missing, ", ")))
	}
	return uid, nil
}

// indexableText joins the SEARCH-axis vCard fields (FN, every EMAIL, every TEL)
// into the plaintext the tokenizer hashes for the encrypted index hint. These
// are the fields a MUA's addressbook-query text-matches against; the hint lets
// a future nest-side encrypted search narrow the fetch without decrypting.
func indexableText(card vcard.Card) string {
	var parts []string
	parts = append(parts, card.Values(vcard.FieldFormattedName)...)
	parts = append(parts, card.Values(vcard.FieldEmail)...)
	parts = append(parts, card.Values(vcard.FieldTelephone)...)
	return strings.Join(parts, " ")
}

// resolveIfMatch reduces a `carddav.PutAddressObjectOptions` to the `*string`
// shape `wsrpc.PutCardCiphertext` expects.
//
//   - opts == nil or IfMatch unset → nil (unconditional PUT).
//   - IfMatch "*" → nil (wildcard match-any; nest's put_card_ciphertext has no
//     "must-exist" gate today, so we accept the wart and treat it as
//     unconditional).
//   - IfMatch quoted ETag → unwrap via emersion's `.ETag()`, pass &etag.
//   - Malformed → 400 BadRequest.
//
// (IfNoneMatch — the "don't overwrite" precondition — is not plumbed to nest;
// put_card_ciphertext has no create-only gate today, matching the CalDAV
// terminator's If-Match-only posture.)
func resolveIfMatch(opts *carddav.PutAddressObjectOptions) (*string, error) {
	if opts == nil || !opts.IfMatch.IsSet() {
		return nil, nil
	}
	if opts.IfMatch.IsWildcard() {
		return nil, nil
	}
	etag, err := opts.IfMatch.ETag()
	if err != nil {
		return nil, webdav.NewHTTPError(http.StatusBadRequest,
			fmt.Errorf("malformed If-Match header: %w", err))
	}
	return &etag, nil
}
