//! Fetch and extraction: the research half of Darash.
//!
//! Search finds sources; research needs the sources read. This module fetches
//! a page with a bounded, always-reporting client and turns HTML into
//! agent-shaped output: markdown, page outlines, selector matches, multi-field
//! rows, and tables. Everything is local, deterministic, and key-free.

use std::collections::HashMap;

#[cfg(feature = "client")]
use std::io::Read;
use std::time::Duration;
#[cfg(feature = "client")]
use std::time::Instant;

use serde::{Deserialize, Serialize};

use crate::Error;

/// Hard cap for fetched response bodies.
pub const FETCH_MAX_BODY_BYTES: usize = 2 * 1024 * 1024;

const FETCH_TIMEOUT: Duration = Duration::from_secs(30);
const USER_AGENT: &str = concat!("darash-fetch/", env!("CARGO_PKG_VERSION"));
const MAX_SAMPLE_CHARS: usize = 80;

/// A completed fetch or local-file read. Never silent: every field is
/// populated even for error statuses, and the body is included.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct FetchReport {
    /// HTTP status, or `None` when the source was a local file.
    pub status: Option<u16>,
    pub ok: bool,
    /// Final URL after redirects, or the source path for local files.
    pub url: String,
    pub redirected: bool,
    pub ms: u64,
    #[serde(rename = "contentType")]
    pub content_type: Option<String>,
    pub bytes: usize,
    pub body: String,
}

impl FetchReport {
    /// Build a report for content read without HTTP, such as a local file.
    pub fn from_source(source: impl Into<String>, body: impl Into<String>) -> Self {
        let body = body.into();
        Self {
            status: None,
            ok: true,
            url: source.into(),
            redirected: false,
            ms: 0,
            content_type: None,
            bytes: body.len(),
            body,
        }
    }

    /// One-line stderr summary for extraction runs.
    pub fn summary(&self) -> String {
        let status = self
            .status
            .map(|status| status.to_string())
            .unwrap_or_else(|| "local".to_owned());
        format!(
            "status={} ok={} url={} ms={} bytes={}",
            status, self.ok, self.url, self.ms, self.bytes
        )
    }
}

/// Options for an HTTP fetch. `Default` is a plain `GET` with no body.
///
/// This mirrors the useful subset of curl's request flags while staying a
/// small, explicit struct. The library never caches; caching is a CLI concern
/// handled by [`crate::disk_cache`].
#[derive(Clone, Debug, Default)]
pub struct FetchOptions {
    /// HTTP method; `None` means `GET`.
    pub method: Option<String>,
    pub headers: Vec<(String, String)>,
    /// Request body bytes; `None` means no body.
    pub body: Option<Vec<u8>>,
    /// HTTP basic auth `(user, password)`.
    pub basic_auth: Option<(String, String)>,
    /// Accept invalid TLS certificates (curl's `-k`).
    pub insecure: bool,
    /// Request timeout; `None` uses the 30-second default.
    pub timeout: Option<Duration>,
    /// Body cap; `None` uses [`FETCH_MAX_BODY_BYTES`].
    pub max_bytes: Option<usize>,
}

/// Fetch a page over HTTP or HTTPS with default options and report everything.
///
/// This is the compatibility wrapper around [`fetch_with`]; new callers that
/// need a method, body, auth, timeout, or body cap should use `fetch_with`.
#[cfg(feature = "client")]
pub async fn fetch(
    url: impl AsRef<str>,
    headers: &[(String, String)],
) -> Result<FetchReport, Error> {
    fetch_with(
        url,
        &FetchOptions {
            headers: headers.to_vec(),
            ..FetchOptions::default()
        },
    )
    .await
}

