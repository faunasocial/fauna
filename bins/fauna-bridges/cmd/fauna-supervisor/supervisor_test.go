package main

import (
	"bufio"
	"encoding/json"
	"errors"
	"io"
	"log/slog"
	"net"
	"testing"
)

func newTestSupervisor(dispatch func(action, service string) error) (*supervisor, *[]string) {
	var calls []string
	if dispatch == nil {
		dispatch = func(action, service string) error {
			calls = append(calls, action+" "+service)
			return nil
		}
	}
	return &supervisor{
		s6ServiceDir: "/run/service",
		dispatch:     dispatch,
		logger:       slog.New(slog.NewJSONHandler(io.Discard, nil)),
	}, &calls
}

func TestHandleAllowlistedUpDown(t *testing.T) {
	sup, calls := newTestSupervisor(nil)
	for _, tc := range []command{
		{Action: "up", Service: "fauna-mail-bridge-mta"},
		{Action: "down", Service: "fauna-mail-bridge-mta"},
		{Action: "up", Service: "fauna-mail-bridge-mda"},
		{Action: "down", Service: "fauna-mail-bridge-mda"},
		{Action: "up", Service: "fauna-atproto-bridge"},
		{Action: "down", Service: "fauna-atproto-bridge"},
	} {
		got := sup.handle(tc)
		if !got.OK {
			t.Fatalf("handle(%+v) = %+v, want ok", tc, got)
		}
	}
	want := []string{
		"up fauna-mail-bridge-mta",
		"down fauna-mail-bridge-mta",
		"up fauna-mail-bridge-mda",
		"down fauna-mail-bridge-mda",
		"up fauna-atproto-bridge",
		"down fauna-atproto-bridge",
	}
	if len(*calls) != len(want) {
		t.Fatalf("dispatch calls = %v, want %v", *calls, want)
	}
	for i := range want {
		if (*calls)[i] != want[i] {
			t.Fatalf("dispatch call %d = %q, want %q", i, (*calls)[i], want[i])
		}
	}
}

func TestHandleRejectsUnknownAction(t *testing.T) {
	sup, calls := newTestSupervisor(nil)
	got := sup.handle(command{Action: "restart", Service: "fauna-mail-bridge-mta"})
	if got.OK {
		t.Fatalf("handle(restart) = %+v, want rejected", got)
	}
	if len(*calls) != 0 {
		t.Fatalf("dispatch ran %v on a rejected action; want none", *calls)
	}
}

func TestHandleRejectsServiceOutsideAllowlist(t *testing.T) {
	sup, calls := newTestSupervisor(nil)
	// fauna-nest is a real service but NOT mail-bridge — must be refused so the
	// socket can't be used to bounce the main server.
	for _, svc := range []string{"fauna-nest", "fauna-bridge-daemon", "../../etc", ""} {
		got := sup.handle(command{Action: "up", Service: svc})
		if got.OK {
			t.Fatalf("handle(up %q) = %+v, want rejected", svc, got)
		}
	}
	if len(*calls) != 0 {
		t.Fatalf("dispatch ran %v on disallowed services; want none", *calls)
	}
}

func TestHandleSurfacesDispatchError(t *testing.T) {
	sup, _ := newTestSupervisor(func(action, service string) error {
		return errors.New("s6-svc exploded")
	})
	got := sup.handle(command{Action: "up", Service: "fauna-mail-bridge-mta"})
	if got.OK || got.Error == "" {
		t.Fatalf("handle = %+v, want ok=false with error", got)
	}
}

// TestServeConnLineFramed drives the full line-framed read/ack loop over an
// in-memory socket pair: two valid commands then one malformed line.
func TestServeConnLineFramed(t *testing.T) {
	sup, calls := newTestSupervisor(nil)
	client, server := net.Pipe()
	go sup.serveConn(server)

	writeDone := make(chan struct{})
	go func() {
		defer close(writeDone)
		w := bufio.NewWriter(client)
		_, _ = w.WriteString(`{"action":"up","service":"fauna-mail-bridge-mta"}` + "\n")
		_, _ = w.WriteString(`{"action":"up","service":"fauna-mail-bridge-mda"}` + "\n")
		_, _ = w.WriteString("not json\n")
		_ = w.Flush()
	}()

	dec := json.NewDecoder(client)
	var acks []ack
	for i := 0; i < 3; i++ {
		var a ack
		if err := dec.Decode(&a); err != nil {
			t.Fatalf("decode ack %d: %v", i, err)
		}
		acks = append(acks, a)
	}
	<-writeDone
	client.Close()

	if !acks[0].OK || !acks[1].OK {
		t.Fatalf("first two acks = %+v, want both ok", acks[:2])
	}
	if acks[2].OK {
		t.Fatalf("malformed-line ack = %+v, want ok=false", acks[2])
	}
	if len(*calls) != 2 {
		t.Fatalf("dispatch calls = %v, want 2 (malformed line must not dispatch)", *calls)
	}
}
