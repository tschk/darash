//! Path-pattern matching, mirroring `@tschk/moonshine-router`'s
//! `compilePattern` / `matchRoutes` so a Rust edge picks the same routes as
//! the JS one for the same manifest.
//!
//! Segment kinds and precedence: static (3) > dynamic (2) > optional (1) >
//! rest (0); shorter score arrays compare as if padded with MISSING (4), so
//! `/a` beats `/a/*rest` for the path `/a`. Optional segments try consuming a
//! part first, then skipping. Rest segments decode per part and reject
//! `.`, `..`, NUL, and encoded separators before they can smuggle structure.

use crate::manifest::RouteArtifact;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Kind {
    Rest = 0,
    Optional = 1,
    Dynamic = 2,
    Static = 3,
}

pub const MISSING: u8 = 4;

#[derive(Debug, Clone)]
pub struct Segment {
    pub kind: Kind,
    pub name: String,
    pub value: String,
}

#[derive(Debug, Clone)]
pub struct Pattern {
    pub pattern: String,
    pub segments: Vec<Segment>,
}

#[derive(Debug, Clone)]
pub struct RouteMatch {
    pub params: Vec<(String, String)>,
    pub score: Vec<u8>,
}

fn split_path(path: &str) -> Vec<&str> {
    path.split('/')
        .filter(|part| !part.is_empty())
        .collect()
}

/// Strict percent-decoding of one path segment. Returns `None` for malformed
/// escapes and NUL.
pub fn decode_part(part: &str) -> Option<String> {
    let bytes = part.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'%' => {
                let hex = bytes.get(i + 1..i + 3)?;
                let hi = (hex[0] as char).to_digit(16)?;
                let lo = (hex[1] as char).to_digit(16)?;
                out.push((hi * 16 + lo) as u8);
                i += 3;
            }
            b => {
                out.push(b);
                i += 1;
            }
        }
    }
    if out.contains(&0) {
        return None;
    }
    String::from_utf8(out).ok()
}

/// Rest-segment decoding: `decode_part` plus the structure checks that keep
/// `/files/%2e%2e%2fsecret` from smuggling `..` or an encoded separator past
/// the router (same rules as `decodeRest` in moonshine-router).
fn decode_rest_part(part: &str) -> Option<String> {
    let decoded = decode_part(part)?;
    match decoded.as_str() {
        "." | ".." => None,
        _ if decoded.contains('/') => None,
        _ => Some(decoded),
    }
}

impl Pattern {
    pub fn compile(pattern: &str) -> Self {
        let mut segments = Vec::new();
        for part in split_path(pattern) {
            if let Some(rest) = part.strip_prefix('*') {
                let name = if rest.is_empty() { "*" } else { rest };
                segments.push(Segment { kind: Kind::Rest, name: name.to_string(), value: part.to_string() });
            } else if let Some(name) = part.strip_prefix(':') {
                let (kind, name) = match name.strip_suffix('?') {
                    Some(base) => (Kind::Optional, base),
                    None => (Kind::Dynamic, name),
                };
                segments.push(Segment { kind, name: name.to_string(), value: name.to_string() });
            } else {
                segments.push(Segment { kind: Kind::Static, name: part.to_string(), value: part.to_string() });
            }
        }
        Self { pattern: pattern.to_string(), segments }
    }

    pub fn matches(&self, pathname: &str) -> Option<RouteMatch> {
        // Segments accumulate innermost-first during recursion; one reversal
        // here restores segment order for the score and params.
        let mut matched = self.match_segments(&split_path(pathname), 0, 0)?;
        matched.score.reverse();
        matched.params.reverse();
        Some(matched)
    }

    fn match_segments(&self, parts: &[&str], si: usize, pi: usize) -> Option<RouteMatch> {
        if si == self.segments.len() {
            return (pi == parts.len()).then_some(RouteMatch { params: Vec::new(), score: Vec::new() });
        }

        let segment = &self.segments[si];
        let part = parts.get(pi).copied();

        match segment.kind {
            Kind::Rest => {
                let mut joined = Vec::with_capacity(parts.len() - pi);
                for part in &parts[pi..] {
                    joined.push(decode_rest_part(part)?);
                }
                let value = joined.join("/");
                let mut rest = self.match_segments(parts, si + 1, parts.len())?;
                rest.score.push(Kind::Rest as u8);
                rest.params.push((segment.name.clone(), value));
                Some(rest)
            }
            Kind::Static => {
                let decoded = decode_part(part?)?;
                if decoded != segment.value {
                    return None;
                }
                let mut rest = self.match_segments(parts, si + 1, pi + 1)?;
                rest.score.push(Kind::Static as u8);
                Some(rest)
            }
            Kind::Dynamic => {
                let decoded = decode_part(part?)?;
                let mut rest = self.match_segments(parts, si + 1, pi + 1)?;
                rest.score.push(Kind::Dynamic as u8);
                rest.params.push((segment.name.clone(), decoded));
                Some(rest)
            }
            Kind::Optional => {
                if let Some(part) = part {
                    if let Some(decoded) = decode_part(part) {
                        if let Some(mut rest) = self.match_segments(parts, si + 1, pi + 1) {
                            rest.score.push(Kind::Optional as u8);
                            rest.params.push((segment.name.clone(), decoded));
                            return Some(rest);
                        }
                    }
                }
                let mut rest = self.match_segments(parts, si + 1, pi)?;
                rest.score.push(0);
                Some(rest)
            }
        }
    }
}

