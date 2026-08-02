use std::env;

use darash::{
    SearchClient, SearchMode, SearchRequest, SearchResponse, SearchSource, DEFAULT_ENDPOINT,
};

struct CliArgs {
    query: String,
    mode: SearchMode,
    sources: Vec<SearchSource>,
    endpoint: Option<String>,
}

#[tokio::main]
async fn main() {
    let args = match parse_args(env::args().skip(1)) {
        Ok(Some(args)) => args,
        Ok(None) => {
            print_usage();
            return;
        }
        Err(error) => {
            eprintln!("{error}");
            print_usage();
            std::process::exit(2);
        }
    };

    if let Err(error) = run(args).await {
        eprintln!("search failed: {error}");
        std::process::exit(1);
    }
}

async fn run(args: CliArgs) -> Result<(), String> {
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
    print_response(&response);
    Ok(())
}

fn parse_args<I>(args: I) -> Result<Option<CliArgs>, String>
where
    I: IntoIterator<Item = String>,
{
    let mut args = args.into_iter();
    match args.next().as_deref() {
        Some("search") => {}
        Some("--help" | "-h") => return Ok(None),
        Some(command) => return Err(format!("unknown command: {command}")),
        None => return Err("missing command".to_owned()),
    }

    let mut query = Vec::new();
    let mut mode = SearchMode::default();
    let mut sources = vec![SearchSource::default()];
    let mut endpoint = None;
    let mut source_set = false;

    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--mode" => {
                mode = parse_mode(&args.next().ok_or("--mode requires a value")?)?;
            }
            "--source" => {
                if !source_set {
                    sources.clear();
                    source_set = true;
                }
                sources.push(parse_source(
                    &args.next().ok_or("--source requires a value")?,
                )?);
            }
            "--url" => {
                endpoint = Some(args.next().ok_or("--url requires a value")?);
            }
            "--help" | "-h" => return Ok(None),
            value if value.starts_with('-') => return Err(format!("unknown option: {value}")),
            value => query.push(value.to_owned()),
        }
    }

    if query.is_empty() {
        return Err("search requires a query".to_owned());
    }

    Ok(Some(CliArgs {
        query: query.join(" "),
        mode,
        sources,
        endpoint,
    }))
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
        let title = if source.title.is_empty() {
            "(untitled)"
        } else {
            &source.title
        };
        println!("{title}\n{}\n{}\n", source.url, source.snippet);
    }
}

fn usage() -> &'static str {
    "Usage: darash search <query> [--mode speed|balanced|quality] [--source web|academic|discussions] [--url endpoint]"
}

fn print_usage() {
    eprintln!("{} (default endpoint: {DEFAULT_ENDPOINT})", usage());
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_search_options_without_environment() {
        let args = parse_args([
            "search".to_owned(),
            "rust".to_owned(),
            "async".to_owned(),
            "--mode".to_owned(),
            "quality".to_owned(),
            "--source".to_owned(),
            "academic".to_owned(),
            "--url".to_owned(),
            "http://localhost:9090".to_owned(),
        ])
        .expect("valid arguments")
        .expect("not help");

        assert_eq!(args.query, "rust async");
        assert_eq!(args.mode, SearchMode::Quality);
        assert_eq!(args.sources, [SearchSource::Academic]);
        assert_eq!(args.endpoint.as_deref(), Some("http://localhost:9090"));
    }

    #[test]
    fn parses_help_without_error() {
        assert!(parse_args(["--help".to_owned()])
            .expect("help is valid")
            .is_none());
        assert!(parse_args(["search".to_owned(), "--help".to_owned()])
            .expect("help is valid")
            .is_none());
    }
}
