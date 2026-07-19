use crate::utils;
use clap::Parser;
use qrcode::render::unicode;
use qrcode::{EcLevel, QrCode};
use std::process::exit;
use terminal_size::{Height, Width, terminal_size};

#[derive(Parser, Debug)]
pub struct Args {
    /// Input file (optional). If not provided, reads from stdin (pipe)
    #[arg(short, long)]
    input: Option<String>,
}

pub fn run(args: &Args) {
    match generate_qrcode(args) {
        Ok(qr) => {
            check_terminal_size(&qr);
            utils::emit(&qr, false).unwrap_or_else(|e| {
                eprintln!("Error: {}", e);
                exit(1);
            });
        }
        Err(e) => {
            eprintln!("Error: {}", e);
            exit(1);
        }
    }
}

fn generate_qrcode(args: &Args) -> Result<String, String> {
    let data = utils::read_stdin_or_file(&args.input)?;
    let data = data.trim_end_matches('\n');

    if data.is_empty() {
        return Err("Empty input provided. Pipe data or use --input file".to_string());
    }

    let code = QrCode::with_error_correction_level(data, EcLevel::M)
        .or_else(|_| QrCode::with_error_correction_level(data, EcLevel::L))
        .map_err(|e| format!("Failed to build QR code: {}", e))?;

    Ok(code
        .render::<unicode::Dense1x2>()
        .dark_color(unicode::Dense1x2::Light)
        .light_color(unicode::Dense1x2::Dark)
        .build())
}

fn check_terminal_size(qr: &str) {
    let need_h = qr.lines().count();
    let need_w = qr.lines().map(|l| l.chars().count()).max().unwrap_or(0);

    if let Some((Width(cols), Height(rows))) = terminal_size() {
        if (cols as usize) < need_w || (rows as usize) < need_h {
            eprintln!(
                "Error: terminal is too small to render the QR code properly: need {}x{}, have {}x{}",
                need_w, need_h, cols, rows
            );
            exit(1);
        }
    }
}
