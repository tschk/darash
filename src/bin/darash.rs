use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::time::Duration;

use darash::fetch::{self, FetchReport, PageState};
use darash::{disk_cache, filter};
use darash::{SearchClient, SearchMode, SearchRequest, SearchResponse, SearchSource};
use serde_json::{json, Value};

const DEFAULT_FETCH_LIMIT: usize = 50;
/// curl's `--fail` exit code for an HTTP error status.
const EXIT_HTTP_ERROR: i32 = 22;

fn main() {
    let args = match std::env::args().skip(1).collect::<Vec<_>>() {
        args if args.is_empty() => {
            print_usage();
            return;
        }
        args => args,
    };
    if args.first().map(String::as_str) == Some("--version") {
        println!("darash {}", env!("CARGO_PKG_VERSION"));
        return;
    }

    let parsed = match parse_args(&args) {
        Ok(Some(parsed)) => parsed,
        Ok(None) => return,
        Err(error) => {
            eprintln!("{error}");
            print_usage();
            std::process::exit(2);
        }
    };

    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("tokio runtime builds");
    let outcome: Result<i32, String> = match parsed {
        Command::Search(args) => runtime
            .block_on(run_search(args))
            .map(|()| 0)
            .map_err(|error| format!("search failed: {error}")),
        Command::Fetch(args) => runtime
            .block_on(run_fetch(*args))
            .map_err(|error| format!("fetch failed: {error}")),
    };
    match outcome {
        Ok(0) => {}
        Ok(code) => std::process::exit(code),
        Err(error) => {
            eprintln!("{error}");
            std::process::exit(1);
        }
    }
}

#[derive(Debug)]
enum Command {
    Search(SearchArgs),
    Fetch(Box<FetchArgs>),
}

#[derive(Debug)]
struct SearchArgs {
    query: String,
    mode: SearchMode,
    sources: Vec<SearchSource>,
    endpoint: Option<String>,
    json: bool,
}

