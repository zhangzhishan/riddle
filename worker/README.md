# Cloudflare Worker deployment

This directory contains the production HTTPS implementation of Ian's Paper Plane Mailbox.

## Architecture

- Cloudflare Worker: strict HTTP routing, capability authentication, family HTML UI, PNG validation.
- D1 (`MAILBOX_DB`): message/reply metadata and ordered integer IDs.
- Workers KV (`MAILBOX_IMAGES`): original PNG bytes (up to 4 MiB each).
- Custom domain: `https://ian-mailbox.code4fun.me`.
- Secrets: `MAILBOX_DEVICE_TOKEN` and `MAILBOX_FAMILY_TOKEN`, stored with Wrangler and never committed.

The Python service under `server/` remains useful for local/LAN deployment and protocol tests. The Worker exposes the same device and family routes so the Kobo client does not need a Cloudflare-specific protocol.

## Test

```sh
cd worker
npm test
cd ..
python3 -m unittest -q server/test_mailbox_server.py
cargo test --quiet
npx wrangler deploy --dry-run
```

## First deployment

Create D1 and KV resources, copy their IDs into `wrangler.jsonc`, then:

```sh
npx wrangler d1 migrations apply ian-paper-mailbox --remote
npx wrangler deploy
npx wrangler secret put MAILBOX_DEVICE_TOKEN
npx wrangler secret put MAILBOX_FAMILY_TOKEN
curl -fsS https://ian-mailbox.code4fun.me/healthz
```

Do not put real tokens in `.dev.vars`, shell history, repository files, issue text, or chat logs. The family bootstrap URL is:

```text
https://ian-mailbox.code4fun.me/?token=<MAILBOX_FAMILY_TOKEN>
```

Opening it once exchanges the URL token for a `Secure`, `HttpOnly`, `SameSite=Strict` cookie and redirects to `/`.

## Data lifecycle

A message insert stores the PNG in KV first, then inserts its D1 metadata. If D1 insertion fails, the Worker deletes the newly written KV object. Deleting a message from D1 cascades its reply, but operational cleanup must also delete the corresponding KV image key.

Private responses use `Cache-Control: no-store`, capability tokens are compared via fixed-length SHA-256 digests, and family content is escaped before HTML rendering. Cloudflare may reject a few edge-level methods (for example `TRACE`) before the Worker runs; methods that reach the Worker return `405` with `Allow: GET, POST` and `no-store`.
