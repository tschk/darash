#!/bin/bash

# A quick Python script to do the replacement safely
python3 -c '
import sys

with open("src/bin/darash.rs", "r") as f:
    content = f.read()

# We need to replace the long run_fetch function and the helper methods,
# merging them to avoid the unreachable!() fallback.
# Specifically, we want `handle_document_extraction` and `handle_structured_extraction`
# to just be part of a single enum match on `Extraction` to get exhaustive checking.
# But since the goal was to *reduce* line count in `run_fetch`, we can still
# move the logic for "structured" and "document" to separate functions,
# but dispatch to them directly from an exhaustive match.

start_str = "async fn run_fetch(args: FetchArgs) -> Result<i32, String> {"
end_str = "Ok(fail_code(args, report))\n}"

start_idx = content.find(start_str)
end_idx = content.find(end_str, start_idx) + len(end_str)

new_run_fetch = """async fn run_fetch(args: FetchArgs) -> Result<i32, String> {
    let extraction = pick_extraction(&args)?;
    validate_extraction_args(&args, extraction)?;

    let report = get_fetch_report(&args).await?;

    if let Some(path) = &args.output {
        atomic_write(path, report.body.as_bytes())
            .map_err(|error| format!("cannot write {}: {error}", path.display()))?;
        eprintln!("darash fetch: {}", report.summary());
        return Ok(fail_code(&args, &report));
    }

    match extraction {
        Extraction::Select | Extraction::Row | Extraction::Table | Extraction::Locate => {
            handle_structured_extraction(extraction, &args, &report)
        }
        Extraction::Report
        | Extraction::Body
        | Extraction::Markdown
        | Extraction::Text
        | Extraction::Outline => handle_document_extraction(extraction, &args, &report),
    }
}

fn validate_extraction_args(args: &FetchArgs, extraction: Extraction) -> Result<(), String> {
    validate_page_options(args, extraction)?;
    let structured = matches!(
        extraction,
        Extraction::Select | Extraction::Row | Extraction::Table | Extraction::Locate
    );

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

async fn get_fetch_report(args: &FetchArgs) -> Result<FetchReport, String> {
    let is_url = args.input.starts_with("http://") || args.input.starts_with("https://");
    let method = args.method.clone().unwrap_or_else(|| "GET".to_owned());
    let no_store = args.headers.iter().any(|(name, value)| {
        name.eq_ignore_ascii_case("cache-control")
            && value.to_ascii_lowercase().contains("no-store")
    });
    let cacheable = is_url
        && !args.render
        && !args.no_cache
        && disk_cache::should_cache(
            &args.input,
            &method,
            !args.headers.is_empty(),
            args.data.is_some(),
            no_store,
        );

    if is_url {
        fetch_report(args, &method, cacheable).await
    } else {
        fetch::read_source(&args.input)
            .await
            .map_err(|error| error.to_string())
    }
}

fn handle_document_extraction(
    extraction: Extraction,
    args: &FetchArgs,
    report: &FetchReport,
) -> Result<i32, String> {
    match extraction {
        Extraction::Report => {
            let rendered =
                serde_json::to_string_pretty(&report).map_err(|error| error.to_string())?;
            println!("{rendered}");
            Ok(fail_code(args, report))
        }
        Extraction::Body => {
            eprintln!("darash fetch: {}", report.summary());
            println!("{}", report.body);
            Ok(fail_code(args, report))
        }
        Extraction::Markdown | Extraction::Text => {
            let main = if args.main_content {
                let fallback = if args.main_fallback_full {
                    darash::main_content::MainContentFallback::FullDocument
                } else {
                    darash::main_content::MainContentFallback::Error
                };
                let content = darash::main_content::extract_main_content(&report.body, fallback)
                    .map_err(|error| error.to_string())?;
                eprintln!("darash fetch: content={:?}", content.source);
                Some(content)
            } else {
                None
            };
            let html = main
                .as_ref()
                .map(|content| content.html.as_str())
                .unwrap_or(&report.body);
            let rendered = if extraction == Extraction::Markdown {
                fetch::to_markdown(html)
            } else {
                fetch::to_text(html)
            };
            eprintln!("darash fetch: {}", report.summary());
            emit_document(rendered, report, args.budget, args.json);
            Ok(fail_code(args, report))
        }
        Extraction::Outline => {
            let entries = fetch::outline(&report.body);
            let items = entries
                .iter()
                .map(|entry| {
                    format!(
                        "{}\\t{}\\t{}",
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
        Extraction::Select | Extraction::Row | Extraction::Table | Extraction::Locate => {
            unreachable!("structured extraction passed to document extraction handler")
        }
    }
}

fn handle_structured_extraction(
    extraction: Extraction,
    args: &FetchArgs,
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
}"""

new_content = content[:start_idx] + new_run_fetch + content[end_idx:]

with open("src/bin/darash.rs", "w") as f:
    f.write(new_content)
'
