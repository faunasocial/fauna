package mailfauna

import (
	"testing"
)

const textPlainFixture = "From: a@example.com\r\n" +
	"To: b@example.com\r\n" +
	"MIME-Version: 1.0\r\n" +
	"Content-Type: text/plain; charset=utf-8\r\n" +
	"\r\n" +
	"Hello\r\nworld\r\nfinal\r\n"

const multipartAlternativeFixture = "From: a@example.com\r\n" +
	"MIME-Version: 1.0\r\n" +
	"Content-Type: multipart/alternative; boundary=\"BNDR\"\r\n" +
	"\r\n" +
	"--BNDR\r\n" +
	"Content-Type: text/plain; charset=utf-8\r\n" +
	"\r\n" +
	"plain body\r\n" +
	"--BNDR\r\n" +
	"Content-Type: text/html; charset=utf-8\r\n" +
	"\r\n" +
	"<p>html body</p>\r\n" +
	"--BNDR--\r\n"

const multipartMixedAttachmentFixture = "From: a@example.com\r\n" +
	"MIME-Version: 1.0\r\n" +
	"Content-Type: multipart/mixed; boundary=\"BNDR\"\r\n" +
	"\r\n" +
	"--BNDR\r\n" +
	"Content-Type: text/plain; charset=utf-8\r\n" +
	"\r\n" +
	"the body\r\n" +
	"--BNDR\r\n" +
	"Content-Type: application/octet-stream; name=\"data.bin\"\r\n" +
	"Content-Disposition: attachment; filename=\"data.bin\"\r\n" +
	"Content-Transfer-Encoding: base64\r\n" +
	"\r\n" +
	"QUFB\r\n" +
	"--BNDR--\r\n"

func TestDeriveBodyStructureTextPlain(t *testing.T) {
	bs, err := DeriveBodyStructure([]byte(textPlainFixture))
	if err != nil {
		t.Fatalf("DeriveBodyStructure: %v", err)
	}
	if bs.Type != "TEXT" {
		t.Errorf("Type: want %q, got %q", "TEXT", bs.Type)
	}
	if bs.Subtype != "PLAIN" {
		t.Errorf("Subtype: want %q, got %q", "PLAIN", bs.Subtype)
	}
	if len(bs.Parts) != 0 {
		t.Errorf("Parts: want empty, got %d entries", len(bs.Parts))
	}
	foundCharset := false
	for _, p := range bs.Parameters {
		if p.Name == "CHARSET" && (p.Value == "utf-8" || p.Value == "UTF-8") {
			foundCharset = true
			break
		}
	}
	if !foundCharset {
		t.Errorf("Parameters: missing CHARSET=utf-8, got %+v", bs.Parameters)
	}
	if bs.Lines == nil || *bs.Lines != 3 {
		t.Errorf("Lines: want 3, got %v", bs.Lines)
	}
}

func TestDeriveBodyStructureMultipartAlternative(t *testing.T) {
	bs, err := DeriveBodyStructure([]byte(multipartAlternativeFixture))
	if err != nil {
		t.Fatalf("DeriveBodyStructure: %v", err)
	}
	if bs.Type != "MULTIPART" {
		t.Errorf("Type: want %q, got %q", "MULTIPART", bs.Type)
	}
	if bs.Subtype != "ALTERNATIVE" {
		t.Errorf("Subtype: want %q, got %q", "ALTERNATIVE", bs.Subtype)
	}
	if len(bs.Parts) != 2 {
		t.Fatalf("Parts: want 2 children, got %d", len(bs.Parts))
	}
	if bs.Parts[0].Type != "TEXT" || bs.Parts[0].Subtype != "PLAIN" {
		t.Errorf("Parts[0]: want TEXT/PLAIN, got %s/%s", bs.Parts[0].Type, bs.Parts[0].Subtype)
	}
	if bs.Parts[1].Type != "TEXT" || bs.Parts[1].Subtype != "HTML" {
		t.Errorf("Parts[1]: want TEXT/HTML, got %s/%s", bs.Parts[1].Type, bs.Parts[1].Subtype)
	}
}

func TestDeriveBodyStructureAttachment(t *testing.T) {
	bs, err := DeriveBodyStructure([]byte(multipartMixedAttachmentFixture))
	if err != nil {
		t.Fatalf("DeriveBodyStructure: %v", err)
	}
	if len(bs.Parts) != 2 {
		t.Fatalf("Parts: want 2 children, got %d", len(bs.Parts))
	}
	attach := bs.Parts[1]
	if attach.Type != "APPLICATION" || attach.Subtype != "OCTET-STREAM" {
		t.Errorf("attach: want APPLICATION/OCTET-STREAM, got %s/%s", attach.Type, attach.Subtype)
	}
	if attach.Disposition == nil || *attach.Disposition != "ATTACHMENT" {
		t.Errorf("attach.Disposition: want ATTACHMENT, got %v", attach.Disposition)
	}
	foundFilename := false
	for _, p := range attach.DispositionParameters {
		if p.Name == "FILENAME" && p.Value == "data.bin" {
			foundFilename = true
			break
		}
	}
	if !foundFilename {
		t.Errorf("DispositionParameters: missing FILENAME=data.bin, got %+v", attach.DispositionParameters)
	}
}
