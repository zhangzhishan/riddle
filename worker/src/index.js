const MAX_IMAGE_BYTES = 4 * 1024 * 1024;
const MAX_REFINED_IMAGE_BYTES = 16 * 1024 * 1024;
const MAX_REPLY_CHARACTERS = 2000;
const MAX_FORM_BYTES = 32 * 1024;
const COOKIE_NAME = "mailbox_family";
const COOKIE_CONTEXT = "paper-plane-family-cookie-v1";
const PNG_SIGNATURE = new Uint8Array([0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a]);
const MAI_IMAGES_EDIT_PATH = "/mai/v1/images/edits";
const MAI_DEFAULT_MODELS = ["MAI-Image-2.5-Pro", "MAI-Image-2.5", "MAI-Image-2.5-Flash"];
const AZURE_OPENAI_IMAGES_EDIT_PATH = "/openai/v1/images/edits";
const AZURE_OPENAI_DEFAULT_IMAGE_MODEL = "gpt-image-2";
const REFINE_PROMPT = `Refine this child's drawing into a polished, colorful children's-book illustration. Preserve the original subject, composition, poses, proportions, line placement, and charming imperfections so it is clearly the same drawing. Clean up the linework, add coherent colors, gentle shading, and a simple supportive background without redesigning it. Do not add text, logos, watermarks, frightening imagery, weapons, or new characters unless they are clearly present in the drawing. Keep it warm, playful, age-appropriate, and use strong value contrast so it remains readable in grayscale.`;

const STYLE = `:root {
  color-scheme: light;
  font-family: ui-rounded, "SF Pro Rounded", system-ui, -apple-system, sans-serif;
  background: #f4efe5;
  color: #2c2924;
}
* { box-sizing: border-box; }
body {
  margin: 0;
  min-height: 100vh;
  background: linear-gradient(180deg, #dbeef1 0, #f4efe5 18rem);
}
main {
  width: min(100% - 1.5rem, 42rem);
  max-width: 42rem;
  margin: 0 auto;
  padding: 2rem 0 4rem;
}
h1 {
  margin: 0;
  font-size: clamp(2rem, 9vw, 3.5rem);
  line-height: 1;
  letter-spacing: -0.04em;
}
.intro { margin: 0.6rem 0 1.8rem; color: #5e5a52; }
.message-card {
  overflow: hidden;
  margin: 0 0 1.4rem;
  border: 1px solid #d6d0c5;
  border-radius: 1.1rem;
  background: #fffdf8;
  box-shadow: 0 0.5rem 1.5rem rgb(69 61 48 / 10%);
}
.message-card header {
  display: flex;
  flex-wrap: wrap;
  justify-content: space-between;
  gap: 0.35rem 1rem;
  padding: 0.9rem 1rem;
  color: #666057;
  font-size: 0.86rem;
}
.message-card img {
  display: block;
  width: 100%;
  height: auto;
  border-block: 1px solid #e5dfd4;
  background: white;
}
form, .reply { padding: 1rem; }
label, .reply strong { display: block; margin-bottom: 0.55rem; font-weight: 700; }
textarea {
  display: block;
  width: 100%;
  min-height: 7rem;
  resize: vertical;
  border: 1px solid #aaa399;
  border-radius: 0.7rem;
  padding: 0.8rem;
  font: inherit;
  background: white;
}
button {
  width: 100%;
  min-height: 3rem;
  margin-top: 0.75rem;
  border: 0;
  border-radius: 999px;
  background: #1d6d74;
  color: white;
  font: inherit;
  font-weight: 750;
  cursor: pointer;
}
button:focus-visible, textarea:focus-visible { outline: 3px solid #edac4d; outline-offset: 2px; }
.reply { background: #fff3cf; }
.reply p { margin: 0; white-space: pre-wrap; overflow-wrap: anywhere; }
.empty { padding: 2rem; border-radius: 1rem; background: #fffdf8; text-align: center; }
@media (min-width: 40rem) {
  main { padding-top: 3.5rem; }
  form, .reply { padding: 1.25rem; }
}`;

