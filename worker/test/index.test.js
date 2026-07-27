import assert from "node:assert/strict";
import test from "node:test";

import worker, {
  escapeHtml,
  parseContentLength,
  refinementModels,
  renderIndex,
  safeEqual,
  validatePng,
} from "../src/index.js";

function crc32(bytes) {
  let crc = 0xffffffff;
  for (const byte of bytes) {
    crc ^= byte;
    for (let bit = 0; bit < 8; bit += 1) {
      crc = crc & 1 ? 0xedb88320 ^ (crc >>> 1) : crc >>> 1;
    }
  }
  return (crc ^ 0xffffffff) >>> 0;
}

function u32(value) {
  return Uint8Array.of(value >>> 24, value >>> 16, value >>> 8, value);
}

function chunk(kind, payload = new Uint8Array()) {
  const kindBytes = new TextEncoder().encode(kind);
  const crcInput = new Uint8Array(kindBytes.length + payload.length);
  crcInput.set(kindBytes);
  crcInput.set(payload, kindBytes.length);
  const result = new Uint8Array(12 + payload.length);
  result.set(u32(payload.length), 0);
  result.set(kindBytes, 4);
  result.set(payload, 8);
  result.set(u32(crc32(crcInput)), 8 + payload.length);
  return result;
}

function concat(...parts) {
  const result = new Uint8Array(parts.reduce((sum, part) => sum + part.length, 0));
  let offset = 0;
  for (const part of parts) {
    result.set(part, offset);
    offset += part.length;
  }
  return result;
}

function makePng(extra = []) {
  const signature = Uint8Array.of(0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a);
  const ihdr = new Uint8Array(13);
  ihdr.set(u32(1), 0);
  ihdr.set(u32(1), 4);
  ihdr.set([8, 2, 0, 0, 0], 8);
  return concat(signature, chunk("IHDR", ihdr), ...extra, chunk("IDAT", Uint8Array.of(1)), chunk("IEND"));
}

const env = {
  MAILBOX_DEVICE_TOKEN: "device-secret",
  MAILBOX_FAMILY_TOKEN: "family-secret",
};

function memoryKv() {
  const values = new Map();
  return {
    values,
    async get(key, type) {
      const value = values.get(key);
      if (value == null) return null;
      if (type === "arrayBuffer") {
        return value.buffer.slice(value.byteOffset, value.byteOffset + value.byteLength);
      }
      return value;
    },
    async put(key, value) {
      values.set(key, new Uint8Array(value));
    },
    async delete(key) {
      values.delete(key);
    },
  };
}

test("PNG validator accepts a complete PNG", () => {
  assert.doesNotThrow(() => validatePng(makePng()));
});

test("PNG validator rejects duplicate and malformed IHDR chunks", () => {
  assert.throws(() => validatePng(makePng([chunk("IHDR", Uint8Array.of(1, 2, 3))])), /PNG header/);
});

test("PNG validator rejects a bad checksum and missing IDAT", () => {
  const corrupt = makePng();
  corrupt[corrupt.length - 5] ^= 1;
  assert.throws(() => validatePng(corrupt), /checksum/);

  const signature = Uint8Array.of(0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a);
  const ihdr = new Uint8Array(13);
  ihdr.set(u32(1), 0);
  ihdr.set(u32(1), 4);
  ihdr.set([8, 2, 0, 0, 0], 8);
  assert.throws(() => validatePng(concat(signature, chunk("IHDR", ihdr), chunk("IEND"))), /incomplete/);
});

test("token comparison and HTML escaping behave safely", async () => {
  assert.equal(await safeEqual("秘密", "秘密"), true);
  assert.equal(await safeEqual("秘密", "secret"), false);
  assert.equal(escapeHtml(`<script a='b'>&"</script>`), "&lt;script a=&#39;b&#39;&gt;&amp;&quot;&lt;/script&gt;");
});

test("content length parser enforces framing and limits", () => {
  const valid = new Request("https://example.test/", { headers: { "Content-Length": "12" } });
  assert.equal(parseContentLength(valid, 12), 12);
  const missing = new Request("https://example.test/");
  assert.throws(() => parseContentLength(missing, 12), /required/);
  const tooLarge = new Request("https://example.test/", { headers: { "Content-Length": "13" } });
  assert.throws(() => parseContentLength(tooLarge, 12), /limit/);
  const chunked = new Request("https://example.test/", {
    headers: { "Content-Length": "4", "Transfer-Encoding": "chunked" },
  });
  assert.throws(() => parseContentLength(chunked, 12), /transfer encoding/);
});

