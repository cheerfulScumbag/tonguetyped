#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
PROJECT_DIR="$(dirname "$SCRIPT_DIR")"

CARGO_BIN=$(find /nix/store -maxdepth 4 -path "*/dv55qwamlq68hdiprg1v104ymwh1c27r-cargo-*/bin/cargo" -type f 2>/dev/null | head -1)
RUSTC_BIN=$(find /nix/store -maxdepth 4 -path "*/cqlx62f919g8xf2f39bmykslpjdh9z0j-rustc-*/bin/rustc" -type f 2>/dev/null | head -1)
CMAKE_BIN_DIR=$(find /nix/store -maxdepth 4 -path "*/c3bm9h7k398cmg00d4rss6hcxkl2zbl2-cmake-*/bin" -type d 2>/dev/null | head -1)
LIBCLANG_DIR=$(find /nix/store -maxdepth 4 -path "*/yc2a9854a2y2c8kci88piblc847iq1l4-clang-*-lib/lib" -type d 2>/dev/null | head -1)
ALSA_PC_DIR=$(find /nix/store -maxdepth 4 -path "*/a0wjrqgl2ypixq3iy5l859ziws9n0idq-alsa-lib-*-dev/lib/pkgconfig" -type d 2>/dev/null | head -1)
OPENSSL_PC_DIR=$(find /nix/store -maxdepth 4 -path "*/g3bcbl53jlgnd5aizrsbiqjnshsnp927-openssl-*-dev/lib/pkgconfig" -type d 2>/dev/null | head -1)

cat > "$PROJECT_DIR/scripts/env.sh" << 'ENVEOF'
#!/usr/bin/env bash
export PKG_CONFIG_PATH="@@ALSA_PC@@:@@OPENSSL_PC@@"
export CMAKE_POLICY_VERSION_MINIMUM=3.5
export LIBCLANG_PATH="@@LIBCLANG@@"
export PATH="@@CARGO_DIR@@:@@CMAKE_DIR@@:$PATH"
export RUSTC="@@RUSTC@@"
ENVEOF

sed -i \
  -e "s|@@ALSA_PC@@|${ALSA_PC_DIR:-MISSING}|g" \
  -e "s|@@OPENSSL_PC@@|${OPENSSL_PC_DIR:-MISSING}|g" \
  -e "s|@@LIBCLANG@@|${LIBCLANG_DIR:-MISSING}|g" \
  -e "s|@@CARGO_DIR@@|$(dirname "${CARGO_BIN:-MISSING}")|g" \
  -e "s|@@CMAKE_DIR@@|${CMAKE_BIN_DIR:-MISSING}|g" \
  -e "s|@@RUSTC@@|${RUSTC_BIN:-MISSING}|g" \
  "$PROJECT_DIR/scripts/env.sh"

chmod +x "$PROJECT_DIR/scripts/env.sh"
echo "Generated scripts/env.sh"