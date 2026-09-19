//! The edge itself: ASSETS-first serving, manifest route matching, per-route
//! edge cache — the Rust counterpart of
//! `@tschk/moonshine-deploy-cloudflare`'s `cloudflareFetch` for prerendered
//! output. SSR route modules stay with the JS adapter.

use crate::manifest::MoonshineManifest;
use crate::router::{normalize_path, RouteGraph};
use worker::{Cache, CacheKey, Context, Env, Fetcher, Headers, Method, Request, Response};

/// Bind an edge to a worker's `ASSETS` binding and a parsed manifest.
///
/// Compiling the route graph is linear in the route count; build one edge per
/// isolate (e.g. in a `OnceLock`) rather than per request for large manifests.
pub struct MoonshineEdge<'a> {
    graph: RouteGraph<'a>,
    assets: Fetcher,
    _manifest: &'a MoonshineManifest,
}

impl<'a> MoonshineEdge<'a> {
    pub fn new(
        env: &Env,
        assets_binding: &str,
        manifest: &'a MoonshineManifest,
    ) -> worker::Result<Self> {
        let assets = env.assets(assets_binding)?;
        Ok(Self {
            graph: RouteGraph::new(&manifest.routes),
            assets,
            _manifest: manifest,
        })
    }

    pub fn manifest(&self) -> &MoonshineManifest {
        self._manifest
    }

    /// Serve a request. Order mirrors `cloudflareFetch`:
    ///
    /// 1. ASSETS-first for GET/HEAD (a miss is a 404, which falls through);
    /// 2. manifest route match — `static` routes are served from the assets
    ///    binding, honoring the route's edge-cache and header settings;
    /// 3. routes that need a server renderer answer 501 (use the JS adapter);
    /// 4. everything else answers plain-text 404.
    pub async fn fetch(&self, req: Request, ctx: &Context) -> worker::Result<Response> {
        let method = req.method();
        let parsed_url = req.url()?;
        let raw_path = parsed_url.path().to_string();
        let path = normalize_path(&raw_path);
        let url = parsed_url.to_string();
        let get_like = matches!(method, Method::Get | Method::Head);

        if get_like {
            if let Some(res) = self.serve_asset(&raw_path, &parsed_url, method.clone()).await? {
                if res.status_code() != 404 {
                    return Ok(res);
                }
            }
        }

        let Some((route, _params)) = self.graph.match_routes(path) else {
            return not_found();
        };

        if !route.is_static_surface() {
            return Response::error(
                format!(
                    "route '{}' mode '{}' needs a server renderer; use the moonshine JS adapter",
                    route.id, route.mode
                ),
                501,
            );
        }

        let cache = route.cache.as_ref();
        if get_like {
            if let Some(hit) = Cache::default().get(CacheKey::Url(url.clone()), false).await? {
                return Ok(hit);
            }
        }

        let asset_path = route
            .static_output
            .as_deref()
            .map(|file| file.strip_prefix('/').unwrap_or(file))
            .unwrap_or(path);
        let Some(mut res) = self.serve_asset(asset_path, &parsed_url, method).await? else {
            return not_found();
        };

        // Asset responses carry immutable headers, so overrides (route
        // headers, cache-control) mean rebuilding the header set once here.
        let cache_control = cache.filter(|_| get_like).map(|cache| {
            cache
                .control
                .clone()
                .unwrap_or_else(|| format!("s-maxage={}", cache.revalidate.unwrap_or(0)))
        });
        let has_route_headers =
            route.headers.as_ref().is_some_and(|headers| !headers.is_empty());
        if cache_control.is_some() || has_route_headers {
            let merged: Headers = res.headers().entries().collect();
            for (name, value) in route.headers.iter().flatten() {
                let _ = merged.set(name, value);
            }
            if let Some(control) = &cache_control {
                let _ = merged.set("cache-control", control);
            }
            res = res.with_headers(merged);
        }

        if let (true, Some(_)) = (get_like, cache) {
            if (200..400).contains(&res.status_code()) {
                // Reading the body disturbs the asset stream, so the client
                // response is rebuilt from the same bytes that feed the cache.
                let bytes = res.bytes().await?;
                let status = res.status_code();
                let headers: Headers = res.headers().entries().collect();
                let cache_bytes = bytes.clone();
                let cache_headers = headers.clone();
                ctx.wait_until(async move {
                    let for_cache = Response::from_bytes(cache_bytes)
                        .map(|res| res.with_status(status).with_headers(cache_headers));
                    if let Ok(for_cache) = for_cache {
                        let _ = Cache::default().put(CacheKey::Url(url), for_cache).await;
                    }
                });
                let response = Response::from_bytes(bytes)?
                    .with_status(status)
                    .with_headers(headers);
                return Ok(response);
            }
        }

        Ok(res)
    }

    /// Fetch a path from the assets binding, resolved against the request's
    /// own URL (workerd rejects relative URLs in `new Request`). The platform
    /// answers misses with 404 responses, which the caller treats as
    /// "no match".
    async fn serve_asset(
        &self,
        path: &str,
        base: &worker::Url,
        method: Method,
    ) -> worker::Result<Option<Response>> {
        let path = if path.starts_with('/') { path.to_string() } else { format!("/{path}") };
        let url = base
            .join(&path)
            .map_err(|e| worker::Error::RustError(format!("invalid asset path {path:?}: {e}")))?;
        let req = Request::new(url.as_str(), method)?;
        Ok(Some(self.assets.fetch_request(req).await?))
    }
}

fn not_found() -> worker::Result<Response> {
    Ok(Response::from_bytes(b"Not Found".to_vec())?.with_status(404))
}
