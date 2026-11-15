import assert from "node:assert/strict";
import test from "node:test";
import { spawnSync } from "node:child_process";
import { readFileSync } from "node:fs";

import worker, {
  azureOpenAiRefinementModels,
  escapeHtml,
  parseContentLength,
  refinementModels,
  renderIndex,
  renderRefinementHistory,
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

function memoryRefinementDb() {
  const rows = [];
  return {
    rows,
    prepare(sql) {
      let bindings = [];
      return {
        bind(...values) {
          bindings = values;
          return this;
        },
        async run() {
          if (!sql.includes("INSERT OR IGNORE INTO refinements")) {
            throw new Error(`unexpected run SQL: ${sql}`);
          }
          const [device_id, refinement_key, input_key, output_key, model, mode, created_at] = bindings;
          assert.match(sql, /model, mode, created_at/);
          assert.ok(["refine", "redraw"].includes(mode));
          const existing = rows.find((row) => (
            row.device_id === device_id && row.refinement_key === refinement_key
          ));
          if (!existing) {
            rows.push({
              id: rows.length + 1,
              device_id,
              refinement_key,
              input_key,
              output_key,
              model,
              mode,
              created_at,
              input_available: 1,
            });
          }
          return { success: true };
        },
        async all() {
          if (!sql.includes("FROM refinements")) throw new Error(`unexpected all SQL: ${sql}`);
          return { results: [...rows].reverse() };
        },
        async first() {
          if (!sql.includes("FROM refinements WHERE id = ?")) {
            throw new Error(`unexpected first SQL: ${sql}`);
          }
          return rows.find((row) => row.id === Number(bindings[0])) || null;
        },
      };
    },
  };
}

const env = {
  MAILBOX_DEVICE_TOKEN: "device-secret",
  MAILBOX_FAMILY_TOKEN: "family-secret",
  MAILBOX_DB: memoryRefinementDb(),
};

function historyDb(messages = [], refinements = [], replies = []) {
  const migrations = ["0001_init.sql", "0002_refinement_history.sql", "0003_refinement_legacy_inputs.sql", "0004_history_indexes.sql", "0005_refinement_mode.sql"];
  const schema = migrations.map((name) => readFileSync(new URL(`../migrations/${name}`, import.meta.url), "utf8")).join("\n");
  return {
    prepare(sql) {
      let bindings = [];
      const query = () => {
        const result = spawnSync("python3", ["-c", `
import json, sqlite3, sys
data = json.load(sys.stdin)
db = sqlite3.connect(':memory:')
db.row_factory = sqlite3.Row
db.executescript(data['schema'])
for table in ('messages', 'refinements', 'replies'):
    for row in data[table]:
        keys = list(row)
        db.execute('INSERT INTO ' + table + ' (' + ','.join(keys) + ') VALUES (' + ','.join('?' for _ in keys) + ')', list(row.values()))
print(json.dumps([dict(row) for row in db.execute(data['sql'], data['bindings'])]))
`], { input: JSON.stringify({ schema, messages, refinements, replies, sql, bindings }), encoding: "utf8" });
        assert.equal(result.status, 0, result.stderr);
        return JSON.parse(result.stdout);
      };
      return {
        bind(...values) { bindings = values; return this; },
        async all() { return { results: query() }; },
        async first() { return query()[0] ?? null; },
      };
    },
  };
}

function deviceHistoryRequest(path, authorization = "Bearer device-secret") {
  return new Request(`https://example.test/api/device/history${path}`, {
    headers: authorization ? { Authorization: authorization } : {},
  });
}

test("device history authenticates before validation or any storage access", async () => {
  const forbiddenStorage = new Proxy({}, { get() { throw new Error("storage accessed"); } });
  for (const path of ["?limit=bad", "/messages/1.png", "/refinements/1/input.png", "/refinements/1/output.png?x=1"]) {
    for (const auth of [null, "Bearer wrong", "Bearer family-secret"]) {
      const response = await worker.fetch(deviceHistoryRequest(path, auth), {
        ...env, MAILBOX_DB: forbiddenStorage, MAILBOX_IMAGES: forbiddenStorage,
      });
      assert.equal(response.status, 401);
      assert.equal(response.headers.get("WWW-Authenticate"), 'Bearer realm="paper-plane-device"');
      assert.equal(await response.text(), "unauthorized\n");
    }
  }
});

test("device history rejects duplicate, unknown and malformed parameters", async () => {
  for (const query of ["limit=0", "limit=51", "limit=-1", "limit=01", "limit=1.5", "limit=", "limit=1e1",
    "limit=1&limit=2", "unknown=1", "cursor=", "cursor=x", "cursor=x&cursor=x",
    ...["legacy|1|1", "2026-11-15T00:00:00Z|3|1", "2026-11-15T00:00:00Z|2|0",
      "2026-11-15T00:00:00Z|2|01", "2026-11-15T00:00:00Z|2|9007199254740992",
      "2026-11-15T00:00:00.000Z|2|1"].map((cursor) => `cursor=${encodeURIComponent(cursor)}`)]) {
    const response = await worker.fetch(deviceHistoryRequest(`?${query}`), env);
    assert.equal(response.status, 400, query);
  }
});

function historyFixtures() {
  const timestamp = "2026-11-15T02:36:40Z";
  const messages = [1, 2, 3].map((id) => ({ id, device_id: "kobo", image_key: `message-${id}`, created_at: timestamp }));
  const refinements = [1, 2, 3, 4, 5].map((id) => ({ id, device_id: "kobo", refinement_key: `key-${id}`,
    input_key: `input-${id}`, output_key: `output-${id}`, input_available: id === 3 ? 0 : 1,
    mode: id === 2 || id === 4 ? "redraw" : "refine",
    model: id === 3 ? "old\tmodel\r\n" : "MAI-Image-2.6", created_at: id >= 3 ? "legacy" : timestamp }));
  const replies = [{ id: 1, message_id: 2, body: "hello", created_at: timestamp }];
  return { ...env, MAILBOX_DB: historyDb(messages, refinements, replies), MAILBOX_IMAGES: memoryKv() };
}

test("device history uses real SQLite ordering and gap-free keyset pagination, legacy last", async () => {
  const historyEnv = historyFixtures();
  let cursor = null;
  const items = [];
  do {
    const response = await worker.fetch(deviceHistoryRequest(`?limit=2${cursor ? `&cursor=${encodeURIComponent(cursor)}` : ""}`), historyEnv);
    assert.equal(response.status, 200);
    assert.equal(response.headers.get("Content-Type"), "text/tab-separated-values; charset=utf-8");
    for (const [key, value] of Object.entries({ "Cache-Control": "no-store", "X-Content-Type-Options": "nosniff",
      "Referrer-Policy": "no-referrer", "X-Frame-Options": "DENY" })) assert.equal(response.headers.get(key), value);
    const text = await response.text();
    assert.ok(text.endsWith("\n"));
    const page = text.trimEnd().split("\n").map((line) => line.split("\t"));
    const fields = text.slice(0, -1).split("\n").map((line) => line.split("\t"));
    assert.equal(page.length, 2);
    for (const row of fields) assert.equal(row.length, 6);
    items.push(...fields);
    cursor = response.headers.get("X-Next-Cursor");
  } while (cursor);
  assert.deepEqual(items.map((row) => `${row[0]}-${row[1]}`), ["message-3", "message-2", "message-1", "redraw-2", "refinement-1", "refinement-5", "redraw-4", "refinement-3"]);
  assert.equal(items[1][4], "1");
  assert.equal(items[0][4], "0");
  assert.equal(items.at(-1)[2], "legacy");
  assert.equal(items.at(-1)[3], "0");
  assert.equal(items.at(-1)[5], "oldmodel");
  const empty = await worker.fetch(deviceHistoryRequest(""), { ...env, MAILBOX_DB: historyDb() });
  assert.equal(await empty.text(), "");
  assert.equal(empty.headers.get("X-Next-Cursor"), null);
});

test("device history downloads message and both refinement sides, rejects absent images", async () => {
  const historyEnv = historyFixtures();
  const png = makePng();
  for (const key of ["message-1", "input-1", "output-1", "input-2", "output-2", "output-3"]) await historyEnv.MAILBOX_IMAGES.put(key, png);
  for (const path of ["/messages/1.png", "/refinements/1/input.png", "/refinements/1/output.png", "/refinements/2/input.png", "/refinements/2/output.png", "/refinements/3/output.png"]) {
    const response = await worker.fetch(deviceHistoryRequest(path), historyEnv);
    assert.equal(response.status, 200);
    assert.equal(response.headers.get("Content-Type"), "image/png");
    assert.deepEqual(new Uint8Array(await response.arrayBuffer()), png);
  }
  for (const path of ["/messages/999.png", "/messages/2.png", "/refinements/999/output.png", "/refinements/3/input.png",
    "/messages/1.png?x=1", "/messages/01.png", "/refinements/1/other.png"]) {
    assert.equal((await worker.fetch(deviceHistoryRequest(path), historyEnv)).status, 404, path);
  }
});

function memoryKv() {
  const values = new Map();
  const metadata = new Map();
  return {
    values,
    metadata,
    async get(key, type) {
      const value = values.get(key);
      if (value == null) return null;
      if (type === "arrayBuffer") {
        return value.buffer.slice(value.byteOffset, value.byteOffset + value.byteLength);
      }
      return value;
    },
    async getWithMetadata(key, type) {
      return {
        value: await this.get(key, type),
        metadata: metadata.get(key) || null,
      };
    },
    async put(key, value, options = {}) {
      values.set(key, new Uint8Array(value));
      metadata.set(key, options.metadata || null);
    },
    async delete(key) {
      values.delete(key);
      metadata.delete(key);
    },
  };
}

function refinementRequest(input, mode) {
  return new Request("https://example.test/api/device/refinements", {
    method: "POST",
    headers: {
      Authorization: "Bearer device-secret",
      "Content-Type": "image/png",
      "Content-Length": String(input.byteLength),
      "X-Device-Id": "ian-kobo",
      "X-Refinement-Key": "1234567890abcdef",
      ...(mode === undefined ? {} : { "X-Refinement-Mode": mode }),
    },
    body: input,
  });
}

function openAiRefinementEnv(overrides = {}) {
  return {
    ...env,
    AZURE_OPENAI_API_KEY: "azure-openai-test-secret",
    AZURE_OPENAI_ENDPOINT: "https://openai-test.openai.azure.com/",
    MAILBOX_DB: memoryRefinementDb(),
    MAILBOX_IMAGES: memoryKv(),
    ...overrides,
  };
}

test("refinement mode validation is strict and follows device authentication", async () => {
  const originalFetch = globalThis.fetch;
  let upstreamCalls = 0;
  globalThis.fetch = async () => { upstreamCalls += 1; throw new Error("unexpected upstream"); };
  try {
    for (const mode of ["", "REFINE", "REDRAW", "other", "refine,redraw", "null"]) {
      const refineEnv = openAiRefinementEnv();
      const request = refinementRequest(makePng(), mode);
      const response = await worker.fetch(request, refineEnv);
      assert.equal(response.status, 400, JSON.stringify(mode));
      assert.equal(await response.text(), "invalid refinement mode\n");
      const unauthorized = refinementRequest(makePng(), mode);
      unauthorized.headers.set("Authorization", "Bearer wrong");
      assert.equal((await worker.fetch(unauthorized, refineEnv)).status, 401);
      assert.equal(refineEnv.MAILBOX_DB.rows.length, 0);
      assert.equal(refineEnv.MAILBOX_IMAGES.values.size, 0);
    }
    assert.equal(upstreamCalls, 0);
  } finally {
    globalThis.fetch = originalFetch;
  }
});

test("refine and redraw isolate identical PNG keys, prompts, metadata and history", async () => {
  const input = makePng();
  const outputs = [makePng([chunk("tEXt", new TextEncoder().encode("refine"))]),
    makePng([chunk("tEXt", new TextEncoder().encode("redraw"))])];
  const refineEnv = openAiRefinementEnv({
    AZURE_MAI_API_KEY: "mai-test-secret", AZURE_MAI_ENDPOINT: "https://mai-test.services.ai.azure.com",
  });
  const originalFetch = globalThis.fetch;
  const forms = [];
  globalThis.fetch = async (_url, options) => {
    forms.push(options.body);
    return Response.json({ data: [{ b64_json: Buffer.from(outputs[forms.length - 1]).toString("base64") }] });
  };
  try {
    for (const [index, mode] of ["refine", "redraw"].entries()) {
      const first = await worker.fetch(refinementRequest(input, mode), refineEnv);
      assert.equal(first.status, 200);
      assert.equal(first.headers.get("X-Refinement-Cache"), "miss");
      assert.equal(first.headers.get("X-Refinement-Mode"), mode);
      assert.deepEqual(new Uint8Array(await first.arrayBuffer()), outputs[index]);
      const key = `${mode === "redraw" ? "redraw-" : ""}1234567890abcdef`;
      assert.deepEqual(refineEnv.MAILBOX_IMAGES.values.get(`refinement-inputs/ian-kobo/${key}.png`), input);
      assert.deepEqual(refineEnv.MAILBOX_IMAGES.metadata.get(`refinements/ian-kobo/${key}.png`), {
        contentType: "image/png", role: "output", model: "MAI-Image-2.6", mode,
      });
    }
    assert.equal(forms.length, 2);
    assert.equal(forms[0].get("input_fidelity"), "high");
    assert.match(forms[0].get("prompt"), /preserving the original subject, composition/);
    assert.equal(forms[1].has("input_fidelity"), false);
    const prompt = forms[1].get("prompt");
    for (const phrase of [/boldly redraw and reimagine the entire scene/, /proportions/, /expressive poses/,
      /perspective/, /lighting/, /textures/, /complete imaginative environment/, /handwritten words/,
      /Chinese handwriting/, /sketch with words/, /Do not merely reproduce or typeset/, /weapons/,
      /age-appropriate/, /grayscale on e-ink/]) assert.match(prompt, phrase);
    assert.deepEqual(refineEnv.MAILBOX_DB.rows.map(({ refinement_key, mode }) => ({ refinement_key, mode })), [
      { refinement_key: "1234567890abcdef", mode: "refine" },
      { refinement_key: "redraw:1234567890abcdef", mode: "redraw" },
    ]);
    const database = historyDb([], refineEnv.MAILBOX_DB.rows);
    assert.deepEqual((await database.prepare("SELECT mode FROM refinements ORDER BY id").all()).results,
      [{ mode: "refine" }, { mode: "redraw" }]);
    for (const mode of [undefined, "refine", "redraw"]) {
      const cached = await worker.fetch(refinementRequest(input, mode), refineEnv);
      assert.equal(cached.headers.get("X-Refinement-Cache"), "hit");
      assert.equal(cached.headers.get("X-Refinement-Mode"), mode ?? "refine");
      assert.deepEqual(new Uint8Array(await cached.arrayBuffer()), outputs[mode === "redraw" ? 1 : 0]);
    }
    assert.equal(forms.length, 2);
    assert.equal(refineEnv.MAILBOX_DB.rows.length, 2);
  } finally {
    globalThis.fetch = originalFetch;
  }
});

test("redraw omits input fidelity throughout every MAI and OpenAI fallback tier", async () => {
  const refineEnv = openAiRefinementEnv({
    AZURE_MAI_API_KEY: "mai-test-secret", AZURE_MAI_ENDPOINT: "https://mai-test.services.ai.azure.com",
  });
  const expected = [...refinementModels({}), ...azureOpenAiRefinementModels({})];
  const models = [];
  const originalFetch = globalThis.fetch;
  globalThis.fetch = async (_url, options) => {
    models.push(options.body.get("model"));
    assert.equal(options.body.has("input_fidelity"), false);
    assert.match(options.body.get("prompt"), /boldly redraw/);
    assert.equal(options.body.get("moderation"), "auto");
    return models.length === expected.length
      ? Response.json({ data: [{ b64_json: Buffer.from(makePng()).toString("base64") }] })
      : new Response("capacity", { status: 429 });
  };
  try {
    const response = await worker.fetch(refinementRequest(makePng(), "redraw"), refineEnv);
    assert.equal(response.status, 200);
    assert.equal(response.headers.get("X-Refinement-Mode"), "redraw");
    assert.equal(response.headers.get("X-Refinement-Model"), "gpt-image-2");
    assert.deepEqual(models, expected);
  } finally {
    globalThis.fetch = originalFetch;
  }
});

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
  assert.match(html, /AI 创作历史/);
  assert.match(html, /ian-kobo&lt;script&gt;/);
  assert.match(html, /你好 &lt;b&gt;Ian&lt;\/b&gt;/);
  assert.doesNotMatch(html, /<b>Ian<\/b>/);
});

