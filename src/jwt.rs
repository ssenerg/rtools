use crate::utils;
use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD_INDIFFERENT as BASE64URL;
use chrono::{DateTime, Utc};
use chrono_humanize::HumanTime;
use clap::{Args as ClapArgs, Parser, Subcommand, ValueEnum};
use hmac::{EagerHash, Hmac, KeyInit, Mac};
use serde_json::{Map, Value, json};
use sha2::{Sha256, Sha384, Sha512};
use std::fs;
use std::io::{self, IsTerminal, Read};
use std::process::exit;

#[derive(Parser, Debug)]
pub struct Args {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand, Debug)]
enum Command {
    /// Show a token's header, payload and times, and check its signature
    Decode(DecodeArgs),
    /// Create a token signed with a secret key
    Encode(EncodeArgs),
}

#[derive(ClapArgs, Debug)]
#[command(after_help = "With a secret, exits with status 1 unless the signature is valid.")]
struct DecodeArgs {
    /// The token. If omitted, reads from stdin (pipe)
    token: Option<String>,

    /// Check the signature with this secret (HS256, HS384 or HS512)
    #[arg(short, long, value_name = "KEY")]
    secret: Option<String>,

    /// Read the secret from a file, keeping it out of your shell history
    #[arg(long, value_name = "PATH", conflicts_with = "secret")]
    secret_file: Option<String>,

    /// Print only the payload, as JSON
    #[arg(short, long)]
    payload: bool,
}

#[derive(ClapArgs, Debug)]
struct EncodeArgs {
    /// The payload, a JSON object like '{"sub":"42"}'. If omitted, reads from stdin (pipe)
    payload: Option<String>,

    /// Secret key to sign with
    #[arg(
        short,
        long,
        value_name = "KEY",
        required_unless_present = "secret_file"
    )]
    secret: Option<String>,

    /// Read the secret from a file, keeping it out of your shell history
    #[arg(long, value_name = "PATH", conflicts_with = "secret")]
    secret_file: Option<String>,

    /// Signing algorithm
    #[arg(short, long, value_enum, ignore_case = true, default_value_t = Alg::Hs256)]
    alg: Alg,

    /// Set `exp` this far from now: 30s, 15m, 12h, 7d or 2w (negative for an expired token)
    #[arg(short, long, value_name = "DURATION", value_parser = parse_duration, allow_hyphen_values = true)]
    exp: Option<i64>,

    /// Set `iat` (issued at) to now
    #[arg(long)]
    iat: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, ValueEnum)]
enum Alg {
    #[value(name = "HS256")]
    Hs256,
    #[value(name = "HS384")]
    Hs384,
    #[value(name = "HS512")]
    Hs512,
}

impl Alg {
    fn from_name(name: &str) -> Option<Alg> {
        match name {
            "HS256" => Some(Alg::Hs256),
            "HS384" => Some(Alg::Hs384),
            "HS512" => Some(Alg::Hs512),
            _ => None,
        }
    }

    fn name(self) -> &'static str {
        match self {
            Alg::Hs256 => "HS256",
            Alg::Hs384 => "HS384",
            Alg::Hs512 => "HS512",
        }
    }

    fn sign(self, key: &[u8], data: &[u8]) -> Vec<u8> {
        match self {
            Alg::Hs256 => hmac::<Sha256>(key, data),
            Alg::Hs384 => hmac::<Sha384>(key, data),
            Alg::Hs512 => hmac::<Sha512>(key, data),
        }
    }

    /// RFC 7518 wants the key at least as long as the hash.
    fn min_key_len(self) -> usize {
        match self {
            Alg::Hs256 => 32,
            Alg::Hs384 => 48,
            Alg::Hs512 => 64,
        }
    }
}

fn hmac<D: EagerHash>(key: &[u8], data: &[u8]) -> Vec<u8> {
    Hmac::<D>::new_from_slice(key)
        .expect("HMAC takes keys of any length")
        .chain_update(data)
        .finalize()
        .into_bytes()
        .to_vec()
}

pub fn run(args: &Args, copy: bool) {
    let result = match &args.command {
        Command::Decode(args) => decode(args, copy),
        Command::Encode(args) => encode(args, copy),
    };

    if let Err(e) = result {
        eprintln!("Error: {}", e);
        exit(1);
    }
}