test("family HTML is latest-ready, responsive, and escapes stored content", () => {
  const html = renderIndex([
    {
      id: 2,
      device_id: "ian-kobo<script>",
      created_at: "2026-07-18T00:00:00Z",
      reply_body: "你好 <b>Ian</b>",
    },
  ]);
  assert.match(html, /Ian 的纸飞机信箱/);
  assert.match(html, /viewport/);
  assert.match(html, /noindex,nofollow,noarchive/);
  assert.match(html, /ian-kobo&lt;script&gt;/);
  assert.match(html, /你好 &lt;b&gt;Ian&lt;\/b&gt;/);
  assert.doesNotMatch(html, /<b>Ian<\/b>/);
});

test("public health and generic method handling return hardened responses", async () => {
  const health = await worker.fetch(new Request("https://example.test/healthz"), env);
  assert.equal(health.status, 200);
  assert.equal(await health.text(), "ok\n");
  assert.equal(health.headers.get("Cache-Control"), "no-store");

  for (const method of ["DELETE", "TRACE", "PROPFIND", "BREW"]) {
    const request = { url: "https://example.test/anything", method, headers: new Headers() };
    const response = await worker.fetch(request, env);
    assert.equal(response.status, 405, method);
    assert.equal(response.headers.get("Allow"), "GET, POST");
    assert.equal(response.headers.get("Cache-Control"), "no-store");
  }
});

test("protected routes reject invalid credentials before storage access", async () => {
  const family = await worker.fetch(new Request("https://example.test/?token=wrong"), env);
  assert.equal(family.status, 403);

  const upload = await worker.fetch(new Request("https://example.test/api/device/messages", {
    method: "POST",
    headers: { Authorization: "Bearer wrong" },
  }), env);
  assert.equal(upload.status, 401);
  assert.equal(upload.headers.get("WWW-Authenticate"), 'Bearer realm="paper-plane-device"');
});

test("device can refine a PNG through MAI-Image-2.5-Pro and retries use the KV cache", async () => {
  const input = makePng();
  const refined = makePng([chunk("tEXt", new TextEncoder().encode("refined"))]);
  const images = memoryKv();
  const refineEnv = {
    ...env,
    AZURE_MAI_API_KEY: "azure-mai-test-secret",
    AZURE_MAI_ENDPOINT: "https://mai-test.services.ai.azure.com/",
    MAILBOX_IMAGES: images,
  };
  let openAiCalls = 0;
  const originalFetch = globalThis.fetch;
  globalThis.fetch = async (url, options) => {
    openAiCalls += 1;
    assert.equal(url, "https://mai-test.services.ai.azure.com/mai/v1/images/edits");
    assert.equal(options.method, "POST");
    assert.equal(options.headers["api-key"], "azure-mai-test-secret");
    assert.equal(options.body.get("model"), "MAI-Image-2.5-Pro");
    assert.equal(options.body.get("input_fidelity"), "high");
    assert.equal(options.body.get("quality"), "low");
    assert.equal(options.body.get("size"), "1024x1536");
    assert.equal(options.body.get("output_format"), "png");
    assert.match(options.body.get("prompt"), /child.*drawing/i);
    const image = options.body.get("image");
    assert.equal(image.type, "image/png");
    assert.deepEqual(new Uint8Array(await image.arrayBuffer()), input);
    return Response.json({
      data: [{ b64_json: Buffer.from(refined).toString("base64") }],
    }, { headers: { "x-request-id": "req_test" } });
  };

  try {
    const request = () => new Request("https://example.test/api/device/refinements", {
      method: "POST",
      headers: {
        Authorization: "Bearer device-secret",
        "Content-Type": "image/png",
        "Content-Length": String(input.byteLength),
        "X-Device-Id": "ian-kobo",
        "X-Refinement-Key": "0123456789abcdef",
      },
      body: input,
    });
    const first = await worker.fetch(request(), refineEnv);
    assert.equal(first.status, 200);
    assert.equal(first.headers.get("Content-Type"), "image/png");
    assert.equal(first.headers.get("X-Refinement-Cache"), "miss");
    assert.equal(first.headers.get("X-Refinement-Model"), "MAI-Image-2.5-Pro");
    assert.deepEqual(new Uint8Array(await first.arrayBuffer()), refined);

    const second = await worker.fetch(request(), refineEnv);
    assert.equal(second.status, 200);
    assert.equal(second.headers.get("X-Refinement-Cache"), "hit");
    assert.deepEqual(new Uint8Array(await second.arrayBuffer()), refined);
    assert.equal(openAiCalls, 1);
    assert.deepEqual(images.values.get("refinements/ian-kobo/0123456789abcdef.png"), refined);
  } finally {
    globalThis.fetch = originalFetch;
  }
});

