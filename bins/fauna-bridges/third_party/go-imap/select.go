package imap

// SelectOptions contains options for the SELECT or EXAMINE command.
type SelectOptions struct {
	ReadOnly  bool
	CondStore bool // requires CONDSTORE

	// QResync carries the SELECT (QRESYNC ...) parameter (RFC 7162
	// §3.2.5), nil unless the client supplied it. Requires QRESYNC.
	//
	// FAUNA-FORK: upstream beta.8's SelectOptions has no QRESYNC field,
	// so `(last_uid_validity, last_modseq, ...)` never reaches
	// Session.Select. This is the parser half of the QRESYNC blocker
	// (FORK.md; the WriteVanished half lives on Expunge/Update writers).
	QResync *SelectQResync
}

// SelectQResync is the parsed SELECT (QRESYNC ...) parameter (RFC 7162
// §3.2.5): `(uidvalidity modseq [known-uids [(seq-match-data)]])`.
//
// FAUNA-FORK: additive type. The Fauna MDA forwards UIDValidity + ModSeq
// to the nest's restore-divergence-detection seam; KnownUIDs is parsed
// for completeness but not forwarded today. The optional seq-match-data
// (4th parameter) is consumed off the wire but not retained.
type SelectQResync struct {
	UIDValidity uint32
	ModSeq      uint64
	KnownUIDs   UIDSet // optional 3rd parameter; nil when absent
}

// SelectData is the data returned by a SELECT command.
//
// In the old RFC 2060, PermanentFlags, UIDNext and UIDValidity are optional.
type SelectData struct {
	// Flags defined for this mailbox
	Flags []Flag
	// Flags that the client can change permanently
	PermanentFlags []Flag
	// Number of messages in this mailbox (aka. "EXISTS")
	NumMessages uint32
	// Sequence number of the first unseen message. Obsolete, IMAP4rev1 only.
	// Server-only, not supported in imapclient.
	FirstUnseenSeqNum uint32
	// Number of recent messages in this mailbox. Obsolete, IMAP4rev1 only.
	// Server-only, not supported in imapclient.
	NumRecent   uint32
	UIDNext     UID
	UIDValidity uint32

	List *ListData // requires IMAP4rev2

	HighestModSeq uint64 // requires CONDSTORE

	// QResync carries the inline SELECT (QRESYNC ...) fast-path output
	// (RFC 7162 §3.2.5): the VANISHED (EARLIER) set + changed-message
	// FETCH responses the server emits *within* the SELECT response,
	// after OK [HIGHESTMODSEQ] and before the tagged OK. nil unless the
	// client supplied a (QRESYNC ...) SELECT parameter and the backend
	// resolved a fast-path delta (common case: last_modseq <=
	// highestmodseq and uidvalidity unchanged).
	//
	// FAUNA-FORK: additive field. Upstream beta.8's handleSelect writes a
	// fixed response sequence with no QRESYNC output, so a reconnecting
	// client cannot receive the inline VANISHED+FETCH and must reconcile
	// via a follow-up `UID FETCH … (CHANGEDSINCE n VANISHED)` (also a
	// fork seam, FetchWriter.WriteVanishedEarlier). This is the inline
	// half of the QRESYNC blocker; see FORK.md.
	QResync *SelectQResyncData // requires QRESYNC
}

// SelectQResyncData is the FAUNA-FORK inline SELECT (QRESYNC ...) output
// (RFC 7162 §3.2.5). Vanished is emitted as one `* VANISHED (EARLIER)
// <uid-set>` response; each Changed entry as a separate
// `* <seq> FETCH (UID <uid> FLAGS (..) MODSEQ (<n>))` response, in the
// order given (callers supply ascending sequence numbers).
type SelectQResyncData struct {
	// Vanished is the set of UIDs tombstoned since the client's
	// last_modseq, read from the backend's expunge log. Empty ⇒ no
	// VANISHED (EARLIER) line is written.
	Vanished UIDSet
	// Changed is the list of messages whose mod-sequence advanced past
	// the client's last_modseq, each emitted as a FLAGS+MODSEQ FETCH.
	Changed []SelectQResyncChange
}

// SelectQResyncChange is one changed-message FETCH line in the inline
// SELECT (QRESYNC ...) fast-path: a message whose flags/mod-sequence
// changed since the client's last_modseq. SeqNum is the message's
// 1-based position in the current mailbox (so the client can correlate
// the FETCH with its local sequence-number view).
type SelectQResyncChange struct {
	SeqNum uint32
	UID    UID
	Flags  []Flag
	ModSeq uint64
}
