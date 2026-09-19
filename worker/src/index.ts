/**
 * darash-site worker.
 *
 * One Cloudflare Worker that serves both the static documentation site
 * (`../site`, via the ASSETS binding) and the darash HTTP API, plus live
 * request analytics backed by a single global Durable Object.
 *
 * Routing:
 *   - `/api/*` is handled by the Hono app below (CORS `*`).
 *   - everything else falls through to `env.ASSETS.fetch()` (plain static,
 *     no SPA fallback).
 *
 * Because `run_worker_first: true` is set in wrangler.jsonc, every request
 * (static included) reaches this worker, so the logging middleware sees and
 * counts all of them.
 */

import { Hono } from "hono";
import { cors } from "hono/cors";
import { parseHTML } from "linkedom";
import type { RequestEvent, Totals } from "./counter";
import { TIER_LIMITS, type RateDecision, type Tier } from "./accounts";

// The Durable Object classes must be exported from the entry module.
export { Counter } from "./counter";
export { Accounts } from "./accounts";

// ---------------------------------------------------------------------------
// Environment & constants
// ---------------------------------------------------------------------------

export interface Env {
  ASSETS: Fetcher;
  COUNTER: DurableObjectNamespace;
  ACCOUNTS: DurableObjectNamespace;
  ADMIN_SECRET?: string;
}

/** SearXNG instances tried in order (provider-neutral JSON out). */
const SEARXNG_INSTANCES = [
  "https://searx.be",
  "https://priv.au",
  "https://paulgo.io",
  "https://search.bus-hit.me",
  "https://searx.tiekoetter.com",
] as const;

const USER_AGENT =
  "Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 (KHTML, like Gecko) " +
  "Chrome/124.0 Safari/537.36 darash/0.7.0";

const CLIENT_SALT = "darash-live-salt";
const COUNTER_NAME = "global";
const SEARCH_TIMEOUT_MS = 4_000;
const FETCH_TIMEOUT_MS = 10_000;
const MAX_FETCH_BYTES = 2 * 1024 * 1024;
const DEFAULT_LIMIT = 8;
const MAX_LIMIT = 50;
const MAX_QUERY_CHARS = 512;

type SearchMode = "speed" | "balanced" | "quality";
type EventKind = RequestEvent["kind"];

type SearchResult = {
  title: string;
  url: string;
  content: string;
  score: number;
  engine: string;
};

type OutlineEntry = { selector: string; count: number };

// ---------------------------------------------------------------------------
// App & middleware
// ---------------------------------------------------------------------------

const app = new Hono<{ Bindings: Env }>();

// Rate limiting + account resolution for the metered API (everything under
// /api except health and stats). Registered before logging so a 429 is also
// logged with its final status. Anonymous callers are limited per-IP, keyed
// callers per-key; invalid keys are rejected outright.
app.use("/api/*", async (c, next) => {
  const pathname = new URL(c.req.url).pathname;
  if (pathname.startsWith("/api/health") || pathname.startsWith("/api/stats") || pathname.startsWith("/api/admin")) {
    return next();
  }

  const rawKey = extractKey(c.req.raw, c.req.url);
  let tier: Tier | "anon" = "anon";
  let identity: string;

  if (rawKey) {
    const lookup = await accountsStub(c.env).fetch("https://accounts/lookup", {
      method: "POST",
      headers: { "content-type": "application/json" },
      body: JSON.stringify({ key: rawKey }),
    });
    if (!lookup.ok) {
      return c.json({ error: "invalid api key" }, 401);
    }
    const { account } = await lookup.json<{ account: { tier: Tier; keyHash: string } | null }>();
    if (!account) {
      return c.json({ error: "invalid api key" }, 401);
    }
    tier = account.tier;
    identity = `key:${account.keyHash}`;
  } else {
    identity = `anon:${await clientPseudonym(c.req.raw)}`;
  }

  const checkResponse = await accountsStub(c.env).fetch("https://accounts/check", {
    method: "POST",
    headers: { "content-type": "application/json" },
    body: JSON.stringify({ identity, tier }),
  });
  const decision = await checkResponse.json<RateDecision>();

  if (!decision.allowed) {
    c.header("retry-after", String(decision.retryAfter));
    return c.json(
      {
        error: "rate limit exceeded",
        tier,
        limits: { hour: TIER_LIMITS[tier].hour, day: TIER_LIMITS[tier].day },
        retryAfter: decision.retryAfter,
      },
      429,
    );
  }

  await next();

  // Only successful responses burn quota.
  if (c.res.status < 400) {
    await accountsStub(c.env).fetch("https://accounts/record", {
      method: "POST",
      headers: { "content-type": "application/json" },
      body: JSON.stringify({ identity }),
    });
  }
  c.set("tier", tier);
});