#[derive(Debug, Default)]
struct FetchArgs {
    input: String,
    headers: Vec<(String, String)>,
    method: Option<String>,
    data: Option<Vec<u8>>,
    basic_auth: Option<(String, String)>,
    head: bool,
    output: Option<PathBuf>,
    insecure: bool,
    max_time: Option<Duration>,
    max_bytes: Option<usize>,
    fail: bool,
    body: bool,
    markdown: bool,
    text: bool,
    outline: bool,
    select: Option<String>,
    row: Option<String>,
    table: bool,
    locate: Option<String>,
    count: bool,
    limit: Option<usize>,
    budget: Option<usize>,
    json: bool,
    json_envelope: bool,
    offset: usize,
    where_: Option<String>,
    fresh: bool,
    no_cache: bool,
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum Extraction {
    Report,
    Body,
    Markdown,
    Text,
    Outline,
    Select,
    Row,
    Table,
    Locate,
}

/// How `-d`/`--data`, `--data-raw`, and `--data-binary` treat their argument.
#[derive(Clone, Copy, Debug, PartialEq)]
enum DataMode {
    /// `-d`: `@file` is read and CR/LF are stripped.
    Strip,
    /// `--data-raw`: `@` is a literal character.
    Raw,
    /// `--data-binary`: `@file` is read byte for byte.
    Binary,
}

/// One structured result: its JSON form and its ordered plain cells.
struct Record {
    json: Value,
    cells: Vec<String>,
}

async fn run_search(args: SearchArgs) -> Result<(), String> {
    let request = SearchRequest::new(args.query)
        .with_mode(args.mode)
        .with_sources(args.sources);
    let client = match args.endpoint {
        Some(endpoint) => SearchClient::new(endpoint),
        None => SearchClient::local(),
    }
    .map_err(|error| error.to_string())?;
    let response = client
        .search_request(&request)
        .await
        .map_err(|error| error.to_string())?;
    if args.json {
        let rendered =
            serde_json::to_string_pretty(&response).map_err(|error| error.to_string())?;
        println!("{rendered}");
    } else {
        print_response(&response);
    }
    Ok(())
}

async fn run_fetch(args: FetchArgs) -> Result<i32, String> {
    let extraction = pick_extraction(&args)?;
    let structured = matches!(
        extraction,
        Extraction::Select | Extraction::Row | Extraction::Table | Extraction::Locate
    );

    validate_fetch_args(&args, extraction, structured)?;

    let is_url = args.input.starts_with("http://") || args.input.starts_with("https://");
    let method = args.method.clone().unwrap_or_else(|| "GET".to_owned());
    let no_store = args.headers.iter().any(|(name, value)| {
        name.eq_ignore_ascii_case("cache-control")
            && value.to_ascii_lowercase().contains("no-store")
    });
    let cacheable = is_url
        && !args.no_cache
        && disk_cache::should_cache(
            &args.input,
            &method,
            !args.headers.is_empty(),
            args.data.is_some(),
            no_store,
        );

    let report = if is_url {
        fetch_report(&args, &method, cacheable).await?
    } else {
        fetch::read_source(&args.input).map_err(|error| error.to_string())?
    };

    if let Some(path) = &args.output {
        atomic_write(path, report.body.as_bytes())
            .map_err(|error| format!("cannot write {}: {error}", path.display()))?;
        eprintln!("darash fetch: {}", report.summary());
        return Ok(fail_code(&args, &report));
    }

    match extraction {
        Extraction::Report => {
            let rendered =
                serde_json::to_string_pretty(&report).map_err(|error| error.to_string())?;
            println!("{rendered}");
            Ok(fail_code(&args, &report))
        }
        Extraction::Body => {
            eprintln!("darash fetch: {}", report.summary());
            println!("{}", report.body);
            Ok(fail_code(&args, &report))
        }
        Extraction::Markdown | Extraction::Text => {
            handle_document_extraction(&args, extraction, &report)
        }
        Extraction::Outline => handle_outline_extraction(&args, &report),
        Extraction::Select | Extraction::Row | Extraction::Table | Extraction::Locate => {
            handle_structured_extraction(&args, extraction, &report)
        }
    }
}

async fn fetch_report(
    args: &FetchArgs,
    method: &str,
    cacheable: bool,
) -> Result<FetchReport, String> {
    if cacheable && !args.fresh {
        if let Some((report, age)) = disk_cache::load(&args.input) {
            // A cached body larger than an explicit cap must not satisfy it.
            let within_cap = args
                .max_bytes
                .map(|max| report.body.len() <= max)
                .unwrap_or(true);
            if within_cap {
                eprintln!("using {age}s-old cached fetch (--fresh to refetch)");
                return Ok(report);
            }
        }
    }
    let options = fetch::FetchOptions {
        method: Some(method.to_owned()),
        headers: args.headers.clone(),
        body: args.data.clone(),
        basic_auth: args.basic_auth.clone(),
        insecure: args.insecure,
        timeout: args.max_time,
        max_bytes: args.max_bytes,
    };
    let report = fetch::fetch_with(&args.input, &options)
        .await
        .map_err(|error| error.to_string())?;
    if cacheable {
        let _ = disk_cache::store(&args.input, &report);
    }
    Ok(report)
}

fn handle_document_extraction(
    args: &FetchArgs,
    extraction: Extraction,
    report: &FetchReport,
) -> Result<i32, String> {
    let rendered = if extraction == Extraction::Markdown {
        fetch::to_markdown(&report.body)
    } else {
        fetch::to_text(&report.body)
    };
    eprintln!("darash fetch: {}", report.summary());
    emit_document(rendered, report, args.budget, args.json);
    Ok(fail_code(args, report))
}

fn handle_outline_extraction(args: &FetchArgs, report: &FetchReport) -> Result<i32, String> {
    let entries = fetch::outline(&report.body);
    let items = entries
        .iter()
        .map(|entry| {
            format!(
                "{}	{}	{}",
                entry.selector,
                entry.count,
                entry.sample.as_deref().unwrap_or_default()
            )
        })
        .collect::<Vec<_>>();
    let data = serde_json::to_value(&entries).map_err(|error| error.to_string())?;
    let (limited, limit_omitted) = cap(items, args.limit.unwrap_or(DEFAULT_FETCH_LIMIT));
    let budgeted = fetch::apply_budget(limited, args.budget);
    let omitted = limit_omitted + budgeted.omitted;
    eprintln!("darash fetch: {}", report.summary());
    if omitted > 0 {
        eprintln!(
            "darash fetch: {omitted} item(s) omitted; raise --limit/--budget or narrow the selector"
        );
    }
    if args.json {
        emit_json(data, report, budgeted.items.len(), omitted);
    } else {
        emit_plain(&budgeted.items);
    }
    Ok(fail_code(args, report))
}

fn handle_structured_extraction(
    args: &FetchArgs,
    extraction: Extraction,
    report: &FetchReport,
) -> Result<i32, String> {
    let (records, headers, force_json) = build_records(extraction, args, report)?;
    if extraction == Extraction::Select && args.count {
        let selector = args.select.as_deref().unwrap_or_default();
        if records.is_empty() {
            return Err(format!("--select {selector:?} matched nothing"));
        }
        eprintln!("darash fetch: {}", report.summary());
        println!("{}", records.len());
        return Ok(0);
    }

    let filtered: Vec<&Record> = match &args.where_ {
        Some(expr) => {
            let compiled = filter::compile(expr).map_err(|error| format!("--where: {error}"))?;
            let matched = records
                .iter()
                .filter(|record| compiled.matches(&record.json))
                .collect::<Vec<_>>();
            if matched.is_empty() && !records.is_empty() {
                eprintln!(
                    "darash fetch: --where matched 0 of {} row(s)",
                    records.len()
                );
            }
            matched
        }
        None => records.iter().collect(),
    };
    let total = filtered.len();
    let meta = fetch::paginate(
        total,
        args.offset,
        args.limit.unwrap_or(DEFAULT_FETCH_LIMIT),
    );
    let page: Vec<&Record> = if meta.state == PageState::PastEnd {
        Vec::new()
    } else {
        filtered[meta.offset..meta.offset + meta.returned].to_vec()
    };
    let data = Value::Array(page.iter().map(|record| record.json.clone()).collect());

    eprintln!("darash fetch: {}", report.summary());
    if !args.json_envelope {
        match meta.state {
            PageState::More => {
                let hidden = meta.total - (meta.offset + meta.returned);
                eprintln!(
                    "darash fetch: {hidden} more result(s) hidden — continue with --offset {}",
                    meta.next_offset.unwrap_or(meta.total)
                );
            }
            PageState::PastEnd => {
                eprintln!(
                    "darash fetch: --offset is past the end — only {} result(s) exist",
                    meta.total
                );
            }
            PageState::Complete => {}
        }
    }

    if args.json_envelope {
        let envelope = json!({
            "data": data,
            "meta": serde_json::to_value(&meta).map_err(|error| error.to_string())?,
        });
        let rendered =
            serde_json::to_string_pretty(&envelope).map_err(|error| error.to_string())?;
        println!("{rendered}");
    } else if force_json && !args.json {
        let rendered = serde_json::to_string_pretty(&data).map_err(|error| error.to_string())?;
        println!("{rendered}");
    } else if args.json {
        emit_json(
            data,
            report,
            meta.returned,
            meta.total.saturating_sub(meta.returned),
        );
    } else {
        let lines = plain_lines(extraction, &page, &headers);
        let budgeted = fetch::apply_budget(lines, args.budget);
        if budgeted.omitted > 0 {
            eprintln!(
                "darash fetch: {} item(s) omitted; raise --budget to include more",
                budgeted.omitted
            );
        }
        emit_plain(&budgeted.items);
    }
    Ok(fail_code(args, report))
}

fn validate_fetch_args(
    args: &FetchArgs,
    extraction: Extraction,
    structured: bool,
) -> Result<(), String> {
    if args.count && extraction != Extraction::Select {
        return Err("--count requires --select".to_owned());
    }
    if !structured && (args.offset > 0 || args.json_envelope) {
        return Err(
            "--offset/--json-envelope apply to --select, --row, --table, or --locate".to_owned(),
        );
    }
    if args.where_.is_some() && !matches!(extraction, Extraction::Row | Extraction::Table) {
        return Err("--where applies to --row or --table output".to_owned());
    }
    if args.output.is_some() && !matches!(extraction, Extraction::Report | Extraction::Body) {
        return Err("--output/-o applies only to the default report or --body".to_owned());
    }
    Ok(())
}

fn build_records(
    extraction: Extraction,
    args: &FetchArgs,
    report: &FetchReport,
) -> Result<(Vec<Record>, Vec<String>, bool), String> {
    match extraction {
        Extraction::Select => {
            let selector = args.select.as_deref().expect("select mode has a selector");
            let texts =
                fetch::select_texts(&report.body, selector).map_err(|error| error.to_string())?;
            let records = texts
                .into_iter()
                .map(|text| Record {
                    json: json!(text),
                    cells: vec![text],
                })
                .collect();
            Ok((records, Vec::new(), false))
        }
        Extraction::Row => {
            let container = args.select.as_deref().expect("row mode has a container");
            let spec = args.row.as_deref().expect("row mode has a spec");
            let rows =
                fetch::rows(&report.body, container, spec).map_err(|error| error.to_string())?;
            let headers = fetch::parse_row_spec(spec)
                .map_err(|error| error.to_string())?
                .into_iter()
                .map(|(name, _)| name)
                .collect::<Vec<_>>();
            let records = rows
                .into_iter()
                .map(|row| {
                    let cells = row
                        .iter()
                        .map(|field| field.value.clone())
                        .collect::<Vec<_>>();
                    let mut object = serde_json::Map::new();
                    for field in &row {
                        object.insert(field.name.clone(), json!(field.value));
                    }
                    Record {
                        json: Value::Object(object),
                        cells,
                    }
                })
                .collect();
            Ok((records, headers, false))
        }
        Extraction::Table => {
            let tables = fetch::tables(&report.body);
            match tables.len() {
                0 => Ok((Vec::new(), Vec::new(), false)),
                1 => {
                    let table = tables.into_iter().next().expect("one table");
                    let headers = table.headers.clone();
                    let records = table
                        .rows
                        .into_iter()
                        .map(|row| {
                            let cells = row.clone();
                            let mut object = serde_json::Map::new();
                            for (index, header) in headers.iter().enumerate() {
                                object.insert(
                                    header.clone(),
                                    json!(row.get(index).cloned().unwrap_or_default()),
                                );
                            }
                            Record {
                                json: Value::Object(object),
                                cells,
                            }
                        })
                        .collect();
                    Ok((records, headers, false))
                }
                _ => {
                    if args.where_.is_some() {
                        return Err(
                            "--where cannot filter multiple tables; use --row or narrow the page"
                                .to_owned(),
                        );
                    }
                    let records = tables
                        .iter()
                        .map(|table| Record {
                            json: serde_json::to_value(table).expect("table serializes"),
                            cells: Vec::new(),
                        })
                        .collect();
                    Ok((records, Vec::new(), true))
                }
            }
        }
        Extraction::Locate => {
            let needle = args.locate.as_deref().expect("locate mode has a needle");
            let hits = fetch::locate(&report.body, needle);
            let records = hits
                .into_iter()
                .map(|hit| {
                    let cells = vec![hit.selector.clone(), hit.snippet.clone()];
                    Record {
                        json: serde_json::to_value(&hit).expect("locate hit serializes"),
                        cells,
                    }
                })
                .collect();
            Ok((records, Vec::new(), false))
        }
        _ => Ok((Vec::new(), Vec::new(), false)),
    }
}

/// Render markdown or text with an optional token budget cut at blocks.
fn emit_document(rendered: String, report: &FetchReport, budget: Option<usize>, json: bool) {
    if rendered.is_empty() {
        eprintln!("darash fetch: no content extracted");
    }
    if json {
        emit_json(json!(rendered), report, 1, 0);
        return;
    }
    let Some(budget) = budget else {
        println!("{rendered}");
        return;
    };
    let blocks = rendered
        .split("\n\n")
        .map(|block| block.to_owned())
        .collect::<Vec<_>>();
    let budgeted = fetch::apply_budget(blocks, Some(budget));
    if budgeted.omitted > 0 {
        eprintln!(
            "darash fetch: {} block(s) omitted; raise --budget to include more",
            budgeted.omitted
        );
    }
    println!("{}", budgeted.items.join("\n\n"));
}

fn emit_json(data: Value, report: &FetchReport, count: usize, omitted: usize) {
    let mut meta = serde_json::to_value(report).expect("report serializes");
    if let Value::Object(map) = &mut meta {
        map.remove("body");
        map.insert("count".to_owned(), json!(count));
        map.insert("omitted".to_owned(), json!(omitted));
    }
    let envelope = serde_json::to_string_pretty(&json!({ "data": data, "meta": meta }))
        .expect("envelope serializes");
    println!("{envelope}");
}

fn emit_plain(items: &[String]) {
    if items.is_empty() {
        eprintln!("darash fetch: no items extracted");
        return;
    }
    println!("{}", items.join("\n"));
}

fn plain_lines(extraction: Extraction, records: &[&Record], headers: &[String]) -> Vec<String> {
    match extraction {
        Extraction::Select => records
            .iter()
            .map(|record| record.cells.first().cloned().unwrap_or_default())
            .collect(),
        Extraction::Locate => records
            .iter()
            .map(|record| record.cells.join("\t"))
            .collect(),
        Extraction::Row | Extraction::Table => {
            let mut lines = Vec::new();
            if !headers.is_empty() && !records.is_empty() {
                lines.push(headers.join("\t"));
            }
            lines.extend(records.iter().map(|record| {
                record
                    .cells
                    .iter()
                    .map(|cell| fold_tsv(cell))
                    .collect::<Vec<_>>()
                    .join("\t")
            }));
            lines
        }
        _ => Vec::new(),
    }
}

/// Fold tabs and newlines inside a scalar so a TSV row stays one line.
fn fold_tsv(value: &str) -> String {
    value
        .chars()
        .map(|ch| match ch {
            '\t' | '\n' | '\r' => ' ',
            other => other,
        })
        .collect()
}

/// Cap a list of items, always keeping at least one.
fn cap(items: Vec<String>, limit: usize) -> (Vec<String>, usize) {
    if items.len() <= limit {
        return (items, 0);
    }
    let mut kept = items;
    let omitted = kept.len() - limit.max(1);
    kept.truncate(limit.max(1));
    (kept, omitted)
}

fn pick_extraction(args: &FetchArgs) -> Result<Extraction, String> {
    // `--select` names a container when combined with `--row`, so the two
    // together are one mode; every other pair conflicts.
    if args.row.is_some() {
        if args.select.is_none() {
            return Err("--row requires --select to name the repeating container".to_owned());
        }
        if args.body
            || args.markdown
            || args.text
            || args.outline
            || args.table
            || args.locate.is_some()
        {
            return Err("choose one extraction mode".to_owned());
        }
        return Ok(Extraction::Row);
    }

    let mut selected = Vec::new();
    if args.body {
        selected.push((Extraction::Body, "--body"));
    }
    if args.markdown {
        selected.push((Extraction::Markdown, "--md"));
    }
    if args.text {
        selected.push((Extraction::Text, "--text"));
    }
    if args.outline {
        selected.push((Extraction::Outline, "--outline"));
    }
    if args.select.is_some() {
        selected.push((Extraction::Select, "--select"));
    }
    if args.table {
        selected.push((Extraction::Table, "--table"));
    }
    if args.locate.is_some() {
        selected.push((Extraction::Locate, "--locate"));
    }
    if selected.len() > 1 {
        let names = selected
            .iter()
            .map(|(_, name)| *name)
            .collect::<Vec<_>>()
            .join(", ");
        return Err(format!("choose one extraction mode, got: {names}"));
    }
    Ok(selected
        .first()
        .map(|(extraction, _)| *extraction)
        .unwrap_or(Extraction::Report))
}

fn fail_code(args: &FetchArgs, report: &FetchReport) -> i32 {
    if args.fail && report.status.map(|status| status >= 400).unwrap_or(false) {
        EXIT_HTTP_ERROR
    } else {
        0
    }
}

/// Write bytes atomically: a private temp file in the target directory, then a
/// rename. On any failure the destination is left untouched.
fn atomic_write(path: &Path, data: &[u8]) -> std::io::Result<()> {
    let parent = match path.parent() {
        Some(parent) if !parent.as_os_str().is_empty() => parent.to_path_buf(),
        _ => PathBuf::from("."),
    };
    let file_name = path.file_name().ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "output path has no file name",
        )
    })?;
    let tmp = parent.join(format!(
        ".{}.tmp.{}",
        file_name.to_string_lossy(),
        std::process::id()
    ));
    let write = || -> std::io::Result<()> {
        let mut file = std::fs::File::create(&tmp)?;
        file.write_all(data)?;
        file.sync_all()?;
        Ok(())
    };
    if let Err(error) = write() {
        let _ = std::fs::remove_file(&tmp);
        return Err(error);
    }
    if let Err(error) = std::fs::rename(&tmp, path) {
        let _ = std::fs::remove_file(&tmp);
        return Err(error);
    }
    Ok(())
}

