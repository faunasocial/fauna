package atprotopds

import (
	"context"
	"errors"
	"sync"
	"testing"
	"time"

	"github.com/faunasocial/fauna/bins/fauna-bridges/internal/wsrpc"
)

// permissionSetDeliveryRecorder is the nest as the delivery sees it: it
// records every deliver_permission_set call and answers `accepted`.
type permissionSetDeliveryRecorder struct {
	mu       sync.Mutex
	accepted bool
	calls    []deliveredPermissionSet
}

type deliveredPermissionSet struct {
	requestID []byte
	nsid      string
	record    []byte
}

func (r *permissionSetDeliveryRecorder) Call(_ context.Context, method string, body any, reply any) error {
	r.mu.Lock()
	defer r.mu.Unlock()
	if method != wsrpc.MethodAtprotoDeliverPermissionSet {
		return errors.New("unexpected method " + method)
	}
	var req struct {
		RequestID []byte `cbor:"request_id"`
		NSID      string `cbor:"nsid"`
		Record    []byte `cbor:"record"`
	}
	fakeReencode(body, &req)
	r.calls = append(r.calls, deliveredPermissionSet{requestID: req.RequestID, nsid: req.NSID, record: req.Record})
	fakeReencode(struct {
		Accepted bool `cbor:"accepted"`
	}{Accepted: r.accepted}, reply)
	return nil
}

func (r *permissionSetDeliveryRecorder) delivered() []deliveredPermissionSet {
	r.mu.Lock()
	defer r.mu.Unlock()
	return append([]deliveredPermissionSet(nil), r.calls...)
}

// fakeSetResolver records what was asked for and answers from a table.
type fakeSetResolver struct {
	mu    sync.Mutex
	asked []string
	docs  map[string][]byte
	fail  map[string]error
}

func (f *fakeSetResolver) ResolveSetDocument(_ context.Context, nsid string) ([]byte, error) {
	f.mu.Lock()
	defer f.mu.Unlock()
	f.asked = append(f.asked, nsid)
	if err, ok := f.fail[nsid]; ok {
		return nil, err
	}
	if doc, ok := f.docs[nsid]; ok {
		return doc, nil
	}
	return nil, errors.New("no fixture for " + nsid)
}

func (f *fakeSetResolver) askedFor() []string {
	f.mu.Lock()
	defer f.mu.Unlock()
	return append([]string(nil), f.asked...)
}

func TestPermissionSetRequestDeliversTheVerifiedBytesVerbatim(t *testing.T) {
	nest := &permissionSetDeliveryRecorder{accepted: true}
	s := NewServer(nest, nil, nil, nil, nil)
	resolver := &fakeSetResolver{docs: map[string][]byte{
		"com.example.calendar.appPerms": []byte("verified dag-cbor"),
	}}
	s.EnablePermissionSets(resolver)

	s.deliverPermissionSet(context.Background(), []byte("request-id-16-by"), "com.example.calendar.appPerms")

	if got := resolver.askedFor(); len(got) != 1 || got[0] != "com.example.calendar.appPerms" {
		t.Fatalf("resolver asked for %v, want the pushed NSID once", got)
	}
	calls := nest.delivered()
	if len(calls) != 1 {
		t.Fatalf("delivered %d times, want 1", len(calls))
	}
	if string(calls[0].requestID) != "request-id-16-by" || calls[0].nsid != "com.example.calendar.appPerms" {
		t.Fatalf("delivery correlates wrongly: %+v", calls[0])
	}
	if string(calls[0].record) != "verified dag-cbor" {
		t.Fatalf("the record must cross verbatim, got %q", calls[0].record)
	}
}

func TestPermissionSetRequestDeliversARefusalAsNoRecord(t *testing.T) {
	nest := &permissionSetDeliveryRecorder{accepted: true}
	s := NewServer(nest, nil, nil, nil, nil)
	resolver := &fakeSetResolver{fail: map[string]error{
		"com.example.calendar.appPerms": errors.New("the authority's PDS answered 502 at https://pds.example.com"),
	}}
	s.EnablePermissionSets(resolver)

	s.deliverPermissionSet(context.Background(), []byte("request-id-16-by"), "com.example.calendar.appPerms")

	calls := nest.delivered()
	if len(calls) != 1 {
		t.Fatalf("a refusal must still be delivered, so the nest need not wait out its deadline; got %d calls", len(calls))
	}
	if calls[0].record != nil {
		t.Fatalf("a refusal carries no record, got %q", calls[0].record)
	}
}

func TestPermissionSetRequestWithNoPlaneWiredRefusesAtOnce(t *testing.T) {
	nest := &permissionSetDeliveryRecorder{accepted: false}
	s := NewServer(nest, nil, nil, nil, nil)

	s.deliverPermissionSet(context.Background(), []byte("request-id-16-by"), "com.example.calendar.appPerms")

	calls := nest.delivered()
	if len(calls) != 1 || calls[0].record != nil {
		t.Fatalf("no plane wired must deliver one refusal, got %+v", calls)
	}
}

func TestHandlePermissionSetRequestedReturnsWithoutWaitingOnTheResolution(t *testing.T) {
	nest := &permissionSetDeliveryRecorder{accepted: true}
	s := NewServer(nest, nil, nil, nil, nil)
	release := make(chan struct{})
	delivered := make(chan struct{})
	s.EnablePermissionSets(blockingSetResolver{release: release, done: delivered})

	// The handler is called on the WS read loop; it must hand the work off.
	s.HandlePermissionSetRequested(context.Background(), []byte("request-id-16-by"), "com.example.calendar.appPerms")
	if got := nest.delivered(); len(got) != 0 {
		t.Fatalf("nothing can have been delivered before the resolver was released: %+v", got)
	}
	close(release)
	<-delivered
	// The delivery follows the resolver's done signal by one Call on the
	// spawned goroutine; wait for it to land rather than assuming an order.
	deadline := time.After(5 * time.Second)
	for {
		if got := nest.delivered(); len(got) == 1 {
			if string(got[0].record) != "released" {
				t.Fatalf("delivered %q, want the resolver's answer", got[0].record)
			}
			return
		}
		select {
		case <-deadline:
			t.Fatal("the released resolution was never delivered")
		case <-time.After(time.Millisecond):
		}
	}
}

// blockingSetResolver answers only once released, so a test can observe that
// the push handler returned before the resolution finished.
type blockingSetResolver struct {
	release <-chan struct{}
	done    chan<- struct{}
}

func (b blockingSetResolver) ResolveSetDocument(ctx context.Context, _ string) ([]byte, error) {
	select {
	case <-b.release:
	case <-ctx.Done():
		return nil, ctx.Err()
	}
	defer close(b.done)
	return []byte("released"), nil
}
