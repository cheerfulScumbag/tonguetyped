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
    flake-utils.lib.eachDefaultSystem (system: let
      overlays = [(import rust-overlay)];
      pkgs = import nixpkgs {
        inherit system overlays;
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
          openssl
          xdotool
        ];

        shellHook = ''
          export RUST_BACKTRACE=1
          export LIBCLANG_PATH="${pkgs.libclang.lib}/lib"
          export CMAKE_POLICY_VERSION_MINIMUM=3.5
        '';
      };

      packages.default = pkgs.rustPlatform.buildRustPackage {
        pname = "tonguetyped";
        version = "0.1.0";
        src = ./.;

        cargoLock = {
          lockFile = ./Cargo.lock;
        };

        nativeBuildInputs = with pkgs; [
          cmake
          libclang
          pkg-config
        ];

        buildInputs = with pkgs; [
          alsa-lib
          openssl
          xdotool
        ];

        LIBCLANG_PATH = "${pkgs.libclang.lib}/lib";
        CMAKE_POLICY_VERSION_MINIMUM = "3.5";
      };
    });
}
