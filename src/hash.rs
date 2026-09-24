use crate::utils;
use clap::{Parser, ValueEnum};
use md5::Md5;
use sha1::Sha1;
use sha2::digest::DynDigest;
use sha2::{Sha256, Sha384, Sha512};
use std::fs::{self, File};
use std::io::{self, ErrorKind, IsTerminal, Read};
use std::path::Path;
use std::process::exit;

#[derive(Parser, Debug)]
#[command(after_help = "\
Examples:
  rtools hash rtools-0.2.0-x86_64-unknown-linux-musl.tar.gz
  rtools hash -a md5 disk.iso notes.txt
  rtools hash --text hello        (piped input keeps echo's newline, --text doesn't)
  rtools hash --check SHA256SUMS  (skips listed files that aren't there)")]
pub struct Args {
    /// Files to hash. Without any, hashes stdin (pipe)
    files: Vec<String>,

    /// Hash this text instead
    #[arg(short, long, conflicts_with = "files")]
    text: Option<String>,

    /// Hash algorithm
    #[arg(short, long, value_enum, default_value_t = Algorithm::Sha256)]
    alg: Algorithm,

    /// Check the files listed in a checksum file like SHA256SUMS, found next to it
    #[arg(long, value_name = "SUMS", conflicts_with_all = ["files", "text"])]
    check: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, ValueEnum)]
enum Algorithm {
    Md5,
    Sha1,
    Sha256,
    Sha384,
    Sha512,
}

impl Algorithm {
    fn hasher(self) -> Box<dyn DynDigest> {
        match self {
            Algorithm::Md5 => Box::new(Md5::default()),
            Algorithm::Sha1 => Box::new(Sha1::default()),
            Algorithm::Sha256 => Box::new(Sha256::default()),
            Algorithm::Sha384 => Box::new(Sha384::default()),
            Algorithm::Sha512 => Box::new(Sha512::default()),
        }
    }

    /// Checksum files don't name the algorithm, but the digest length gives it away.
    fn from_hex_len(len: usize) -> Option<Algorithm> {
        match len {
            32 => Some(Algorithm::Md5),
            40 => Some(Algorithm::Sha1),
            64 => Some(Algorithm::Sha256),
            96 => Some(Algorithm::Sha384),
            128 => Some(Algorithm::Sha512),
            _ => None,
        }
    }
}

pub fn run(args: &Args, copy: bool) {
    let result = match &args.check {
        Some(sums) => check(sums),
        None => hash(args, copy),
    };

    if let Err(e) = result {
        eprintln!("Error: {}", e);
        exit(1);
    }
}

fn hash(args: &Args, copy: bool) -> Result<(), String> {
    if let Some(text) = &args.text {
        let digest = digest(args.alg, text.as_bytes()).expect("reading a string can't fail");
        return utils::emit(&digest, copy);
    }
    if args.files.is_empty() {
        if io::stdin().is_terminal() {
            return Err("nothing to hash: pass files or --text, or pipe data in".to_string());
        }
        let digest = digest(args.alg, io::stdin().lock())
            .map_err(|e| format!("failed to read stdin: {e}"))?;
        return utils::emit(&digest, copy);
    }

    let mut lines = Vec::with_capacity(args.files.len());
    for path in &args.files {
        let digest = digest_file(args.alg, Path::new(path))
            .map_err(|e| format!("failed to read {path}: {e}"))?;
        // The layout sha256sum uses, so the output works as a checksum file.
        lines.push(format!("{digest}  {path}"));
    }
    utils::emit(&lines.join("\n"), copy)
}

fn digest(alg: Algorithm, mut data: impl Read) -> io::Result<String> {
    let mut hasher = alg.hasher();
    let mut buf = vec![0; 64 * 1024];
    loop {
        match data.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => hasher.update(&buf[..n]),
            Err(e) if e.kind() == ErrorKind::Interrupted => {}
            Err(e) => return Err(e),
        }
    }
    Ok(utils::hex(&hasher.finalize()))
}

fn digest_file(alg: Algorithm, path: &Path) -> io::Result<String> {
    digest(alg, File::open(path)?)
}

#[derive(Debug, PartialEq)]
struct Entry {
    alg: Algorithm,
    digest: String,
    name: String,
}

/// Lines of `sha256sum`-style output: `<hex>  <name>`, or `<hex> *<name>` in binary mode.
fn parse_sums(sums: &str) -> Result<Vec<Entry>, String> {
    let mut entries = Vec::new();
    for (number, line) in sums.lines().enumerate() {
        let line = line.trim_end();
        if line.trim().is_empty() || line.starts_with('#') {
            continue;
        }
        let bad = || format!("line {} isn't `<checksum>  <file>`: {line}", number + 1);
        let (digest, name) = line.split_once(' ').ok_or_else(bad)?;
        let name = name.strip_prefix([' ', '*']).ok_or_else(bad)?;
        let alg = Algorithm::from_hex_len(digest.len())
            .filter(|_| digest.bytes().all(|b| b.is_ascii_hexdigit()))
            .ok_or_else(bad)?;
        if name.is_empty() {
            return Err(bad());
        }
        entries.push(Entry {
            alg,
            digest: digest.to_ascii_lowercase(),
            name: name.to_string(),
        });
    }
    Ok(entries)
}