test("refine route validates auth, key, and upstream failures", async () => {
  const input = makePng();
  const images = memoryKv();
  const refineEnv = { ...env, AZURE_MAI_API_KEY: "azure-mai-test-secret", AZURE_MAI_ENDPOINT: "https://mai-test.services.ai.azure.com", MAILBOX_IMAGES: images };
  const request = (headers = {}, body = input) => new Request(
    "https://example.test/api/device/refinements",
    { method: "POST", headers, body },
  );

  const unauthorized = await worker.fetch(request({ "Content-Length": String(input.length) }), refineEnv);
  assert.equal(unauthorized.status, 401);

  const badKey = await worker.fetch(request({
    Authorization: "Bearer device-secret",
    "Content-Type": "image/png",
    "Content-Length": String(input.length),
    "X-Device-Id": "ian-kobo",
    "X-Refinement-Key": "not-safe",
  }), refineEnv);
  assert.equal(badKey.status, 400);

  const originalFetch = globalThis.fetch;
  globalThis.fetch = async () => new Response("upstream detail must stay private", {
    status: 429,
    headers: { "x-request-id": "req_rate_limit" },
  });
  try {
    const upstream = await worker.fetch(request({
      Authorization: "Bearer device-secret",
      "Content-Type": "image/png",
      "Content-Length": String(input.length),
      "X-Device-Id": "ian-kobo",
      "X-Refinement-Key": "fedcba9876543210",
    }), refineEnv);
    assert.equal(upstream.status, 502);
    assert.equal(await upstream.text(), "image refinement failed\n");
  } finally {
    globalThis.fetch = originalFetch;
  }
});

test("refinement falls back down the model tiers when capacity is exhausted", async () => {
  const input = makePng();
  const refined = makePng([chunk("tEXt", new TextEncoder().encode("flash"))]);
  const images = memoryKv();
  const refineEnv = {
    ...env,
    AZURE_MAI_API_KEY: "azure-mai-test-secret",
    AZURE_MAI_ENDPOINT: "https://mai-test.services.ai.azure.com",
    MAILBOX_IMAGES: images,
  };
  const attempted = [];
  const originalFetch = globalThis.fetch;
  globalThis.fetch = async (_url, options) => {
    const model = options.body.get("model");
    attempted.push(model);
    if (model !== "MAI-Image-2.5-Flash") {
      return new Response("rate limited", { status: 429 });
    }
    return Response.json({ data: [{ b64_json: Buffer.from(refined).toString("base64") }] });
  };
  try {
    const response = await worker.fetch(new Request("https://example.test/api/device/refinements", {
      method: "POST",
      headers: {
        Authorization: "Bearer device-secret",
        "Content-Type": "image/png",
        "Content-Length": String(input.byteLength),
        "X-Device-Id": "ian-kobo",
        "X-Refinement-Key": "abcdef0123456789",
      },
      body: input,
    }), refineEnv);
    assert.equal(response.status, 200);
    assert.equal(response.headers.get("X-Refinement-Model"), "MAI-Image-2.5-Flash");
    assert.deepEqual(attempted, ["MAI-Image-2.5-Pro", "MAI-Image-2.5", "MAI-Image-2.5-Flash"]);
    assert.deepEqual(new Uint8Array(await response.arrayBuffer()), refined);
  } finally {
    globalThis.fetch = originalFetch;
  }
});

test("a 400 from the first tier does not waste the fallback tiers", async () => {
  const input = makePng();
  const images = memoryKv();
  const refineEnv = {
    ...env,
    AZURE_MAI_API_KEY: "azure-mai-test-secret",
    AZURE_MAI_ENDPOINT: "https://mai-test.services.ai.azure.com",
    MAILBOX_IMAGES: images,
  };
  let calls = 0;
  const originalFetch = globalThis.fetch;
  globalThis.fetch = async () => {
    calls += 1;
    return new Response("moderation blocked", { status: 400 });
  };
  try {
    const response = await worker.fetch(new Request("https://example.test/api/device/refinements", {
      method: "POST",
      headers: {
        Authorization: "Bearer device-secret",
        "Content-Type": "image/png",
        "Content-Length": String(input.byteLength),
        "X-Device-Id": "ian-kobo",
        "X-Refinement-Key": "0f0f0f0f0f0f0f0f",
      },
      body: input,
    }), refineEnv);
    assert.equal(response.status, 502);
    assert.equal(calls, 1);
  } finally {
    globalThis.fetch = originalFetch;
  }
});

test("AZURE_MAI_MODEL overrides the default tier order", () => {
  assert.deepEqual(
    refinementModels({}),
    ["MAI-Image-2.5-Pro", "MAI-Image-2.5", "MAI-Image-2.5-Flash"],
  );
  assert.deepEqual(
    refinementModels({ AZURE_MAI_MODEL: " MAI-Image-2.5 , MAI-Image-2.5-Flash " }),
    ["MAI-Image-2.5", "MAI-Image-2.5-Flash"],
  );
  assert.deepEqual(
    refinementModels({ AZURE_MAI_MODEL: "  " }),
    ["MAI-Image-2.5-Pro", "MAI-Image-2.5", "MAI-Image-2.5-Flash"],
  );
});
