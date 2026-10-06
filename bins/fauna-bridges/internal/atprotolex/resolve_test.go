package atprotolex

import (
	"context"
	"errors"
	"strings"
	"testing"
)

// stubResolver answers from a fixed map; a missing name is NXDOMAIN, which is
// an error rather than an empty success — the same shape the real resolver and
// the harness's fake both produce.
type stubResolver struct {
	records map[string][]string
	asked   []string
}

func (s *stubResolver) LookupTXT(_ context.Context, name string) ([]string, error) {
	s.asked = append(s.asked, name)
	records, ok := s.records[name]
	if !ok {
		return nil, errors.New("NXDOMAIN")
	}
	return records, nil
}

func TestAuthorityDomainReversesEverySegmentButTheName(t *testing.T) {
	for _, tc := range []struct{ nsid, want string }{
		{"com.example.calendar.appPerms", "calendar.example.com"},
		{"com.example.fooBar", "example.com"},
		{"app.bsky.feed.post", "feed.bsky.app"},
		{"com.example.a.b.c.thing", "c.b.a.example.com"},
	} {
		got, err := AuthorityDomain(tc.nsid)
		if err != nil {
			t.Fatalf("AuthorityDomain(%q): %v", tc.nsid, err)
		}
		if got != tc.want {
			t.Errorf("AuthorityDomain(%q) = %q, want %q", tc.nsid, got, tc.want)
		}
	}
}

// The authority is the same group the pure expander confines members to — the
// hierarchy constraint restated, so the two must not drift apart.
func TestTheAuthorityIsTheGroupTheHierarchyConstraintUses(t *testing.T) {
	const set = "com.example.calendar.appPerms"
	authority, err := AuthorityDomain(set)
	if err != nil {
		t.Fatal(err)
	}
	// The expander's group, spelled forward.
	group := set[:strings.LastIndex(set, ".")]
	segments := strings.Split(group, ".")
	for i, j := 0, len(segments)-1; i < j; i, j = i+1, j-1 {
		segments[i], segments[j] = segments[j], segments[i]
	}
	if want := strings.Join(segments, "."); authority != want {
		t.Errorf("authority %q is not the reversed expander group %q", authority, want)
	}
}

func TestAuthorityDomainRefusesAQueryItCannotBuild(t *testing.T) {
	for _, bad := range []string{"com.example", "com", "", "com..appPerms"} {
		if got, err := AuthorityDomain(bad); err == nil {
			t.Errorf("AuthorityDomain(%q) = %q, want an error", bad, got)
		}
	}
}

func TestResolveAuthorityDIDReadsTheDidRecord(t *testing.T) {
	r := &stubResolver{records: map[string][]string{
		"_lexicon.calendar.example.com": {"did=did:plc:abc123"},
	}}
	got, err := ResolveAuthorityDID(context.Background(), r, "com.example.calendar.appPerms")
	if err != nil {
		t.Fatal(err)
	}
	if got != "did:plc:abc123" {
		t.Errorf("got %q", got)
	}
	if len(r.asked) != 1 || r.asked[0] != "_lexicon.calendar.example.com" {
		t.Errorf("queried %v", r.asked)
	}
}

// TXT is a shared namespace: an unrelated record at the same name is ordinary.
func TestUnrelatedTxtRecordsAreIgnoredNotRefused(t *testing.T) {
	r := &stubResolver{records: map[string][]string{
		"_lexicon.calendar.example.com": {
			"v=spf1 -all",
			"  did=did:plc:abc123  ",
			"some-other-verification=xyz",
		},
	}}
	got, err := ResolveAuthorityDID(context.Background(), r, "com.example.calendar.appPerms")
	if err != nil {
		t.Fatal(err)
	}
	if got != "did:plc:abc123" {
		t.Errorf("got %q", got)
	}
}

// Two publishers claiming one namespace is not a tie to break: choosing either
// would mean this server silently decided who speaks for the authority.
func TestConflictingDidRecordsRefuseRatherThanPickOne(t *testing.T) {
	r := &stubResolver{records: map[string][]string{
		"_lexicon.calendar.example.com": {"did=did:plc:abc123", "did=did:plc:def456"},
	}}
	_, err := ResolveAuthorityDID(context.Background(), r, "com.example.calendar.appPerms")
	if err == nil {
		t.Fatal("expected a refusal")
	}
	if !strings.Contains(err.Error(), "conflicting") {
		t.Errorf("unexpected error: %v", err)
	}
}

// The same DID published twice is not a conflict — a duplicated record is a
// common DNS shape and says nothing contradictory.
func TestADuplicatedIdenticalRecordResolves(t *testing.T) {
	r := &stubResolver{records: map[string][]string{
		"_lexicon.calendar.example.com": {"did=did:plc:abc123", "did=did:plc:abc123"},
	}}
	got, err := ResolveAuthorityDID(context.Background(), r, "com.example.calendar.appPerms")
	if err != nil || got != "did:plc:abc123" {
		t.Fatalf("got %q, %v", got, err)
	}
}

func TestAMissingOrNonDidRecordFailsResolution(t *testing.T) {
	cases := map[string][]string{
		"no did= record at all":     {"v=spf1 -all"},
		"an empty did= value":       {"did="},
		"a value that is not a DID": {"did=example.com"},
	}
	for name, records := range cases {
		r := &stubResolver{records: map[string][]string{
			"_lexicon.calendar.example.com": records,
		}}
		if _, err := ResolveAuthorityDID(context.Background(), r, "com.example.calendar.appPerms"); err == nil {
			t.Errorf("%s: expected a refusal", name)
		}
	}
}

// NXDOMAIN is a failed resolution, and the error names the query it made — so
// the refusal that reaches the client developer says which record the set's
// authority never published, rather than an unattributable "could not resolve".
func TestNxdomainFailsAndNamesTheQuery(t *testing.T) {
	r := &stubResolver{records: map[string][]string{}}
	_, err := ResolveAuthorityDID(context.Background(), r, "com.example.calendar.appPerms")
	if err == nil {
		t.Fatal("expected a refusal")
	}
	if !strings.Contains(err.Error(), "_lexicon.calendar.example.com") {
		t.Errorf("error does not name the query: %v", err)
	}
}
