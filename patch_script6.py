import re

with open('src/bin/darash.rs', 'r') as f:
    content = f.read()

# Replace parse_fetch_args definition
pattern = r"fn parse_fetch_args\(args: &\[String\]\) -> Result<Option<FetchArgs>, String> \{.*?\n\n    Ok\(Some\(parsed\)\)\n\}"
new_func = """fn parse_fetch_args(args: &[String]) -> Result<Option<FetchArgs>, String> {
    let mut parsed = FetchArgs::default();

    // Check if "--" exists and separate positional args if it does
    let (lexopt_args, trailing_args) = if let Some(dash_dash) = args.iter().position(|s| s == "--") {
        (&args[..dash_dash], &args[dash_dash + 1..])
    } else {
        (args, &[][..])
    };

    // Note: lexopt expects the first argument to be the program name/binary name and ignores it!
    // Since we pass `args` which are already skipped, we need to prefix a dummy binary name
    // so lexopt doesn't drop the first actual argument.
    let mut args_for_lexopt = vec![std::ffi::OsString::from("dummy_bin_name")];
    args_for_lexopt.extend(lexopt_args.iter().map(|s| std::ffi::OsString::from(s)));
    let mut parser = lexopt::Parser::from_iter(args_for_lexopt);
    use lexopt::prelude::*;

    let mut get_string = || -> Result<String, String> {
        parser.value()
            .map_err(|e| e.to_string())?
            .into_string()
            .map_err(|e| e.to_string_lossy().into_owned())
    };

    while let Some(arg) = parser.next().map_err(|e| e.to_string())? {
        match arg {
            Long("body") => parsed.body = true,
            Long("md") => parsed.markdown = true,
            Long("text") => parsed.text = true,
            Long("outline") => parsed.outline = true,
            Long("table") => parsed.table = true,
            Long("count") => parsed.count = true,
            Long("json") => parsed.json = true,
            Long("json-envelope") => parsed.json_envelope = true,
            Long("insecure") | Short('k') => parsed.insecure = true,
            Long("fail") | Short('f') => parsed.fail = true,
            Long("head") | Short('I') => parsed.head = true,
            Long("fresh") => parsed.fresh = true,
            Long("no-cache") => parsed.no_cache = true,
            Long("render") => parsed.render = true,
            Long("main-content") => parsed.main_content = true,
            Long("browser") => parsed.browser = Some(PathBuf::from(get_string()?)),
            Long("wait-for") => parsed.wait_for = Some(get_string()?),
            Long("main-fallback") => {
                let value = get_string()?;
                if value != "full" {
                    return Err("--main-fallback accepts only full".into());
                }
                parsed.main_fallback_full = true;
            }
            Long("compressed") => {}
            Long("select") => parsed.select = Some(get_string()?),
            Long("row") => parsed.row = Some(get_string()?),
            Long("locate") => parsed.locate = Some(get_string()?),
            Long("where") => parsed.where_ = Some(get_string()?),
            Long("method") | Short('X') => parsed.method = Some(get_string()?),
            Long("user") | Short('u') => {
                parsed.basic_auth = Some(parse_basic_auth(&get_string()?));
            }
            Long("output") | Short('o') => {
                parsed.output = Some(PathBuf::from(get_string()?));
            }
            Long("max-time") | Short('m') => {
                parsed.max_time = Some(parse_seconds(&get_string()?)?);
            }
            Long("max-bytes") => {
                parsed.max_bytes = Some(parse_usize(&get_string()?, "--max-bytes")?);
            }
            Long("limit") => {
                parsed.limit = Some(parse_usize(&get_string()?, "--limit")?);
            }
            Long("budget") => {
                parsed.budget = Some(parse_usize(&get_string()?, "--budget")?);
            }
            Long("offset") => {
                parsed.offset = parse_usize(&get_string()?, "--offset")?;
            }
            Long("header") => {
                parsed.headers.push(parse_header(&get_string()?)?);
            }
            Long("data") | Short('d') => {
                add_data(&mut parsed, &get_string()?, DataMode::Strip)?;
            }
            Long("data-raw") => {
                add_data(&mut parsed, &get_string()?, DataMode::Raw)?;
            }
            Long("data-binary") => {
                add_data(&mut parsed, &get_string()?, DataMode::Binary)?;
            }
            Short('L') | Short('i') | Short('s') | Short('S') => {}
            Long("help") | Short('h') => return Ok(None),
            Value(val) => {
                let val_str = val.into_string().map_err(|e| e.to_string_lossy().into_owned())?;
                if !parsed.input.is_empty() {
                    return Err(format!("unexpected argument: {}", val_str));
                }
                parsed.input = val_str;
            }
            Long(name) => return Err(format!("unknown option: --{name}")),
            Short(name) => return Err(format!("unknown option: -{name}")),
        }
    }

    for val in trailing_args {
        if !parsed.input.is_empty() {
            return Err(format!("unexpected argument: {val}"));
        }
        parsed.input = val.clone();
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
}"""

content = re.sub(pattern, new_func, content, flags=re.DOTALL)
with open('src/bin/darash.rs', 'w') as f:
    f.write(content)