const CRC_TABLE = (() => {
  const table = new Uint32Array(256);
  for (let n = 0; n < 256; n += 1) {
    let c = n;
    for (let k = 0; k < 8; k += 1) {
      c = c & 1 ? 0xedb88320 ^ (c >>> 1) : c >>> 1;
    }
    table[n] = c >>> 0;
  }
  return table;
})();

function crc32(bytes) {
  let crc = 0xffffffff;
  for (const byte of bytes) {
    crc = CRC_TABLE[(crc ^ byte) & 0xff] ^ (crc >>> 8);
  }
  return (crc ^ 0xffffffff) >>> 0;
}

function readU32(bytes, offset) {
  return (
    bytes[offset] * 0x1000000 +
    bytes[offset + 1] * 0x10000 +
    bytes[offset + 2] * 0x100 +
    bytes[offset + 3]
  ) >>> 0;
}

export function validatePng(input, maximum = MAX_IMAGE_BYTES) {
  const bytes = input instanceof Uint8Array ? input : new Uint8Array(input);
  if (bytes.byteLength > maximum) throw new Error("image exceeds the size limit");
  if (bytes.byteLength < PNG_SIGNATURE.length) throw new Error("invalid PNG signature");
  for (let i = 0; i < PNG_SIGNATURE.length; i += 1) {
    if (bytes[i] !== PNG_SIGNATURE[i]) throw new Error("invalid PNG signature");
  }

  let offset = PNG_SIGNATURE.length;
  let first = true;
  let sawIhdr = false;
  let sawIdat = false;
  let sawIend = false;
  while (offset < bytes.length) {
    if (bytes.length - offset < 12) throw new Error("invalid PNG chunk");
    const length = readU32(bytes, offset);
    const chunkEnd = offset + 12 + length;
    if (!Number.isSafeInteger(chunkEnd) || chunkEnd > bytes.length) {
      throw new Error("invalid PNG chunk length");
    }
    const kindBytes = bytes.slice(offset + 4, offset + 8);
    const kind = String.fromCharCode(...kindBytes);
    const data = bytes.slice(offset + 8, offset + 8 + length);
    const expectedCrc = readU32(bytes, offset + 8 + length);
    const crcInput = new Uint8Array(4 + length);
    crcInput.set(kindBytes, 0);
    crcInput.set(data, 4);
    if (crc32(crcInput) !== expectedCrc) throw new Error("invalid PNG chunk checksum");
    if (first && (kind !== "IHDR" || length !== 13)) throw new Error("invalid PNG header");

    if (kind === "IHDR") {
      if (sawIhdr || length !== 13) throw new Error("invalid PNG header");
      sawIhdr = true;
      if (readU32(data, 0) === 0 || readU32(data, 4) === 0) {
        throw new Error("invalid PNG dimensions");
      }
    } else if (kind === "IDAT") {
      sawIdat = true;
    } else if (kind === "IEND") {
      if (length !== 0 || chunkEnd !== bytes.length) throw new Error("invalid PNG ending");
      sawIend = true;
    }
    offset = chunkEnd;
    first = false;
    if (sawIend) break;
  }
  if (!sawIhdr || !sawIdat || !sawIend) throw new Error("incomplete PNG");
}

async function digest(value) {
  return new Uint8Array(
    await crypto.subtle.digest("SHA-256", new TextEncoder().encode(value)),
  );
}

export async function safeEqual(left, right) {
  const [a, b] = await Promise.all([digest(left), digest(right)]);
  let difference = 0;
  for (let i = 0; i < a.length; i += 1) difference |= a[i] ^ b[i];
  return difference === 0;
}

async function familyCookieValue(familyToken) {
  const key = await crypto.subtle.importKey(
    "raw",
    new TextEncoder().encode(familyToken),
    { name: "HMAC", hash: "SHA-256" },
    false,
    ["sign"],
  );
  const signature = new Uint8Array(
    await crypto.subtle.sign("HMAC", key, new TextEncoder().encode(COOKIE_CONTEXT)),
  );
  let binary = "";
  for (const byte of signature) binary += String.fromCharCode(byte);
  return btoa(binary).replaceAll("+", "-").replaceAll("/", "_").replace(/=+$/, "");
}