test("refinement history renders protected before-and-after pairs safely", () => {
  const html = renderRefinementHistory([{
    id: 7,
    device_id: "ian<script>",
    model: "MAI<Pro>",
    created_at: "2026-07-27T12:00:00Z",
    input_available: 1,
  }]);
  assert.match(html, /发给 AI 的原图/);
  assert.match(html, /AI 生成的图片/);
  assert.match(html, /\/refinements\/7\/input\.png/);
  assert.match(html, /\/refinements\/7\/output\.png/);
  assert.match(html, /ian&lt;script&gt;/);
  assert.match(html, /MAI&lt;Pro&gt;/);
  assert.match(html, /模式：AI 润色 · 模型：MAI&lt;Pro&gt;/);
  assert.doesNotMatch(html, /ian<script>/);
  const redraw = renderRefinementHistory([{
    id: 9, device_id: "ian-kobo", mode: "redraw", model: '<img src=x onerror="alert(1)">',
    created_at: "2026-11-15T00:00:00Z", input_available: 1,
  }]);
  assert.match(redraw, /模式：AI 大幅重画 · 模型：&lt;img/);
  assert.doesNotMatch(redraw, /<img src=x/);
  assert.doesNotMatch(renderRefinementHistory([{ id: 10, mode: "<script>" }]), /<script>/);

  const legacy = renderRefinementHistory([{
    id: 8,
    device_id: "ian-kobo",
    model: "gpt-image-2",
    created_at: "legacy",
    input_available: 0,
  }]);
  assert.match(legacy, /旧版本当时没有保存原图/);
  assert.doesNotMatch(legacy, /\/refinements\/8\/input\.png/);
  assert.match(legacy, /\/refinements\/8\/output\.png/);
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

  const history = await worker.fetch(new Request("https://example.test/refinements"), env);
  assert.equal(history.status, 403);
  const image = await worker.fetch(new Request("https://example.test/refinements/1/input.png"), env);
  assert.equal(image.status, 403);
});

