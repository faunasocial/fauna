package imap

import (
	"context"
	"errors"
	"time"

	"github.com/emersion/go-imap/v2"
	"github.com/emersion/go-imap/v2/imapserver"

	"github.com/faunasocial/fauna/bins/fauna-bridges/internal/wsrpc"
)

// listRPCTimeout caps wsrpc round-trips during LIST so a stuck nest
// doesn't hang the IMAP connection. Per imap-server.md § Read surface.
const listRPCTimeout = 30 * time.Second

// listWriter is the seam Session.list dispatches through. Production
// passes *imapserver.ListWriter (whose WriteList satisfies this
// interface); tests pass a fake that captures the writes for
// inspection.
type listWriter interface {
	WriteList(*imap.ListData) error
}

// canonicalMailboxAttr returns the RFC 6154 SPECIAL-USE attribute for
// the standard mailboxes per `imap-server.md` § Standard mailboxes.
// User-created mailboxes return ("", false). INBOX has no SPECIAL-USE
// attribute (RFC 6154; INBOX is identified by name on the wire).
func canonicalMailboxAttr(name string) (imap.MailboxAttr, bool) {
	switch name {
	case "Drafts":
		return imap.MailboxAttrDrafts, true
	case "Sent":
		return imap.MailboxAttrSent, true
	case "Trash":
		return imap.MailboxAttrTrash, true
	case "Junk":
		return imap.MailboxAttrJunk, true
	case "Archive":
		return imap.MailboxAttrArchive, true
	}
	return "", false
}

// List implements imapserver.Session.List for the LIST, LSUB, and
// LIST (SUBSCRIBED) commands. Translates one ListMailboxes RPC
// (plus an optional second RPC for `RETURN (SUBSCRIBED)`), applies
// pattern + reference filtering MDA-side via imapserver.MatchList
// (per RFC 9051 §6.3.9 wildcards), and writes one ListData per
// match. The path separator is always '/' per
// `imap-server.md` § Mailbox model.
//
// LIST-EXTENDED options handled:
//
//   - SelectSubscribed (LSUB / LIST (SUBSCRIBED)): the request goes
//     to nest with `subscribed_only=true`; only mailboxes the actor
//     has SUBSCRIBEd to come back. Each gets the \Subscribed attr.
//   - ReturnSubscribed (LIST RETURN (SUBSCRIBED)): plain LIST plus a
//     second `subscribed_only=true` query builds a name set; each
//     returned mailbox that's in the set gets the \Subscribed attr.
//   - ReturnSpecialUse: the SPECIAL-USE attr lands on every standard
//     mailbox unconditionally (Dovecot-compatible — clients that
//     don't ask still benefit).
func (s *Session) List(w *imapserver.ListWriter, ref string, patterns []string, options *imap.ListOptions) error {
	return s.list(&listWriterAdapter{w: w}, ref, patterns, options)
}

// listWriterAdapter wraps emersion's *imapserver.ListWriter to satisfy
// the listWriter seam. WriteList is the public method on the
// emersion type, so the adapter is a one-call thunk.
type listWriterAdapter struct {
	w *imapserver.ListWriter
}

func (a *listWriterAdapter) WriteList(d *imap.ListData) error {
	return a.w.WriteList(d)
}

// list is the seam-friendly LIST implementation tested in list_test.go.
func (s *Session) list(w listWriter, ref string, patterns []string, options *imap.ListOptions) error {
	s.mu.Lock()
	actorID := s.actorID
	client := s.client
	s.mu.Unlock()
	if actorID == nil {
		return errors.New("imap: LIST requires authenticated state")
	}
	if client == nil {
		return errors.New("imap: LIST has no wsrpc client")
	}

	ctx, cancel := context.WithTimeout(context.Background(), listRPCTimeout)
	defer cancel()

	subscribedOnly := options != nil && options.SelectSubscribed
	mailboxes, err := wsrpc.ListMailboxes(ctx, client, actorID, subscribedOnly)
	if err != nil {
		return err
	}

	// Build the \Subscribed-attr decision set. Three cases:
	//   - subscribedOnly: every result is by construction subscribed.
	//   - ReturnSubscribed without subscribedOnly: do a second RPC
	//     to get the subscription set so we can tag matches.
	//   - neither: no \Subscribed tagging at all.
	subscribedSet, err := s.listResolveSubscribedSet(ctx, client, actorID, options, subscribedOnly, mailboxes)
	if err != nil {
		return err
	}
	tagSubscribed := options != nil && (options.SelectSubscribed || options.ReturnSubscribed)

	for _, m := range mailboxes {
		// Pattern filter (RFC 9051 §6.3.9). Empty pattern list ⇒ no
		// match (LIST "" "" is the canonical "ping" form, returning
		// only the namespace separator response).
		matched := false
		for _, p := range patterns {
			if imapserver.MatchList(m.Name, '/', ref, p) {
				matched = true
				break
			}
		}
		if !matched {
			continue
		}

		attrs := []imap.MailboxAttr{}
		if attr, ok := canonicalMailboxAttr(m.Name); ok {
			attrs = append(attrs, attr)
		}
		if tagSubscribed {
			if _, ok := subscribedSet[m.Name]; ok {
				attrs = append(attrs, imap.MailboxAttrSubscribed)
			}
		}

		if err := w.WriteList(&imap.ListData{
			Mailbox: m.Name,
			Delim:   '/',
			Attrs:   attrs,
		}); err != nil {
			return err
		}
	}
	return nil
}

// listResolveSubscribedSet computes the set of mailbox names that
// the actor has SUBSCRIBEd to, but only when needed (LIST RETURN
// (SUBSCRIBED) with a non-subscribed-only base query). Returns nil
// when no second RPC is needed. When `subscribedOnly` is true every
// member of `mailboxes` is by construction subscribed; the returned
// map covers each.
func (s *Session) listResolveSubscribedSet(
	ctx context.Context,
	client wsrpc.Caller,
	actorID []byte,
	options *imap.ListOptions,
	subscribedOnly bool,
	mailboxes []wsrpc.MailboxEntry,
) (map[string]struct{}, error) {
	if subscribedOnly {
		set := make(map[string]struct{}, len(mailboxes))
		for _, m := range mailboxes {
			set[m.Name] = struct{}{}
		}
		return set, nil
	}
	if options == nil || !options.ReturnSubscribed {
		return nil, nil
	}
	subs, err := wsrpc.ListMailboxes(ctx, client, actorID, true)
	if err != nil {
		return nil, err
	}
	set := make(map[string]struct{}, len(subs))
	for _, m := range subs {
		set[m.Name] = struct{}{}
	}
	return set, nil
}
