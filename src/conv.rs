//! Converts between JSON, YAML and TOML, in any direction.

use crate::{json, utils};
use clap::{Parser, ValueEnum};
use serde_json::{Map, Number, Value};
use std::fs;
use std::io::{self, IsTerminal};
use std::path::Path;
use std::process::exit;
use yaml_rust2::{Yaml, YamlLoader};

#[derive(Parser, Debug)]
#[command(after_help = "\
Examples:
  rtools conv config.yaml --to toml        print it as TOML
  rtools conv config.yaml config.toml      write a file; its extension picks the format
  kubectl get deploy api -o json | rtools conv --to yaml

The input format comes from the file's extension, or is detected. YAML anchors and
merge keys (<<) are resolved. Several YAML documents become a JSON array.
TOML has no null and needs a table at the top level.")]
pub struct Args {
    /// File to convert. If omitted, reads from stdin (pipe)
    input: Option<String>,

    /// File to write. Its extension picks the format
    output: Option<String>,

    /// Format to convert to
    #[arg(short, long, value_enum)]
    to: Option<Format>,

    /// Format of the input [default: from the file's extension, or detected]
    #[arg(short, long, value_enum)]
    from: Option<Format>,

    /// Print JSON on one line
    #[arg(short, long)]
    minify: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, ValueEnum)]
enum Format {
    Json,
    #[value(alias = "yml")]
    Yaml,
    Toml,
}

impl Format {
    fn of_path(path: &str) -> Option<Format> {
        let extension = Path::new(path).extension()?.to_str()?.to_ascii_lowercase();
        match extension.as_str() {
            "json" => Some(Format::Json),
            "yaml" | "yml" => Some(Format::Yaml),
            "toml" => Some(Format::Toml),
            _ => None,
        }
    }

    fn name(self) -> &'static str {
        match self {
            Format::Json => "JSON",
            Format::Yaml => "YAML",
            Format::Toml => "TOML",
        }
    }
}

pub fn run(args: &Args, copy: bool) {
    if let Err(e) = convert_args(args, copy) {
        eprintln!("Error: {e}");
        exit(1);
    }
}

fn convert_args(args: &Args, copy: bool) -> Result<(), String> {
    let out_format = match (args.to, args.output.as_deref().map(Format::of_path)) {
        (Some(to), Some(Some(of_file))) if to != of_file => {
            return Err(format!(
                "--to {} doesn't match the output file's extension",
                to.name()
            ));
        }
        (Some(to), _) => to,
        (None, Some(Some(of_file))) => of_file,
        (None, Some(None)) => {
            return Err(
                "can't tell the format from the output file's name. Add --to json, yaml or toml"
                    .to_string(),
            );
        }
        (None, None) => {
            return Err(
                "say what to convert to: --to json, yaml or toml, or an output file like out.toml"
                    .to_string(),
            );
        }
    };

    if args.input.is_none() && io::stdin().is_terminal() {
        return Err("nothing to convert: pass a file or pipe it in".to_string());
    }
    let text = utils::read_stdin_or_file(&args.input)?;
    let known = args
        .from
        .or_else(|| args.input.as_deref().and_then(Format::of_path));
    // An empty file is an empty TOML table or an empty YAML document, but not JSON.
    if text.trim().is_empty() && !matches!(known, Some(Format::Toml | Format::Yaml)) {
        return Err("the input is empty".to_string());
    }
    let in_format = known.unwrap_or_else(|| detect(&text));

    let docs = parse(&text, in_format)?;
    let converted = write(&docs, out_format, args.minify)?;
    for warning in &converted.warnings {
        eprintln!("Warning: {warning}");
    }
    match &args.output {
        Some(path) => fs::write(path, format!("{}\n", converted.text))
            .map_err(|e| format!("can't write {path}: {e}")),
        None => utils::emit(&converted.text, copy),
    }
}

/// Guesses the format of text piped in without a file name.
fn detect(text: &str) -> Format {
    let start = text.trim_start();
    if start.starts_with('{') || serde_json::from_str::<Value>(text).is_ok() {
        return Format::Json;
    }
    if toml::from_str::<toml::Table>(text).is_ok() {
        return Format::Toml;
    }
    // Broken input still gets the error of the format it looks like.
    let first = start.lines().next().unwrap_or("");
    let toml_like = start.starts_with("[[")
        || (first.starts_with('[') && first.trim_end().ends_with(']') && !first.contains(','))
        || first.split_once('=').is_some_and(|(key, _)| {
            !key.trim().is_empty() && !key.contains(':') && !key.trim().contains(' ')
        });
    if toml_like {
        Format::Toml
    } else if start.starts_with('[') {
        Format::Json
    } else {
        Format::Yaml
    }
}