test("family can browse protected refinement history and both images", async () => {
  const database = memoryRefinementDb();
  const images = memoryKv();
  const input = makePng([chunk("tEXt", new TextEncoder().encode("input"))]);
  const output = makePng([chunk("tEXt", new TextEncoder().encode("output"))]);
  database.rows.push({
    id: 1,
    device_id: "ian-kobo",
    refinement_key: "0123456789abcdef",
    input_key: "history/input.png",
    output_key: "history/output.png",
    model: "MAI-Image-2.5-Pro",
    created_at: "2026-07-27T12:00:00Z",
    input_available: 1,
  });
  await images.put("history/input.png", input);
  await images.put("history/output.png", output);
  const historyEnv = { ...env, MAILBOX_DB: database, MAILBOX_IMAGES: images };
  const bootstrap = await worker.fetch(
    new Request("https://example.test/?token=family-secret"),
    historyEnv,
  );
  assert.equal(bootstrap.status, 303);
  const cookie = bootstrap.headers.get("Set-Cookie").split(";", 1)[0];

  const page = await worker.fetch(new Request("https://example.test/refinements", {
    headers: { Cookie: cookie },
  }), historyEnv);
  assert.equal(page.status, 200);
  assert.match(await page.text(), /MAI-Image-2\.5-Pro/);

  for (const [side, expected] of [["input", input], ["output", output]]) {
    const response = await worker.fetch(new Request(
      `https://example.test/refinements/1/${side}.png`,
      { headers: { Cookie: cookie } },
    ), historyEnv);
    assert.equal(response.status, 200);
    assert.equal(response.headers.get("Content-Type"), "image/png");
    assert.deepEqual(new Uint8Array(await response.arrayBuffer()), expected);
  }
});

