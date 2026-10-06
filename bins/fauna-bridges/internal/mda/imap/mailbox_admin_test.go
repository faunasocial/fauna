package imap

import (
	"context"
	"errors"
	"log/slog"
	"strings"
	"testing"

	"github.com/emersion/go-imap/v2"
	"github.com/fxamacker/cbor/v2"

	"github.com/faunasocial/fauna/bins/fauna-bridges/internal/dagcbor"
	"github.com/faunasocial/fauna/bins/fauna-bridges/internal/wsrpc"
)

// mailboxAdminCaller is the per-file fake for the CREATE / DELETE /
// RENAME paths. Each Session method fans out to exactly one RPC, so
// the caller routes on method and replies with a fixture-supplied
// outcome. By convention, the fake is declared per-file so
// future drift on one test's stub doesn't take the rest of the suite
// with it.
type mailboxAdminCaller struct {
	// fixtures — only one is consulted per Session method call.
	createOutcome map[string]any // {"outcome": "...", "uid_validity"?: N, "reason"?: "..."}
	deleteOutcome map[string]any
	renameOutcome map[string]any

	// error injection — overrides outcome if non-nil.
	createErr error
	deleteErr error
	renameErr error

	// captured request state.
	createCalls    int
	deleteCalls    int
	renameCalls    int
	capturedCreate capturedAdminCreateReq
	capturedDelete capturedAdminDeleteReq
	capturedRename capturedAdminRenameReq
}

type capturedAdminCreateReq struct {
	ActorID []byte `cbor:"actor_id"`
	Name    string `cbor:"name"`
}

type capturedAdminDeleteReq struct {
	ActorID []byte `cbor:"actor_id"`
	Name    string `cbor:"name"`
}

type capturedAdminRenameReq struct {
	ActorID []byte `cbor:"actor_id"`
	OldName string `cbor:"old_name"`
	NewName string `cbor:"new_name"`
}

func (c *mailboxAdminCaller) Call(_ context.Context, method string, body, reply any) error {
	enc, err := dagcbor.Marshal(body)
	if err != nil {
		return err
	}
	switch method {
	case wsrpc.MethodCreateMailbox:
		c.createCalls++
		_ = cbor.Unmarshal(enc, &c.capturedCreate)
		if c.createErr != nil {
			return c.createErr
		}
		rep, err := dagcbor.Marshal(c.createOutcome)
		if err != nil {
			return err
		}
		return cbor.Unmarshal(rep, reply)
	case wsrpc.MethodDeleteMailbox:
		c.deleteCalls++
		_ = cbor.Unmarshal(enc, &c.capturedDelete)
		if c.deleteErr != nil {
			return c.deleteErr
		}
		rep, err := dagcbor.Marshal(c.deleteOutcome)
		if err != nil {
			return err
		}
		return cbor.Unmarshal(rep, reply)
	case wsrpc.MethodRenameMailbox:
		c.renameCalls++
		_ = cbor.Unmarshal(enc, &c.capturedRename)
		if c.renameErr != nil {
			return c.renameErr
		}
		rep, err := dagcbor.Marshal(c.renameOutcome)
		if err != nil {
			return err
		}
		return cbor.Unmarshal(rep, reply)
	}
	return errors.New("mailboxAdminCaller: unexpected method " + method)
}

// adminSession builds a minimally-AUTH'd Session sufficient for the
// mailbox-admin command paths (no MLS unwrap, no MLS pubkey — these
// commands don't touch them).
func adminSession(caller *mailboxAdminCaller) *Session {
	return &Session{
		client:       caller,
		actorID:      fixtureActorID,
		credentialID: fixtureCredentialID,
		logger:       slog.Default(),
	}
}

// expectImapError asserts that `err` is a *imap.Error with the given
// type + text. Returns the error so the caller can `t.Fatal` on a
// fresh line for clearer test output.
func expectImapError(t *testing.T, err error, wantType imap.StatusResponseType, wantText string) {
	t.Helper()
	if err == nil {
		t.Fatalf("expected *imap.Error %q, got nil", wantText)
	}
	var ie *imap.Error
	if !errors.As(err, &ie) {
		t.Fatalf("expected *imap.Error, got %T: %v", err, err)
	}
	if ie.Type != wantType {
		t.Errorf("imap.Error type %v, want %v", ie.Type, wantType)
	}
	if !strings.Contains(ie.Text, wantText) {
		t.Errorf("imap.Error text %q does not contain %q", ie.Text, wantText)
	}
}

