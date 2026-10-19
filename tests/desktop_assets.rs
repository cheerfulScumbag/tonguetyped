//! Desktop integration assets: the app icon and the desktop entries that
//! reference it. These files are installed by the Nix package and the local
//! Flatpak test manifest, so a missing or misnamed asset only shows up on an
//! installed system; catch it here instead.

use std::path::{Path, PathBuf};

const APP_ID: &str = "io.github.cheerfulScumbag.tonguetyped";

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