declare module "hono" {
  interface ContextVariableMap {
    tier: Tier | "anon";
  }
}

// Log and count every request. Registered first so it wraps all routing and
// records the final status/duration. The DO call is deferred via waitUntil so
// it never delays the response.
app.use("*", async (c, next) => {
  const started = Date.now();
  let status = 500;
  try {
    await next();
    status = c.res.status;
  } finally {
    const url = new URL(c.req.url);
    const request = c.req.raw;
    const { env, executionCtx } = c;
    const base: Omit<RequestEvent, "client"> = {
      id: crypto.randomUUID(),
      ts: Date.now(),
      method: request.method,
      path: redactQuery(url),
      kind: kindFor(url.pathname),
      status,
      ms: Date.now() - started,
      tier: c.get("tier") ?? "anon",
    };
    executionCtx.waitUntil(
      (async () => {
        const client = await clientPseudonym(request);
        await counterStub(env).fetch("https://counter/add", {
          method: "POST",
          headers: { "content-type": "application/json" },
          body: JSON.stringify({ ...base, client } satisfies RequestEvent),
        });
      })().catch(() => undefined),
    );
  }
});

// CORS is enabled for the API only (the site is same-origin static).
// WebSocket upgrades are skipped so the 101 handshake is left untouched.
const apiCors = cors({ origin: "*" });
app.use("/api/*", async (c, next) => {
  if (c.req.header("upgrade")?.toLowerCase() === "websocket") {
    return next();
  }
  return apiCors(c, next);
});

// ---------------------------------------------------------------------------
// Health
// ---------------------------------------------------------------------------

app.get("/api/health", (c) => c.json({ ok: true }));

// ---------------------------------------------------------------------------
// Search (SearXNG-backed)
// ---------------------------------------------------------------------------

app.get("/api/search", async (c) => {
  const started = Date.now();
  const query = (c.req.query("q") ?? "").trim();
  if (!query) {
    return c.json({ error: "q is required" }, 400);
  }
  if (query.length > MAX_QUERY_CHARS) {
    return c.json({ error: `q must be at most ${MAX_QUERY_CHARS} characters` }, 400);
  }

  const mode = parseMode(c.req.query("mode"));
  const limit = parseLimit(c.req.query("limit"));
  const wanted = mode === "speed" ? 1 : mode === "balanced" ? 2 : 3;

  const used: string[] = [];
  const lists: SearchResult[][] = [];
  const failures: string[] = [];

  for (const base of SEARXNG_INSTANCES) {
    if (lists.length >= wanted) break;
    try {
      lists.push(await queryInstance(base, query));
      used.push(base);
    } catch (error) {
      failures.push(`${base}: ${errorMessage(error)}`);
    }
  }

  if (lists.length === 0) {
    // Datacenter egress is often 429'd by public SearXNG instances; fall back
    // to keyless engines that tolerate server-side fetches.
    for (const fallback of [queryBingRss, queryMarginalia]) {
      try {
        const results = await fallback(query);
        if (results.length > 0) {
          lists.push(results);
          used.push(results[0].engine);
          break;
        }
      } catch (error) {
        failures.push(`${fallback.name}: ${errorMessage(error)}`);
      }
    }
  }

  if (lists.length === 0) {
    return c.json({ error: "all search instances failed", detail: failures }, 502);
  }

  return c.json({
    data: { query, results: mergeResults(lists, limit) },
    meta: { ms: Date.now() - started, instances: used },
  });
});

