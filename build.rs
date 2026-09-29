fn main() {
    // Only the gpu-cuda feature needs this: transcribe-cpp-sys's CMake build
    // links against the CUDA runtime/cuBLAS/driver libraries without itself
    // emitting the cargo link directives Cargo needs to find them on NixOS,
    // where they don't live on the linker's default search path the way an
    // FHS CUDA install would. Scoped to the feature so the default and
    // gpu-vulkan builds stay exactly as they were (no CUDA dependency).
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