fn parse_args(args: &[String]) -> Result<Option<Command>, String> {
    match args.first().map(String::as_str) {
        Some("search") => parse_search_args(&args[1..]).map(|parsed| parsed.map(Command::Search)),
        Some("fetch") => parse_fetch_args(&args[1..])
            .map(|parsed| parsed.map(|args| Command::Fetch(Box::new(args)))),
        Some("--help" | "-h") => Ok(None),
        Some(command) => Err(format!("unknown command: {command}")),
        None => Err("missing command".to_owned()),
    }
}

fn parse_search_args(args: &[String]) -> Result<Option<SearchArgs>, String> {
    let mut query = Vec::new();
    let mut mode = SearchMode::default();
    let mut sources = vec![SearchSource::default()];
    let mut endpoint = None;
    let mut json = false;
    let mut source_set = false;

    let mut index = 0;
    while index < args.len() {
        let arg = args[index].as_str();
        index += 1;
        match arg {
            "--mode" => {
                mode = parse_mode(next_value(args, &mut index, "--mode")?)?;
            }
            "--source" => {
                if !source_set {
                    sources.clear();
                    source_set = true;
                }
                sources.push(parse_source(next_value(args, &mut index, "--source")?)?);
            }
            "--url" => {
                endpoint = Some(next_value(args, &mut index, "--url")?.to_owned());
            }
            "--json" => json = true,
            "--help" | "-h" => return Ok(None),
            value if value.starts_with('-') => return Err(format!("unknown option: {value}")),
            value => query.push(value.to_owned()),
        }
    }

    if query.is_empty() {
        return Err("search requires a query".to_owned());
    }

    Ok(Some(SearchArgs {
        query: query.join(" "),
        mode,
        sources,
        endpoint,
        json,
    }))
}