async function queryInstance(base: string, query: string): Promise<SearchResult[]> {
  const controller = new AbortController();
  const timer = setTimeout(() => controller.abort(), SEARCH_TIMEOUT_MS);
  try {
    const target = `${base}/search?q=${encodeURIComponent(query)}&format=json`;
    const response = await fetch(target, {
      headers: { "user-agent": USER_AGENT, accept: "application/json" },
      signal: controller.signal,
    });
    if (!response.ok) {
      throw new Error(`HTTP ${response.status}`);
    }
    const body = (await response.json()) as { results?: unknown };
    if (!Array.isArray(body.results)) {
      throw new Error("no results array");
    }
    return body.results
      .filter((item): item is Record<string, unknown> => typeof item === "object" && item !== null)
      .map((item) => ({
        title: asString(item.title),
        url: asString(item.url),
        content: asString(item.content),
        engine: asString(item.engine),
        score: 0,
      }))
      .filter((result) => result.url.length > 0);
  } finally {
    clearTimeout(timer);
  }
}

/** Dedupe by normalized host+path, score by instance count + position. */
function mergeResults(lists: SearchResult[][], limit: number): SearchResult[] {
  const merged = new Map<string, SearchResult>();
  for (const list of lists) {
    list.forEach((result, index) => {
      const key = normalizeUrl(result.url);
      const positionBonus = list.length - index;
      const existing = merged.get(key);
      if (existing) {
        existing.score += 10 + positionBonus;
      } else {
        merged.set(key, { ...result, score: 10 + positionBonus });
      }
    });
  }
  return [...merged.values()].sort((a, b) => b.score - a.score).slice(0, limit);
}