/// Fetch a page over HTTP or HTTPS with explicit options and report everything.
///
/// Non-2xx statuses are still a valid [`FetchReport`] with `ok == false`;
/// only transport failures and the body cap return [`Error`].
#[cfg(feature = "client")]
pub async fn fetch_with(
    url: impl AsRef<str>,
    options: &FetchOptions,
) -> Result<FetchReport, Error> {
    let parsed = ::url::Url::parse(url.as_ref()).map_err(|error| {
        Error::InvalidEndpoint(format!("{} is not a valid URL: {error}", url.as_ref()))
    })?;
    if !matches!(parsed.scheme(), "http" | "https") {
        return Err(Error::InvalidEndpoint(format!(
            "fetch supports only http and https, got {}",
            parsed.scheme()
        )));
    }
    let original = parsed.clone();

    let method = match options.method.as_deref() {
        Some(method) => reqwest::Method::from_bytes(method.as_bytes())
            .map_err(|_| Error::InvalidMethod(method.to_owned()))?,
        None => reqwest::Method::GET,
    };
    let max_bytes = options.max_bytes.unwrap_or(FETCH_MAX_BODY_BYTES);

    let mut headers_map = reqwest::header::HeaderMap::new();
    for (name, value) in &options.headers {
        let name = reqwest::header::HeaderName::from_bytes(name.as_bytes()).map_err(|error| {
            Error::InvalidEndpoint(format!("invalid header name {name:?}: {error}"))
        })?;
        let value = reqwest::header::HeaderValue::from_str(value).map_err(|error| {
            Error::InvalidEndpoint(format!("invalid header value {value:?}: {error}"))
        })?;
        headers_map.insert(name, value);
    }
    let client = reqwest::Client::builder()
        .user_agent(USER_AGENT)
        .timeout(options.timeout.unwrap_or(FETCH_TIMEOUT))
        .danger_accept_invalid_certs(options.insecure)
        .build()
        .map_err(Error::ClientBuild)?;

    let started = Instant::now();
    let mut request = client.request(method, parsed).headers(headers_map);
    if let Some((user, password)) = &options.basic_auth {
        request = request.basic_auth(user, Some(password));
    }
    if let Some(body) = &options.body {
        request = request.body(body.clone());
    }
    let mut response = request.send().await.map_err(Error::Fetch)?;

    let status = response.status();
    let final_url = response.url().to_string();
    let redirected = response.url() != &original;
    let content_type = response
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .map(|value| value.to_owned());

    let mut bytes = Vec::new();
    while let Some(chunk) = response.chunk().await.map_err(Error::Fetch)? {
        if bytes.len() + chunk.len() > max_bytes {
            return Err(Error::FetchBodyTooLarge { limit: max_bytes });
        }
        bytes.extend_from_slice(&chunk);
    }
    let body = String::from_utf8_lossy(&bytes).into_owned();

    Ok(FetchReport {
        status: Some(status.as_u16()),
        ok: status.is_success(),
        url: final_url,
        redirected,
        ms: started.elapsed().as_millis() as u64,
        content_type,
        bytes: body.len(),
        body,
    })
}

/// Render mode shared by the HTML renderer.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RenderMode {
    /// GitHub-flavored markdown.
    Markdown,
    /// Plain text without markup.
    Plain,
}

/// Convert an HTML document to markdown.
pub fn to_markdown(html: &str) -> String {
    render_html(html, RenderMode::Markdown)
}

/// Convert an HTML document to plain text.
pub fn to_text(html: &str) -> String {
    render_html(html, RenderMode::Plain)
}

fn render_html(html: &str, mode: RenderMode) -> String {
    let document = scraper::Html::parse_document(html);
    let mut out = String::new();
    render_children(document.root_element(), mode, &mut out);
    finish_document(&mut out);
    out
}

const SKIPPED_ELEMENTS: [&str; 8] = [
    "script", "style", "noscript", "template", "svg", "head", "iframe", "canvas",
];

fn render_children(element: scraper::ElementRef, mode: RenderMode, out: &mut String) {
    for node in element.children() {
        if let Some(text) = node.value().as_text() {
            push_paragraph(out, &collapse_ws(&text.text));
            continue;
        }
        let Some(child) = scraper::ElementRef::wrap(node) else {
            continue;
        };
        let name = child.value().name();
        if SKIPPED_ELEMENTS.contains(&name) {
            continue;
        }
        render_element(name, child, mode, out);
    }
}

/// Containers whose children render recursively as blocks; anything else
/// falls through to the inline renderer as a paragraph.
const BLOCK_CONTAINERS: [&str; 18] = [
    "body",
    "td",
    "div",
    "section",
    "article",
    "main",
    "header",
    "footer",
    "aside",
    "nav",
    "figure",
    "figcaption",
    "details",
    "summary",
    "address",
    "fieldset",
    "form",
    "center",
];

fn render_element(name: &str, element: scraper::ElementRef, mode: RenderMode, out: &mut String) {
    match name {
        "h1" | "h2" | "h3" | "h4" | "h5" | "h6" => {
            let level = name.as_bytes()[1] - b'0';
            let text = render_inline(element, mode);
            if text.trim().is_empty() {
                return;
            }
            if mode == RenderMode::Markdown {
                push_paragraph(
                    out,
                    &format!("{} {}", "#".repeat(level as usize), text.trim()),
                );
            } else {
                push_paragraph(out, text.trim());
            }
        }
        "p" => {
            let text = render_inline(element, mode);
            if !text.trim().is_empty() {
                push_paragraph(out, text.trim());
            }
        }
        "ul" | "ol" => render_list(element, mode, out),
        "pre" => {
            let code = element.text().collect::<String>();
            if code.trim().is_empty() {
                return;
            }
            if mode == RenderMode::Markdown {
                push_paragraph(out, &format!("```\n{}\n```", code.trim_end()));
            } else {
                push_paragraph(out, code.trim_end());
            }
        }
        "blockquote" => {
            let mut inner = String::new();
            render_children(element, mode, &mut inner);
            if inner.trim().is_empty() {
                return;
            }
            let quoted = inner
                .trim()
                .lines()
                .map(|line| format!("> {}", line.trim()))
                .collect::<Vec<_>>()
                .join("\n");
            push_paragraph(out, &quoted);
        }
        "table" => {
            if let Some(table) = table_from_element(element) {
                push_paragraph(out, &table.to_markdown());
            }
        }
        "hr" => {
            if mode == RenderMode::Markdown {
                push_paragraph(out, "---");
            }
        }
        "dl" => {
            for item in element.child_elements() {
                match item.value().name() {
                    "dt" => push_paragraph(out, render_inline(item, mode).trim()),
                    "dd" => push_paragraph(out, &format!(": {}", render_inline(item, mode).trim())),
                    _ => {}
                }
            }
        }
        name if BLOCK_CONTAINERS.contains(&name) => render_children(element, mode, out),
        _ => {
            let mut buf = String::new();
            render_inline_node(name, element, mode, &mut buf);
            if !buf.trim().is_empty() {
                push_paragraph(out, buf.trim());
            }
        }
    }
}