test("device can refine a PNG through MAI-Image-2.6 and retries use the KV cache", async () => {
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
    assert.equal(options.body.get("model"), "MAI-Image-2.6");
    assert.equal(options.body.get("input_fidelity"), "high");
    assert.equal(options.body.get("quality"), "low");
    assert.equal(options.body.get("size"), "1024x1536");
    assert.equal(options.body.get("output_format"), "png");
    assert.match(options.body.get("prompt"), /child.*drawing/i);
    assert.match(options.body.get("prompt"), /handwritten words/i);
    assert.match(options.body.get("prompt"), /Chinese handwriting/i);
    assert.match(options.body.get("prompt"), /generate the picture/i);
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
    assert.equal(first.headers.get("X-Refinement-Mode"), "refine");
    assert.equal(first.headers.get("X-Refinement-Model"), "MAI-Image-2.6");
    assert.deepEqual(new Uint8Array(await first.arrayBuffer()), refined);

    const second = await worker.fetch(request(), refineEnv);
    assert.equal(second.status, 200);
    assert.equal(second.headers.get("X-Refinement-Cache"), "hit");
    assert.equal(second.headers.get("X-Refinement-Mode"), "refine");
    assert.equal(second.headers.get("X-Refinement-Model"), "MAI-Image-2.6");
    assert.deepEqual(new Uint8Array(await second.arrayBuffer()), refined);
    assert.equal(openAiCalls, 1);
    assert.deepEqual(images.values.get("refinement-inputs/ian-kobo/0123456789abcdef.png"), input);
    assert.deepEqual(images.values.get("refinements/ian-kobo/0123456789abcdef.png"), refined);
    const history = refineEnv.MAILBOX_DB.rows.find((row) => row.refinement_key === "0123456789abcdef");
    assert.equal(history.device_id, "ian-kobo");
    assert.equal(history.model, "MAI-Image-2.6");
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
    assert.deepEqual(attempted, [
      "MAI-Image-2.6",
      "MAI-Image-2.6-Flash",
      "MAI-Image-2.5-Pro",
      "MAI-Image-2.5",
      "MAI-Image-2.5-Flash",
    ]);
    assert.deepEqual(new Uint8Array(await response.arrayBuffer()), refined);
  } finally {
    globalThis.fetch = originalFetch;
  }
});

