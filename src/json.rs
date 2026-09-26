use crate::utils;
use clap::Parser;
use serde_json::{Map, Value};
use std::io::{self, IsTerminal};
use std::process::exit;

#[derive(Parser, Debug)]
#[command(after_help = "\
Queries:
  .items[0].name    keys and array indexes (negative ones count from the end)
  items.0.name      the same, without brackets
  .[\"a key\"]        keys with spaces or dots

Examples:
  curl -s https://api.example.com/users | rtools json -q '.[0].email' -r
  rtools json config.json --sort-keys")]
pub struct Args {
    /// JSON file. If omitted, reads from stdin (pipe)
    file: Option<String>,

    /// Print on one line
    #[arg(short, long)]
    minify: bool,

    /// Print only the part at this path, like .items[0].name
    #[arg(short, long, value_name = "PATH")]
    query: Option<String>,

    /// Print a string result without quotes
    #[arg(short, long)]
    raw: bool,

    /// Sort object keys
    #[arg(short, long)]
    sort_keys: bool,
}

pub fn run(args: &Args, copy: bool) {
    if let Err(e) = render(args).and_then(|out| utils::emit(&out, copy)) {
        eprintln!("Error: {}", e);
        exit(1);
    }
}

fn render(args: &Args) -> Result<String, String> {
    if args.file.is_none() && io::stdin().is_terminal() {
        return Err("no JSON given: pass a file or pipe it in".to_string());
    }
    let input = utils::read_stdin_or_file(&args.file)?;
    let mut value = parse(&input)?;
    if let Some(path) = &args.query {
        value = query(&value, path)?.clone();
    }
    if args.sort_keys {
        sort_keys(&mut value);
    }

    Ok(match value {
        Value::String(s) if args.raw => s,
        value if args.minify => value.to_string(),
        value => serde_json::to_string_pretty(&value).expect("JSON values always serialize"),
    })
}

/// Parse `input`, pointing at the exact spot if it isn't valid JSON.
pub fn parse(input: &str) -> Result<Value, String> {
    if input.trim().is_empty() {
        return Err("the input is empty".to_string());
    }
    serde_json::from_str(input).map_err(|e| {
        let message = e.to_string();
        let message = message.split(" at line ").next().unwrap_or(&message);
        if e.line() == 0 {
            return format!("invalid JSON: {message}");
        }
        let text = input.lines().nth(e.line() - 1).unwrap_or("");
        let gutter = e.line().to_string();
        let caret = " ".repeat(text.chars().take(e.column().saturating_sub(1)).count());
        format!(
            "invalid JSON at line {}, column {}: {message}\n  {gutter} | {text}\n  {} | {caret}^",
            e.line(),
            e.column(),
            " ".repeat(gutter.len())
        )
    })
}

#[derive(Debug, PartialEq)]
enum Step {
    Key(String),
    Index(i64),
}

fn parse_path(path: &str) -> Result<Vec<Step>, String> {
    let bad = |why: &str| format!("bad query {path:?}: {why}");
    let mut steps = Vec::new();
    let mut rest = path.trim();
    while !rest.is_empty() {
        if let Some(after) = rest.strip_prefix('[') {
            let end = if let Some(quoted) = after.strip_prefix('"') {
                // A quoted key: find the closing quote that isn't escaped.
                let mut escaped = false;
                let close = quoted
                    .char_indices()
                    .find(|&(_, c)| {
                        let found = c == '"' && !escaped;
                        escaped = c == '\\' && !escaped;
                        found
                    })
                    .map(|(i, _)| i + 2)
                    .ok_or_else(|| bad("unclosed quote"))?;
                if !after[close..].starts_with(']') {
                    return Err(bad("expected ] after the quoted key"));
                }
                let key: String =
                    serde_json::from_str(&after[..close]).map_err(|_| bad("invalid quoted key"))?;
                steps.push(Step::Key(key));
                close
            } else {
                let close = after.find(']').ok_or_else(|| bad("missing ]"))?;
                let index = after[..close]
                    .trim()
                    .parse()
                    .map_err(|_| bad("[...] takes a number or a quoted key"))?;
                steps.push(Step::Index(index));
                close
            };
            rest = &after[end + 1..];
        } else if let Some(after) = rest.strip_prefix('.') {
            rest = after;
        } else {
            let end = rest.find(['.', '[']).unwrap_or(rest.len());
            let key = &rest[..end];
            steps.push(match key.parse() {
                Ok(index) => Step::Index(index),
                Err(_) => Step::Key(key.to_string()),
            });
            rest = &rest[end..];
        }
    }
    Ok(steps)
}

fn query<'a>(value: &'a Value, path: &str) -> Result<&'a Value, String> {
    let mut current = value;
    let mut at = String::new();
    for step in parse_path(path)? {
        let where_ = if at.is_empty() { "." } else { &at };
        current = match (&step, current) {
            (Step::Key(key), Value::Object(map)) => map
                .get(key)
                .ok_or_else(|| format!("no key {key:?} at {where_}"))?,
            // `.items.0` on an object with a "0" key.
            (Step::Index(index), Value::Object(map)) => map
                .get(&index.to_string())
                .ok_or_else(|| format!("no key \"{index}\" at {where_}"))?,
            (Step::Index(index), Value::Array(items)) => {
                let len = items.len() as i64;
                let i = if *index < 0 { len + index } else { *index };
                items
                    .get(usize::try_from(i).unwrap_or(usize::MAX))
                    .ok_or_else(|| {
                        format!("index {index} is out of range at {where_} ({len} items)")
                    })?
            }
            (step, other) => {
                let kind = match other {
                    Value::Null => "null",
                    Value::Bool(_) => "a boolean",
                    Value::Number(_) => "a number",
                    Value::String(_) => "a string",
                    Value::Array(_) => "an array",
                    Value::Object(_) => "an object",
                };
                let step = match step {
                    Step::Key(key) => format!("key {key:?}"),
                    Step::Index(index) => format!("index {index}"),
                };
                return Err(format!("{where_} is {kind}, it has no {step}"));
            }
        };
        match step {
            Step::Key(key) => at.push_str(&format!(".{key}")),
            Step::Index(index) => at.push_str(&format!("[{index}]")),
        }
    }
    Ok(current)
}