fn parse_fetch_args(args: &[String]) -> Result<Option<FetchArgs>, String> {
    let mut parsed = FetchArgs::default();

    let mut index = 0;
    while index < args.len() {
        let arg = args[index].as_str();
        index += 1;

        if arg == "--" {
            for value in &args[index..] {
                if !parsed.input.is_empty() {
                    return Err(format!("unexpected argument: {value}"));
                }
                parsed.input = value.clone();
            }
            break;
        }

        if let Some(long) = arg.strip_prefix("--") {
            let (name, inline) = match long.split_once('=') {
                Some((name, value)) => (name, Some(value.to_owned())),
                None => (long, None),
            };
            match name {
                "body" => parsed.body = true,
                "md" => parsed.markdown = true,
                "text" => parsed.text = true,
                "outline" => parsed.outline = true,
                "table" => parsed.table = true,
                "count" => parsed.count = true,
                "json" => parsed.json = true,
                "json-envelope" => parsed.json_envelope = true,
                "insecure" => parsed.insecure = true,
                "fail" => parsed.fail = true,
                "head" => parsed.head = true,
                "fresh" => parsed.fresh = true,
                "no-cache" => parsed.no_cache = true,
                // Accepted no-op: curl's compressed-transfer request.
                "compressed" => {}
                "select" => {
                    parsed.select = Some(option_value(args, &mut index, inline, "--select")?)
                }
                "row" => parsed.row = Some(option_value(args, &mut index, inline, "--row")?),
                "locate" => {
                    parsed.locate = Some(option_value(args, &mut index, inline, "--locate")?)
                }
                "where" => parsed.where_ = Some(option_value(args, &mut index, inline, "--where")?),
                "method" => {
                    parsed.method = Some(option_value(args, &mut index, inline, "--method")?)
                }
                "user" => {
                    let value = option_value(args, &mut index, inline, "--user")?;
                    parsed.basic_auth = Some(parse_basic_auth(&value));
                }
                "output" => {
                    parsed.output = Some(PathBuf::from(option_value(
                        args, &mut index, inline, "--output",
                    )?));
                }
                "max-time" => {
                    let value = option_value(args, &mut index, inline, "--max-time")?;
                    parsed.max_time = Some(parse_seconds(&value)?);
                }
                "max-bytes" => {
                    let value = option_value(args, &mut index, inline, "--max-bytes")?;
                    parsed.max_bytes = Some(parse_usize(&value, "--max-bytes")?);
                }
                "limit" => {
                    let value = option_value(args, &mut index, inline, "--limit")?;
                    parsed.limit = Some(parse_usize(&value, "--limit")?);
                }
                "budget" => {
                    let value = option_value(args, &mut index, inline, "--budget")?;
                    parsed.budget = Some(parse_usize(&value, "--budget")?);
                }
                "offset" => {
                    let value = option_value(args, &mut index, inline, "--offset")?;
                    parsed.offset = parse_usize(&value, "--offset")?;
                }
                "header" => {
                    let value = option_value(args, &mut index, inline, "--header")?;
                    parsed.headers.push(parse_header(&value)?);
                }
                "data" => {
                    let value = option_value(args, &mut index, inline, "--data")?;
                    add_data(&mut parsed, &value, DataMode::Strip)?;
                }
                "data-raw" => {
                    let value = option_value(args, &mut index, inline, "--data-raw")?;
                    add_data(&mut parsed, &value, DataMode::Raw)?;
                }
                "data-binary" => {
                    let value = option_value(args, &mut index, inline, "--data-binary")?;
                    add_data(&mut parsed, &value, DataMode::Binary)?;
                }
                "help" => return Ok(None),
                other => return Err(format!("unknown option: --{other}")),
            }
            continue;
        }

        if arg.len() > 1 && arg.starts_with('-') {
            let chars: Vec<char> = arg[1..].chars().collect();
            let mut position = 0;
            while position < chars.len() {
                let flag = chars[position];
                match flag {
                    'X' | 'd' | 'u' | 'o' | 'm' => {
                        let rest: String = chars[position + 1..].iter().collect();
                        let value = if rest.is_empty() {
                            next_value(args, &mut index, "-")?.to_owned()
                        } else {
                            rest
                        };
                        match flag {
                            'X' => parsed.method = Some(value),
                            'd' => add_data(&mut parsed, &value, DataMode::Strip)?,
                            'u' => parsed.basic_auth = Some(parse_basic_auth(&value)),
                            'o' => parsed.output = Some(PathBuf::from(value)),
                            'm' => parsed.max_time = Some(parse_seconds(&value)?),
                            _ => unreachable!(),
                        }
                        break;
                    }
                    'I' => parsed.head = true,
                    'k' => parsed.insecure = true,
                    'f' => parsed.fail = true,
                    // Accepted no-ops: follow redirects, include headers, silent.
                    'L' | 'i' | 's' | 'S' => {}
                    'h' => return Ok(None),
                    other => return Err(format!("unknown option: -{other}")),
                }
                position += 1;
            }
            continue;
        }

        if !parsed.input.is_empty() {
            return Err(format!("unexpected argument: {arg}"));
        }
        parsed.input = arg.to_owned();
    }

    if parsed.input.is_empty() {
        return Err("fetch requires a URL or a local file".to_owned());
    }

    if parsed.head {
        parsed.method = Some("HEAD".to_owned());
    } else if parsed.data.is_some() && parsed.method.is_none() {
        parsed.method = Some("POST".to_owned());
    }
    if parsed.data.is_some()
        && !parsed
            .headers
            .iter()
            .any(|(name, _)| name.eq_ignore_ascii_case("content-type"))
    {
        parsed.headers.push((
            "Content-Type".to_owned(),
            "application/x-www-form-urlencoded".to_owned(),
        ));
    }

    Ok(Some(parsed))
}

