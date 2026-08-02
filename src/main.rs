mod gostruct;
mod qrcode;
mod tconv;
mod utils;
mod uuidgen;

use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(
    author,
    version,
    about = "PAK-ARSHIA devtools",
    long_about = "Dige baraye karaye dev o ina search nakon ya az AI komak nakha az PAK-ARSHIA devtools estefade kon, bale rtools."
)]
struct Cli {
    #[command(subcommand)]
    command: Commands,

    #[arg(short = 'c', long = "copy", global = true)]
    copy: bool,
}

#[derive(Subcommand)]
enum Commands {
    /// Generate a UUID
    #[command(name = "uuid")]
    UuidGen(uuidgen::UuidGenArgs),

    /// Work with time
    #[command(name = "tconv")]
    Tconv(tconv::Args),

    /// Generate a Go struct from a JSON file
    #[command(name = "gostruct")]
    GoStruct(gostruct::Args),

    /// Generate QRCode in terminal
    #[command(name = "qrcode")]
    QRCode(qrcode::Args),
}

fn main() {
    let cli = Cli::parse();

    match &cli.command {
        Commands::UuidGen(args) => uuidgen::run(args, cli.copy),
        Commands::Tconv(args) => tconv::run(args, cli.copy),
        Commands::GoStruct(args) => gostruct::run(args, cli.copy),
        Commands::QRCode(args) => qrcode::run(args),
    }
}
