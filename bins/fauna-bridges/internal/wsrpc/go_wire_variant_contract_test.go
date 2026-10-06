// The Go half of the wire-mirror variant-name contract with Rust.
//
// Covers two families, because they are the same mechanism with the same gap:
// the mail-auth verdicts (fauna_core::mail_auth, joined 2026-08-17) and the
// content-scan results (fauna_core::mail_scan, joined 2026-08-18, row 172).
//
// # What was unpinned
//
// methods.go hand-writes a THIRD copy of each verdict shape — a per-verdict
// `{Kind string; Data *…}` struct — and internal/mailfauna/mailfauna.go's
// `*ToWire` switches translate the UniFFI tagged-union Go types onto it. That
// copy is a legitimate language-boundary encoder, not drift: UniFFI's Go enums
// do not serialize to the serde adjacently-tagged map nest decodes.
//
// But wsrpc_conformance_test.go's list covers those mirror structs' `cbor:`
// FIELD tags only. Nothing asserted that the VARIANT STRINGS the switches emit
// ("none", "pass", "soft_fail", "clean", "infected", …) still match the serde
// names the Rust enums produce. A variant added Rust-side compiled everywhere,
// the Go switch simply had no arm for it, and the failure surfaced as a wrong
// wire VALUE rather than a red test.
//
// # How this test avoids being a fourth copy
//
// It types no variant name of its own. It reads two things:
//
//   - testdata/go-wire-variants.json — written from serde itself and kept
//     honest by libs/fauna-core/tests/go_wire_variant_contract.rs, which
//     derives the same map live on every `cargo test -p fauna-core` and reds
//     when the file is stale. Its variant lists are compiler-enforced complete
//     (an exhaustive `match` with no `_` arm), so they cannot lag the enums.
//   - The switches' own AST. Parsing beats a hand-written expectation table for
//     the same reason the fixture beats a hand-written name list: a table is a
//     copy, and a copy passes while wrong.
//
// Three properties, and the middle one is the one a set-comparison alone would
// miss:
//
//  1. every Rust variant has an arm (Rust grew, Go did not → RED);
//  2. each arm is wired to the RIGHT variant — `case DkimVerdictPass` must emit
//     `pass`, not merely some name in the set;
//  3. the `default:` arm lands on the ruled fail-safe value.
//
// # Why here and not in package mailfauna
//
// mailfauna imports the cgo-linked UniFFI binding, so a test there cannot run
// without a built libfauna_ffi. This one parses source and reads a fixture, so
// it runs under a bare `go test ./internal/wsrpc/` on any machine — including
// the merge path, where no build slot is available. `wsrpc` is also the package
// that owns the wire mirror the contract is about.
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

// The Go source whose switches are under contract, relative to this package.
const mailfaunaSrc = "../mailfauna/mailfauna.go"

// Which switch function translates which Rust enum, and where an unmappable
// variant must land. This IS a hand-written table, and deliberately a small one:
// it maps Go function names to Rust type names, which is a fact about neither
// language's variant set and cannot drift with either. Every variant NAME comes
// from the fixture.
//
// defaultVariant names the Rust variant whose serde name the `default:` arm must
// emit — resolved through the fixture rather than spelled as a string, so the
// pin follows a rename.
var toWireSwitches = []struct {
	fn             string
	rustEnum       string
	defaultVariant string
	why            string
}{
	{"dkimToWire", "DkimVerdict", "TempError",
		"`none` is the most permissive point of an auth lattice (\"no policy published\"); an unmappable variant must say \"could not determine\""},
	{"spfToWire", "SpfVerdict", "TempError", "see dkimToWire"},
	{"dmarcToWire", "DmarcVerdict", "TempError", "see dkimToWire"},
	{"arcToWire", "ArcVerdict", "TempError", "see dkimToWire"},
	// The scan family, joined 2026-08-18 with row 172. Same shape, sharper
	// stakes: `clean` is not merely permissive, it is an affirmative claim that
	// no malware signature matched — one we cannot make about a variant we could
	// not map. Its error member is spelled `Error` rather than `TempError`
	// (clamd either answered or it did not; no perm/temp split).
	{"ClamavVerdictToWire", "ClamavVerdict", "Error",
		"`clean` asserts \"no signature matched\"; an unmappable malware verdict must not claim that"},
	// The deliberate asymmetry. DmarcPolicy is what the domain owner PUBLISHED,
	// not a determination, and RFC 7489 fixes its set at exactly three with no
	// error member. Both alternatives are stricter, and inventing a stricter
	// policy than the domain published is the harm direction for a policy field.
	{"dmarcPolicyToWire", "DmarcPolicy", "None",
		"a policy field under-claims when unsure; there is no error member to defer to (RFC 7489)"},
}

