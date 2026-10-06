package webdav

import (
	"context"
	"log/slog"
	"net/http"
	"sort"
	"strconv"
	"strings"

	"github.com/faunasocial/fauna/bins/fauna-bridges/internal/auth"
	"github.com/faunasocial/fauna/bins/fauna-bridges/internal/mda/davauth"
	"github.com/faunasocial/fauna/bins/fauna-bridges/internal/wsrpc"
	faunaFfi "github.com/faunasocial/fauna/libs/fauna-mail-go/fauna_ffi"
)

// The bearer door — a third-party principal's read of the folders its
// `fauna:folder:read:<id>` scopes name (`webdav-server.md` § Key model → *A
// principal's read*). Beside the Basic arm on the same mount, it takes RFC
// 9449's `Authorization: DPoP <token>` + the `DPoP` proof(s), and the set's
// content keys in the `Fauna-Folder-Keys` header (rule (3)); it admits the
// token through the nest (`webdav_admit_principal`, rule (6)) and serves
// OPTIONS, PROPFIND, GET and HEAD over `/webdav/{user}/{folder-id}/` — the
// folder's id, the one name both the principal (its scope) and the admission
// (its reply) hold for a set whose row keeps no plaintext name.
//
// Every request is admitted by the nest; nothing is cached. The proof's nonce
// and replay state live on the nest, so the MDA cannot check a proof itself,
// and an admission cached per token would let a request carrying the token
// alone — no proof — ride an honest client's admission.

// bearerScheme is the `Authorization` scheme the door answers (RFC 9449 § 7.1).
const bearerScheme = "DPoP"

// folderKeysHeader is the request header carrying the set's FolderContentKeys,
// canonical CBOR in base64url — fauna_core::folder_keys::FOLDER_KEYS_HEADER.
const folderKeysHeader = "Fauna-Folder-Keys"

// isBearerRequest reports whether `r` presents the bearer door's credential
// (scheme matched case-insensitively, RFC 9110 § 11.1).
func isBearerRequest(r *http.Request) bool {
	_, ok := bearerToken(r)
	return ok
}

func bearerToken(r *http.Request) (string, bool) {
	h := r.Header.Get("Authorization")
	scheme, token, ok := strings.Cut(h, " ")
	if !ok || !strings.EqualFold(scheme, bearerScheme) {
		return "", false
	}
	token = strings.TrimSpace(token)
	return token, token != ""
}

// bearerReadMethod is the door's whole method set (rule (6)); every other
// method is a write, and the write door is deposit, not DAV.
func bearerReadMethod(method string) bool {
	switch method {
	case http.MethodOptions, "PROPFIND", http.MethodGet, http.MethodHead:
		return true
	default:
		return false
	}
}

// authDispatch routes a request to the bearer door when it presents a DPoP
// credential, else to the Basic arm.
func authDispatch(basic, bearer http.Handler) http.Handler {
	return http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		if isBearerRequest(r) {
			bearer.ServeHTTP(w, r)
			return
		}
		basic.ServeHTTP(w, r)
	})
}

// bearerMiddleware admits a principal's request through the nest and hands
// `next` a Session for the admitted account plus the principalView its
// request reaches.
func bearerMiddleware(next http.Handler, client wsrpc.Caller, logger *slog.Logger) http.Handler {
	if logger == nil {
		logger = slog.Default()
	}
	return http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		if !bearerReadMethod(r.Method) {
			http.Error(w, "webdav: the bearer door is read-only", http.StatusForbidden)
			return
		}
		token, _ := bearerToken(r)
		// The proof's `htu`: the request's absolute URL without query or
		// fragment (RFC 9449 § 4.2). The listener terminates TLS, so `https`.
		htu := "https://" + r.Host + r.URL.EscapedPath()

		ctx, cancel := context.WithTimeout(r.Context(), auth.RPCTimeout)
		reply, err := wsrpc.WebdavAdmitPrincipal(ctx, client, token, r.Header.Values("DPoP"), r.Method, htu)
		cancel()
		if err != nil {
			logger.Warn("webdav: bearer admission failed", "err", err)
			http.Error(w, "webdav: admission unavailable", http.StatusServiceUnavailable)
			return
		}
		if reply.Admitted == nil {
			relayRefusal(w, reply)
			return
		}

		view := newPrincipalView(reply.Admitted, r.Header.Get(folderKeysHeader), logger)
		defer view.close()
		sess := davauth.NewSession(client, logger, reply.Admitted.ActorID, "", "")
		defer sess.Close()
		ctx = davauth.ContextWithSession(r.Context(), sess)
		next.ServeHTTP(w, r.WithContext(withPrincipal(ctx, view)))
	})
}

