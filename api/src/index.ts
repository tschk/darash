import { Hono } from "hono";
import { cors } from "hono/cors";
import { spawnSync } from "node:child_process";

const DARASH_BIN = process.env.DARASH_BIN ?? "darash";
const PORT = Number(process.env.PORT ?? 8787);
const TIMEOUT_MS = Number(process.env.DARASH_TIMEOUT_MS ?? 90_000);

type Flag = { name: string; value?: string };

function run(command: string[], env?: Record<string, string>) {
  const result = spawnSync(DARASH_BIN, command, {
    env: { ...process.env, ...env },
    timeout: TIMEOUT_MS,
    encoding: "utf8",
  });
  return {
    code: result.status,
    stdout: (result.stdout ?? "").trimEnd(),
    stderr: (result.stderr ?? "").trimEnd(),
  };
}

const app = new Hono();

app.use("*", cors());

app.get("/", (c) =>
  c.json({
    service: "darash-api",
    crate: "https://crates.io/crates/darash",
    version: "0.7.0",
    endpoints: {
      "GET /search": "q, mode=speed|balanced|quality, sources=web,academic,discussions, url, json",
      "GET /fetch":
        "url, md, text, outline, select, row, table, body, locate, count, where, offset, limit, budget, header, json, jsonEnvelope, tsv, all, fresh, noCache, method, data, user, head, maxTime, maxBytes, fail",
      "GET /health": "liveness",
    },
  }),
);

app.get("/health", (c) => c.json({ ok: true }));

app.get("/search", (c) => {
  const q = c.req.query("q");
  if (!q || !q.trim()) return c.json({ error: "q is required" }, 400);

  const args = ["search", q, "--json"];
  const mode = c.req.query("mode");
  if (mode) args.push("--mode", mode);
  const endpoint = c.req.query("url");
  if (endpoint) args.push("--url", endpoint);
  for (const source of parseList(c.req.query("sources"))) {
    args.push("--source", source);
  }

  const started = Date.now();
  const { code, stdout, stderr } = run(args);
  const ms = Date.now() - started;
  if (code !== 0) {
    return c.json({ error: "search failed", detail: stderr }, 502);
  }
  return c.json({ data: JSON.parse(stdout), meta: { ms } });
});

app.get("/fetch", (c) => {
  const url = c.req.query("url");
  if (!url || !url.trim()) return c.json({ error: "url is required" }, 400);

  const flags: Flag[] = [];
  const on = (name: string, query: string) => {
    if (c.req.query(query) !== undefined) flags.push({ name });
  };
  on("--md", "md");
  on("--text", "text");
  on("--outline", "outline");
  on("--table", "table");
  on("--body", "body");
  on("--count", "count");
  on("--tsv", "tsv");
  on("--all", "all");
  on("--fresh", "fresh");
  on("--no-cache", "noCache");
  on("--head", "head");
  on("--fail", "fail");
  if (c.req.query("jsonEnvelope") !== undefined) {
    flags.push({ name: "--json-envelope" });
  } else {
    flags.push({ name: "--json" });
  }
  const valued: [string, string][] = [
    ["--select", "select"],
    ["--row", "row"],
    ["--limit", "limit"],
    ["--budget", "budget"],
    ["--offset", "offset"],
    ["--where", "where"],
    ["--locate", "locate"],
    ["--method", "method"],
    ["--data", "data"],
    ["--user", "user"],
    ["--max-time", "maxTime"],
    ["--max-bytes", "maxBytes"],
  ];
  for (const [name, query] of valued) {
    const value = c.req.query(query);
    if (value) flags.push({ name, value });
  }
  for (const header of parseList(c.req.query("header"))) {
    flags.push({ name: "--header", value: header });
  }

  const args = ["fetch", url];
  for (const flag of flags) {
    args.push(flag.name);
    if (flag.value !== undefined) args.push(flag.value);
  }

  const started = Date.now();
  const { code, stdout, stderr } = run(args);
  const ms = Date.now() - started;
  if (code !== 0) {
    return c.json({ error: "fetch failed", detail: stderr }, code === 22 ? 422 : 502);
  }
  const parsed = JSON.parse(stdout);
  if (typeof parsed === "number") {
    return c.json({ data: parsed, meta: { ms } });
  }
  return c.json({ ...parsed, meta: { ...(parsed.meta ?? {}), ms } });
});

function parseList(value?: string): string[] {
  return (value ?? "")
    .split(",")
    .map((part) => part.trim())
    .filter(Boolean);
}

// Bun serves the default export over HTTP (port + fetch).
export default { port: PORT, fetch: app.fetch };