fn render_list(list: scraper::ElementRef, mode: RenderMode, out: &mut String) {
    let ordered = list.value().name() == "ol";
    let mut lines: Vec<String> = Vec::new();
    let mut index = 0;
    for item in list.child_elements() {
        if item.value().name() != "li" {
            continue;
        }
        index += 1;
        let marker = if ordered {
            format!("{index}.")
        } else {
            "-".to_owned()
        };
        let mut content = String::new();
        render_children(item, mode, &mut content);
        let content = content.trim();
        if content.is_empty() {
            continue;
        }
        let block_start = content.find("\n\n").unwrap_or(content.len());
        let head = content[..block_start].replace('\n', " ");
        let mut rendered = format!("{marker} {}", head.trim());
        if !content[block_start..].trim().is_empty() {
            for line in content[block_start..].trim().lines() {
                rendered.push_str("\n  ");
                rendered.push_str(line);
            }
        }
        lines.push(rendered);
    }
    if !lines.is_empty() {
        push_paragraph(out, &lines.join("\n"));
    }
}

/// Render an element's inline content: links, emphasis, code, images.
fn render_inline(element: scraper::ElementRef, mode: RenderMode) -> String {
    let mut out = String::new();
    render_inline_children(element, mode, &mut out);
    out
}

fn render_inline_children(element: scraper::ElementRef, mode: RenderMode, out: &mut String) {
    for node in element.children() {
        if let Some(text) = node.value().as_text() {
            out.push_str(&collapse_ws(&text.text));
            continue;
        }
        if node.value().as_comment().is_some() {
            continue;
        }
        let Some(child) = scraper::ElementRef::wrap(node) else {
            continue;
        };
        let name = child.value().name();
        if SKIPPED_ELEMENTS.contains(&name) {
            continue;
        }
        render_inline_node(name, child, mode, out);
    }
}

/// Render one inline element with its own markup, shared by the inline and
/// block-level paths.
fn render_inline_node(name: &str, child: scraper::ElementRef, mode: RenderMode, out: &mut String) {
    match name {
        "br" => out.push('\n'),
        "strong" | "b" => {
            let inner = render_inline(child, mode);
            if mode == RenderMode::Markdown && !inner.trim().is_empty() {
                out.push_str(&format!("**{}**", inner.trim()));
            } else {
                out.push_str(&inner);
            }
        }
        "em" | "i" => {
            let inner = render_inline(child, mode);
            if mode == RenderMode::Markdown && !inner.trim().is_empty() {
                out.push_str(&format!("*{}*", inner.trim()));
            } else {
                out.push_str(&inner);
            }
        }
        "code" | "kbd" | "samp" => {
            let inner = child.text().collect::<String>();
            if mode == RenderMode::Markdown {
                out.push_str(&format!("`{}`", inner.trim()));
            } else {
                out.push_str(inner.trim());
            }
        }
        "img" => {
            if mode == RenderMode::Markdown {
                let alt = child.value().attr("alt").unwrap_or_default();
                if let Some(src) = child.value().attr("src") {
                    out.push_str(&format!("![{alt}]({src})"));
                }
            }
        }
        "a" => {
            let inner = render_inline(child, mode);
            let href = child.value().attr("href");
            if mode == RenderMode::Markdown {
                if let Some(href) = href {
                    let label = if inner.trim().is_empty() {
                        href
                    } else {
                        inner.trim()
                    };
                    out.push_str(&format!("[{label}]({href})"));
                } else {
                    out.push_str(&inner);
                }
            } else {
                out.push_str(&inner);
            }
        }
        "p" | "div" | "li" | "section" | "article" => out.push(' '),
        _ => out.push_str(&render_inline(child, mode)),
    }
}

fn push_paragraph(out: &mut String, paragraph: &str) {
    if paragraph.trim().is_empty() {
        return;
    }
    out.push_str(paragraph.trim());
    out.push_str("\n\n");
}

fn finish_document(out: &mut String) {
    *out = out.trim().to_owned();
    while out.contains("\n\n\n") {
        *out = out.replace("\n\n\n", "\n\n");
    }
    if !out.is_empty() {
        out.push('\n');
    }
}