fn option_value(
    args: &[String],
    index: &mut usize,
    inline: Option<String>,
    flag: &str,
) -> Result<String, String> {
    match inline {
        Some(value) => Ok(value),
        None => Ok(next_value(args, index, flag)?.to_owned()),
    }
}

fn next_value<'a>(args: &'a [String], index: &mut usize, flag: &str) -> Result<&'a str, String> {
    let value = args
        .get(*index)
        .ok_or_else(|| format!("{flag} requires a value"))?;
    *index += 1;
    Ok(value)
}

fn add_data(parsed: &mut FetchArgs, value: &str, mode: DataMode) -> Result<(), String> {
    let bytes = match mode {
        DataMode::Raw => value.as_bytes().to_vec(),
        DataMode::Strip | DataMode::Binary => match value.strip_prefix('@') {
            Some(source) => {
                let raw = read_at_source(source)?;
                if mode == DataMode::Strip {
                    raw.into_iter()
                        .filter(|byte| *byte != b'\r' && *byte != b'\n')
                        .collect()
                } else {
                    raw
                }
            }
            None => value.as_bytes().to_vec(),
        },
    };
    match &mut parsed.data {
        Some(existing) => {
            existing.push(b'&');
            existing.extend_from_slice(&bytes);
        }
        None => parsed.data = Some(bytes),
    }
    Ok(())
}

