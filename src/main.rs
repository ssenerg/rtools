mod tconv;
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
}

#[derive(Subcommand)]
enum Commands {
    UUidGen,
    Tconv,
}

fn main() {
    let cli = Cli::parse();

    match &cli.command {
        Commands::UUidGen => {}
        Commands::Tconv => {}
    }
}
