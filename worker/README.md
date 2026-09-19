# darash-site worker

A single Cloudflare Worker that serves **both** the static darash
documentation site (the files in `../site/`, untouched by this directory) and
the darash HTTP API, with live request analytics.

- **Static site** — served straight from the `ASSETS` binding (`../site`),
  including `index.html` and `llms.txt`. No SPA fallback: unknown paths are a
  plain static 404.
- **API** — `/api/*` is handled by a [Hono](https://hono.dev) app and is
  CORS-enabled (`*`). The site itself is same-origin and gets no CORS headers.
- **Analytics** — every request is logged to a single global `Counter` Durable
  Object: durable cumulative totals plus a live in-memory ring of the last 30
  requests, streamed over WebSocket.

The search endpoint is **SearXNG-backed** and returns provider-neutral JSON;
the fetch endpoint runs entirely **in-worker** (no external reader service).

## Run locally

```sh
cd worker
npm install
npm run dev          # wrangler dev, http://localhost:8787
```

`wrangler dev` gives you a local `ASSETS` binding, the `COUNTER` Durable
Object (SQLite-backed), and a local `CF-Connecting-IP`-less environment (the
client pseudonym falls back to a stable `local` hash).

## Deploy

```sh
cd worker
npm run deploy       # wrangler deploy
```

The first deploy applies the `v1` migration that creates the `Counter`
SQLite-backed Durable Object. `wrangler.jsonc` uses `run_worker_first: true`
so the Worker sees every request (that is what makes static requests
countable); it re-serves assets through `env.ASSETS.fetch()`.

## Configuration

`wrangler.jsonc` only; no environment variables, secrets, or `.dev.vars` are
required. The worker has no external API keys.

| Binding   | Type                     | Purpose                          |
| --------- | ------------------------ | -------------------------------- |
| `ASSETS`  | Static assets (`../site`) | serves the docs site             |
| `COUNTER` | Durable Object `Counter`  | totals + live event ring + WS    |

## Endpoints

| Method | Path          | Query / notes                                                                 |
| ------ | ------------- | ----------------------------------------------------------------------------- |
| GET    | `/api/health` | `{"ok":true}`                                                                  |
| GET    | `/api/search` | `q` (required, ≤512 chars), `mode=speed\|balanced\|quality` (default `balanced`), `limit` (default 8, max 50) |
| GET    | `/api/fetch`  | `url` (required), `md`, `text`, `outline`, `select=<css>`, `limit`, `budget`   |
| GET    | `/api/stats`  | `{ totals, live }` — polled by the site (cheap; `no-store`)                    |
| GET    | `/api/account`| requires key; `{ tier, name, usage }` with window reset timestamps             |
| GET    | `/api/live`   | WebSocket upgrade; streams `{type:"event",event}` plus a `{type:"hello",...}` primer |
| POST   | `/api/admin/keys` | admin only (`Authorization: Bearer $ADMIN_SECRET`); body `{tier, name?}` → key, returned once |
| GET    | `/api/admin/keys` | admin only; list keys with usage today                          |
| POST   | `/api/admin/keys/revoke` | admin only; body `{key}`                                  |
| GET    | `/llms.txt`   | served from `ASSETS` like any other static file                               |
| GET    | `/*`          | static site                                                                   |

## Auth & rate limits

Keys come from `Authorization: Bearer dk_…` (or `?key=dk_…`). Keys are stored
as SHA-256 hashes only; the raw key is shown once at creation. `/api/health`
and `/api/stats` are unmetered.

| Tier | Hourly | Daily  | How to get it                          |
| ---- | ------ | ------ | -------------------------------------- |
| anon | 30     | 100    | no key (per-IP)                        |
| free | 60     | 1,000  | `POST /api/admin/keys {"tier":"free"}` |
| pro  | 600    | 25,000 | `POST /api/admin/keys {"tier":"pro"}`  |

Over-limit responses are `429` with a `Retry-After` header. Errors (4xx/5xx)
do not burn quota. Check usage any time with `GET /api/account`.

Set the admin secret once with `wrangler secret put ADMIN_SECRET`; admin
routes 401 while it is unset.

### `GET /api/search`

Queries the SearXNG instances listed in `src/index.ts` in order, each with a
4 s `AbortController` timeout and a browser-like User-Agent. `mode` controls
how many successful instances are merged: `speed` = 1, `balanced` = 2,
`quality` = 3. Results are deduped by normalized `host+path` and scored as
`instances * 10 + position bonus`, then capped at `limit`.

```sh
curl 'http://localhost:8787/api/search?q=rust+async&mode=quality&limit=5'
```

```jsonc
{
  "data": { "query": "rust async", "results": [ { "title": "…", "url": "…", "content": "…", "score": 23, "engine": "…" } ] },
  "meta": { "ms": 812, "instances": ["https://searx.be", "https://priv.au"] }
}
```

On total failure: `502` with `{ "error": "all search instances failed", "detail": [...] }`.

### `GET /api/fetch`

Fetches the URL in-worker (browser-ish UA, 10 s timeout, 2 MB cap) and parses
it with [`linkedom`](https://github.com/WebReflection/linkedom). Mode priority
is `md` → `text` → `outline` → `select`; with no mode, `md` is used.

- `md` — headings, paragraphs, lists, links, images, and tables as Markdown.
- `text` — visible text with markup stripped.
- `outline` — `tag` / `tag.class` occurrence counts (repeats only, sorted desc).
- `select=<css>` — trimmed `textContent` of every match.
- `budget=n` — for `select`, keeps whole items up to ~`n*4` chars (always ≥1).

```sh
curl 'http://localhost:8787/api/fetch?url=https://example.com&md'
curl 'http://localhost:8787/api/fetch?url=https://example.com&outline'
curl 'http://localhost:8787/api/fetch?url=https://example.com&select=h1&budget=100'
```

```jsonc
{
  "data": "# Example Domain\n\n…",
  "meta": { "status": 200, "url": "https://example.com/", "ms": 240, "bytes": 1256, "contentType": "text/html" }
}
```

A bad upstream status or a body over 2 MB returns `502` with
`{ "error": …, "status": …, "url": … }`.

### `GET /api/stats` and `GET /api/live`

`/api/stats` returns the Durable Object's state:

```jsonc
{
  "totals": { "requests": 42, "searches": 7, "fetches": 3, "errors": 0, "startedAt": 1750000000000 },
  "live": [ { "id": "…", "ts": 1750000000000, "method": "GET", "path": "/api/search?q=…", "kind": "search", "status": 200, "ms": 640, "client": "1a2b3c4d" } ]
}
```

Query parameter **values** are redacted in `path` (names kept); `client` is the
first 8 hex chars of `SHA-256(CF-Connecting-IP + "darash-live-salt")`. Connect
to `/api/live` for the same events as they happen:

```js
const socket = new WebSocket("ws://localhost:8787/api/live");
socket.onmessage = (event) => console.log(JSON.parse(event.data));
```

## Notes & limits

- Totals are persisted to Durable Object storage on every event; the live ring
  is memory-only (it resets when the object is evicted).
- `errors` counts responses with status ≥ 400.
- `budget` applies to `select` item lists; `md`/`text` are returned whole.