fn read_at_source(source: &str) -> Result<Vec<u8>, String> {
    if source == "-" {
        let mut buffer = Vec::new();
        std::io::stdin()
            .read_to_end(&mut buffer)
            .map_err(|error| format!("cannot read stdin: {error}"))?;
        return Ok(buffer);
    }
    std::fs::read(source).map_err(|error| format!("cannot read {source}: {error}"))
}

fn parse_basic_auth(value: &str) -> (String, String) {
    match value.split_once(':') {
        Some((user, password)) => (user.to_owned(), password.to_owned()),
        None => (value.to_owned(), String::new()),
    }
}

fn parse_seconds(value: &str) -> Result<Duration, String> {
    value
        .parse::<f64>()
        .ok()
        .filter(|seconds| seconds.is_finite() && *seconds >= 0.0)
        .map(Duration::from_secs_f64)
        .ok_or_else(|| format!("--max-time expects seconds, got {value:?}"))
}

fn parse_usize(value: &str, flag: &str) -> Result<usize, String> {
    value
        .parse::<usize>()
        .map_err(|_| format!("{flag} expects a non-negative integer, got {value:?}"))
}

fn parse_header(value: &str) -> Result<(String, String), String> {
    let Some((name, header_value)) = value.split_once(':') else {
        return Err(format!("header {value:?} must look like 'Name: value'"));
    };
    let name = name.trim();
    if name.is_empty() {
        return Err(format!("header {value:?} is missing a name"));
    }
    Ok((name.to_owned(), header_value.trim().to_owned()))
}

