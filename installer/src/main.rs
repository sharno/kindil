//! `kindil-install`: puts kindil on a Kindle Paperwhite 1 plugged in over USB.
use anyhow::{bail, Context};
use clap::{Parser, Subcommand};
use kindil_installer::check::{self, Status};
use kindil_installer::detect::Kindle;
use kindil_installer::install::Event;
use kindil_installer::release::{self, Github};
use kindil_installer::{eject, NEXT_STEPS};
use std::io::{BufRead, Write};
use std::path::PathBuf;

#[derive(Parser)]
#[command(
    name = "kindil-install",
    version,
    about = "Install kindil on a jailbroken Kindle Paperwhite (1st generation) over USB"
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// List the Kindles plugged in, with model, firmware and any kindil on them
    Detect,
    /// Check that a Kindle is ready for kindil: a PW1, jailbroken, with KUAL
    Check {
        /// The Kindle's drive, e.g. /Volumes/Kindle or E:\ (default: the only one)
        #[arg(long)]
        volume: Option<PathBuf>,
    },
    /// Download the latest signed release and install it on the Kindle
    Install {
        /// A release other than the latest, e.g. 0.13.2
        #[arg(long)]
        version: Option<String>,
        /// The Kindle's drive (default: the only one plugged in)
        #[arg(long)]
        volume: Option<PathBuf>,
        /// Don't ask: install, then eject
        #[arg(long, short)]
        yes: bool,
        /// Install although the model can't be told (you checked it's a PW1)
        #[arg(long)]
        force: bool,
        /// Leave the Kindle mounted afterwards
        #[arg(long)]
        no_eject: bool,
    },
    /// Eject the Kindle's drive
    Eject {
        #[arg(long)]
        volume: Option<PathBuf>,
    },
}

fn main() {
    if let Err(error) = run(Cli::parse()) {
        eprintln!("error: {error:#}");
        std::process::exit(1);
    }
}

fn run(cli: Cli) -> anyhow::Result<()> {
    match cli.command {
        Command::Detect => detect(),
        Command::Check { volume } => {
            let kindle = kindil_installer::choose(volume.as_deref())?;
            describe(&kindle);
            if !print_checks(&kindle, false) {
                std::process::exit(2);
            }
            println!("\nReady: run `kindil-install install`.");
            Ok(())
        }
        Command::Install {
            version,
            volume,
            yes,
            force,
            no_eject,
        } => install(version, volume, yes, force, no_eject),
        Command::Eject { volume } => {
            let kindle = kindil_installer::choose(volume.as_deref())?;
            eject_now(&kindle)
        }
    }
}

fn detect() -> anyhow::Result<()> {
    let found = kindil_installer::detect::scan();
    if found.is_empty() {
        println!(
            "No Kindle found. Plug it in with a data cable and unlock its screen: it should \
             show \"USB drive mode\" and appear as a drive named Kindle."
        );
    }
    for kindle in &found {
        describe(kindle);
    }
    Ok(())
}

fn describe(kindle: &Kindle) {
    println!("Kindle at {}", kindle.volume.display());
    println!("  model:    {}", kindle.model);
    if let Some(serial) = &kindle.serial {
        println!("  serial:   {}…", serial.get(..4).unwrap_or(serial));
    }
    match &kindle.firmware {
        Some(f) => println!("  firmware: {f}"),
        None => println!("  firmware: unknown (no system/version.txt)"),
    }
    match &kindle.installed {
        Some(i) => println!(
            "  kindil:   {} installed{}",
            i.version.as_deref().unwrap_or("a version"),
            if i.config.is_empty() {
                String::new()
            } else {
                format!(" (settings: {})", i.config.join(", "))
            }
        ),
        None => println!("  kindil:   not installed"),
    }
}

