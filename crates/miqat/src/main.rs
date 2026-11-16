use miqat::{config, layout, prayer, render, sources};
mod server;

use std::path::PathBuf;

use clap::{Parser, Subcommand};

use crate::config::Config;

/// Renders a prayer-times and weather dashboard as a PNG for a Kindle.
#[derive(Parser)]
#[command(version)]
struct Cli {
    #[arg(long, default_value = "config.toml")]
    config: PathBuf,
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Serve the dashboard at /dash.png (and a browser preview at /).
    Serve,
    /// Render one frame to a file.
    Render {
        #[arg(long, default_value = "dash.png")]
        out: PathBuf,
    },
}

fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    let config = Config::load(&cli.config)?;
    match cli.command {
        Command::Serve => server::run(&config),
        Command::Render { out } => {
            let png = server::App::new(&config).frame(&config, chrono::Utc::now())?;
            std::fs::write(&out, png)?;
            eprintln!("wrote {}", out.display());
            Ok(())
        }
    }
}
