package logplane

import "fmt"

// This file is the **mail bridge's own** part of the log-plane catalogue — the
// events only the MTA/MDA binary reports, on top of the shared set in
// catalogue.go. The authoring rules there (§ Deliverability, § The
// interpolation rule) bind here unchanged.

// mailEventIDs is every event id this file defines.
var mailEventIDs = []string{
	"scanner_unavailable",
	"scanner_recovered",
}

// ScannerName names one of the two third-party content scanners the MTA's
// circuit breakers guard. ClamAV and rspamd are never plane sources (their
// free-form container logs are exactly what the plane excludes); their *effect*
// on a box is the MTA's fail-closed breaker, which the MTA reports here. The
// type is a fixed pair so the name is a bounded class by construction — a call
// site can never hand over the error string it holds.
type ScannerName string

const (
	ScannerClamd  ScannerName = "clamd"
	ScannerRspamd ScannerName = "rspamd"
)

func safeScanner(s ScannerName) string {
	switch s {
	case ScannerClamd, ScannerRspamd:
		return string(s)
	}
	return "unknown"
}

// ScannerUnavailable — a scanner's circuit breaker went closed→open. While it is
// open every inbound message tempfails (fail-closed, never allow-without-scan),
// and nothing else on the admin surface says why. Call it on the *transition*
// only, never per tempfail, so a flapping scanner is bounded by the plane's own
// rate limit and the ring is not flooded.
//
// Deliverable by the § Deliverability rule: the MTA holds its authenticated nest
// link while it serves inbound mail, and the source-side queue delivers the
// event once the channel is back.
func ScannerUnavailable(scanner ScannerName) {
	Emit(LevelWarn, "scanner_unavailable",
		fmt.Sprintf("scanner %s unavailable: breaker open, inbound mail tempfails", safeScanner(scanner)))
}

// ScannerRecovered — the breaker's half-open trial succeeded and it closed
// again. The "it came back" counterpart of ScannerUnavailable; transition-only.
func ScannerRecovered(scanner ScannerName) {
	Emit(LevelInfo, "scanner_recovered",
		fmt.Sprintf("scanner %s recovered: breaker closed, inbound scanning resumed", safeScanner(scanner)))
}
