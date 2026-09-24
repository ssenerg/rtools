mod factor;
mod gostruct;
mod ports;
mod qrcode;
mod tconv;
mod update;
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

    /// View active listening ports and kill them
    #[command(name = "ports")]
    Ports(ports::Args),
    /// Factor of a number
    #[command(name = "factor")]
    Factor(factor::Args),

    /// Update rtools to the latest release
    #[command(name = "update")]
    Update(update::Args),
}

fn main() {
    let cli = Cli::parse();

    // `update` talks to GitHub itself; every other command gets the background check.
    let update_check = match cli.command {
        Commands::Update(_) => None,
        _ => update::BackgroundCheck::start(),
    };

    match &cli.command {
        Commands::UuidGen(args) => uuidgen::run(args, cli.copy),
        Commands::Tconv(args) => tconv::run(args, cli.copy),
        Commands::GoStruct(args) => gostruct::run(args, cli.copy),
        Commands::QRCode(args) => qrcode::run(args),
        Commands::Ports(args) => ports::run(args, cli.copy),
        Commands::Factor(args) => factor::run(args),
        Commands::Update(args) => update::run(args),
    }

    if let Some(check) = update_check {
        check.finish();
    }
}