export function escapeHtml(value) {
  return String(value)
    .replaceAll("&", "&amp;")
    .replaceAll("<", "&lt;")
    .replaceAll(">", "&gt;")
    .replaceAll('"', "&quot;")
    .replaceAll("'", "&#39;");
}

export function parseContentLength(request, maximum) {
  if (request.headers.has("Transfer-Encoding")) throw new HttpError(400, "unsupported transfer encoding");
  const raw = request.headers.get("Content-Length");
  if (raw === null) throw new HttpError(411, "Content-Length is required");
  if (!/^(0|[1-9][0-9]*)$/.test(raw)) throw new HttpError(400, "invalid Content-Length");
  const length = Number(raw);
  if (!Number.isSafeInteger(length)) throw new HttpError(400, "invalid Content-Length");
  if (length > maximum) {
    throw new HttpError(413, maximum === MAX_IMAGE_BYTES ? "request exceeds 4 MiB limit" : "request exceeds form size limit");
  }
  return length;
}

class HttpError extends Error {
  constructor(status, message, headers = {}) {
    super(message);
    this.status = status;
    this.headers = headers;
  }
}

function responseHeaders(contentType, extra = {}) {
  const headers = new Headers(extra);
  if (contentType) headers.set("Content-Type", contentType);
  headers.set("Cache-Control", "no-store");
  headers.set("X-Content-Type-Options", "nosniff");
  headers.set("Referrer-Policy", "no-referrer");
  headers.set("X-Frame-Options", "DENY");
  return headers;
}

function plain(status, message, extra = {}) {
  return new Response(`${message}\n`, {
    status,
    headers: responseHeaders("text/plain; charset=utf-8", extra),
  });
}

function empty(status, extra = {}) {
  return new Response(null, { status, headers: responseHeaders(null, extra) });
}

function parseCookie(header) {
  const result = new Map();
  if (!header) return result;
  for (const item of header.split(";")) {
    const index = item.indexOf("=");
    if (index <= 0) continue;
    result.set(item.slice(0, index).trim(), item.slice(index + 1).trim());
  }
  return result;
}

async function deviceAuthorized(request, env) {
  const value = request.headers.get("Authorization") || "";
  if (!value.startsWith("Bearer ")) return false;
  return safeEqual(value.slice(7), env.MAILBOX_DEVICE_TOKEN);
}

async function familyAuthorized(request, env) {
  const supplied = parseCookie(request.headers.get("Cookie")).get(COOKIE_NAME);
  if (!supplied) return false;
  return safeEqual(supplied, await familyCookieValue(env.MAILBOX_FAMILY_TOKEN));
}

function exactEntries(searchParams) {
  return [...searchParams.entries()];
}

function utcTimestamp() {
  return new Date().toISOString().replace(/\.\d{3}Z$/, "Z");
}

function decodeBase64Image(value) {
  if (typeof value !== "string" || value.length === 0) {
    throw new HttpError(502, "image refinement returned no image");
  }
  if (value.length > Math.ceil(MAX_REFINED_IMAGE_BYTES / 3) * 4 + 8) {
    throw new HttpError(502, "image refinement returned an oversized image");
  }
  let binary;
  try {
    binary = atob(value);
  } catch {
    throw new HttpError(502, "image refinement returned invalid image data");
  }
  const bytes = new Uint8Array(binary.length);
  for (let index = 0; index < binary.length; index += 1) {
    bytes[index] = binary.charCodeAt(index);
  }
  try {
    validatePng(bytes, MAX_REFINED_IMAGE_BYTES);
  } catch {
    throw new HttpError(502, "image refinement returned an invalid PNG");
  }
  return bytes;
}

