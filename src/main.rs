use clap::{Parser, Subcommand};
use kindil::config::Config;
use kindil::host::Simulated;
use kindil::system::Touches;
use kindil::ui::{App, Execution, Exit, Options, Panel};
use slint::ComponentHandle;
use std::path::PathBuf;
use std::sync::Arc;

/// kindil · قنديل — prayer-time dashboard and reader for the Kindle Paperwhite.
#[derive(Parser)]
#[command(version)]
struct Cli {
    /// Settings file [default: /mnt/us/kindil/kindil.toml when present]
    #[arg(long, global = true)]
    config: Option<PathBuf>,
    /// Override the Miqat settings file
    #[arg(long, global = true)]
    miqat_config: Option<PathBuf>,
    /// Override the books directory
    #[arg(long, global = true)]
    books: Option<PathBuf>,
    /// Override where on-device choices are saved
    #[arg(long, global = true)]
    prefs: Option<PathBuf>,
    #[command(subcommand)]
    mode: Mode,
}

#[derive(Subcommand)]
enum Mode {
    /// Render every screen to DIR/*.ppm with a simulated device
    Preview { dir: PathBuf },
    /// Interactive window with a simulated device
    #[cfg(feature = "desktop")]
    Desktop,
    /// Run on the Kindle; start it from the KUAL launcher, which stops and
    /// restarts Amazon's UI around it
    #[cfg(feature = "kindle")]
    Kindle,
    /// Print kernel uevents (power button, USB cable) as they arrive
    Uevents,
}

impl Cli {
    fn config(&self) -> anyhow::Result<Config> {
        let config = Config::load(self.config.as_deref())?;
        Ok(Config {
            miqat_config: self.miqat_config.clone().unwrap_or(config.miqat_config),
            books: self.books.clone().unwrap_or(config.books),
            prefs: self.prefs.clone().unwrap_or(config.prefs),
            ..config
        })
    }
}

/// Simulated runs never write next to the device's real preferences.
fn scratch_prefs(cli: &Cli, config: Config, dir: &std::path::Path) -> Config {
    Config {
        prefs: cli
            .prefs
            .clone()
            .unwrap_or_else(|| dir.join("kindil-prefs.toml")),
        ..config
    }
}

fn preview(cli: &Cli, dir: &std::path::Path) -> anyhow::Result<()> {
    let config = scratch_prefs(cli, cli.config()?, dir);
    let miqat_config = config.miqat_config.clone();
    let window = kindil::preview::install()?;
    let app = App::new(Options {
        config,
        host: Arc::new(Simulated::default()),
        panel: Panel::Lcd,
        execution: Execution::Inline,
        touches: Touches::default(),
        now: chrono::Utc::now,
    })?;
    app.refresh_now();
    if miqat_config.is_file() {
        let miqat = miqat::config::Config::load(&miqat_config)?;
        let noon = chrono::DateTime::from_timestamp(1_794_398_400, 0)
            .ok_or_else(|| anyhow::anyhow!("fixture instant"))?;
        app.set_miqat(kindil::dashboards::preview(&miqat, noon)?);
    }
    app.window.show()?;
    kindil::preview::screens(&app, &window, dir)?;
    println!("previews: {}", dir.display());
    Ok(())
}

#[cfg(feature = "desktop")]
fn desktop(cli: &Cli) -> anyhow::Result<()> {
    let config = scratch_prefs(cli, cli.config()?, &std::env::temp_dir());
    let app = App::new(Options {
        config,
        host: Arc::new(Simulated::default()),
        panel: Panel::Lcd,
        execution: Execution::Background,
        touches: Touches::default(),
        now: chrono::Utc::now,
    })?;
    app.start();
    kindil::signals::quit_on_termination()?;
    app.window.run()?;
    app.shutdown();
    Ok(())
}

#[cfg(feature = "kindle")]
fn kindle(cli: &Cli) -> anyhow::Result<Exit> {
    use kindil::kindle::KindleHost;
    const REGULAR: &[u8] =
        include_bytes!("../crates/miqat/assets/fonts/IBMPlexSansArabic-Regular.ttf");
    let config = cli.config()?;
    let backend = slint_backend_kindle::install(REGULAR)?;
    let app = App::new(Options {
        config,
        host: Arc::new(KindleHost::open()),
        panel: Panel::Eink {
            full_refresh: backend.full_refresh_flag(),
            motion: backend.motion_flag(),
        },
        execution: Execution::Background,
        touches: Touches::new(backend.touch_counter()),
        now: chrono::Utc::now,
    })?;
    app.start();
    kindil::signals::quit_on_termination()?;
    app.window.run()?;
    app.shutdown();
    Ok(app.exit())
}

fn uevents() -> anyhow::Result<()> {
    let listener = kindil::uevent::Listener::open()?;
    loop {
        println!("{}", listener.next()?);
    }
}

fn main() -> std::process::ExitCode {
    let cli = Cli::parse();
    let result = match &cli.mode {
        Mode::Preview { dir } => preview(&cli, dir).map(|()| Exit::Quit),
        #[cfg(feature = "desktop")]
        Mode::Desktop => desktop(&cli).map(|()| Exit::Quit),
        #[cfg(feature = "kindle")]
        Mode::Kindle => kindle(&cli),
        Mode::Uevents => uevents().map(|()| Exit::Quit),
    };
    match result {
        Ok(Exit::Quit) => std::process::ExitCode::SUCCESS,
        Ok(Exit::Restart) => std::process::ExitCode::from(kindil::update::RESTART_EXIT_CODE),
        Err(error) => {
            eprintln!("kindil: {error:#}");
            std::process::ExitCode::FAILURE
        }
    }
}
