// The Go half of the DECODE-direction wire-vocabulary contract with Rust.
//
// # The direction that had nothing
//
// go_wire_variant_contract_test.go (next to this one) pins the vocabularies
// this bridge ENCODES onto the wire — the mail-auth / content-scan verdicts
// translated out of UniFFI types. This file pins the opposite direction: the
// discriminator strings nest PRODUCES and this bridge switches on.
//
// Nothing covered that. wsrpc_reply_cross_language_test.go decodes each
// reply-*.cbor fixture into the Go mirror and re-encodes it, which catches a
// field RENAME or TYPE change — and structurally cannot catch a VALUE rename,
// because `Outcome string` round-trips byte-identically whatever it says. So
// "fetch_error" could have become "fetcherror" on the Rust side, every Rust test
// that spells the literal updated in the same commit, every fixture still
// round-tripping — and this bridge would have silently stopped recognising the
// outcome, across a binary boundary no single test spans.
//
// # Why MTA-STS is the one that got the contract first
//
// The outcome is not merely read here, it is ECHOED BACK on report_tls_attempt
// so nest can rebuild the RFC 8460 §4.4 TLSRPT policy bucket without a second
// fetch. Producer and consumer are a round trip THROUGH THIS BINARY, and before
// this contract each end hand-wrote its own table. RFC 8461 §5 then makes the
// tokens load-bearing in the safety direction: a published-but-broken policy
// must be treated as no-policy and must never force plaintext or refusal, so an
// unrecognised token is a delivery decision taken on a fallback arm.
//
// # How this test avoids being another copy
//
// It types no token of its own. It reads two things:
//
//   - testdata/go-wire-outcomes.json — derived from the owning Rust enums
//     (fauna_mail::outbound::mta_sts::{MtaStsOutcome,MtaStsMode}) and kept
//     honest by libs/fauna-mail/tests/go_wire_outcome_contract.rs, whose token
//     lists are compiler-enforced complete (exhaustive matches, no `_` arm).
//   - This package's own const declarations, from the AST.
//
// Three properties:
//
//  1. every Rust token has a Go constant (Rust grew, Go did not → RED);
//  2. every Go constant is a Rust token (Go invented one → RED);
//  3. each constant is bound to the RIGHT token — the identifier must be
//     <TypeName><RustVariantName>, so MtaStsOutcomeFetchError cannot hold
//     "invalid" and pass a set comparison.
//
// A fourth test pins that the constants are actually USED: ../mta/outbound.go,
// the file that consumes the vocabulary, may not spell any of these tokens as a
// literal. Without that, the const family is decorative and the contract above
// guards nothing that ships.
package wsrpc

import (
	"encoding/json"
	"go/ast"
	"go/parser"
	"go/token"
	"os"
	"path/filepath"
	"strconv"
	"testing"
)

// The Go source declaring every const family under contract, relative to this
// package.
const outcomeConstSrc = "methods.go"

// Which fixture group is declared under which Go type, and which file consumes
// it. This IS a hand-written table, and deliberately a small one: it maps type
// names to type names to a path, and carries no token of either language, so it
// cannot drift with the vocabularies it describes.
//
// `banIn` is per-family on purpose rather than one banned-token set over one
// file. These tokens are ordinary words that other, unrelated vocabularies
// legitimately use elsewhere in the same package — `enforce` is also the FCrDNS
// mode in ../mta/policy.go, `found` is also fetch_message_ciphertext's
// discriminator in methods.go — so a package-wide ban would be a lie about
// which vocabulary is which. Banning each family only where it is consumed says
// exactly what is true.
// `fixture` names which committed file the family's tokens come from. There is
// one fixture per OWNING RUST CRATE, not one per contract: fauna-mail owns the
// reply discriminators, fauna-client-mail-settings owns the policy values (the
// same tokens three admin-mail pickers write), and the second cannot be derived
// from the first without a dev-dependency cycle. One fixture with two writers
// would be worse — whichever Rust test ran last would decide its contents.
var outcomeFamilies = []struct {
	rustEnum string
	goType   string
	banIn    string
	fixture  string
}{
	{"MtaStsOutcome", "MtaStsOutcome", "../mta/outbound.go", outcomeFixture},
	{"MtaStsMode", "MtaStsMode", "../mta/outbound.go", outcomeFixture},
	{"SrsBounceOutcome", "SrsBounceOutcome", "../mta/server.go", outcomeFixture},
	{"FcrdnsMode", "FCrDNSModeWire", "../mta/policy.go", policyFixture},
}

