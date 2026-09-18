# darash-api

A hosted Darash instance: HTTP search and fetch/extract served by
[Hono](https://hono.dev) on Bun, shelling out to the `darash` binary. This
lives on the `hono-api` branch, separate from the Rust crate.

## Run

```sh
cargo build --release                # or install darash: cargo install darash
cd api && bun install
DARASH_BIN=../target/release/darash bun run start   # listens on :8787
```

`DARASH_BIN` defaults to `darash` on `PATH`.

## Endpoints

- `GET /` — service info
- `GET /health` — liveness
- `GET /search?q=rust+async&mode=quality&sources=web,academic&json` — runs
  `darash search`, returns the full response JSON plus `meta.ms`
- `GET /fetch?url=https://example.com&md=1&budget=800` — runs
  `darach fetch --json` and forwards the `{data, meta}` envelope

Fetch options: `md`, `text`, `outline`, `table`, `body`, `select`,
`row`, `limit`, `budget`, `header` (comma-separated for repeats).

## Examples

```sh
curl 'localhost:8787/search?q=searxng'
curl 'localhost:8787/fetch?url=https://example.com&md=1'
curl 'localhost:8787/fetch?url=https://example.com&outline=1'
```
