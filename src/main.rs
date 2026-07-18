mod gostruct;
mod tconv;
mod unix;
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

    /// Generate a Go struct from a JSON file
    #[command(name = "gostruct")]
    GoStruct(gostruct::Args),

    /// Work with unix timestamps
    #[command(name = "unix")]
    Unix(unix::Args),
}

fn main() {
    let cli = Cli::parse();

    match &cli.command {
        Commands::UuidGen(args) => uuidgen::run(args, cli.copy),
        Commands::Tconv => tconv::run(cli.copy),
        Commands::GoStruct(args) => gostruct::run(args, cli.copy),
        Commands::Unix(args) => unix::run(args),
    }
}
