package atprotoid

import (
	"bytes"
	"context"
	"encoding/json"
	"fmt"
	"io"
	"net/http"
	"strings"
)

// DefaultPLCDirectoryURL is the production PLC directory. Hard-coded on
// purpose: which directory a deployment trusts is not a user/admin choice
// (product invariant — no operator config surface), so there is no config
// knob for it.
//
// PLCDirectoryBaseURL, the accessor every caller resolves the directory
// through, lives in directory_seam.go (e2e flavor, honours the
// FAUNA_ATPROTO_PLC_DIRECTORY_URL harness seam) and directory_seam_absent.go
// (production, always this constant). Convention 15: the seam is compiled out
// of the release bridge, not switched off at run time.
const DefaultPLCDirectoryURL = "https://plc.directory"

// SubmitOperation POSTs a signed PLC operation to `{base}/{did}` as plain
// JSON (the directory's submit format — dag-cbor is only the signing/CID
// encoding, never the HTTP body). Any 2xx is success; anything else returns
// an error carrying the response body text. The genesis special case is
// server-side: the directory itself re-derives the DID from the submitted op
// and rejects a mismatch.
func SubmitOperation(ctx context.Context, httpClient *http.Client, directoryBaseURL, did string, signedOp *PlcOperation) error {
	if signedOp.Sig == "" {
		return fmt.Errorf("refusing to submit an unsigned plc op")
	}
	body, err := json.Marshal(signedOp)
	if err != nil {
		return fmt.Errorf("marshal plc op: %w", err)
	}
	url := strings.TrimRight(directoryBaseURL, "/") + "/" + did
	req, err := http.NewRequestWithContext(ctx, http.MethodPost, url, bytes.NewReader(body))
	if err != nil {
		return fmt.Errorf("build plc submit request: %w", err)
	}
	req.Header.Set("Content-Type", "application/json")
	resp, err := httpClient.Do(req)
	if err != nil {
		return fmt.Errorf("submit plc op to %s: %w", directoryBaseURL, err)
	}
	defer resp.Body.Close()
	if resp.StatusCode/100 != 2 {
		rb, _ := io.ReadAll(io.LimitReader(resp.Body, 4096))
		return fmt.Errorf("plc directory returned %d for %s: %s",
			resp.StatusCode, did, strings.TrimSpace(string(rb)))
	}
	return nil
}

// auditEntry is one row of the directory's `/{did}/log/audit` response. Only
// the three fields the rename path needs are modelled; the directory sends
// more (createdAt, did) and unknown fields are ignored.
type auditEntry struct {
	CID       string          `json:"cid"`
	Operation json.RawMessage `json:"operation"`
	Nullified bool            `json:"nullified"`
}

// FetchLastOp reads the DID's operation log from the directory and returns the
// last operation that still stands, plus its CID — everything a rename needs:
// the AUTHORITATIVE current state to build the next op from, and the `prev`
// link that chains to it.
//
// The directory, not our store, is the source of truth here. A rename must be
// a pure `alsoKnownAs` delta over whatever the log currently says, because the
// ratified custody model (atproto-pds-bridge.md § State & data shape) lets the
// user's client rotate the signing key or the PDS endpoint through the PLC log
// with no involvement from this box. Rebuilding an op from nest's roster view
// would silently revert exactly that.
//
// Nullified entries (ops a more-senior rotation key contested inside PLC's 72h
// window) are skipped: they are no longer part of the chain, so chaining `prev`
// to one would be rejected.
//
// A non-`plc_operation` head (a legacy v0 `create` op) is refused rather than
// coerced: this bridge only ever manages DIDs it minted itself, so seeing one
// means the DID is not ours to update.
func FetchLastOp(ctx context.Context, httpClient *http.Client, directoryBaseURL, did string) (*PlcOperation, string, error) {
	url := strings.TrimRight(directoryBaseURL, "/") + "/" + did + "/log/audit"
	req, err := http.NewRequestWithContext(ctx, http.MethodGet, url, nil)
	if err != nil {
		return nil, "", fmt.Errorf("build plc audit request: %w", err)
	}
	resp, err := httpClient.Do(req)
	if err != nil {
		return nil, "", fmt.Errorf("fetch plc audit log for %s: %w", did, err)
	}
	defer resp.Body.Close()
	if resp.StatusCode/100 != 2 {
		rb, _ := io.ReadAll(io.LimitReader(resp.Body, 4096))
		return nil, "", fmt.Errorf("plc directory returned %d for %s audit log: %s",
			resp.StatusCode, did, strings.TrimSpace(string(rb)))
	}
	var entries []auditEntry
	if err := json.NewDecoder(io.LimitReader(resp.Body, maxAuditLogBytes)).Decode(&entries); err != nil {
		return nil, "", fmt.Errorf("decode plc audit log for %s: %w", did, err)
	}
	for i := len(entries) - 1; i >= 0; i-- {
		e := entries[i]
		if e.Nullified {
			continue
		}
		if e.CID == "" {
			return nil, "", fmt.Errorf("plc audit log entry for %s has no cid", did)
		}
		var op PlcOperation
		if err := json.Unmarshal(e.Operation, &op); err != nil {
			return nil, "", fmt.Errorf("decode plc operation for %s: %w", did, err)
		}
		if op.Type != OpTypeOperation {
			return nil, "", fmt.Errorf("plc log head for %s is a %q op, not %q — not a DID this bridge minted",
				did, op.Type, OpTypeOperation)
		}
		return &op, e.CID, nil
	}
	return nil, "", fmt.Errorf("plc audit log for %s has no standing operation", did)
}

// maxAuditLogBytes bounds the audit-log read. A DID's log is a handful of small
// ops; anything vastly larger is a misbehaving or hostile directory, and this
// response is parsed before anything else validates it.
const maxAuditLogBytes = 1 << 20
