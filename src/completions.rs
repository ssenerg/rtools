use clap::{CommandFactory, Parser};
use clap_complete::Shell;
use std::io::{self, Write};
use std::process::exit;

#[derive(Parser, Debug)]
#[command(after_help = "\
To load completions in every new terminal, run the line for your shell once:

  bash        echo 'eval \"$(rtools completions bash)\"' >> ~/.bashrc
  zsh         echo 'eval \"$(rtools completions zsh)\"' >> ~/.zshrc
  fish        echo 'rtools completions fish | source' >> ~/.config/fish/config.fish
  PowerShell  Add-Content $PROFILE 'rtools completions powershell | Out-String | Invoke-Expression'

Then open a new terminal. In zsh the line needs to come after compinit, which oh-my-zsh
and most setups already run.")]
pub struct Args {
    /// Shell to complete for. If omitted, uses the shell you're in ($SHELL)
    #[arg(value_enum)]
    shell: Option<Shell>,
}

pub fn run(args: &Args) {
    let Some(shell) = args.shell.or_else(Shell::from_env) else {
        eprintln!(
            "Error: couldn't tell which shell you use. Name it: rtools completions bash|zsh|fish|powershell|elvish"
        );
        exit(1);
    };
    let mut script = Vec::new();
    clap_complete::generate(shell, &mut crate::Cli::command(), "rtools", &mut script);
    if let Err(e) = io::stdout().write_all(&script)
        && e.kind() != io::ErrorKind::BrokenPipe
    {
        eprintln!("Error: failed to write the script: {e}");
        exit(1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::ValueEnum;

    #[test]
    fn completes_every_command_in_every_shell() {
        for &shell in Shell::value_variants() {
            let mut script = Vec::new();
            clap_complete::generate(shell, &mut crate::Cli::command(), "rtools", &mut script);
            let script = String::from_utf8(script).unwrap();
            for command in ["uuid", "tconv", "jwt", "cron", "completions", "update"] {
                assert!(script.contains(command), "{shell}: {command}");
            }
        }
    }
}
