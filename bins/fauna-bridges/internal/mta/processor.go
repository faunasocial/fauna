package mta

import (
	"context"
	"log/slog"
	"time"

	gosmtp "github.com/emersion/go-smtp"

	"github.com/faunasocial/fauna/bins/fauna-bridges/internal/metrics"
	"github.com/faunasocial/fauna/bins/fauna-bridges/internal/wsrpc"
)

// processorIngestTimeout bounds the hand-off of one processor-bound message.
// The nest decompresses and parses a report of up to 50 MiB inside the call.
const processorIngestTimeout = 30 * time.Second

// noAuthVerdicts is the RFC-canonical "no determination made" verdict set, for
// a processor hand-off that ran no inbound authentication (the submission door:
// the sender is an authenticated local user).
func noAuthVerdicts() wsrpc.AuthVerdicts {
	return wsrpc.AuthVerdicts{
		Dkim:  wsrpc.DkimVerdict{Kind: "none"},
		Spf:   wsrpc.SpfVerdict{Kind: "none"},
		Dmarc: wsrpc.DmarcVerdict{Kind: "none"},
		Arc:   wsrpc.ArcVerdict{Kind: "none"},
	}
}

// dispatchProcessor hands a message whose RCPT resolved to a nest processor
// (resolve_recipient's Processor outcome — dmarc-report@, the DMARC report
// ingest, dmarc-reporting.md § Envelope-time routing; tlsrpt@, the TLSRPT
// ingest, smtp-server.md § TLSRPT inbound report listener) to that
// processor instead of any mailbox. Whatever the nest made of the report —
// stored, duplicate, malformed, oversize, about a domain we do not serve — the
// recipient is done: accept-on-malformed means the sender gets 250
// (§ Architectural rules). Only a transport / nest fault returns an error, a
// 451 so the sender retries and the report is not lost.
//
// Shared by the inbound MX path and the submission path so both doors reach the
// processor the same way.
func dispatchProcessor(caller wsrpc.Caller, logger *slog.Logger, processor string, raw []byte, env wsrpc.ReportSourceEnvelope, receivedAt int64) *gosmtp.SMTPError {
	if logger == nil {
		logger = slog.Default()
	}
	var (
		ingest func(context.Context, wsrpc.Caller, []byte, wsrpc.ReportSourceEnvelope, int64) (wsrpc.IngestReportReply, error)
		label  string // metric-label prefix and log noun
	)
	switch processor {
	case wsrpc.ProcessorDmarcReport:
		ingest, label = wsrpc.IngestDmarcReport, "dmarc_report"
	case wsrpc.ProcessorTlsrptInbound:
		ingest, label = wsrpc.IngestTlsrptReport, "tlsrpt_report"
	default:
		// ResolveRecipient refuses an unknown processor, so this is unreachable;
		// tempfail rather than drop if a future one slips past it.
		logger.Error("smtp: unknown recipient processor", "processor", processor)
		return &gosmtp.SMTPError{
			Code:         451,
			EnhancedCode: gosmtp.EnhancedCode{4, 3, 0},
			Message:      "Recipient processing temporarily unavailable; try again later",
		}
	}
	ctx, cancel := context.WithTimeout(context.Background(), processorIngestTimeout)
	defer cancel()
	reply, err := ingest(ctx, caller, raw, env, receivedAt)
	if err != nil {
		logger.Warn("smtp: report hand-off failed (→451)", "processor", processor,
			"client_ip", env.ClientIP, "mail_from", env.MailFrom, "err", err)
		return &gosmtp.SMTPError{
			Code:         451,
			EnhancedCode: gosmtp.EnhancedCode{4, 3, 0},
			Message:      "Report processing temporarily unavailable; try again later",
		}
	}
	metrics.SMTPInboundMessagesTotal.WithLabelValues(label + "_" + reply.Outcome).Inc()
	logger.Info("smtp: report handed to the nest", "processor", processor,
		"client_ip", env.ClientIP, "mail_from", env.MailFrom,
		"kind", reply.Kind, "outcome", reply.Outcome)
	return nil
}