func loadVariantFixture(t *testing.T) map[string]map[string]string {
	t.Helper()
	raw, err := os.ReadFile(filepath.Join("testdata", "go-wire-variants.json"))
	if err != nil {
		t.Fatalf("read the variant fixture: %v\n\nIt is written by "+
			"libs/fauna-core/tests/go_wire_variant_contract.rs — run "+
			"`cargo test -p fauna-core --test go_wire_variant_contract` and follow "+
			"its failure, which prints the exact bytes.", err)
	}
	var out map[string]map[string]string
	if err := json.Unmarshal(raw, &out); err != nil {
		t.Fatalf("parse the variant fixture: %v", err)
	}
	if len(out) == 0 {
		t.Fatal("the variant fixture is empty — a vacuous pass is not coverage")
	}
	return out
}

// armKind returns the Kind string a case body emits, from either shape the
// switches use: `return wsrpc.XVerdict{Kind: "…"}` or a bare `return "…"`.
func armKind(body []ast.Stmt) (string, bool) {
	var found string
	var ok bool
	for _, stmt := range body {
		ast.Inspect(stmt, func(n ast.Node) bool {
			switch node := n.(type) {
			case *ast.KeyValueExpr:
				if key, isIdent := node.Key.(*ast.Ident); isIdent && key.Name == "Kind" {
					if s, isLit := stringLit(node.Value); isLit {
						found, ok = s, true
					}
				}
			case *ast.ReturnStmt:
				if len(node.Results) == 1 {
					if s, isLit := stringLit(node.Results[0]); isLit {
						found, ok = s, true
					}
				}
			}
			return !ok
		})
		if ok {
			return found, true
		}
	}
	return "", false
}

func stringLit(e ast.Expr) (string, bool) {
	lit, isLit := e.(*ast.BasicLit)
	if !isLit || lit.Kind != token.STRING {
		return "", false
	}
	s, err := strconv.Unquote(lit.Value)
	if err != nil {
		return "", false
	}
	return s, true
}

// caseVariantIdent pulls `Pass` out of a `case faunaCore.DkimVerdictPass:` /
// `case faunaCore.SpfVerdictPass:` clause, given the Rust enum's Go type prefix.
func caseVariantIdent(expr ast.Expr, enumPrefix string) (string, bool) {
	sel, isSel := expr.(*ast.SelectorExpr)
	if !isSel {
		return "", false
	}
	name := sel.Sel.Name
	if len(name) <= len(enumPrefix) || name[:len(enumPrefix)] != enumPrefix {
		return "", false
	}
	return name[len(enumPrefix):], true
}

// switchArms walks a named function and returns variant-ident → emitted Kind,
// plus the `default:` arm's Kind.
func switchArms(t *testing.T, file *ast.File, fn, enumPrefix string) (map[string]string, string) {
	t.Helper()
	arms := map[string]string{}
	defaultKind := ""
	seen := false

	for _, decl := range file.Decls {
		fd, isFn := decl.(*ast.FuncDecl)
		if !isFn || fd.Name.Name != fn {
			continue
		}
		seen = true
		ast.Inspect(fd, func(n ast.Node) bool {
			clause, isClause := n.(*ast.CaseClause)
			if !isClause {
				return true
			}
			kind, hasKind := armKind(clause.Body)
			if !hasKind {
				return true
			}
			if clause.List == nil { // `default:`
				defaultKind = kind
				return true
			}
			for _, expr := range clause.List {
				if ident, matched := caseVariantIdent(expr, enumPrefix); matched {
					arms[ident] = kind
				}
			}
			return true
		})
	}
	if !seen {
		t.Fatalf("%s: no func %s in %s — the contract's anchor moved; "+
			"update toWireSwitches rather than deleting the test", fn, fn, mailfaunaSrc)
	}
	return arms, defaultKind
}

func parseMailfauna(t *testing.T) *ast.File {
	t.Helper()
	fset := token.NewFileSet()
	file, err := parser.ParseFile(fset, mailfaunaSrc, nil, parser.SkipObjectResolution)
	if err != nil {
		t.Fatalf("parse %s: %v", mailfaunaSrc, err)
	}
	return file
}