/// Collapse whitespace runs to single spaces while keeping boundary spaces so
/// adjacent inline nodes stay separated; callers trim where needed.
fn collapse_ws(text: &str) -> String {
    let mut out = String::new();
    let mut pending_space = false;
    for ch in text.chars() {
        if ch.is_whitespace() {
            pending_space = true;
        } else {
            if pending_space {
                out.push(' ');
            }
            pending_space = false;
            out.push(ch);
        }
    }
    if pending_space {
        out.push(' ');
    }
    out
}

/// One repeating structure found by [`outline`].
#[derive(Clone, Debug, Serialize)]
pub struct OutlineEntry {
    /// Element tag, or `tag.class` for repeated classes.
    pub selector: String,
    pub count: usize,
    /// Text of the first instance, trimmed to a short sample.
    pub sample: Option<String>,
}

const OUTLINE_TAGS: [&str; 22] = [
    "h1",
    "h2",
    "h3",
    "h4",
    "h5",
    "h6",
    "p",
    "a",
    "ul",
    "ol",
    "li",
    "table",
    "tr",
    "article",
    "section",
    "nav",
    "aside",
    "form",
    "button",
    "img",
    "pre",
    "blockquote",
];

/// Discover a page's repeating structures without dumping raw HTML.
pub fn outline(html: &str) -> Vec<OutlineEntry> {
    let document = scraper::Html::parse_document(html);
    let mut counts: HashMap<String, (usize, Option<String>)> = HashMap::new();

    let every = scraper::Selector::parse("*").expect("universal selector parses");
    for element in document.select(&every) {
        let name = element.value().name();
        if SKIPPED_ELEMENTS.contains(&name) {
            continue;
        }
        let mut note = |selector: String| {
            let entry = counts.entry(selector).or_insert((0, None));
            entry.0 += 1;
            if entry.1.is_none() {
                let sample = collapse_ws(element.text().collect::<String>().trim());
                entry.1 = Some(sample.chars().take(MAX_SAMPLE_CHARS).collect());
            }
        };
        if OUTLINE_TAGS.contains(&name) {
            note(name.to_owned());
        }
        if let Some(classes) = element.value().attr("class") {
            if let Some(first) = classes.split_whitespace().next() {
                note(format!("{name}.{first}"));
            }
        }
    }

    let mut entries: Vec<OutlineEntry> = counts
        .into_iter()
        .filter(|(selector, (count, _))| {
            selector.contains('.') || *count > 1 || OUTLINE_TAGS.contains(&selector.as_str())
        })
        .map(|(selector, (count, sample))| OutlineEntry {
            selector,
            count,
            sample,
        })
        .collect();
    entries.sort_by(|a, b| b.count.cmp(&a.count).then(a.selector.cmp(&b.selector)));
    entries.truncate(25);
    entries
}

/// One extracted table.
#[derive(Clone, Debug, Serialize)]
pub struct Table {
    pub headers: Vec<String>,
    pub rows: Vec<Vec<String>>,
}

impl Table {
    fn to_markdown(&self) -> String {
        let width = self.headers.len();
        if width == 0 {
            return String::new();
        }
        let cell = |value: &str| collapse_ws(value).replace('|', "\\|");
        let mut out = String::new();
        out.push_str("| ");
        out.push_str(
            &self
                .headers
                .iter()
                .map(|header| cell(header))
                .collect::<Vec<_>>()
                .join(" | "),
        );
        out.push_str(" |\n| ");
        out.push_str(&vec!["---"; width].join(" | "));
        out.push_str(" |\n");
        for row in &self.rows {
            let mut cells: Vec<String> = row.iter().map(|value| cell(value)).collect();
            cells.resize(width, String::new());
            out.push_str("| ");
            out.push_str(&cells.join(" | "));
            out.push_str(" |\n");
        }
        out.trim_end().to_owned()
    }
}

/// Extract every `<table>` as keyed rows.
pub fn tables(html: &str) -> Vec<Table> {
    let document = scraper::Html::parse_document(html);
    let Ok(selector) = scraper::Selector::parse("table") else {
        return Vec::new();
    };
    document
        .select(&selector)
        .filter_map(table_from_element)
        .collect()
}

fn table_from_element(table: scraper::ElementRef) -> Option<Table> {
    let row_selector = scraper::Selector::parse("tr").ok()?;
    let mut headers: Vec<String> = Vec::new();
    let mut rows: Vec<Vec<String>> = Vec::new();

    for row in table.select(&row_selector) {
        let cells: Vec<String> = row
            .child_elements()
            .filter(|cell| matches!(cell.value().name(), "td" | "th"))
            .map(|cell| collapse_ws(cell.text().collect::<String>().trim()))
            .collect();
        if cells.is_empty() {
            continue;
        }
        if headers.is_empty() {
            headers = cells;
        } else {
            rows.push(cells);
        }
    }

    if headers.is_empty() {
        return None;
    }
    Some(Table { headers, rows })
}

/// One named field extracted from a row container.
#[derive(Clone, Debug, Serialize)]
pub struct Field {
    pub name: String,
    pub value: String,
}

