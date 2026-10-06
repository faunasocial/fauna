package carddav

import (
	"bytes"
	"context"
	"encoding/hex"
	"errors"
	"fmt"
	"net/http"
	"time"

	"github.com/emersion/go-vcard"
	"github.com/emersion/go-webdav"
	"github.com/emersion/go-webdav/carddav"
	"github.com/faunasocial/fauna/bins/fauna-bridges/internal/mda/davauth"
	"github.com/faunasocial/fauna/bins/fauna-bridges/internal/wsrpc"
)

const (
	// reportPageSize bounds each query_cards reply so a large address book is
	// fetched in chunks that stay well under the bridge's 16 MiB WS read limit.
	// A single unbounded reply could otherwise exceed that frame cap and fail
	// the whole REPORT.
	reportPageSize = 1000
	// maxReportCards caps the total cards one REPORT / PROPFIND / multiget
	// unseals + parses. Past this the result is truncated + logged rather than
	// letting an authenticated client force unbounded unseal/parse work in one
	// request. MUST be a multiple of reportPageSize.
	maxReportCards = 10000
)

// QueryAddressObjects implements emersion's carddav.Backend interface for
// REPORT addressbook-query (RFC 6352 §8.6). nest stores opaque sealed bodies,
// so we fetch every card in the book, HPKE-open in-session, then apply the
// query's prop-filters locally via the exported carddav.Filter matcher (nest
// cannot filter sealed vCards). A nil / empty filter returns every card.
//
// Errors:
//   - AddressbookNotFound → 404.
//   - Per-card decrypt/parse failures are logged + skipped (mirrors
//     ListAddressBooks' "drop one, return the rest" pattern).
func (b *Backend) QueryAddressObjects(
	ctx context.Context,
	urlPath string,
	query *carddav.AddressBookQuery,
) ([]carddav.AddressObject, error) {
	sess, err := b.session(ctx)
	if err != nil {
		return nil, err
	}
	abID, err := parseAddressbookID(sess, urlPath)
	if err != nil {
		return nil, err
	}

	cards, err := b.fetchAllCards(ctx, sess, abID)
	if err != nil {
		return nil, err
	}

	all := make([]carddav.AddressObject, 0, len(cards))
	for i := range cards {
		obj, err := b.openCard(sess, abID, &cards[i])
		if err != nil {
			b.logger.Warn(
				"carddav: skipping unservable card",
				"addressbook_id_hex", hex.EncodeToString(abID),
				"uid_hash_hex", hex.EncodeToString(cards[i].UIDHash),
				"err", err,
			)
			continue
		}
		all = append(all, obj)
	}

	// Apply the addressbook-query prop-filters locally, over the opened
	// plaintext cards — matchable-property filtering over sealed bodies can
	// only happen here (post-decrypt), never nest-side. The filter the client
	// sent is matched by our own RFC 6352 §10.5 matcher (query_filter.go —
	// the library's compares case-sensitively under every collation); a
	// request with no filter body (a depth-1 PROPFIND listing) returns all.
	if q, ok := filterFromContext(ctx); ok {
		return applyAddressbookQuery(q, all), nil
	}
	filtered, err := carddav.Filter(query, all)
	if err != nil {
		return nil, webdav.NewHTTPError(http.StatusBadRequest,
			fmt.Errorf("carddav: apply addressbook-query filter: %w", err))
	}
	return filtered, nil
}

// GetAddressObject implements emersion's carddav.Backend interface for REPORT
// addressbook-multiget (per-href fetch), HEAD/GET, and PROPFIND on a single
// card resource. Path scheme:
//
//	/carddav/{user@domain}/{addressbook_id_hex}/{uid_hash_hex}.vcf
//
// nest has no by-uid_hash query RPC today; we full-fetch the address book and
// pick the matching row.
//
// Errors:
//   - Address book missing → 404 (AddressbookNotFound from query_cards).
//   - Card missing in the book → 404 (a *internal.HTTPError with 404 so
//     emersion's Options/multiget-error paths classify it correctly).
func (b *Backend) GetAddressObject(
	ctx context.Context,
	urlPath string,
	req *carddav.AddressDataRequest,
) (*carddav.AddressObject, error) {
	sess, err := b.session(ctx)
	if err != nil {
		return nil, err
	}
	abID, uidHash, err := parseCardResourcePath(sess, urlPath)
	if err != nil {
		return nil, err
	}

	cards, err := b.fetchAllCards(ctx, sess, abID)
	if err != nil {
		return nil, err
	}

	for i := range cards {
		if !bytes.Equal(cards[i].UIDHash, uidHash) {
			continue
		}
		obj, err := b.openCard(sess, abID, &cards[i])
		if err != nil {
			return nil, fmt.Errorf("open card %s: %w",
				hex.EncodeToString(uidHash), err)
		}
		return &obj, nil
	}
	return nil, webdav.NewHTTPError(http.StatusNotFound,
		fmt.Errorf("card %s not found in address book %s",
			hex.EncodeToString(uidHash), hex.EncodeToString(abID)))
}

