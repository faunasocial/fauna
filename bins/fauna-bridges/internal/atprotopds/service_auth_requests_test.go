package atprotopds

import (
	"context"
	"errors"
	"sync"
	"testing"
	"time"

	"github.com/faunasocial/fauna/bins/fauna-bridges/internal/atprotoid"
	"github.com/faunasocial/fauna/bins/fauna-bridges/internal/wsrpc"
)

// serviceAuthDeliveryRecorder is the nest as the delivery sees it: it records
// every deliver_service_auth call and answers `accepted`.
type serviceAuthDeliveryRecorder struct {
	mu       sync.Mutex
	accepted bool
	calls    []deliveredServiceAuth
}

type deliveredServiceAuth struct {
	requestID []byte
	token     *string
}

func (r *serviceAuthDeliveryRecorder) Call(_ context.Context, method string, body any, reply any) error {
	r.mu.Lock()
	defer r.mu.Unlock()
	if method != wsrpc.MethodAtprotoDeliverServiceAuth {
		return errors.New("unexpected method " + method)
	}
	var req struct {
		RequestID []byte  `cbor:"request_id"`
		Token     *string `cbor:"token"`
	}
	fakeReencode(body, &req)
	r.calls = append(r.calls, deliveredServiceAuth{requestID: req.RequestID, token: req.Token})
	fakeReencode(struct {
		Accepted bool `cbor:"accepted"`
	}{Accepted: r.accepted}, reply)
	return nil
}

func (r *serviceAuthDeliveryRecorder) delivered() []deliveredServiceAuth {
	r.mu.Lock()
	defer r.mu.Unlock()
	return append([]deliveredServiceAuth(nil), r.calls...)
}

func servicePush(exp int64) ServiceAuthPush {
	return ServiceAuthPush{
		RequestID: []byte("request-id-16-by"),
		ActorID:   []byte("actor"),
		DID:       "did:plc:oracleaccount",
		Aud:       "did:web:feed.example#bsky_fg",
		Lxm:       "app.bsky.feed.getFeedSkeleton",
		Exp:       exp,
	}
}

// The nest decided; the bridge mints with the account's own repo key — the
// token verifies against it and carries exactly the pushed iss/aud/lxm and
// the pushed exp — and delivers it under the pushed request id.
func TestServiceAuthRequestMintsWithTheRepoKeyAndDeliversTheToken(t *testing.T) {
	nest := &serviceAuthDeliveryRecorder{accepted: true}
	signer, didKey := testSigner(t)
	s := NewServer(nest, nil, nil, &stubSigners{signer: signer}, nil)
	exp := time.Now().Add(30 * time.Second).Unix()

	s.deliverServiceAuth(context.Background(), servicePush(exp))

	calls := nest.delivered()
	if len(calls) != 1 || calls[0].token == nil {
		t.Fatalf("want one delivered token, got %+v", calls)
	}
	if string(calls[0].requestID) != "request-id-16-by" {
		t.Fatalf("delivery correlates wrongly: %q", calls[0].requestID)
	}
	_, claims, signingInput, sig := decodeJWT(t, *calls[0].token)
	pub, err := atprotoid.ParsePublicDIDKey(didKey)
	if err != nil {
		t.Fatalf("parse pubkey: %v", err)
	}
	if err := pub.HashAndVerify([]byte(signingInput), sig); err != nil {
		t.Fatalf("the token does not verify against the repo signing key: %v", err)
	}
	if claims["iss"] != "did:plc:oracleaccount" || claims["aud"] != "did:web:feed.example#bsky_fg" ||
		claims["lxm"] != "app.bsky.feed.getFeedSkeleton" {
		t.Fatalf("claims are not the pushed ones: %v", claims)
	}
	if got, _ := claims["exp"].(float64); int64(got) != exp {
		t.Fatalf("exp: want the pushed %d, got %v", exp, claims["exp"])
	}
}