/// Parse a `--row` specification like `title=h2, url=a@href`.
///
/// Field selectors are relative to each row container. A selector may end with
/// `@attr` to take an attribute value instead of text. Selectors cannot
/// contain commas.
pub fn parse_row_spec(spec: &str) -> Result<Vec<(String, String)>, Error> {
    let mut fields = Vec::new();
    for part in spec.split(',') {
        let part = part.trim();
        if part.is_empty() {
            continue;
        }
        let Some((name, selector)) = part.split_once('=') else {
            return Err(Error::InvalidSelector(format!(
                "row field {part:?} is missing '=': use name=selector or name=selector@attr"
            )));
        };
        let name = name.trim();
        let selector = selector.trim();
        if name.is_empty() || selector.is_empty() {
            return Err(Error::InvalidSelector(format!(
                "row field {part:?} needs both a name and a selector"
            )));
        }
        fields.push((name.to_owned(), selector.to_owned()));
    }
    if fields.is_empty() {
        return Err(Error::InvalidSelector(
            "row specification is empty: use name=selector, ...".to_owned(),
        ));
    }
    Ok(fields)
}

/// Extract multi-field rows from repeating containers.
///
/// `container` selects the repeating element (for example `.item`); each field
/// from [`parse_row_spec`] is resolved inside it.
pub fn rows(html: &str, container: &str, spec: &str) -> Result<Vec<Vec<Field>>, Error> {
    let container_selector = scraper::Selector::parse(container)
        .map_err(|error| Error::InvalidSelector(format!("{container:?}: {error}")))?;
    let fields = parse_row_spec(spec)?;
    let compiled = fields
        .iter()
        .map(|(name, selector)| {
            let (selector, attr) = split_attr(selector);
            let selector = scraper::Selector::parse(&selector)
                .map_err(|error| Error::InvalidSelector(format!("{selector:?}: {error}")))?;
            Ok((name.clone(), selector, attr))
        })
        .collect::<Result<Vec<_>, Error>>()?;

    let document = scraper::Html::parse_document(html);
    let mut extracted = Vec::new();
    for element in document.select(&container_selector) {
        let mut row = Vec::with_capacity(compiled.len());
        for (name, selector, attr) in &compiled {
            let value = element
                .select(selector)
                .next()
                .map(|found| match attr {
                    Some(attr) => found.value().attr(attr).unwrap_or_default().to_owned(),
                    None => collapse_ws(found.text().collect::<String>().trim()),
                })
                .unwrap_or_default();
            row.push(Field {
                name: name.clone(),
                value,
            });
        }
        extracted.push(row);
    }
    Ok(extracted)
}

fn split_attr(selector: &str) -> (String, Option<String>) {
    // `a@href` takes the href attribute of the first `a` match. An `@` inside
    // brackets is part of the selector and is left alone.
    if let Some(open) = selector.find('[') {
        if selector[open..].contains('@') {
            return (selector.to_owned(), None);
        }
    }
    match selector.rsplit_once('@') {
        Some((selector, attr))
            if !attr.is_empty()
                && attr
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_') =>
        {
            (selector.trim().to_owned(), Some(attr.to_owned()))
        }
        _ => (selector.to_owned(), None),
    }
}

/// Extract the text (or attribute) values of every element matching a selector.
pub fn select_texts(html: &str, selector: &str) -> Result<Vec<String>, Error> {
    let compiled = scraper::Selector::parse(selector)
        .map_err(|error| Error::InvalidSelector(format!("{selector:?}: {error}")))?;
    let document = scraper::Html::parse_document(html);
    Ok(document
        .select(&compiled)
        .map(|element| collapse_ws(element.text().collect::<String>().trim()))
        .collect())
}

/// Estimate model tokens for text at roughly four characters per token.
pub fn estimate_tokens(text: &str) -> usize {
    text.chars().count().div_ceil(4)
}

/// The outcome of applying a token budget to a set of items.
#[derive(Clone, Debug)]
pub struct Budgeted {
    pub items: Vec<String>,
    /// Items that did not fit, when a budget was set.
    pub omitted: usize,
}

/// Apply a token budget, cutting at item boundaries.
///
/// Always keeps at least one item; an oversized first item is returned whole.
pub fn apply_budget(items: Vec<String>, budget: Option<usize>) -> Budgeted {
    let Some(budget) = budget else {
        return Budgeted { items, omitted: 0 };
    };
    let total = items.len();
    let mut kept = Vec::new();
    let mut used = 0usize;
    for (index, item) in items.iter().enumerate() {
        let cost = estimate_tokens(item);
        if index > 0 && used + cost > budget {
            let omitted = total - kept.len();
            return Budgeted {
                items: kept,
                omitted,
            };
        }
        used += cost;
        kept.push(item.clone());
    }
    Budgeted {
        items: kept,
        omitted: 0,
    }
}