/** Bing RSS fallback (keyless, datacenter-friendly). */
async function queryBingRss(query: string): Promise<SearchResult[]> {
  const controller = new AbortController();
  const timer = setTimeout(() => controller.abort(), SEARCH_TIMEOUT_MS * 2);
  try {
    const response = await fetch(`https://www.bing.com/search?q=${encodeURIComponent(query)}&format=rss`, {
      headers: { "user-agent": USER_AGENT, accept: "application/rss+xml, text/xml" },
      signal: controller.signal,
    });
    if (!response.ok) {
      throw new Error(`HTTP ${response.status}`);
    }
    const xml = await response.text();
    const decode = (value: string) =>
      value
        .replace(/&lt;/g, "<")
        .replace(/&gt;/g, ">")
        .replace(/&amp;/g, "&")
        .replace(/&#x27;|&#39;/g, "'")
        .replace(/&quot;/g, '"')
        .trim();
    const results: SearchResult[] = [];
    const items = /<item>([\s\S]*?)<\/item>/g;
    let match: RegExpExecArray | null;
    while ((match = items.exec(xml)) !== null) {
      const item = match[1];
      const title = decode(item.match(/<title>([\s\S]*?)<\/title>/)?.[1] ?? "");
      const url = decode(item.match(/<link>([\s\S]*?)<\/link>/)?.[1] ?? "");
      const content = decode(item.match(/<description>([\s\S]*?)<\/description>/)?.[1] ?? "");
      if (!title || !/^https?:\/\//.test(url)) {
        continue;
      }
      results.push({ title, url, content, score: 0, engine: "bing" });
    }
    return results;
  } finally {
    clearTimeout(timer);
  }
}

/** Marginalia public API fallback (keyless, no-bot index). */
async function queryMarginalia(query: string): Promise<SearchResult[]> {
  const controller = new AbortController();
  const timer = setTimeout(() => controller.abort(), SEARCH_TIMEOUT_MS * 2);
  try {
    const response = await fetch(`https://api.marginalia.nu/public/search/${encodeURIComponent(query)}`, {
      headers: { "user-agent": USER_AGENT, accept: "application/json" },
      signal: controller.signal,
    });
    if (!response.ok) {
      throw new Error(`HTTP ${response.status}`);
    }
    const body = (await response.json()) as { results?: unknown };
    if (!Array.isArray(body.results)) {
      throw new Error("no results array");
    }
    return body.results
      .filter((item): item is Record<string, unknown> => typeof item === "object" && item !== null)
      .map((item) => ({
        title: asString(item.title),
        url: asString(item.url),
        content: asString(item.description),
        score: 0,
        engine: "marginalia",
      }))
      .filter((result) => result.title.length > 0 && result.url.startsWith("http"));
  } finally {
    clearTimeout(timer);
  }
}


function normalizeUrl(raw: string): string {
  try {
    const url = new URL(raw);
    const host = url.host.toLowerCase().replace(/^www\./, "");
    const path = url.pathname.replace(/\/+$/, "");
    return `${host}${path}`;
  } catch {
    return raw;
  }
}

// ---------------------------------------------------------------------------
// Fetch + extract (in-worker)
// ---------------------------------------------------------------------------

app.get("/api/fetch", async (c) => {
  const started = Date.now();
  const rawUrl = (c.req.query("url") ?? "").trim();
  if (!rawUrl) {
    return c.json({ error: "url is required" }, 400);
  }

  let target: URL;
  try {
    target = new URL(rawUrl);
  } catch {
    return c.json({ error: "invalid url" }, 400);
  }
  if (target.protocol !== "http:" && target.protocol !== "https:") {
    return c.json({ error: "only http and https urls are supported" }, 400);
  }

  const controller = new AbortController();
  const timer = setTimeout(() => controller.abort(), FETCH_TIMEOUT_MS);
  let upstream: Response;
  try {
    upstream = await fetch(target.toString(), {
      headers: {
        "user-agent": USER_AGENT,
        accept: "text/html,application/xhtml+xml,application/xml;q=0.9,*/*;q=0.8",
      },
      redirect: "follow",
      signal: controller.signal,
    });
  } catch (error) {
    return c.json({ error: "fetch failed", detail: errorMessage(error) }, 502);
  } finally {
    clearTimeout(timer);
  }

  const contentType = upstream.headers.get("content-type") ?? "";
  const url = upstream.url || target.toString();

  if (!upstream.ok) {
    return c.json({ error: "bad upstream status", status: upstream.status, url }, 502);
  }

  const declared = Number.parseInt(upstream.headers.get("content-length") ?? "", 10);
  if (Number.isFinite(declared) && declared > MAX_FETCH_BYTES) {
    return c.json(
      { error: "response too large", status: upstream.status, url, limit: MAX_FETCH_BYTES },
      502,
    );
  }

  let bytes: Uint8Array;
  try {
    bytes = await readCapped(upstream, MAX_FETCH_BYTES);
  } catch (error) {
    if (error instanceof BodyTooLargeError) {
      return c.json(
        { error: "response too large", status: upstream.status, url, limit: MAX_FETCH_BYTES },
        502,
      );
    }
    return c.json({ error: "fetch failed", detail: errorMessage(error), url }, 502);
  }

  const html = new TextDecoder("utf-8").decode(bytes);
  const document = parseHTML(html).document as unknown as DomElement;

  const limit = parseOptionalInt(c.req.query("limit"));
  const budget = parseOptionalInt(c.req.query("budget"));

  let data: string | string[] | OutlineEntry[];
  let omitted = 0;
  try {
    if (flag(c.req.query("md"))) {
      data = toMarkdown(rootElement(document));
    } else if (flag(c.req.query("text"))) {
      data = toText(rootElement(document));
    } else if (flag(c.req.query("outline"))) {
      const entries = outline(document);
      data = limit === undefined ? entries : entries.slice(0, limit);
    } else if (c.req.query("select") !== undefined) {
      const selector = c.req.query("select") ?? "";
      if (!selector.trim()) {
        return c.json({ error: "select requires a CSS selector" }, 400);
      }
      let items = selectTexts(document, selector);
      if (limit !== undefined) {
        items = items.slice(0, limit);
      }
      const budgeted = applyBudget(items, budget);
      items = budgeted.items;
      omitted = budgeted.omitted;
      data = items;
    } else {
      // No mode requested: default to markdown.
      data = toMarkdown(rootElement(document));
    }
  } catch (error) {
    return c.json({ error: "extraction failed", detail: errorMessage(error) }, 400);
  }

  return c.json({
    data,
    meta: {
      status: upstream.status,
      url,
      ms: Date.now() - started,
      bytes: bytes.byteLength,
      contentType,
      ...(omitted > 0 ? { omitted } : {}),
    },
  });
});

class BodyTooLargeError extends Error {}

async function readCapped(response: Response, cap: number): Promise<Uint8Array> {
  if (!response.body) {
    return new Uint8Array(await response.arrayBuffer());
  }
  const reader = response.body.getReader();
  const chunks: Uint8Array[] = [];
  let total = 0;
  for (;;) {
    const { done, value } = await reader.read();
    if (done) break;
    total += value.byteLength;
    if (total > cap) {
      await reader.cancel().catch(() => undefined);
      throw new BodyTooLargeError(`body exceeds ${cap} bytes`);
    }
    chunks.push(value);
  }
  const out = new Uint8Array(total);
  let offset = 0;
  for (const chunk of chunks) {
    out.set(chunk, offset);
    offset += chunk.byteLength;
  }
  return out;
}

// ---------------------------------------------------------------------------
// Account (requires key)
// ---------------------------------------------------------------------------

app.get("/api/account", async (c) => {
  const rawKey = extractKey(c.req.raw, c.req.url);
  if (!rawKey) {
    return c.json({ error: "api key required (Authorization: Bearer <key>)" }, 401);
  }
  const lookupResponse = await accountsStub(c.env).fetch("https://accounts/lookup", {
    method: "POST",
    headers: { "content-type": "application/json" },
    body: JSON.stringify({ key: rawKey }),
  });
  if (!lookupResponse.ok) {
    return c.json({ error: "invalid api key" }, 401);
  }
  const { account } = await lookupResponse.json<{
    account: { tier: Tier; keyHash: string; name: string } | null;
  }>();
  if (!account) {
    return c.json({ error: "invalid api key" }, 401);
  }
  const snapshotResponse = await accountsStub(c.env).fetch("https://accounts/snapshot", {
    method: "POST",
    headers: { "content-type": "application/json" },
    body: JSON.stringify({ identity: `key:${account.keyHash}`, tier: account.tier }),
  });
  const snapshot = await snapshotResponse.json<{ tier: Tier | "anon"; usage: Record<string, number> }>();
  return c.json({ ...snapshot, name: account.name }, 200, { "cache-control": "no-store" });
});

// ---------------------------------------------------------------------------
// Admin (requires ADMIN_SECRET) — issue, list, revoke keys
// ---------------------------------------------------------------------------

function adminAuthorized(c: { req: { raw: Request } }, env: Env): boolean {
  const expected = env.ADMIN_SECRET;
  if (!expected) return false;
  const header = c.req.raw.headers.get("authorization") ?? "";
  return header === `Bearer ${expected}`;
}

app.post("/api/admin/keys", async (c) => {
  if (!adminAuthorized(c, c.env)) {
    return c.json({ error: "unauthorized" }, 401);
  }
  const body = await c.req.json<{ tier?: string; name?: string }>().catch(() => ({}) as { tier?: string; name?: string });
  const response = await accountsStub(c.env).fetch("https://accounts/create", {
    method: "POST",
    headers: { "content-type": "application/json" },
    body: JSON.stringify({ tier: body.tier, name: body.name ?? "" }),
  });
  return c.json(await response.json(), 201);
});

app.get("/api/admin/keys", async (c) => {
  if (!adminAuthorized(c, c.env)) {
    return c.json({ error: "unauthorized" }, 401);
  }
  const response = await accountsStub(c.env).fetch("https://accounts/list");
  return c.json(await response.json());
});

app.post("/api/admin/keys/revoke", async (c) => {
  if (!adminAuthorized(c, c.env)) {
    return c.json({ error: "unauthorized" }, 401);
  }
  const body = await c.req.json<{ key?: string }>().catch(() => ({}) as { key?: string });
  const response = await accountsStub(c.env).fetch("https://accounts/revoke", {
    method: "POST",
    headers: { "content-type": "application/json" },
    body: JSON.stringify({ key: body.key }),
  });
  return c.json(await response.json());
});

// ---------------------------------------------------------------------------
// Stats & live stream (backed by the Counter DO)
// ---------------------------------------------------------------------------

app.get("/api/stats", async (c) => {
  const response = await counterStub(c.env).fetch("https://counter/state");
  const state = (await response.json()) as { totals: Totals; live: RequestEvent[] };
  c.header("cache-control", "no-store");
  return c.json(state);
});

app.get("/api/live", async (c) => {
  if (c.req.header("upgrade")?.toLowerCase() !== "websocket") {
    return c.json({ error: "expected websocket upgrade" }, 426);
  }
  return counterStub(c.env).fetch(c.req.raw);
});

// ---------------------------------------------------------------------------
// Fallbacks
// ---------------------------------------------------------------------------

app.all("/api/*", (c) => c.json({ error: "not found" }, 404));
app.all("*", (c) => c.env.ASSETS.fetch(c.req.raw));

export default app;

// ---------------------------------------------------------------------------
// Counter helpers
// ---------------------------------------------------------------------------

function counterStub(env: Env): DurableObjectStub {
  return env.COUNTER.get(env.COUNTER.idFromName(COUNTER_NAME));
}

function accountsStub(env: Env): DurableObjectStub {
  return env.ACCOUNTS.get(env.ACCOUNTS.idFromName(COUNTER_NAME));
}

/** Raw key from `Authorization: Bearer <key>` or `?key=`; null otherwise. */
function extractKey(request: Request, url: string): string | null {
  const header = request.headers.get("authorization") ?? "";
  if (header.startsWith("Bearer dk_")) {
    return header.slice("Bearer ".length).trim();
  }
  const query = new URL(url).searchParams.get("key");
  return query && query.startsWith("dk_") ? query : null;
}


async function clientPseudonym(request: Request): Promise<string> {
  const ip =
    request.headers.get("cf-connecting-ip") ??
    request.headers.get("x-forwarded-for") ??
    "local";
  const digest = await crypto.subtle.digest(
    "SHA-256",
    new TextEncoder().encode(`${ip}${CLIENT_SALT}`),
  );
  return [...new Uint8Array(digest)]
    .slice(0, 4)
    .map((byte) => byte.toString(16).padStart(2, "0"))
    .join("");
}

/** Keep query parameter names, drop their values. */
function redactQuery(url: URL): string {
  if (!url.search) return url.pathname;
  const names = [...url.searchParams.keys()].map((name) => `${name}=…`);
  return `${url.pathname}?${names.join("&")}`;
}

function kindFor(pathname: string): EventKind {
  if (pathname.startsWith("/api/search")) return "search";
  if (pathname.startsWith("/api/fetch")) return "fetch";
  return "other";
}

// ---------------------------------------------------------------------------
// Small parsing helpers
// ---------------------------------------------------------------------------

function parseMode(value: string | undefined): SearchMode {
  return value === "speed" || value === "quality" ? value : "balanced";
}

function parseLimit(value: string | undefined): number {
  const parsed = Number.parseInt(value ?? "", 10);
  if (!Number.isFinite(parsed) || parsed <= 0) return DEFAULT_LIMIT;
  return Math.min(parsed, MAX_LIMIT);
}

function parseOptionalInt(value: string | undefined): number | undefined {
  if (value === undefined || value.trim() === "") return undefined;
  const parsed = Number.parseInt(value, 10);
  return Number.isFinite(parsed) ? parsed : undefined;
}

/** Presence means "on" (matching the existing API); `0`/`false` opt out. */
function flag(value: string | undefined): boolean {
  return value !== undefined && value !== "0" && value !== "false";
}

function asString(value: unknown): string {
  return typeof value === "string" ? value : "";
}

function errorMessage(error: unknown): string {
  return error instanceof Error ? error.message : String(error);
}

// ---------------------------------------------------------------------------
// DOM (linkedom) — minimal structural view
// ---------------------------------------------------------------------------

/**
 * linkedom's generated types are loose (`any`-heavy), so we describe the small
 * subset of the DOM we use and cast the parsed document once.
 */
interface DomNode {
  readonly nodeType: number;
  readonly textContent: string | null;
  readonly childNodes: ArrayLike<DomNode>;
}

interface DomElement extends DomNode {
  readonly tagName: string;
  getAttribute(name: string): string | null;
  readonly children: ArrayLike<DomElement>;
  querySelector(selector: string): DomElement | null;
  querySelectorAll(selector: string): ArrayLike<DomElement>;
}

function isElement(node: DomNode): node is DomElement {
  return node.nodeType === 1;
}

const SKIP_TAGS = new Set([
  "script",
  "style",
  "noscript",
  "template",
  "svg",
  "head",
  "iframe",
  "canvas",
]);

const BLOCK_TAGS = new Set([
  "address",
  "article",
  "aside",
  "blockquote",
  "body",
  "center",
  "dd",
  "details",
  "div",
  "dl",
  "dt",
  "fieldset",
  "figcaption",
  "figure",
  "footer",
  "form",
  "h1",
  "h2",
  "h3",
  "h4",
  "h5",
  "h6",
  "header",
  "hr",
  "li",
  "main",
  "nav",
  "ol",
  "p",
  "pre",
  "section",
  "summary",
  "table",
  "td",
  "th",
  "tr",
  "ul",
]);

function rootElement(document: DomElement): DomElement {
  return (
    document.querySelector("article") ??
    document.querySelector("main") ??
    document.querySelector("body") ??
    document
  );
}

function collapseWhitespace(value: string): string {
  return value.replace(/\s+/g, " ");
}

// ---------------------------------------------------------------------------
// Markdown rendering
// ---------------------------------------------------------------------------

function toMarkdown(root: DomElement): string {
  const blocks: string[] = [];
  renderBlocks(root, blocks);
  return blocks.join("\n\n").replace(/\n{3,}/g, "\n\n").trim();
}

function renderBlocks(container: DomElement, blocks: string[]): void {
  let buffer = "";
  const flush = () => {
    const text = collapseWhitespace(buffer).trim();
    if (text) blocks.push(text);
    buffer = "";
  };

  for (const node of Array.from(container.childNodes)) {
    if (node.nodeType === 3) {
      buffer += node.textContent ?? "";
      continue;
    }
    if (!isElement(node)) continue;
    const tag = node.tagName.toLowerCase();
    if (SKIP_TAGS.has(tag)) continue;

    if (/^h[1-6]$/.test(tag)) {
      flush();
      const text = inlineText(node);
      if (text) blocks.push(`${"#".repeat(Number(tag[1]))} ${text}`);
    } else if (tag === "p") {
      flush();
      const text = inlineText(node);
      if (text) blocks.push(text);
    } else if (tag === "ul" || tag === "ol") {
      flush();
      const list = renderList(node, tag === "ol");
      if (list) blocks.push(list);
    } else if (tag === "pre") {
      flush();
      const code = (node.textContent ?? "").replace(/\s+$/, "");
      if (code.trim()) blocks.push(`\`\`\`\n${code}\n\`\`\``);
    } else if (tag === "blockquote") {
      flush();
      const inner = toMarkdown(node);
      if (inner) {
        blocks.push(inner.split("\n").map((line) => `> ${line}`).join("\n"));
      }
    } else if (tag === "table") {
      flush();
      const table = renderTable(node);
      if (table) blocks.push(table);
    } else if (tag === "hr") {
      flush();
      blocks.push("---");
    } else if (BLOCK_TAGS.has(tag)) {
      flush();
      renderBlocks(node, blocks);
    } else {
      buffer += inlineNode(node);
    }
  }
  flush();
}

function renderList(list: DomElement, ordered: boolean): string {
  const lines: string[] = [];
  let index = 1;
  for (const item of Array.from(list.children)) {
    if (item.tagName.toLowerCase() !== "li") continue;
    let text = "";
    const nested: string[] = [];
    for (const node of Array.from(item.childNodes)) {
      if (isElement(node)) {
        const tag = node.tagName.toLowerCase();
        if (tag === "ul" || tag === "ol") {
          nested.push(renderList(node, tag === "ol"));
          continue;
        }
        if (SKIP_TAGS.has(tag)) continue;
      }
      text += node.nodeType === 3 ? node.textContent ?? "" : inlineNode(node);
    }
    lines.push(`${ordered ? `${index}. ` : "- "}${collapseWhitespace(text).trim()}`);
    for (const block of nested) {
      lines.push(block.split("\n").map((line) => `  ${line}`).join("\n"));
    }
    index += 1;
  }
  return lines.join("\n");
}

function renderTable(table: DomElement): string {
  const rows = Array.from(table.querySelectorAll("tr"));
  if (rows.length === 0) return "";
  const cells = (row: DomElement): string[] =>
    Array.from(row.querySelectorAll("th,td")).map((cell) =>
      collapseWhitespace(cell.textContent ?? "").trim().replace(/\|/g, "\\|"),
    );
  const header = cells(rows[0]);
  const width = Math.max(header.length, 1);
  const line = (values: string[]): string => {
    const padded = values.slice(0, width);
    while (padded.length < width) padded.push("");
    return `| ${padded.join(" | ")} |`;
  };
  const output = [line(header), `| ${Array.from({ length: width }, () => "---").join(" | ")} |`];
  for (const row of rows.slice(1)) {
    output.push(line(cells(row)));
  }
  return output.join("\n");
}

function inlineText(element: DomElement): string {
  return collapseWhitespace(
    Array.from(element.childNodes).map(inlineNode).join(""),
  ).trim();
}

function inlineNode(node: DomNode): string {
  if (node.nodeType === 3) return node.textContent ?? "";
  if (!isElement(node)) return "";
  const tag = node.tagName.toLowerCase();
  if (SKIP_TAGS.has(tag)) return "";

  switch (tag) {
    case "br":
      return "\n";
    case "a": {
      const text = inlineText(node);
      const href = node.getAttribute("href");
      return href ? `[${text || href}](${href})` : text;
    }
    case "img": {
      const alt = node.getAttribute("alt") ?? "";
      const src = node.getAttribute("src") ?? "";
      return src ? `![${alt}](${src})` : alt;
    }
    case "strong":
    case "b": {
      const text = inlineText(node);
      return text ? `**${text}**` : "";
    }
    case "em":
    case "i": {
      const text = inlineText(node);
      return text ? `*${text}*` : "";
    }
    case "code": {
      const text = collapseWhitespace(node.textContent ?? "").trim();
      return text ? `\`${text}\`` : "";
    }
    default:
      return Array.from(node.childNodes).map(inlineNode).join("");
  }
}

// ---------------------------------------------------------------------------
// Plain text
// ---------------------------------------------------------------------------

function toText(root: DomElement): string {
  const lines: string[] = [];
  let buffer = "";
  const flush = () => {
    const text = collapseWhitespace(buffer).trim();
    if (text) lines.push(text);
    buffer = "";
  };
  const walk = (element: DomElement) => {
    for (const node of Array.from(element.childNodes)) {
      if (node.nodeType === 3) {
        buffer += node.textContent ?? "";
        continue;
      }
      if (!isElement(node)) continue;
      const tag = node.tagName.toLowerCase();
      if (SKIP_TAGS.has(tag)) continue;
      if (BLOCK_TAGS.has(tag)) {
        flush();
        walk(node);
        flush();
      } else {
        walk(node);
      }
    }
  };
  walk(root);
  flush();
  return lines.join("\n");
}

// ---------------------------------------------------------------------------
// Outline & select
// ---------------------------------------------------------------------------

/** Count `tag` and `tag.class` occurrences, keep repeats, sort desc. */
function outline(document: DomElement): OutlineEntry[] {
  const counts = new Map<string, number>();
  const bump = (selector: string) => counts.set(selector, (counts.get(selector) ?? 0) + 1);

  for (const element of Array.from(document.querySelectorAll("*"))) {
    const tag = element.tagName.toLowerCase();
    if (SKIP_TAGS.has(tag)) continue;
    bump(tag);
    const firstClass = (element.getAttribute("class") ?? "").split(/\s+/).find(Boolean);
    if (firstClass) bump(`${tag}.${firstClass}`);
  }

  return [...counts.entries()]
    .filter(([, count]) => count >= 2)
    .map(([selector, count]) => ({ selector, count }))
    .sort((a, b) => b.count - a.count || a.selector.localeCompare(b.selector));
}

function selectTexts(document: DomElement, selector: string): string[] {
  return Array.from(document.querySelectorAll(selector)).map((element) =>
    collapseWhitespace(element.textContent ?? "").trim(),
  );
}

/** Keep whole items up to ~`budget * 4` chars, always at least one. */
function applyBudget(
  items: string[],
  budget: number | undefined,
): { items: string[]; omitted: number } {
  if (budget === undefined || budget <= 0) return { items, omitted: 0 };
  const cap = budget * 4;
  const kept: string[] = [];
  let used = 0;
  for (let index = 0; index < items.length; index += 1) {
    const cost = items[index].length;
    if (index > 0 && used + cost > cap) {
      return { items: kept, omitted: items.length - kept.length };
    }
    used += cost;
    kept.push(items[index]);
  }
  return { items: kept, omitted: 0 };
}