// ---------------------------------------------------------------------------
// Reading

/// The documents in the input; only YAML can hold more than one.
fn parse(text: &str, format: Format) -> Result<Vec<Value>, String> {
    match format {
        Format::Json => Ok(vec![json::parse(text)?]),
        Format::Toml => {
            let table: toml::Table = toml::from_str(text)
                .map_err(|e| format!("invalid TOML: {}", e.to_string().trim_end()))?;
            Ok(vec![from_toml(toml::Value::Table(table))])
        }
        Format::Yaml => {
            let docs = YamlLoader::load_from_str(text).map_err(|e| format!("invalid YAML: {e}"))?;
            let mut values = docs
                .iter()
                .enumerate()
                .map(|(i, doc)| {
                    let path = if docs.len() > 1 {
                        format!("document {}", i + 1)
                    } else {
                        String::new()
                    };
                    match doc {
                        // An empty document.
                        Yaml::BadValue => Ok(Value::Null),
                        doc => from_yaml(doc, &path),
                    }
                })
                .collect::<Result<Vec<_>, _>>()?;
            // `---` at the very end leaves an empty document behind.
            if values.len() > 1 {
                values.retain(|v| !v.is_null());
            }
            Ok(values)
        }
    }
}

fn from_toml(value: toml::Value) -> Value {
    match value {
        toml::Value::String(s) => Value::String(s),
        toml::Value::Integer(i) => i.into(),
        toml::Value::Float(f) => Number::from_f64(f).map_or(Value::Null, Value::Number),
        toml::Value::Boolean(b) => Value::Bool(b),
        // JSON and YAML have no dates, so they become strings.
        toml::Value::Datetime(d) => Value::String(d.to_string()),
        toml::Value::Array(items) => Value::Array(items.into_iter().map(from_toml).collect()),
        toml::Value::Table(table) => {
            Value::Object(table.into_iter().map(|(k, v)| (k, from_toml(v))).collect())
        }
    }
}

fn from_yaml(yaml: &Yaml, path: &str) -> Result<Value, String> {
    let at = |what: &str| {
        if path.is_empty() {
            what.to_string()
        } else {
            format!("{what} at {path}")
        }
    };
    Ok(match yaml {
        Yaml::Null => Value::Null,
        Yaml::BadValue => return Err(at("a value that doesn't match its tag")),
        Yaml::Boolean(b) => Value::Bool(*b),
        Yaml::Integer(i) => (*i).into(),
        Yaml::Real(text) => {
            let number = yaml
                .as_f64()
                .and_then(Number::from_f64)
                .ok_or_else(|| at(&format!("{text}: JSON and TOML have no infinity or NaN")))?;
            Value::Number(number)
        }
        Yaml::String(s) => Value::String(s.clone()),
        Yaml::Array(items) => Value::Array(
            items
                .iter()
                .enumerate()
                .map(|(i, item)| from_yaml(item, &format!("{path}[{i}]")))
                .collect::<Result<_, _>>()?,
        ),
        Yaml::Hash(hash) => {
            // Keys written out beat keys merged in with `<<`, wherever they are.
            let explicit: Vec<String> = hash
                .keys()
                .filter(|k| k.as_str() != Some("<<"))
                .map(|k| yaml_key(k, path))
                .collect::<Result<_, _>>()?;
            let mut map = Map::new();
            for (key, value) in hash {
                if key.as_str() == Some("<<") {
                    let sources = match value {
                        Yaml::Array(items) => items.iter().collect(),
                        other => vec![other],
                    };
                    for source in sources {
                        let Yaml::Hash(_) = source else {
                            return Err(at("a << merge key that isn't a mapping"));
                        };
                        if let Value::Object(merged) = from_yaml(source, path)? {
                            for (k, v) in merged {
                                if !explicit.contains(&k) && !map.contains_key(&k) {
                                    map.insert(k, v);
                                }
                            }
                        }
                    }
                    continue;
                }
                let key = yaml_key(key, path)?;
                let child = if path.is_empty() {
                    key.clone()
                } else {
                    format!("{path}.{key}")
                };
                map.insert(key, from_yaml(value, &child)?);
            }
            Value::Object(map)
        }
        Yaml::Alias(_) => return Err(at("an alias to an anchor that isn't defined")),
    })
}

