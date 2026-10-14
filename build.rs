use std::path::{Path, PathBuf};
use std::process::Command;

// The CLI lives in `src/cli.rs`, a plain library module that depends only on
// clap, so the build script attaches it with `#[path]` and renders the man
// pages and shell completions from the exact same definition `--help` shows,
// instead of a second, driftable copy. `cli.rs` reads its version from
// `crate::build_info::VERSION`, which the build script has no module for, so
// the stub below supplies the crate version.
#[path = "src/cli.rs"]
#[allow(dead_code, unused_imports)]
mod cli;

mod build_info {
    pub const VERSION: &str = env!("CARGO_PKG_VERSION");
}

use clap::{Command as ClapCommand, CommandFactory};
use clap_complete::{generate_to, Shell};
use clap_mangen::Man;

fn main() {
    emit_build_commit();
    link_cuda();
    generate_dist();
}

/// Exposes the git commit this binary was built from as
/// `TONGUETYPED_BUILD_COMMIT` (read by `src/build_info.rs`), so `--version`,
/// `status`, and `doctor` can tell one build from another even though the
/// Cargo package version rarely changes. A packager that builds without a
/// `.git` directory (the Nix flake, whose source copy has none) passes the
/// commit in through the same-named environment variable instead; with
/// neither, the commit is reported as "unknown".
fn emit_build_commit() {
    println!("cargo:rerun-if-env-changed=TONGUETYPED_BUILD_COMMIT");
    // Declaring any rerun-if-changed replaces Cargo's default "rerun on any
    // package file change", so list what the commit/dirty state depends on.
    for path in ["build.rs", "src", "Cargo.toml", "Cargo.lock"] {
        println!("cargo:rerun-if-changed={path}");
    }

    let commit = std::env::var("TONGUETYPED_BUILD_COMMIT")
        .ok()
        .filter(|value| !value.trim().is_empty())
        .or_else(git_commit)
        .unwrap_or_else(|| "unknown".to_string());
    println!("cargo:rustc-env=TONGUETYPED_BUILD_COMMIT={}", commit.trim());
}

fn git_commit() -> Option<String> {
    let manifest_dir = PathBuf::from(std::env::var_os("CARGO_MANIFEST_DIR")?);
    // Rebuild when HEAD moves: HEAD itself (branch switch, detached
    // checkout), the branch ref it points at, and packed-refs (where a ref
    // lands after `git gc`). `--git-path` resolves each correctly inside a
    // linked worktree, where HEAD and refs live in different directories.
    let mut watched = vec!["HEAD".to_string(), "packed-refs".to_string()];
    if let Some(head_ref) = git(&manifest_dir, &["symbolic-ref", "-q", "HEAD"]) {
        watched.push(head_ref);
    }
    for name in watched {
        if let Some(path) = git(&manifest_dir, &["rev-parse", "--git-path", &name]) {
            println!(
                "cargo:rerun-if-changed={}",
                manifest_dir.join(path).display()
            );
        }
    }

    let hash = git(&manifest_dir, &["rev-parse", "--short=7", "HEAD"])?;
    // Untracked (non-gitignored) files count as dirty too, since a new
    // source file wired in via a `mod` statement compiles into the binary
    // without being committed. An edit to a file outside the
    // rerun-if-changed list above (a doc or test-only change) won't refresh
    // this flag until something that list does watch also changes.
    let dirty = git(&manifest_dir, &["status", "--porcelain"]).is_some();
    Some(if dirty { format!("{hash}-dirty") } else { hash })
}

/// Runs git in `dir`, returning its trimmed stdout, or `None` on failure,
/// empty output, or when git isn't installed at all.
fn git(dir: &Path, args: &[&str]) -> Option<String> {
    let output = Command::new("git")
        .args(args)
        .current_dir(dir)
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let stdout = String::from_utf8(output.stdout).ok()?;
    let trimmed = stdout.trim();
    (!trimmed.is_empty()).then(|| trimmed.to_string())
}

