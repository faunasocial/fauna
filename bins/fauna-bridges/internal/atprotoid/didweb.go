package atprotoid

import (
	"encoding/json"
	"fmt"
)

// DIDWeb constructs the did:web identifier for an ATProto handle — no
// directory involved; the DID *is* the handle's domain.
func DIDWeb(handle string) string {
	return "did:web:" + handle
}

// BuildServiceDIDWebDoc renders the DID document for the PDS *service* itself,
// served at `https://pds.<domain>/.well-known/did.json` — the doc anything
// resolving `did:web:pds.<domain>` (the `pds_endpoint` nest publishes, and the
// service DID F1's session tokens carry) reads.
//
// It is deliberately NOT BuildDIDWebDoc's shape: that one describes a *user*
// (an `alsoKnownAs` handle, a repo signing key under `#atproto`), and a service
// has neither. It declares only what it is — the PDS at this host.
//
// Content decision (S3, minor while did:plc is the default): the doc publishes
// no verification method. Nothing verifies a signature against the service DID
// today — repo commits are signed with each account's own key, and per
// atproto-pds-full.md § Reconciliation the service-auth JWTs F3 will mint are
// signed with the account's repo signing key too (which is what keeps C7's "no
// custody widening" true). A future surface that needs a service-level key adds
// a verificationMethod here additively.
func BuildServiceDIDWebDoc(pdsHost string) ([]byte, error) {
	did := DIDWeb(pdsHost)
	doc := map[string]any{
		"@context": []any{"https://www.w3.org/ns/did/v1"},
		"id":       did,
		"service": []any{map[string]any{
			"id":              "#atproto_pds",
			"type":            "AtprotoPersonalDataServer",
			"serviceEndpoint": "https://" + pdsHost,
		}},
	}
	b, err := json.MarshalIndent(doc, "", "  ")
	if err != nil {
		return nil, fmt.Errorf("marshal service did:web doc: %w", err)
	}
	return b, nil
}

// BuildDIDWebDoc renders the DID document served at
// `https://<handle>/.well-known/did.json` for a did:web identity —
// parameterized twin of the S0 probe's builder (same @context, Multikey
// verification method under the `#atproto` fragment, `atproto_pds` service).
//
//   - handle: the ATProto handle == the did:web host (`alice.example.com`).
//   - signingPubMultibase: the repo signing key's bare public multibase
//     (`z…`; from a did:key via MultibaseFromDIDKey).
//   - pdsEndpoint: the PDS service endpoint (`https://<primary-domain>`).
func BuildDIDWebDoc(handle, signingPubMultibase, pdsEndpoint string) ([]byte, error) {
	did := DIDWeb(handle)
	doc := map[string]any{
		"@context": []any{
			"https://www.w3.org/ns/did/v1",
			"https://w3id.org/security/multikey/v1",
			"https://w3id.org/security/suites/secp256k1-2019/v1",
		},
		"id":          did,
		"alsoKnownAs": []any{"at://" + handle},
		"verificationMethod": []any{map[string]any{
			"id":                 did + "#atproto",
			"type":               "Multikey",
			"controller":         did,
			"publicKeyMultibase": signingPubMultibase,
		}},
		"service": []any{map[string]any{
			"id":              "#atproto_pds",
			"type":            "AtprotoPersonalDataServer",
			"serviceEndpoint": pdsEndpoint,
		}},
	}
	b, err := json.MarshalIndent(doc, "", "  ")
	if err != nil {
		return nil, fmt.Errorf("marshal did:web doc: %w", err)
	}
	return b, nil
}
