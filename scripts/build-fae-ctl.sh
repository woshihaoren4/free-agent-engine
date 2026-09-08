#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd -- "${SCRIPT_DIR}/.." && pwd)"

PACKAGE_NAME="${PACKAGE_NAME:-fae}"
BIN_NAME="${BIN_NAME:-fae}"
OUT_DIR="${OUT_DIR:-${REPO_ROOT}/docs/bin}"
MAC_OUT_DIR="${MAC_OUT_DIR:-${OUT_DIR}/mac}"
LINUX_OUT_DIR="${LINUX_OUT_DIR:-${OUT_DIR}/linux}"
LOCKED="${LOCKED:-1}"

export ZIG_GLOBAL_CACHE_DIR="${ZIG_GLOBAL_CACHE_DIR:-${REPO_ROOT}/target/zig-global-cache}"
export ZIG_LOCAL_CACHE_DIR="${ZIG_LOCAL_CACHE_DIR:-${REPO_ROOT}/target/zig-local-cache}"

MAC_TARGET="${MAC_TARGET:-aarch64-apple-darwin}"
LINUX_TARGET="${LINUX_TARGET:-x86_64-unknown-linux-gnu}"
MAC_OUT="${MAC_OUT:-fae}"
LINUX_OUT="${LINUX_OUT:-fae}"

die() {
  echo "fae release: $*" >&2
  exit 1
}

require_command() {
  command -v "$1" >/dev/null 2>&1 || die "required command not found: $1"
}

ensure_rust_target() {
  local target="$1"

  command -v rustup >/dev/null 2>&1 ||
    die "rustup is required to install the Rust target ${target}"

  if ! rustup target list --installed | grep -qx "${target}"; then
    echo "Installing Rust target: ${target}"
    rustup target add "${target}"
  fi
}

build_command() {
  local target="$1"

  if [[ "$(uname -s)" == "Darwin" && "${target}" == *-unknown-linux-gnu ]]; then
    command -v cargo-zigbuild >/dev/null 2>&1 ||
      die "cargo-zigbuild is required to build ${target} on macOS"
    command -v zig >/dev/null 2>&1 ||
      die "zig is required to build ${target} on macOS"
    echo "cargo zigbuild"
    return
  fi

  echo "cargo build"
}

write_sha256() {
  local path="$1"
  local output="${path}.sha256"
  local hash

  if command -v sha256sum >/dev/null 2>&1; then
    hash="$(sha256sum "${path}" | awk '{ print $1 }')"
  elif command -v shasum >/dev/null 2>&1; then
    hash="$(shasum -a 256 "${path}" | awk '{ print $1 }')"
  else
    die "sha256sum or shasum is required to create release checksums"
  fi

  printf '%s  %s\n' "${hash}" "$(basename "${path}")" > "${output}.tmp"
  mv "${output}.tmp" "${output}"
  echo "Wrote ${output}"
}

verify_binary_format() {
  local target="$1"
  local path="$2"
  local description

  description="$(file -b "${path}")"
  case "${target}" in
    aarch64-apple-darwin)
      [[ "${description}" == Mach-O*"arm64"* ]] ||
        die "${path} is not an arm64 macOS executable: ${description}"
      ;;
    x86_64-apple-darwin)
      [[ "${description}" == Mach-O*"x86_64"* ]] ||
        die "${path} is not an x86_64 macOS executable: ${description}"
      ;;
    x86_64-unknown-linux-*)
      [[ "${description}" == ELF*"x86-64"* ]] ||
        die "${path} is not an x86_64 Linux executable: ${description}"
      ;;
    aarch64-unknown-linux-*)
      [[ "${description}" == ELF*"ARM aarch64"* ]] ||
        die "${path} is not an arm64 Linux executable: ${description}"
      ;;
    *-unknown-linux-*)
      [[ "${description}" == ELF* ]] ||
        die "${path} is not a Linux executable: ${description}"
      ;;
  esac
}

build_target() {
  local target="$1"
  local output_dir="$2"
  local output_name="$3"
  local command_line
  local binary_path
  local output_path
  local build_args

  ensure_rust_target "${target}"
  command_line="$(build_command "${target}")"

  echo "Building ${PACKAGE_NAME} for ${target} with ${command_line}"
  read -r -a command_parts <<< "${command_line}"
  (
    cd "${REPO_ROOT}"
    build_args=(--release -p "${PACKAGE_NAME}" --target "${target}")
    if [[ "${LOCKED}" == "1" ]]; then
      build_args+=(--locked)
    fi
    "${command_parts[@]}" "${build_args[@]}"
  )

  binary_path="${REPO_ROOT}/target/${target}/release/${BIN_NAME}"
  output_path="${output_dir}/${output_name}"

  if [[ ! -x "${binary_path}" ]]; then
    die "expected binary not found: ${binary_path}"
  fi

  mkdir -p "${output_dir}"
  cp "${binary_path}" "${output_path}.tmp"
  chmod 755 "${output_path}.tmp"
  verify_binary_format "${target}" "${output_path}.tmp"
  mv "${output_path}.tmp" "${output_path}"
  echo "Wrote ${output_path}"
  write_sha256 "${output_path}"
}

require_command cargo
require_command file
require_command rustc

build_target "${MAC_TARGET}" "${MAC_OUT_DIR}" "${MAC_OUT}"
build_target "${LINUX_TARGET}" "${LINUX_OUT_DIR}" "${LINUX_OUT}"

echo "Release artifacts are ready in ${OUT_DIR}"