/// Read a local file as a fetch report. URLs stay on the network path.
#[cfg(feature = "client")]
pub fn read_source(path: impl AsRef<std::path::Path>) -> Result<FetchReport, Error> {
    let path = path.as_ref();
    let mut file = std::fs::File::open(path)
        .map_err(|error| Error::SourceRead(format!("cannot read {}: {error}", path.display())))?;
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes)
        .map_err(|error| Error::SourceRead(format!("cannot read {}: {error}", path.display())))?;
    if bytes.len() > FETCH_MAX_BODY_BYTES {
        return Err(Error::FetchBodyTooLarge {
            limit: FETCH_MAX_BODY_BYTES,
        });
    }
    let body = String::from_utf8_lossy(&bytes).into_owned();
    Ok(FetchReport::from_source(path.display().to_string(), body))
}

/// One match found by [`locate`].
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct LocateHit {
    /// CSS selector path from below `<body>`/`<html>` down to the holder.
    pub selector: String,
    /// Short snippet of the matching attribute or text.
    pub snippet: String,
    /// `"attr"` when an attribute matched, `"text"` when element text matched.
    pub kind: String,
    /// Attribute name when `kind == "attr"`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub attribute: Option<String>,
}

/// Find the deepest elements whose text or attributes contain `needle`.
///
/// Matching is case-insensitive. An ancestor that only contains the needle
/// through a descendant's text is skipped, so each hit is the element that
/// actually holds the text. Attribute hits are reported for the element that
/// owns the attribute. Selector paths use `tag#id` when an id exists, else
/// `tag.class1.class2`, with CSS identifiers escaped so names like
/// `sm:w-1/2` round-trip.
pub fn locate(html: &str, needle: &str) -> Vec<LocateHit> {
    let document = scraper::Html::parse_document(html);
    let needle = needle.to_lowercase();
    let mut hits = Vec::new();
    let mut path = Vec::new();
    visit_locate(document.root_element(), &mut path, &needle, &mut hits);
    hits
}

struct PathPart {
    tag: String,
    id: Option<String>,
    classes: Vec<String>,
}

fn visit_locate(
    element: scraper::ElementRef,
    path: &mut Vec<PathPart>,
    needle: &str,
    hits: &mut Vec<LocateHit>,
) {
    let value = element.value();
    let tag = value.name().to_owned();
    let id = value.id().map(str::to_owned);
    let classes = value.classes().map(str::to_owned).collect::<Vec<_>>();

    let mut attr_hit = None;
    for (name, attr_value) in value.attrs() {
        if attr_value.to_lowercase().contains(needle) {
            attr_hit = Some((name.to_owned(), attr_value.to_owned()));
            break;
        }
    }
    let child_hit = element
        .child_elements()
        .any(|child| text_contains(child.text(), needle));
    let text = element.text().collect::<String>();
    let text_hit = !child_hit && text_contains(element.text(), needle);

    path.push(PathPart { tag, id, classes });
    if let Some((name, attr_value)) = attr_hit {
        hits.push(LocateHit {
            selector: selector_path(path),
            snippet: snippet_cap(&format!("{name}=\"{attr_value}\"")),
            kind: "attr".to_owned(),
            attribute: Some(name),
        });
    } else if text_hit {
        hits.push(LocateHit {
            selector: selector_path(path),
            snippet: snippet_cap(&collapse_ws(text.trim())),
            kind: "text".to_owned(),
            attribute: None,
        });
    }

    for child in element.child_elements() {
        visit_locate(child, path, needle, hits);
    }
    path.pop();
}

fn text_contains<'a>(text_iter: impl Iterator<Item = &'a str>, needle: &str) -> bool {
    let mut s = text_iter.collect::<String>();
    if s.is_ascii() {
        s.make_ascii_lowercase();
        s.contains(needle)
    } else {
        s.to_lowercase().contains(needle)
    }
}

fn selector_path(path: &[PathPart]) -> String {
    path.iter()
        .filter(|part| part.tag != "body" && part.tag != "html")
        .map(selector_part)
        .collect::<Vec<_>>()
        .join(" > ")
}

fn selector_part(part: &PathPart) -> String {
    if let Some(id) = &part.id {
        return format!("{}#{}", part.tag, escape_css_ident(id));
    }
    if part.classes.is_empty() {
        return part.tag.clone();
    }
    let classes = part
        .classes
        .iter()
        .map(|class| format!(".{}", escape_css_ident(class)))
        .collect::<String>();
    format!("{}{}", part.tag, classes)
}

/// Escape a CSS identifier so names like `sm:w-1/2` round-trip through a
/// selector. Non-ASCII characters are kept; ASCII punctuation is backslash
/// escaped, and a leading digit uses a hex escape.
pub fn escape_css_ident(ident: &str) -> String {
    let mut out = String::new();
    for (index, ch) in ident.chars().enumerate() {
        if ch.is_ascii_alphanumeric() || ch == '-' || ch == '_' {
            if index == 0 && ch.is_ascii_digit() {
                out.push_str(&format!("\\{:x} ", ch as u32));
            } else {
                out.push(ch);
            }
        } else if ch.is_ascii() {
            out.push('\\');
            out.push(ch);
        } else {
            out.push(ch);
        }
    }
    out
}

fn snippet_cap(text: &str) -> String {
    text.chars().take(MAX_SAMPLE_CHARS).collect()
}

