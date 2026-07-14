use crate::utils;
use clap::{Args, ValueEnum};
use uuid::Uuid;

#[derive(Args)]
pub struct UuidGenArgs {
    #[arg(short = 'v', long = "version", value_name = "1-8", default_value_t = 4)]
    version: u8,

    #[arg(short = 'n', long = "count", value_name = "N", default_value_t = 1)]
    count: usize,

    #[arg(long, value_name = "NS")]
    namespace: Option<String>,

    #[arg(long, value_name = "NAME")]
    name: Option<String>,

    #[arg(short, long, value_enum, default_value_t = Format::Hyphenated)]
    format: Format,

    #[arg(short = 'U', long)]
    uppercase: bool,
}

#[derive(Clone, Copy, ValueEnum)]
enum Format {
    Hyphenated,
    Simple,
    Urn,
    Braced,
}

pub fn run(args: &UuidGenArgs, copy: bool) {
    let mut out = Vec::with_capacity(args.count);
    for _ in 0..args.count {
        let uuid = generate(
            args.version,
            args.namespace.as_deref(),
            args.name.as_deref(),
        )
        .expect("failed to generate UUID");

        out.push(format_uuid(&uuid, args.format, args.uppercase));
    }
    utils::emit(&out.join("\n"), copy).expect("failed to emit UUIDs")
}

fn generate(version: u8, namespace: Option<&str>, name: Option<&str>) -> Result<Uuid, String> {
    match version {
        1 => Ok(Uuid::now_v1(&random_node())),
        3 => {
            let (ns, name) = ns_and_name(namespace, name, 3)?;
            Ok(Uuid::new_v3(&ns, name.as_bytes()))
        }
        4 => Ok(Uuid::new_v4()),
        5 => {
            let (ns, name) = ns_and_name(namespace, name, 5)?;
            Ok(Uuid::new_v5(&ns, name.as_bytes()))
        }
        6 => Ok(Uuid::now_v6(&random_node())),
        7 => Ok(Uuid::now_v7()),
        8 => Ok(Uuid::new_v8(Uuid::new_v4().into_bytes())),
        2 => Err("UUID v2 (DCE Security) is not supported".to_string()),
        other => Err(format!(
            "unsupported UUID version: {other} (expected 1, 3, 4, 5, 6, 7, or 8)"
        )),
    }
}

fn ns_and_name(
    namespace: Option<&str>,
    name: Option<&str>,
    version: u8,
) -> Result<(Uuid, String), String> {
    let namespace = namespace.ok_or_else(|| {
        format!("UUID v{version} requires --namespace (dns, url, oid, x500, or a UUID)")
    })?;
    let name = name.ok_or_else(|| format!("UUID v{version} requires --name"))?;
    Ok((parse_namespace(namespace)?, name.to_string()))
}

fn parse_namespace(s: &str) -> Result<Uuid, String> {
    match s.to_ascii_lowercase().as_str() {
        "dns" => Ok(Uuid::NAMESPACE_DNS),
        "url" => Ok(Uuid::NAMESPACE_URL),
        "oid" => Ok(Uuid::NAMESPACE_OID),
        "x500" => Ok(Uuid::NAMESPACE_X500),
        _ => Uuid::parse_str(s).map_err(|_| {
            format!("invalid namespace '{s}': expected dns, url, oid, x500, or a UUID")
        }),
    }
}

fn random_node() -> [u8; 6] {
    let bytes = Uuid::new_v4().into_bytes();
    let mut node = [bytes[0], bytes[1], bytes[2], bytes[3], bytes[4], bytes[5]];
    node[0] |= 0x01;
    node
}

