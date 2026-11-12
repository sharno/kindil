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
    app.window.invoke_rename_file(0, "renamed.txt".into());
    assert!(books.path().join("renamed.txt").exists());
    app.window.invoke_trash_file(0);
    assert!(!books.path().join("renamed.txt").exists());
    app.window.invoke_undo_trash();
    assert!(books.path().join("renamed.txt").exists());
}
