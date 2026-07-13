mod gostruct;
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
    #[command(name = "uuidgen")]
    UUidGen,
    #[command(name = "tconv")]
    Tconv,

    /// Generate a Go struct from a JSON file
    #[command(name = "gostruct")]
    GoStruct(gostruct::Args),
}

fn main() {
    let cli = Cli::parse();

    match &cli.command {
        Commands::UUidGen => {
            let generated = uuidgen::gen_uuid();
            println!("{}", generated);
        }
        Commands::Tconv => {}
        Commands::GoStruct(args) => {
            gostruct::run(args);
        }
    }
}
