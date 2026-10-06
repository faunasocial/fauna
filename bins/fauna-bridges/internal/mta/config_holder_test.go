package mta

import (
	"testing"

	"github.com/faunasocial/fauna/bins/fauna-bridges/internal/mailfauna"
	"github.com/faunasocial/fauna/bins/fauna-bridges/internal/wsrpc"
)

// TestMTAConfigHolderHotApply proves the shared holder's ApplyConfig swaps the
// live config atomically: a re-fetched snapshot replaces the local-domains
// list (the inbound RCPT gate, also covered end-to-end by the tier_3
// test_mta_local_domains_hot_reloads_without_restart) and the bridge-side
// outbound knobs (IPv6 egress + 5xx allowlist), which are read live by the
// outbound sender via the cfg.outboundPolicy accessor.
func TestMTAConfigHolderHotApply(t *testing.T) {
	t.Parallel()

	initial := wsrpc.ConfigSnapshot{
		LocalDomains: []string{"a.example"},
		Outbound: wsrpc.OutboundPolicy{
			IPv6Enabled:         true,
			Treat5xxAsTransient: nil,
		},
	}
	h := newMTAConfigHolder(initial, nil, nil, nil)

	if got := h.current().localDomains; len(got) != 1 || got[0] != "a.example" {
		t.Fatalf("initial local domains = %v, want [a.example]", got)
	}
	if pol := h.outboundPolicy(); !pol.ipv6Enabled || len(pol.treat5xxAsTransient) != 0 {
		t.Fatalf("initial outbound policy = %+v, want {ipv6Enabled:true, treat5xx:[]}", pol)
	}

	// Hot-apply a changed snapshot (admin added a domain, disabled IPv6 egress,
	// and allowlisted a 5xx code).
	h.ApplyConfig(wsrpc.ConfigSnapshot{
		LocalDomains: []string{"a.example", "b.example"},
		Outbound: wsrpc.OutboundPolicy{
			IPv6Enabled:         false,
			Treat5xxAsTransient: []string{"5.7.1"},
		},
	})

	if got := h.current().localDomains; len(got) != 2 || got[1] != "b.example" {
		t.Errorf("post-apply local domains = %v, want [a.example b.example]", got)
	}
	pol := h.outboundPolicy()
	if pol.ipv6Enabled {
		t.Errorf("post-apply ipv6Enabled = true, want false (hot-applied disable)")
	}
	if len(pol.treat5xxAsTransient) != 1 || pol.treat5xxAsTransient[0] != "5.7.1" {
		t.Errorf("post-apply treat5xxAsTransient = %v, want [5.7.1]", pol.treat5xxAsTransient)
	}
}

// The perimeter ceiling is `max_message_bytes` alone since ceiling retirement
// (smtp-server.md § Message size limits): crossing the wire is solved (a body
// over the inline budget rides the bulk-byte plane by reference) and resting is
// solved (continuation records rest any size), so there is no at-rest clamp.
//
// The regression this pins: a 2 MB message must be ACCEPTED at the door (it
// delivers by reference) and the shipped 50 MB default must pass through
// unclamped — where the old inline clamp refused anything over ~1.5 MB and the
// interim at-rest clamp held it at ~8 MB.
func TestEffectiveMaxMessageBytes(t *testing.T) {
	// Mirrors SpamPolicyThresholds::default().max_message_bytes (shared Rust).
	const productDefault uint32 = 50_000_000
	// A 0 knob (no snapshot) is NOT uncapped — go-smtp treats a 0 MaxMessageBytes
	// as unlimited, so it falls back to the shipped product default.
	if got := mailfauna.EffectiveMaxRawMessageBytes(0); got != productDefault {
		t.Fatalf("a 0 knob must fall back to the product default %d, got %d", productDefault, got)
	}
	// The effective ceiling far exceeds the inline ceiling — a body above it
	// crosses by reference, it is not refused.
	if inline := mailfauna.MaxInlineRawMessageBytes(); productDefault <= inline {
		t.Fatalf("the ceiling %d must exceed the inline ceiling %d — a body above the "+
			"inline ceiling crosses by reference, it is not refused", productDefault, inline)
	}
	cases := []struct{ in, want uint32 }{
		{0, productDefault},        // no snapshot: falls back to the product default, not uncapped
		{50_000_000, 50_000_000},   // shipped default passes through — genuinely deliverable now
		{2_000_000, 2_000_000},     // over the INLINE ceiling: admitted, rides by reference
		{1_000, 1_000},             // admin lowered: the knob is the ceiling
		{100_000_000, 100_000_000}, // admin raised: no at-rest clamp caps it
		// The product ceiling's upper BOUND (2026-08-26 ruling;
		// mail-message-size.md, Message size limits) is enforced at the WRITE
		// (`put_spam_policy` refuses a larger knob), and no pre-refusal knob
		// exists — so the read is an identity above the bound too (tranche
		// C7 dropped the read clamp; `effective_max_raw_message_bytes`'s doc).
		// The bound itself still passes through unchanged.
		{500_000_000, 500_000_000},
		{mailfauna.MaxMessageBytesCeiling(), mailfauna.MaxMessageBytesCeiling()},
	}
	for _, c := range cases {
		if got := effectiveMaxMessageBytes(c.in); got != c.want {
			t.Errorf("effectiveMaxMessageBytes(%d) = %d, want %d", c.in, got, c.want)
		}
	}
	// The snapshot wiring reads the knob unclamped (not just the helper).
	lc := buildMTALiveConfig(wsrpc.ConfigSnapshot{
		Spam: wsrpc.SpamPolicyThresholds{MaxMessageBytes: 50_000_000},
	}, nil, nil)
	if lc.maxMessageBytes != 50_000_000 {
		t.Errorf("buildMTALiveConfig maxMessageBytes = %d, want the unclamped 50000000", lc.maxMessageBytes)
	}
}