/// JSON and TOML keys are strings; YAML also allows numbers and booleans.
fn yaml_key(key: &Yaml, path: &str) -> Result<String, String> {
    match key {
        Yaml::String(s) | Yaml::Real(s) => Ok(s.clone()),
        Yaml::Integer(i) => Ok(i.to_string()),
        Yaml::Boolean(b) => Ok(b.to_string()),
        Yaml::Null => Ok("null".to_string()),
        _ => Err(format!(
            "a key that is a list or mapping{}: JSON and TOML keys are text",
            if path.is_empty() {
                String::new()
            } else {
                format!(" at {path}")
            }
        )),
    }
}

// ---------------------------------------------------------------------------
// Writing

#[derive(Debug)]
struct Converted {
    text: String,
    warnings: Vec<String>,
}

fn write(docs: &[Value], format: Format, minify: bool) -> Result<Converted, String> {
    let mut warnings = Vec::new();
    let text = match format {
        Format::Json => {
            let value = match docs {
                [doc] => doc.clone(),
                docs => {
                    warnings.push(format!(
                        "the {} YAML documents became a JSON array",
                        docs.len()
                    ));
                    Value::Array(docs.to_vec())
                }
            };
            if minify {
                value.to_string()
            } else {
                serde_json::to_string_pretty(&value).expect("JSON values always serialize")
            }
        }
        Format::Yaml => docs
            .iter()
            .map(to_yaml_text)
            .collect::<Vec<_>>()
            .join("\n---\n"),
        Format::Toml => {
            let [doc] = docs else {
                return Err(format!(
                    "TOML holds one document, and this has {}",
                    docs.len()
                ));
            };
            let Value::Object(map) = doc else {
                return Err(format!(
                    "TOML needs a table at the top level, and this is {}",
                    describe(doc)
                ));
            };
            let mut dropped = Vec::new();
            let table = to_toml_table(map, "", &mut dropped)?;
            if !dropped.is_empty() {
                warnings.push(format!(
                    "TOML has no null, so these were left out: {}",
                    dropped.join(", ")
                ));
            }
            toml::to_string_pretty(&table)
                .map_err(|e| format!("can't write TOML: {e}"))?
                .trim_end()
                .to_string()
        }
    };
    Ok(Converted { text, warnings })
}

fn describe(value: &Value) -> &'static str {
    match value {
        Value::Null => "null",
        Value::Bool(_) => "a boolean",
        Value::Number(_) => "a number",
        Value::String(_) => "a string",
        Value::Array(_) => "an array",
        Value::Object(_) => "an object",
    }
}

fn to_toml_table(
    map: &Map<String, Value>,
    path: &str,
    dropped: &mut Vec<String>,
) -> Result<toml::Table, String> {
    let mut table = toml::Table::new();
    for (key, value) in map {
        let child = if path.is_empty() {
            key.clone()
        } else {
            format!("{path}.{key}")
        };
        if value.is_null() {
            dropped.push(child);
            continue;
        }
        table.insert(key.clone(), to_toml(value, &child, dropped)?);
    }
    Ok(table)
}

fn to_toml(value: &Value, path: &str, dropped: &mut Vec<String>) -> Result<toml::Value, String> {
    Ok(match value {
        Value::Null => {
            return Err(format!(
                "TOML has no null, and a list can't leave one out: {path}"
            ));
        }
        Value::Bool(b) => toml::Value::Boolean(*b),
        Value::Number(n) => match (n.as_i64(), n.as_f64()) {
            (Some(i), _) => toml::Value::Integer(i),
            (None, _) if n.is_u64() => {
                return Err(format!(
                    "{n} at {path} is too big for a TOML integer, which stops at 9223372036854775807"
                ));
            }
            (None, Some(f)) => toml::Value::Float(f),
            (None, None) => return Err(format!("{n} at {path} isn't a number TOML can hold")),
        },
        Value::String(s) => toml::Value::String(s.clone()),
        Value::Array(items) => toml::Value::Array(
            items
                .iter()
                .enumerate()
                .map(|(i, item)| to_toml(item, &format!("{path}[{i}]"), dropped))
                .collect::<Result<_, _>>()?,
        ),
        Value::Object(map) => toml::Value::Table(to_toml_table(map, path, dropped)?),
    })
}

