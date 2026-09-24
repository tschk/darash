import sys
import re

def main():
    with open('src/bin/darash.rs', 'r') as f:
        content = f.read()

    new_func = """
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
"""

    # We need to replace the old `async fn run_fetch(args: FetchArgs) -> Result<i32, String> { ... }` block
    # It spans from `async fn run_fetch(args: FetchArgs) -> Result<i32, String> {` to the next function definition `async fn fetch_report(`

    start_idx = content.find("async fn run_fetch(args: FetchArgs) -> Result<i32, String> {")
    if start_idx == -1:
        print("Could not find run_fetch start")
        sys.exit(1)

    end_idx = content.find("async fn fetch_report(", start_idx)
    if end_idx == -1:
        print("Could not find fetch_report, looking for the end of run_fetch")
        sys.exit(1)

    # include the newline before `async fn fetch_report`
    while content[end_idx-1].isspace():
        end_idx -= 1

    old_run_fetch = content[start_idx:end_idx]

    content = content[:start_idx] + new_func.strip() + "\n\n" + content[end_idx:]

    with open('src/bin/darash.rs', 'w') as f:
        f.write(content)

if __name__ == '__main__':
    main()
