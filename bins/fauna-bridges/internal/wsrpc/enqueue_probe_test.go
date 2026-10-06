package wsrpc

// Throwaway diagnostic probe (env-gated; skipped in normal runs): replay
// fauna.bridges.enqueue_outbound_mail against a live nest USING THE BRIDGE'S
// OWN client stack (auth, framing, dag-cbor encoding) and decode the ok=false
// payload the production bridge only logs as a byte count. Used to root-cause
// the 2026-07-09 production 451 "Outbound enqueue temporarily unavailable".

import (
	"context"
	"crypto/ed25519"
	"crypto/tls"
	"errors"
	"net/http"
	"os"
	"testing"
	"time"

	"github.com/fxamacker/cbor/v2"
)

func TestEnqueueProbeAgainstLiveNest(t *testing.T) {
	url := os.Getenv("FAUNA_ENQUEUE_PROBE_URL")
	if url == "" {
		t.Skip("set FAUNA_ENQUEUE_PROBE_URL (+ _KEYFILE, _SENDER) to run the probe")
	}
	keyfile := os.Getenv("FAUNA_ENQUEUE_PROBE_KEYFILE")
	sender := os.Getenv("FAUNA_ENQUEUE_PROBE_SENDER")
	raw, err := os.ReadFile(keyfile)
	if err != nil {
		t.Fatalf("read keyfile: %v", err)
	}
	var kf struct {
		Seed []byte `cbor:"ed25519_seed"`
	}
	if err := cbor.Unmarshal(raw, &kf); err != nil {
		t.Fatalf("decode keyfile: %v", err)
	}
	priv := ed25519.NewKeyFromSeed(kf.Seed)
	pub := priv.Public().(ed25519.PublicKey)

	httpClient := &http.Client{
		Timeout: 30 * time.Second,
		Transport: &http.Transport{
			TLSClientConfig: &tls.Config{InsecureSkipVerify: true},
		},
	}
	ac := NewAuthClient(httpClient, url, pub, priv)
	ctx, cancel := context.WithTimeout(context.Background(), 60*time.Second)
	defer cancel()
	c, err := Dial(ctx, ClientConfig{
		NestEndpoint: url,
		AuthClient:   ac,
		HTTPClient:   httpClient,
	})
	if err != nil {
		t.Fatalf("dial: %v", err)
	}
	defer c.Close()

	ids, err := EnqueueOutboundMail(ctx, c, "<probe-go@repro>", sender,
		[]string{"probe@example.com"}, []byte("From: x\r\n\r\nbody\r\n"), nil, nil)
	if err != nil {
		var se *ServerError
		if errors.As(err, &se) {
			var decoded map[string]any
			_ = cbor.Unmarshal(se.Payload, &decoded)
			t.Fatalf("enqueue failed; payload hex=%x decoded=%v", se.Payload, decoded)
		}
		t.Fatalf("enqueue failed (no ServerError): %v", err)
	}
	t.Logf("enqueue OK ids=%v", ids)
}
