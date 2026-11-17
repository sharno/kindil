//! The installer against fake Kindle volumes in temporary folders.
use kindil_installer::check::{self, Status};
use kindil_installer::detect::{self, looks_like_kindle};
use kindil_installer::install::{self, Event};
use kindil_installer::model::Model;
use kindil_installer::release::Release;
use std::path::Path;

/// A Kindle's storage as drive mode shows it, on `firmware`.
fn kindle(firmware: &str) -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    for folder in ["documents", "system", "audible", "fonts"] {
        std::fs::create_dir(root.join(folder)).unwrap();
    }
    std::fs::write(
        root.join("system/version.txt"),
        format!("Kindle {firmware} (2689890035)\n"),
    )
    .unwrap();
    dir
}

fn put(root: &Path, path: &str, body: &[u8]) {
    let path = root.join(path);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, body).unwrap();
}

/// Jailbroken, with KUAL's booklet and MRPI.
fn with_kual(root: &Path) {
    put(root, "documents/KUAL.kual", b"");
    std::fs::create_dir_all(root.join("extensions/MRInstaller")).unwrap();
    std::fs::create_dir_all(root.join("mrpackages")).unwrap();
}

fn status(checks: &[check::Check], title: &str) -> Status {
    checks
        .iter()
        .find(|c| c.title.starts_with(title))
        .unwrap_or_else(|| panic!("no {title} check"))
        .status
}

#[test]
fn a_jailbroken_pw1_with_kual_is_ready() {
    let dir = kindle("5.6.1.1");
    with_kual(dir.path());
    assert!(looks_like_kindle(dir.path()));
    let found = detect::inspect(dir.path(), Some("B0241234567890AB"));
    assert!(found.model.is_paperwhite1());
    assert_eq!(found.firmware.as_ref().unwrap().version, "5.6.1.1");
    assert_eq!(found.prereqs.kual, ["documents/KUAL.kual"]);
    assert!(found.prereqs.jailbroken());
    assert_eq!(found.installed, None);
    let checks = check::checklist(&found);
    assert!(
        checks.iter().all(|c| c.status == Status::Pass),
        "{checks:#?}"
    );
    assert!(check::ready(&checks, false));
}

#[test]
fn refuses_another_kindle_model() {
    let dir = kindle("5.12.2.2");
    with_kual(dir.path());
    let found = detect::inspect(dir.path(), Some("90D4000000000000"));
    assert_eq!(
        found.model,
        Model::Other {
            name: "Kindle Paperwhite 2 (2013)".into()
        }
    );
    let checks = check::checklist(&found);
    assert_eq!(status(&checks, "Kindle Paperwhite"), Status::Fail);
    assert!(checks[0]
        .detail
        .contains("only runs on the first-generation"));
    assert!(
        !check::ready(&checks, true),
        "--force doesn't override a known model"
    );
}

#[test]
fn an_unknown_model_needs_force() {
    let dir = kindle("5.6.1.1");
    with_kual(dir.path());
    let found = detect::inspect(dir.path(), None);
    let checks = check::checklist(&found);
    assert_eq!(status(&checks, "Kindle Paperwhite"), Status::Unknown);
    assert!(!check::ready(&checks, false));
    assert!(check::ready(&checks, true));
}

#[test]
fn a_plain_drive_is_no_kindle() {
    let dir = tempfile::tempdir().unwrap();
    put(dir.path(), "photos/a.jpg", b"x");
    assert!(!looks_like_kindle(dir.path()));
    // A Kindle without version.txt (older firmware) still is one.
    std::fs::create_dir(dir.path().join("documents")).unwrap();
    assert!(!looks_like_kindle(dir.path()));
    std::fs::create_dir(dir.path().join("system")).unwrap();
    assert!(looks_like_kindle(dir.path()));
    assert_eq!(detect::inspect(dir.path(), None).firmware, None);
}

