package imap

import (
	"errors"
	"strings"
	"testing"

	faunaFfi "github.com/faunasocial/fauna/libs/fauna-mail-go/fauna_ffi"
)

// A rail failure must cross back into Rust as a *faunaFfi.FfiError. The
// generated wrapper lowers only that type; any other Go error crosses as an
// unexpected-callback error with no payload, and its reason is lost.
func TestContentIndexRail_FailuresCrossAsTypedFfiErrorsWithTheirReason(t *testing.T) {
	rail := newContentIndexRail(failingCaller{}, nil, []byte{1}, nil)

	var ffiErr *faunaFfi.FfiError

	_, err := rail.ListEntries()
	if !errors.As(err, &ffiErr) {
		t.Fatalf("ListEntries: want a *faunaFfi.FfiError, got %T: %v", err, err)
	}
	if !strings.Contains(err.Error(), "simulated RPC failure") {
		t.Fatalf("ListEntries: the reason must survive, got %q", err.Error())
	}

	_, err = rail.FetchBlob("abc")
	if !errors.As(err, &ffiErr) {
		t.Fatalf("FetchBlob: want a *faunaFfi.FfiError, got %T: %v", err, err)
	}
	if !strings.Contains(err.Error(), "no byte-plane client wired") {
		t.Fatalf("FetchBlob: the reason must survive, got %q", err.Error())
	}
}