async function handleUpload(request, env) {
  if (!(await deviceAuthorized(request, env))) {
    throw new HttpError(401, "unauthorized", { "WWW-Authenticate": 'Bearer realm="paper-plane-device"' });
  }
  if ((request.headers.get("Content-Type") || "").toLowerCase() !== "image/png") {
    throw new HttpError(415, "Content-Type must be image/png");
  }
  const deviceId = request.headers.get("X-Device-Id") || "";
  if (!deviceId.trim() || deviceId.length > 200 || /[\r\n]/.test(deviceId)) {
    throw new HttpError(400, "device ID must not be empty or invalid");
  }
  const declaredLength = parseContentLength(request, MAX_IMAGE_BYTES);
  const payload = new Uint8Array(await request.arrayBuffer());
  if (payload.byteLength !== declaredLength) throw new HttpError(400, "incomplete request body");
  try {
    validatePng(payload);
  } catch (error) {
    throw new HttpError(400, error.message);
  }

  const imageKey = `images/${crypto.randomUUID()}.png`;
  await env.MAILBOX_IMAGES.put(imageKey, payload, { metadata: { contentType: "image/png" } });
  try {
    const result = await env.MAILBOX_DB.prepare(
      "INSERT INTO messages (device_id, image_key, created_at) VALUES (?, ?, ?)",
    ).bind(deviceId, imageKey, utcTimestamp()).run();
    return plain(201, String(result.meta.last_row_id).trim());
  } catch (error) {
    await env.MAILBOX_IMAGES.delete(imageKey);
    throw error;
  }
}

export function refinementModels(env) {
  const raw = typeof env?.AZURE_MAI_MODEL === "string" ? env.AZURE_MAI_MODEL : "";
  const configured = raw
    .split(",")
    .map((entry) => entry.trim())
    .filter((entry) => entry.length > 0 && entry.length <= 100 && !/[\r\n]/.test(entry));
  return configured.length > 0 ? configured : MAI_DEFAULT_MODELS;
}

function refinementForm(payload, model, imageField) {
  const form = new FormData();
  form.append("model", model);
  form.append(imageField, new Blob([payload], { type: "image/png" }), "drawing.png");
  form.append("prompt", REFINE_PROMPT);
  form.append("input_fidelity", "high");
  form.append("quality", "low");
  form.append("size", "1024x1536");
  form.append("output_format", "png");
  form.append("moderation", "auto");
  form.append("n", "1");
  return form;
}

async function classifyUpstreamFailure(response) {
  let body = null;
  try {
    body = await response.clone().json();
  } catch {
    // Some upstream failures have an empty or non-JSON body.
  }

  const error = body?.error;
  const signals = [
    response.headers.get("x-ms-error-code"),
    error?.code,
    error?.innererror?.code,
    body?.code,
    error?.message,
  ].filter((value) => typeof value === "string" && value.length > 0);
  const moderation = signals.some((value) => (
    /content.?policy|content.?filter|moderation|responsible.?ai|safety.?system/i.test(value)
  ));
  const code = signals
    .find((value) => !/\s/.test(value))
    ?.replace(/[\r\n]/g, " ")
    .slice(0, 100) || "no-error-code";
  return { code, moderation };
}