test("refinement reaches legacy gpt-image-2 after every newer tier is exhausted", async () => {
  const input = makePng();
  const refined = makePng([chunk("tEXt", new TextEncoder().encode("gpt-image-2"))]);
  const images = memoryKv();
  const refineEnv = {
    ...env,
    AZURE_MAI_API_KEY: "azure-mai-test-secret",
    AZURE_MAI_ENDPOINT: "https://mai-test.services.ai.azure.com",
    AZURE_OPENAI_API_KEY: "azure-openai-test-secret",
    AZURE_OPENAI_ENDPOINT: "https://openai-test.openai.azure.com/",
    MAILBOX_IMAGES: images,
  };
  const attempted = [];
  const originalFetch = globalThis.fetch;
  globalThis.fetch = async (url, options) => {
    const model = options.body.get("model");
    attempted.push(model);
    if (url === "https://mai-test.services.ai.azure.com/mai/v1/images/edits") {
      assert.equal(options.headers["api-key"], "azure-mai-test-secret");
      assert.equal(options.body.get("image").type, "image/png");
      assert.equal(options.body.get("image[]"), null);
      return new Response("rate limited", { status: 429 });
    }

    assert.equal(url, "https://openai-test.openai.azure.com/openai/v1/images/edits");
    assert.equal(options.headers["api-key"], "azure-openai-test-secret");
    assert.equal(options.body.get("image"), null);
    assert.equal(options.body.get("input_fidelity"), model === "gpt-image-2" ? "high" : null);
    assert.equal(options.body.get("quality"), "low");
    assert.equal(options.body.get("size"), "1024x1536");
    assert.equal(options.body.get("output_format"), "png");
    assert.equal(options.body.get("moderation"), "auto");
    assert.equal(options.body.get("n"), "1");
    const image = options.body.get("image[]");
    assert.equal(image.type, "image/png");
    assert.deepEqual(new Uint8Array(await image.arrayBuffer()), input);
    if (model !== "gpt-image-2") return new Response("rate limited", { status: 429 });
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
        "X-Refinement-Key": "1234567890abcdef",
      },
      body: input,
    }), refineEnv);
    assert.equal(response.status, 200);
    assert.equal(response.headers.get("X-Refinement-Model"), "gpt-image-2");
    assert.deepEqual(attempted, [
      "MAI-Image-2.6",
      "MAI-Image-2.6-Flash",
      "MAI-Image-2.5-Pro",
      "MAI-Image-2.5",
      "MAI-Image-2.5-Flash",
      "gpt-image-2.5-sunburst",
      "gpt-image-2.5-flare",
      "gpt-image-2",
    ]);
    assert.deepEqual(new Uint8Array(await response.arrayBuffer()), refined);
  } finally {
    globalThis.fetch = originalFetch;
  }
});

