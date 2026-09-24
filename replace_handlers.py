import sys

def main():
    with open('src/bin/darash.rs', 'r') as f:
        content = f.read()

    new_funcs = """
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
                "{}\t{}\t{}",
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
"""

    # insert before `validate_fetch_args`
    insert_pos = content.find("fn validate_fetch_args(")
    if insert_pos == -1:
        print("Could not find validate_fetch_args")
        sys.exit(1)

    content = content[:insert_pos] + new_funcs + "\n" + content[insert_pos:]

    with open('src/bin/darash.rs', 'w') as f:
        f.write(content)

if __name__ == '__main__':
    main()
