mod cli;
mod config;
mod constants;
mod debug;
mod nsclient;
mod profile;
mod rendering;
mod tokens;

use crate::cli::{Cli, Commands};
use crate::nsclient::route_ns_client;
use crate::profile::route_profile;
use crate::rendering::{PrintRender, Rendering};
use clap::Parser;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();

    debug::set_level(cli.debug);
    debug::log(1, format!("Debug output enabled (level {})", cli.debug));
    if cli.wsl {
        tokens::enable_wsl_workaround();
    }

    // Handle Subcommands
    match &cli.command {
        Commands::NSClient(args) => {
            let output_sink = Box::new(PrintRender::new());

            let exit_code = route_ns_client(
                Rendering::new(cli.output, cli.output_style, cli.output_long, output_sink),
                args,
            )
            .await?;
            if exit_code != 0 {
                std::process::exit(exit_code);
            }
        }
        Commands::Version {} => {
            println!("Version: {}", env!("CARGO_PKG_VERSION"));
        }
        Commands::Profile { command } => {
            let output_sink = Box::new(PrintRender::new());
            route_profile(
                Rendering::new(cli.output, cli.output_style, cli.output_long, output_sink),
                command,
            )
            .await?
        }
    }

    Ok(())
}