// The nest clamped already; the bridge clamps again, so a push past the cap
// can never mint a longer-lived token.
func TestServiceAuthRequestClampsAnExpPastTheCap(t *testing.T) {
	nest := &serviceAuthDeliveryRecorder{accepted: true}
	signer, _ := testSigner(t)
	s := NewServer(nest, nil, nil, &stubSigners{signer: signer}, nil)
	before := time.Now()

	s.deliverServiceAuth(context.Background(), servicePush(before.Add(time.Hour).Unix()))

	calls := nest.delivered()
	if len(calls) != 1 || calls[0].token == nil {
		t.Fatalf("want one delivered token, got %+v", calls)
	}
	_, claims, _, _ := decodeJWT(t, *calls[0].token)
	exp, _ := claims["exp"].(float64)
	if limit := time.Now().Add(MaxServiceAuthLifetime).Unix(); int64(exp) > limit {
		t.Fatalf("exp %v exceeds the cap (%d)", exp, limit)
	}
}

// A signer the bridge cannot produce is a refusal, delivered at once so the
// nest need not wait out its deadline; the reason stays in this log.
func TestServiceAuthRequestDeliversARefusalWhenNoSigner(t *testing.T) {
	for name, signers := range map[string]RepoSignerSource{
		"the unseal failed":  &stubSigners{err: errors.New("hpke open failed")},
		"no source is wired": nil,
	} {
		nest := &serviceAuthDeliveryRecorder{accepted: true}
		s := NewServer(nest, nil, nil, signers, nil)

		s.deliverServiceAuth(context.Background(), servicePush(time.Now().Add(30*time.Second).Unix()))

		calls := nest.delivered()
		if len(calls) != 1 || calls[0].token != nil {
			t.Fatalf("%s: want one refusal (no token), got %+v", name, calls)
		}
	}
}

// An incomplete push — no DID — must never become an unbound token.
func TestServiceAuthRequestRefusesAPushWithNoIssuer(t *testing.T) {
	nest := &serviceAuthDeliveryRecorder{accepted: true}
	signer, _ := testSigner(t)
	s := NewServer(nest, nil, nil, &stubSigners{signer: signer}, nil)
	push := servicePush(time.Now().Add(30 * time.Second).Unix())
	push.DID = ""

	s.deliverServiceAuth(context.Background(), push)

	if calls := nest.delivered(); len(calls) != 1 || calls[0].token != nil {
		t.Fatalf("want one refusal, got %+v", calls)
	}
}

// The push handler runs on the WS read loop; it must hand the mint off and
// return at once.
func TestHandleServiceAuthRequestedReturnsWithoutWaitingOnTheMint(t *testing.T) {
	nest := &serviceAuthDeliveryRecorder{accepted: true}
	release := make(chan struct{})
	s := NewServer(nest, nil, nil, blockingSigners{release: release}, nil)

	s.HandleServiceAuthRequested(context.Background(), servicePush(time.Now().Add(30*time.Second).Unix()))
	if got := nest.delivered(); len(got) != 0 {
		t.Fatalf("nothing can be delivered before the signer is released: %+v", got)
	}
	close(release)
	deadline := time.After(5 * time.Second)
	for {
		if got := nest.delivered(); len(got) == 1 {
			if got[0].token == nil {
				t.Fatal("the released mint delivered a refusal")
			}
			return
		}
		select {
		case <-deadline:
			t.Fatal("the released mint was never delivered")
		case <-time.After(time.Millisecond):
		}
	}
}

// blockingSigners hands back the fixed test key only once released.
type blockingSigners struct{ release <-chan struct{} }

func (b blockingSigners) RepoSigner(ctx context.Context, _ []byte) (RepoSigner, error) {
	select {
	case <-b.release:
	case <-ctx.Done():
		return nil, ctx.Err()
	}
	scalar := make([]byte, 32)
	for i := range scalar {
		scalar[i] = byte(i + 1)
	}
	key, err := atprotoid.PrivateKeyFromK256Scalar(scalar)
	if err != nil {
		return nil, err
	}
	return key, nil
}
