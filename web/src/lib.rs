//! darash-web — public edge for darash.tsc.hk, in Rust (workers-rs).
//!
//! Layering:
//!   1. `/api/*` streams straight to the Rust API worker (`darash-site`) over
//!      a service binding — GET/POST/websockets, unmodified. The API worker
//!      keeps its name (and therefore its Durable Object state); only the
//!      route lives here.
//!   2. Everything else is served by the moonshine-cloudflare edge: the
//!      platform's assets binding serves the built site (crepuscularity shell
//!      + Svelte islands) before this worker runs; misses fall through to the
//!      Moonshine manifest edge for manifest-routed prerendered pages.
//!
//! The TypeScript version of this worker used `@tschk/moonshine-deploy-
//! cloudflare`; this is its Rust replacement.

use std::sync::OnceLock;

use moonshine_cloudflare::{MoonshineEdge, MoonshineManifest};
use worker::*;

static MANIFEST_JSON: &str = include_str!("manifest.json");

fn manifest() -> &'static MoonshineManifest {
    static MANIFEST: OnceLock<MoonshineManifest> = OnceLock::new();
    MANIFEST.get_or_init(|| {
        MoonshineManifest::parse(MANIFEST_JSON).expect("embedded Moonshine manifest is valid")
    })
}

fn edge(env: &Env) -> worker::Result<MoonshineEdge<'_>> {
    MoonshineEdge::new(env, "ASSETS", manifest())
}

#[event(fetch)]
async fn fetch(req: Request, env: Env, ctx: Context) -> Result<Response> {
    let path = req.path();
    if path == "/api" || path.starts_with("/api/") {
        // Same-origin API: hand the request to the Rust worker untouched.
        return env.service("API")?.fetch_request(req).await;
    }
    edge(&env)?.fetch(req, &ctx).await
}
