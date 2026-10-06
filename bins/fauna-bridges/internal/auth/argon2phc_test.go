package auth

import (
	"bufio"
	"encoding/base64"
	"os"
	"strings"
	"testing"

	"golang.org/x/crypto/argon2"
)

// lightPHC mints a PHC string with cheap parameters so tests don't pay the
// production 64 MiB per hash (mirrors the Rust twin's `light_phc`).
func lightPHC(t *testing.T, secret string) string {
	t.Helper()
	salt := []byte("0123456789abcdef")
	hash := argon2.IDKey([]byte(secret), salt, 1, 4096, 1, 32)
	enc := base64.RawStdEncoding.EncodeToString
	return "$argon2id$v=19$m=4096,t=1,p=1$" + enc(salt) + "$" + enc(hash)
}

func TestVerifyArgon2PHCRoundTrip(t *testing.T) {
	phc := lightPHC(t, "3ssn-cuqp-4u7r-farx")
	if !VerifyArgon2PHC("3ssn-cuqp-4u7r-farx", phc) {
		t.Fatal("correct secret rejected")
	}
	if VerifyArgon2PHC("aaaa-bbbb-cccc-dddd", phc) {
		t.Fatal("wrong secret accepted")
	}
	if VerifyArgon2PHC("", phc) {
		t.Fatal("empty secret accepted")
	}
}

func TestVerifyArgon2PHCRejectsMalformed(t *testing.T) {
	phc := lightPHC(t, "secret")
	cases := map[string]string{
		"empty":              "",
		"not phc":            "not-a-phc-string",
		"wrong variant":      strings.Replace(phc, "argon2id", "argon2i", 1),
		"wrong version":      strings.Replace(phc, "v=19", "v=16", 1),
		"missing hash field": phc[:strings.LastIndex(phc, "$")],
		"bad salt b64":       strings.Replace(phc, "$m=4096,t=1,p=1$", "$m=4096,t=1,p=1$!!!$", 1),
		"zero memory":        strings.Replace(phc, "m=4096", "m=0", 1),
		"huge memory":        strings.Replace(phc, "m=4096", "m=999999999", 1), // > 1 GiB cap → refused pre-derive
		"huge time":          strings.Replace(phc, "t=1", "t=9999", 1),
		"huge threads":       strings.Replace(phc, "p=1", "p=250", 1),
		"duplicate key":      strings.Replace(phc, "m=4096", "m=4096,m=4096", 1),
		"unknown key":        strings.Replace(phc, "p=1", "p=1,x=3", 1),
		"padded b64 salt":    strings.Replace(phc, "$m=4096,t=1,p=1$", "$m=4096,t=1,p=1$QUJDRA==$", 1),
	}
	for name, bad := range cases {
		if VerifyArgon2PHC("secret", bad) {
			t.Errorf("%s: malformed PHC accepted: %q", name, bad)
		}
	}
}

// TestCrossLanguageFixture pins this verifier to the Rust minter's exact
// output: the committed fixture was produced once by
// libs/fauna-client-bridges/src/atproto_credential.rs
// `compute_app_credential_verifier` under the production parameters
// (m=65536,t=2,p=1). The matching Rust test is
// `cross_language_fixture_verifies`. One production-cost derive (~64 MiB).
func TestCrossLanguageFixture(t *testing.T) {
	f, err := os.Open("testdata/atproto_app_credential_phc.txt")
	if err != nil {
		t.Fatalf("open fixture: %v", err)
	}
	defer f.Close()
	var lines []string
	sc := bufio.NewScanner(f)
	for sc.Scan() {
		line := strings.TrimSpace(sc.Text())
		if line == "" || strings.HasPrefix(line, "#") {
			continue
		}
		lines = append(lines, line)
	}
	if err := sc.Err(); err != nil || len(lines) != 2 {
		t.Fatalf("fixture shape: err=%v lines=%d (want 2)", err, len(lines))
	}
	secret, phc := lines[0], lines[1]
	if !strings.HasPrefix(phc, "$argon2id$v=19$m=65536,t=2,p=1$") {
		t.Fatalf("fixture PHC is not the production shape: %s", RedactPHC(phc))
	}
	if !VerifyArgon2PHC(secret, phc) {
		t.Fatal("Go verifier rejects the Rust-minted production fixture")
	}
	if VerifyArgon2PHC("aaaa-bbbb-cccc-dddd", phc) {
		t.Fatal("Go verifier accepts a wrong secret against the fixture")
	}
}

func TestRedactPHC(t *testing.T) {
	phc := lightPHC(t, "secret")
	red := RedactPHC(phc)
	if strings.Contains(red, phc[strings.LastIndex(phc, "$")+1:]) {
		t.Fatalf("redacted form leaks hash: %s", red)
	}
	if !strings.Contains(red, "m=4096,t=1,p=1") {
		t.Fatalf("redacted form lost parameters: %s", red)
	}
	if RedactPHC("junk") != "<malformed-phc>" {
		t.Fatal("malformed input not flagged")
	}
}