fn format_uuid(uuid: &Uuid, format: Format, uppercase: bool) -> String {
    let s = match format {
        Format::Hyphenated => uuid.hyphenated().to_string(),
        Format::Simple => uuid.simple().to_string(),
        Format::Urn => uuid.urn().to_string(),
        Format::Braced => uuid.braced().to_string(),
    };
    if uppercase { s.to_uppercase() } else { s }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make(version: u8) -> Uuid {
        generate(version, None, None).expect("version should generate")
    }

    #[test]
    fn generates_each_supported_version() {
        for version in [1u8, 4, 6, 7, 8] {
            assert_eq!(
                make(version).get_version_num(),
                version as usize,
                "v{version} should report its own version number"
            );
        }
    }

    #[test]
    fn v3_and_v5_report_their_version() {
        let v3 = generate(3, Some("dns"), Some("example.com")).unwrap();
        let v5 = generate(5, Some("dns"), Some("example.com")).unwrap();
        assert_eq!(v3.get_version_num(), 3);
        assert_eq!(v5.get_version_num(), 5);
    }

    #[test]
    fn v3_and_v5_are_deterministic_with_known_values() {
        // Canonical hashes for the DNS namespace + "example.com".
        assert_eq!(
            generate(3, Some("dns"), Some("example.com"))
                .unwrap()
                .hyphenated()
                .to_string(),
            "9073926b-929f-31c2-abc9-fad77ae3e8eb"
        );
        assert_eq!(
            generate(5, Some("dns"), Some("example.com"))
                .unwrap()
                .hyphenated()
                .to_string(),
            "cfbff0d1-9375-5685-968c-48ce8b15ae17"
        );
    }

    #[test]
    fn v4_is_random_across_calls() {
        assert_ne!(make(4), make(4));
    }

    #[test]
    fn v7_is_time_ordered() {
        // v7 embeds a millisecond timestamp in the high bits
        let first = make(7);
        let second = make(7);
        assert!(second >= first);
    }

    #[test]
    fn v3_requires_namespace_and_name() {
        assert!(
            generate(3, None, Some("x"))
                .unwrap_err()
                .contains("namespace")
        );
        assert!(generate(3, Some("dns"), None).unwrap_err().contains("name"));
        assert!(generate(5, None, None).unwrap_err().contains("namespace"));
    }

    #[test]
    fn v2_is_rejected() {
        assert!(generate(2, None, None).unwrap_err().contains("v2"));
    }

    #[test]
    fn unknown_versions_are_rejected() {
        for version in [0u8, 9, 255] {
            assert!(
                generate(version, None, None)
                    .unwrap_err()
                    .contains("unsupported"),
                "v{version} should be unsupported"
            );
        }
    }

    #[test]
    fn parse_namespace_accepts_aliases_case_insensitively() {
        assert_eq!(parse_namespace("dns").unwrap(), Uuid::NAMESPACE_DNS);
        assert_eq!(parse_namespace("URL").unwrap(), Uuid::NAMESPACE_URL);
        assert_eq!(parse_namespace("Oid").unwrap(), Uuid::NAMESPACE_OID);
        assert_eq!(parse_namespace("x500").unwrap(), Uuid::NAMESPACE_X500);
    }

    #[test]
    fn parse_namespace_accepts_a_raw_uuid() {
        let raw = "6ba7b810-9dad-11d1-80b4-00c04fd430c8";
        assert_eq!(parse_namespace(raw).unwrap(), Uuid::parse_str(raw).unwrap());
    }

    #[test]
    fn parse_namespace_rejects_garbage() {
        assert!(
            parse_namespace("not-a-namespace")
                .unwrap_err()
                .contains("invalid namespace")
        );
    }

    #[test]
    fn random_node_sets_multicast_bit() {
        // The multicast bit must be set -> the synthetic node never collides with a real (unicast) hardware MAC
        for _ in 0..100 {
            assert_eq!(random_node()[0] & 0x01, 0x01);
        }
    }

    #[test]
    fn random_node_varies() {
        assert_ne!(random_node(), random_node());
    }

    #[test]
    fn format_uuid_renders_each_form() {
        let uuid = Uuid::parse_str("67e55044-10b1-426f-9247-bb680e5fe0c8").unwrap();
        assert_eq!(
            format_uuid(&uuid, Format::Hyphenated, false),
            "67e55044-10b1-426f-9247-bb680e5fe0c8"
        );
        assert_eq!(
            format_uuid(&uuid, Format::Simple, false),
            "67e5504410b1426f9247bb680e5fe0c8"
        );
        assert_eq!(
            format_uuid(&uuid, Format::Urn, false),
            "urn:uuid:67e55044-10b1-426f-9247-bb680e5fe0c8"
        );
        assert_eq!(
            format_uuid(&uuid, Format::Braced, false),
            "{67e55044-10b1-426f-9247-bb680e5fe0c8}"
        );
    }

    #[test]
    fn format_uuid_uppercases_when_requested() {
        let uuid = Uuid::parse_str("67e55044-10b1-426f-9247-bb680e5fe0c8").unwrap();
        assert_eq!(
            format_uuid(&uuid, Format::Hyphenated, true),
            "67E55044-10B1-426F-9247-BB680E5FE0C8"
        );
    }
}
