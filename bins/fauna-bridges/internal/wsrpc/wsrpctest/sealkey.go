// Package wsrpctest holds the fixtures the bridge's hand-built test callers
// share when they answer a WS-RPC kind. It imports nothing from the bridge, so
// every test package — wsrpc's own included — can use it.
package wsrpctest

import (
	"bytes"
	"crypto/mlkem"
	"sync"
)

var recipientMlkemEk = sync.OnceValue(func() []byte {
	dk, err := mlkem.GenerateKey768()
	if err != nil {
		panic("wsrpctest: generate ML-KEM-768 key: " + err.Error())
	}
	return dk.EncapsulationKey().Bytes()
})

// RecipientMlkemEk returns a valid 1184-byte ML-KEM-768 encapsulation key, the
// same one for the whole test process, so a test that runs the real seal seals
// X-Wing to it. The decapsulation key is discarded: no bridge test opens what
// it sealed.
func RecipientMlkemEk() []byte {
	return bytes.Clone(recipientMlkemEk())
}

// RecipientSealKey returns the `key` value of a fetch_recipient_mls_pubkey
// reply for a recipient with a key on file: both halves, as the nest sends
// them. The bridge refuses a key without its ML-KEM half, so a mock answering
// with `mls_pubkey` alone models a reply no nest sends.
func RecipientSealKey(mlsPubkey []byte) map[string][]byte {
	return map[string][]byte{"mls_pubkey": mlsPubkey, "mlkem_ek": RecipientMlkemEk()}
}
