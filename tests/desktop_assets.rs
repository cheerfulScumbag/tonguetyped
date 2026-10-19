//! Desktop integration assets: the app icon and the desktop entries that
//! reference it. These files are installed by the Nix, Flatpak, Debian, RPM,
//! and AUR packages, so a missing or misnamed asset only shows up on an
//! installed system; catch it here instead.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const APP_ID: &str = "io.github.cheerfulScumbag.tonguetyped";
const ICON_SOURCE: &str =
    "data/icons/hicolor/scalable/apps/io.github.cheerfulScumbag.tonguetyped.svg";
const ICON_INSTALL_PATH: &str =
    "usr/share/icons/hicolor/scalable/apps/io.github.cheerfulScumbag.tonguetyped.svg";

fn manifest_dir() -> &'static Path {
    Path::new(env!("CARGO_MANIFEST_DIR"))
}

fn unique_dir(tag: &str) -> PathBuf {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let path = std::env::temp_dir().join(format!(
        "tonguetyped-desktop-assets-{tag}-{}-{nanos}",
        std::process::id()
    ));
    std::fs::create_dir_all(&path).unwrap();
    path
}

fn run(command: &mut Command) -> Output {
    let description = format!("{command:?}");
    let output = command
        .output()
        .unwrap_or_else(|error| panic!("failed to run {description}: {error}"));
    assert!(
        output.status.success(),
        "{description}\nstdout: {}\nstderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    output
}

fn data_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("data")
}

fn icon_path() -> PathBuf {
    data_dir()
        .join("icons/hicolor/scalable/apps")
        .join(format!("{APP_ID}.svg"))
}

fn attribute<'a>(document: &'a str, name: &str) -> &'a str {
    let marker = format!("{name}=\"");
    let start = document
        .find(&marker)
        .unwrap_or_else(|| panic!("missing {name} attribute"))
        + marker.len();
    let rest = &document[start..];
    &rest[..rest.find('"').expect("unterminated attribute")]
}

#[test]
fn app_icon_is_a_square_transparent_svg() {
    let icon = icon_path();
    let svg = std::fs::read_to_string(&icon)
        .unwrap_or_else(|error| panic!("{} must exist: {error}", icon.display()));

    assert_eq!(attribute(&svg, "width"), attribute(&svg, "height"));

    let view_box = attribute(&svg, "viewBox")
        .split_whitespace()
        .map(|value| value.parse::<f64>().expect("numeric viewBox"))
        .collect::<Vec<_>>();
    assert_eq!(view_box.len(), 4);
    assert_eq!(view_box[2], view_box[3], "the viewBox must be square");

    // The background stays transparent when the icon is composited: the art
    // is paths only, with no canvas-filling rectangle or embedded raster.
    assert!(
        !svg.contains("<rect"),
        "the icon must not paint a background"
    );
    assert!(!svg.contains("<image"), "the icon must be vector artwork");
}

#[test]
fn every_desktop_entry_uses_the_app_icon_that_resolves() {
    let icon = icon_path();
    assert!(icon.is_file(), "{} must exist", icon.display());

    let mut entries = 0;
    let data = data_dir();
    for entry in std::fs::read_dir(&data).expect("read data directory") {
        let path = entry.expect("data entry").path();
        if path.extension().and_then(|value| value.to_str()) != Some("desktop") {
            continue;
        }
        entries += 1;

        let contents = std::fs::read_to_string(&path).expect("read desktop entry");
        let icon_line = contents
            .lines()
            .find_map(|line| line.strip_prefix("Icon="))
            .unwrap_or_else(|| panic!("{} has no Icon= line", path.display()));
        assert_eq!(
            icon_line,
            APP_ID,
            "{} must reference the app-ID icon",
            path.display()
        );
        assert!(
            icon.is_file(),
            "{} references {APP_ID}, which has no icon at {}",
            path.display(),
            icon.display()
        );
    }

    assert!(
        entries > 0,
        "no desktop entries found in {}",
        data.display()
    );
}

#[test]
fn deb_and_rpm_packages_install_the_app_icon() {
    let manifest_path = manifest_dir().join("Cargo.toml");
    let manifest: toml::Value =
        toml::from_str(&std::fs::read_to_string(&manifest_path).expect("read Cargo.toml"))
            .expect("parse Cargo.toml");
    let metadata = &manifest["package"]["metadata"];

    let deb_assets = metadata["deb"]["assets"].as_array().expect("deb assets");
    assert!(
        deb_assets.iter().any(|asset| {
            asset.as_array().is_some_and(|entry| {
                entry.first().and_then(toml::Value::as_str) == Some(ICON_SOURCE)
                    && entry.get(1).and_then(toml::Value::as_str) == Some(ICON_INSTALL_PATH)
            })
        }),
        "the .deb must install {ICON_INSTALL_PATH}"
    );

    let rpm_destination = format!("/{ICON_INSTALL_PATH}");
    let rpm_assets = metadata["generate-rpm"]["assets"]
        .as_array()
        .expect("rpm assets");
    assert!(
        rpm_assets.iter().any(|asset| {
            asset.get("source").and_then(toml::Value::as_str) == Some(ICON_SOURCE)
                && asset.get("dest").and_then(toml::Value::as_str) == Some(rpm_destination.as_str())
        }),
        "the .rpm must install {rpm_destination}"
    );
}

#[test]
fn arch_release_bundle_and_pkgbuild_install_the_app_icon() {
    let root = unique_dir("arch");
    let dist = root.join("dist");
    std::fs::create_dir_all(dist.join("completions")).unwrap();
    std::fs::create_dir_all(dist.join("man")).unwrap();
    for completion in ["tonguetyped.bash", "_tonguetyped", "tonguetyped.fish"] {
        std::fs::write(dist.join("completions").join(completion), "").unwrap();
    }
    std::fs::write(dist.join("man/tonguetyped.1"), "").unwrap();

    let pkgbuild = manifest_dir().join("PKGBUILD");
    let pkgver = String::from_utf8(
        run(Command::new("bash")
            .arg("-c")
            .arg("source \"$0\"; printf '%s' \"$pkgver\"")
            .arg(&pkgbuild))
        .stdout,
    )
    .unwrap();
    let tag = format!("v{pkgver}");

    let assets = root.join("assets");
    run(Command::new("bash")
        .arg(manifest_dir().join("scripts/bundle-release.sh"))
        .arg(&tag)
        .arg(&dist)
        .arg(&assets));

    let tarball = assets.join(format!("tonguetyped-{tag}-completions-and-man.tar.gz"));
    let srcdir = root.join("srcdir");
    std::fs::create_dir_all(&srcdir).unwrap();
    run(Command::new("tar")
        .arg("-xzf")
        .arg(&tarball)
        .arg("-C")
        .arg(&srcdir));
    std::fs::write(
        srcdir.join(format!("tonguetyped-{tag}-linux-x86_64-gpu-vulkan")),
        "",
    )
    .unwrap();

    let pkgdir = root.join("pkgdir");
    run(Command::new("bash")
        .arg("-c")
        .arg("source \"$PKGBUILD\"; package")
        .env("PKGBUILD", &pkgbuild)
        .env("srcdir", &srcdir)
        .env("pkgdir", &pkgdir));

    let installed = pkgdir.join(ICON_INSTALL_PATH);
    assert_eq!(
        std::fs::read(&installed).unwrap_or_else(|error| panic!(
            "the AUR package must install {}: {error}",
            installed.display()
        )),
        std::fs::read(icon_path()).unwrap()
    );

    std::fs::remove_dir_all(root).unwrap();
}
