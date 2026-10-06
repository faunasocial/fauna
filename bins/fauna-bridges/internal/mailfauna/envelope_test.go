package mailfauna

import (
	"testing"
)

const envelopeSimpleFixture = "From: Alice <alice@example.com>\r\n" +
	"To: Bob <bob@example.com>\r\n" +
	"Subject: Hi\r\n" +
	"Date: Wed, 12 Mar 2026 10:30:00 -0700\r\n" +
	"\r\n" +
	"body\r\n"

const envelopeReplyFixture = "From: alice@example.com\r\n" +
	"Reply-To: noreply@example.com\r\n" +
	"Subject: Re: Hi\r\n" +
	"Message-ID: <child@example.com>\r\n" +
	"In-Reply-To: <parent@example.com>\r\n" +
	"\r\n" +
	"body\r\n"

const envelopeMultiToCcFixture = "From: a@example.com\r\n" +
	"To: b@example.com, c@example.com\r\n" +
	"Cc: d@example.com, e@example.com\r\n" +
	"Subject: Many\r\n" +
	"\r\n" +
	"body\r\n"

func TestDeriveEnvelopeSimple(t *testing.T) {
	env, err := DeriveEnvelope([]byte(envelopeSimpleFixture))
	if err != nil {
		t.Fatalf("DeriveEnvelope: %v", err)
	}
	if env.Subject == nil || *env.Subject != "Hi" {
		t.Errorf("Subject: want Hi, got %v", env.Subject)
	}
	if env.Date == nil || *env.Date != "2026-03-12T10:30:00-07:00" {
		t.Errorf("Date: want 2026-03-12T10:30:00-07:00, got %v", env.Date)
	}
	if len(env.From) != 1 {
		t.Fatalf("From: want 1 entry, got %d", len(env.From))
	}
	if env.From[0].Mailbox != "alice" || env.From[0].Host != "example.com" {
		t.Errorf("From[0]: want alice@example.com, got %s@%s", env.From[0].Mailbox, env.From[0].Host)
	}
	if env.From[0].Personal == nil || *env.From[0].Personal != "Alice" {
		t.Errorf("From[0].Personal: want Alice, got %v", env.From[0].Personal)
	}
	if len(env.To) != 1 || env.To[0].Mailbox != "bob" {
		t.Errorf("To: want [bob], got %+v", env.To)
	}
}

func TestDeriveEnvelopeReplyAndMessageID(t *testing.T) {
	env, err := DeriveEnvelope([]byte(envelopeReplyFixture))
	if err != nil {
		t.Fatalf("DeriveEnvelope: %v", err)
	}
	if len(env.ReplyTo) != 1 || env.ReplyTo[0].Mailbox != "noreply" {
		t.Errorf("ReplyTo: want [noreply], got %+v", env.ReplyTo)
	}
	if env.MessageId == nil || *env.MessageId != "<child@example.com>" {
		t.Errorf("MessageId: want <child@example.com>, got %v", env.MessageId)
	}
	if env.InReplyTo == nil || *env.InReplyTo != "<parent@example.com>" {
		t.Errorf("InReplyTo: want <parent@example.com>, got %v", env.InReplyTo)
	}
}

func TestDeriveEnvelopeMultiToCc(t *testing.T) {
	env, err := DeriveEnvelope([]byte(envelopeMultiToCcFixture))
	if err != nil {
		t.Fatalf("DeriveEnvelope: %v", err)
	}
	if len(env.To) != 2 {
		t.Fatalf("To: want 2 entries, got %d", len(env.To))
	}
	if env.To[0].Mailbox != "b" || env.To[1].Mailbox != "c" {
		t.Errorf("To: want [b, c], got [%s, %s]", env.To[0].Mailbox, env.To[1].Mailbox)
	}
	if len(env.Cc) != 2 {
		t.Fatalf("Cc: want 2 entries, got %d", len(env.Cc))
	}
	if env.Cc[0].Mailbox != "d" || env.Cc[1].Mailbox != "e" {
		t.Errorf("Cc: want [d, e], got [%s, %s]", env.Cc[0].Mailbox, env.Cc[1].Mailbox)
	}
}