/// A compiled manifest route set. Ambiguous routes (same normalized pattern
/// and precedence) are rejected like `createRouteGraph` does.
pub struct RouteGraph<'a> {
    routes: Vec<(&'a RouteArtifact, Pattern)>,
}

impl<'a> RouteGraph<'a> {
    pub fn new(routes: &'a [RouteArtifact]) -> Self {
        Self { routes: routes.iter().map(|route| (route, Pattern::compile(&route.path))).collect() }
    }

    pub fn is_empty(&self) -> bool {
        self.routes.is_empty()
    }

    /// Best match for a normalized pathname, or `None`. Higher scores win;
    /// shorter scores are padded with MISSING, mirroring `compareMatches`.
    pub fn match_routes(&self, pathname: &str) -> Option<(&'a RouteArtifact, RouteMatch)> {
        let parts = split_path(pathname);
        // Mirror of firstSegment: the raw (undecoded) first segment only
        // enables the prefilter; percent-encoding disables it.
        let first: Option<&str> = parts.first().copied().filter(|part| !part.contains('%'));
        let mut best: Option<(&'a RouteArtifact, RouteMatch)> = None;

        for (route, pattern) in &self.routes {
            if let Some(first) = first {
                if let Some(head) = pattern.segments.first() {
                    if head.kind == Kind::Static && head.value != first {
                        continue;
                    }
                }
            }
            let Some(candidate) = pattern.matches(pathname) else {
                continue;
            };
            let better = match &best {
                None => true,
                Some((_, best_match)) => score_better(&candidate.score, &best_match.score),
            };
            if better {
                best = Some((route, candidate));
            }
        }
        best
    }
}

/// `true` when `a` outranks `b` under the MISSING-padded comparison.
fn score_better(a: &[u8], b: &[u8]) -> bool {
    let max = a.len().max(b.len());
    for i in 0..max {
        let av = a.get(i).copied().unwrap_or(MISSING);
        let bv = b.get(i).copied().unwrap_or(MISSING);
        if av != bv {
            return av > bv;
        }
    }
    false
}

/// Moonshine's pathname normalization: collapse trailing slashes, keep "/".
pub fn normalize_path(path: &str) -> &str {
    let trimmed = path.trim_end_matches('/');
    if trimmed.is_empty() {
        "/"
    } else {
        trimmed
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn params_of(pattern: &str, path: &str) -> Option<Vec<(String, String)>> {
        Pattern::compile(pattern).matches(path).map(|m| m.params)
    }

    #[test]
    fn static_dynamic_optional_rest() {
        assert!(params_of("/x", "/x").is_some());
        assert!(params_of("/x", "/x/").is_some());
        assert!(params_of("/x", "/").is_none());
        assert_eq!(
            params_of("/u/:id", "/u/42"),
            Some(vec![("id".into(), "42".into())])
        );
        assert_eq!(
            params_of("/:lang?/about", "/en/about"),
            Some(vec![("lang".into(), "en".into())])
        );
        assert_eq!(params_of("/:lang?/about", "/about"), Some(vec![]));
        assert_eq!(
            params_of("/files/*path", "/files/a/b/c"),
            Some(vec![("path".into(), "a/b/c".into())])
        );
        // rest segments reject dot smuggling, per decodeRest
        assert_eq!(params_of("/files/*path", "/files/%2e%2e%2fsecret"), None);
    }

    #[test]
    fn static_outranks_rest() {
        let routes = ["/", "/about", "/about/*rest", "/*rest"];
        let artifacts: Vec<RouteArtifact> = routes
            .iter()
            .enumerate()
            .map(|(i, path)| RouteArtifact {
                id: i.to_string(),
                path: path.to_string(),
                file: String::new(),
                mode: "static".into(),
                runtime: None,
                static_output: None,
                cache: None,
                headers: None,
            })
            .collect();
        let graph = RouteGraph::new(&artifacts);
        let (route, _) = graph.match_routes("/about").unwrap();
        assert_eq!(route.path, "/about");
        let (route, _) = graph.match_routes("/about/x").unwrap();
        assert_eq!(route.path, "/about/*rest");
        let (route, _) = graph.match_routes("/other").unwrap();
        assert_eq!(route.path, "/*rest");
        assert!(graph.match_routes("/").is_some());
    }

    #[test]
    fn decoding_and_normalization() {
        assert_eq!(decode_part("a%20b"), Some("a b".into()));
        assert_eq!(decode_part("a%2"), None);
        assert_eq!(decode_part("a b"), Some("a b".into()));
        assert_eq!(decode_part("a\u{0}b"), None);
        assert_eq!(normalize_path("/x/"), "/x");
        assert_eq!(normalize_path("/"), "/");
        assert_eq!(normalize_path("//"), "/");
    }
}