fn decode(args: &DecodeArgs, copy: bool) -> Result<(), String> {
    let input = arg_or_stdin(&args.token, "token")?;
    let token = Token::parse(&input)?;
    let secret = read_secret(&args.secret, &args.secret_file)?;
    let signature = secret.as_deref().map(|key| token.verify(key));

    if args.payload {
        if let Some(status) = signature.filter(|s| !matches!(s, Signature::Valid(_))) {
            return Err(format!("signature {}", status.describe()));
        }
        return utils::emit(&pretty(&Value::Object(token.payload.clone())), copy);
    }

    utils::emit(&token.report(signature.as_ref(), Utc::now()), copy)?;
    match signature {
        Some(Signature::Valid(_)) | None => Ok(()),
        Some(_) => exit(1),
    }
}

fn encode(args: &EncodeArgs, copy: bool) -> Result<(), String> {
    let input = arg_or_stdin(&args.payload, "payload")?;
    let mut payload = match serde_json::from_str::<Value>(&input) {
        Ok(Value::Object(payload)) => payload,
        Ok(_) => return Err(r#"the payload must be a JSON object, like {"sub":"42"}"#.to_string()),
        Err(e) => return Err(format!("the payload isn't valid JSON: {e}")),
    };
    let secret = read_secret(&args.secret, &args.secret_file)?
        .ok_or("a secret is required to sign the token")?;
    if secret.len() < args.alg.min_key_len() {
        eprintln!(
            "Warning: {} secrets should be at least {} bytes long, this one has {}",
            args.alg.name(),
            args.alg.min_key_len(),
            secret.len()
        );
    }

    let now = Utc::now().timestamp();
    if args.iat {
        payload.insert("iat".to_string(), json!(now));
    }
    if let Some(exp) = args.exp {
        payload.insert("exp".to_string(), json!(now + exp));
    }

    utils::emit(&sign(&payload, args.alg, &secret), copy)
}

fn sign(payload: &Map<String, Value>, alg: Alg, secret: &[u8]) -> String {
    let header = json!({ "alg": alg.name(), "typ": "JWT" });
    let signing_input = format!(
        "{}.{}",
        BASE64URL.encode(header.to_string()),
        BASE64URL.encode(Value::Object(payload.clone()).to_string())
    );
    let signature = alg.sign(secret, signing_input.as_bytes());
    format!("{signing_input}.{}", BASE64URL.encode(signature))
}

struct Token {
    /// `header.payload`, the part the signature covers.
    signing_input: String,
    header: Map<String, Value>,
    payload: Map<String, Value>,
    signature: Vec<u8>,
}

#[derive(Debug, PartialEq)]
enum Signature {
    Valid(Alg),
    Invalid(Alg),
    /// `alg: none`, or no signature at all.
    Unsigned,
    /// Not an HMAC algorithm, so a secret can't check it.
    NotHmac(String),
}

impl Signature {
    fn describe(&self) -> String {
        match self {
            Signature::Valid(alg) => format!("valid ({})", alg.name()),
            Signature::Invalid(alg) => {
                format!("invalid, it doesn't match the secret ({})", alg.name())
            }
            Signature::Unsigned => "missing, the token is unsigned".to_string(),
            Signature::NotHmac(alg) => {
                format!("{alg} can't be checked with a secret, only HS256, HS384 and HS512 can")
            }
        }
    }
}

impl Token {
    fn parse(input: &str) -> Result<Token, String> {
        let input = input.trim();
        // Accept a pasted `Authorization: Bearer <token>` value too.
        let token = input
            .get(..7)
            .filter(|prefix| prefix.eq_ignore_ascii_case("bearer "))
            .map_or(input, |_| input[7..].trim_start());

        let parts: Vec<&str> = token.split('.').collect();
        let [header, payload, signature] = parts[..] else {
            return Err(match parts.len() {
                5 => {
                    "this is an encrypted token (JWE), only signed ones can be decoded".to_string()
                }
                n => format!("a JWT has 3 parts separated by dots, this has {n}"),
            });
        };

        Ok(Token {
            signing_input: format!("{header}.{payload}"),
            header: decode_object(header, "header")?,
            payload: decode_object(payload, "payload")?,
            signature: BASE64URL
                .decode(signature)
                .map_err(|_| "the signature isn't valid base64url".to_string())?,
        })
    }

    fn alg(&self) -> &str {
        self.header
            .get("alg")
            .and_then(Value::as_str)
            .unwrap_or("none")
    }

    fn verify(&self, secret: &[u8]) -> Signature {
        let alg = self.alg();
        if alg.eq_ignore_ascii_case("none") || self.signature.is_empty() {
            return Signature::Unsigned;
        }
        let Some(alg) = Alg::from_name(alg) else {
            return Signature::NotHmac(alg.to_string());
        };
        let expected = alg.sign(secret, self.signing_input.as_bytes());
        if constant_time_eq(&expected, &self.signature) {
            Signature::Valid(alg)
        } else {
            Signature::Invalid(alg)
        }
    }

    fn report(&self, signature: Option<&Signature>, now: DateTime<Utc>) -> String {
        let mut sections = vec![
            format!(
                "Header\n{}",
                indent(&pretty(&Value::Object(self.header.clone())))
            ),
            format!(
                "Payload\n{}",
                indent(&pretty(&Value::Object(self.payload.clone())))
            ),
        ];

        let times: Vec<String> = [
            ("iat", "Issued at"),
            ("nbf", "Not before"),
            ("exp", "Expires"),
        ]
        .into_iter()
        .filter_map(|(claim, label)| {
            let value = self.payload.get(claim)?;
            let label = match claim {
                "exp" if expired(value, now) => "Expired",
                _ => label,
            };
            Some(format!("  {label:<11}{}", describe_time(value, now)))
        })
        .collect();
        if !times.is_empty() {
            sections.push(format!("Times\n{}", times.join("\n")));
        }

        let status = match signature {
            Some(signature) => signature.describe(),
            None if self.signature.is_empty() || self.alg().eq_ignore_ascii_case("none") => {
                Signature::Unsigned.describe()
            }
            None => match Alg::from_name(self.alg()) {
                Some(alg) => format!("not checked, pass --secret to verify it ({})", alg.name()),
                None => Signature::NotHmac(self.alg().to_string()).describe(),
            },
        };
        sections.push(format!("Signature\n  {status}"));

        sections.join("\n\n")
    }
}

fn decode_object(part: &str, name: &str) -> Result<Map<String, Value>, String> {
    let bytes = BASE64URL
        .decode(part)
        .map_err(|_| format!("the {name} isn't valid base64url"))?;
    match serde_json::from_slice(&bytes) {
        Ok(Value::Object(object)) => Ok(object),
        Ok(_) => Err(format!("the {name} isn't a JSON object")),
        Err(e) => Err(format!("the {name} isn't valid JSON: {e}")),
    }
}

/// NumericDate claims are seconds since the epoch, possibly fractional.
fn timestamp(value: &Value) -> Option<DateTime<Utc>> {
    DateTime::from_timestamp(value.as_f64()? as i64, 0)
}

fn expired(exp: &Value, now: DateTime<Utc>) -> bool {
    timestamp(exp).is_some_and(|exp| exp <= now)
}

fn describe_time(value: &Value, now: DateTime<Utc>) -> String {
    match timestamp(value) {
        Some(time) => format!(
            "{} ({})",
            time.format("%Y-%m-%d %H:%M:%S UTC"),
            HumanTime::from(time - now)
        ),
        None => format!("{value} (not a timestamp)"),
    }
}

fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    a.len() == b.len() && a.iter().zip(b).fold(0, |diff, (x, y)| diff | (x ^ y)) == 0
}

