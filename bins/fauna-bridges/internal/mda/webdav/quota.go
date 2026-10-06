package webdav

import (
	"bytes"
	"context"
	"encoding/xml"
	"fmt"
	"io"
	"log/slog"
	"net/http"
	"net/url"
	"strconv"
	"strings"
	"time"

	"github.com/faunasocial/fauna/bins/fauna-bridges/internal/mda/davauth"
	"github.com/faunasocial/fauna/bins/fauna-bridges/internal/wsrpc"
)

// RFC 4331 quota reporting (webdav-server.md § Protocol surface (v1) and
// deliberate deferrals, its QUOTA bullet): `quota-used-bytes` and
// `quota-available-bytes` on the mount's collections, from the same storage
// meter and tier ceiling `webdav_record_change` enforces (the `507` refusal
// in backend.go's error map) — so the space a file manager shows is the space
// a save is refused against.
//
// go-webdav's FileSystem has no quota seam, so this middleware sits between
// davauth and the handler and works on the wire:
//
//  1. a PROPFIND whose `<prop>` names a quota property has those elements cut
//     out of the request body before the handler sees it (the handler would
//     otherwise answer them 404);
//  2. the handler's 207 multistatus is kept byte-for-byte, and one extra
//     `<propstat>` carrying the quota values is spliced into each
//     `<response>` for a collection (files get the RFC's 404 for a property
//     they do not have).
//
// `allprop` / `propname` requests pass through untouched: RFC 4331 §3 keeps
// quota properties out of allprop, and they are live properties a propname
// listing need not advertise.

const quotaRPCTimeout = 5 * time.Second

var (
	quotaUsedName      = xml.Name{Space: "DAV:", Local: "quota-used-bytes"}
	quotaAvailableName = xml.Name{Space: "DAV:", Local: "quota-available-bytes"}
)

// quotaMiddleware wraps `next` (the go-webdav handler) with RFC 4331 quota
// reporting. It must run inside davauth: the quota is the AUTH'd actor's.
func quotaMiddleware(next http.Handler, logger *slog.Logger) http.Handler {
	if logger == nil {
		logger = slog.Default()
	}
	return http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		if r.Method != "PROPFIND" || r.Body == nil {
			next.ServeHTTP(w, r)
			return
		}
		body, err := io.ReadAll(r.Body)
		if err != nil {
			http.Error(w, fmt.Sprintf("webdav: read request body: %v", err), http.StatusBadRequest)
			return
		}
		stripped, wanted := stripQuotaProps(body)
		if len(wanted) == 0 {
			r.Body = io.NopCloser(bytes.NewReader(body))
			next.ServeHTTP(w, r)
			return
		}
		r.Body = io.NopCloser(bytes.NewReader(stripped))
		r.ContentLength = int64(len(stripped))

		var quota *wsrpc.WebdavQuota
		if sess := davauth.SessionFromContext(r.Context()); sess != nil {
			ctx, cancel := context.WithTimeout(r.Context(), quotaRPCTimeout)
			q, err := wsrpc.WebdavQuotaFor(ctx, sess.Client(), sess.ActorID())
			cancel()
			if err != nil {
				// A transient failure: the
				// listing still answers, the quota properties read as absent.
				logger.Debug("webdav: quota unavailable", "err", err)
			} else {
				quota = &q
			}
		}

		rec := &bufferedResponse{header: http.Header{}, status: http.StatusOK}
		next.ServeHTTP(rec, r)
		out := rec.body.Bytes()
		if rec.status == http.StatusMultiStatus {
			if spliced, err := spliceQuota(out, wanted, quota); err == nil {
				out = spliced
			} else {
				logger.Debug("webdav: quota splice skipped", "err", err)
			}
		}
		for k, vs := range rec.header {
			if strings.EqualFold(k, "Content-Length") {
				continue
			}
			for _, v := range vs {
				w.Header().Add(k, v)
			}
		}
		w.Header().Set("Content-Length", strconv.Itoa(len(out)))
		w.WriteHeader(rec.status)
		_, _ = w.Write(out)
	})
}

// stripQuotaProps returns the PROPFIND body with every quota property element
// under `propfind/prop` removed, and the quota properties it named (in request
// order). A body naming none — allprop, propname, a malformed body — comes back
// unchanged with no names, so the handler answers it exactly as before.
func stripQuotaProps(body []byte) ([]byte, []xml.Name) {
	dec := xml.NewDecoder(bytes.NewReader(body))
	var (
		stack  []xml.Name
		wanted []xml.Name
		cuts   [][2]int64
	)
	for {
		start := dec.InputOffset()
		tok, err := dec.Token()
		if err == io.EOF {
			break
		}
		if err != nil {
			return body, nil
		}
		switch t := tok.(type) {
		case xml.StartElement:
			if (t.Name == quotaUsedName || t.Name == quotaAvailableName) &&
				len(stack) == 2 &&
				stack[0] == (xml.Name{Space: "DAV:", Local: "propfind"}) &&
				stack[1] == (xml.Name{Space: "DAV:", Local: "prop"}) {
				if err := dec.Skip(); err != nil {
					return body, nil
				}
				cuts = append(cuts, [2]int64{start, dec.InputOffset()})
				wanted = append(wanted, t.Name)
				continue
			}
			stack = append(stack, t.Name)
		case xml.EndElement:
			if len(stack) > 0 {
				stack = stack[:len(stack)-1]
			}
		}
	}
	if len(cuts) == 0 {
		return body, nil
	}
	var out bytes.Buffer
	prev := int64(0)
	for _, c := range cuts {
		out.Write(body[prev:c[0]])
		prev = c[1]
	}
	out.Write(body[prev:])
	return out.Bytes(), wanted
}

