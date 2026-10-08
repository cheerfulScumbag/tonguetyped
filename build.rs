use std::path::{Path, PathBuf};
use std::process::Command;

fn main() {
    emit_build_commit();
    link_cuda();
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
    println!("cargo:rustc-link-lib=cudart");
    println!("cargo:rustc-link-lib=cublas");

    // libcuda.so (the driver API) ships with the NVIDIA driver, not the CUDA
    // toolkit; /run/opengl-driver/lib is NixOS's stable path to whichever
    // driver is active. Also rpath it into the binary so a built binary
    // doesn't additionally need LD_LIBRARY_PATH set at run time.
    println!("cargo:rustc-link-search=native=/run/opengl-driver/lib");
    println!("cargo:rustc-link-lib=cuda");
    println!("cargo:rustc-link-arg=-Wl,-rpath,/run/opengl-driver/lib");
}