fn pretty(value: &Value) -> String {
    serde_json::to_string_pretty(value).expect("JSON values always serialize")
}

fn indent(text: &str) -> String {
    text.lines()
        .map(|line| format!("  {line}"))
        .collect::<Vec<_>>()
        .join("\n")
}

/// `90` or `90s`, `15m`, `12h`, `7d`, `2w`; a leading `-` means in the past.
fn parse_duration(input: &str) -> Result<i64, String> {
    let (number, unit) = match input.find(|c: char| c.is_ascii_alphabetic()) {
        Some(at) => input.split_at(at),
        None => (input, "s"),
    };
    let seconds = match unit {
        "s" => 1,
        "m" => 60,
        "h" => 60 * 60,
        "d" => 24 * 60 * 60,
        "w" => 7 * 24 * 60 * 60,
        _ => return Err(format!("unknown unit {unit:?}, use s, m, h, d or w")),
    };
    let number: i64 = number
        .parse()
        .map_err(|_| format!("{input:?} isn't a duration like 30s, 15m, 12h or 7d"))?;
    number
        .checked_mul(seconds)
        .ok_or_else(|| format!("{input:?} is too long"))
}

fn read_secret(secret: &Option<String>, file: &Option<String>) -> Result<Option<Vec<u8>>, String> {
    let secret = match (secret, file) {
        (Some(secret), _) => secret.clone().into_bytes(),
        (None, Some(path)) => {
            let mut contents = fs::read(path)
                .map_err(|e| format!("failed to read the secret from {path}: {e}"))?;
            // Editors end files with a newline that isn't part of the secret.
            while contents.last().is_some_and(|b| *b == b'\n' || *b == b'\r') {
                contents.pop();
            }
            contents
        }
        (None, None) => return Ok(None),
    };
    if secret.is_empty() {
        return Err("the secret is empty".to_string());
    }
    Ok(Some(secret))
}

