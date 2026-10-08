//! Identifies exactly which build of TongueTyped is running. The Cargo package
//! version alone rarely changes between commits, so it can't tell an installed
//! binary or a long-running daemon apart from a freshly built one; the git
//! commit captured by `build.rs` can.

/// Short git commit this binary was built from, with a `-dirty` suffix for
/// uncommitted changes, or `"unknown"` when built outside a git checkout
/// without `TONGUETYPED_BUILD_COMMIT` set (see `build.rs`).
pub const COMMIT: &str = env!("TONGUETYPED_BUILD_COMMIT");

/// Full build identifier: package version plus commit, e.g. `0.1.0 (a1b2c3d)`.
/// Shown by `--version`, reported by the daemon's `status`, and compared by
/// `doctor` to detect a daemon left running from an older build.
pub const VERSION: &str = concat!(
    env!("CARGO_PKG_VERSION"),
    " (",
    env!("TONGUETYPED_BUILD_COMMIT"),
    ")"
);
