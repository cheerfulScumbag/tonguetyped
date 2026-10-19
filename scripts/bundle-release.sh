#!/usr/bin/env bash
set -euo pipefail

tag="${1:?usage: bundle-release.sh <tag> <dist-dir> <output-dir>}"
dist="${2:?usage: bundle-release.sh <tag> <dist-dir> <output-dir>}"
output="${3:?usage: bundle-release.sh <tag> <dist-dir> <output-dir>}"
repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

bundle="tonguetyped-${tag}-completions-and-man"
stage="$(mktemp -d)"
trap 'rm -rf "${stage}"' EXIT

mkdir -p \
  "${stage}/${bundle}/completions" \
  "${stage}/${bundle}/man"
cp "${dist}/completions/"* "${stage}/${bundle}/completions/"
cp "${dist}/man/"*.1 "${stage}/${bundle}/man/"

icon="${repo_root}/data/icons/hicolor/scalable/apps/io.github.cheerfulScumbag.tonguetyped.svg"
if [ -f "${icon}" ]; then
  mkdir -p "${stage}/${bundle}/icons/hicolor/scalable/apps"
  cp "${icon}" "${stage}/${bundle}/icons/hicolor/scalable/apps/"
fi

mkdir -p "${output}"
tar -C "${stage}" -czf "${output}/${bundle}.tar.gz" "${bundle}"