fn parse_mode(value: &str) -> Result<SearchMode, String> {
    match value {
        "speed" => Ok(SearchMode::Speed),
        "balanced" => Ok(SearchMode::Balanced),
        "quality" => Ok(SearchMode::Quality),
        _ => Err(format!("invalid mode: {value}")),
    }
}

fn parse_source(value: &str) -> Result<SearchSource, String> {
    match value {
        "web" => Ok(SearchSource::Web),
        "academic" => Ok(SearchSource::Academic),
        "discussions" => Ok(SearchSource::Discussions),
        _ => Err(format!("invalid source: {value}")),
    }
}

fn print_response(response: &SearchResponse) {
    if let Some(answer) = &response.answer {
        println!("{answer}\n");
    }
    for source in response.cited_sources() {
        println!("{source}\n");
    }
}

fn usage() -> &'static str {
    "Usage:
  darash search <query> [--mode speed|balanced|quality] [--source web|academic|discussions] [--url endpoint] [--json]
  darash fetch <url|file> [extraction] [curl options] [--json]
    extraction: --md | --text | --outline | --select CSS [--count] | --row spec | --table | --locate TEXT | --body
    request:    -X/--method M, -d/--data BODY|@FILE|@-, --data-raw B, --data-binary B, -u USER:PASS,
                -I/--head, -k/--insecure, -m/--max-time SECS, --max-bytes N, --header 'Name: value'
    output:     -o FILE, -f/--fail, --limit N, --budget N, --offset N, --where EXPR, --json, --json-envelope
    cache:      --fresh, --no-cache"
}