const (
	// Written by libs/fauna-mail/tests/go_wire_outcome_contract.rs.
	outcomeFixture = "go-wire-outcomes.json"
	// Written by libs/fauna-client-mail-settings/tests/go_wire_policy_contract.rs.
	policyFixture = "go-wire-policy-values.json"
)

// writerOf names the Rust test that owns each fixture, so a missing or
// unparseable file points at the one command that regenerates it.
var writerOf = map[string]string{
	outcomeFixture: "cargo test -p fauna-mail --test go_wire_outcome_contract",
	policyFixture:  "cargo test -p fauna-client-mail-settings --test go_wire_policy_contract",
}

// loadOutcomeFixture reads every committed fixture and merges their groups.
// Group names are unique across fixtures by construction — each names a Rust
// type, and the two owning crates cannot both define one — so a collision is a
// real error rather than a merge policy to decide.
func loadOutcomeFixture(t *testing.T) map[string]map[string]string {
	t.Helper()
	out := map[string]map[string]string{}
	for _, name := range []string{outcomeFixture, policyFixture} {
		raw, err := os.ReadFile(filepath.Join("testdata", name))
		if err != nil {
			t.Fatalf("read %s: %v\n\nIt is written by a Rust test — run "+
				"`%s` and follow its failure, which prints the exact bytes.",
				name, err, writerOf[name])
		}
		var one map[string]map[string]string
		if err := json.Unmarshal(raw, &one); err != nil {
			t.Fatalf("parse testdata/%s: %v", name, err)
		}
		for group, tokens := range one {
			if _, dup := out[group]; dup {
				t.Fatalf("group %q appears in more than one fixture — two Rust "+
					"crates cannot both own a vocabulary", group)
			}
			out[group] = tokens
		}
	}
	return out
}

// goConstsOfType returns ident → string value for every `const Ident Type = "…"`
// declaration of the named type in the parsed file. Only explicitly-typed specs
// count: an untyped sibling in the same block is not part of the family and
// would not carry the type's meaning at a call site either.
func goConstsOfType(t *testing.T, file *ast.File, typeName string) map[string]string {
	t.Helper()
	out := map[string]string{}
	for _, decl := range file.Decls {
		gen, ok := decl.(*ast.GenDecl)
		if !ok || gen.Tok != token.CONST {
			continue
		}
		for _, spec := range gen.Specs {
			vs, ok := spec.(*ast.ValueSpec)
			if !ok {
				continue
			}
			ident, ok := vs.Type.(*ast.Ident)
			if !ok || ident.Name != typeName {
				continue
			}
			for i, name := range vs.Names {
				if i >= len(vs.Values) {
					continue
				}
				lit, ok := vs.Values[i].(*ast.BasicLit)
				if !ok || lit.Kind != token.STRING {
					continue
				}
				val, err := strconv.Unquote(lit.Value)
				if err != nil {
					t.Fatalf("%s: unquote %s: %v", typeName, name.Name, err)
				}
				out[name.Name] = val
			}
		}
	}
	return out
}

func parseGo(t *testing.T, path string) *ast.File {
	t.Helper()
	file, err := parser.ParseFile(token.NewFileSet(), path, nil, parser.ParseComments)
	if err != nil {
		t.Fatalf("parse %s: %v", path, err)
	}
	return file
}

