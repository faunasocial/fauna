package imap

import (
	"context"
	"errors"
	"fmt"
	"time"

	"github.com/emersion/go-imap/v2"

	"github.com/faunasocial/fauna/bins/fauna-bridges/internal/wsrpc"
)

// mailboxAdminRPCTimeout caps the round-trip for CREATE / DELETE /
// RENAME against nest. Each touches at most one mailbox-state row and
// (for DELETE / INBOX-rename) the message-placement table; the worst
// case is an INBOX-rename migrating every placement, but the per-actor
// placement count is already bounded by the quota policy.
const mailboxAdminRPCTimeout = 5 * time.Second

// Create implements emersion/go-imap's Session.Create. Per
// `docs/goal/behavior/imap-server.md` § Write surface, the IMAP wire
// command translates 1:1 to `fauna.bridges.create_mailbox`; the
// reserved-name / already-exists / invalid-name outcomes map back to
// `NO` / `BAD` status responses with the goal-doc-documented text.
//
// CreateOptions (RFC 6154 SPECIAL-USE attributes — `\Archive` etc.)
// are ignored: per the goal doc § Standard mailboxes, SPECIAL-USE
// attributes other than the six standard names are not assigned to
// user-created mailboxes. Quietly accepting the option and dropping
// it is the Dovecot-parity behavior (RFC 6154 §2 lets servers ignore
// unsupported usage).
func (s *Session) Create(mailbox string, _ *imap.CreateOptions) error {
	s.mu.Lock()
	actorID := s.actorID
	s.mu.Unlock()
	if actorID == nil {
		return errors.New("imap: CREATE requires an authenticated session")
	}

	ctx, cancel := context.WithTimeout(context.Background(), mailboxAdminRPCTimeout)
	defer cancel()
	outcome, err := wsrpc.CreateMailbox(ctx, s.client, actorID, mailbox)
	if err != nil {
		return err
	}
	switch outcome.Kind {
	case wsrpc.CreateMailboxOutcomeCreated:
		return nil
	case wsrpc.CreateMailboxOutcomeAlreadyExists:
		return &imap.Error{
			Type: imap.StatusResponseTypeNo,
			Text: "Mailbox already exists",
		}
	case wsrpc.CreateMailboxOutcomeReserved:
		return &imap.Error{
			Type: imap.StatusResponseTypeNo,
			Text: "Mailbox name is reserved",
		}
	case wsrpc.CreateMailboxOutcomeInvalidName:
		return &imap.Error{
			Type: imap.StatusResponseTypeBad,
			Text: "Invalid mailbox name: " + outcome.Reason,
		}
	default:
		return fmt.Errorf("imap: CREATE unknown outcome %q", outcome.Kind)
	}
}

// Delete implements emersion/go-imap's Session.Delete. Maps to
// `fauna.bridges.delete_mailbox`; the non-empty-policy gate
// (`mail.imap.delete_nonempty`) is read server-side from
// `ImapPolicy::default()` — under the default `forbidden` policy a
// non-empty mailbox returns `NO Mailbox is not empty` (RFC 9051
// §6.3.5 lets either stance stand; the safer default per the goal
// doc § Write surface DELETE row).
func (s *Session) Delete(mailbox string) error {
	s.mu.Lock()
	actorID := s.actorID
	s.mu.Unlock()
	if actorID == nil {
		return errors.New("imap: DELETE requires an authenticated session")
	}

	ctx, cancel := context.WithTimeout(context.Background(), mailboxAdminRPCTimeout)
	defer cancel()
	outcome, err := wsrpc.DeleteMailbox(ctx, s.client, actorID, mailbox)
	if err != nil {
		return err
	}
	switch outcome {
	case wsrpc.DeleteMailboxOutcomeDeleted:
		return nil
	case wsrpc.DeleteMailboxOutcomeNoSuchMailbox:
		return &imap.Error{
			Type: imap.StatusResponseTypeNo,
			Text: "Mailbox does not exist",
		}
	case wsrpc.DeleteMailboxOutcomeReserved:
		return &imap.Error{
			Type: imap.StatusResponseTypeNo,
			Text: "Mailbox is reserved and cannot be deleted",
		}
	case wsrpc.DeleteMailboxOutcomeNotEmpty:
		return &imap.Error{
			Type: imap.StatusResponseTypeNo,
			Text: "Mailbox is not empty",
		}
	default:
		return fmt.Errorf("imap: DELETE unknown outcome %q", outcome)
	}
}

// Rename implements emersion/go-imap's Session.Rename. Maps to
// `fauna.bridges.rename_mailbox`; INBOX as `oldName` triggers the
// RFC 9051 §6.3.6 special-case server-side (migrate INBOX contents
// to `newName`, re-seed an empty INBOX). RenameOptions has no
// fields in emersion v2.0.0-beta.8; the parameter is ignored.
func (s *Session) Rename(oldName, newName string, _ *imap.RenameOptions) error {
	s.mu.Lock()
	actorID := s.actorID
	s.mu.Unlock()
	if actorID == nil {
		return errors.New("imap: RENAME requires an authenticated session")
	}

	ctx, cancel := context.WithTimeout(context.Background(), mailboxAdminRPCTimeout)
	defer cancel()
	outcome, err := wsrpc.RenameMailbox(ctx, s.client, actorID, oldName, newName)
	if err != nil {
		return err
	}
	switch outcome.Kind {
	case wsrpc.RenameMailboxOutcomeRenamed:
		return nil
	case wsrpc.RenameMailboxOutcomeNoSuchSource:
		return &imap.Error{
			Type: imap.StatusResponseTypeNo,
			Text: "Source mailbox does not exist",
		}
	case wsrpc.RenameMailboxOutcomeReservedSource:
		return &imap.Error{
			Type: imap.StatusResponseTypeNo,
			Text: "Mailbox is reserved and cannot be renamed",
		}
	case wsrpc.RenameMailboxOutcomeTargetReserved:
		return &imap.Error{
			Type: imap.StatusResponseTypeNo,
			Text: "Target name is reserved",
		}
	case wsrpc.RenameMailboxOutcomeTargetExists:
		return &imap.Error{
			Type: imap.StatusResponseTypeNo,
			Text: "Target mailbox already exists",
		}
	case wsrpc.RenameMailboxOutcomeInvalidName:
		return &imap.Error{
			Type: imap.StatusResponseTypeBad,
			Text: "Invalid mailbox name: " + outcome.Reason,
		}
	default:
		return fmt.Errorf("imap: RENAME unknown outcome %q", outcome.Kind)
	}
}
