package imap

import (
	"context"
	"encoding/hex"
	"errors"
	"fmt"
	"time"

	"github.com/emersion/go-imap/v2"
	"github.com/emersion/go-imap/v2/imapserver"

	"github.com/faunasocial/fauna/bins/fauna-bridges/internal/wsrpc"
)

// Compile-time proof that *Session implements imapserver.SessionQuota
// (the FAUNA-FORK server-side QUOTA dispatch interface, see
// third_party/go-imap FORK.md row 17). This is what wires GetQuota /
// GetQuotaRoot below into the GETQUOTA / GETQUOTAROOT command handlers.
var _ imapserver.SessionQuota = (*Session)(nil)

// The QUOTA response payload type now lives in the fork's imapserver
// package (imapserver.QuotaData / QuotaResourceData) — the server-side
// dispatch interface SessionQuota returns it. The MDA used to carry a
// local mirror here (because upstream exported the type only on the
// client side); the fork's server seam supersedes it.

// quotaRPCTimeout caps the get_quota RPC. The handler's SQL is one
// indexed aggregation; a 5 s ceiling matches the other read-side
// MDA RPCs (search_messages, fetch_message_metadata).
const quotaRPCTimeout = 5 * time.Second

// errQuotaPreAuth fires when GetQuota is called on a session whose
// AUTH step has not completed. nest's get_quota handler would reject
// an all-zero actor_id at the malformed-payload gate, but we'd rather
// fail fast here than spend a round-trip discovering the same thing.
var errQuotaPreAuth = errors.New("imap: GETQUOTA before AUTH")

// GetQuota issues `fauna.bridges.get_quota` for the AUTH'd actor and
// translates the reply to RFC 9208 QuotaData.
//
// `root` is the quota-root name the MUA passed (echoed back into the
// reply's Root field). The MDA stores no per-root state — `user/<x>`
// is the single root for every mailbox per imap-server.md § QUOTA —
// so the value flows through verbatim.
//
// Wire translation (RFC 9208 §3.2):
//   - STORAGE usage/limit are in **KiB** (1024 bytes). The handler
//     stores bytes; we round usage UP (a partial KiB still costs the
//     user a slot) and round the limit DOWN (no fractional ceiling).
//   - MESSAGE usage/limit are raw counts.
//
// This is the SessionQuota.GetQuota dispatch hook: the FAUNA-FORK of
// emersion/go-imap (third_party/go-imap, FORK.md row 17) added the
// server-side QUOTA seam (SessionQuota interface + GETQUOTA/GETQUOTAROOT
// parsers + `* QUOTA` writers) that calls this. See imap-server.md
// § QUOTA / § Upstream-blocked gaps.
func (s *Session) GetQuota(root string) (*imapserver.QuotaData, error) {
	s.mu.Lock()
	actor := s.actorID
	s.mu.Unlock()
	if actor == nil {
		return nil, errQuotaPreAuth
	}

	ctx, cancel := context.WithTimeout(context.Background(), quotaRPCTimeout)
	defer cancel()
	reply, err := wsrpc.GetQuota(ctx, s.client, actor)
	if err != nil {
		return nil, fmt.Errorf("imap: GETQUOTA: %w", err)
	}

	return &imapserver.QuotaData{
		Root: root,
		Resources: map[imap.QuotaResourceType]imapserver.QuotaResourceData{
			imap.QuotaResourceStorage: {
				Usage: int64(bytesToKiBCeil(reply.StorageBytesUsed)),
				Limit: int64(reply.StorageBytesLimit / 1024),
			},
			imap.QuotaResourceMessage: {
				Usage: int64(reply.MessageCountUsed),
				Limit: int64(reply.MessageCountLimit),
			},
		},
	}, nil
}

// GetQuotaRoot returns the list of quota roots for `mailbox`. Per
// imap-server.md § Quota root model, every mailbox belongs to a
// single per-actor root `user/<handle>`; this method returns that
// one root.
//
// Handle resolution:
//   - When AUTH captured a localPart (`authedLocalPart`), use it.
//     Common path: the localPart matches `users.handle` in
//     nest's user table.
//   - Otherwise fall back to the first 16 hex characters of the
//     actor_id. Stable across sessions for the same actor;
//     recognizable in logs even if the MUA renders it raw.
//
// Canonical handle resolution from `users.handle` is a Phase F+
// track (the QUOTA wire dispatch itself is already wired via the
// fork's SessionQuota seam — see imap-server.md § QUOTA).
//
// An un-AUTH'd session returns an empty list per RFC 9208 §5
// (server MAY return zero roots when the mailbox is outside any
// known root).
func (s *Session) GetQuotaRoot(_ string) []string {
	s.mu.Lock()
	actor := s.actorID
	localPart := s.authedLocalPart
	s.mu.Unlock()
	if actor == nil {
		return nil
	}
	if localPart != "" {
		return []string{"user/" + localPart}
	}
	return []string{"user/" + hex.EncodeToString(actor[:8])}
}

// bytesToKiBCeil divides bytes by 1024 rounding UP, so a partial KiB
// still counts as one toward the user's quota usage. Defends against
// the off-by-one where a 1-byte message reports as 0 KiB used.
func bytesToKiBCeil(b uint64) uint64 {
	return (b + 1023) / 1024
}

// mapOverQuota translates a nest `fauna.bridges.over_quota` RpcError into
// an IMAP `NO [OVERQUOTA]` status response (RFC 9208 quota *enforcement*);
// any other error — and nil — passes through unchanged. Shared by the
// APPEND / COPY / MOVE write paths so the wire mapping lives in one place.
// See imap-server.md § Quota enforcement points.
func mapOverQuota(err error) error {
	if code, ok := wsrpc.RpcErrorCode(err); ok && code == wsrpc.CodeOverQuota {
		return &imap.Error{
			Type: imap.StatusResponseTypeNo,
			Code: imap.ResponseCodeOverQuota,
			Text: "Mailbox quota exceeded",
		}
	}
	return err
}