// ── CREATE ────────────────────────────────────────────────────────

func TestCreateForwardsActorAndNameToWire(t *testing.T) {
	caller := &mailboxAdminCaller{
		createOutcome: map[string]any{
			"outcome":      "created",
			"uid_validity": uint32(0xDEADBEEF),
		},
	}
	sess := adminSession(caller)

	if err := sess.Create("Projects", nil); err != nil {
		t.Fatalf("Create returned %v, want nil on Created", err)
	}
	if caller.createCalls != 1 {
		t.Fatalf("create_mailbox fired %d times, want 1", caller.createCalls)
	}
	if !equalBytes(caller.capturedCreate.ActorID, fixtureActorID) {
		t.Errorf("actor_id: %x", caller.capturedCreate.ActorID)
	}
	if caller.capturedCreate.Name != "Projects" {
		t.Errorf("name: %q", caller.capturedCreate.Name)
	}
}

func TestCreateAlreadyExistsReturnsImapNo(t *testing.T) {
	caller := &mailboxAdminCaller{
		createOutcome: map[string]any{"outcome": "already_exists"},
	}
	sess := adminSession(caller)
	err := sess.Create("Projects", nil)
	expectImapError(t, err, imap.StatusResponseTypeNo, "already exists")
}

func TestCreateReservedReturnsImapNo(t *testing.T) {
	caller := &mailboxAdminCaller{
		createOutcome: map[string]any{"outcome": "reserved"},
	}
	sess := adminSession(caller)
	err := sess.Create("INBOX", nil)
	expectImapError(t, err, imap.StatusResponseTypeNo, "reserved")
}

func TestCreateInvalidNameReturnsImapBad(t *testing.T) {
	caller := &mailboxAdminCaller{
		createOutcome: map[string]any{
			"outcome": "invalid_name",
			"reason":  "too long",
		},
	}
	sess := adminSession(caller)
	err := sess.Create("X", nil)
	expectImapError(t, err, imap.StatusResponseTypeBad, "too long")
}

func TestCreatePropagatesWireError(t *testing.T) {
	caller := &mailboxAdminCaller{
		createErr: errors.New("kaboom"),
	}
	sess := adminSession(caller)
	if err := sess.Create("X", nil); err == nil || !strings.Contains(err.Error(), "kaboom") {
		t.Fatalf("expected wire error propagated, got %v", err)
	}
}

func TestCreateRejectsUnauthenticatedSession(t *testing.T) {
	caller := &mailboxAdminCaller{}
	sess := &Session{client: caller, logger: slog.Default()} // no actorID
	if err := sess.Create("X", nil); err == nil {
		t.Fatal("expected error on unauthenticated session, got nil")
	}
	if caller.createCalls != 0 {
		t.Errorf("create_mailbox fired %d times on unauthenticated, want 0", caller.createCalls)
	}
}

// ── DELETE ────────────────────────────────────────────────────────

func TestDeleteForwardsActorAndNameToWire(t *testing.T) {
	caller := &mailboxAdminCaller{
		deleteOutcome: map[string]any{"outcome": "deleted"},
	}
	sess := adminSession(caller)

	if err := sess.Delete("Old"); err != nil {
		t.Fatalf("Delete returned %v, want nil on Deleted", err)
	}
	if caller.deleteCalls != 1 {
		t.Fatalf("delete_mailbox fired %d times, want 1", caller.deleteCalls)
	}
	if !equalBytes(caller.capturedDelete.ActorID, fixtureActorID) {
		t.Errorf("actor_id: %x", caller.capturedDelete.ActorID)
	}
	if caller.capturedDelete.Name != "Old" {
		t.Errorf("name: %q", caller.capturedDelete.Name)
	}
}

func TestDeleteNoSuchMailboxReturnsImapNo(t *testing.T) {
	caller := &mailboxAdminCaller{
		deleteOutcome: map[string]any{"outcome": "no_such_mailbox"},
	}
	sess := adminSession(caller)
	err := sess.Delete("Missing")
	expectImapError(t, err, imap.StatusResponseTypeNo, "does not exist")
}