/// YAML that both YAML 1.2 and older 1.1 readers (PyYAML, go-yaml v2) read back
/// as the same data: anything that could pass for a number, boolean, null or
/// date is quoted.
fn to_yaml_text(value: &Value) -> String {
    let mut out = String::new();
    match value {
        Value::Object(map) if !map.is_empty() => yaml_map(map, 0, &mut out),
        Value::Array(items) if !items.is_empty() => yaml_list(items, 0, &mut out),
        Value::String(s) if s.contains('\n') && literal_ok(s) => {
            out.push_str(&yaml_literal(s, 0));
        }
        scalar => {
            out.push_str(&yaml_inline(scalar));
            out.push('\n');
        }
    }
    out.trim_end_matches('\n').to_string()
}

fn yaml_map(map: &Map<String, Value>, indent: usize, out: &mut String) {
    for (key, value) in map {
        out.push_str(&" ".repeat(indent));
        out.push_str(&yaml_string(key));
        out.push(':');
        yaml_after(value, indent, out);
    }
}

fn yaml_list(items: &[Value], indent: usize, out: &mut String) {
    for item in items {
        out.push_str(&" ".repeat(indent));
        out.push('-');
        match item {
            // `- key: value` and `- - item`, with the rest lined up below.
            Value::Object(map) if !map.is_empty() => {
                let mut nested = String::new();
                yaml_map(map, indent + 2, &mut nested);
                out.push(' ');
                out.push_str(&nested[indent + 2..]);
            }
            Value::Array(inner) if !inner.is_empty() => {
                let mut nested = String::new();
                yaml_list(inner, indent + 2, &mut nested);
                out.push(' ');
                out.push_str(&nested[indent + 2..]);
            }
            other => yaml_after(other, indent, out),
        }
    }
}

/// What follows `key:` or `-`.
fn yaml_after(value: &Value, indent: usize, out: &mut String) {
    match value {
        Value::Object(map) if !map.is_empty() => {
            out.push('\n');
            yaml_map(map, indent + 2, out);
        }
        Value::Array(items) if !items.is_empty() => {
            out.push('\n');
            yaml_list(items, indent + 2, out);
        }
        Value::String(s) if s.contains('\n') && literal_ok(s) => {
            out.push(' ');
            out.push_str(&yaml_literal(s, indent + 2));
        }
        scalar => {
            out.push(' ');
            out.push_str(&yaml_inline(scalar));
            out.push('\n');
        }
    }
}

fn yaml_inline(value: &Value) -> String {
    match value {
        Value::Null => "null".to_string(),
        Value::Bool(b) => b.to_string(),
        Value::Number(n) if n.is_f64() => yaml_float(&n.to_string()),
        Value::Number(n) => n.to_string(),
        Value::String(s) => yaml_string(s),
        Value::Array(_) => "[]".to_string(),
        Value::Object(_) => "{}".to_string(),
    }
}

/// Floats with a decimal point and a signed exponent, as YAML 1.1 requires:
/// 1e-7 becomes 1.0e-7.
fn yaml_float(text: &str) -> String {
    let Some((mantissa, exponent)) = text.split_once(['e', 'E']) else {
        return text.to_string();
    };
    let mantissa = if mantissa.contains('.') {
        mantissa.to_string()
    } else {
        format!("{mantissa}.0")
    };
    let exponent = if exponent.starts_with(['-', '+']) {
        exponent.to_string()
    } else {
        format!("+{exponent}")
    };
    format!("{mantissa}e{exponent}")
}

/// A literal block keeps text with line breaks readable; it can't hold text
/// that starts with a space, or other control characters.
fn literal_ok(s: &str) -> bool {
    !s.starts_with([' ', '\t', '\n'])
        && !s.chars().any(|c| c.is_control() && c != '\n' && c != '\t')
        && !s.lines().any(|line| line.ends_with([' ', '\t']))
}

