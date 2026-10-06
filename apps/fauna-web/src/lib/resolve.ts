import { nodeUrl } from './api';
import { classifyRecipient, nestResolve, actorByHandle } from './wasm';

export interface ResolvedRecipient {
  actorId: string;
  nodeUrl: string;
  handle?: string;
  domain?: string;
}

/** Resolve a domain to its canonical fauna node URL over the pre-identity
 *  anonymous WS-RPC `fauna.nest.resolve` kind (replacing the deleted HTTP
 *  `GET /api/v1/resolve-node/{domain}`; `api-layers.md` § Public). */
export async function resolveNodeUrl(domain: string): Promise<string> {
  const url = await nestResolve(domain, nodeUrl());
  if (!url) throw new Error(`No node URL returned for ${domain}`);
  return url;
}

/** Resolve a handle to its actor ID over the anonymous WS-RPC
 *  `fauna.actor.by_handle` kind (replacing the deleted HTTP
 *  `GET /api/v1/actor/by-handle/{handle}`). The anonymous WS is CORS-exempt, so
 *  resolving against a remote `targetNodeUrl` works where the HTTP fetch was
 *  cross-origin-blocked. */
export async function resolveHandle(handle: string, targetNodeUrl: string, domain?: string): Promise<{ actor_id: string; handle: string; domain: string }> {
  return actorByHandle(handle, targetNodeUrl, domain);
}

export async function parseRecipient(input: string): Promise<ResolvedRecipient> {
  // The 64-hex actor-id check + `user@domain` split are shared with android/linux via
  // `fauna_core::resolve::classify_recipient` (over wasm); only the network resolution
  // of a handle stays web-specific (the nest's `nest.resolve` + `actor.by_handle` kinds).
  const classified = classifyRecipient(input);

  if (classified.kind === 'actor_id') {
    return { actorId: classified.actorId, nodeUrl: nodeUrl() };
  }

  if (classified.kind === 'handle') {
    const targetUrl = await resolveNodeUrl(classified.domain);
    // Pass the typed domain as the multi-domain qualifier so the nest echoes it
    // back (`bob@domain2` stays `bob@domain2` rather than collapsing to the
    // canonical identity domain) — mail-multidomain.md § Multi-domain handles.
    const resolved = await resolveHandle(classified.user, targetUrl, classified.domain);
    return {
      actorId: resolved.actor_id,
      nodeUrl: targetUrl,
      handle: resolved.handle,
      domain: resolved.domain,
    };
  }

  throw new Error('Enter a handle (alice@fauna.social) or 64-char actor ID');
}
