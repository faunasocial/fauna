// The per-identity "post-auth edge" latch: `fire(secret, status)` answers true
// exactly once per transition INTO `connected` for a signed-in identity (and
// once more for each identity change while connected). Leaving `connected` —
// a socket drop, or a sign-out clearing the identity — re-arms it, so the
// next reconnect fires again. The mail connect-time refresh rides this
// (`mail-credentials.md` § Hard revoke → *Published before the nest moves*,
// rule (4): every app refreshes after every reconnect of its client).
export function createConnectEdge(): (secret: string | null | undefined, status: string) => boolean {
  let armedFor: string | null = null;
  return (secret, status) => {
    if (!secret || status !== 'connected') {
      armedFor = null;
      return false;
    }
    if (armedFor === secret) return false;
    armedFor = secret;
    return true;
  };
}
