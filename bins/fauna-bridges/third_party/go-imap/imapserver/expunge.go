package imapserver

import (
	"github.com/emersion/go-imap/v2"
	"github.com/emersion/go-imap/v2/internal/imapwire"
)

func (c *Conn) handleExpunge(dec *imapwire.Decoder) error {
	if !dec.ExpectCRLF() {
		return dec.Err()
	}
	return c.expunge(nil)
}

func (c *Conn) handleUIDExpunge(dec *imapwire.Decoder) error {
	var uidSet imap.UIDSet
	if !dec.ExpectSP() || !dec.ExpectUIDSet(&uidSet) || !dec.ExpectCRLF() {
		return dec.Err()
	}
	return c.expunge(&uidSet)
}

func (c *Conn) expunge(uids *imap.UIDSet) error {
	if err := c.checkState(imap.ConnStateSelected); err != nil {
		return err
	}
	w := &ExpungeWriter{conn: c}
	return c.session.Expunge(w, uids)
}

func (c *Conn) writeExpunge(seqNum uint32) error {
	enc := newResponseEncoder(c)
	defer enc.end()
	enc.Atom("*").SP().Number(seqNum).SP().Atom("EXPUNGE")
	return enc.CRLF()
}

// writeVanished writes the FAUNA-FORK QRESYNC `* VANISHED <uid-set>`
// response, or `* VANISHED (EARLIER) <uid-set>` when earlier is true
// (RFC 7162 §3.2.10). Unlike `OK [MODIFIED ...]`, VANISHED is an
// *untagged* response with no `*imap.Error` status-channel slot, so it
// is emitted directly through a response encoder. Upstream beta.8 has
// no VANISHED writer; this is the QRESYNC last-mile blocker. Tracked by
// FORK.md.
func (c *Conn) writeVanished(uids imap.UIDSet, earlier bool) error {
	enc := newResponseEncoder(c)
	defer enc.end()
	enc.Atom("*").SP().Atom("VANISHED")
	if earlier {
		enc.SP().Special('(').Atom("EARLIER").Special(')')
	}
	enc.SP().NumSet(uids)
	return enc.CRLF()
}

// ExpungeWriter writes EXPUNGE updates.
type ExpungeWriter struct {
	conn *Conn
}

// WriteExpunge notifies the client that the message with the provided sequence
// number has been deleted.
func (w *ExpungeWriter) WriteExpunge(seqNum uint32) error {
	if w.conn == nil {
		return nil
	}
	return w.conn.writeExpunge(seqNum)
}

// WriteVanished notifies the client that the messages with the provided
// UIDs have been permanently removed, via the QRESYNC `* VANISHED
// <uid-set>` response (RFC 7162 §3.2.10). Used in place of per-UID
// WriteExpunge once the client has ENABLE QRESYNC'd. No EARLIER tag —
// these are live (this-session) expunges.
//
// FAUNA-FORK: additive seam — upstream beta.8's ExpungeWriter exposes
// only WriteExpunge(seqNum). Tracked by FORK.md.
func (w *ExpungeWriter) WriteVanished(uids imap.UIDSet) error {
	if w.conn == nil {
		return nil
	}
	return w.conn.writeVanished(uids, false)
}
