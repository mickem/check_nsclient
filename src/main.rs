mod cli;
mod config;
mod constants;
mod debug;
mod nagios;
mod nrpe;
mod nsclient;
mod profile;
mod rendering;
mod tokens;

use crate::cli::{Cli, Commands};
use crate::nrpe::route_nrpe;
use crate::nsclient::route_ns_client;
use crate::profile::route_profile;
use crate::rendering::{PrintRender, Rendering};
use clap::Parser;

/// End as a Nagios plugin that could not run its check: the reason on stdout,
/// where Nagios looks for it, and exit code 3.
fn nagios_unknown(reason: &str) -> ! {
    println!("UNKNOWN: {reason}");
    std::process::exit(3)
}

/// The command line did not parse (or asked for help).
///
/// clap exits with 2 on a usage error, which Nagios reads as CRITICAL: a typo
/// in a check definition would page somebody about a host that is fine. For
/// the `nrpe` command, which is run as a plugin, that becomes an UNKNOWN; the
/// full message with the usage still goes to stderr. Everything else, help
/// and version included, ends the way clap ends it.
fn usage_error(error: clap::Error) -> ! {
    if error.use_stderr() && cli::is_nrpe_invocation(std::env::args().skip(1)) {
        let _ = error.print();
        // The message proper ends at the first blank line; what follows is
        // the usage. It can span lines ("... were not provided:" and then the
        // arguments), and Nagios shows only the first, so join them.
        let message = error.to_string();
        let reason: Vec<&str> = message
            .lines()
            .take_while(|line| !line.trim().is_empty())
            .map(str::trim)
            .collect();
        let reason = reason.join(" ");
        nagios_unknown(reason.strip_prefix("error: ").unwrap_or(&reason));
    }
    error.exit()
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let cli = Cli::try_parse().unwrap_or_else(|e| usage_error(e));

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
        Commands::Nrpe(args) => {
            let output_sink = Box::new(PrintRender::new());

            let result = route_nrpe(
                Rendering::new(cli.output, cli.output_style, cli.output_long, output_sink),
                args,
            )
            .await;
            match result {
                Ok(exit_code) => {
                    if exit_code != 0 {
                        std::process::exit(exit_code);
                    }
                }
                // Returning the error would exit with 1, a WARNING to Nagios,
                // and leave stdout empty.
                Err(e) => nagios_unknown(&format!("{e:#}")),
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
