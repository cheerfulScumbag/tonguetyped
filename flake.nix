{
  description = "TongueTyped - Linux dictation application";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    rust-overlay.url = "github:oxalica/rust-overlay";
    flake-utils.url = "github:numtide/flake-utils";
  };

  outputs =
    {
      self,
      nixpkgs,
      rust-overlay,
      flake-utils,
    }:
    flake-utils.lib.eachSystem [ "x86_64-linux" "aarch64-linux" ] (
      system:
      let
        overlays = [ (import rust-overlay) ];
        pkgs = import nixpkgs {
          inherit system overlays;
          # cudaPackages.cudatoolkit (below) is unfree; needed to build the
          # optional gpu-cuda transcribe-cpp backend.
          config.allowUnfree = true;
        };
        rustToolchain = pkgs.rust-bin.stable.latest.default.override {
          extensions = [
            "rust-src"
            "rust-analyzer"
          ];
        };
        isX86_64 = system == "x86_64-linux";
        rocmInputs = with pkgs.rocmPackages; [
          clr
          hipblas
          rocblas
        ];
        mkPackage =
          {
            feature,
            extraNativeBuildInputs ? [ ],
            extraBuildInputs ? [ ],
            extraPostFixup ? "",
            extraAttrs ? { },
          }:
          pkgs.rustPlatform.buildRustPackage (
            {
              pname = "tonguetyped";
              version = "0.3.0";
              src = ./.;

              cargoLock = {
                lockFile = ./Cargo.lock;
                outputHashes = {
                  "vad-rs-0.1.6" = "sha256-zQr/WVa9SBcPSTrm5DIKYBa3bPuYTQTDEzFkl7cjYTI=";
                };
              };

              nativeBuildInputs =
                with pkgs;
                [
                  cmake
                  desktop-file-utils
                  libclang
                  makeWrapper
                  pkg-config
                ]
                ++ extraNativeBuildInputs;

              buildInputs =
                with pkgs;
                [
                  alsa-lib
                  onnxruntime
                  xdotool
                  openssl
                  wayland
                  # libxkbcommon: the native libei typing backend (src/libei.rs)
                  # compiles the compositor's keymap and maps characters to
                  # keycodes through the `xkbcommon` Rust crate.
                  libxkbcommon
                ]
                ++ extraBuildInputs;

              buildFeatures = [ feature ];

              # `tests/media_pause.rs` starts a private `dbus-daemon`.
              nativeCheckInputs = [ pkgs.dbus ];

              # The setup console's `pty_*`-prefixed tests allocate a real
              # pseudo-terminal and spawn the built binary into it
              # (`portable-pty`); the Nix build sandbox's `checkPhase` has no
              # usable pty/tty subsystem for that nested spawn (fails with ENOENT
              # even though the binary itself builds fine and `openpty` succeeds).
              # `nix develop -c cargo test` and plain `cargo test` both run them
              # normally and are the real coverage for those files.
              cargoTestFlags = [
                "--"
                "--skip"
                "pty_"
              ];

              LIBCLANG_PATH = "${pkgs.libclang.lib}/lib";
              BINDGEN_EXTRA_CLANG_ARGS = "-I${pkgs.glibc.dev}/include";
              CMAKE_POLICY_VERSION_MINIMUM = "3.5";
              # `src = ./.` copies the tree without `.git`, so build.rs can't run
              # git itself; hand it the flake's commit for `--version`/`doctor`.
              TONGUETYPED_BUILD_COMMIT = self.shortRev or self.dirtyShortRev or "unknown";
              ORT_LIB_LOCATION = "${pkgs.onnxruntime}/lib";
              ORT_PREFER_DYNAMIC_LINK = "1";

              postInstall = ''
                install -Dm644 data/io.github.cheerfulScumbag.tonguetyped.desktop \
                  $out/share/applications/io.github.cheerfulScumbag.tonguetyped.desktop
                install -Dm644 data/icons/hicolor/scalable/apps/io.github.cheerfulScumbag.tonguetyped.svg \
                  $out/share/icons/hicolor/scalable/apps/io.github.cheerfulScumbag.tonguetyped.svg

                # build.rs renders these during the release build. Locate them
                # rather than hardcoding a path: buildRustPackage passes
                # `--target <triple>`, so the profile directory is
                # target/<triple>/release, not target/release.
                dist="$(find target -type d -path '*/release/dist' -print -quit)"
                test -d "$dist"
                install -Dm644 "$dist/completions/tonguetyped.bash" \
                  $out/share/bash-completion/completions/tonguetyped
                install -Dm644 "$dist/completions/_tonguetyped" \
                  $out/share/zsh/site-functions/_tonguetyped
                install -Dm644 "$dist/completions/tonguetyped.fish" \
                  $out/share/fish/vendor_completions.d/tonguetyped.fish
                install -d $out/share/man/man1
                install -m644 "$dist/man/"*.1 $out/share/man/man1/
              '';

              postFixup = ''
                wrapProgram $out/bin/tonguetyped \
                  --prefix PATH : ${
                    pkgs.lib.makeBinPath [
                      pkgs.libcanberra-gtk3
                      pkgs.wtype
                      pkgs.dotool
                      pkgs.wl-clipboard
                      pkgs.xclip
                    ]
                  }
              ''
              + extraPostFixup;

              doInstallCheck = true;
              installCheckPhase = ''
                runHook preInstallCheck
                test -x $out/bin/tonguetyped
                desktop-file-validate $out/share/applications/io.github.cheerfulScumbag.tonguetyped.desktop

                # Every installed desktop entry must point at the app-ID icon,
                # and that icon must resolve inside the hicolor theme.
                for entry in $out/share/applications/*.desktop; do
                  icon="$(sed -n 's/^Icon=//p' "$entry")"
                  test "$icon" = io.github.cheerfulScumbag.tonguetyped
                  test -f "$out/share/icons/hicolor/scalable/apps/$icon.svg"
                done
                runHook postInstallCheck
              '';
            }
            // extraAttrs
          );
        vulkanPackage = mkPackage {
          feature = "gpu-vulkan";
          extraNativeBuildInputs = [ pkgs.shaderc ];
          extraBuildInputs = with pkgs; [
            spirv-headers
            vulkan-headers
            vulkan-loader
          ];
        };
        rocmPackage = mkPackage {
          feature = "gpu-rocm";
          extraBuildInputs = rocmInputs;
          extraAttrs = {
            ROCM_PATH = "${pkgs.rocmPackages.clr}";
            CMAKE_PREFIX_PATH = pkgs.lib.makeSearchPath "" rocmInputs;
            TRANSCRIBE_CMAKE_ARGS = "-DCMAKE_HIP_COMPILER=${pkgs.rocmPackages.clr.hipClangPath}/clang++ -DCMAKE_HIP_COMPILER_ROCM_ROOT=${pkgs.rocmPackages.clr} -DAMDGPU_TARGETS=gfx803;gfx900;gfx906;gfx908;gfx90a;gfx942;gfx1030;gfx1100;gfx1101;gfx1102;gfx1201";
          };
        };
        cudaPackage = mkPackage {
          feature = "gpu-cuda";
          # Prepends /run/opengl-driver/lib (the host's libcuda.so.1) to RUNPATH.
          extraNativeBuildInputs = [ pkgs.autoAddDriverRunpath ];
          extraBuildInputs = [ pkgs.cudaPackages.cudatoolkit ];
          # The sandbox links libcuda against the toolkit's stub (see build.rs),
          # which the linker wrapper records in RUNPATH; drop it so the real
          # driver is the only libcuda.so.1 the binary can load.
          extraPostFixup = ''
            elf=$out/bin/.tonguetyped-wrapped
            patchelf --set-rpath "$(patchelf --print-rpath $elf | tr ':' '\n' | grep -v '/lib/stubs$' | paste -sd:)" $elf
          '';
          extraAttrs = {
            CUDAToolkit_ROOT = "${pkgs.cudaPackages.cudatoolkit}";
            CUDACXX = "${pkgs.cudaPackages.cudatoolkit}/bin/nvcc";
          };
        };
      in
      {
        devShells.default = pkgs.mkShell {
          buildInputs =
            with pkgs;
            [
              rustToolchain
              cmake
              libclang
              pkg-config
              alsa-lib
              spirv-headers
              xdotool
              # wtype: the wlroots Wayland typing helper; dotool: the helper that
              # types through /dev/uinput on KWin and other compositors wtype
              # cannot reach; wl-clipboard: Wayland clipboard tools. All are
              # bundled into the installed binary's PATH below so a normal install
              # has them without the user discovering and installing them
              # manually; present in the dev shell too so `cargo run`/tests match
              # the packaged behavior. dotool still needs write access to
              # /dev/uinput (the `input` group or a udev rule), which the install
              # does not grant.
              wtype
              dotool
              wl-clipboard
              openssl
              # libwayland-client.so: the layer-shell overlay's Wayland connection
              # (src/overlay.rs). Unused, without erroring, on non-Wayland sessions.
              wayland
              # libxkbcommon: the native libei typing backend (src/libei.rs).
              libxkbcommon
              # Vulkan: builds transcribe.cpp's `-DTRANSCRIBE_VULKAN=ON` backend
              # (`cargo build --features gpu-vulkan`).
              vulkan-headers
              vulkan-loader
              shaderc
              # CUDA: builds transcribe.cpp's `-DTRANSCRIBE_CUDA=ON` backend
              # (`cargo build --features gpu-cuda`). Requires an NVIDIA GPU +
              # driver on the host to actually run.
              cudaPackages.cudatoolkit
              # ROCm: builds transcribe.cpp's `-DTRANSCRIBE_HIP=ON` backend
              # (`cargo build --features gpu-rocm`).
            ]
            ++ pkgs.lib.optionals isX86_64 rocmInputs;

          shellHook = ''
            export RUST_BACKTRACE=1
            export LIBCLANG_PATH="${pkgs.libclang.lib}/lib"
            export BINDGEN_EXTRA_CLANG_ARGS="-I${pkgs.glibc.dev}/include"
            export CMAKE_POLICY_VERSION_MINIMUM=3.5
            export CUDAToolkit_ROOT="${pkgs.cudaPackages.cudatoolkit}"
            ${pkgs.lib.optionalString isX86_64 ''export ROCM_PATH="${pkgs.rocmPackages.clr}"''}
            # NixOS doesn't put CUDA (or the driver's libcuda.so) on the default
            # runtime search path the way an FHS install would; build.rs adds
            # the matching rustc-link-search/-rpath for linking a gpu-cuda
            # build, this covers running other CUDA-touching tools from the shell.
            export LD_LIBRARY_PATH="$CUDAToolkit_ROOT/lib:/run/opengl-driver/lib''${LD_LIBRARY_PATH:+:$LD_LIBRARY_PATH}"
          '';
        };

        packages = {
          default = vulkanPackage;
          gpu-vulkan = vulkanPackage;
        }
        // pkgs.lib.optionalAttrs isX86_64 {
          gpu-rocm = rocmPackage;
          gpu-cuda = cudaPackage;
        };

        # Evaluates the NixOS module and runs the daemon launcher it generates
        # (with a stub daemon, so no TongueTyped build is needed) against an
        # existing config.toml, checking declared keys win and others survive.
        checks.nixos-module =
          let
            nixos = nixpkgs.lib.nixosSystem {
              inherit system;
              modules = [
                self.nixosModules.default
                {
                  boot.isContainer = true;
                  system.stateVersion = "25.11";
                  programs.tonguetyped = {
                    enable = true;
                    package = pkgs.writeShellScriptBin "tonguetyped" "";
                    settings = {
                      activation.mode = "toggle";
                      model.idle_unload.timeout_minutes = 5;
                      audio.feedback_volume = 0.5;
                      overlay.style = "pill";
                      overlay.live_waveform = true;
                      text = {
                        spoken_punctuation = true;
                        remove_fillers = true;
                        post_process_timeout_seconds = 3;
                        replacements."vox type" = "Voxtype";
                      };
                    };
                  };
                }
              ];
            };
            service = nixos.config.systemd.user.services.tonguetyped;
          in
          pkgs.runCommand "tonguetyped-nixos-module-check"
            {
              nativeBuildInputs = [
                pkgs.jq
                pkgs.remarshal
              ];
            }
            ''
              export HOME=$TMPDIR/home XDG_CONFIG_HOME=$TMPDIR/home/.config
              mkdir -p $XDG_CONFIG_HOME/tonguetyped
              cat >$XDG_CONFIG_HOME/tonguetyped/config.toml <<'TOML'
              [activation]
              mode = "hold"
              keybind = "Super+O"

              [overlay]
              style = "badge"
              position = "top-left"
              TOML

              ${service.serviceConfig.ExecStart}

              remarshal --from toml --to json $XDG_CONFIG_HOME/tonguetyped/config.toml >config.json
              jq -e '
                .activation == {mode: "toggle", keybind: "Super+O"}
                and .model == {idle_unload: {timeout_minutes: 5}}
                and .audio == {feedback_volume: 0.5}
                and .overlay == {style: "pill", look: "waveform", position: "top-left"}
                and .text == {
                  spoken_punctuation: true,
                  remove_fillers: true,
                  post_process_timeout_seconds: 3,
                  replacements: {"vox type": "Voxtype"}
                }
              ' config.json
              test "$(stat -c %a $XDG_CONFIG_HOME/tonguetyped/config.toml)" = 600
              test ${pkgs.lib.escapeShellArg (builtins.toJSON service.wantedBy)} = '["graphical-session.target"]'
              test ${pkgs.lib.escapeShellArg service.serviceConfig.RestartPreventExitStatus} = "3"
              touch $out
            '';
      }
    )
    // {
      nixosModules = rec {
        default = tonguetyped;
        tonguetyped = import ./nix/modules/nixos.nix { inherit self; };
      };
      homeManagerModules = rec {
        default = tonguetyped;
        tonguetyped = import ./nix/modules/home-manager.nix { inherit self; };
      };
      # The newer name for the same output.
      homeModules = self.homeManagerModules;
    };
}
