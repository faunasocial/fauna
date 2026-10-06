package atprotoid

import (
	"encoding/json"
	"testing"
)

// TestBuildDIDWebDoc mirrors the S0 probe's DID-document shape assertions,
// parameterized: id/controller carry the did:web, the verification method is
// a Multikey under the #atproto fragment, and the atproto_pds service points
// at the endpoint.
func TestBuildDIDWebDoc(t *testing.T) {
	key, err := PrivateKeyFromK256Scalar(fixedTestScalar())
	if err != nil {
		t.Fatalf("PrivateKeyFromK256Scalar: %v", err)
	}
	didKey, err := DIDKeyForPrivate(key)
	if err != nil {
		t.Fatalf("DIDKeyForPrivate: %v", err)
	}
	mb, err := MultibaseFromDIDKey(didKey)
	if err != nil {
		t.Fatalf("MultibaseFromDIDKey: %v", err)
	}

	docBytes, err := BuildDIDWebDoc("alice.example.com", mb, "https://example.com")
	if err != nil {
		t.Fatalf("BuildDIDWebDoc: %v", err)
	}

	var doc struct {
		Context     []string `json:"@context"`
		ID          string   `json:"id"`
		AlsoKnownAs []string `json:"alsoKnownAs"`
		VM          []struct {
			ID                 string `json:"id"`
			Type               string `json:"type"`
			Controller         string `json:"controller"`
			PublicKeyMultibase string `json:"publicKeyMultibase"`
		} `json:"verificationMethod"`
		Service []struct {
			ID              string `json:"id"`
			Type            string `json:"type"`
			ServiceEndpoint string `json:"serviceEndpoint"`
		} `json:"service"`
	}
	if err := json.Unmarshal(docBytes, &doc); err != nil {
		t.Fatalf("doc is not JSON: %v", err)
	}
	if doc.ID != "did:web:alice.example.com" {
		t.Errorf("id = %q, want did:web:alice.example.com", doc.ID)
	}
	if len(doc.AlsoKnownAs) != 1 || doc.AlsoKnownAs[0] != "at://alice.example.com" {
		t.Errorf("alsoKnownAs = %v, want [at://alice.example.com]", doc.AlsoKnownAs)
	}
	if len(doc.VM) != 1 {
		t.Fatalf("verificationMethod len = %d, want 1", len(doc.VM))
	}
	vm := doc.VM[0]
	if vm.ID != "did:web:alice.example.com#atproto" {
		t.Errorf("vm id = %q, want the #atproto fragment", vm.ID)
	}
	if vm.Type != "Multikey" {
		t.Errorf("vm type = %q, want Multikey", vm.Type)
	}
	if vm.Controller != "did:web:alice.example.com" {
		t.Errorf("vm controller = %q, want the did:web", vm.Controller)
	}
	if vm.PublicKeyMultibase != mb {
		t.Errorf("publicKeyMultibase = %q, want %q", vm.PublicKeyMultibase, mb)
	}
	if len(doc.Service) != 1 {
		t.Fatalf("service len = %d, want 1", len(doc.Service))
	}
	svc := doc.Service[0]
	if svc.ID != "#atproto_pds" || svc.Type != "AtprotoPersonalDataServer" || svc.ServiceEndpoint != "https://example.com" {
		t.Errorf("service = %+v, want #atproto_pds AtprotoPersonalDataServer https://example.com", svc)
	}
	if len(doc.Context) == 0 || doc.Context[0] != "https://www.w3.org/ns/did/v1" {
		t.Errorf("@context = %v, want did/v1 first", doc.Context)
	}
}

// TestBuildServiceDIDWebDoc pins the PDS *service* DID document: it names the
// PDS at this host and nothing else. Specifically it must NOT carry the user
// shape — no alsoKnownAs handle, no repo signing key — because a service is not
// an account, and publishing a service-level verification method would imply a
// signing role the design deliberately does not give it (C7: service-auth JWTs
// are signed with each account's own repo key).
func TestBuildServiceDIDWebDoc(t *testing.T) {
	docBytes, err := BuildServiceDIDWebDoc("pds.example.com")
	if err != nil {
		t.Fatalf("BuildServiceDIDWebDoc: %v", err)
	}
	var doc struct {
		Context     []string `json:"@context"`
		ID          string   `json:"id"`
		AlsoKnownAs []any    `json:"alsoKnownAs"`
		VM          []any    `json:"verificationMethod"`
		Service     []struct {
			ID              string `json:"id"`
			Type            string `json:"type"`
			ServiceEndpoint string `json:"serviceEndpoint"`
		} `json:"service"`
	}
	if err := json.Unmarshal(docBytes, &doc); err != nil {
		t.Fatalf("doc is not JSON: %v", err)
	}
	if doc.ID != "did:web:pds.example.com" {
		t.Errorf("id = %q, want did:web:pds.example.com", doc.ID)
	}
	if len(doc.AlsoKnownAs) != 0 {
		t.Errorf("service doc carries alsoKnownAs %v; a service has no handle", doc.AlsoKnownAs)
	}
	if len(doc.VM) != 0 {
		t.Errorf("service doc carries a verificationMethod %v; none is defined today", doc.VM)
	}
	if len(doc.Service) != 1 {
		t.Fatalf("service len = %d, want 1", len(doc.Service))
	}
	svc := doc.Service[0]
	if svc.ID != "#atproto_pds" || svc.Type != "AtprotoPersonalDataServer" {
		t.Errorf("service = %+v, want #atproto_pds AtprotoPersonalDataServer", svc)
	}
	if svc.ServiceEndpoint != "https://pds.example.com" {
		t.Errorf("serviceEndpoint = %q, want https://pds.example.com", svc.ServiceEndpoint)
	}
	if len(doc.Context) == 0 || doc.Context[0] != "https://www.w3.org/ns/did/v1" {
		t.Errorf("@context = %v, want did/v1 first", doc.Context)
	}
}