/// Prints the checklist; whether it allows installing.
fn print_checks(kindle: &Kindle, force: bool) -> bool {
    let checks = check::checklist(kindle);
    println!();
    for c in &checks {
        let mark = match c.status {
            Status::Pass => "[ok]",
            Status::Fail => "[missing]",
            Status::Unknown => "[?]",
        };
        println!("{mark} {}", c.title);
        println!("{}", indent(&textwrap(&c.detail, 76), "    "));
        for link in &c.links {
            println!("    → {link}");
        }
    }
    check::ready(&checks, force)
}

fn textwrap(text: &str, width: usize) -> String {
    textwrap::fill(text, width)
}

fn indent(text: &str, by: &str) -> String {
    text.lines()
        .map(|l| format!("{by}{l}"))
        .collect::<Vec<_>>()
        .join("\n")
}

fn ask(question: &str, yes: bool) -> anyhow::Result<bool> {
    if yes {
        return Ok(true);
    }
    print!("{question} [y/N] ");
    std::io::stdout().flush()?;
    let mut answer = String::new();
    std::io::stdin().lock().read_line(&mut answer)?;
    Ok(matches!(answer.trim(), "y" | "Y" | "yes" | "Yes"))
}

fn install(
    version: Option<String>,
    volume: Option<PathBuf>,
    yes: bool,
    force: bool,
    no_eject: bool,
) -> anyhow::Result<()> {
    println!("1. Looking for the Kindle");
    let kindle = kindil_installer::choose(volume.as_deref())?;
    describe(&kindle);
    println!("\n2. Checking it's ready");
    if !print_checks(&kindle, force) {
        bail!("not installing: see the checklist above");
    }
    let github = Github::default();
    let version = match version {
        Some(v) => release::normalize(&v)?,
        None => github.latest()?,
    };
    let question = match kindle.installed.as_ref().and_then(|i| i.version.as_deref()) {
        Some(old) if old == version => {
            format!("\nkindil {version} is already installed. Reinstall it?")
        }
        Some(old) => format!("\nUpgrade kindil {old} to {version}? Your settings stay."),
        None if kindle.installed.is_some() => {
            format!("\nkindil is installed. Replace it with {version}? Your settings stay.")
        }
        None => format!("\nInstall kindil {version} on {}?", kindle.volume.display()),
    };
    if !ask(&question, yes)? {
        println!("Nothing changed.");
        return Ok(());
    }
    println!("\n3. Downloading kindil {version}");
    let mut last = String::new();
    let release = github.download(&version, |what, n, total| {
        let line = match total {
            Some(t) if t > 0 => format!("   {what}: {}%", n * 100 / t),
            _ => format!("   {what}: {} KB", n / 1024),
        };
        if line != last {
            print!("\r{line}   ");
            let _ = std::io::stdout().flush();
            last = line;
        }
    })?;
    println!("\r   downloaded; signature checked against kindil's release key");
    println!("\n4. Copying to {}", kindle.volume.display());
    let report =
        kindil_installer::install::install(&kindle.volume, &release, |event| match event {
            Event::Wrote { path, n, of } => println!("   [{n}/{of}] {}", path.display()),
            Event::Kept { path } => println!("   kept your {}", path.display()),
            Event::Removed { path } => println!("   removed stale {}", path.display()),
        })
        .context("copying to the Kindle")?;
    println!(
        "   {} files written and checked{}",
        report.written.len(),
        if report.kept.is_empty() {
            String::new()
        } else {
            format!("; {} settings files kept", report.kept.len())
        }
    );
    println!("\nkindil {version} is on the Kindle.\n");
    println!("{}", textwrap(NEXT_STEPS, 78));
    if !no_eject && ask("\nEject the Kindle now?", yes)? {
        eject_now(&kindle)?;
    }
    Ok(())
}

fn eject_now(kindle: &Kindle) -> anyhow::Result<()> {
    match eject::eject(&kindle.volume) {
        Ok(()) => {
            println!(
                "Ejected {}: you can unplug the Kindle.",
                kindle.volume.display()
            );
            Ok(())
        }
        Err(error) => {
            println!("Couldn't eject: {error:#}\n{}", eject::by_hand());
            Ok(())
        }
    }
}
