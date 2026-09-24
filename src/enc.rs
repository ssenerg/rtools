use crate::utils;
use base64::Engine;
use base64::engine::general_purpose::{
    STANDARD, STANDARD_NO_PAD_INDIFFERENT, URL_SAFE_NO_PAD, URL_SAFE_NO_PAD_INDIFFERENT,
};
use clap::{Parser, ValueEnum};
use std::fs;
use std::io::{self, IsTerminal, Read, Write};
use std::process::exit;

#[derive(Parser, Debug)]
#[command(after_help = "\
Examples:
  rtools enc base64 'hello world'        aGVsbG8gd29ybGQ=
  rtools enc base64 -d aGVsbG8gd29ybGQ=   hello world
  rtools enc url 'a b&c=d'               a%20b%26c%3Dd
  rtools enc hex -i logo.png             the file's exact bytes

Piped input loses its trailing newline, so `echo hi | rtools enc hex` encodes \"hi\".")]
pub struct Args {
    /// Encoding
    #[arg(value_enum)]
    format: Format,

    /// Text to encode or decode. If omitted, reads from stdin (pipe)
    text: Option<String>,

    /// Decode instead of encode
    #[arg(short, long)]
    decode: bool,

    /// Read the data from a file, byte for byte
    #[arg(short, long, value_name = "FILE", conflicts_with = "text")]
    input: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, ValueEnum)]
enum Format {
    /// Base64 with + and /, padded with =
    Base64,
    /// URL-safe base64 with - and _, unpadded (as in JWTs)
    Base64url,
    /// Hexadecimal
    Hex,
    /// Percent-encoding for URLs and query strings
    Url,
}

pub fn run(args: &Args, copy: bool) {
    if let Err(e) = convert(args, copy) {
        eprintln!("Error: {}", e);
        exit(1);
    }
}

fn convert(args: &Args, copy: bool) -> Result<(), String> {
    let data = read_input(args)?;
    if !args.decode {
        return utils::emit(&encode(args.format, &data), copy);
    }

    let decoded = decode(args.format, &data)?;
    if let Some(text) = as_text(&decoded) {
        return utils::emit(text, copy);
    }
    if io::stdout().is_terminal() || copy {
        return Err(format!(
            "the decoded data isn't text ({} bytes). Redirect it to a file: … > out.bin",
            decoded.len()
        ));
    }
    io::stdout()
        .write_all(&decoded)
        .map_err(|e| format!("failed to write the output: {e}"))
}

/// Printable text: UTF-8 without control characters other than line breaks and tabs.
fn as_text(bytes: &[u8]) -> Option<&str> {
    std::str::from_utf8(bytes).ok().filter(|text| {
        !text
            .chars()
            .any(|c| c.is_control() && !matches!(c, '\n' | '\r' | '\t'))
    })
}

fn read_input(args: &Args) -> Result<Vec<u8>, String> {
    if let Some(text) = &args.text {
        return Ok(text.clone().into_bytes());
    }
    if let Some(path) = &args.input {
        return fs::read(path).map_err(|e| format!("failed to read {path}: {e}"));
    }
    if io::stdin().is_terminal() {
        return Err("nothing to convert: pass text or --input, or pipe it in".to_string());
    }
    let mut data = Vec::new();
    io::stdin()
        .read_to_end(&mut data)
        .map_err(|e| format!("failed to read stdin: {e}"))?;
    // `echo` adds a newline nobody means to encode.
    if data.ends_with(b"\n") {
        data.pop();
        if data.ends_with(b"\r") {
            data.pop();
        }
    }
    Ok(data)
}

fn encode(format: Format, data: &[u8]) -> String {
    match format {
        Format::Base64 => STANDARD.encode(data),
        Format::Base64url => URL_SAFE_NO_PAD.encode(data),
        Format::Hex => utils::hex(data),
        Format::Url => data
            .iter()
            .map(|&b| match b {
                b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                    (b as char).to_string()
                }
                _ => format!("%{b:02X}"),
            })
            .collect(),
    }
}