fn link_cuda() {
    // Only the gpu-cuda feature needs this: transcribe-cpp-sys's CMake build
    // links against the CUDA runtime/cuBLAS/driver libraries without itself
    // emitting the cargo link directives Cargo needs to find them on NixOS,
    // where they don't live on the linker's default search path the way an
    // FHS CUDA install would. Scoped to the feature so the default and
    // gpu-vulkan builds stay exactly as they were (no CUDA dependency).
    println!("cargo:rerun-if-env-changed=CUDAToolkit_ROOT");
    if std::env::var_os("CARGO_FEATURE_GPU_CUDA").is_none() {
        return;
    }

    if let Ok(cuda_root) = std::env::var("CUDAToolkit_ROOT") {
        println!("cargo:rustc-link-search=native={cuda_root}/lib");
    }

    // libcuda.so (the driver API) ships with the NVIDIA driver, not the CUDA
    // toolkit; /run/opengl-driver/lib is NixOS's stable path to whichever
    // driver is active. Also rpath it into the binary so a built binary
    // doesn't additionally need LD_LIBRARY_PATH set at run time.
    // A Nix build sandbox can't see /run/opengl-driver, so fall back to the
    // toolkit's link-only stub; the rpath still resolves the real driver.
    println!("cargo:rustc-link-search=native=/run/opengl-driver/lib");
    if let Ok(cuda_root) = std::env::var("CUDAToolkit_ROOT") {
        println!("cargo:rustc-link-search=native={cuda_root}/lib/stubs");
    }
    // Passed as link args rather than `rustc-link-lib`: in the crate's own
    // test binaries a `rustc-link-lib` lands before transcribe-cpp-sys's
    // static archive that uses it, so `--as-needed` drops it as unused.
    // Link args go after every archive on the command line.
    println!(
        "cargo:rustc-link-arg=-Wl,--push-state,--no-as-needed,-lcudart,-lcublas,-lcuda,--pop-state"
    );
    println!("cargo:rustc-link-arg=-Wl,-rpath,/run/opengl-driver/lib");
}

/// Renders the man pages and shell completions that the packages and the
/// release asset bundle carry, into `<target>/[<triple>/]<profile>/dist/`.
/// That path is exactly what cargo-deb and cargo-generate-rpm call
/// `target/release` when they remap an asset's `target/release/...` source, so
/// both packagers and the release workflow find the files in one place.
///
/// Only release builds write it: a debug `cargo build` (and `cargo test` /
/// `cargo clippy`) skips generation so everyday builds leave no artifacts
/// behind. Set `TONGUETYPED_GEN_DIST=1` to force it for a local dry run.
fn generate_dist() {
    let is_release = std::env::var("PROFILE").as_deref() == Ok("release");
    if !is_release && std::env::var_os("TONGUETYPED_GEN_DIST").is_none() {
        return;
    }

    let out_dir = PathBuf::from(std::env::var_os("OUT_DIR").expect("cargo sets OUT_DIR"));
    // OUT_DIR is `<target>/[<triple>/]<profile>/build/<pkg>-<hash>/out`; three
    // ancestors up is the profile directory, whatever the target/triple layout.
    let profile_dir = out_dir
        .ancestors()
        .nth(3)
        .expect("OUT_DIR always has a profile directory above it")
        .to_path_buf();
    let dist_dir = profile_dir.join("dist");
    let man_dir = dist_dir.join("man");
    let completion_dir = dist_dir.join("completions");
    for dir in [&man_dir, &completion_dir] {
        if let Err(error) = std::fs::create_dir_all(dir) {
            panic!("failed to create {}: {error}", dir.display());
        }
    }

    let mut command = cli::Cli::command();
    let name = command.get_name().to_string();

    render_man_pages(&command, &name, &man_dir);
    for shell in [Shell::Bash, Shell::Zsh, Shell::Fish] {
        if let Err(error) = generate_to(shell, &mut command, &name, &completion_dir) {
            panic!("failed to generate {shell:?} completions: {error}");
        }
    }
}

/// Writes `<name>.1` plus one page per subcommand (and nested subcommand),
/// named the way man(1) resolves `man <name>-<sub>`, e.g. `tonguetyped-model`,
/// `tonguetyped-model-install`.
fn render_man_pages(command: &ClapCommand, name: &str, man_dir: &Path) {
    render_man(command, &man_dir.join(format!("{name}.1")));
    for subcommand in command.get_subcommands() {
        let sub_name = subcommand.get_name().to_string();
        if sub_name == "help" {
            continue;
        }
        render_man(subcommand, &man_dir.join(format!("{name}-{sub_name}.1")));
        for nested in subcommand.get_subcommands() {
            let nested_name = nested.get_name().to_string();
            if nested_name == "help" {
                continue;
            }
            render_man(
                nested,
                &man_dir.join(format!("{name}-{sub_name}-{nested_name}.1")),
            );
        }
    }
}

fn render_man(command: &ClapCommand, path: &Path) {
    let mut file = std::fs::File::create(path)
        .unwrap_or_else(|error| panic!("failed to create {}: {error}", path.display()));
    Man::new(command.clone())
        .render(&mut file)
        .unwrap_or_else(|error| panic!("failed to write {}: {error}", path.display()));
}