#[test]
fn without_a_jailbreak_points_to_the_pw1_instructions() {
    let dir = kindle("5.6.1.1");
    let found = detect::inspect(dir.path(), Some("B024AAAA"));
    let checks = check::checklist(&found);
    assert_eq!(status(&checks, "Jailbreak"), Status::Fail);
    assert_eq!(status(&checks, "KUAL"), Status::Fail);
    let jailbreak = &checks[1];
    assert!(jailbreak.detail.contains("only runs code Amazon signed"));
    assert!(jailbreak.detail.contains("downgrade to 5.3.3"));
    assert_eq!(jailbreak.links[0], check::DOWNGRADE);
    assert!(jailbreak.links.contains(&check::JAILBREAK));
    assert!(checks[2].links.contains(&check::KUAL));
    assert!(!check::ready(&checks, true));

    // On 5.4.4.2 the classic jailbreak applies as it is.
    let dir = kindle("5.4.4.2");
    let checks = check::checklist(&detect::inspect(dir.path(), Some("B024AAAA")));
    assert_eq!(checks[1].links[0], check::JAILBREAK);
}

#[test]
fn a_jailbreak_without_kual_asks_for_kual() {
    let dir = kindle("5.3.3");
    std::fs::create_dir_all(dir.path().join("usbnet")).unwrap();
    put(dir.path(), "documents/hotfix.run_hotfix", b"");
    let found = detect::inspect(dir.path(), Some("B01D0000"));
    assert_eq!(found.prereqs.jailbreak.len(), 2, "{:?}", found.prereqs);
    let checks = check::checklist(&found);
    assert_eq!(status(&checks, "Jailbreak"), Status::Pass);
    assert_eq!(status(&checks, "KUAL"), Status::Fail);
}

#[test]
fn finds_the_kindlet_kual_too() {
    let dir = kindle("5.6.1.1");
    put(dir.path(), "documents/KUAL-KDK-2.0.azw2", b"");
    let found = detect::inspect(dir.path(), Some("B024"));
    assert_eq!(found.prereqs.kual, ["documents/KUAL-KDK-2.0.azw2"]);
    assert!(check::ready(&check::checklist(&found), false));
}

/// A source tarball as GitHub makes it for tag `v{version}`.
fn source(version: &str, extra: &[(&str, &str)]) -> Vec<u8> {
    let mut out = tar::Builder::new(flate2::write::GzEncoder::new(
        Vec::new(),
        flate2::Compression::fast(),
    ));
    let top = format!("kindil-{version}");
    let mut header = tar::Header::new_gnu();
    header.set_entry_type(tar::EntryType::Directory);
    header.set_size(0);
    header.set_mode(0o755);
    out.append_data(&mut header, format!("{top}/"), std::io::empty())
        .unwrap();
    let files = [
        ("kindle/extensions/kindil/menu.json", "{\"items\": []}"),
        ("kindle/extensions/kindil/config.xml", "<extension/>"),
        (
            "kindle/extensions/kindil/bin/start.sh",
            "#!/bin/sh\necho start\n",
        ),
        (
            "kindle/extensions/kindil/bin/kindil",
            "a stale binary in git",
        ),
        ("kindle/extensions/kindil/boot/install.sh", "#!/bin/sh\n"),
        ("crates/miqat/kindle/extensions/miqat/menu.json", "{}"),
        ("examples/miqat.toml", "[location]\nlatitude = 1.0\n"),
        (
            "examples/kindil.toml",
            "dashboard = \"miqat\"\nmiqat_config = \"examples/miqat.toml\"\n",
        ),
        ("src/main.rs", "fn main() {}"),
    ];
    for (path, body) in files.iter().chain(extra) {
        let mut header = tar::Header::new_gnu();
        header.set_size(body.len() as u64);
        header.set_mode(0o644);
        out.append_data(&mut header, format!("{top}/{path}"), body.as_bytes())
            .unwrap();
    }
    out.into_inner().unwrap().finish().unwrap()
}