fn decode(format: Format, data: &[u8]) -> Result<Vec<u8>, String> {
    let text = std::str::from_utf8(data).map_err(|_| format!("that isn't {format:?} text"))?;
    // Wrapped base64 and spaced-out hex are common; spaces mean nothing in either.
    let compact: String = text.chars().filter(|c| !c.is_whitespace()).collect();
    match format {
        Format::Base64 => STANDARD_NO_PAD_INDIFFERENT
            .decode(&compact)
            .map_err(|e| format!("invalid base64: {e}")),
        Format::Base64url => URL_SAFE_NO_PAD_INDIFFERENT
            .decode(&compact)
            .map_err(|e| format!("invalid base64url: {e}")),
        Format::Hex => decode_hex(compact.strip_prefix("0x").unwrap_or(&compact)),
        Format::Url => decode_url(text.trim()),
    }
}

fn decode_hex(hex: &str) -> Result<Vec<u8>, String> {
    if !hex.len().is_multiple_of(2) {
        return Err(format!("invalid hex: odd number of digits ({})", hex.len()));
    }
    (0..hex.len())
        .step_by(2)
        .map(|i| {
            u8::from_str_radix(&hex[i..i + 2], 16)
                .map_err(|_| format!("invalid hex: {:?} at position {i}", &hex[i..i + 2]))
        })
        .collect()
}

/// Percent-decoding; `+` stays `+`, as in decodeURIComponent.
fn decode_url(text: &str) -> Result<Vec<u8>, String> {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' {
            let pair = bytes
                .get(i + 1..i + 3)
                .and_then(|pair| std::str::from_utf8(pair).ok())
                .and_then(|pair| u8::from_str_radix(pair, 16).ok())
                .ok_or_else(|| format!("invalid percent-encoding at position {i}"))?;
            out.push(pair);
            i += 3;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encodes_every_format() {
        let data = "héllo wörld/?+&=~".as_bytes();
        assert_eq!(encode(Format::Base64, data), "aMOpbGxvIHfDtnJsZC8/KyY9fg==");
        assert_eq!(
            encode(Format::Base64url, data),
            "aMOpbGxvIHfDtnJsZC8_KyY9fg"
        );
        assert_eq!(
            encode(Format::Hex, data),
            "68c3a96c6c6f2077c3b6726c642f3f2b263d7e"
        );
        assert_eq!(
            encode(Format::Url, data),
            "h%C3%A9llo%20w%C3%B6rld%2F%3F%2B%26%3D~"
        );
    }

    #[test]
    fn round_trips_arbitrary_bytes() {
        let data: Vec<u8> = (0..=255).collect();
        for format in [Format::Base64, Format::Base64url, Format::Hex, Format::Url] {
            let encoded = encode(format, &data);
            assert_eq!(
                decode(format, encoded.as_bytes()).unwrap(),
                data,
                "{format:?}"
            );
        }
    }

    #[test]
    fn decodes_leniently() {
        // Wrapped lines, missing padding, uppercase hex with a 0x prefix.
        assert_eq!(decode(Format::Base64, b"aGVs\nbG8=\n").unwrap(), b"hello");
        assert_eq!(decode(Format::Base64, b"aGVsbG8").unwrap(), b"hello");
        assert_eq!(decode(Format::Base64url, b"aGVsbG8=").unwrap(), b"hello");
        assert_eq!(decode(Format::Hex, b"0x68 65 6C 6C 6F").unwrap(), b"hello");
        assert_eq!(decode(Format::Url, b"a%20b+c%2b").unwrap(), b"a b+c+");
    }

    #[test]
    fn tells_text_from_binary() {
        assert_eq!(
            as_text(b"line one\n\tindented\r\n"),
            Some("line one\n\tindented\r\n")
        );
        assert_eq!(as_text("héllo".as_bytes()), Some("héllo"));
        assert_eq!(as_text(&[0, 1, 2]), None);
        assert_eq!(as_text(b"\x1b[31mred"), None);
        assert_eq!(as_text(&[0xff, 0xfe]), None);
    }

    #[test]
    fn explains_bad_input() {
        assert!(
            decode(Format::Hex, b"abc")
                .unwrap_err()
                .contains("odd number")
        );
        assert!(decode(Format::Hex, b"zz").unwrap_err().contains("\"zz\""));
        assert!(
            decode(Format::Url, b"50%")
                .unwrap_err()
                .contains("position 2")
        );
        assert!(
            decode(Format::Url, b"%zz")
                .unwrap_err()
                .contains("position 0")
        );
        assert!(
            decode(Format::Base64, b"a$b=")
                .unwrap_err()
                .starts_with("invalid base64")
        );
    }
}
