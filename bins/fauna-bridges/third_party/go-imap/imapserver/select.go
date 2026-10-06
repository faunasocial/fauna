package imapserver

import (
	"fmt"
	"strings"

	"github.com/emersion/go-imap/v2"
	"github.com/emersion/go-imap/v2/internal/imapwire"
)

func (c *Conn) handleSelect(tag string, dec *imapwire.Decoder, readOnly bool) error {
	var mailbox string
	if !dec.ExpectSP() || !dec.ExpectMailbox(&mailbox) {
		return dec.Err()
	}

	options := imap.SelectOptions{ReadOnly: readOnly}
	// FAUNA-FORK: optional select-params (RFC 4466 / RFC 9051 §6.3.2),
	// e.g. `(CONDSTORE)` (RFC 7162 §3.1.8) or
	// `(QRESYNC (uidvalidity modseq [known-uids [(seq-match-data)]]))`
	// (RFC 7162 §3.2.5). Upstream beta.8 expected CRLF immediately after
	// the mailbox, dropping these parameters.
	if dec.SP() {
		if err := readSelectParams(dec, &options); err != nil {
			return err
		}
	}
	if !dec.ExpectCRLF() {
		return dec.Err()
	}

	if err := c.checkState(imap.ConnStateAuthenticated); err != nil {
		return err
	}

	if c.state == imap.ConnStateSelected {
		if err := c.session.Unselect(); err != nil {
			return err
		}
		c.state = imap.ConnStateAuthenticated
		err := c.writeStatusResp("", &imap.StatusResponse{
			Type: imap.StatusResponseTypeOK,
			Code: "CLOSED",
			Text: "Previous mailbox is now closed",
		})
		if err != nil {
			return err
		}
	}

	data, err := c.session.Select(mailbox, &options)
	if err != nil {
		return err
	}

	if err := c.writeExists(data.NumMessages); err != nil {
		return err
	}
	if !c.enabled.Has(imap.CapIMAP4rev2) && c.server.options.caps().Has(imap.CapIMAP4rev1) {
		if err := c.writeObsoleteRecent(data.NumRecent); err != nil {
			return err
		}
		if data.FirstUnseenSeqNum != 0 {
			if err := c.writeObsoleteUnseen(data.FirstUnseenSeqNum); err != nil {
				return err
			}
		}
	}
	if err := c.writeUIDValidity(data.UIDValidity); err != nil {
		return err
	}
	if err := c.writeUIDNext(data.UIDNext); err != nil {
		return err
	}
	if err := c.writeFlags(data.Flags); err != nil {
		return err
	}
	if err := c.writePermanentFlags(data.PermanentFlags); err != nil {
		return err
	}
	if data.List != nil {
		if err := c.writeList(data.List); err != nil {
			return err
		}
	}
	// FAUNA-FORK: CONDSTORE `OK [HIGHESTMODSEQ <n>]` status response
	// (RFC 7162 §3.1.2.1), emitted once the client has ENABLE CONDSTORE'd
	// and the backend supplied a non-zero highest-modseq.
	if c.enabled.Has(imap.CapCondStore) && data.HighestModSeq != 0 {
		if err := c.writeHighestModSeq(data.HighestModSeq); err != nil {
			return err
		}
	}
	// FAUNA-FORK: inline SELECT (QRESYNC ...) fast-path output (RFC 7162
	// §3.2.5) — `* VANISHED (EARLIER) <set>` then the changed-message
	// `* <seq> FETCH (UID FLAGS MODSEQ)` responses, emitted within the
	// SELECT response (after HIGHESTMODSEQ, before the tagged OK). Only
	// when the client ENABLE QRESYNC'd and the backend resolved a delta
	// (data.QResync != nil). Upstream beta.8 has no QRESYNC SELECT output.
	if c.enabled.Has(imap.CapQResync) && data.QResync != nil {
		if len(data.QResync.Vanished) > 0 {
			if err := c.writeVanished(data.QResync.Vanished, true); err != nil {
				return err
			}
		}
		if len(data.QResync.Changed) > 0 {
			fw := &FetchWriter{conn: c}
			for _, m := range data.QResync.Changed {
				mw := fw.CreateMessage(m.SeqNum)
				mw.WriteUID(m.UID)
				mw.WriteFlags(m.Flags)
				mw.WriteModSeq(m.ModSeq)
				if err := mw.Close(); err != nil {
					return err
				}
			}
		}
	}

	c.state = imap.ConnStateSelected
	// TODO: forbid write commands in read-only mode

	var (
		cmdName string
		code    imap.ResponseCode
	)
	if readOnly {
		cmdName = "EXAMINE"
		code = "READ-ONLY"
	} else {
		cmdName = "SELECT"
		code = "READ-WRITE"
	}
	return c.writeStatusResp(tag, &imap.StatusResponse{
		Type: imap.StatusResponseTypeOK,
		Code: code,
		Text: fmt.Sprintf("%v completed", cmdName),
	})
}

// readSelectParams parses the FAUNA-FORK optional SELECT/EXAMINE
// select-params list `(param [SP param]...)` (RFC 4466). The leading SP
// has already been consumed by the caller. Supported params: `CONDSTORE`
// (RFC 7162 §3.1.8) and `QRESYNC (...)` (RFC 7162 §3.2.5).
func readSelectParams(dec *imapwire.Decoder, options *imap.SelectOptions) error {
	return dec.ExpectList(func() error {
		var name string
		if !dec.ExpectAtom(&name) {
			return dec.Err()
		}
		switch strings.ToUpper(name) {
		case "CONDSTORE":
			options.CondStore = true
			return nil
		case "QRESYNC":
			return readQResyncParam(dec, options)
		default:
			return newClientBugError("unknown SELECT parameter")
		}
	})
}

