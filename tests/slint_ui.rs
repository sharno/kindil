//! Test the production component and renderer, not a reimplementation.
use kindil::{config::Config, ui};
use slint::{ComponentHandle, Model};
#[test]
fn actual_slint_miqat_reader_sliders_and_arabic() {
    let window = ui::preview::install().unwrap();
    let books = tempfile::tempdir().unwrap();
    std::fs::write(
        books.path().join("test.txt"),
        "A real book\nقنديل\n".repeat(100),
    )
    .unwrap();
    let config = Config::default();
    let app = ui::create(config, books.path()).unwrap();
    app.window.show().unwrap();
    let miqat = miqat::config::Config::load(std::path::Path::new("examples/miqat.toml")).unwrap();
    let instant = chrono::DateTime::from_timestamp(1794398400, 0).unwrap();
    let gray = kindil::dashboards::preview(&miqat, instant).unwrap();
    let expected = gray.pixels.clone();
    app.set_miqat(gray);
    let rendered = ui::preview::pixels(&window);
    assert_eq!(rendered.len(), expected.len());
    assert!(
        rendered
            .iter()
            .zip(expected)
            .all(|(p, g)| p.r == g && p.g == g && p.b == g),
        "Slint must show Miqat pixel-for-pixel without chrome/scaling"
    );
    assert_eq!(app.window.get_books().row_count(), 1);
    app.window.invoke_open_book(0);
    assert_eq!(app.window.get_page(), 2);
    assert!(app.window.get_book_text().contains("A real book"));
    app.window.invoke_turn_page(true);
    assert!(app.window.get_book_page().starts_with("2 /"));
    app.window
        .set_book_text("Scroll this line\n".repeat(100).into());
    let _ = ui::preview::pixels(&window);
    {
        use slint::platform::{PointerEventButton, WindowAdapter, WindowEvent};
        let position = slint::LogicalPosition::new(300.0, 600.0);
        window.window().dispatch_event(WindowEvent::PointerPressed {
            position,
            button: PointerEventButton::Left,
        });
        for y in [550.0, 450.0, 300.0] {
            window.window().dispatch_event(WindowEvent::PointerMoved {
                position: slint::LogicalPosition::new(300.0, y),
            });
        }
        window
            .window()
            .dispatch_event(WindowEvent::PointerReleased {
                position: slint::LogicalPosition::new(300.0, 300.0),
                button: PointerEventButton::Left,
            });
    }
    let _ = ui::preview::pixels(&window);
    assert!(
        app.window.get_reader_offset() < 0.0,
        "touch drag must scroll the reader"
    );
    app.window.invoke_turn_page(true);
    let _ = ui::preview::pixels(&window);
    assert_eq!(
        app.window.get_reader_offset(),
        0.0,
        "new page starts at top"
    );
    app.window.set_reading_font_size(36.0);
    app.window.invoke_font_size_changed(36.0);
    assert!(!app.window.get_book_text().is_empty());
    app.window.invoke_brightness_changed(80.0);
    assert!(app
        .window
        .get_brightness_status()
        .contains("no device writes"));
    app.window.set_dashboard_mode(1);
    app.window.set_page(0);
    // ZWNJ deliberately disables contextual joining. A proper shaping pipeline
    // produces a narrower normal word than its explicitly disconnected form.
    let span = |pixels: Vec<slint::Rgb8Pixel>| {
        let xs = (28..300)
            .filter(|x| (28..68).any(|y| pixels[y * 758 + x].r < 128))
            .collect::<Vec<_>>();
        xs.last().unwrap() - xs.first().unwrap()
    };
    app.window.set_brand("قنديل".into());
    let frame = ui::preview::pixels(&window);
    assert_eq!(
        frame[0],
        slint::Rgb8Pixel {
            r: 255,
            g: 255,
            b: 255
        },
        "fresh preview buffers must repaint unchanged background too"
    );
    let joined = span(frame);
    app.window
        .set_brand("ق\u{200c}ن\u{200c}د\u{200c}ي\u{200c}ل".into());
    let disconnected = span(ui::preview::pixels(&window));
    assert!(
        joined < disconnected,
        "Arabic must use contextual joining ({joined} vs {disconnected})"
    );
    // Do not leak the deliberately broken fixture into later workflow previews.
    app.window.set_brand("kindil · قنديل".into());
    app.window.invoke_rename_file(0, "renamed.txt".into());
    assert!(books.path().join("renamed.txt").exists());
    app.window.invoke_trash_file(0);
    assert!(!books.path().join("renamed.txt").exists());
    app.window.invoke_undo_trash();
    assert!(books.path().join("renamed.txt").exists());
    touch_entry(&app, &window);
    app.window.set_page(1);
    app.window.set_selected_file(-1);
    app.window.set_file_status("".into());
    let _ = ui::preview::pixels(&window);
    ui::preview::tap(&window, 690.0, 210.0); // actual file-actions button
    assert_eq!(app.window.get_selected_file(), 0);
    app.window.set_new_filename("".into());
    let _ = ui::preview::pixels(&window);
    ui::preview::tap(&window, 140.0, 834.0);
    let _ = ui::preview::pixels(&window);
    assert!(
        app.window.get_keyboard_visible(),
        "rename field supports touch entry"
    );
    for (x, y) in [
        (109.0, 742.0),
        (595.0, 870.0),
        (354.0, 742.0),
        (161.0, 870.0),
        (354.0, 742.0),
    ] {
        ui::preview::tap(&window, x, y);
        let _ = ui::preview::pixels(&window);
    }
    assert_eq!(app.window.get_new_filename(), "q.txt");
    ui::preview::tap(&window, 721.0, 806.0); // Return dismisses keyboard
    let _ = ui::preview::pixels(&window);
    assert!(!app.window.get_keyboard_visible());
    ui::preview::tap(&window, 180.0, 868.0); // actual Rename button
    assert_eq!(
        std::fs::read_to_string(books.path().join("q.txt")).unwrap(),
        "A real book\nقنديل\n".repeat(100)
    );
}