/// Pagination state for structured extraction output.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PageState {
    /// More results follow at `next_offset`.
    More,
    /// The returned page reaches the end of the result set.
    Complete,
    /// `offset` is beyond the end; no results were returned.
    PastEnd,
}

/// Metadata describing a paginated slice of structured results.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct PageMeta {
    pub state: PageState,
    pub total: usize,
    pub offset: usize,
    pub returned: usize,
    pub next_offset: Option<usize>,
}

/// Compute the pagination slice for `total` results at `offset` with `limit`.
///
/// `offset` past a non-empty result set is `PastEnd`; an empty result set is
/// `Complete`. `next_offset` is `Some` only when more results follow.
pub fn paginate(total: usize, offset: usize, limit: usize) -> PageMeta {
    if offset >= total && offset > 0 {
        return PageMeta {
            state: PageState::PastEnd,
            total,
            offset,
            returned: 0,
            next_offset: None,
        };
    }
    let limit = limit.max(1);
    let returned = total.saturating_sub(offset).min(limit);
    let next_offset = if offset + returned < total {
        Some(offset + returned)
    } else {
        None
    };
    PageMeta {
        state: if next_offset.is_none() {
            PageState::Complete
        } else {
            PageState::More
        },
        total,
        offset,
        returned,
        next_offset,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const FIXTURE: &str = r#"
        <html>
        <head><title>Shop</title><style>.a{color:red}</style></head>
        <body>
            <h1>Shop</h1>
            <p>Welcome to the <strong>shop</strong>.</p>
            <ul>
                <li>One</li>
                <li>Two</li>
            </ul>
            <blockquote>Quoted text</blockquote>
            <pre><code>let x = 1;</code></pre>
            <div class="item">
                <h2>Alpha</h2>
                <a href="/alpha">details</a>
                <p>First product</p>
            </div>
            <div class="item">
                <h2>Beta</h2>
                <a href="/beta">details</a>
                <p>Second product</p>
            </div>
            <table>
                <tr><th>Name</th><th>Price</th></tr>
                <tr><td>Alpha</td><td>$1</td></tr>
                <tr><td>Beta</td><td>$2</td></tr>
            </table>
            <script>alert("nope")</script>
        </body>
        </html>
    "#;

    #[test]
    fn markdown_renders_headings_links_and_lists() {
        let markdown = to_markdown(FIXTURE);
        assert!(markdown.starts_with("# Shop\n\n"));
        assert!(markdown.contains("Welcome to the **shop**."));
        assert!(markdown.contains("- One\n- Two"));
        assert!(markdown.contains("> Quoted text"));
        assert!(markdown.contains("```\nlet x = 1;\n```"));
        assert!(markdown.contains("[details](/alpha)"));
        assert!(!markdown.contains("alert"));
        assert!(!markdown.contains("color:red"));
    }

    #[test]
    fn plain_mode_strips_markup() {
        let text = to_text("<h1>Hi</h1><p>See <a href=\"/x\">this</a> and <b>bold</b>.</p>");
        assert_eq!(text, "Hi\n\nSee this and bold.\n");
    }

    #[test]
    fn markdown_links_use_href_labels_when_empty() {
        let markdown = to_markdown("<p><a href=\"/go\"></a></p>");
        assert!(markdown.contains("[/go](/go)"));
    }

    #[test]
    fn outline_reports_repeated_structures() {
        let entries = outline(FIXTURE);
        let item = entries
            .iter()
            .find(|entry| entry.selector == "div.item")
            .expect("item class is outlined");
        assert_eq!(item.count, 2);
        assert!(item.sample.as_deref().unwrap_or_default().contains("Alpha"));

        let h1 = entries
            .iter()
            .find(|entry| entry.selector == "h1")
            .expect("headings are outlined");
        assert_eq!(h1.count, 1);
    }

    #[test]
    fn tables_extract_keyed_rows() {
        let tables = tables(FIXTURE);
        assert_eq!(tables.len(), 1);
        assert_eq!(tables[0].headers, ["Name", "Price"]);
        assert_eq!(tables[0].rows, [["Alpha", "$1"], ["Beta", "$2"]]);

        let markdown = tables[0].to_markdown();
        assert!(markdown.contains("| Name | Price |"));
        assert!(markdown.contains("| Alpha | $1 |"));
    }

    #[test]
    fn rows_extract_named_fields_with_attributes() {
        let rows = rows(FIXTURE, ".item", "title=h2, url=a@href").expect("valid spec and selector");
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0][0].name, "title");
        assert_eq!(rows[0][0].value, "Alpha");
        assert_eq!(rows[0][1].value, "/alpha");
        assert_eq!(rows[1][1].value, "/beta");
    }

    #[test]
    fn row_spec_rejects_malformed_fields() {
        let error = parse_row_spec("title").expect_err("missing = is rejected");
        assert!(error.to_string().contains("missing '='"));

        let error = parse_row_spec("").expect_err("empty spec is rejected");
        assert!(error.to_string().contains("empty"));
    }

    #[test]
    fn select_texts_return_matches_in_order() {
        let titles = select_texts(FIXTURE, ".item h2").expect("valid selector");
        assert_eq!(titles, ["Alpha", "Beta"]);
    }

    #[test]
    fn invalid_selectors_are_reported() {
        let error = select_texts(FIXTURE, ".item >>").expect_err("invalid selector");
        assert!(error.to_string().contains("invalid selector"));
    }

    #[test]
    fn budget_keeps_item_boundaries_and_always_one_item() {
        let items = vec!["aaaa".repeat(8), "bbbb".repeat(8), "cccc".repeat(8)];
        let budgeted = apply_budget(items.clone(), Some(4));
        assert_eq!(budgeted.items.len(), 1);
        assert_eq!(budgeted.omitted, 2);

        let budgeted = apply_budget(items.clone(), Some(100));
        assert_eq!(budgeted.items.len(), 3);
        assert_eq!(budgeted.omitted, 0);

        let oversized = vec!["x".repeat(1000)];
        let budgeted = apply_budget(oversized, Some(1));
        assert_eq!(budgeted.items.len(), 1, "first item is always kept");
    }

    #[test]
    fn tokens_estimate_at_four_characters() {
        assert_eq!(estimate_tokens(""), 0);
        assert_eq!(estimate_tokens("abc"), 1);
        assert_eq!(estimate_tokens("abcd"), 1);
        assert_eq!(estimate_tokens("abcde"), 2);
    }

    #[test]
    fn local_reports_read_files_without_http() {
        let mut path = std::env::temp_dir();
        path.push(format!("darash-fetch-test-{}.html", std::process::id()));
        std::fs::write(&path, "<h1>Local</h1>").expect("fixture writes");

        let report = read_source(&path).expect("fixture reads");
        assert_eq!(report.status, None);
        assert!(report.ok);
        assert_eq!(report.body, "<h1>Local</h1>");
        assert!(report.summary().starts_with("status=local ok=true"));

        std::fs::remove_file(&path).ok();
    }

    #[tokio::test]
    async fn fetch_rejects_non_http_schemes() {
        let error = fetch("ftp://example.test", &[])
            .await
            .expect_err("ftp is rejected before the network");
        assert!(error.to_string().contains("only http and https"));
    }

    #[test]
    fn locate_reports_deepest_holders_and_escapes_selectors() {
        let html = r#"
            <html><body>
                <div class="card sm:w-1/2" id="first">
                    <h2>Example Domain</h2>
                    <a href="https://example.com/more" data-note="Example link">More</a>
                </div>
                <div class="card">
                    <p>Nothing to see</p>
                </div>
            </body></html>
        "#;
        let hits = locate(html, "Example Domain");
        assert_eq!(
            hits.len(),
            1,
            "only the h2 holds the text, not its ancestors"
        );
        assert_eq!(hits[0].selector, "div#first > h2");
        assert_eq!(hits[0].kind, "text");
        assert_eq!(hits[0].snippet, "Example Domain");

        let attr_hits = locate(html, "Example link");
        assert_eq!(attr_hits.len(), 1);
        assert_eq!(attr_hits[0].kind, "attr");
        assert_eq!(attr_hits[0].attribute.as_deref(), Some("data-note"));
        assert!(attr_hits[0].snippet.contains("data-note=\"Example link\""));

        // Class escaping round-trips CSS identifiers like `sm:w-1/2`.
        let class_hits = locate(html, "Nothing");
        assert_eq!(class_hits[0].selector, "div.card > p");
    }

    #[test]
    fn escape_css_ident_escapes_punctuation_and_leading_digits() {
        assert_eq!(escape_css_ident("sm:w-1/2"), "sm\\:w-1\\/2");
        assert_eq!(escape_css_ident("plain_name-1"), "plain_name-1");
        assert_eq!(escape_css_ident("2col"), "\\32 col");
    }

    #[test]
    fn locate_prefers_attribute_hits_for_the_owning_element() {
        let hits = locate("<p><a href=\"/needle\">x</a></p>", "needle");
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].selector, "p > a");
        assert_eq!(hits[0].kind, "attr");
    }

    #[test]
    fn paginate_state_machine_covers_more_complete_and_past_end() {
        let more = paginate(100, 0, 50);
        assert_eq!(more.state, PageState::More);
        assert_eq!(more.returned, 50);
        assert_eq!(more.next_offset, Some(50));

        let complete = paginate(100, 50, 50);
        assert_eq!(complete.state, PageState::Complete);
        assert_eq!(complete.returned, 50);
        assert_eq!(complete.next_offset, None);

        let past = paginate(100, 100, 50);
        assert_eq!(past.state, PageState::PastEnd);
        assert_eq!(past.returned, 0);
        assert_eq!(past.next_offset, None);

        let empty = paginate(0, 0, 50);
        assert_eq!(empty.state, PageState::Complete);
        assert_eq!(empty.returned, 0);

        let short = paginate(3, 0, 50);
        assert_eq!(short.state, PageState::Complete);
        assert_eq!(short.returned, 3);

        let past_empty = paginate(0, 5, 50);
        assert_eq!(past_empty.state, PageState::PastEnd);
    }
}
