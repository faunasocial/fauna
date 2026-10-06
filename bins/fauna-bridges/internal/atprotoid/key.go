package atprotoid

import (
	"fmt"
	"strings"

	"github.com/bluesky-social/indigo/atproto/atcrypto"
	"github.com/mr-tron/base58"
)

// k256PrivMulticodecVarint is the unsigned-varint encoding of the public
// multicodec table's `secp256k1-priv` code 0x1301 — the two bytes a K-256
// private-key multibase string carries ahead of the raw 32-byte scalar.
var k256PrivMulticodecVarint = []byte{0x81, 0x26}

// PrivateKeyFromK256Scalar constructs an indigo K-256 private key from a raw
// 32-byte scalar (the form the sealed AtprotoIdentityKeyBundle carries) by
// building the private-key multibase ("z" + base58btc(varint(0x1301) ||
// scalar)) and parsing it through atcrypto — so every downstream consumer
// sees exactly the key type the rest of the atproto stack uses.
//
// The multibase construction is proven by test round-trip against
// atcrypto-generated keys (key_test.go): scalar extracted from a generated
// key's own Multibase() re-enters here and must yield the identical key.
func PrivateKeyFromK256Scalar(scalar []byte) (atcrypto.PrivateKeyExportable, error) {
	if len(scalar) != 32 {
		return nil, fmt.Errorf("k256 scalar must be 32 bytes, got %d", len(scalar))
	}
	buf := make([]byte, 0, len(k256PrivMulticodecVarint)+len(scalar))
	buf = append(buf, k256PrivMulticodecVarint...)
	buf = append(buf, scalar...)
	key, err := atcrypto.ParsePrivateMultibase("z" + base58.Encode(buf))
	if err != nil {
		return nil, fmt.Errorf("parse k256 scalar as private multibase: %w", err)
	}
	return key, nil
}

// DIDKeyForPrivate returns the `did:key:z…` string of a private key's public
// half — the spelling the identity roster and PLC operations carry.
func DIDKeyForPrivate(key atcrypto.PrivateKeyExportable) (string, error) {
	pub, err := key.PublicKey()
	if err != nil {
		return "", fmt.Errorf("derive public key: %w", err)
	}
	return pub.DIDKey(), nil
}

// ParsePublicDIDKey parses a `did:key:z…` string into an atcrypto public key
// (e.g. the bridge rotation pubkey a signed op verifies against).
func ParsePublicDIDKey(didKey string) (atcrypto.PublicKey, error) {
	pub, err := atcrypto.ParsePublicDIDKey(didKey)
	if err != nil {
		return nil, fmt.Errorf("parse %q: %w", didKey, err)
	}
	return pub, nil
}

// MultibaseFromDIDKey strips the fixed `did:key:` prefix, returning the bare
// public-key multibase (`z…`) a DID document's `publicKeyMultibase` field
// carries.
func MultibaseFromDIDKey(didKey string) (string, error) {
	mb, ok := strings.CutPrefix(didKey, "did:key:")
	if !ok {
		return "", fmt.Errorf("%q is not a did:key", didKey)
	}
	return mb, nil
}
