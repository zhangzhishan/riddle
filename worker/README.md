# Cloudflare Worker deployment

This directory contains the production HTTPS implementation of Ian's Paper Plane Mailbox.

## Architecture

- Cloudflare Worker: strict HTTP routing, capability authentication, family HTML UI, PNG validation.
- D1 (`MAILBOX_DB`): message/reply metadata, AI refinement history, and ordered integer IDs.
- Workers KV (`MAILBOX_IMAGES`): mailbox PNGs plus each AI input/output pair.
- Custom domain: `https://ian-mailbox.code4fun.me`.
- Secrets: `MAILBOX_DEVICE_TOKEN`, `MAILBOX_FAMILY_TOKEN`, `AZURE_MAI_API_KEY`, `AZURE_MAI_ENDPOINT`, `AZURE_OPENAI_API_KEY`, and `AZURE_OPENAI_ENDPOINT`, stored with Wrangler and never committed. `AZURE_MAI_MODEL` is optional: a comma-separated tier list that overrides the default `MAI-Image-2.5-Pro,MAI-Image-2.5,MAI-Image-2.5-Flash`. `AZURE_OPENAI_IMAGE_DEPLOYMENT` optionally overrides the final fallback deployment, which defaults to `gpt-image-2`.

The Kobo can deliberately request **AI 润色** after drawing. If the input is mainly handwriting, including Chinese, the fixed prompt tells the model to treat those words as the scene request and generate the described picture instead of simply reproducing the text. The Worker keeps Azure credentials off the device and returns a PNG that the Kobo scales to its grayscale canvas. After generation, Ian can either return to the original or adopt the AI result as a new background and continue drawing on top; later send/refine actions export the full composited canvas. A deterministic `X-Refinement-Key` caches successful results in KV, so retrying the same drawing normally does not create another paid image request.

Every successful AI request stores its submitted PNG and generated PNG as a pair. Family members who already hold the private family cookie can open `/refinements` to browse the newest pairs first; both image routes remain capability-protected and non-cacheable.

MAI preview quota may be unavailable or heavily constrained, so refinement
walks a quality-ordered tier list and steps down on network failures,
`429`, or `5xx`:
`MAI-Image-2.5-Pro` → `MAI-Image-2.5` → `MAI-Image-2.5-Flash` → Azure OpenAI `gpt-image-2`.
Explicit content-moderation rejections stop immediately instead of trying
another provider. Other MAI client errors skip the remaining MAI tiers and go
directly to the separately configured Azure OpenAI fallback. The model that
produced the image is reported in the `X-Refinement-Model` response header and
stored in KV entry metadata.

The MAI path is `${AZURE_MAI_ENDPOINT}/mai/v1/images/edits` with an `api-key`
header and an `image` form field. The Azure OpenAI fallback uses
`${AZURE_OPENAI_ENDPOINT}/openai/v1/images/edits`, the separate Azure OpenAI
key, and an `image[]` form field. MAI models are **not** reachable through the
`/openai/...` routes.

The Python service under `server/` remains useful for local/LAN deployment and the original mailbox protocol tests. Upload, reply, family, and health routes stay compatible between both implementations; AI refinement is provided by the production Worker because that is where the Azure secret and result cache live.

## Device refinement protocol

`POST /api/device/refinements` requires the normal device bearer token plus:

- `Content-Type: image/png`
- `Content-Length` up to 4 MiB
- `X-Device-Id`
- `X-Refinement-Key`: 16 lowercase hexadecimal characters, derived deterministically from the PNG
- raw PNG request body

Success returns `200 image/png`. `X-Refinement-Cache` is `miss` for a new edit and `hit` when KV supplies a previous successful result; `X-Refinement-Model` names the tier that served it. The UI requires two deliberate stylus taps before starting; a failed request preserves the drawing and is retryable.

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
npx wrangler secret put AZURE_MAI_API_KEY
npx wrangler secret put AZURE_MAI_ENDPOINT   # e.g. https://<account>.services.ai.azure.com
npx wrangler secret put AZURE_OPENAI_API_KEY
npx wrangler secret put AZURE_OPENAI_ENDPOINT
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
