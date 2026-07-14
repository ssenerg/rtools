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
    #[command(name = "uuid")]
    UuidGen(uuidgen::UuidGenArgs),
    #[command(name = "tconv")]
    Tconv,
}

fn main() {
    let cli = Cli::parse();

    match &cli.command {
        Commands::UuidGen(args) => uuidgen::run(args, cli.copy),
        Commands::Tconv => tconv::run(cli.copy),
    }
}
