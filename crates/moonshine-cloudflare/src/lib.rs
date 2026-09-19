//! # moonshine-cloudflare
//!
//! Serve a [Moonshine](https://github.com/tschk/moonshine) (crepuscularity)
//! manifest from a Rust Cloudflare Worker built with
//! [`workers`](https://crates.io/crates/worker) — the Rust counterpart of the
//! `@tschk/moonshine-deploy-cloudflare` JS adapter.
//!
//! ```ignore
//! use moonshine_cloudflare::{MoonshineEdge, MoonshineManifest};
//! use std::sync::OnceLock;
//! use worker::*;
//!
//! static MANIFEST: OnceLock<MoonshineManifest> = OnceLock::new();
//!
//! #[event(fetch)]
//! async fn fetch(req: Request, env: Env, ctx: Context) -> Result<Response> {
//!     let manifest = MANIFEST.get_or_init(|| {
//!         MoonshineManifest::parse(include_str!("manifest.json")).expect("valid manifest")
//!     });
//!     let edge = MoonshineEdge::new(&env, "ASSETS", manifest)?;
//!     edge.fetch(req, &ctx).await
//! }
//! ```
//!
//! ## Scope
//!
//! This is the **static surface** of a Moonshine deployment: ASSETS-first
//! serving, manifest route matching (static/prerendered routes, including
//! `api`-mode routes with a `staticOutput`), per-route edge cache
//! (`cache.control` / `cache.revalidate`), and route headers.
//!
//! Routes that need a server renderer (`ssr`, `island`, `spa`) answer **501**
//! here — run those through the JS adapter instead, or pre-render them at
//! build time. The wire contract (manifest schema, path patterns, precedence,
//! percent-decoding rules, pathname normalization) mirrors
//! `@tschk/moonshine-framework` + `@tschk/moonshine-router`, so the same
//! `manifest.json` routes identically in both runtimes.

mod manifest;
mod router;
mod serve;

pub use manifest::{
    ManifestAsset, ManifestEntries, MoonshineManifest, RouteArtifact, RouteCache, MANIFEST_VERSION,
};
pub use router::{normalize_path, Pattern, RouteGraph, RouteMatch};
pub use serve::MoonshineEdge;
