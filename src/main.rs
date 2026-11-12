use kindil::{config::Config, ui};
use slint::{ComponentHandle, Model};
use std::path::PathBuf;

fn run() -> Result<(), Box<dyn std::error::Error>> {
    if std::env::args()
        .next()
        .is_some_and(|s| s.ends_with("kindil-dhcp"))
    {
        return kindil::runtime::dhcp_hook(&std::env::args().nth(1).unwrap_or_default())
            .map_err(Into::into);
    }
    let pid1 = std::process::id() == 1;
    let mut args = std::env::args().skip(1);
    let mut preview = None;
    let mut desktop = false;
    let mut kindle = false;
    let mut config = Config::load()?;
    let mut books = config.books.clone();
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--preview" | "--sim" => {
                preview = Some(PathBuf::from(
                    args.next().ok_or("--preview requires a directory")?,
                ))
            }
            "--config" => {
                config = Config::from_path(&PathBuf::from(
                    args.next().ok_or("--config requires a file")?,
                ))?;
                books = config.books.clone();
            }
            "--miqat-config" => {
                config.miqat_config =
                    PathBuf::from(args.next().ok_or("--miqat-config requires a file")?)
            }
            "--books" => books = PathBuf::from(args.next().ok_or("--books requires a directory")?),
            "--desktop" => desktop = true,
            "--kindle" => kindle = true,
            "--version" => {
                println!("kindil {}", env!("CARGO_PKG_VERSION"));
                return Ok(());
            }
            "--help" | "-h" => {
                println!("kindil · قنديل\n\n--preview DIR  Render Slint screens without touching hardware\n--desktop      Interactive preview (build --features desktop)\n--kindle       Experimental device UI (build --features kindle)\n--config FILE  Configuration file\n--books DIR    UTF-8 .txt library\n\nNo flashing, bootloader modification, or automatic suspend.");
                return Ok(());
            }
            other => return Err(format!("unknown argument: {other}").into()),
        }
    }
    if pid1 && preview.is_none() && !desktop {
        kindle = true;
    }
    if [preview.is_some(), desktop, kindle]
        .into_iter()
        .filter(|x| *x)
        .count()
        != 1
    {
        return Err("choose exactly one of --preview DIR, --desktop, --kindle (see --help)".into());
    }
    if let Some(dir) = preview {
        std::fs::create_dir_all(&dir)?;
        let window = ui::preview::install()?;
        let app = ui::create(config.clone(), &books)?;
        if config.miqat_config.is_file() {
            let miqat = miqat::config::Config::load(&config.miqat_config)?;
            let now =
                chrono::DateTime::from_timestamp(1794398400, 0).ok_or("invalid fixture instant")?;
            app.set_miqat(kindil::dashboards::preview(&miqat, now)?);
        }
        ui::refresh_at(
            &app.window,
            &config,
            kindil::civil::unix_from_civil(2026, 11, 11, 12, 0, 0),
        );
        app.window.show()?;
        for (page, name) in [
            (0, "dashboard"),
            (1, "library"),
            (3, "wifi"),
            (4, "settings"),
        ] {
            app.window.set_page(page);
            ui::preview::snapshot(&window, &dir.join(format!("{name}.ppm")))?;
        }
        if app.window.get_books().row_count() > 0 {
            app.window.invoke_open_book(0);
            ui::preview::snapshot(&window, &dir.join("reader.ppm"))?;
        }
        app.window.set_controls_open(true);
        ui::preview::snapshot(&window, &dir.join("controls.ppm"))?;
        println!("Slint software previews: {}", dir.display());
        return Ok(());
    }
    if desktop {
        #[cfg(feature = "desktop")]
        {
            let path = config.miqat_config.clone();
            let app = ui::create(config, &books)?;
            app.start_miqat(path);
            app.window.run()?;
            return Ok(());
        }
        #[cfg(not(feature = "desktop"))]
        return Err("rebuild with --features desktop".into());
    }
    #[cfg(feature = "kindle")]
    {
        if pid1 {
            kindil::runtime::prepare_pid1()?;
        }
        let supervisor = std::rc::Rc::new(std::cell::RefCell::new(if pid1 {
            kindil::services::Supervisor::from_config(
                &config,
                std::path::Path::new("/run/kindil/log"),
            )
        } else {
            kindil::services::Supervisor::empty()
        }));
        let _backend = slint_backend_kindle::install(include_bytes!("../assets/DejaVuSans.ttf"))?;
        // No set_wake_schedule: suspend/wake is opt-in only after PW1 validation.
        let path = config.miqat_config.clone();
        let interface = config.interface.clone();
        let app = ui::create(config, &books)?;
        app.start_miqat(path);
        app.monitor_wifi(interface);
        app.enable_frontlight();
        app.window.set_status(
            kindil::battery::read(std::path::Path::new("/sys"))
                .label()
                .into(),
        );
        let service_timer = slint::Timer::default();
        let weak = app.window.as_weak();
        service_timer.start(
            slint::TimerMode::Repeated,
            std::time::Duration::from_secs(5),
            move || {
                supervisor.borrow_mut().poll(std::time::Instant::now());
                if let Some(ui) = weak.upgrade() {
                    let supervisor = supervisor.borrow();
                    let status = |name: &str| {
                        supervisor
                            .statuses()
                            .into_iter()
                            .find(|(n, _)| *n == name)
                            .map(|(_, s)| s.label())
                            .unwrap_or(if pid1 { "off" } else { "not managed" })
                    };
                    ui.set_ssh_state(status("dropbear").into());
                    ui.set_sync_state(status("syncthing").into());
                }
                if pid1 {
                    // Reap orphaned grandchildren after managed Child handles were polled.
                    while unsafe { libc::waitpid(-1, std::ptr::null_mut(), libc::WNOHANG) } > 0 {}
                }
            },
        );
        app.window.run()?;
        Ok(())
    }
    #[cfg(not(feature = "kindle"))]
    Err("rebuild with --features kindle".into())
}
fn main() {
    let result = run();
    if let Err(ref error) = result {
        eprintln!("kindil: {error}");
    }
    if std::process::id() == 1 {
        // Exiting PID 1 panics the kernel, including after a successful UI close.
        loop {
            std::thread::sleep(std::time::Duration::from_secs(60));
        }
    }
    if result.is_err() {
        std::process::exit(1);
    }
}
