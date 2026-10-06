package main

import (
	"context"
	"flag"
	"fmt"
	"log/slog"
	"net"
	"os"
	"os/signal"
	"os/user"
	"strconv"
	"syscall"
)

func main() {
	if err := run(os.Args, os.Stderr); err != nil {
		fmt.Fprintln(os.Stderr, err)
		os.Exit(1)
	}
}

func run(args []string, logW *os.File) error {
	fs := flag.NewFlagSet(args[0], flag.ContinueOnError)
	socketPath := fs.String("socket", "/run/fauna-supervisor.sock", "Unix-domain socket nest writes up/down commands to")
	s6ServiceDir := fs.String("s6-service-dir", "/run/service", "s6-rc live service scandir passed to s6-svc")
	socketOwner := fs.String("socket-owner", "fauna", "chown the socket to this user so nest (which runs as it) can write; empty = no chown")
	if err := fs.Parse(args[1:]); err != nil {
		return err
	}

	logger := slog.New(slog.NewJSONHandler(logW, nil)).With("service", "fauna-supervisor")

	// Remove a stale socket from a prior unclean exit; bind() fails with
	// EADDRINUSE on a leftover socket file otherwise.
	if err := os.Remove(*socketPath); err != nil && !os.IsNotExist(err) {
		return fmt.Errorf("remove stale socket %s: %w", *socketPath, err)
	}
	ln, err := net.Listen("unix", *socketPath)
	if err != nil {
		return fmt.Errorf("listen %s: %w", *socketPath, err)
	}
	defer ln.Close()

	// 0600 so only the owning user can read/write; chown to `fauna` so nest
	// (which runs as `fauna`) can connect while the supervisor itself runs as
	// root for s6-svc. Per lifecycle.md § Wire shapes. A missing user is a
	// soft failure — the socket stays root-owned (a root nest could still
	// write), so log + continue rather than refuse to start.
	if err := os.Chmod(*socketPath, 0o600); err != nil {
		return fmt.Errorf("chmod %s: %w", *socketPath, err)
	}
	if *socketOwner != "" {
		if u, lookErr := user.Lookup(*socketOwner); lookErr != nil {
			logger.Warn("socket owner lookup failed; leaving socket root-owned", "owner", *socketOwner, "err", lookErr)
		} else {
			uid, _ := strconv.Atoi(u.Uid)
			gid, _ := strconv.Atoi(u.Gid)
			if err := os.Chown(*socketPath, uid, gid); err != nil {
				logger.Warn("socket chown failed; leaving socket root-owned", "owner", *socketOwner, "err", err)
			}
		}
	}

	sup := &supervisor{
		s6ServiceDir: *s6ServiceDir,
		dispatch:     s6svcDispatch(*s6ServiceDir),
		logger:       logger,
	}

	ctx, stop := signal.NotifyContext(context.Background(), syscall.SIGINT, syscall.SIGTERM)
	defer stop()
	go func() {
		<-ctx.Done()
		ln.Close() // unblocks accept()
	}()

	logger.Info("fauna-supervisor listening", "socket", *socketPath, "s6_service_dir", *s6ServiceDir)
	sup.accept(ln)
	logger.Info("fauna-supervisor stopped")
	return nil
}