async function handleRefinement(request, env) {
  if (!(await deviceAuthorized(request, env))) {
    throw new HttpError(401, "unauthorized", { "WWW-Authenticate": 'Bearer realm="paper-plane-device"' });
  }
  const maiConfigured = Boolean(env.AZURE_MAI_API_KEY && env.AZURE_MAI_ENDPOINT);
  const azureOpenAiConfigured = Boolean(env.AZURE_OPENAI_API_KEY && env.AZURE_OPENAI_ENDPOINT);
  if (!maiConfigured && !azureOpenAiConfigured) {
    throw new HttpError(503, "image refinement is not configured");
  }
  if ((request.headers.get("Content-Type") || "").toLowerCase() !== "image/png") {
    throw new HttpError(415, "Content-Type must be image/png");
  }
  const deviceId = request.headers.get("X-Device-Id") || "";
  if (!deviceId.trim() || deviceId.length > 200 || /[\r\n]/.test(deviceId)) {
    throw new HttpError(400, "device ID must not be empty or invalid");
  }
  const refinementKey = request.headers.get("X-Refinement-Key") || "";
  if (!/^[a-f0-9]{16}$/.test(refinementKey)) {
    throw new HttpError(400, "X-Refinement-Key must be 16 lowercase hexadecimal characters");
  }
  const declaredLength = parseContentLength(request, MAX_IMAGE_BYTES);
  const payload = new Uint8Array(await request.arrayBuffer());
  if (payload.byteLength !== declaredLength) throw new HttpError(400, "incomplete request body");
  try {
    validatePng(payload);
  } catch (error) {
    throw new HttpError(400, error.message);
  }

  const cacheKey = `refinements/${encodeURIComponent(deviceId)}/${refinementKey}.png`;
  const cached = await env.MAILBOX_IMAGES.get(cacheKey, "arrayBuffer");
  if (cached) {
    return new Response(cached, {
      status: 200,
      headers: responseHeaders("image/png", { "X-Refinement-Cache": "hit" }),
    });
  }

  let refined = null;
  let model = null;
  if (maiConfigured) {
    const models = refinementModels(env);
    const endpoint = `${env.AZURE_MAI_ENDPOINT.replace(/\/+$/, "")}${MAI_IMAGES_EDIT_PATH}`;
    for (let index = 0; index < models.length; index += 1) {
      const candidate = models[index];
      let upstream;
      try {
        upstream = await fetch(endpoint, {
          method: "POST",
          headers: { "api-key": env.AZURE_MAI_API_KEY },
          body: refinementForm(payload, candidate, "image"),
        });
      } catch (error) {
        console.error("image refinement request failed", candidate, error?.message || String(error));
        if (index + 1 < models.length) continue;
        break;
      }
      if (!upstream.ok) {
        const failure = await classifyUpstreamFailure(upstream);
        console.error(
          "image refinement upstream error",
          candidate,
          upstream.status,
          failure.code,
          upstream.headers.get("x-request-id") || "no-request-id",
        );
        if (failure.moderation) throw new HttpError(502, "image refinement failed");

        // Capacity/transient failures try the remaining MAI tiers. Other MAI
        // client errors skip directly to the separately configured provider.
        const retryable = upstream.status === 429 || upstream.status >= 500;
        if (retryable && index + 1 < models.length) continue;
        break;
      }

      try {
        const result = await upstream.json();
        refined = decodeBase64Image(result?.data?.[0]?.b64_json);
        model = candidate;
        break;
      } catch {
        console.error("image refinement returned an invalid response", candidate);
        if (index + 1 < models.length) continue;
        break;
      }
    }
  }

  if (refined === null && azureOpenAiConfigured) {
    const candidate = env.AZURE_OPENAI_IMAGE_DEPLOYMENT || AZURE_OPENAI_DEFAULT_IMAGE_MODEL;
    const endpoint = `${env.AZURE_OPENAI_ENDPOINT.replace(/\/+$/, "")}${AZURE_OPENAI_IMAGES_EDIT_PATH}`;
    let upstream;
    try {
      upstream = await fetch(endpoint, {
        method: "POST",
        headers: { "api-key": env.AZURE_OPENAI_API_KEY },
        body: refinementForm(payload, candidate, "image[]"),
      });
    } catch (error) {
      console.error("image refinement request failed", candidate, error?.message || String(error));
      throw new HttpError(502, "image refinement failed");
    }
    if (!upstream.ok) {
      console.error(
        "image refinement upstream error",
        candidate,
        upstream.status,
        upstream.headers.get("x-request-id") || "no-request-id",
      );
      throw new HttpError(502, "image refinement failed");
    }
    try {
      const result = await upstream.json();
      refined = decodeBase64Image(result?.data?.[0]?.b64_json);
      model = candidate;
    } catch {
      throw new HttpError(502, "image refinement returned an invalid response");
    }
  }

  if (refined === null) throw new HttpError(502, "image refinement failed");
  await env.MAILBOX_IMAGES.put(cacheKey, refined, {
    metadata: { contentType: "image/png", model },
  });
  return new Response(refined, {
    status: 200,
    headers: responseHeaders("image/png", { "X-Refinement-Cache": "miss", "X-Refinement-Model": model }),
  });
}

