package main

import (
	"strings"
	"testing"

	"github.com/faunasocial/fauna/bins/fauna-bridges/internal/atprotopds"
)

// The D8 matrix lives in Rust and is exhaustively tested there
// (libs/fauna-bridge-atproto/src/authz.rs). What CANNOT be tested there is the
// binding: that the input really crosses the FFI intact and the verdict really
// comes back. This suite is that proof, and it is deliberately the only place
// a Go test asserts a matrix outcome — every other Go test stubs the seam, so
// that no second copy of the policy grows on this side.
//
// It drives the production adapter (ffiAuthorizer), so a regression in the
// generated binding, the record field order, or the enum-variant mapping fails
// here rather than in production.

func authzInput(plane, scope, lxm string, enabled bool) atprotopds.AuthzInput {
	return atprotopds.AuthzInput{
		Plane:               plane,
		Scopes:              []string{scope},
		ExternalAppsEnabled: enabled,
		Lxm:                 lxm,
		EndpointClass:       "authed",
	}
}

func TestFfiAuthorizerCarriesTheRealMatrix(t *testing.T) {
	a := ffiAuthorizer{}
	const app = "app_credential"
	const plain = "com.atproto.appPass"
	const privileged = "com.atproto.appPassPrivileged"

	for _, tc := range []struct {
		name      string
		input     atprotopds.AuthzInput
		wantAllow bool
		wantErr   string
	}{
		{"repo read is granted", authzInput(app, plain, "com.atproto.repo.getRecord", true), true, ""},
		{"repo write is granted", authzInput(app, plain, "com.atproto.repo.createRecord", true), true, ""},
		{"blob upload is granted", authzInput(app, plain, "com.atproto.repo.uploadBlob", true), true, ""},
		{"kill-switch off refuses uniformly", authzInput(app, plain, "com.atproto.repo.getRecord", false), false, "AuthenticationRequired"},
		{"unknown plane refuses uniformly", authzInput("martian", plain, "com.atproto.repo.getRecord", true), false, "AuthenticationRequired"},
		{"DM needs the privileged scope", authzInput(app, plain, "chat.bsky.convo.listConvos", true), false, "InvalidToken"},
		{"DM with the privileged scope", authzInput(app, privileged, "chat.bsky.convo.listConvos", true), true, ""},
		{"account management is refused", authzInput(app, plain, "com.atproto.server.createAppPassword", true), false, "InvalidToken"},
		{"migration is DEFERRED, not policy", authzInput(app, plain, "com.atproto.repo.importRepo", true), false, "MethodNotImplemented"},
		{"an unclassified method is refused", authzInput(app, plain, "com.example.newThing", true), false, "InvalidToken"},
	} {
		t.Run(tc.name, func(t *testing.T) {
			got := a.Authorize(tc.input)
			if got.Allow != tc.wantAllow {
				t.Fatalf("Allow = %v, want %v (deny was %q: %s)", got.Allow, tc.wantAllow, got.XrpcError, got.Message)
			}
			if !tc.wantAllow && got.XrpcError != tc.wantErr {
				t.Fatalf("XrpcError = %q, want %q", got.XrpcError, tc.wantErr)
			}
			if !tc.wantAllow && got.Message == "" {
				t.Error("a deny must carry a message across the binding")
			}
		})
	}
}

// The deferred sub-type must survive the crossing intact — a session reading
// "not yet" must never mistake it for permanent policy.
func TestFfiAuthorizerPreservesTheDeferredWording(t *testing.T) {
	v := ffiAuthorizer{}.Authorize(authzInput("app_credential", "com.atproto.appPass", "com.atproto.server.activateAccount", true))
	if v.Allow || v.XrpcError != "MethodNotImplemented" {
		t.Fatalf("verdict = %+v", v)
	}
	if !strings.Contains(v.Message, "not yet") {
		t.Errorf("deferred message = %q, want it to say 'not yet'", v.Message)
	}
}

// `aud` is the one Option<String> in the input; a binding that dropped or
// mangled it would silently disable the whole audience half of the matrix.
func TestFfiAuthorizerCarriesTheOptionalAudience(t *testing.T) {
	a := ffiAuthorizer{}
	base := authzInput("app_credential", "com.atproto.appPass", "app.bsky.feed.getTimeline", true)

	// Nil audience: an ordinary AppView read.
	if v := a.Authorize(base); !v.Allow {
		t.Fatalf("nil aud must be allowed, got %+v", v)
	}

	// Set audience naming the chat service: the same lxm now needs the
	// privileged scope, which only happens if `aud` truly crossed.
	chat := "did:web:api.bsky.chat#bsky_chat"
	withAud := base
	withAud.Aud = &chat
	v := a.Authorize(withAud)
	if v.Allow {
		t.Fatal("aud=chat did not cross the binding: the DM gate never fired")
	}
	if v.XrpcError != "InvalidToken" {
		t.Fatalf("XrpcError = %q, want InvalidToken", v.XrpcError)
	}
}
