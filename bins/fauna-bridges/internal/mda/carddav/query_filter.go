package carddav

import (
	"bytes"
	"context"
	"encoding/xml"
	"io"
	"net/http"
	"strings"

	"github.com/emersion/go-vcard"
	"github.com/emersion/go-webdav/carddav"
)

// addressbook-query filtering (RFC 6352 §10.5), done here rather than by the DAV
// library's matcher: that matcher compared text case-sensitively whatever
// collation the client named (probed over the wire — `report` never found
// "Report" even under `i;unicode-casemap`, the RFC's case-insensitive default),
// so a contacts app searching a name in lower case found nothing
// (carddav-server.md § Address-book collection model — "addressbook-query
// REPORT (property + text-match filters)"). The filter is read from the REPORT
// body the client sent, which the middleware below keeps for the backend, and
// matched against the opened cards — the only place sealed vCards can be
// searched at all.

// maxQueryBody bounds how much of a REPORT body is kept for filtering; an
// addressbook-query filter is a few hundred bytes.
const maxQueryBody = 1 << 20

type reportBodyCtxKey struct{}

// reportBodyMiddleware keeps a REPORT's body in the request context (and hands
// the handler an identical copy), so QueryAddressObjects can read the filter
// exactly as the client wrote it.
func reportBodyMiddleware(next http.Handler) http.Handler {
	return http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		if r.Method != "REPORT" || r.Body == nil {
			next.ServeHTTP(w, r)
			return
		}
		body, err := io.ReadAll(io.LimitReader(r.Body, maxQueryBody+1))
		if err != nil || len(body) > maxQueryBody {
			http.Error(w, "carddav: REPORT body unreadable or too large", http.StatusBadRequest)
			return
		}
		r.Body = io.NopCloser(bytes.NewReader(body))
		next.ServeHTTP(w, r.WithContext(context.WithValue(r.Context(), reportBodyCtxKey{}, body)))
	})
}

type abQueryXML struct {
	XMLName xml.Name     `xml:"urn:ietf:params:xml:ns:carddav addressbook-query"`
	Filter  *abFilterXML `xml:"urn:ietf:params:xml:ns:carddav filter"`
	Limit   *struct {
		NResults int `xml:"urn:ietf:params:xml:ns:carddav nresults"`
	} `xml:"urn:ietf:params:xml:ns:carddav limit"`
}

type abFilterXML struct {
	Test        string            `xml:"test,attr"`
	PropFilters []abPropFilterXML `xml:"urn:ietf:params:xml:ns:carddav prop-filter"`
}

type abPropFilterXML struct {
	Name         string             `xml:"name,attr"`
	Test         string             `xml:"test,attr"`
	IsNotDefined *struct{}          `xml:"urn:ietf:params:xml:ns:carddav is-not-defined"`
	TextMatches  []abTextMatchXML   `xml:"urn:ietf:params:xml:ns:carddav text-match"`
	ParamFilters []abParamFilterXML `xml:"urn:ietf:params:xml:ns:carddav param-filter"`
}

type abParamFilterXML struct {
	Name         string          `xml:"name,attr"`
	IsNotDefined *struct{}       `xml:"urn:ietf:params:xml:ns:carddav is-not-defined"`
	TextMatch    *abTextMatchXML `xml:"urn:ietf:params:xml:ns:carddav text-match"`
}

type abTextMatchXML struct {
	Collation string `xml:"collation,attr"`
	MatchType string `xml:"match-type,attr"`
	Negate    string `xml:"negate-condition,attr"`
	Text      string `xml:",chardata"`
}

// filterFromContext parses the addressbook-query the client sent, or returns
// ok=false when the request carried none (a PROPFIND listing, a multiget).
func filterFromContext(ctx context.Context) (abQueryXML, bool) {
	body, _ := ctx.Value(reportBodyCtxKey{}).([]byte)
	if len(body) == 0 {
		return abQueryXML{}, false
	}
	var q abQueryXML
	if err := xml.Unmarshal(body, &q); err != nil || q.XMLName.Space != nsCardDAV {
		return abQueryXML{}, false
	}
	return q, true
}

// applyAddressbookQuery returns the cards the query selects, in order, capped
// at its nresults limit.
func applyAddressbookQuery(q abQueryXML, cards []carddav.AddressObject) []carddav.AddressObject {
	out := make([]carddav.AddressObject, 0, len(cards))
	for _, c := range cards {
		if q.Filter == nil || matchFilter(*q.Filter, c.Card) {
			out = append(out, c)
		}
		if q.Limit != nil && q.Limit.NResults > 0 && len(out) >= q.Limit.NResults {
			break
		}
	}
	return out
}

// matchFilter: the card matches when any (anyof, the default) or every
// (allof) prop-filter does; an empty filter matches every card.
func matchFilter(f abFilterXML, card vcard.Card) bool {
	if len(f.PropFilters) == 0 {
		return true
	}
	return combine(f.Test, len(f.PropFilters), func(i int) bool {
		return matchPropFilter(f.PropFilters[i], card)
	})
}

func matchPropFilter(pf abPropFilterXML, card vcard.Card) bool {
	fields := card[strings.ToUpper(pf.Name)]
	if pf.IsNotDefined != nil {
		return len(fields) == 0
	}
	if len(fields) == 0 {
		return false
	}
	tests := len(pf.TextMatches) + len(pf.ParamFilters)
	if tests == 0 {
		return true
	}
	return combine(pf.Test, tests, func(i int) bool {
		if i < len(pf.TextMatches) {
			tm := pf.TextMatches[i]
			for _, f := range fields {
				if matchText(tm, f.Value) {
					return true
				}
			}
			return false
		}
		pr := pf.ParamFilters[i-len(pf.TextMatches)]
		for _, f := range fields {
			if matchParamFilter(pr, f) {
				return true
			}
		}
		return false
	})
}

func matchParamFilter(pr abParamFilterXML, f *vcard.Field) bool {
	values := f.Params[strings.ToUpper(pr.Name)]
	if pr.IsNotDefined != nil {
		return len(values) == 0
	}
	if len(values) == 0 {
		return false
	}
	if pr.TextMatch == nil {
		return true
	}
	for _, v := range values {
		if matchText(*pr.TextMatch, v) {
			return true
		}
	}
	return false
}

// matchText applies one text-match: `i;octet` compares bytes exactly; the
// casemap collations (and an omitted collation, whose default is
// `i;unicode-casemap`) compare case-insensitively. match-type defaults to
// `contains`; `negate-condition="yes"` inverts the result.
func matchText(tm abTextMatchXML, value string) bool {
	text := strings.TrimSpace(tm.Text)
	if !strings.EqualFold(tm.Collation, "i;octet") {
		text, value = strings.ToLower(text), strings.ToLower(value)
	}
	var hit bool
	switch strings.ToLower(tm.MatchType) {
	case "equals":
		hit = value == text
	case "starts-with":
		hit = strings.HasPrefix(value, text)
	case "ends-with":
		hit = strings.HasSuffix(value, text)
	default:
		hit = strings.Contains(value, text)
	}
	if strings.EqualFold(tm.Negate, "yes") {
		return !hit
	}
	return hit
}

// combine evaluates n tests under an RFC 6352 `test` attribute: "allof" needs
// every test, anything else (the default "anyof") needs one.
func combine(test string, n int, ok func(int) bool) bool {
	all := strings.EqualFold(test, "allof")
	for i := 0; i < n; i++ {
		if ok(i) != all {
			return !all
		}
	}
	return all
}
