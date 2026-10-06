package webdav

import (
	"encoding/xml"
	"strings"
	"testing"

	"github.com/faunasocial/fauna/bins/fauna-bridges/internal/wsrpc"
)

func u64(v uint64) *uint64 { return &v }

func TestStripQuotaPropsCutsOnlyTheQuotaElements(t *testing.T) {
	body := `<?xml version="1.0"?><d:propfind xmlns:d="DAV:"><d:prop>` +
		`<d:displayname/><d:quota-available-bytes/><d:getetag/>` +
		`<d:quota-used-bytes></d:quota-used-bytes></d:prop></d:propfind>`
	out, wanted := stripQuotaProps([]byte(body))
	if len(wanted) != 2 || wanted[0] != quotaAvailableName || wanted[1] != quotaUsedName {
		t.Fatalf("wanted = %v", wanted)
	}
	got := string(out)
	if strings.Contains(got, "quota") {
		t.Fatalf("quota elements survived the strip: %s", got)
	}
	for _, keep := range []string{"<d:displayname/>", "<d:getetag/>", "</d:prop></d:propfind>"} {
		if !strings.Contains(got, keep) {
			t.Fatalf("strip lost %q: %s", keep, got)
		}
	}
}

func TestStripQuotaPropsLeavesAllpropAndForeignNamespacesAlone(t *testing.T) {
	for _, body := range []string{
		`<d:propfind xmlns:d="DAV:"><d:allprop/></d:propfind>`,
		// Same local name, not the DAV: namespace — not ours to answer.
		`<d:propfind xmlns:d="DAV:" xmlns:x="urn:x"><d:prop><x:quota-used-bytes/></d:prop></d:propfind>`,
		``,
		`<not xml`,
	} {
		out, wanted := stripQuotaProps([]byte(body))
		if len(wanted) != 0 || string(out) != body {
			t.Fatalf("body %q: wanted=%v out=%q", body, wanted, out)
		}
	}
}

const sampleMultistatus = `<?xml version="1.0" encoding="UTF-8"?>
<D:multistatus xmlns:D="DAV:"><D:response><D:href>/webdav/a%40x.test/</D:href><D:propstat><D:prop><D:displayname></D:displayname></D:prop><D:status>HTTP/1.1 200 OK</D:status></D:propstat></D:response><D:response><D:href>/webdav/a%40x.test/docs/</D:href><D:propstat><D:prop></D:prop><D:status>HTTP/1.1 200 OK</D:status></D:propstat></D:response><D:response><D:href>/webdav/a%40x.test/docs/f.txt</D:href><D:propstat><D:prop></D:prop><D:status>HTTP/1.1 200 OK</D:status></D:propstat></D:response></D:multistatus>`

type parsedResponse struct {
	Href      string `xml:"href"`
	Propstats []struct {
		Status string `xml:"status"`
		Prop   struct {
			Used      *string `xml:"quota-used-bytes"`
			Available *string `xml:"quota-available-bytes"`
		} `xml:"prop"`
	} `xml:"propstat"`
}

func parseMS(t *testing.T, b []byte) []parsedResponse {
	t.Helper()
	var ms struct {
		Responses []parsedResponse `xml:"response"`
	}
	if err := xml.Unmarshal(b, &ms); err != nil {
		t.Fatalf("spliced multistatus does not parse: %v\n%s", err, b)
	}
	return ms.Responses
}

func TestSpliceQuotaReportsOnCollectionsOnly(t *testing.T) {
	quota := &wsrpc.WebdavQuota{Used: 600, Limit: u64(1000)}
	out, err := spliceQuota([]byte(sampleMultistatus), []xml.Name{quotaUsedName, quotaAvailableName}, quota)
	if err != nil {
		t.Fatal(err)
	}
	rs := parseMS(t, out)
	if len(rs) != 3 {
		t.Fatalf("responses = %d", len(rs))
	}
	for i, want := range []bool{true, true, false} {
		var used, avail string
		var notFound bool
		for _, ps := range rs[i].Propstats {
			if ps.Prop.Used != nil && strings.Contains(ps.Status, "200") {
				used = *ps.Prop.Used
			}
			if ps.Prop.Available != nil && strings.Contains(ps.Status, "200") {
				avail = *ps.Prop.Available
			}
			if strings.Contains(ps.Status, "404") && ps.Prop.Used != nil {
				notFound = true
			}
		}
		if want && (used != "600" || avail != "400") {
			t.Fatalf("%s: used=%q available=%q", rs[i].Href, used, avail)
		}
		if !want && (used != "" || !notFound) {
			t.Fatalf("%s: a file must answer the quota properties 404", rs[i].Href)
		}
	}
	// Everything the handler wrote is still there, verbatim.
	if !strings.Contains(string(out), `<D:displayname></D:displayname>`) {
		t.Fatalf("splice altered the handler's bytes:\n%s", out)
	}
}

func TestSpliceQuotaWithoutACeilingOrAQuotaReadsAvailableAsAbsent(t *testing.T) {
	out, err := spliceQuota([]byte(sampleMultistatus), []xml.Name{quotaUsedName, quotaAvailableName},
		&wsrpc.WebdavQuota{Used: 7})
	if err != nil {
		t.Fatal(err)
	}
	root := parseMS(t, out)[0]
	var used string
	var availMissing bool
	for _, ps := range root.Propstats {
		if ps.Prop.Used != nil && strings.Contains(ps.Status, "200") {
			used = *ps.Prop.Used
		}
		if ps.Prop.Available != nil && strings.Contains(ps.Status, "404") {
			availMissing = true
		}
	}
	if used != "7" || !availMissing {
		t.Fatalf("used=%q availableMissing=%v\n%s", used, availMissing, out)
	}

	out, err = spliceQuota([]byte(sampleMultistatus), []xml.Name{quotaUsedName}, nil)
	if err != nil {
		t.Fatal(err)
	}
	for _, ps := range parseMS(t, out)[0].Propstats {
		if ps.Prop.Used != nil && !strings.Contains(ps.Status, "404") {
			t.Fatalf("a failed quota read must answer 404, not %q", ps.Status)
		}
	}
}

func TestSpliceQuotaOverTheCeilingReportsNothingLeft(t *testing.T) {
	v, ok := quotaValue(quotaAvailableName, &wsrpc.WebdavQuota{Used: 1200, Limit: u64(1000)})
	if !ok || v != "0" {
		t.Fatalf("available = %q, %v", v, ok)
	}
}

func TestSpliceQuotaHonoursADefaultNamespaceDocument(t *testing.T) {
	ms := `<multistatus xmlns="DAV:"><response><href>/webdav/u/</href></response></multistatus>`
	out, err := spliceQuota([]byte(ms), []xml.Name{quotaUsedName}, &wsrpc.WebdavQuota{Used: 1})
	if err != nil {
		t.Fatal(err)
	}
	if !strings.Contains(string(out), `<propstat><prop><quota-used-bytes>1</quota-used-bytes></prop>`) {
		t.Fatalf("default-namespace splice:\n%s", out)
	}
	parseMS(t, out)
}
