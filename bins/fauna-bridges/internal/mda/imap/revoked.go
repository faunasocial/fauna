package imap

import (
	"context"

	"github.com/emersion/go-imap/v2"
	"github.com/faunasocial/fauna/bins/fauna-bridges/internal/wsrpc"
)

// revokedByeCaller is the per-session wsrpc.Caller every IMAP Session talks to
// the nest through. It changes one thing: a call the nest refuses with
// [wsrpc.CodeAccountRevoked] — the AUTH'd account was locked or suspended
// after the session signed in — comes back as an error that is also an
// IMAP `BYE`, so the command that met the refusal ends the session instead of
// answering `NO` and leaving the connection open
// (`docs/goal/architecture/transport-connection.md` § Revocation teardown, the
// bridge-held sessions paragraph: the sessions that already exist close).
//
// The nest refuses every IMAP serving call for such an account
// (`require_local_mail_serving`), so the session reads nothing after the lock
// either way; the BYE is what makes the disconnect explicit to the mail app.
// A session that makes no call — one in IDLE, or one between commands — is
// ended at the lock itself by the nest's `account_revoked` push
// (revocation_teardown.go).
type revokedByeCaller struct {
	inner wsrpc.Caller
}

func (c revokedByeCaller) Call(ctx context.Context, method string, body any, reply any) error {
	err := c.inner.Call(ctx, method, body, reply)
	if err == nil {
		return nil
	}
	if code, ok := wsrpc.RpcErrorCode(err); ok && code == wsrpc.CodeAccountRevoked {
		return &accountRevokedError{cause: err}
	}
	return err
}

// accountRevokedError keeps the nest's refusal reachable (errors.As on a
// *wsrpc.ServerError, wsrpc.RpcErrorCode) while presenting itself to the IMAP
// server as a BYE status response.
type accountRevokedError struct {
	cause error
}

func (e *accountRevokedError) Error() string { return "imap: account locked or suspended" }

func (e *accountRevokedError) Unwrap() error { return e.cause }

// As lets errors.As(err, **imap.Error) find the BYE, which is what the IMAP
// server keys its close on.
func (e *accountRevokedError) As(target any) bool {
	t, ok := target.(**imap.Error)
	if !ok {
		return false
	}
	*t = &imap.Error{
		Type: imap.StatusResponseTypeBye,
		Text: "This account is locked or suspended",
	}
	return true
}
