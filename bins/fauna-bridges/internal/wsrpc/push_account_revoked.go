package wsrpc

// PushKindAccountRevoked is the nest→MDA push kind naming a user account
// that stopped standing — locked, suspended, evicted or deleted. Mirrors
// fauna-protocol's PUSH_KIND_BRIDGE_ACCOUNT_REVOKED (bridge_routing.rs); the
// nest sends it to every approved MDA bridge from
// `AppState::end_bridge_held_sessions`
// (`docs/goal/architecture/transport-connection.md` § Revocation teardown,
// the bridge-held sessions paragraph).
const PushKindAccountRevoked = "fauna.bridges.account_revoked"

// BridgeAccountRevokedPush mirrors
// libs/fauna-protocol/src/bridge_routing.rs:BridgeAccountRevokedPush. On it
// the MDA ends every IMAP session AUTH'd as ActorID at once — an IDLE one
// with a `BYE` — instead of at the session's next command.
type BridgeAccountRevokedPush struct {
	ActorID []byte `cbor:"actor_id"`
}
