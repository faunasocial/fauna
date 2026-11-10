package httpdrain

import (
	"go/ast"
	"go/parser"
	"go/token"
	"io/fs"
	"path/filepath"
	"strings"
	"testing"
)

// TestEveryBridgeListenerDrainsWhatItsHandlersLeave sweeps every http.Server
// the bridges build, rather than the listeners a test remembered to name: the
// reset is a property of the LISTENER (any route that answers before reading,
// any refusal written ahead of a body), so one unwrapped server brings it back
// for every route on it. Each `http.Server{...}` literal in non-test bridge
// source must set `Handler: httpdrain.Handler(...)`.
//
// The vendored tree (third_party/) is skipped: it is not ours to change, and
// the bridges never hand it a listener of their own.
func TestEveryBridgeListenerDrainsWhatItsHandlersLeave(t *testing.T) {
	root, err := filepath.Abs(filepath.Join("..", ".."))
	if err != nil {
		t.Fatal(err)
	}
	fset := token.NewFileSet()
	var servers, offenders []string
	err = filepath.WalkDir(root, func(path string, d fs.DirEntry, err error) error {
		if err != nil {
			return err
		}
		if d.IsDir() {
			switch d.Name() {
			case "third_party", "vendor", "testdata":
				return filepath.SkipDir
			}
			if strings.HasPrefix(d.Name(), ".") && path != root {
				return filepath.SkipDir
			}
			return nil
		}
		if !strings.HasSuffix(path, ".go") || strings.HasSuffix(path, "_test.go") {
			return nil
		}
		file, perr := parser.ParseFile(fset, path, nil, 0)
		if perr != nil {
			return perr
		}
		ast.Inspect(file, func(n ast.Node) bool {
			lit, ok := n.(*ast.CompositeLit)
			if !ok || !isSelector(lit.Type, "http", "Server") {
				return true
			}
			where := fset.Position(lit.Pos()).String()
			servers = append(servers, where)
			wrapped := false
			for _, elt := range lit.Elts {
				kv, ok := elt.(*ast.KeyValueExpr)
				if !ok {
					continue
				}
				if key, ok := kv.Key.(*ast.Ident); !ok || key.Name != "Handler" {
					continue
				}
				if call, ok := kv.Value.(*ast.CallExpr); ok && isSelector(call.Fun, "httpdrain", "Handler") {
					wrapped = true
				}
			}
			if !wrapped {
				offenders = append(offenders, where)
			}
			return true
		})
		return nil
	})
	if err != nil {
		t.Fatal(err)
	}
	if len(servers) == 0 {
		t.Fatalf("found no http.Server literal under %s — the sweep is not looking where the listeners are", root)
	}
	if len(offenders) > 0 {
		t.Fatalf("%d of %d bridge http.Server(s) do not wrap their Handler in httpdrain.Handler, so a reply "+
			"written before the body is read can reach a Connection: close client as a reset on Windows:\n  %s",
			len(offenders), len(servers), strings.Join(offenders, "\n  "))
	}
}

func isSelector(e ast.Expr, pkg, name string) bool {
	sel, ok := e.(*ast.SelectorExpr)
	if !ok || sel.Sel.Name != name {
		return false
	}
	id, ok := sel.X.(*ast.Ident)
	return ok && id.Name == pkg
}
