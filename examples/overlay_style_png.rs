//! Offline renderer for the layer-shell overlay styles (src/overlay.rs).
//!
//! Where `examples/overlay_preview.rs` drives the real Wayland actor on a live
//! compositor, this renders the overlay's pixels directly to PNG files, with
//! no display needed - for design review (especially the animated `blob`
//! plasma style) and for checking a look on a machine without wlr-layer-shell.
//!
//! Run with `cargo run --example overlay_style_png -- <output-dir>` (defaults
//! to `overlay-preview` in the current directory). Every style is written once
//! per phase; the `blob`, `border`, and `half-circle` styles additionally get
//! frames across one animation cycle, for both the default and streaming
//! treatments. The full-screen `border` frames are written at their
//! representative preview size rather than a compositor-provided output size.

use std::fs;
use std::path::{Path, PathBuf};

use tonguetyped::overlay::render_frame_pixels;

const STYLES: [&str; 6] = ["badge", "minimal", "pill", "blob", "border", "half-circle"];
const PHASES: [&str; 6] = [
    "recording",
    "transcribing",
    "success",
    "no-speech",
    "cancelled",
    "error",
];

fn main() -> std::io::Result<()> {
    let out = std::env::args()
        .nth(1)
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("overlay-preview"));
    fs::create_dir_all(&out)?;

    for style in STYLES {
        for phase in PHASES {
            let (width, height, pixels) = render_frame_pixels(style, Some(phase), 0.0, false);
            write_png(
                &out.join(format!("{style}-{phase}.png")),
                width,
                height,
                &pixels,
            )?;
        }
    }

    for (index, t) in [0.0_f32, 0.25, 0.5, 0.75].iter().enumerate() {
        for style in ["blob", "border", "half-circle"] {
            for (label, streaming) in [("pulse", false), ("streaming", true)] {
                let (width, height, pixels) =
                    render_frame_pixels(style, Some("recording"), *t, streaming);
                write_png(
                    &out.join(format!("{style}-recording-{label}-{index}.png")),
                    width,
                    height,
                    &pixels,
                )?;
            }
        }
    }

    println!("wrote overlay style preview frames to {}", out.display());
    Ok(())
}

fn write_png(path: &Path, width: u32, height: u32, rgba: &[u8]) -> std::io::Result<()> {
    let mut out = Vec::new();
    out.extend_from_slice(&[0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A]);

    let mut ihdr = Vec::new();
    ihdr.extend_from_slice(&width.to_be_bytes());
    ihdr.extend_from_slice(&height.to_be_bytes());
    ihdr.extend_from_slice(&[8, 6, 0, 0, 0]); // RGBA8, no compression/filter/interlace
    write_chunk(&mut out, b"IHDR", &ihdr);

    // PNG scanlines are each prefixed by a filter byte; filter 0 is "none".
    // The canvas buffer is wl_shm Argb8888 (little-endian BGRA), so reorder to
    // PNG's RGBA.
    let mut raw = Vec::with_capacity((height * (1 + width * 4)) as usize);
    for y in 0..height {
        raw.push(0);
        for x in 0..width {
            let index = ((y * width + x) * 4) as usize;
            raw.extend_from_slice(&[
                rgba[index + 2],
                rgba[index + 1],
                rgba[index],
                rgba[index + 3],
            ]);
        }
    }
    write_chunk(&mut out, b"IDAT", &zlib_stored(&raw));
    write_chunk(&mut out, b"IEND", &[]);

    fs::write(path, out)
}

fn write_chunk(out: &mut Vec<u8>, kind: &[u8; 4], data: &[u8]) {
    out.extend_from_slice(&(data.len() as u32).to_be_bytes());
    out.extend_from_slice(kind);
    out.extend_from_slice(data);
    let mut crc_input = Vec::with_capacity(4 + data.len());
    crc_input.extend_from_slice(kind);
    crc_input.extend_from_slice(data);
    out.extend_from_slice(&crc32(&crc_input).to_be_bytes());
}

/// Wraps `raw` in a zlib stream using stored (uncompressed) deflate blocks -
/// no compressor dependency for a handful of small preview frames.
fn zlib_stored(raw: &[u8]) -> Vec<u8> {
    let mut out = vec![0x78, 0x01]; // CMF/FLG: default window, no preset dict
    if raw.is_empty() {
        out.extend_from_slice(&[0x01, 0x00, 0x00, 0xFF, 0xFF]);
    }
    let mut chunks = raw.chunks(65535).peekable();
    while let Some(chunk) = chunks.next() {
        let final_block = chunks.peek().is_none();
        out.push(u8::from(final_block));
        let len = chunk.len() as u16;
        out.extend_from_slice(&len.to_le_bytes());
        out.extend_from_slice(&(!len).to_le_bytes());
        out.extend_from_slice(chunk);
    }
    out.extend_from_slice(&adler32(raw).to_be_bytes());
    out
}

fn crc32(data: &[u8]) -> u32 {
    let mut crc = 0xFFFF_FFFFu32;
    for &byte in data {
        crc ^= byte as u32;
        for _ in 0..8 {
            let mask = (crc & 1).wrapping_neg();
            crc = (crc >> 1) ^ (0xEDB8_8320 & mask);
        }
    }
    !crc
}

fn adler32(data: &[u8]) -> u32 {
    const MOD: u32 = 65521;
    let (mut a, mut b) = (1u32, 0u32);
    for &byte in data {
        a = (a + byte as u32) % MOD;
        b = (b + a) % MOD;
    }
    (b << 16) | a
}