fn arg_or_stdin(arg: &Option<String>, what: &str) -> Result<String, String> {
    if let Some(arg) = arg {
        return Ok(arg.clone());
    }
    if io::stdin().is_terminal() {
        return Err(format!(
            "no {what} given: pass it as an argument or pipe it in"
        ));
    }
    let mut input = String::new();
    io::stdin()
        .read_to_string(&mut input)
        .map_err(|e| format!("failed to read stdin: {e}"))?;
    Ok(input)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The example on jwt.io.
    const JWT_IO: &str = "eyJhbGciOiJIUzI1NiIsInR5cCI6IkpXVCJ9.eyJzdWIiOiIxMjM0NTY3ODkwIiwibmFtZSI6IkpvaG4gRG9lIiwiaWF0IjoxNTE2MjM5MDIyfQ.SflKxwRJSMeKKF2QT4fwpMeJf36POk6yJV_adQssw5c";
    const JWT_IO_SECRET: &[u8] = b"your-256-bit-secret";

    fn object(json: &str) -> Map<String, Value> {
        match serde_json::from_str(json).unwrap() {
            Value::Object(object) => object,
            _ => panic!("not an object"),
        }
    }

    #[test]
    fn signs_like_jwt_io() {
        let payload = object(r#"{"sub":"1234567890","name":"John Doe","iat":1516239022}"#);
        assert_eq!(sign(&payload, Alg::Hs256, JWT_IO_SECRET), JWT_IO);
    }

    #[test]
    fn verifies_jwt_io_example() {
        let token = Token::parse(JWT_IO).unwrap();
        assert_eq!(token.header, object(r#"{"alg":"HS256","typ":"JWT"}"#));
        assert_eq!(token.payload["name"], "John Doe");
        assert_eq!(token.verify(JWT_IO_SECRET), Signature::Valid(Alg::Hs256));
        assert_eq!(token.verify(b"wrong"), Signature::Invalid(Alg::Hs256));
    }

    #[test]
    fn verifies_rfc_7515_hs256_example() {
        // RFC 7515, appendix A.1: a binary key and JSON with line breaks.
        let key = BASE64URL
            .decode("AyM1SysPpbyDfgZld3umj1qzKObwVMkoqQ-EstJQLr_T-1qS0gZH75aKtMN3Yj0iPS4hcgUuTwjAzZr1Z9CAow")
            .unwrap();
        let token = Token::parse(
            "eyJ0eXAiOiJKV1QiLA0KICJhbGciOiJIUzI1NiJ9.eyJpc3MiOiJqb2UiLA0KICJleHAiOjEzMDA4MTkzODAsDQogImh0dHA6Ly9leGFtcGxlLmNvbS9pc19yb290Ijp0cnVlfQ.dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk",
        )
        .unwrap();
        assert_eq!(token.payload["iss"], "joe");
        assert_eq!(token.verify(&key), Signature::Valid(Alg::Hs256));
    }

    #[test]
    fn round_trips_every_algorithm() {
        let payload = object(r#"{"sub":"42","admin":true}"#);
        for alg in [Alg::Hs256, Alg::Hs384, Alg::Hs512] {
            let token = Token::parse(&sign(&payload, alg, b"s3cret")).unwrap();
            assert_eq!(token.payload, payload);
            assert_eq!(token.alg(), alg.name());
            assert_eq!(token.verify(b"s3cret"), Signature::Valid(alg));
            assert_eq!(token.verify(b"s3cret!"), Signature::Invalid(alg));
        }
    }

    #[test]
    fn rejects_tampering_and_unsigned_tokens() {
        let [header, _, signature] = JWT_IO.split('.').collect::<Vec<_>>()[..] else {
            unreachable!()
        };
        let forged_payload = BASE64URL.encode(r#"{"sub":"1234567890","name":"Admin"}"#);
        let forged = Token::parse(&format!("{header}.{forged_payload}.{signature}")).unwrap();
        assert_eq!(forged.verify(JWT_IO_SECRET), Signature::Invalid(Alg::Hs256));

        // `alg: none` must never pass, whatever the secret.
        let none = format!(
            "{}.{forged_payload}.",
            BASE64URL.encode(r#"{"alg":"none","typ":"JWT"}"#)
        );
        assert_eq!(
            Token::parse(&none).unwrap().verify(JWT_IO_SECRET),
            Signature::Unsigned
        );

        let rs256 = format!(
            "{}.{forged_payload}.{signature}",
            BASE64URL.encode(r#"{"alg":"RS256"}"#)
        );
        assert_eq!(
            Token::parse(&rs256).unwrap().verify(JWT_IO_SECRET),
            Signature::NotHmac("RS256".to_string())
        );
    }

    #[test]
    fn accepts_a_pasted_bearer_header() {
        for input in [format!("Bearer {JWT_IO}"), format!("  bearer   {JWT_IO}\n")] {
            let token = Token::parse(&input).unwrap();
            assert_eq!(token.verify(JWT_IO_SECRET), Signature::Valid(Alg::Hs256));
        }
    }

    #[test]
    fn explains_malformed_tokens() {
        let bad = |input: &str| Token::parse(input).err().unwrap();
        assert!(bad("abc.def").contains("3 parts"));
        assert!(bad("a.b.c.d.e").contains("encrypted"));
        assert!(bad("!!!.e30.").contains("header isn't valid base64url"));
        let not_json = BASE64URL.encode("hello");
        assert!(bad(&format!("e30.{not_json}.")).contains("payload isn't valid JSON"));
        let array = BASE64URL.encode("[1]");
        assert!(bad(&format!("{array}.e30.")).contains("header isn't a JSON object"));
    }

    #[test]
    fn reports_times_relative_to_now() {
        let now = DateTime::from_timestamp(1_700_000_000, 0).unwrap();
        let payload = object(r#"{"iat":1699990000,"exp":1700003600}"#);
        let token = Token::parse(&sign(&payload, Alg::Hs256, b"k")).unwrap();
        let report = token.report(None, now);
        assert!(
            report.contains("Issued at  2023-11-14 19:26:40 UTC"),
            "{report}"
        );
        assert!(
            report.contains("Expires    2023-11-14 23:13:20 UTC (in an hour)"),
            "{report}"
        );
        assert!(report.contains("not checked, pass --secret"), "{report}");

        let later = DateTime::from_timestamp(1_700_010_000, 0).unwrap();
        assert!(
            token
                .report(None, later)
                .contains("Expired    2023-11-14 23:13:20 UTC")
        );
    }

    #[test]
    fn parses_durations() {
        assert_eq!(parse_duration("90"), Ok(90));
        assert_eq!(parse_duration("30s"), Ok(30));
        assert_eq!(parse_duration("15m"), Ok(900));
        assert_eq!(parse_duration("12h"), Ok(43_200));
        assert_eq!(parse_duration("7d"), Ok(604_800));
        assert_eq!(parse_duration("2w"), Ok(1_209_600));
        assert_eq!(parse_duration("-1h"), Ok(-3600));
        assert!(parse_duration("1y").is_err());
        assert!(parse_duration("h").is_err());
        assert!(parse_duration("").is_err());
    }

    #[test]
    fn compares_in_constant_time() {
        assert!(constant_time_eq(b"abc", b"abc"));
        assert!(!constant_time_eq(b"abc", b"abd"));
        assert!(!constant_time_eq(b"abc", b"ab"));
    }
}