test("a non-moderation MAI 400 skips directly to Azure sunburst", async () => {
  const input = makePng();
  const refined = makePng([chunk("tEXt", new TextEncoder().encode("gpt-image-2-after-400"))]);
  const images = memoryKv();
  const refineEnv = {
    ...env,
    AZURE_MAI_API_KEY: "azure-mai-test-secret",
    AZURE_MAI_ENDPOINT: "https://mai-test.services.ai.azure.com",
    AZURE_OPENAI_API_KEY: "azure-openai-test-secret",
    AZURE_OPENAI_ENDPOINT: "https://openai-test.openai.azure.com",
    MAILBOX_IMAGES: images,
  };
  const attempted = [];
  const originalFetch = globalThis.fetch;
  globalThis.fetch = async (url, options) => {
    attempted.push(options.body.get("model"));
    if (url === "https://mai-test.services.ai.azure.com/mai/v1/images/edits") {
      return Response.json({
        error: { code: "invalid_request_error", message: "unsupported request parameter" },
      }, { status: 400 });
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
        "X-Refinement-Key": "f0f0f0f0f0f0f0f0",
      },
      body: input,
    }), refineEnv);
    assert.equal(response.status, 200);
    assert.equal(response.headers.get("X-Refinement-Model"), "gpt-image-2.5-sunburst");
    assert.deepEqual(attempted, ["MAI-Image-2.6", "gpt-image-2.5-sunburst"]);
    assert.deepEqual(new Uint8Array(await response.arrayBuffer()), refined);
  } finally {
    globalThis.fetch = originalFetch;
  }
});

test("a moderation 400 from the first tier does not cross providers", async () => {
  const input = makePng();
  const images = memoryKv();
  const refineEnv = {
    ...env,
    AZURE_MAI_API_KEY: "azure-mai-test-secret",
    AZURE_MAI_ENDPOINT: "https://mai-test.services.ai.azure.com",
    AZURE_OPENAI_API_KEY: "azure-openai-test-secret",
    AZURE_OPENAI_ENDPOINT: "https://openai-test.openai.azure.com",
    MAILBOX_IMAGES: images,
  };
  let calls = 0;
  const originalFetch = globalThis.fetch;
  globalThis.fetch = async () => {
    calls += 1;
    return Response.json({
      error: {
        code: "content_policy_violation",
        message: "The request was rejected by the safety system.",
      },
    }, { status: 400 });
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
    [
      "MAI-Image-2.6",
      "MAI-Image-2.6-Flash",
      "MAI-Image-2.5-Pro",
      "MAI-Image-2.5",
      "MAI-Image-2.5-Flash",
    ],
  );
  assert.deepEqual(
    refinementModels({ AZURE_MAI_MODEL: " MAI-Image-2.5 , MAI-Image-2.5-Flash " }),
    ["MAI-Image-2.5", "MAI-Image-2.5-Flash"],
  );
  assert.deepEqual(
    refinementModels({ AZURE_MAI_MODEL: "  " }),
    [
      "MAI-Image-2.6",
      "MAI-Image-2.6-Flash",
      "MAI-Image-2.5-Pro",
      "MAI-Image-2.5",
      "MAI-Image-2.5-Flash",
    ],
  );
});

test("AZURE_OPENAI_IMAGE_DEPLOYMENT accepts a single deployment or an ordered list", () => {
  const defaults = ["gpt-image-2.5-sunburst", "gpt-image-2.5-flare", "gpt-image-2"];
  assert.deepEqual(azureOpenAiRefinementModels({}), defaults);
  assert.deepEqual(azureOpenAiRefinementModels({ AZURE_OPENAI_IMAGE_DEPLOYMENT: "  " }), defaults);
  assert.deepEqual(
    azureOpenAiRefinementModels({ AZURE_OPENAI_IMAGE_DEPLOYMENT: " gpt-image-2 " }),
    ["gpt-image-2"],
  );
  assert.deepEqual(
    azureOpenAiRefinementModels({
      AZURE_OPENAI_IMAGE_DEPLOYMENT: " gpt-image-2.5-flare , , gpt-image-2 ",
    }),
    ["gpt-image-2.5-flare", "gpt-image-2"],
  );
  assert.deepEqual(
    azureOpenAiRefinementModels({ AZURE_OPENAI_IMAGE_DEPLOYMENT: `bad\rname,${"x".repeat(101)}` }),
    defaults,
  );
});

