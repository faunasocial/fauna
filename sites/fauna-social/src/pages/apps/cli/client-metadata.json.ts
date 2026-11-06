// /apps/cli/client-metadata.json — the cli app's OAuth Client ID Metadata
// Document (docs/goal/architecture/apps/cli.md § Open decisions 1). The URL is
// the cli's client_id: every nest a user connects the cli to fetches it, and
// the consent card shows its client_name beside the URL.
//
// The document is single-sourced from docs/sites/fauna-social/apps/cli/
// client-metadata.json, a public text (docs/public-texts/inventory.toml,
// `cli-client-metadata`). It is served byte for byte (`?raw`), never parsed
// and re-serialized here, so what the round approved is what a nest reads.
import type { APIRoute } from 'astro';
import document from '../../../../../../docs/sites/fauna-social/apps/cli/client-metadata.json?raw';

export const GET: APIRoute = () =>
  new Response(document, {
    headers: { 'Content-Type': 'application/json; charset=utf-8' },
  });