func TestDeleteReservedReturnsImapNo(t *testing.T) {
	caller := &mailboxAdminCaller{
		deleteOutcome: map[string]any{"outcome": "reserved"},
	}
	sess := adminSession(caller)
	err := sess.Delete("INBOX")
	expectImapError(t, err, imap.StatusResponseTypeNo, "reserved")
}

func TestDeleteNotEmptyReturnsImapNo(t *testing.T) {
	caller := &mailboxAdminCaller{
		deleteOutcome: map[string]any{"outcome": "not_empty"},
	}
	sess := adminSession(caller)
	err := sess.Delete("Busy")
	expectImapError(t, err, imap.StatusResponseTypeNo, "not empty")
}

func TestDeletePropagatesWireError(t *testing.T) {
	caller := &mailboxAdminCaller{
		deleteErr: errors.New("kaboom"),
	}
	sess := adminSession(caller)
	if err := sess.Delete("X"); err == nil || !strings.Contains(err.Error(), "kaboom") {
		t.Fatalf("expected wire error propagated, got %v", err)
	}
}

// ── RENAME ────────────────────────────────────────────────────────

func TestRenameForwardsActorOldAndNewToWire(t *testing.T) {
	caller := &mailboxAdminCaller{
		renameOutcome: map[string]any{"outcome": "renamed"},
	}
	sess := adminSession(caller)

	if err := sess.Rename("Old", "New", nil); err != nil {
		t.Fatalf("Rename returned %v, want nil on Renamed", err)
	}
	if caller.renameCalls != 1 {
		t.Fatalf("rename_mailbox fired %d times, want 1", caller.renameCalls)
	}
	if !equalBytes(caller.capturedRename.ActorID, fixtureActorID) {
		t.Errorf("actor_id: %x", caller.capturedRename.ActorID)
	}
	if caller.capturedRename.OldName != "Old" {
		t.Errorf("old_name: %q", caller.capturedRename.OldName)
	}
	if caller.capturedRename.NewName != "New" {
		t.Errorf("new_name: %q", caller.capturedRename.NewName)
	}
}

func TestRenameNoSuchSourceReturnsImapNo(t *testing.T) {
	caller := &mailboxAdminCaller{
		renameOutcome: map[string]any{"outcome": "no_such_source"},
	}
	sess := adminSession(caller)
	err := sess.Rename("Old", "New", nil)
	expectImapError(t, err, imap.StatusResponseTypeNo, "does not exist")
}

func TestRenameReservedSourceReturnsImapNo(t *testing.T) {
	caller := &mailboxAdminCaller{
		renameOutcome: map[string]any{"outcome": "reserved_source"},
	}
	sess := adminSession(caller)
	err := sess.Rename("Trash", "Old-Trash", nil)
	expectImapError(t, err, imap.StatusResponseTypeNo, "reserved and cannot be renamed")
}

func TestRenameTargetReservedReturnsImapNo(t *testing.T) {
	caller := &mailboxAdminCaller{
		renameOutcome: map[string]any{"outcome": "target_reserved"},
	}
	sess := adminSession(caller)
	err := sess.Rename("Foo", "Archive", nil)
	expectImapError(t, err, imap.StatusResponseTypeNo, "Target name is reserved")
}

func TestRenameTargetExistsReturnsImapNo(t *testing.T) {
	caller := &mailboxAdminCaller{
		renameOutcome: map[string]any{"outcome": "target_exists"},
	}
	sess := adminSession(caller)
	err := sess.Rename("Foo", "Bar", nil)
	expectImapError(t, err, imap.StatusResponseTypeNo, "already exists")
}

func TestRenameInvalidNameReturnsImapBad(t *testing.T) {
	caller := &mailboxAdminCaller{
		renameOutcome: map[string]any{
			"outcome": "invalid_name",
			"reason":  "contains NUL",
		},
	}
	sess := adminSession(caller)
	err := sess.Rename("Foo", "bad\x00name", nil)
	expectImapError(t, err, imap.StatusResponseTypeBad, "contains NUL")
}

func TestRenamePropagatesWireError(t *testing.T) {
	caller := &mailboxAdminCaller{
		renameErr: errors.New("kaboom"),
	}
	sess := adminSession(caller)
	if err := sess.Rename("Old", "New", nil); err == nil || !strings.Contains(err.Error(), "kaboom") {
		t.Fatalf("expected wire error propagated, got %v", err)
	}
}
