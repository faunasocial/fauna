// Command fauna-supervisor is the nest→supervisor sidekick: a tiny
// Unix-domain socket server that lets nest bring the flag-gated mail-bridge
// s6 services up/down without filesystem polling.
//
// Per docs/goal/behavior/mail-bridge-lifecycle.md § Default-off on first claim
// and § Why the supervisor sidekick socket: when the admin toggles
// `mail.enabled` in their Fauna app, nest writes the `/data/imap-enabled`
// flag and sends an "up" command on this socket; the supervisor dispatches
// `s6-svc -u` so s6 brings the service up in milliseconds (vs. the latency +
// idle CPU of a 2 s `stat` poll loop). The wire-level shape lives in
// docs/goal/architecture/installers/docker.md § Supervisor sidekick socket.
//
// Security model (lifecycle.md § Wire shapes): the socket is owned by the
// `fauna` user at mode 0600, so only nest (which runs as `fauna`) can write to
// it; the supervisor runs as root so `s6-svc` can signal the s6 service
// control FIFOs. The action set is fixed to up/down and the service set is a
// fixed allowlist — a writer cannot ask the supervisor to run anything else.
//
// Although born for the mail bridge, the supervisor is protocol-agnostic:
// future bridges (ATProto, Mastodon, Nostr) that inherit the lifecycle shape
// register their service names in allowedServices and reuse this binary
// unchanged.
package main

import (
	"bufio"
	"encoding/json"
	"fmt"
	"log/slog"
	"net"
	"os/exec"
)

// command is one line-framed JSON request from nest.
//
//	{"action": "up", "service": "fauna-mail-bridge-mta"}
type command struct {
	Action  string `json:"action"`
	Service string `json:"service"`
}

// ack is the one-line JSON reply the supervisor writes back per command.
type ack struct {
	OK    bool   `json:"ok"`
	Error string `json:"error,omitempty"`
}

// allowedServices is the fixed allowlist of s6 services this supervisor will
// act on. A command naming anything else is rejected — the socket is a narrow
// control surface, not a general "run s6-svc on any service" channel. New
// bridge roles add their service names here.
var allowedServices = map[string]bool{
	"fauna-mail-bridge-mta": true,
	"fauna-mail-bridge-mda": true,
	// The out-of-process ATProto PDS bridge (role atproto.pds): down by
	// default, brought up by nest's set_atproto_enabled when the admin enables
	// Bluesky (docs/goal/behavior/atproto-pds-bridge.md § Enable UX).
	"fauna-atproto-bridge": true,
}

// supervisor handles decoded commands. dispatch is a seam so tests can assert
// the up/down decision without spawning s6-svc; production wires it to
// s6svcDispatch.
type supervisor struct {
	s6ServiceDir string
	dispatch     func(action, service string) error
	logger       *slog.Logger
}

// handle validates one command and dispatches it. It never returns an error —
// the outcome rides in the ack so the connection stays open for further
// commands (nest sends mta + mda back-to-back on a single enable toggle).
func (s *supervisor) handle(cmd command) ack {
	if cmd.Action != "up" && cmd.Action != "down" {
		return ack{OK: false, Error: fmt.Sprintf("unknown action %q (want up or down)", cmd.Action)}
	}
	if !allowedServices[cmd.Service] {
		return ack{OK: false, Error: fmt.Sprintf("service %q is not in the supervisor allowlist", cmd.Service)}
	}
	if err := s.dispatch(cmd.Action, cmd.Service); err != nil {
		s.logger.Error("dispatch failed", "action", cmd.Action, "service", cmd.Service, "err", err)
		return ack{OK: false, Error: err.Error()}
	}
	s.logger.Info("dispatched", "action", cmd.Action, "service", cmd.Service)
	return ack{OK: true}
}

// serveConn reads line-framed JSON commands off one connection until EOF,
// handling each and writing back its ack. A malformed line yields an error ack
// but does not tear the connection down.
func (s *supervisor) serveConn(conn net.Conn) {
	defer conn.Close()
	scanner := bufio.NewScanner(conn)
	enc := json.NewEncoder(conn)
	for scanner.Scan() {
		line := scanner.Bytes()
		if len(line) == 0 {
			continue
		}
		var cmd command
		if err := json.Unmarshal(line, &cmd); err != nil {
			s.logger.Warn("malformed command line", "err", err)
			_ = enc.Encode(ack{OK: false, Error: fmt.Sprintf("malformed JSON: %v", err)})
			continue
		}
		_ = enc.Encode(s.handle(cmd))
	}
	if err := scanner.Err(); err != nil {
		s.logger.Warn("connection read error", "err", err)
	}
}

// accept runs the connection-accept loop until ln is closed.
func (s *supervisor) accept(ln net.Listener) {
	for {
		conn, err := ln.Accept()
		if err != nil {
			// A closed listener (shutdown) surfaces here; the caller closes ln
			// on signal, so a post-close Accept error is the normal exit.
			s.logger.Info("accept loop stopping", "err", err)
			return
		}
		go s.serveConn(conn)
	}
}

// s6svcDispatch is the production dispatch: `s6-svc -u|-d <s6ServiceDir>/<svc>`.
// -u brings the (down-by-default) service up; -d brings it down (sends SIGTERM,
// the bridge drains per mail-bridge-lifecycle.md § Shutting down).
func s6svcDispatch(s6ServiceDir string) func(action, service string) error {
	return func(action, service string) error {
		flag := "-u"
		if action == "down" {
			flag = "-d"
		}
		path := s6ServiceDir + "/" + service
		out, err := exec.Command("s6-svc", flag, path).CombinedOutput()
		if err != nil {
			return fmt.Errorf("s6-svc %s %s: %w: %s", flag, path, err, out)
		}
		return nil
	}
}
