package imapserver

import (
	"github.com/emersion/go-imap/v2"
	"github.com/emersion/go-imap/v2/internal/imapwire"
)

// FAUNA-FORK: server-side QUOTA (RFC 9208) wire emission. Upstream
// emersion/go-imap v2 beta.8 ships no server-framework seam for QUOTA —
// no GETQUOTA/GETQUOTAROOT command parser, no Session hook, and no
// `* QUOTA` / `* QUOTAROOT` response writers (the imapclient package has
// the client side, but nothing server-side). This file adds the
// additive server seam; the MDA's nest-backed quota lookup lives in
// `internal/mda/imap/quota.go`. See FORK.md row 17. We only support the
// read side (GETQUOTA / GETQUOTAROOT); SETQUOTA is intentionally absent —
// Fauna quota ceilings are nest configuration, not an IMAP-settable knob.

// QuotaData is the server-side QUOTA response payload for one quota root
// (RFC 9208 §5.1). It mirrors imapclient.QuotaData — duplicated here
// because upstream exports the type only on the client side. When
// upstream lands a server-side QUOTA dispatch, this either retires or
// aligns with whatever upstream picks.
type QuotaData struct {
	Root      string
	Resources map[imap.QuotaResourceType]QuotaResourceData
}

// QuotaResourceData is one resource row of a QUOTA response: the current
// usage and the limit, in the resource's native unit (KiB for STORAGE,
// raw count for MESSAGE — RFC 9208 §3.2).
type QuotaResourceData struct {
	Usage int64
	Limit int64
}

// quotaResourceOrder is the deterministic emission order for QUOTA
// resources. Go map iteration is unordered; emitting in a fixed order
// keeps the wire response stable (and matches the RFC 9208 examples,
// STORAGE before MESSAGE). Resource types absent from the map are
// skipped; any not listed here are appended afterwards in map order.
var quotaResourceOrder = []imap.QuotaResourceType{
	imap.QuotaResourceStorage,
	imap.QuotaResourceMessage,
	imap.QuotaResourceMailbox,
	imap.QuotaResourceAnnotationStorage,
}

// handleGetQuota implements the GETQUOTA command (RFC 9208 §4.2):
//
//	GETQUOTA <quota-root> → * QUOTA <quota-root> (<resource> <usage> <limit> ...)
func (c *Conn) handleGetQuota(dec *imapwire.Decoder) error {
	var root string
	if !dec.ExpectSP() || !dec.ExpectAString(&root) || !dec.ExpectCRLF() {
		return dec.Err()
	}

	if err := c.checkState(imap.ConnStateAuthenticated); err != nil {
		return err
	}

	session, ok := c.session.(SessionQuota)
	if !ok {
		return newClientBugError("QUOTA is not supported")
	}

	data, err := session.GetQuota(root)
	if err != nil {
		return err
	}
	return c.writeQuota(data)
}

// handleGetQuotaRoot implements the GETQUOTAROOT command (RFC 9208 §4.3):
//
//	GETQUOTAROOT <mailbox> → * QUOTAROOT <mailbox> <quota-root>*
//	                         * QUOTA <quota-root> (...)   (one per root)
func (c *Conn) handleGetQuotaRoot(dec *imapwire.Decoder) error {
	var mailbox string
	if !dec.ExpectSP() || !dec.ExpectMailbox(&mailbox) || !dec.ExpectCRLF() {
		return dec.Err()
	}

	if err := c.checkState(imap.ConnStateAuthenticated); err != nil {
		return err
	}

	session, ok := c.session.(SessionQuota)
	if !ok {
		return newClientBugError("QUOTA is not supported")
	}

	roots := session.GetQuotaRoot(mailbox)
	if err := c.writeQuotaRoot(mailbox, roots); err != nil {
		return err
	}
	for _, root := range roots {
		data, err := session.GetQuota(root)
		if err != nil {
			return err
		}
		if err := c.writeQuota(data); err != nil {
			return err
		}
	}
	return nil
}

func (c *Conn) writeQuota(data *QuotaData) error {
	enc := newResponseEncoder(c)
	defer enc.end()

	enc.Atom("*").SP().Atom("QUOTA").SP().String(data.Root).SP()
	listEnc := enc.BeginList()
	written := make(map[imap.QuotaResourceType]struct{}, len(data.Resources))
	writeRes := func(typ imap.QuotaResourceType, res QuotaResourceData) {
		listEnc.Item().Atom(string(typ)).SP().Number64(res.Usage).SP().Number64(res.Limit)
		written[typ] = struct{}{}
	}
	for _, typ := range quotaResourceOrder {
		if res, ok := data.Resources[typ]; ok {
			writeRes(typ, res)
		}
	}
	// Any resource type not in the canonical order (forward-compat) is
	// appended after the known ones.
	for typ, res := range data.Resources {
		if _, done := written[typ]; !done {
			writeRes(typ, res)
		}
	}
	listEnc.End()

	return enc.CRLF()
}

func (c *Conn) writeQuotaRoot(mailbox string, roots []string) error {
	enc := newResponseEncoder(c)
	defer enc.end()

	enc.Atom("*").SP().Atom("QUOTAROOT").SP().Mailbox(mailbox)
	for _, root := range roots {
		enc.SP().String(root)
	}
	return enc.CRLF()
}