// readQResyncParam parses the FAUNA-FORK QRESYNC select-param value
// `(uidvalidity modseq [known-uids [(known-seqset known-uidset)]])`
// (RFC 7162 §3.2.5). The `QRESYNC` atom has already been consumed. Only
// uidvalidity + modseq are forwarded to the backend; known-uids is
// retained on SelectQResync, and the optional seq-match-data is consumed
// off the wire but discarded.
func readQResyncParam(dec *imapwire.Decoder, options *imap.SelectOptions) error {
	if !dec.ExpectSP() || !dec.ExpectSpecial('(') {
		return dec.Err()
	}
	var (
		uidValidity uint32
		modSeq      int64
	)
	if !dec.ExpectNumber(&uidValidity) || !dec.ExpectSP() || !dec.ExpectNumber64(&modSeq) {
		return dec.Err()
	}
	qr := &imap.SelectQResync{
		UIDValidity: uidValidity,
		ModSeq:      uint64(modSeq),
	}
	// Optional 3rd parameter: known-uids (a UID set).
	if dec.SP() {
		var knownUIDs imap.UIDSet
		if !dec.ExpectUIDSet(&knownUIDs) {
			return dec.Err()
		}
		qr.KnownUIDs = knownUIDs
		// Optional 4th parameter: seq-match-data =
		// "(" known-sequence-set SP known-uid-set ")". Same numeric-set
		// wire shape on both halves; consumed but not retained.
		if dec.SP() {
			var knownSeqSet, knownUIDSet imap.UIDSet
			if !dec.ExpectSpecial('(') ||
				!dec.ExpectUIDSet(&knownSeqSet) || !dec.ExpectSP() ||
				!dec.ExpectUIDSet(&knownUIDSet) || !dec.ExpectSpecial(')') {
				return dec.Err()
			}
		}
	}
	if !dec.ExpectSpecial(')') {
		return dec.Err()
	}
	options.QResync = qr
	return nil
}

func (c *Conn) handleUnselect(dec *imapwire.Decoder, expunge bool) error {
	if !dec.ExpectCRLF() {
		return dec.Err()
	}

	if err := c.checkState(imap.ConnStateSelected); err != nil {
		return err
	}

	if expunge {
		w := &ExpungeWriter{}
		if err := c.session.Expunge(w, nil); err != nil {
			return err
		}
	}

	if err := c.session.Unselect(); err != nil {
		return err
	}

	c.state = imap.ConnStateAuthenticated
	return nil
}

func (c *Conn) writeExists(numMessages uint32) error {
	enc := newResponseEncoder(c)
	defer enc.end()
	return enc.Atom("*").SP().Number(numMessages).SP().Atom("EXISTS").CRLF()
}

func (c *Conn) writeObsoleteRecent(n uint32) error {
	enc := newResponseEncoder(c)
	defer enc.end()
	return enc.Atom("*").SP().Number(n).SP().Atom("RECENT").CRLF()
}

func (c *Conn) writeObsoleteUnseen(n uint32) error {
	enc := newResponseEncoder(c)
	defer enc.end()
	enc.Atom("*").SP().Atom("OK").SP()
	enc.Special('[').Atom("UNSEEN").SP().Number(n).Special(']')
	enc.SP().Text("First unseen message")
	return enc.CRLF()
}

// writeHighestModSeq writes the FAUNA-FORK CONDSTORE
// `* OK [HIGHESTMODSEQ <n>]` status response (RFC 7162 §3.1.2.1).
func (c *Conn) writeHighestModSeq(modSeq uint64) error {
	enc := newResponseEncoder(c)
	defer enc.end()
	enc.Atom("*").SP().Atom("OK").SP()
	enc.Special('[').Atom("HIGHESTMODSEQ").SP().ModSeq(modSeq).Special(']')
	enc.SP().Text("Highest mod-sequence")
	return enc.CRLF()
}

func (c *Conn) writeUIDValidity(uidValidity uint32) error {
	enc := newResponseEncoder(c)
	defer enc.end()
	enc.Atom("*").SP().Atom("OK").SP()
	enc.Special('[').Atom("UIDVALIDITY").SP().Number(uidValidity).Special(']')
	enc.SP().Text("UIDs valid")
	return enc.CRLF()
}

func (c *Conn) writeUIDNext(uidNext imap.UID) error {
	enc := newResponseEncoder(c)
	defer enc.end()
	enc.Atom("*").SP().Atom("OK").SP()
	enc.Special('[').Atom("UIDNEXT").SP().UID(uidNext).Special(']')
	enc.SP().Text("Predicted next UID")
	return enc.CRLF()
}

func (c *Conn) writeFlags(flags []imap.Flag) error {
	enc := newResponseEncoder(c)
	defer enc.end()
	enc.Atom("*").SP().Atom("FLAGS").SP().List(len(flags), func(i int) {
		enc.Flag(flags[i])
	})
	return enc.CRLF()
}

func (c *Conn) writePermanentFlags(flags []imap.Flag) error {
	enc := newResponseEncoder(c)
	defer enc.end()
	enc.Atom("*").SP().Atom("OK").SP()
	enc.Special('[').Atom("PERMANENTFLAGS").SP().List(len(flags), func(i int) {
		enc.Flag(flags[i])
	}).Special(']')
	enc.SP().Text("Permanent flags")
	return enc.CRLF()
}
