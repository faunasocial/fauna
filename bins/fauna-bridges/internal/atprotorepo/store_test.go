package atprotorepo

import (
	"context"
	"testing"
)

// TestProjectionStateDefaults: a DID with no row reads as the zero cursor with
// the first-emit gate ON (matching the column defaults), so the loop never
// special-cases first sight.
func TestProjectionStateDefaults(t *testing.T) {
	ctx := context.Background()
	st, err := Open(":memory:")
	if err != nil {
		t.Fatal(err)
	}
	defer st.Close()

	state, err := st.ProjectionState(ctx, "did:web:alice.example")
	if err != nil {
		t.Fatal(err)
	}
	if state.LastCreatedAtMicros != 0 || state.LastPostID != "" {
		t.Errorf("fresh cursor = (%d,%q), want (0,\"\")", state.LastCreatedAtMicros, state.LastPostID)
	}
	if !state.FirstEmitGated {
		t.Error("fresh DID should read first-emit gated")
	}
}

// TestProjectionWatermarkAdvanceAndGate: the watermark upserts independently of
// the gate, and clearing the gate leaves the watermark intact.
func TestProjectionWatermarkAdvanceAndGate(t *testing.T) {
	ctx := context.Background()
	st, err := Open(":memory:")
	if err != nil {
		t.Fatal(err)
	}
	defer st.Close()
	did := "did:plc:alice000000000000000000000"

	// Advance the watermark (creates the row; gate stays at its default ON).
	if err := st.SetProjectionWatermark(ctx, did, 1234, "aa"); err != nil {
		t.Fatal(err)
	}
	state, _ := st.ProjectionState(ctx, did)
	if state.LastCreatedAtMicros != 1234 || state.LastPostID != "aa" {
		t.Errorf("watermark = (%d,%q), want (1234,\"aa\")", state.LastCreatedAtMicros, state.LastPostID)
	}
	if !state.FirstEmitGated {
		t.Error("watermark advance must not clear the gate")
	}

	// Advance again — upsert, not a second row.
	if err := st.SetProjectionWatermark(ctx, did, 5678, "bb"); err != nil {
		t.Fatal(err)
	}
	state, _ = st.ProjectionState(ctx, did)
	if state.LastCreatedAtMicros != 5678 || state.LastPostID != "bb" {
		t.Errorf("watermark = (%d,%q), want (5678,\"bb\")", state.LastCreatedAtMicros, state.LastPostID)
	}

	// Clear the gate — watermark survives.
	if err := st.SetFirstEmitGated(ctx, did, false); err != nil {
		t.Fatal(err)
	}
	state, _ = st.ProjectionState(ctx, did)
	if state.FirstEmitGated {
		t.Error("gate not cleared")
	}
	if state.LastCreatedAtMicros != 5678 || state.LastPostID != "bb" {
		t.Errorf("clearing the gate disturbed the watermark: (%d,%q)", state.LastCreatedAtMicros, state.LastPostID)
	}
}

// TestSetFirstEmitGatedCreatesRow: gating a DID that has no watermark yet creates
// the row (so a not-yet-projected identity can carry the gate).
func TestSetFirstEmitGatedCreatesRow(t *testing.T) {
	ctx := context.Background()
	st, err := Open(":memory:")
	if err != nil {
		t.Fatal(err)
	}
	defer st.Close()
	did := "did:web:bob.example"

	if err := st.SetFirstEmitGated(ctx, did, false); err != nil {
		t.Fatal(err)
	}
	state, _ := st.ProjectionState(ctx, did)
	if state.FirstEmitGated {
		t.Error("gate should read cleared after SetFirstEmitGated(false)")
	}
}

// TestPublishedHandleRoundTrip: the rename detector's cache reads back, is
// independent of the watermark and the gate (each setter owns its own column),
// and defaults to "" — the adopt-silently signal (D-s3-6).
func TestPublishedHandleRoundTrip(t *testing.T) {
	ctx := context.Background()
	st, err := Open(":memory:")
	if err != nil {
		t.Fatal(err)
	}
	defer st.Close()
	did := "did:plc:abc"

	state, err := st.ProjectionState(ctx, did)
	if err != nil {
		t.Fatal(err)
	}
	if state.PublishedHandle != "" {
		t.Errorf("fresh published handle = %q, want empty (never observed)", state.PublishedHandle)
	}

	if err := st.SetPublishedHandle(ctx, did, "alice.example.com"); err != nil {
		t.Fatal(err)
	}
	if err := st.SetProjectionWatermark(ctx, did, 42, "post-1"); err != nil {
		t.Fatal(err)
	}
	if err := st.SetFirstEmitGated(ctx, did, false); err != nil {
		t.Fatal(err)
	}
	state, err = st.ProjectionState(ctx, did)
	if err != nil {
		t.Fatal(err)
	}
	if state.PublishedHandle != "alice.example.com" {
		t.Errorf("published handle = %q", state.PublishedHandle)
	}
	if state.LastCreatedAtMicros != 42 || state.LastPostID != "post-1" || state.FirstEmitGated {
		t.Errorf("SetPublishedHandle disturbed the other columns: %+v", state)
	}

	// A rename overwrites in place.
	if err := st.SetPublishedHandle(ctx, did, "renamed.example.com"); err != nil {
		t.Fatal(err)
	}
	state, _ = st.ProjectionState(ctx, did)
	if state.PublishedHandle != "renamed.example.com" || state.LastPostID != "post-1" {
		t.Errorf("after rename: %+v", state)
	}
}