fn binary(version: &str) -> Vec<u8> {
    format!("\x7fELF arm binary ... kindil-version={version}\n ...").into_bytes()
}

fn release(version: &str) -> Release {
    Release::new(version.into(), binary(version), &source(version, &[])).unwrap()
}

#[test]
fn takes_the_launcher_and_examples_from_the_tag() {
    let release = release("0.13.2");
    let paths: Vec<String> = release
        .extension
        .iter()
        .map(|f| f.path.display().to_string().replace('\\', "/"))
        .collect();
    assert_eq!(
        paths,
        [
            "extensions/kindil/bin/start.sh",
            "extensions/kindil/boot/install.sh",
            "extensions/kindil/config.xml",
            "extensions/kindil/menu.json",
            "extensions/kindil/bin/kindil",
        ],
        "the release binary last, git's stale one never"
    );
    assert_eq!(release.extension[4].bytes, binary("0.13.2"));
    let kindil_toml = String::from_utf8(release.examples[0].bytes.clone()).unwrap();
    assert_eq!(release.examples[0].path, Path::new("kindil/kindil.toml"));
    assert!(kindil_toml.contains("# miqat_config = \"examples/miqat.toml\""));
    assert_eq!(release.examples[1].path, Path::new("kindil/miqat.toml"));
}

#[test]
fn refuses_a_tarball_with_unsafe_paths() {
    // tar::Builder refuses "..", so the entry is written by hand.
    let mut header = tar::Header::new_gnu();
    header.set_size(1);
    header.set_mode(0o644);
    header.as_gnu_mut().unwrap().name[..17].copy_from_slice(b"kindil-1/../evil\0");
    header.set_cksum();
    let mut raw = header.as_bytes().to_vec();
    raw.extend_from_slice(&[b'x'; 512]);
    raw.extend_from_slice(&[0; 1024]);
    let mut gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
    std::io::Write::write_all(&mut gz, &raw).unwrap();
    let error = Release::new("1.0.0".into(), vec![], &gz.finish().unwrap()).unwrap_err();
    assert!(error.to_string().contains("unsafe path"), "{error}");
    // And one without the launcher isn't a kindil release.
    let mut empty = tar::Builder::new(flate2::write::GzEncoder::new(
        Vec::new(),
        flate2::Compression::fast(),
    ));
    let mut header = tar::Header::new_gnu();
    header.set_size(0);
    empty
        .append_data(&mut header, "x/README.md", std::io::empty())
        .unwrap();
    let tgz = empty.into_inner().unwrap().finish().unwrap();
    assert!(Release::new("1.0.0".into(), vec![], &tgz).is_err());
}

fn read(root: &Path, path: &str) -> Vec<u8> {
    std::fs::read(root.join(path)).unwrap_or_else(|e| panic!("{path}: {e}"))
}

fn leftovers(dir: &Path) -> Vec<String> {
    let mut found = Vec::new();
    for entry in std::fs::read_dir(dir).unwrap().flatten() {
        let path = entry.path();
        if path.is_dir() {
            found.extend(leftovers(&path));
        } else if path.extension().is_some_and(|e| e == "part") {
            found.push(path.display().to_string());
        }
    }
    found
}

#[test]
fn installs_into_a_fresh_kindle() {
    let dir = kindle("5.6.1.1");
    let root = dir.path();
    with_kual(root);
    let release = release("0.13.2");
    let mut events = Vec::new();
    let report = install::install(root, &release, |e| events.push(e.clone())).unwrap();
    assert_eq!(report.written.len(), 7, "{report:?}");
    assert!(report.kept.is_empty());
    assert_eq!(read(root, "extensions/kindil/bin/kindil"), binary("0.13.2"));
    assert_eq!(
        read(root, "extensions/kindil/menu.json"),
        b"{\"items\": []}"
    );
    assert_eq!(
        read(root, "kindil/miqat.toml"),
        b"[location]\nlatitude = 1.0\n"
    );
    assert!(leftovers(root).is_empty(), "{:?}", leftovers(root));
    assert!(matches!(
        events.last(),
        Some(Event::Wrote { n: 7, of: 7, .. })
    ));
    // KUAL's own things are untouched.
    assert!(root.join("documents/KUAL.kual").exists());
    assert!(root.join("extensions/MRInstaller").is_dir());
    let installed = detect::installed(root).unwrap();
    assert_eq!(installed.version.as_deref(), Some("0.13.2"));
    assert_eq!(
        installed.config,
        ["kindil/miqat.toml", "kindil/kindil.toml"]
    );
}