// TestToWireCoversEveryRustVariant is the property row 170 exists for: a verdict
// variant added on the Rust side must not be able to reach the wire untranslated.
func TestToWireCoversEveryRustVariant(t *testing.T) {
	fixture := loadVariantFixture(t)
	file := parseMailfauna(t)

	for _, sw := range toWireSwitches {
		names, known := fixture[sw.rustEnum]
		if !known {
			t.Fatalf("%s: the fixture has no entry for %s — regenerate it via "+
				"cargo test -p fauna-core --test go_wire_variant_contract",
				sw.fn, sw.rustEnum)
		}
		arms, _ := switchArms(t, file, sw.fn, sw.rustEnum)

		for variant, wantKind := range names {
			got, armed := arms[variant]
			if !armed {
				t.Errorf("%s has NO arm for %s::%s.\n\n"+
					"A message carrying that variant would fall through to the default "+
					"arm and be recorded as something it is not. Add:\n"+
					"    case faunaCore.%s%s:\n        return wsrpc.%s{Kind: %q}",
					sw.fn, sw.rustEnum, variant, sw.rustEnum, variant, sw.rustEnum, wantKind)
				continue
			}
			// The mis-mapping check: presence in the set is not enough — the arm
			// must emit the name for THIS variant.
			if got != wantKind {
				t.Errorf("%s maps %s::%s to %q, but serde names it %q — the nest would "+
					"record the wrong verdict for a message that authenticated fine",
					sw.fn, sw.rustEnum, variant, got, wantKind)
			}
		}
	}
}

// TestToWireEmitsNoNameRustDoesNotKnow is the other direction: a Go-side typo
// produces a Kind the Rust decoder has never heard of.
func TestToWireEmitsNoNameRustDoesNotKnow(t *testing.T) {
	fixture := loadVariantFixture(t)
	file := parseMailfauna(t)

	for _, sw := range toWireSwitches {
		names := fixture[sw.rustEnum]
		valid := map[string]bool{}
		for _, kind := range names {
			valid[kind] = true
		}
		arms, defaultKind := switchArms(t, file, sw.fn, sw.rustEnum)
		for variant, kind := range arms {
			if !valid[kind] {
				t.Errorf("%s emits Kind %q for %s — no variant of %s serializes to that "+
					"name, so nest's strict decode would reject the request",
					sw.fn, kind, variant, sw.rustEnum)
			}
		}
		if defaultKind != "" && !valid[defaultKind] {
			t.Errorf("%s's default arm emits Kind %q, which %s has no variant for",
				sw.fn, defaultKind, sw.rustEnum)
		}
	}
}

// TestToWireDefaultArmsAreFailSafe pins rider (a)'s ruling so a later session
// cannot quietly restore the permissive fallback.
//
// The four verdict switches must land an unmappable variant on TempError
// ("could not determine"), never None ("no policy published") — the latter is
// the most permissive point of an authentication lattice and asserts something
// false. dmarcPolicyToWire is the documented exception, and this test carries
// the asymmetry rather than leaving it to a comment.
func TestToWireDefaultArmsAreFailSafe(t *testing.T) {
	fixture := loadVariantFixture(t)
	file := parseMailfauna(t)

	for _, sw := range toWireSwitches {
		want, known := fixture[sw.rustEnum][sw.defaultVariant]
		if !known {
			t.Fatalf("%s: %s has no %s variant in the fixture — the fail-safe landing "+
				"point this arm was ruled onto is gone; re-rule the arm rather than "+
				"relaxing this test", sw.fn, sw.rustEnum, sw.defaultVariant)
		}
		_, got := switchArms(t, file, sw.fn, sw.rustEnum)
		if got == "" {
			t.Errorf("%s has no default arm emitting a Kind — an unmappable variant "+
				"must land somewhere ruled, not on a zero value", sw.fn)
			continue
		}
		if got != want {
			t.Errorf("%s's default arm emits %q, want %q (%s::%s).\n\nWhy: %s",
				sw.fn, got, want, sw.rustEnum, sw.defaultVariant, sw.why)
		}
	}
}

// TestEveryToWireSwitchIsUnderContract stops the table above from silently
// falling behind mailfauna.go: a new `*ToWire` translator for an auth type with
// no row here would be exactly as unpinned as the five were before this test.
func TestEveryToWireSwitchIsUnderContract(t *testing.T) {
	fixture := loadVariantFixture(t)
	covered := map[string]bool{}
	for _, sw := range toWireSwitches {
		covered[sw.rustEnum] = true
	}
	for rustEnum := range fixture {
		if !covered[rustEnum] {
			t.Errorf("the fixture knows %s but no toWireSwitches row translates it — "+
				"either mailfauna.go is missing a switch, or this table is", rustEnum)
		}
	}
}
