package atprotorepo

import (
	"bytes"
	"context"
	"encoding/binary"
	"io"

	"github.com/ipfs/go-cid"
)

// A minimal, dependency-free CARv1 writer. We deliberately do NOT pull in
// github.com/ipld/go-car (v1): its root package drags the legacy IPFS stack
// (go-merkledag, go-blockservice, go-ipfs-blockstore, gorm, go-log) into a
// security-sensitive bridge just to frame a header — and adds a go-log v1/v2
// diamond conflict. The CARv1 wire format is tiny and fully specified, and we
// verify our output against an independent parser (go-car/v2's BlockReader,
// already a dependency) plus indigo's own repo.LoadRepoFromCAR in the tests.
//
// CARv1 = <section:header-cbor> <section:block>*
//   section = uvarint(len(payload)) || payload
//   header  = dag-cbor { "roots": [<cid-link>], "version": 1 }
//   block   = raw cid.Bytes() || block-data   (framed as one section)

// carWriter streams a CARv1 to an io.Writer: the header on construction, then
// one section per block as the producer reaches it. It is the ONE owner of the
// framing — the buffered [writeCAR] below is a thin wrapper over it, so a
// streamed CAR and a buffered one are byte-identical by construction rather
// than by two encoders agreeing.
//
// Streaming exists because sync.getRepo's payload is O(live repo) with no page
// ceiling the lexicon would let us apply, so the only place its cost can be
// bounded is HOW it is produced: materialising the whole CAR before the first
// byte made peak resident memory ~2x the repo (a blockstore map plus a buffer
// copy) per in-flight anonymous call. Streaming makes it O(one block).
type carWriter struct {
	w io.Writer
}

// newCARWriter writes the CARv1 header rooted at rootCID and returns the writer
// the caller feeds blocks to.
func newCARWriter(w io.Writer, rootCID cid.Cid) (*carWriter, error) {
	if err := writeSection(w, carHeaderCBOR(rootCID)); err != nil {
		return nil, err
	}
	return &carWriter{w: w}, nil
}

// writeBlock appends one block section (raw CID bytes || block data).
func (c *carWriter) writeBlock(blockCID cid.Cid, raw []byte) error {
	return writeSection(c.w, blockCID.Bytes(), raw)
}

// writeCAR serializes a CARv1 with root [rootCID] into memory, writing exactly
// the listed CIDs from bs in order (commit root first, per the atproto repo
// spec). For a payload whose size is bounded by the caller's own request — a
// record proof, a commit diff, a single commit block — buffering costs nothing
// and keeps the error clean: nothing is written until the whole CAR exists.
// The unbounded case (a whole repo) streams instead; see [Store.ExportRepoTo].
func writeCAR(ctx context.Context, rootCID cid.Cid, bs *memBlockstore, only []cid.Cid) ([]byte, error) {
	var buf bytes.Buffer
	cw, err := newCARWriter(&buf, rootCID)
	if err != nil {
		return nil, err
	}
	for _, c := range only {
		blk, err := bs.Get(ctx, c)
		if err != nil {
			return nil, err
		}
		if err := cw.writeBlock(c, blk.RawData()); err != nil {
			return nil, err
		}
	}
	return buf.Bytes(), nil
}

// writeSection writes uvarint(total len of parts) followed by the parts. This
// is the CARv1 length-delimited framing (unsigned LEB128, the same encoding
// encoding/binary.Uvarint reads).
//
// One Write per section, not per part: a section is the CAR's atom, and on a
// streaming writer that is also what makes "bytes left the process" mean "a
// whole section left the process".
func writeSection(w io.Writer, parts ...[]byte) error {
	total := 0
	for _, p := range parts {
		total += len(p)
	}
	var hdr [binary.MaxVarintLen64]byte
	n := binary.PutUvarint(hdr[:], uint64(total))
	section := make([]byte, 0, n+total)
	section = append(section, hdr[:n]...)
	for _, p := range parts {
		section = append(section, p...)
	}
	_, err := w.Write(section)
	return err
}

// carHeaderCBOR encodes the CARv1 header { "roots": [rootCID], "version": 1 }
// as canonical dag-cbor. Map keys are emitted shortest-first ("roots" before
// "version"), the dag-cbor canonical order.
func carHeaderCBOR(root cid.Cid) []byte {
	var b bytes.Buffer
	b.WriteByte(0xA2) // map(2)
	cborText(&b, "roots")
	b.WriteByte(0x81) // array(1)
	cborCIDLink(&b, root)
	cborText(&b, "version")
	b.WriteByte(0x01) // unsigned int 1
	return b.Bytes()
}

// cborCIDLink writes a dag-cbor CID link: tag 42 wrapping a byte string of
// (0x00 identity-multibase-prefix || cid.Bytes()).
func cborCIDLink(b *bytes.Buffer, c cid.Cid) {
	b.Write([]byte{0xD8, 0x2A}) // tag(42)
	raw := make([]byte, 0, 1+len(c.Bytes()))
	raw = append(raw, 0x00)
	raw = append(raw, c.Bytes()...)
	cborHead(b, 2, uint64(len(raw))) // major 2 = byte string
	b.Write(raw)
}

func cborText(b *bytes.Buffer, s string) {
	cborHead(b, 3, uint64(len(s))) // major 3 = text string
	b.WriteString(s)
}

// cborHead writes a CBOR type header for major type and argument n, using the
// shortest encoding (the dag-cbor requirement).
func cborHead(b *bytes.Buffer, major byte, n uint64) {
	mt := major << 5
	switch {
	case n < 24:
		b.WriteByte(mt | byte(n))
	case n < 1<<8:
		b.WriteByte(mt | 24)
		b.WriteByte(byte(n))
	case n < 1<<16:
		b.WriteByte(mt | 25)
		_ = binary.Write(b, binary.BigEndian, uint16(n))
	case n < 1<<32:
		b.WriteByte(mt | 26)
		_ = binary.Write(b, binary.BigEndian, uint32(n))
	default:
		b.WriteByte(mt | 27)
		_ = binary.Write(b, binary.BigEndian, n)
	}
}
