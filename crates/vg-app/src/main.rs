//! `vg` — run with no arguments for the GUI, or use the subcommands for scripting.

mod cli;
mod config;
mod recorder;
mod gui;

fn main() -> anyhow::Result<()> {
    cli::run()
}