fn print_usage() {
    eprintln!(
        "{} (search defaults to the in-process Darash backend; fetch reports every request)",
        usage()
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(list: &[&str]) -> Vec<String> {
        list.iter().map(|value| value.to_string()).collect()
    }

    fn fetch_args(list: &[&str]) -> FetchArgs {
        let command = parse_args(&args(list))
            .expect("valid arguments")
            .expect("not help");
        let Command::Fetch(fetch) = command else {
            panic!("expected fetch command");
        };
        *fetch
    }

    #[test]
    fn parses_search_options_without_environment() {
        let command = parse_args(&args(&[
            "search",
            "rust",
            "async",
            "--mode",
            "quality",
            "--source",
            "academic",
            "--url",
            "http://localhost:9090",
            "--json",
        ]))
        .expect("valid arguments")
        .expect("not help");

        let Command::Search(search) = command else {
            panic!("expected search command");
        };
        assert_eq!(search.query, "rust async");
        assert_eq!(search.mode, SearchMode::Quality);
        assert_eq!(search.sources, [SearchSource::Academic]);
        assert_eq!(search.endpoint.as_deref(), Some("http://localhost:9090"));
        assert!(search.json);
    }

    #[test]
    fn parses_fetch_flags() {
        let fetch = fetch_args(&[
            "fetch",
            "https://example.test",
            "--md",
            "--limit",
            "10",
            "--budget",
            "200",
            "--header",
            "Accept: text/html",
            "--json",
        ]);
        assert_eq!(fetch.input, "https://example.test");
        assert!(fetch.markdown);
        assert_eq!(fetch.limit, Some(10));
        assert_eq!(fetch.budget, Some(200));
        assert_eq!(
            fetch.headers,
            vec![("Accept".to_owned(), "text/html".to_owned())]
        );
        assert!(fetch.json);
    }

    #[test]
    fn parses_row_mode_with_container() {
        let fetch = fetch_args(&[
            "fetch",
            "page.html",
            "--select",
            ".item",
            "--row",
            "title=h2, url=a@href",
        ]);
        assert_eq!(fetch.input, "page.html");
        assert_eq!(fetch.select.as_deref(), Some(".item"));
        assert_eq!(fetch.row.as_deref(), Some("title=h2, url=a@href"));
        assert_eq!(pick_extraction(&fetch).expect("row mode"), Extraction::Row);
    }

    #[test]
    fn fetch_defaults_to_report() {
        let fetch = fetch_args(&["fetch", "page.html"]);
        assert_eq!(
            pick_extraction(&fetch).expect("report mode"),
            Extraction::Report
        );
    }

    #[test]
    fn conflicting_extraction_modes_are_rejected() {
        let fetch = fetch_args(&["fetch", "page.html", "--md", "--outline"]);
        let error = pick_extraction(&fetch).expect_err("conflict is rejected");
        assert!(error.contains("choose one extraction mode"));
    }

    #[test]
    fn row_mode_requires_a_container() {
        let fetch = fetch_args(&["fetch", "page.html", "--row", "title=h2"]);
        let error = pick_extraction(&fetch).expect_err("row without container is rejected");
        assert!(error.contains("--row requires --select"));
    }

    #[test]
    fn fetch_requires_a_source() {
        let error = parse_args(&args(&["fetch", "--md"])).expect_err("missing input");
        assert!(error.contains("fetch requires"));
    }

    #[test]
    fn parses_help_without_error() {
        assert!(parse_args(&args(&["--help"]))
            .expect("help is valid")
            .is_none());
        assert!(parse_args(&args(&["search", "--help"]))
            .expect("help is valid")
            .is_none());
        assert!(parse_args(&args(&["fetch", "--help"]))
            .expect("help is valid")
            .is_none());
    }

    #[test]
    fn headers_need_name_and_value() {
        let error = parse_header("novalue").expect_err("header without colon");
        assert!(error.contains("Name: value"));

        let (name, value) = parse_header("X-Test:  spaced ").expect("header parses");
        assert_eq!(name, "X-Test");
        assert_eq!(value, "spaced");
    }

    #[test]
    fn cap_keeps_at_least_one_item() {
        let (kept, omitted) = cap(vec!["a".to_owned(), "b".to_owned()], 1);
        assert_eq!(kept, ["a"]);
        assert_eq!(omitted, 1);

        let (kept, omitted) = cap(vec!["a".to_owned(), "b".to_owned(), "c".to_owned()], 5);
        assert_eq!(kept.len(), 3);
        assert_eq!(omitted, 0);
    }

    #[test]
    fn parses_curl_style_request_flags() {
        let fetch = fetch_args(&[
            "fetch",
            "https://example.test",
            "-X",
            "PUT",
            "-d",
            "a=1",
            "-u",
            "user:pass",
            "-k",
            "-m",
            "5",
            "--max-bytes",
            "100",
            "-o",
            "out.bin",
        ]);
        assert_eq!(fetch.method.as_deref(), Some("PUT"));
        assert_eq!(fetch.data.as_deref(), Some(b"a=1".as_slice()));
        assert_eq!(
            fetch.basic_auth,
            Some(("user".to_owned(), "pass".to_owned()))
        );
        assert!(fetch.insecure);
        assert_eq!(fetch.max_time, Some(Duration::from_secs(5)));
        assert_eq!(fetch.max_bytes, Some(100));
        assert_eq!(fetch.output, Some(PathBuf::from("out.bin")));
    }

    #[test]
    fn data_defaults_method_to_post_and_sets_content_type() {
        let fetch = fetch_args(&["fetch", "https://example.test", "-d", "a=1"]);
        assert_eq!(fetch.method.as_deref(), Some("POST"));
        assert!(fetch
            .headers
            .iter()
            .any(|(name, value)| name == "Content-Type"
                && value == "application/x-www-form-urlencoded"));
    }

    #[test]
    fn head_forces_head_and_no_op_flags_are_accepted() {
        let fetch = fetch_args(&["fetch", "https://example.test", "-ILsS", "--compressed"]);
        assert!(fetch.head);
        assert_eq!(fetch.method.as_deref(), Some("HEAD"));
    }

    #[test]
    fn data_raw_never_reads_a_file() {
        let mut parsed = FetchArgs::default();
        add_data(&mut parsed, "@not-a-file", DataMode::Raw).expect("raw data");
        assert_eq!(parsed.data.as_deref(), Some(b"@not-a-file".as_slice()));
    }

    #[test]
    fn parses_structured_output_flags() {
        let fetch = fetch_args(&[
            "fetch",
            "https://example.test",
            "--select",
            "a",
            "--count",
            "--offset",
            "10",
            "--json-envelope",
            "--where",
            "price > 1",
        ]);
        assert!(fetch.count);
        assert_eq!(fetch.offset, 10);
        assert!(fetch.json_envelope);
        assert_eq!(fetch.where_.as_deref(), Some("price > 1"));
    }

    #[test]
    fn fold_tsv_flattens_scalars() {
        assert_eq!(fold_tsv("a\tb\nc"), "a b c");
        assert_eq!(fold_tsv("plain"), "plain");
    }
}