// relayRefusal answers the DAV client with the challenge the nest's own door
// rendered — status, `WWW-Authenticate`, the fresh `DPoP-Nonce` — verbatim.
func relayRefusal(w http.ResponseWriter, reply wsrpc.WebdavAdmitPrincipalReply) {
	status := int(reply.Status)
	if status < 400 || status > 599 {
		status = http.StatusUnauthorized
	}
	if reply.WWWAuthenticate != nil {
		w.Header().Set("WWW-Authenticate", *reply.WWWAuthenticate)
	}
	if reply.DPoPNonce != nil {
		w.Header().Set("DPoP-Nonce", *reply.DPoPNonce)
	}
	http.Error(w, davauth.ErrAuthFailed.Error(), status)
}

// principalView is the bearer door's setSource: the folders the admission
// found live and served, addressed by id, and the keys the request's header
// carried for the one set it names.
type principalView struct {
	// folders maps a folder id (canonical decimal, the path segment) to its
	// set_name_hash — only folders whose grant is live AND which are served now.
	folders map[string][]byte
	// bundle is the header's keys; nil when absent or unparseable, which
	// omits every sealed row and opens no byte (rule (3)).
	bundle *faunaFfi.FfiFolderContentKeys
}

func newPrincipalView(p *wsrpc.WebdavAdmittedPrincipal, keysHeader string, logger *slog.Logger) *principalView {
	v := &principalView{folders: make(map[string][]byte, len(p.Folders))}
	for _, f := range p.Folders {
		if f.GrantLive && f.Served && len(f.NameHash) > 0 {
			v.folders[strconv.FormatInt(f.FolderID, 10)] = f.NameHash
		}
	}
	if keysHeader != "" {
		keys, err := faunaFfi.WebdavDecodeFolderKeysHeader(keysHeader)
		if err != nil {
			// The error names no byte of the header (shared Rust guarantees it).
			logger.Debug("webdav: bearer keys header refused", "err", err)
		} else {
			v.bundle = &keys
		}
	}
	return v
}

func (v *principalView) rootNames(context.Context) ([]string, error) {
	ids := make([]int64, 0, len(v.folders))
	for s := range v.folders {
		id, _ := strconv.ParseInt(s, 10, 64)
		ids = append(ids, id)
	}
	sort.Slice(ids, func(i, j int) bool { return ids[i] < ids[j] })
	out := make([]string, len(ids))
	for i, id := range ids {
		out[i] = strconv.FormatInt(id, 10)
	}
	return out, nil
}

func (v *principalView) served(_ context.Context, set string) (bool, error) {
	_, ok := v.folders[set]
	return ok, nil
}

// nameHash is the admitted folder's hash; nil for a segment outside the view,
// which the served gate has already answered 404.
func (v *principalView) nameHash(set string) []byte {
	return v.folders[set]
}

// keys hands the header's bundle to the one set the request names. Read-only
// by construction: the door has no write path.
func (v *principalView) keys(_ context.Context, set string) (faunaFfi.FfiServedSetKeys, bool, error) {
	if _, ok := v.folders[set]; !ok || v.bundle == nil {
		return faunaFfi.FfiServedSetKeys{}, false, nil
	}
	return faunaFfi.FfiServedSetKeys{SetName: set, ReadOnly: true, Keys: *v.bundle}, true, nil
}

// close zeroizes the header's key bytes at request end.
func (v *principalView) close() {
	if v.bundle == nil {
		return
	}
	clear(v.bundle.Current.Key)
	for i := range v.bundle.Prior {
		clear(v.bundle.Prior[i].Key)
	}
	v.bundle = nil
}

type principalCtxKey struct{}

func withPrincipal(ctx context.Context, v *principalView) context.Context {
	return context.WithValue(ctx, principalCtxKey{}, v)
}

// principalFromContext returns the bearer door's view, or nil on a Basic
// request.
func principalFromContext(ctx context.Context) *principalView {
	v, _ := ctx.Value(principalCtxKey{}).(*principalView)
	return v
}
