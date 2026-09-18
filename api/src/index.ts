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
    endpoints: {
      "GET /search": "q, mode=speed|balanced|quality, sources=web,academic,discussions, url, json",
      "GET /fetch": "url, md, text, outline, select, row, table, body, limit, budget, header, json",
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
  if (c.req.query("md") !== undefined) flags.push({ name: "--md" });
  if (c.req.query("text") !== undefined) flags.push({ name: "--text" });
  if (c.req.query("outline") !== undefined) flags.push({ name: "--outline" });
  if (c.req.query("table") !== undefined) flags.push({ name: "--table" });
  if (c.req.query("body") !== undefined) flags.push({ name: "--body" });
  if (c.req.query("select")) flags.push({ name: "--select", value: c.req.query("select")! });
  if (c.req.query("row")) flags.push({ name: "--row", value: c.req.query("row")! });
  if (c.req.query("limit")) flags.push({ name: "--limit", value: c.req.query("limit")! });
  if (c.req.query("budget")) flags.push({ name: "--budget", value: c.req.query("budget")! });
  for (const header of parseList(c.req.query("header"))) {
    flags.push({ name: "--header", value: header });
  }

  const args = ["fetch", url, "--json"];
  for (const flag of flags) {
    args.push(flag.name);
    if (flag.value !== undefined) args.push(flag.value);
  }

  const started = Date.now();
  const { code, stdout, stderr } = run(args);
  const ms = Date.now() - started;
  if (code !== 0) {
    return c.json({ error: "fetch failed", detail: stderr }, 502);
  }
  const parsed = JSON.parse(stdout);
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