fn yaml_literal(s: &str, indent: usize) -> String {
    let body = s.trim_end_matches('\n');
    let chomp = match s.len() - body.len() {
        0 => "-",
        1 => "",
        _ => "+",
    };
    let mut out = format!("|{chomp}\n");
    let pad = " ".repeat(indent.max(2));
    for line in s.strip_suffix('\n').unwrap_or(s).split('\n') {
        if !line.is_empty() {
            out.push_str(&pad);
            out.push_str(line);
        }
        out.push('\n');
    }
    out
}

fn yaml_string(s: &str) -> String {
    if plain_ok(s) {
        s.to_string()
    } else {
        // JSON strings are valid double-quoted YAML.
        Value::String(s.to_string()).to_string()
    }
}

/// Whether `s` can go unquoted and still read back as this string.
fn plain_ok(s: &str) -> bool {
    const SPECIAL: [&str; 28] = [
        "y", "yes", "n", "no", "true", "false", "on", "off", "null", "~", ".inf", "-.inf", "+.inf",
        ".nan", "<<", "=", "-", "?", ":", "|", ">", "!", "&", "*", "#", "%", "@", "`",
    ];
    let Some(first) = s.chars().next() else {
        return false;
    };
    let lower = s.to_lowercase();
    // Anything made of digits and number punctuation could read as a number,
    // date, time or IP-like value in some YAML version.
    let numberish = s
        .chars()
        .all(|c| c.is_ascii_digit() || "+-._:eExXoObB".contains(c))
        && s.chars().any(|c| c.is_ascii_digit());
    (first.is_alphanumeric() || first == '/' || first == '_' || first == '.')
        && !first.is_ascii_digit()
        && !numberish
        && !SPECIAL.contains(&lower.as_str())
        && !s.ends_with(' ')
        && s.chars()
            .all(|c| c.is_alphanumeric() || " _-./@+".contains(c))
        && !s.contains(" #")
        && !lower.starts_with("0x")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn convert(text: &str, from: Format, to: Format) -> Result<String, String> {
        write(&parse(text, from)?, to, false).map(|c| c.text)
    }

    #[test]
    fn detects_the_input_format() {
        assert_eq!(detect("{\"a\": 1}"), Format::Json);
        assert_eq!(detect("[1, 2]"), Format::Json);
        assert_eq!(detect("[server]\nport = 1\n"), Format::Toml);
        assert_eq!(detect("[[items]]\nname = \"a\"\n"), Format::Toml);
        assert_eq!(detect("name = \"a\"\n"), Format::Toml);
        assert_eq!(detect("name: a\nlist: [1, 2]\n"), Format::Yaml);
        assert_eq!(detect("- a\n- b\n"), Format::Yaml);
        // Broken input still gets the error of the format it looks like.
        assert_eq!(detect("{\"a\": 1,}"), Format::Json);
        assert_eq!(detect("name = \n"), Format::Toml);
    }

    #[test]
    fn resolves_yaml_anchors_and_merge_keys() {
        let yaml = "\
base: &base {timeout: 30, retries: 3}
db:
  timeout: 60
  <<: *base
  url: x
";
        assert_eq!(
            convert(yaml, Format::Yaml, Format::Json).unwrap(),
            serde_json::to_string_pretty(&serde_json::json!({
                "base": {"timeout": 30, "retries": 3},
                "db": {"timeout": 60, "retries": 3, "url": "x"}
            }))
            .unwrap()
        );
    }

    #[test]
    fn keeps_several_yaml_documents() {
        let docs = parse("a: 1\n---\nb: 2\n---\n", Format::Yaml).unwrap();
        assert_eq!(docs.len(), 2);
        let json = write(&docs, Format::Json, true).unwrap();
        assert_eq!(json.text, r#"[{"a":1},{"b":2}]"#);
        assert_eq!(json.warnings, ["the 2 YAML documents became a JSON array"]);
        assert_eq!(
            write(&docs, Format::Yaml, false).unwrap().text,
            "a: 1\n---\nb: 2"
        );
        assert!(
            write(&docs, Format::Toml, false)
                .unwrap_err()
                .contains("one document")
        );
    }

    #[test]
    fn writes_yaml_that_old_readers_read_the_same() {
        let json = r##"{"a": "yes", "b": "NO", "c": "1.20", "d": "2026-09-28", "e": "0x1F", "f": "", "g": "- x",
            "h": "a: b", "i": "#c", "j": 1e-7, "k": 1e21, "l": "plain text", "m": "سلام", "u": "12:30",
            "o": "multi\nline\n", "p": "no end\nnewline", "q": [], "r": {}, "s": "~", "t": null}"##;
        let expected = "\
a: \"yes\"
b: \"NO\"
c: \"1.20\"
d: \"2026-09-28\"
e: \"0x1F\"
f: \"\"
g: \"- x\"
h: \"a: b\"
i: \"#c\"
j: 1.0e-7
k: 1.0e+21
l: plain text
m: سلام
u: \"12:30\"
o: |
  multi
  line
p: |-
  no end
  newline
q: []
r: {}
s: \"~\"
t: null";
        assert_eq!(convert(json, Format::Json, Format::Yaml).unwrap(), expected);
        // And it reads back as the same data.
        let back = convert(expected, Format::Yaml, Format::Json).unwrap();
        let original: Value = serde_json::from_str(json).unwrap();
        assert_eq!(serde_json::from_str::<Value>(&back).unwrap(), original);
    }

    #[test]
    fn nests_yaml_lists_and_maps() {
        let json = r#"{"items": [{"name": "a", "tags": ["x", "w"]}, [1, 2], "z"]}"#;
        assert_eq!(
            convert(json, Format::Json, Format::Yaml).unwrap(),
            "items:\n  - name: a\n    tags:\n      - x\n      - w\n  - - 1\n    - 2\n  - z"
        );
        assert_eq!(
            convert("[1, 2]", Format::Json, Format::Yaml).unwrap(),
            "- 1\n- 2"
        );
        assert_eq!(convert("\"hi\"", Format::Json, Format::Yaml).unwrap(), "hi");
    }

    #[test]
    fn converts_toml_both_ways() {
        let toml = "\
title = \"app\"
when = 2026-09-28T10:00:00Z

[server]
port = 8080

[[users]]
name = \"sara\"
";
        assert_eq!(
            convert(toml, Format::Toml, Format::Json).unwrap(),
            serde_json::to_string_pretty(&serde_json::json!({
                "title": "app",
                "when": "2026-09-28T10:00:00Z",
                "server": {"port": 8080},
                "users": [{"name": "sara"}]
            }))
            .unwrap()
        );
        let back = convert(
            r#"{"title": "app", "server": {"port": 8080}, "users": [{"name": "sara"}]}"#,
            Format::Json,
            Format::Toml,
        )
        .unwrap();
        assert_eq!(
            back,
            "title = \"app\"\n\n[server]\nport = 8080\n\n[[users]]\nname = \"sara\""
        );
    }

    #[test]
    fn explains_what_toml_cannot_hold() {
        let docs = parse(r#"{"a": 1, "b": null, "c": {"d": null}}"#, Format::Json).unwrap();
        let converted = write(&docs, Format::Toml, false).unwrap();
        assert_eq!(converted.text, "a = 1\n\n[c]");
        assert_eq!(
            converted.warnings,
            ["TOML has no null, so these were left out: b, c.d"]
        );
        let error = |json: &str| convert(json, Format::Json, Format::Toml).unwrap_err();
        assert!(error("[1]").contains("needs a table at the top level"));
        assert!(error(r#"{"a": [1, null]}"#).contains("a[1]"));
        assert!(error(r#"{"n": 18446744073709551615}"#).contains("too big"));
        assert!(convert("a: .inf\n", Format::Yaml, Format::Json).is_err());
    }

    #[test]
    fn reads_yaml_keys_that_are_not_strings() {
        assert_eq!(
            convert("1: a\ntrue: b\n", Format::Yaml, Format::Json).unwrap(),
            "{\n  \"1\": \"a\",\n  \"true\": \"b\"\n}"
        );
        assert!(convert("? [a, b]\n: c\n", Format::Yaml, Format::Json).is_err());
    }

    #[test]
    fn formats_yaml_floats_for_every_reader() {
        assert_eq!(yaml_float("1e-7"), "1.0e-7");
        assert_eq!(yaml_float("1e21"), "1.0e+21");
        assert_eq!(yaml_float("2.5E10"), "2.5e+10");
        assert_eq!(yaml_float("0.25"), "0.25");
    }
}
