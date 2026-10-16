//! Native format, lint, and language-server entry point.
mod commands;

use clap::Parser;
use std::process::ExitCode;
use tex_ls::cli::Cli;

fn main() -> ExitCode {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("warn"))
        .target(env_logger::Target::Stderr)
        .init();
    commands::run(Cli::parse())
}
