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
