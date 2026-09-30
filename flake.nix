{
  description = "TongueTyped - Linux dictation application";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    rust-overlay.url = "github:oxalica/rust-overlay";
    flake-utils.url = "github:numtide/flake-utils";
  };

  outputs = {
    self,
    nixpkgs,
    rust-overlay,
    flake-utils,
  }:
    flake-utils.lib.eachSystem ["x86_64-linux" "aarch64-linux"] (system: let
      overlays = [(import rust-overlay)];
      pkgs = import nixpkgs {
        inherit system overlays;
        # cudaPackages.cudatoolkit (below) is unfree; needed to build the
        # optional gpu-cuda transcribe-cpp backend.
        config.allowUnfree = true;
      };
      rustToolchain = pkgs.rust-bin.stable.latest.default.override {
        extensions = ["rust-src" "rust-analyzer"];
      };
    in {
      devShells.default = pkgs.mkShell {
        buildInputs = with pkgs; [
          rustToolchain
          cmake
          libclang
          pkg-config
          alsa-lib
          xdotool
          openssl
          # libwayland-client.so: the layer-shell overlay's Wayland connection
          # (src/overlay.rs). Unused, without erroring, on non-Wayland sessions.
          wayland
          # Vulkan: builds transcribe.cpp's `-DTRANSCRIBE_VULKAN=ON` backend
          # (`cargo build --features gpu-vulkan`).
          vulkan-headers
          vulkan-loader
          shaderc
          # CUDA: builds transcribe.cpp's `-DTRANSCRIBE_CUDA=ON` backend
          # (`cargo build --features gpu-cuda`). Requires an NVIDIA GPU +
          # driver on the host to actually run.
          cudaPackages.cudatoolkit
        ];

        shellHook = ''
          export RUST_BACKTRACE=1
          export LIBCLANG_PATH="${pkgs.libclang.lib}/lib"
          export CMAKE_POLICY_VERSION_MINIMUM=3.5
          export CUDAToolkit_ROOT="${pkgs.cudaPackages.cudatoolkit}"
          # NixOS doesn't put CUDA (or the driver's libcuda.so) on the default
          # runtime search path the way an FHS install would; build.rs adds
          # the matching rustc-link-search/-rpath for linking a gpu-cuda
          # build, this covers running other CUDA-touching tools from the shell.
          export LD_LIBRARY_PATH="$CUDAToolkit_ROOT/lib:/run/opengl-driver/lib''${LD_LIBRARY_PATH:+:$LD_LIBRARY_PATH}"
        '';
      };

      packages.default = pkgs.rustPlatform.buildRustPackage {
        pname = "tonguetyped";
        version = "0.1.0";
        src = ./.;

        cargoLock = {
          lockFile = ./Cargo.lock;
          outputHashes = {
            "vad-rs-0.1.6" = "sha256-zQr/WVa9SBcPSTrm5DIKYBa3bPuYTQTDEzFkl7cjYTI=";
          };
        };

        nativeBuildInputs = with pkgs; [
          cmake
          desktop-file-utils
          libclang
          makeWrapper
          pkg-config
        ];

        buildInputs = with pkgs; [
          alsa-lib
          onnxruntime
          xdotool
          openssl
          wayland
        ];

        LIBCLANG_PATH = "${pkgs.libclang.lib}/lib";
        CMAKE_POLICY_VERSION_MINIMUM = "3.5";
        ORT_LIB_LOCATION = "${pkgs.onnxruntime}/lib";
        ORT_PREFER_DYNAMIC_LINK = "1";

        postInstall = ''
          install -Dm644 data/tonguetyped.desktop \
            $out/share/applications/io.github.cheerfulScumbag.tonguetyped.desktop
        '';

        postFixup = ''
          wrapProgram $out/bin/tonguetyped \
            --prefix PATH : ${pkgs.lib.makeBinPath [pkgs.libcanberra-gtk3]}
        '';

        doInstallCheck = true;
        installCheckPhase = ''
          runHook preInstallCheck
          test -x $out/bin/tonguetyped
          desktop-file-validate $out/share/applications/io.github.cheerfulScumbag.tonguetyped.desktop
          runHook postInstallCheck
        '';
      };
    });
}