async function handlePoll(request, env, url) {
  if (!(await deviceAuthorized(request, env))) {
    throw new HttpError(401, "unauthorized", { "WWW-Authenticate": 'Bearer realm="paper-plane-device"' });
  }
  const entries = exactEntries(url.searchParams);
  if (entries.length !== 1 || entries[0][0] !== "after" || !/^(0|[1-9][0-9]*)$/.test(entries[0][1])) {
    throw new HttpError(400, "after must be one non-negative decimal reply ID");
  }
  const after = Number(entries[0][1]);
  if (!Number.isSafeInteger(after)) throw new HttpError(400, "invalid reply ID");
  const reply = await env.MAILBOX_DB.prepare(
    "SELECT id, body FROM replies WHERE id > ? ORDER BY id ASC LIMIT 1",
  ).bind(after).first();
  if (!reply) return empty(204);
  return new Response(reply.body, {
    status: 200,
    headers: responseHeaders("text/plain; charset=utf-8", { "X-Reply-Id": String(reply.id) }),
  });
}

async function handleFamilyIndex(request, env, url) {
  const entries = exactEntries(url.searchParams);
  if (entries.length > 0) {
    if (entries.length !== 1 || entries[0][0] !== "token" || !(await safeEqual(entries[0][1], env.MAILBOX_FAMILY_TOKEN))) {
      throw new HttpError(403, "forbidden");
    }
    const cookieValue = await familyCookieValue(env.MAILBOX_FAMILY_TOKEN);
    return empty(303, {
      Location: "/",
      "Set-Cookie": `${COOKIE_NAME}=${cookieValue}; Path=/; HttpOnly; Secure; SameSite=Strict; Max-Age=31536000`,
    });
  }
  if (!(await familyAuthorized(request, env))) throw new HttpError(403, "forbidden");
  const query = await env.MAILBOX_DB.prepare(
    `SELECT m.id, m.device_id, m.created_at, r.body AS reply_body
     FROM messages AS m
     LEFT JOIN replies AS r ON r.message_id = m.id
     ORDER BY m.id DESC`,
  ).all();
  const html = renderIndex(query.results || []);
  return new Response(html, {
    status: 200,
    headers: responseHeaders("text/html; charset=utf-8", {
      "Content-Security-Policy": "default-src 'none'; style-src 'self'; img-src 'self'; form-action 'self'; base-uri 'none'; frame-ancestors 'none'",
    }),
  });
}

async function handleCss(request, env) {
  if (!(await familyAuthorized(request, env))) throw new HttpError(403, "forbidden");
  return new Response(STYLE, { status: 200, headers: responseHeaders("text/css; charset=utf-8") });
}

async function handleImage(request, env, messageId) {
  if (!(await familyAuthorized(request, env))) throw new HttpError(403, "forbidden");
  const row = await env.MAILBOX_DB.prepare(
    "SELECT image_key FROM messages WHERE id = ?",
  ).bind(messageId).first();
  if (!row) throw new HttpError(404, "not found");
  const image = await env.MAILBOX_IMAGES.get(row.image_key, "arrayBuffer");
  if (!image) throw new HttpError(404, "not found");
  return new Response(image, { status: 200, headers: responseHeaders("image/png") });
}

async function handleReply(request, env, messageId) {
  if (!(await familyAuthorized(request, env))) throw new HttpError(403, "forbidden");
  const contentType = (request.headers.get("Content-Type") || "").toLowerCase();
  if (!/^application\/x-www-form-urlencoded(?:\s*;\s*charset=utf-8)?$/.test(contentType)) {
    throw new HttpError(415, "Content-Type must be application/x-www-form-urlencoded");
  }
  const declaredLength = parseContentLength(request, MAX_FORM_BYTES);
  const bytes = new Uint8Array(await request.arrayBuffer());
  if (bytes.byteLength !== declaredLength) throw new HttpError(400, "incomplete request body");
  let text;
  try {
    text = new TextDecoder("utf-8", { fatal: true }).decode(bytes);
  } catch {
    throw new HttpError(400, "invalid form body");
  }
  const entries = [...new URLSearchParams(text).entries()];
  if (entries.length !== 1 || entries[0][0] !== "body") {
    throw new HttpError(400, "form requires one body field");
  }
  const body = entries[0][1];
  if (!body.trim()) throw new HttpError(400, "reply must not be empty");
  if ([...body].length > MAX_REPLY_CHARACTERS) {
    throw new HttpError(400, "reply must be at most 2,000 characters");
  }
  const exists = await env.MAILBOX_DB.prepare("SELECT 1 AS found FROM messages WHERE id = ?")
    .bind(messageId).first();
  if (!exists) throw new HttpError(404, "message does not exist");
  try {
    await env.MAILBOX_DB.prepare(
      "INSERT INTO replies (message_id, body, created_at) VALUES (?, ?, ?)",
    ).bind(messageId, body, utcTimestamp()).run();
  } catch (error) {
    if (/unique/i.test(error.message)) throw new HttpError(409, "message already has a reply");
    throw error;
  }
  return empty(303, { Location: `/#message-${messageId}` });
}

