# Darash Audit Report

## Scope

This report separates the historical Websurfx release from the current
in-process backend.

- Historical v0.2.0: commit `1d3709c`, tag `v0.2.0`.
- Published v0.3.1: commit `93496f7`, tag `v0.3.1`.
- This documentation follow-up: commit `500a975`.
- Current v0.4.0: the post-v0.3.1 implementation under review in this report.

## Historical v0.2.0 findings

The following findings described the v0.2.0 Websurfx integration and are not
claims about the current dependency graph or backend.

### Resolved in v0.3.1

- DARASH-001: answer-bearing SearxNG responses could fail decoding. The current
  `answers` deserializer accepts text and common answer-object shapes.
- DARASH-002: Websurfx introduced a large legacy dependency tree and reported
  advisories. Websurfx and its dependency tree were removed in v0.3.1.
- DARASH-003: a missing SearxNG result count could be reported as zero. Remote
  responses now derive the count from decoded results when the field is absent;
  local responses report their deduplicated count before mode truncation.
- DARASH-004: nullable SearxNG URLs could fail decoding. The current response
  model maps a null URL to an empty string.
- DARASH-005: documentation called the result body `snippet` even though the
  public `SearchResult` field is `content`. Current documentation uses
  `content`; `Citation` continues to expose `snippet`.
- DARASH-006: mode truncation was undocumented. Current documentation records
  the 5, 10, and 20 result/source limits for speed, balanced, and quality.
- DARASH-008: v0.2.0 changed the package metadata to AGPL-3.0 with the
  Websurfx integration. v0.3.1 is ISC and no longer links Websurfx. The
  already-published v0.2.0 artifact remains unchanged.

### Still open or intentionally bounded

- DARASH-007: `SearchConfig` accepts any HTTP or HTTPS endpoint. Callers that
  pass an untrusted endpoint still need their own allowlist or network policy.
- DARASH-009: the CLI prints titles, URLs, content, and backend error bodies
  without terminal escaping. Structured output remains a future improvement.
- DARASH-010: CI runs formatting, build, test, and Clippy, but does not run
  package verification or an advisory scan. Workflow action revisions are also
  not pinned to immutable commits.
- DARASH-011: the current tests cover response compatibility, limits, URL
  normalization, provider fixture parsing, local-client selection, CLI parsing,
  and the direct-search future's `Send` bound. Live provider failures and
  network cancellation remain opt-in rather than deterministic CI tests.

## Websurfx adapter history

In v0.2.0, `SearchClient::local()` started Websurfx in the current process on
an ephemeral loopback listener. Darash queried its `/search` route with the
SearxNG-style query plus `json=true`, then mapped Websurfx's camelCase JSON
fields, including `relevanceScore`, into `SearchResponse`.

That adapter, route, protocol, temporary configuration, assets, and source are
not part of v0.3.1. The current local client calls Darash's provider adapters
directly and does not start an HTTP listener or a subprocess.

## Current v0.4.0 behavior

`SearchClient::local()` selects the requested providers and runs them
concurrently:

- `web`: DuckDuckGo HTML results.
- `academic`: OpenAlex works.
- `discussions`: Hacker News Algolia results.

Each provider requests up to 10 results in balanced and quality modes, or up to
5 in speed mode. Darash accepts successful provider results and records each
provider outcome in `SearchResponse.provider_status`; if every selected provider
fails it returns `Error::Local` containing the provider errors. Local responses
do not provide backend answers, corrections, or suggestions.

Results are normalized to title, URL, `content`, provider fields, category,
publication date, and score. Only HTTP and HTTPS result URLs are retained;
fragments are removed. Duplicate URLs are merged, provider names are merged,
and results are deterministically ranked with tokenized TF-IDF-style weighting
across title, content, and URL.

`SearchMode` controls provider selection and retrieval as well as truncating
`results` and `sources` to 5 (`speed`), 10 (`balanced`), or 20 (`quality`).
`SafeSearch` supports levels 0 through 4, with provider-specific DuckDuckGo
parameter mappings and optional level-3/4 allowlist/blocklist filtering.
`SearchFilters` reports the applied level and filtering state. Darash also has a
bounded in-memory TTL cache configurable through `SearchConfig`.

Remote clients retain the SearxNG-compatible `/search?format=json` request
path, a 15-second default timeout, a 256 KiB response cap, disabled redirects,
and endpoint validation that rejects non-HTTP(S) schemes and embedded
credentials.

The v0.4.0 verification set currently passes 33 library tests, 2 CLI tests,
5 integration tests, formatting, build, Clippy, and the no-Websurfx dependency
check.

## v0.4.0 release follow-up

The v0.4.0 implementation after v0.3.1 adds provider status metadata, native provider
parameter mapping, checked pagination, tokenized ranking, HTML/control-text
sanitization, configurable filtering, a bounded TTL cache, and dependency-free
Websurfx response/query adapters. These changes require the v0.4.0 tag and
package release. Re-run the dependency graph check, package verification,
advisory scan, and live provider smoke tests before publishing.
