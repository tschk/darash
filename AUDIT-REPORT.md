# Darash Audit Report

## Scope

This report separates the historical Websurfx release from the current
in-process backend.

- Historical v0.2.0: commit `1d3709c`, tag `v0.2.0`.
- Current v0.3.1: commit `93496f7`, tag `v0.3.1`, and `origin/main` at the time
  of this report.
- There is no v0.4.0 tag or implementation at the time of this report. This
  report assigns no behavior to that unreleased version.

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

## Current v0.3.1 behavior

`SearchClient::local()` selects the requested providers and runs them
concurrently:

- `web`: DuckDuckGo HTML results.
- `academic`: OpenAlex works.
- `discussions`: Hacker News Algolia results.

Each provider requests up to 10 results. Darash accepts successful provider
results, drops a provider error if another selected provider succeeds, and
returns `Error::Local` containing joined provider errors when all selected
providers fail. Successful local responses do not include per-provider error
metadata, backend answers, corrections, or suggestions.

Results are normalized to title, URL, `content`, provider fields, category,
publication date, and score. Only HTTP and HTTPS result URLs are retained;
fragments are removed. Duplicate URLs are merged, provider names are merged,
and results are deterministically ranked by query-term matches in title,
content, and URL, with title matches weighted higher.

`SearchMode` truncates `results` and `sources` to 5 (`speed`), 10
(`balanced`), or 20 (`quality`). `number_of_results` is not changed by this
truncation. `SafeSearch` maps to SearxNG values 0, 1, and 2; local mode passes
it to DuckDuckGo, while OpenAlex and Hacker News do not apply it. There is no
allowlist, blocklist, or persistent result cache in Darash. Remote cache
behavior belongs to the configured SearxNG service.

Remote clients retain the SearxNG-compatible `/search?format=json` request
path, a 15-second default timeout, a 256 KiB response cap, disabled redirects,
and endpoint validation that rejects non-HTTP(S) schemes and embedded
credentials.

The v0.3.1 verification set passed 15 library tests, 2 CLI tests, formatting,
build, Clippy, package verification, and the no-Websurfx dependency check.

## v0.4.0 follow-up

v0.4.0 is not released and has no implementation to audit. Before assigning
new behavior to that version, rerun the dependency graph check, package
verification, advisory scan, provider fixture tests, and live local-provider
smoke tests. Any future cache, provider-error metadata, terminal-safe output,
or endpoint policy must be documented only after it is implemented and tested.