#[derive(Debug, PartialEq)]
enum Outcome {
    Ok,
    Mismatch,
    Unreadable(String),
    Missing,
}

fn check_entries(entries: &[Entry], dir: &Path) -> Vec<Outcome> {
    entries
        .iter()
        .map(|entry| {
            let path = dir.join(&entry.name);
            if !path.exists() {
                return Outcome::Missing;
            }
            match digest_file(entry.alg, &path) {
                Ok(actual) if actual == entry.digest => Outcome::Ok,
                Ok(_) => Outcome::Mismatch,
                Err(e) => Outcome::Unreadable(e.to_string()),
            }
        })
        .collect()
}

fn check(sums_path: &str) -> Result<(), String> {
    let sums =
        fs::read_to_string(sums_path).map_err(|e| format!("failed to read {sums_path}: {e}"))?;
    let entries = parse_sums(&sums)?;
    if entries.is_empty() {
        return Err(format!("{sums_path} lists no checksums"));
    }
    let dir = Path::new(sums_path).parent().unwrap_or(Path::new(""));
    let outcomes = check_entries(&entries, dir);

    let (mut ok, mut failed, mut missing) = (0, 0, 0);
    for (entry, outcome) in entries.iter().zip(&outcomes) {
        match outcome {
            Outcome::Ok => {
                ok += 1;
                println!("OK      {}", entry.name);
            }
            Outcome::Mismatch => {
                failed += 1;
                println!("FAILED  {}", entry.name);
            }
            Outcome::Unreadable(e) => {
                failed += 1;
                println!("FAILED  {} ({e})", entry.name);
            }
            Outcome::Missing => missing += 1,
        }
    }
    if missing > 0 {
        eprintln!("{missing} listed file(s) aren't there, so weren't checked");
    }
    if failed > 0 {
        return Err(format!("{failed} of {} file(s) did NOT match", ok + failed));
    }
    if ok == 0 {
        return Err(format!("none of the files {sums_path} lists are there"));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matches_known_digests_of_abc() {
        let cases = [
            (Algorithm::Md5, "900150983cd24fb0d6963f7d28e17f72"),
            (Algorithm::Sha1, "a9993e364706816aba3e25717850c26c9cd0d89d"),
            (
                Algorithm::Sha256,
                "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad",
            ),
            (
                Algorithm::Sha384,
                "cb00753f45a35e8bb5a03d699ac65007272c32ab0eded1631a8b605a43ff5bed8086072ba1e7cc2358baeca134c825a7",
            ),
            (
                Algorithm::Sha512,
                "ddaf35a193617abacc417349ae20413112e6fa4e89a97ea20a9eeee64b55d39a2192992a274fc1a836ba3c23a3feebbd454d4423643ce80e2a9ac94fa54ca49f",
            ),
        ];
        for (alg, expected) in cases {
            assert_eq!(digest(alg, &b"abc"[..]).unwrap(), expected, "{alg:?}");
            assert_eq!(Algorithm::from_hex_len(expected.len()), Some(alg));
        }
    }

    #[test]
    fn hashes_data_larger_than_the_buffer() {
        let data = vec![b'a'; 1_000_000];
        assert_eq!(
            digest(Algorithm::Sha256, &data[..]).unwrap(),
            "cdc76e5c9914fb9281a1c7e284d73e67f1809a48a497200e046d39ccc7112cd0"
        );
    }

    #[test]
    fn parses_checksum_files() {
        let sha = "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad";
        let md5 = "900150983CD24FB0D6963F7D28E17F72";
        let sums = format!("# comment\n{sha}  a file.txt\n\n{md5} *b.bin\n");
        assert_eq!(
            parse_sums(&sums).unwrap(),
            vec![
                Entry {
                    alg: Algorithm::Sha256,
                    digest: sha.to_string(),
                    name: "a file.txt".to_string()
                },
                Entry {
                    alg: Algorithm::Md5,
                    digest: md5.to_ascii_lowercase(),
                    name: "b.bin".to_string()
                },
            ]
        );
        assert!(parse_sums("abc  file").unwrap_err().contains("line 1"));
        assert!(parse_sums(sha).is_err());
        assert!(parse_sums(&format!("{sha}x file")).is_err());
    }

    #[test]
    fn checks_files_next_to_the_checksum_file() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("good"), "abc").unwrap();
        fs::write(dir.path().join("bad"), "abd").unwrap();
        let abc = "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad";
        let entries = parse_sums(&format!("{abc}  good\n{abc}  bad\n{abc}  gone\n")).unwrap();
        assert_eq!(
            check_entries(&entries, dir.path()),
            vec![Outcome::Ok, Outcome::Mismatch, Outcome::Missing]
        );
    }
}