fn touch_entry(app: &ui::App, window: &slint::platform::software_renderer::MinimalSoftwareWindow) {
    use std::{cell::RefCell, rc::Rc};
    let captured = Rc::new(RefCell::new(None));
    let result = captured.clone();
    // Fixture interception proves exact values without opening a real socket.
    app.window.on_connect_wifi(move |ssid, password| {
        *result.borrow_mut() = Some((ssid.to_string(), password.to_string()));
    });
    app.window.set_controls_open(false);
    app.window.set_page(3);
    let _ = ui::preview::pixels(window);
    ui::preview::tap(window, 140.0, 250.0);
    let _ = ui::preview::pixels(window);
    assert!(
        app.window.get_keyboard_visible(),
        "tapping SSID opens keyboard"
    );
    let tap = |x, y| {
        ui::preview::tap(window, x, y);
        let _ = ui::preview::pixels(window);
    };
    tap(109.0, 742.0); // q
    tap(171.0, 742.0); // w
    assert_eq!(app.window.get_ssid(), "qw");
    tap(683.0, 934.0); // cursor left
    tap(232.0, 742.0); // e in middle
    assert_eq!(app.window.get_ssid(), "qew");
    tap(721.0, 742.0); // backspace
    assert_eq!(app.window.get_ssid(), "qw");
    tap(37.0, 870.0); // shift
    tap(109.0, 742.0);
    assert_eq!(app.window.get_ssid(), "qQw");
    tap(75.0, 934.0); // symbols
    tap(109.0, 742.0); // 1
    assert_eq!(app.window.get_ssid(), "qQ1w");
    tap(75.0, 934.0); // Arabic
    tap(293.0, 742.0); // ق
    assert_eq!(app.window.get_ssid(), "qQ1قw");
    tap(721.0, 742.0); // delete one Unicode scalar, not one byte
    assert_eq!(app.window.get_ssid(), "qQ1w");
    // Enter a whole joined word through actual Arabic key taps, not a setter.
    app.window.set_ssid("".into());
    for (x, y) in [
        (293.0, 742.0),
        (537.0, 806.0),
        (285.0, 870.0),
        (232.0, 806.0),
        (354.0, 806.0),
    ] {
        tap(x, y);
    }
    assert_eq!(app.window.get_ssid(), "قنديل");
    if let Some(dir) = std::env::var_os("KINDIL_TEST_PREVIEW_DIR") {
        let dir = std::path::PathBuf::from(dir);
        std::fs::create_dir_all(&dir).unwrap();
        ui::preview::snapshot(window, &dir.join("arabic-keyboard.ppm")).unwrap();
    }
    app.window.set_ssid("qQ1w".into());
    app.window
        .global::<ui::VirtualKeyboardHandler>()
        .invoke_key_pressed(slint::platform::Key::End.into());
    app.window
        .global::<ui::VirtualKeyboardHandler>()
        .invoke_key_pressed(slint::platform::Key::LeftArrow.into());
    tap(75.0, 934.0); // wrap back to Latin
    tap(109.0, 742.0);
    assert_eq!(app.window.get_ssid(), "qQ1qw");
    tap(42.0, 742.0); // Esc dismisses and removes field focus
    assert!(!app.window.get_keyboard_visible());
    tap(140.0, 250.0); // same field can reopen keyboard
    assert!(app.window.get_keyboard_visible());
    tap(140.0, 282.0); // password field
    for x in [109.0, 171.0, 232.0, 293.0, 354.0, 415.0, 476.0, 537.0] {
        tap(x, 742.0);
    }
    if let Some(dir) = std::env::var_os("KINDIL_TEST_PREVIEW_DIR") {
        let dir = std::path::PathBuf::from(dir);
        std::fs::create_dir_all(&dir).unwrap();
        ui::preview::snapshot(window, &dir.join("password-keyboard.ppm")).unwrap();
    }
    tap(140.0, 316.0); // Connect remains reachable above keyboard
    assert!(!app.window.get_keyboard_visible());
    assert_eq!(
        *captured.borrow(),
        Some(("qQ1qw".into(), "qwertyui".into()))
    );
    tap(140.0, 316.0);
    assert_eq!(
        *captured.borrow(),
        Some(("qQ1qw".into(), String::new())),
        "password field is cleared immediately after submission"
    );

    // Every printable ASCII character is reachable in the upstream key model.
    let mut available = std::collections::HashSet::from([' ']);
    for layout in app
        .window
        .global::<ui::VirtualKeyboardHandler>()
        .get_default_key_sets()
        .iter()
    {
        for row in layout.iter() {
            for key in row.iter() {
                available.extend(key.key.chars());
                available.extend(key.shift_key.chars());
            }
        }
    }
    for byte in 32u8..=126 {
        assert!(
            available.contains(&char::from(byte)),
            "missing ASCII key {byte}"
        );
    }
}
