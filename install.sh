#!/bin/sh

set -eu

repository="${AWS_GOOGLE_OIDC_REPOSITORY:-dialohq/aws-google-oidc}"
version="${AWS_GOOGLE_OIDC_VERSION:-latest}"
install_dir="${AWS_GOOGLE_OIDC_INSTALL_DIR:-$HOME/.local/bin}"

say() {
  printf '%s\n' "aws-google-oidc: $*"
}

fail() {
  say "$*" >&2
  exit 1
}

command -v tar >/dev/null 2>&1 || fail "tar is required"

case "$(uname -s):$(uname -m)" in
  Linux:x86_64 | Linux:amd64)
    platform="x86_64-linux"
    ;;
  Linux:aarch64 | Linux:arm64)
    platform="aarch64-linux"
    ;;
  Darwin:arm64 | Darwin:aarch64)
    platform="aarch64-darwin"
    ;;
  Darwin:x86_64)
    fail "Intel macOS is not supported; use the Nix installation from the README"
    ;;
  *)
    fail "unsupported platform: $(uname -s) $(uname -m)"
    ;;
esac

archive="aws-google-oidc-${platform}.tar.gz"
if [ "$version" = "latest" ]; then
  download_base="https://github.com/${repository}/releases/latest/download"
else
  case "$version" in
    v*) ;;
    *) version="v${version}" ;;
  esac
  download_base="https://github.com/${repository}/releases/download/${version}"
fi

tmp_dir="$(mktemp -d "${TMPDIR:-/tmp}/aws-google-oidc.XXXXXX")"
trap 'rm -rf "$tmp_dir"' EXIT HUP INT TERM

download() {
  url="$1"
  destination="$2"

  if command -v curl >/dev/null 2>&1; then
    curl --proto '=https' --tlsv1.2 -fLsS "$url" -o "$destination"
  elif command -v wget >/dev/null 2>&1; then
    wget -q "$url" -O "$destination"
  else
    fail "curl or wget is required"
  fi
}

say "downloading ${archive}"
download "${download_base}/${archive}" "${tmp_dir}/${archive}"
download "${download_base}/${archive}.sha256" "${tmp_dir}/${archive}.sha256"

expected_checksum="$(awk 'NR == 1 { print $1 }' "${tmp_dir}/${archive}.sha256")"
if command -v sha256sum >/dev/null 2>&1; then
  actual_checksum="$(sha256sum "${tmp_dir}/${archive}" | awk '{ print $1 }')"
elif command -v shasum >/dev/null 2>&1; then
  actual_checksum="$(shasum -a 256 "${tmp_dir}/${archive}" | awk '{ print $1 }')"
else
  fail "sha256sum or shasum is required to verify the download"
fi

[ -n "$expected_checksum" ] || fail "release checksum is empty"
[ "$actual_checksum" = "$expected_checksum" ] || fail "checksum verification failed"

tar -xzf "${tmp_dir}/${archive}" -C "$tmp_dir"
[ -f "${tmp_dir}/aws-google-oidc" ] || fail "release archive does not contain aws-google-oidc"

mkdir -p "$install_dir"
if command -v install >/dev/null 2>&1; then
  install -m 0755 "${tmp_dir}/aws-google-oidc" "${install_dir}/aws-google-oidc"
else
  cp "${tmp_dir}/aws-google-oidc" "${install_dir}/aws-google-oidc"
  chmod 0755 "${install_dir}/aws-google-oidc"
fi

say "installed ${install_dir}/aws-google-oidc"
case ":${PATH}:" in
  *":${install_dir}:"*) ;;
  *) say "add ${install_dir} to PATH" ;;
esac
