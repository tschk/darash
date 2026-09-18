use std::env;

use darash::fetch::{self, FetchReport};
use darash::{SearchClient, SearchMode, SearchRequest, SearchResponse, SearchSource};
use serde_json::json;

const DEFAULT_FETCH_LIMIT: usize = 50;

fn main() {
    let args = match env::args().skip(1).collect::<Vec<_>>() {
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
    let outcome = match parsed {
        Command::Search(args) => runtime
            .block_on(run_search(args))
            .map_err(|error| format!("search failed: {error}")),
        Command::Fetch(args) => runtime
            .block_on(run_fetch(args))
            .map_err(|error| format!("fetch failed: {error}")),
    };
    if let Err(error) = outcome {
        eprintln!("{error}");
        std::process::exit(1);
    }
}

#[derive(Debug)]
enum Command {
    Search(SearchArgs),
    Fetch(FetchArgs),
}

#[derive(Debug)]
struct SearchArgs {
    query: String,
    mode: SearchMode,
    sources: Vec<SearchSource>,
    endpoint: Option<String>,
    json: bool,
}

#[derive(Debug)]
struct FetchArgs {
    input: String,
    headers: Vec<(String, String)>,
    body: bool,
    markdown: bool,
    text: bool,
    outline: bool,
    select: Option<String>,
    row: Option<String>,
    table: bool,
    limit: Option<usize>,
    budget: Option<usize>,
    json: bool,
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum Extraction {
    Body,
    Markdown,
    Text,
    Outline,
    Select,
    Row,
    Table,
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

async fn run_fetch(args: FetchArgs) -> Result<(), String> {
    let extraction = pick_extraction(&args)?;
    let report = if args.input.starts_with("http://") || args.input.starts_with("https://") {
        fetch::fetch(&args.input, &args.headers)
            .await
            .map_err(|error| error.to_string())?
    } else {
        fetch::read_source(&args.input).map_err(|error| error.to_string())?
    };

    if extraction == Extraction::Body {
        eprintln!("darash fetch: {}", report.summary());
        println!("{}", report.body);
        return Ok(());
    }
    if extraction == Extraction::Markdown || extraction == Extraction::Text {
        let rendered = if extraction == Extraction::Markdown {
            fetch::to_markdown(&report.body)
        } else {
            fetch::to_text(&report.body)
        };
        eprintln!("darash fetch: {}", report.summary());
        emit_document(rendered, &report, args.budget, args.json);
        return Ok(());
    }

    let (items, data) = match extraction {
        Extraction::Outline => {
            let entries = fetch::outline(&report.body);
            let items = entries
                .iter()
                .map(|entry| {
                    format!(
                        "{}\t{}\t{}",
                        entry.selector,
                        entry.count,
                        entry.sample.as_deref().unwrap_or_default()
                    )
                })
                .collect::<Vec<_>>();
            let data = serde_json::to_value(&entries).map_err(|error| error.to_string())?;
            (items, data)
        }
        Extraction::Select => {
            let selector = args.select.as_deref().expect("select mode has a selector");
            let texts =
                fetch::select_texts(&report.body, selector).map_err(|error| error.to_string())?;
            let data = serde_json::to_value(&texts).map_err(|error| error.to_string())?;
            (texts, data)
        }
        Extraction::Row => {
            let container = args.select.as_deref().expect("row mode has a container");
            let spec = args.row.as_deref().expect("row mode has a spec");
            let rows =
                fetch::rows(&report.body, container, spec).map_err(|error| error.to_string())?;
            let header = rows
                .first()
                .map(|row| {
                    row.iter()
                        .map(|field| field.name.clone())
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default();
            let items = if rows.is_empty() {
                Vec::new()
            } else {
                std::iter::once(header.join("\t"))
                    .chain(rows.iter().map(|row| {
                        row.iter()
                            .map(|field| field.value.clone())
                            .collect::<Vec<_>>()
                            .join("\t")
                    }))
                    .collect::<Vec<_>>()
            };
            let objects = rows
                .iter()
                .map(|row| {
                    row.iter()
                        .fold(serde_json::Map::new(), |mut object, field| {
                            object.insert(field.name.clone(), json!(field.value));
                            object
                        })
                })
                .collect::<Vec<_>>();
            let data = serde_json::Value::Array(
                objects.into_iter().map(serde_json::Value::Object).collect(),
            );
            (items, data)
        }
        Extraction::Table => {
            let tables = fetch::tables(&report.body);
            let items = tables.iter().map(table_to_text).collect::<Vec<_>>();
            let data = serde_json::to_value(&tables).map_err(|error| error.to_string())?;
            (items, data)
        }
        Extraction::Body | Extraction::Markdown | Extraction::Text => unreachable!(),
    };

    let limit = args.limit.unwrap_or(DEFAULT_FETCH_LIMIT);
    let (limited, limit_omitted) = cap(items, limit);
    let budgeted = fetch::apply_budget(limited, args.budget);
    let omitted = limit_omitted + budgeted.omitted;

    eprintln!("darash fetch: {}", report.summary());
    if omitted > 0 {
        eprintln!(
            "darash fetch: {omitted} item(s) omitted; raise --limit/--budget or narrow the selector"
        );
    }
    if args.json {
        emit_json(data, &report, budgeted.items.len(), omitted);
    } else {
        emit_plain(&budgeted.items);
    }
    Ok(())
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

fn emit_json(data: serde_json::Value, report: &FetchReport, count: usize, omitted: usize) {
    let mut meta = serde_json::to_value(report).expect("report serializes");
    if let serde_json::Value::Object(map) = &mut meta {
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

fn table_to_text(table: &fetch::Table) -> String {
    let mut lines = vec![table.headers.join("\t")];
    lines.extend(table.rows.iter().map(|row| row.join("\t")));
    lines.join("\n")
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
        if args.body || args.markdown || args.text || args.outline || args.table {
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
        .unwrap_or(Extraction::Body))
}

fn parse_args(args: &[String]) -> Result<Option<Command>, String> {
    match args.first().map(String::as_str) {
        Some("search") => parse_search_args(&args[1..]).map(|parsed| parsed.map(Command::Search)),
        Some("fetch") => parse_fetch_args(&args[1..]).map(|parsed| parsed.map(Command::Fetch)),
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
    let mut parsed = FetchArgs {
        input: String::new(),
        headers: Vec::new(),
        body: false,
        markdown: false,
        text: false,
        outline: false,
        select: None,
        row: None,
        table: false,
        limit: None,
        budget: None,
        json: false,
    };

    let mut index = 0;
    while index < args.len() {
        let arg = args[index].as_str();
        index += 1;
        match arg {
            "--body" => parsed.body = true,
            "--md" => parsed.markdown = true,
            "--text" => parsed.text = true,
            "--outline" => parsed.outline = true,
            "--select" => {
                parsed.select = Some(next_value(args, &mut index, "--select")?.to_owned())
            }
            "--row" => parsed.row = Some(next_value(args, &mut index, "--row")?.to_owned()),
            "--table" => parsed.table = true,
            "--limit" => {
                parsed.limit = Some(parse_usize(
                    next_value(args, &mut index, "--limit")?,
                    "--limit",
                )?);
            }
            "--budget" => {
                parsed.budget = Some(parse_usize(
                    next_value(args, &mut index, "--budget")?,
                    "--budget",
                )?);
            }
            "--header" => {
                parsed
                    .headers
                    .push(parse_header(next_value(args, &mut index, "--header")?)?);
            }
            "--json" => parsed.json = true,
            "--help" | "-h" => return Ok(None),
            value if value.starts_with('-') => return Err(format!("unknown option: {value}")),
            value => {
                if !parsed.input.is_empty() {
                    return Err(format!("unexpected argument: {value}"));
                }
                parsed.input = value.to_owned();
            }
        }
    }

    if parsed.input.is_empty() {
        return Err("fetch requires a URL or a local file".to_owned());
    }
    Ok(Some(parsed))
}

fn next_value<'a>(args: &'a [String], index: &mut usize, flag: &str) -> Result<&'a str, String> {
    let value = args
        .get(*index)
        .ok_or_else(|| format!("{flag} requires a value"))?;
    *index += 1;
    Ok(value)
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
  darash fetch <url|file> [--md] [--text] [--outline] [--select CSS] [--row name=sel,url=a@href] [--table] [--body] [--limit N] [--budget N] [--header 'Name: value'] [--json]"
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
        let command = parse_args(&args(&[
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
        ]))
        .expect("valid arguments")
        .expect("not help");

        let Command::Fetch(fetch) = command else {
            panic!("expected fetch command");
        };
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
        let command = parse_args(&args(&[
            "fetch",
            "page.html",
            "--select",
            ".item",
            "--row",
            "title=h2, url=a@href",
        ]))
        .expect("valid arguments")
        .expect("not help");

        let Command::Fetch(fetch) = command else {
            panic!("expected fetch command");
        };
        assert_eq!(fetch.input, "page.html");
        assert_eq!(fetch.select.as_deref(), Some(".item"));
        assert_eq!(fetch.row.as_deref(), Some("title=h2, url=a@href"));
        assert_eq!(pick_extraction(&fetch).expect("row mode"), Extraction::Row);
    }

    #[test]
    fn fetch_defaults_to_body_report() {
        let command = parse_args(&args(&["fetch", "page.html"]))
            .expect("valid arguments")
            .expect("not help");
        let Command::Fetch(fetch) = command else {
            panic!("expected fetch command");
        };
        assert_eq!(
            pick_extraction(&fetch).expect("body mode"),
            Extraction::Body
        );
    }

    #[test]
    fn conflicting_extraction_modes_are_rejected() {
        let command = parse_args(&args(&["fetch", "page.html", "--md", "--outline"]))
            .expect("parses")
            .expect("not help");
        let Command::Fetch(fetch) = command else {
            panic!("expected fetch command");
        };
        let error = pick_extraction(&fetch).expect_err("conflict is rejected");
        assert!(error.contains("choose one extraction mode"));
    }

    #[test]
    fn row_mode_requires_a_container() {
        let command = parse_args(&args(&["fetch", "page.html", "--row", "title=h2"]))
            .expect("parses")
            .expect("not help");
        let Command::Fetch(fetch) = command else {
            panic!("expected fetch command");
        };
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
}
