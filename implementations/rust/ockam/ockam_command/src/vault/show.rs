use clap::Args;
use ockam_api::cli_state::traits::StateDirTrait;

use crate::{docs, CommandGlobalOpts};

const LONG_ABOUT: &str = include_str!("./static/show/long_about.txt");
const AFTER_LONG_HELP: &str = include_str!("./static/show/after_long_help.txt");

/// Show the details of a vault
#[derive(Clone, Debug, Args)]
#[command(
    long_about = docs::about(LONG_ABOUT),
    after_long_help = docs::after_help(AFTER_LONG_HELP)
)]
pub struct ShowCommand {
    /// Name of the vault
    pub name: Option<String>,
}

impl ShowCommand {
    pub fn run(self, opts: CommandGlobalOpts) {
        if let Err(e) = run_impl(opts, self) {
            eprintln!("{e}");
            std::process::exit(e.code());
        }
    }
}

fn run_impl(opts: CommandGlobalOpts, cmd: ShowCommand) -> crate::Result<()> {
    let name = cmd
        .name
        .unwrap_or(opts.state.vaults.default()?.name().to_string());
    let state = opts.state.vaults.get(name)?;
    println!("Vault:");
    for line in state.to_string().lines() {
        println!("{:2}{}", "", line)
    }
    Ok(())
}
