//! kindil's development and device tasks: `cargo xtask <task>`.
//!
//! The device tasks reach the Kindle over SSH (see [`device`]); the build
//! tasks use the flake's toolchains, as `nix develop` provides them.
mod dev;
mod device;
mod release;

use clap::{Parser, Subcommand};
use std::path::{Path, PathBuf};

#[derive(Parser)]
#[command(name = "cargo xtask", about = "kindil's development and device tasks")]
struct Cli {
    #[command(subcommand)]
    task: Task,
}

#[derive(Subcommand)]
enum Task {
    /// fmt --check, clippy, and every test suite
    Check,
    /// The test suites on the host
    Test,
    /// The test suites on the ARM target under qemu-arm
    TestArm,
    /// dist/kindil: the static ARMv7 binary
    Device,
    /// Every screen to dist/sim/*.png, on a simulated device
    Sim,
    /// An interactive window, on a simulated device
    Desktop,
    /// Format the Rust and Nix sources
    Fmt,
    /// Build, then copy the binary and KUAL launcher to the Kindle
    Deploy,
    /// Install a signed GitHub release on the Kindle, with rollback
    Release {
        /// The version, e.g. 0.9.1
        version: String,
        /// The release to roll back to if this one doesn't come up (default:
        /// the version it replaces)
        #[arg(long)]
        rollback: Option<String>,
        /// How long to wait for the Kindle to answer
        #[arg(long, default_value_t = 12)]
        wait: u64,
    },
    /// Run a script on the Kindle with a lifeline beside it
    Run {
        script: PathBuf,
        /// Seconds until the lifeline brings kindil and Wi-Fi back regardless
        deadline: u64,
        args: Vec<String>,
    },
    /// Wait for the Kindle to answer, then run a command on it
    Wait {
        #[arg(long, default_value_t = 12)]
        hours: u64,
        command: Vec<String>,
    },
}

fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("xtask sits in the repo")
        .to_owned()
}

fn main() -> anyhow::Result<()> {
    let root = root();
    std::env::set_current_dir(&root)?;
    match Cli::parse().task {
        Task::Check => dev::check(),
        Task::Test => dev::test(),
        Task::TestArm => dev::test_arm(),
        Task::Device => dev::device().map(drop),
        Task::Sim => dev::sim(),
        Task::Desktop => dev::desktop(),
        Task::Fmt => dev::fmt(),
        Task::Deploy => dev::deploy(),
        Task::Release {
            version,
            rollback,
            wait,
        } => release::install(
            &root,
            version.trim_start_matches('v'),
            rollback.as_deref().map(|v| v.trim_start_matches('v')),
            wait,
        ),
        Task::Run {
            script,
            deadline,
            args,
        } => dev::run_on_device(&script, deadline, &args),
        Task::Wait { hours, command } => dev::wait(hours, &command),
    }
}