fn sort_keys(value: &mut Value) {
    match value {
        Value::Object(map) => {
            let mut entries: Vec<(String, Value)> = std::mem::take(map).into_iter().collect();
            entries.sort_by(|a, b| a.0.cmp(&b.0));
            *map = entries
                .into_iter()
                .map(|(key, mut value)| {
                    sort_keys(&mut value);
                    (key, value)
                })
                .collect::<Map<_, _>>();
        }
        Value::Array(items) => items.iter_mut().for_each(sort_keys),
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn parses_query_paths() {
        use Step::*;
        assert_eq!(parse_path(".").unwrap(), vec![]);
        assert_eq!(
            parse_path(".items[0].name").unwrap(),
            vec![Key("items".into()), Index(0), Key("name".into())]
        );
        assert_eq!(
            parse_path("items.-1").unwrap(),
            vec![Key("items".into()), Index(-1)]
        );
        assert_eq!(
            parse_path(r#".["a.b \"c\""][2]"#).unwrap(),
            vec![Key(r#"a.b "c""#.into()), Index(2)]
        );
        assert!(parse_path(".a[").is_err());
        assert!(parse_path(".a[x]").is_err());
        assert!(parse_path(r#".["open]"#).is_err());
    }

    #[test]
    fn queries_values() {
        let value = json!({"items": [{"name": "a"}, {"name": "b"}], "0": "zero", "n": 1});
        assert_eq!(query(&value, ".items[1].name").unwrap(), "b");
        assert_eq!(query(&value, "items.-1.name").unwrap(), "b");
        assert_eq!(query(&value, ".0").unwrap(), "zero");
        assert_eq!(query(&value, ".").unwrap(), &value);
        assert_eq!(
            query(&value, ".items[5]").unwrap_err(),
            "index 5 is out of range at .items (2 items)"
        );
        assert_eq!(
            query(&value, ".items[0].nope").unwrap_err(),
            "no key \"nope\" at .items[0]"
        );
        assert_eq!(
            query(&value, ".n.x").unwrap_err(),
            ".n is a number, it has no key \"x\""
        );
    }

    #[test]
    fn points_at_invalid_json() {
        let error = parse("{\n  \"a\": 1\n  \"b\": 2\n}").unwrap_err();
        assert_eq!(
            error,
            "invalid JSON at line 3, column 3: expected `,` or `}`\n  3 |   \"b\": 2\n    |   ^"
        );
        assert_eq!(parse(" \n").unwrap_err(), "the input is empty");
        assert!(parse("[1,").unwrap_err().contains("EOF"));
    }

    #[test]
    fn sorts_keys_recursively_and_keeps_arrays_in_order() {
        let mut value = json!({"b": 1, "a": {"d": [3, 1], "c": 2}});
        sort_keys(&mut value);
        assert_eq!(value.to_string(), r#"{"a":{"c":2,"d":[3,1]},"b":1}"#);
    }

    #[test]
    fn preserves_key_order_by_default() {
        let value = parse(r#"{"z": 1, "a": 2}"#).unwrap();
        assert_eq!(value.to_string(), r#"{"z":1,"a":2}"#);
    }
}