// ListAddressObjects implements emersion's carddav.Backend interface for
// PROPFIND with Depth: 1 on an address-book collection. Delegates to
// QueryAddressObjects with a nil query so PROPFIND surfaces the same data the
// REPORT path returns — modern MUAs typically use REPORT but a depth-1 PROPFIND
// must produce the same result set per RFC 4918.
func (b *Backend) ListAddressObjects(
	ctx context.Context,
	urlPath string,
	req *carddav.AddressDataRequest,
) ([]carddav.AddressObject, error) {
	return b.QueryAddressObjects(ctx, urlPath, nil)
}

// fetchAllCards pulls every card in the address book (sinceModseq=nil) via
// bounded pagination: it loops wsrpc.QueryCards in reportPageSize chunks —
// resuming on the prior page's last CardID (nest orders by card_id ASC) — until
// the book is exhausted OR the maxReportCards hard cap is reached. Maps
// AddressbookNotFound to a 404 HTTPError so callers can return it verbatim.
func (b *Backend) fetchAllCards(
	ctx context.Context, sess *davauth.Session, abID []byte,
) ([]wsrpc.CardEntry, error) {
	var (
		acc    []wsrpc.CardEntry
		cursor []byte // afterCardID; nil on the first page
	)
	for {
		res, err := wsrpc.QueryCards(
			ctx, sess.Client(),
			sess.ActorID(), abID,
			nil, cursor, reportPageSize,
		)
		if err != nil {
			return nil, fmt.Errorf("query_cards: %w", err)
		}
		switch res.Outcome {
		case wsrpc.QueryCardsOk:
			// fall through
		case wsrpc.QueryCardsAddressbookNotFound:
			return nil, webdav.NewHTTPError(http.StatusNotFound,
				fmt.Errorf("address book %s not found", hex.EncodeToString(abID)))
		default:
			return nil, fmt.Errorf("query_cards: unexpected outcome %q", res.Outcome)
		}
		acc = append(acc, res.Cards...)
		// Stop on the last page, an empty page (defensive against a
		// non-advancing cursor), or the hard cap.
		if !res.More || len(res.Cards) == 0 {
			break
		}
		if len(acc) >= maxReportCards {
			if len(acc) > maxReportCards {
				acc = acc[:maxReportCards]
			}
			b.logger.Warn(
				"carddav: address book exceeds the REPORT card cap; truncating",
				"addressbook_id_hex", hex.EncodeToString(abID),
				"returned", len(acc),
				"cap", maxReportCards,
			)
			break
		}
		cursor = res.Cards[len(res.Cards)-1].CardID
	}
	return acc, nil
}

// openCard unseals a single CardEntry's encrypted body and decodes it into the
// carddav.AddressObject shape emersion's response writer encodes. Returns a
// per-card decrypt/parse failure; the caller logs + skips (or, on multiget,
// returns it as the resource's error). SEAL-ALWAYS — there is no plaintext
// pass-through arm; the body is always HPKE-opened.
func (b *Backend) openCard(
	sess *davauth.Session,
	abID []byte,
	entry *wsrpc.CardEntry,
) (carddav.AddressObject, error) {
	// Per-session F2 opener, STRICT open — the same strict open every record
	// gets: card bodies rest sealed, so a shape-corrupt blob must error —
	// never pass through verbatim.
	opener := sess.RecordOpener()
	if opener == nil {
		return carddav.AddressObject{}, errors.New(
			"session missing MLS snapshot — user's primary client must " +
				"provision_mls_snapshot_blob before the MDA can HPKE-open card bodies")
	}
	plaintext, err := opener.Open(entry.EncryptedBody)
	if err != nil {
		return carddav.AddressObject{}, fmt.Errorf("open mail record: %w", err)
	}

	card, err := vcard.NewDecoder(bytes.NewReader(plaintext)).Decode()
	if err != nil {
		return carddav.AddressObject{}, fmt.Errorf("decode vCard: %w", err)
	}
	// Pre-validate that `card` RE-ENCODES before handing it to emersion's
	// streaming multistatus writer. go-vcard's encoder requires VERSION and
	// emersion commits the response status before it encodes each
	// <address-data>, so an encode error THERE breaks the ENTIRE
	// REPORT/multiget response. Catch it here so a single malformed stored
	// card is skipped + logged like an undecryptable one (mirrors the CalDAV
	// terminator's pre-encode guard).
	if _, encErr := encodeVCard(card); encErr != nil {
		return carddav.AddressObject{}, fmt.Errorf("re-encode vCard: %w", encErr)
	}

	obj := carddav.AddressObject{
		Path: userBasePath(sess) +
			hex.EncodeToString(abID) + "/" +
			hex.EncodeToString(entry.UIDHash) + ".vcf",
		ContentLength: int64(len(plaintext)),
		ETag:          entry.ETag,
		Card:          card,
	}
	if entry.InternalDate > 0 {
		obj.ModTime = time.Unix(entry.InternalDate, 0).UTC()
	}
	return obj, nil
}