test("Azure OpenAI advances past deployment failures and caches only the successful model", async (t) => {
  const failures = [
    ["success", null],
    ["network failure", () => { throw new Error("connection reset"); }],
    ["invalid JSON", () => new Response("{")],
    ["missing image", () => Response.json({ data: [] })],
    ["invalid base64", () => Response.json({ data: [{ b64_json: "!" }] })],
    ["invalid PNG", () => Response.json({ data: [{ b64_json: "aW52YWxpZA==" }] })],
    ...[400, 404, 408, 409, 422, 429, 500, 503].map((status) => [
      `HTTP ${status}`,
      () => Response.json({ error: { code: "deployment_unavailable" } }, { status }),
    ]),
  ];
  for (const [name, fail] of failures) {
    await t.test(name, async () => {
      const input = makePng();
      const refined = makePng([chunk("tEXt", new TextEncoder().encode(name))]);
      const refineEnv = openAiRefinementEnv();
      const attempts = [];
      const originalFetch = globalThis.fetch;
      globalThis.fetch = async (url, options) => {
        attempts.push({ url, options });
        if (attempts.length === 1 && fail) return fail();
        return Response.json({ data: [{ b64_json: Buffer.from(refined).toString("base64") }] });
      };
      try {
        const successfulModel = fail ? "gpt-image-2.5-flare" : "gpt-image-2.5-sunburst";
        const expectedModels = fail
          ? ["gpt-image-2.5-sunburst", "gpt-image-2.5-flare"]
          : ["gpt-image-2.5-sunburst"];
        const first = await worker.fetch(refinementRequest(input), refineEnv);
        assert.equal(first.status, 200);
        assert.equal(first.headers.get("X-Refinement-Model"), successfulModel);
        assert.equal(first.headers.get("X-Refinement-Cache"), "miss");
        assert.deepEqual(new Uint8Array(await first.arrayBuffer()), refined);
        assert.deepEqual(attempts.map(({ options }) => options.body.get("model")), expectedModels);
        for (const { url, options } of attempts) {
          assert.equal(url, "https://openai-test.openai.azure.com/openai/v1/images/edits");
          assert.equal(options.headers["api-key"], "azure-openai-test-secret");
          assert.equal(options.body.get("image"), null);
          assert.deepEqual(new Uint8Array(await options.body.get("image[]").arrayBuffer()), input);
          assert.equal(options.body.has("input_fidelity"), false);
          assert.equal(options.body.get("quality"), "low");
          assert.equal(options.body.get("size"), "1024x1536");
          assert.equal(options.body.get("output_format"), "png");
          assert.equal(options.body.get("moderation"), "auto");
          assert.equal(options.body.get("n"), "1");
        }
        const cacheKey = "refinements/ian-kobo/1234567890abcdef.png";
        assert.equal(refineEnv.MAILBOX_IMAGES.metadata.get(cacheKey).model, successfulModel);
        assert.equal(refineEnv.MAILBOX_DB.rows.length, 1);
        assert.equal(refineEnv.MAILBOX_DB.rows[0].model, successfulModel);

        const cached = await worker.fetch(refinementRequest(input), refineEnv);
        assert.equal(cached.headers.get("X-Refinement-Cache"), "hit");
        assert.equal(cached.headers.get("X-Refinement-Model"), successfulModel);
        assert.deepEqual(new Uint8Array(await cached.arrayBuffer()), refined);
        assert.equal(attempts.length, expectedModels.length);
        assert.equal(refineEnv.MAILBOX_DB.rows.length, 1);
      } finally {
        globalThis.fetch = originalFetch;
      }
    });
  }
});

test("Azure OpenAI stops on moderation, shared authentication, and permanent request failures", async (t) => {
  const failures = [
    ["moderation", 400, { error: { code: "content_policy_violation" } }],
    ["header moderation", 429, {}, { "x-ms-error-code": "content_filter" }],
    ["unauthorized", 401, { error: { code: "invalid_api_key" } }],
    ["forbidden", 403, { error: { code: "permission_denied" } }],
    ["oversized request", 413, { error: { code: "request_too_large" } }],
  ];
  for (const [name, status, body, headers] of failures) {
    await t.test(name, async () => {
      const refineEnv = openAiRefinementEnv();
      let calls = 0;
      const originalFetch = globalThis.fetch;
      globalThis.fetch = async () => {
        calls += 1;
        return Response.json(body, { status, headers });
      };
      try {
        const response = await worker.fetch(refinementRequest(makePng()), refineEnv);
        assert.equal(response.status, 502);
        assert.equal(await response.text(), "image refinement failed\n");
        assert.equal(calls, 1);
        assert.equal(refineEnv.MAILBOX_IMAGES.values.size, 0);
        assert.equal(refineEnv.MAILBOX_DB.rows.length, 0);
      } finally {
        globalThis.fetch = originalFetch;
      }
    });
  }
});

