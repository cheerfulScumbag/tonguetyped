# Maintainer: cheerfulScumbag <164391367+cheerfulScumbag@users.noreply.github.com>
#
# Template for the AUR `tonguetyped-bin` package. This file tracks the
# project's current version for local reference only; `.github/workflows/
# release.yml`'s `arch` job resolves `pkgver`/`sha256sums` for the actual
# release tag and pushes the resolved file to AUR, it does not push this
# copy verbatim. Repackages the prebuilt default-variant Linux release
# binary (the same one `cargo-deb` repackages for the .deb, see
# `[package.metadata.deb]` in Cargo.toml) rather than rebuilding from
# source, matching the common AUR "-bin" convention for Rust CLI tools.
pkgname=tonguetyped-bin
pkgver=0.1.0
pkgrel=1
pkgdesc="Local Linux dictation controlled from the terminal or a desktop-wide keyboard shortcut"
arch=('x86_64')
url="https://github.com/cheerfulScumbag/tonguetyped"
license=('custom')
# Repackaging an already-built, already-stripped binary leaves nothing for
# makepkg's debug-info extraction to work with (there is no build tree to
# correlate symbols against), so skip the auto-generated -debug split
# package it would otherwise emit.
options=('!debug')
provides=('tonguetyped')
conflicts=('tonguetyped')
# wtype and wl-clipboard are hard dependencies: the AUR package should type
# into the focused application out of the box on Wayland and ship the Wayland
# clipboard tools (xdotool already covers the built-in enigo backend on X11).
# dotool stays optional as the detected alternative.
depends=('alsa-lib' 'openssl' 'xdotool' 'wtype' 'wl-clipboard')
optdepends=(
  'wayland: desktop overlay feedback while recording'
  'dotool: typing output without a windowing system'
)
source=("https://github.com/cheerfulScumbag/tonguetyped/releases/download/v${pkgver}/tonguetyped-v${pkgver}-linux-x86_64")
sha256sums=('0000000000000000000000000000000000000000000000000000000000000000')

package() {
  install -Dm755 "${srcdir}/tonguetyped-v${pkgver}-linux-x86_64" "${pkgdir}/usr/bin/tonguetyped"
}