#[test]
fn upgrades_and_keeps_the_settings() {
    let dir = kindle("5.6.1.1");
    let root = dir.path();
    with_kual(root);
    install::install(root, &release("0.12.0"), |_| {}).unwrap();
    // The owner's own settings, and an update kindil staged but never ran.
    put(root, "kindil/miqat.toml", b"[location]\nlatitude = 30.04\n");
    std::fs::remove_file(root.join("kindil/kindil.toml")).unwrap();
    put(root, "kindil/prefs.toml", b"home = \"clock\"\n");
    put(root, "extensions/kindil/bin/kindil.next", b"0.12.1, staged");
    put(root, "extensions/kindil/bin/kindil.previous", b"0.11.0");

    let found = detect::inspect(root, Some("B024"));
    let installed = found.installed.as_ref().unwrap();
    assert_eq!(installed.version.as_deref(), Some("0.12.0"));
    assert_eq!(installed.config, ["kindil/miqat.toml"]);

    let mut events = Vec::new();
    let report = install::install(root, &release("0.13.2"), |e| events.push(e.clone())).unwrap();
    assert_eq!(report.kept, [Path::new("kindil/miqat.toml")]);
    assert_eq!(
        report.removed,
        [Path::new("extensions/kindil/bin/kindil.next")]
    );
    assert_eq!(
        read(root, "kindil/miqat.toml"),
        b"[location]\nlatitude = 30.04\n",
        "the owner's settings stay"
    );
    assert_eq!(read(root, "kindil/prefs.toml"), b"home = \"clock\"\n");
    assert!(
        root.join("kindil/kindil.toml").exists(),
        "a missing settings file is created"
    );
    assert_eq!(read(root, "extensions/kindil/bin/kindil"), binary("0.13.2"));
    assert_eq!(
        read(root, "extensions/kindil/bin/kindil.previous"),
        b"0.11.0"
    );
    assert!(!root.join("extensions/kindil/bin/kindil.next").exists());
    assert_eq!(
        detect::installed(root).unwrap().version.as_deref(),
        Some("0.13.2")
    );
    assert!(events.contains(&Event::Kept {
        path: "kindil/miqat.toml".into()
    }));
}

#[test]
fn a_vanished_volume_is_an_error() {
    let dir = kindle("5.6.1.1");
    let gone = dir.path().join("unplugged");
    let error = install::install(&gone, &release("0.13.2"), |_| {}).unwrap_err();
    assert!(error.to_string().contains("unplugged"), "{error}");
}

/// The real thing: the latest release from GitHub, its signature checked.
#[test]
#[ignore = "network: downloads the latest release from GitHub"]
fn downloads_and_verifies_the_latest_release() {
    let github = kindil_installer::release::Github::default();
    let version = github.latest().unwrap();
    let release = github.download(&version, |_, _, _| {}).unwrap();
    let binary = &release.extension.last().unwrap().bytes;
    assert!(binary.starts_with(b"\x7fELF"));
    // Releases before the marker (0.13.2 and older) don't say.
    if let Some(found) = detect::binary_version(binary) {
        assert_eq!(found, version);
    }
    assert!(release
        .extension
        .iter()
        .any(|f| f.path == Path::new("extensions/kindil/bin/start.sh")));
    assert_eq!(release.examples.len(), 2);
    let dir = kindle("5.6.1.1");
    install::install(dir.path(), &release, |_| {}).unwrap();
}