test("Azure OpenAI exhausts only the configured deployments without caching a failure", async (t) => {
  const configurations = [
    ["defaults", {}, ["gpt-image-2.5-sunburst", "gpt-image-2.5-flare", "gpt-image-2"]],
    ["single override", { AZURE_OPENAI_IMAGE_DEPLOYMENT: "gpt-image-2" }, ["gpt-image-2"]],
    [
      "ordered override",
      { AZURE_OPENAI_IMAGE_DEPLOYMENT: " gpt-image-2.5-flare , gpt-image-2 " },
      ["gpt-image-2.5-flare", "gpt-image-2"],
    ],
  ];
  for (const [name, overrides, expectedModels] of configurations) {
    await t.test(name, async () => {
      const refineEnv = openAiRefinementEnv(overrides);
      const attempted = [];
      const originalFetch = globalThis.fetch;
      globalThis.fetch = async (_url, options) => {
        attempted.push(options.body.get("model"));
        return new Response("upstream detail must stay private", { status: 503 });
      };
      try {
        const response = await worker.fetch(refinementRequest(makePng()), refineEnv);
        assert.equal(response.status, 502);
        assert.equal(await response.text(), "image refinement failed\n");
        assert.deepEqual(attempted, expectedModels);
        assert.equal(refineEnv.MAILBOX_IMAGES.values.size, 0);
        assert.equal(refineEnv.MAILBOX_DB.rows.length, 0);
      } finally {
        globalThis.fetch = originalFetch;
      }
    });
  }
});

test("existing legacy-model cache entries remain usable after the model upgrade", async () => {
  const input = makePng();
  const cachedImage = makePng([chunk("tEXt", new TextEncoder().encode("legacy output"))]);
  const refineEnv = openAiRefinementEnv();
  await refineEnv.MAILBOX_IMAGES.put("refinements/ian-kobo/1234567890abcdef.png", cachedImage, {
    metadata: { contentType: "image/png", role: "output", model: "gpt-image-2" },
  });
  let calls = 0;
  const originalFetch = globalThis.fetch;
  globalThis.fetch = async () => {
    calls += 1;
    throw new Error("cached output must not invoke an upstream");
  };
  try {
    const response = await worker.fetch(refinementRequest(input), refineEnv);
    assert.equal(response.status, 200);
    assert.equal(response.headers.get("X-Refinement-Cache"), "hit");
    assert.equal(response.headers.get("X-Refinement-Model"), "gpt-image-2");
    assert.deepEqual(new Uint8Array(await response.arrayBuffer()), cachedImage);
    assert.equal(calls, 0);
    assert.equal(refineEnv.MAILBOX_DB.rows[0].model, "gpt-image-2");
  } finally {
    globalThis.fetch = originalFetch;
  }
});

test("input fidelity is omitted only for the two known GPT Image 2.5 deployments", async (t) => {
  const deployments = [
    ["mai", "MAI-Image-2.6", "high"],
    ["mai", "MAI-Image-2.6-Flash", "high"],
    ["mai", "MAI-Image-2.5-Pro", "high"],
    ["mai", "MAI-Image-2.5", "high"],
    ["mai", "MAI-Image-2.5-Flash", "high"],
    ["openai", "gpt-image-2.5-sunburst", null],
    ["openai", "gpt-image-2.5-flare", null],
    ["openai", "gpt-image-2", "high"],
    ["openai", "custom-image-deployment", "high"],
    ["openai", "my-gpt-image-2.5-sunburst", "high"],
  ];
  for (const [provider, model, inputFidelity] of deployments) {
    await t.test(model, async () => {
      const input = makePng();
      const refineEnv = provider === "mai"
        ? {
          ...env,
          AZURE_MAI_API_KEY: "azure-mai-test-secret",
          AZURE_MAI_ENDPOINT: "https://mai-test.services.ai.azure.com",
          AZURE_MAI_MODEL: model,
          MAILBOX_DB: memoryRefinementDb(),
          MAILBOX_IMAGES: memoryKv(),
        }
        : openAiRefinementEnv({ AZURE_OPENAI_IMAGE_DEPLOYMENT: model });
      let submitted;
      const originalFetch = globalThis.fetch;
      globalThis.fetch = async (_url, options) => {
        submitted = options.body;
        return Response.json({ data: [{ b64_json: Buffer.from(input).toString("base64") }] });
      };
      try {
        const response = await worker.fetch(refinementRequest(input), refineEnv);
        assert.equal(response.status, 200);
        assert.equal(submitted.get("model"), model);
        assert.equal(submitted.get("input_fidelity"), inputFidelity);
        assert.equal(submitted.has("input_fidelity"), inputFidelity !== null);
        assert.equal(submitted.get("quality"), "low");
        assert.equal(submitted.get("size"), "1024x1536");
        assert.equal(submitted.get("output_format"), "png");
        assert.equal(submitted.get("moderation"), "auto");
        assert.equal(submitted.get("n"), "1");
        const imageField = provider === "mai" ? "image" : "image[]";
        assert.deepEqual(new Uint8Array(await submitted.get(imageField).arrayBuffer()), input);
      } finally {
        globalThis.fetch = originalFetch;
      }
    });
  }
});
