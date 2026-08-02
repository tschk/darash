# Darash Audit Report

## Scope

This report records the parallel audit performed against commit `1f37a83`
(`v0.1.1`) and the follow-up verification at current `main`:

- Current HEAD: `1d3709c` (`v0.2.0`), also `origin/main`.
- Working tree: clean before this report was added.
- Post-audit changes: embedded Websurfx, dependency updates, and the AGPL-3.0
  license change.
- The post-audit implementation changes were checked for build and test
  health, but they need a dedicated security review before release.

## Executive summary

The original client audit found one high-impact response compatibility bug,
several medium API and trust-boundary risks, and CI/test coverage gaps. The
Websurfx integration adds a more urgent supply-chain concern: `cargo audit`
now reports 9 vulnerabilities and 18 allowed warnings through `websurfx`
1.29.9's legacy dependency tree.

## Findings

### High

#### DARASH-001: Answer-bearing SearxNG responses fail decoding

`SearchResponse.answers` is `Vec<String>`, but current SearxNG JSON emits
answer objects. A response containing an answer therefore returns
`Error::Decode` instead of search results. The fallback at
`src/lib.rs:374-375` still routes the malformed SearxNG body through the
Websurfx parser and preserves the decode failure.

Locations: `src/lib.rs:423`, `src/lib.rs:372-375`.

Remediation: model the SearxNG answer object or use a deliberate untagged
compatibility type, then add an object-shaped fixture test.

#### DARASH-002: Vulnerable legacy dependency tree introduced by Websurfx

`cargo audit` at `1d3709c` reports 9 vulnerabilities, including critical
advisories for `hyper 0.12.36` and `failure 0.1.8`, plus unsound and
unmaintained legacy crates. The dependency path is primarily
`websurfx 1.29.9 -> fake-useragent -> reqwest 0.9.24`.

Location: `Cargo.toml:18`, `Cargo.lock`.

Remediation: upgrade or replace the embedded backend with a dependency tree
that has no unresolved advisories; do not ship the embedded backend while
critical/unsound advisories remain accepted.

### Medium

#### DARASH-003: Stock SearxNG result count is silently reported as zero

`number_of_results` defaults to zero, while current SearxNG JSON does not
emit that field. Callers can receive results with a misleading count.

Location: `src/lib.rs:419`.

Remediation: derive the count when absent, or distinguish backend count from
the locally returned result count.

#### DARASH-004: Nullable SearxNG URLs fail response decoding

SearxNG permits `url: null`, but `SearchResult.url` is a `String`. A result
with an explicit null URL makes the entire response fail decoding.

Location: `src/lib.rs:453`.

Remediation: use `Option<String>` or a documented null-to-empty conversion,
with a fixture test.

#### DARASH-005: Public API and README disagree on `snippet`

The README says `SearchResult` exposes `snippet`, while the public struct
exposes `content`; only `Citation` has `snippet`.

Locations: `README.md:65`, `src/lib.rs:449-466`.

Remediation: document `content`, or rename the public field in a deliberate
breaking API release.

#### DARASH-006: Mode truncation is undocumented

`search_request` truncates results and sources to 5, 10, or 20 items by mode,
while `number_of_results` remains the backend count.

Locations: `src/lib.rs:95-100`, `src/lib.rs:386-389`.

Remediation: document the limits and count semantics, or expose an explicit
unlimited/raw search path.

#### DARASH-007: Conditional SSRF through caller-controlled endpoints

`SearchConfig` accepts any HTTP or HTTPS host, including private and link-local
addresses. This is a risk when an embedding service forwards an untrusted
endpoint to `SearchClient::new` or the CLI's `--url` option.

Locations: `src/lib.rs:273-291`, `src/bin/darash.rs:84-86`.

Remediation: keep local mode explicit and add an optional caller policy or
allowlist when endpoints come from untrusted input.

#### DARASH-008: License changed from MPL-2.0 to AGPL-3.0

The `v0.2.0` package changes its declared license and README license without
a migration note. This can materially change downstream redistribution and
integration obligations.

Locations: `Cargo.toml:3-6`, `README.md:145`.

Remediation: confirm the intended licensing decision and call it out in the
release notes before publishing the new major-compatibility contract.

### Low / process

#### DARASH-009: Raw backend data is written to terminals and logs

Answers, titles, URLs, snippets, and HTTP error bodies are printed verbatim.
Control characters can affect terminals, and an error body up to 256 KiB can
be emitted to stderr.

Locations: `src/bin/darash.rs:123-134`, `src/lib.rs:394-411`,
`src/lib.rs:506-507`.

Remediation: provide structured output or escape terminal data; keep raw
error bodies out of the default `Display` implementation.

#### DARASH-010: CI omits documented package and security gates

The README lists `cargo package --locked`, but CI does not run it. CI also
does not run `cargo audit`, and its GitHub Actions use mutable tags.

Locations: `.github/workflows/ci.yml:13-20`, `README.md:131-141`.

Remediation: add package and advisory checks, and pin action revisions if
reproducible CI is required.

#### DARASH-011: Test coverage misses important failure paths

Tests do not cover answer objects, null URLs, oversized or invalid bodies,
redirects, invalid endpoints, most CLI failures, or embedded-server startup
failure.

Locations: `src/lib.rs:550-730`, `src/bin/darash.rs:145-179`.

Remediation: add the smallest fixtures that fail for each supported response
contract and trust boundary.

## Simplification candidates

These are optional maintenance cuts, not release blockers:

- Replace `futures-util` streaming with `reqwest::Response::chunk()` and drop
  the direct dependency and `stream` feature.
- Remove unused one-source and string helper methods only if preserving the
  published public API is not required.
- Inline the single-use CLI usage helper.

## Verification

At current HEAD, these commands passed:

```text
cargo fmt --all -- --check
cargo build --locked
cargo test --locked      # 12 tests passed
cargo clippy --all-targets --all-features -- -D warnings
cargo package --locked
```

`cargo audit` completed with the 9 vulnerabilities and 18 allowed warnings
described in DARASH-002. The working tree was clean before adding this report.