// Property 1+2+3: the Go const family is exactly the Rust token set, and each
// constant is bound to the token its name claims.
func TestMtaStsConstFamiliesMatchTheRustTokens(t *testing.T) {
	fixture := loadOutcomeFixture(t)
	file := parseGo(t, outcomeConstSrc)

	for _, fam := range outcomeFamilies {
		rustTokens, ok := fixture[fam.rustEnum]
		if !ok {
			t.Fatalf("testdata/go-wire-outcomes.json has no %q group — the Rust "+
				"contract stopped emitting it, or this table names a type that no "+
				"longer exists", fam.rustEnum)
		}
		goConsts := goConstsOfType(t, file, fam.goType)

		// 1 + 3: every Rust variant has a correctly-named constant holding its
		// exact token.
		for variant, token := range rustTokens {
			want := fam.goType + variant
			got, ok := goConsts[want]
			if !ok {
				t.Errorf("%s.%s (wire %q) has no Go constant %s in %s.\n"+
					"Rust grew a variant this bridge cannot name; add the constant "+
					"and give it an arm wherever the vocabulary is switched on.",
					fam.rustEnum, variant, token, want, outcomeConstSrc)
				continue
			}
			if got != token {
				t.Errorf("%s = %q but Rust's %s.%s is %q — the constant is bound to "+
					"the wrong token, which a set comparison alone would not catch",
					want, got, fam.rustEnum, variant, token)
			}
		}

		// 2: Go invented nothing. A stale constant is worse than a missing one —
		// it reads as a supported outcome at every call site.
		for name, val := range goConsts {
			variant, ok := trimPrefix(name, fam.goType)
			if !ok {
				t.Errorf("%s is typed %s but is not named %s<RustVariant>; the "+
					"identifier↔variant check cannot see it, so it is an unpinned copy",
					name, fam.goType, fam.goType)
				continue
			}
			want, ok := rustTokens[variant]
			if !ok {
				t.Errorf("%s = %q names %s.%s, which Rust does not have — a token "+
					"removed or renamed on the owning side, still advertised here",
					name, val, fam.rustEnum, variant)
				continue
			}
			if want != val {
				t.Errorf("%s = %q, Rust says %q", name, val, want)
			}
		}
	}
}

func trimPrefix(s, prefix string) (string, bool) {
	if len(s) <= len(prefix) || s[:len(prefix)] != prefix {
		return "", false
	}
	return s[len(prefix):], true
}

// Property 4: the constants are load-bearing, not decorative.
//
// Each family is banned only in the file that consumes it (see outcomeFamilies'
// `banIn` and the comment there for why the scope is per-family rather than
// package-wide).
func TestConsumingFilesSpellNoWireTokenByHand(t *testing.T) {
	fixture := loadOutcomeFixture(t)

	// token → const name, per consuming file.
	banned := map[string]map[string]string{}
	for _, fam := range outcomeFamilies {
		if banned[fam.banIn] == nil {
			banned[fam.banIn] = map[string]string{}
		}
		for variant, tok := range fixture[fam.rustEnum] {
			banned[fam.banIn][tok] = fam.goType + variant
		}
	}

	for src, tokens := range banned {
		scanForHandSpelledTokens(t, src, tokens)
	}
}

func scanForHandSpelledTokens(t *testing.T, src string, banned map[string]string) {
	t.Helper()
	fset := token.NewFileSet()
	file, err := parser.ParseFile(fset, src, nil, 0)
	if err != nil {
		t.Fatalf("parse %s: %v", src, err)
	}
	ast.Inspect(file, func(n ast.Node) bool {
		lit, ok := n.(*ast.BasicLit)
		if !ok || lit.Kind != token.STRING {
			return true
		}
		val, err := strconv.Unquote(lit.Value)
		if err != nil {
			return true
		}
		if constName, bad := banned[val]; bad {
			t.Errorf("%s: %q is spelled by hand — use %s.\n"+
				"A literal here is invisible to the fixture contract above, so the "+
				"token could be renamed on the owning Rust side and this site would "+
				"keep comparing against the old one and silently take the fallback "+
				"arm — which for both of these vocabularies is a delivery decision "+
				"(RFC 8461 §5 for MTA-STS; a 451 tempfail on every bounce for SRS).",
				fset.Position(lit.Pos()), val, constName)
		}
		return true
	})
}
