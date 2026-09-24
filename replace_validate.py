import sys

def main():
    with open('src/bin/darash.rs', 'r') as f:
        content = f.read()

    new_func = """
fn validate_fetch_args(args: &FetchArgs, extraction: Extraction, structured: bool) -> Result<(), String> {
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
"""

    # insert before `build_records`
    insert_pos = content.find("fn build_records(")
    if insert_pos == -1:
        print("Could not find build_records")
        sys.exit(1)

    content = content[:insert_pos] + new_func + "\n" + content[insert_pos:]

    with open('src/bin/darash.rs', 'w') as f:
        f.write(content)

if __name__ == '__main__':
    main()
