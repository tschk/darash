/**
 * darash-web — public edge for darash.tsc.hk.
 *
 * Layering:
 *   1. `/api/*` streams straight to the Rust worker (`darash-site`) over a
 *      service binding — GET/POST/websockets, unmodified. The Rust worker keeps
 *      its name (and therefore its Durable Object state); only the route moved.
 *   2. `GET /` serves the built site document (crepuscularity shell + Svelte
 *      islands) from the ASSETS binding, which maps `../site`.
 *   3. Everything else falls through to moonshine's cloudflare adapter
 *      (`cloudflareFetch`): ASSETS-first for non-root GET/HEAD, then the
 *      Moonshine route graph (manifest has no dynamic routes today), then 404.
 */
import cloudflareFetch from "@tschk/moonshine-deploy-cloudflare";
import manifest from "./manifest.json";

type Env = {
  ASSETS: { fetch: (request: Request) => Promise<Response> };
  API: { fetch: (request: Request) => Promise<Response> };
};

type Ctx = { waitUntil: (promise: Promise<unknown>) => void };

export default {
  async fetch(request: Request, env: Env, ctx: Ctx): Promise<Response> {
    const { pathname } = new URL(request.url);

    // Same-origin API: hand the request to the Rust worker untouched.
    if (pathname === "/api" || pathname.startsWith("/api/")) {
      return env.API.fetch(request);
    }

    // The adapter skips the ASSETS binding at "/", so serve the document here.
    if (
      (request.method === "GET" || request.method === "HEAD") &&
      (pathname === "/" || pathname === "/index.html")
    ) {
      const home = await env.ASSETS.fetch(new Request(new URL("/", request.url), request));
      if (home.status !== 404) return home;
    }

    // Static assets (islands.js, unocss.js, favicon, llms.txt) and fallbacks.
    return cloudflareFetch(
      request,
      env,
      ctx,
      manifest as Parameters<typeof cloudflareFetch>[3],
    );
  },
};