export function renderIndex(messages) {
  const cards = messages.map((message) => {
    const id = Number(message.id);
    const device = escapeHtml(message.device_id);
    const created = escapeHtml(message.created_at);
    const reply = message.reply_body == null
      ? `<form action="/messages/${id}/reply" method="post">
          <label for="reply-${id}">给 Ian 回信</label>
          <textarea id="reply-${id}" name="body" maxlength="2000" required></textarea>
          <button type="submit">发送回信</button>
        </form>`
      : `<div class="reply"><strong>家人的回信</strong><p>${escapeHtml(message.reply_body)}</p></div>`;
    return `<article class="message-card" id="message-${id}">
      <header><span>来自 ${device}</span><time datetime="${created}">${created}</time></header>
      <img src="/messages/${id}.png" alt="Ian 的第 ${id} 张画" loading="lazy">
      ${reply}
    </article>`;
  });
  if (cards.length === 0) cards.push('<p class="empty">还没有纸飞机飞过来。</p>');
  return `<!doctype html>
<html lang="zh-CN">
<head>
  <meta charset="utf-8">
  <meta name="viewport" content="width=device-width, initial-scale=1">
  <meta name="robots" content="noindex,nofollow,noarchive">
  <title>Ian 的纸飞机信箱</title>
  <link rel="stylesheet" href="/static/style.css">
</head>
<body>
  <main>
    <h1>Ian 的纸飞机信箱</h1>
    <p class="intro">Ian 的画，最新一张在最前面。</p>
    ${cards.join("\n")}
  </main>
</body>
</html>`;
}

async function handleRequest(request, env) {
  if (!env.MAILBOX_DEVICE_TOKEN || !env.MAILBOX_FAMILY_TOKEN) {
    throw new Error("mailbox secrets are not configured");
  }
  const url = new URL(request.url);
  if (url.hash) throw new HttpError(404, "not found");
  if (request.method !== "GET" && request.method !== "POST") {
    throw new HttpError(405, "method not allowed", { Allow: "GET, POST" });
  }

  if (request.method === "GET" && url.pathname === "/healthz" && url.search === "") {
    return plain(200, "ok");
  }
  if (request.method === "POST" && url.pathname === "/api/device/messages" && url.search === "") {
    return handleUpload(request, env);
  }
  if (request.method === "POST" && url.pathname === "/api/device/refinements" && url.search === "") {
    return handleRefinement(request, env);
  }
  if (request.method === "GET" && url.pathname === "/api/device/replies") {
    return handlePoll(request, env, url);
  }
  if (request.method === "GET" && url.pathname === "/") {
    return handleFamilyIndex(request, env, url);
  }
  if (request.method === "GET" && url.pathname === "/static/style.css" && url.search === "") {
    return handleCss(request, env);
  }
  let match = url.pathname.match(/^\/messages\/([1-9][0-9]*)\.png$/);
  if (request.method === "GET" && match && url.search === "") {
    return handleImage(request, env, Number(match[1]));
  }
  match = url.pathname.match(/^\/messages\/([1-9][0-9]*)\/reply$/);
  if (request.method === "POST" && match && url.search === "") {
    return handleReply(request, env, Number(match[1]));
  }
  throw new HttpError(404, "not found");
}

export default {
  async fetch(request, env) {
    try {
      return await handleRequest(request, env);
    } catch (error) {
      if (error instanceof HttpError) return plain(error.status, error.message, error.headers);
      const url = new URL(request.url);
      console.error("mailbox request failed", request.method, url.pathname, error?.message || String(error));
      return plain(500, "internal server error");
    }
  },
};
