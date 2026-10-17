import { nodeUrl } from './api';
import { classifyRecipient, findUser } from './wasm';

export interface ResolvedRecipient {
  actorId: string;
  /** The nest the actor lives on: this nest's URL, or the peer's. */
  nodeUrl: string;
  /** The peer nest the lookup resolved on — `null` for a home-nest actor. The
   *  knock hands it to `fauna.inbox.send` as `recipientNestUrl`; carried from the
   *  lookup, never re-derived (`contacts.md` § Cross-nest knocks). */
  nestUrl: string | null;
  handle?: string;
  domain?: string;
}

export async function parseRecipient(input: string): Promise<ResolvedRecipient> {
  // The 64-hex actor-id check + `user@domain` split are shared with android/linux via
  // `fauna_core::resolve::classify_recipient` (over wasm); only the network resolution
  // of a handle stays web-specific (the nest's `nest.resolve` + `actor.by_handle` kinds).
  const classified = classifyRecipient(input);

  if (classified.kind === 'actor_id') {
    return { actorId: classified.actorId, nodeUrl: nodeUrl(), nestUrl: null };
  }

  if (classified.kind === 'handle') {
    // One shared lookup (`find_user_across_nests` over wasm): a home probe with
    // the typed domain as the multi-domain qualifier (so `bob@domain2` stays
    // `bob@domain2` — mail-multidomain.md § Multi-domain handles), and, when the
    // domain is foreign, a direct dial of the peer. It names the nest it
    // resolved on, so the foreign decision and URL are never re-derived here.
    const hit = await findUser(classified.user, nodeUrl(), classified.domain);
    return {
      actorId: hit.actor_id,
      nodeUrl: hit.nest_url ?? nodeUrl(),
      nestUrl: hit.nest_url,
      handle: hit.handle,
      domain: hit.domain,
    };
  }

  throw new Error('Enter a handle (alice@fauna.social) or 64-char actor ID');
}