// spliceQuota inserts a quota `<propstat>` before the close of every
// top-level `<response>` in a multistatus, keeping every other byte as the
// handler wrote it. Collections get the values (200); a property with no value
// (no ceiling to measure against, or the quota read failed) and every property
// on a non-collection come back 404, as RFC 4918 answers a property a resource
// does not have.
func spliceQuota(ms []byte, wanted []xml.Name, quota *wsrpc.WebdavQuota) ([]byte, error) {
	dec := xml.NewDecoder(bytes.NewReader(ms))
	type insert struct {
		at   int64
		text string
	}
	var (
		inserts []insert
		depth   int
		prefix  string // the prefix this document binds to DAV: ("" = default ns)
		inHref  bool
		href    strings.Builder
	)
	for {
		start := dec.InputOffset()
		tok, err := dec.RawToken()
		if err == io.EOF {
			break
		}
		if err != nil {
			return nil, err
		}
		switch t := tok.(type) {
		case xml.StartElement:
			depth++
			if depth == 1 {
				if t.Name.Local != "multistatus" {
					return nil, fmt.Errorf("not a multistatus: %s", t.Name.Local)
				}
				prefix = t.Name.Space
			}
			if depth == 2 && t.Name.Local == "response" {
				href.Reset()
			}
			if depth == 3 && t.Name.Local == "href" {
				inHref = true
			}
		case xml.CharData:
			if inHref {
				href.Write(t)
			}
		case xml.EndElement:
			if depth == 3 && t.Name.Local == "href" {
				inHref = false
			}
			if depth == 2 && t.Name.Local == "response" {
				inserts = append(inserts, insert{
					at:   start,
					text: quotaPropstat(prefix, wanted, quota, isCollectionHref(href.String())),
				})
			}
			depth--
		}
	}
	var out bytes.Buffer
	prev := int64(0)
	for _, in := range inserts {
		out.Write(ms[prev:in.at])
		out.WriteString(in.text)
		prev = in.at
	}
	out.Write(ms[prev:])
	return out.Bytes(), nil
}

// isCollectionHref reports whether a multistatus href names a collection: the
// mount root, a served folder's root, or any path the handler wrote with a
// trailing slash (its spelling for a directory).
func isCollectionHref(href string) bool {
	h := strings.TrimSpace(href)
	p := h
	if u, err := url.Parse(h); err == nil && u.Path != "" {
		p = u.Path
	}
	if strings.HasSuffix(p, "/") {
		return true
	}
	if dec, err := url.PathUnescape(p); err == nil {
		p = dec
	}
	return parsePath(p).rel == ""
}

// quotaPropstat renders the quota propstat(s) for one response, in the
// document's own DAV: prefix.
func quotaPropstat(prefix string, wanted []xml.Name, quota *wsrpc.WebdavQuota, collection bool) string {
	el := func(local string) string {
		if prefix == "" {
			return local
		}
		return prefix + ":" + local
	}
	var found, missing []string
	for _, n := range wanted {
		val, ok := quotaValue(n, quota)
		if !collection || !ok {
			missing = append(missing, "<"+el(n.Local)+"/>")
			continue
		}
		found = append(found, "<"+el(n.Local)+">"+val+"</"+el(n.Local)+">")
	}
	var b strings.Builder
	write := func(props []string, status string) {
		if len(props) == 0 {
			return
		}
		b.WriteString("<" + el("propstat") + "><" + el("prop") + ">")
		for _, p := range props {
			b.WriteString(p)
		}
		b.WriteString("</" + el("prop") + "><" + el("status") + ">" + status + "</" + el("status") + "></" + el("propstat") + ">")
	}
	write(found, "HTTP/1.1 200 OK")
	write(missing, "HTTP/1.1 404 Not Found")
	return b.String()
}

// quotaValue is one property's value: bytes used, or bytes left under the
// ceiling (never negative — a meter already past its ceiling has 0 left).
func quotaValue(n xml.Name, quota *wsrpc.WebdavQuota) (string, bool) {
	if quota == nil {
		return "", false
	}
	switch n {
	case quotaUsedName:
		return strconv.FormatUint(quota.Used, 10), true
	case quotaAvailableName:
		if quota.Limit == nil {
			return "", false
		}
		if quota.Used >= *quota.Limit {
			return "0", true
		}
		return strconv.FormatUint(*quota.Limit-quota.Used, 10), true
	}
	return "", false
}

// bufferedResponse captures the handler's whole answer so the multistatus can
// be amended before any byte reaches the client.
type bufferedResponse struct {
	header http.Header
	status int
	wrote  bool
	body   bytes.Buffer
}

func (b *bufferedResponse) Header() http.Header { return b.header }

func (b *bufferedResponse) WriteHeader(code int) {
	if !b.wrote {
		b.status = code
		b.wrote = true
	}
}

func (b *bufferedResponse) Write(p []byte) (int, error) {
	b.wrote = true
	return b.body.Write(p)
}
