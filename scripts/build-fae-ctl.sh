#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd -- "${SCRIPT_DIR}/.." && pwd)"

PACKAGE_NAME="${PACKAGE_NAME:-fae}"
BIN_NAME="${BIN_NAME:-fae}"
OUT_DIR="${OUT_DIR:-${REPO_ROOT}/docs/bin}"
MAC_OUT_DIR="${MAC_OUT_DIR:-${OUT_DIR}/mac}"
LINUX_OUT_DIR="${LINUX_OUT_DIR:-${OUT_DIR}/linux}"

export ZIG_GLOBAL_CACHE_DIR="${ZIG_GLOBAL_CACHE_DIR:-${REPO_ROOT}/target/zig-global-cache}"
export ZIG_LOCAL_CACHE_DIR="${ZIG_LOCAL_CACHE_DIR:-${REPO_ROOT}/target/zig-local-cache}"

host_target="$(rustc -vV | awk '/^host:/ { print $2 }')"
if [[ "${host_target}" == *-apple-darwin ]]; then
  default_mac_target="${host_target}"
else
  default_mac_target="aarch64-apple-darwin"
fi

MAC_TARGET="${MAC_TARGET:-${default_mac_target}}"
LINUX_TARGET="${LINUX_TARGET:-x86_64-unknown-linux-gnu}"
MAC_OUT="${MAC_OUT:-fae}"
LINUX_OUT="${LINUX_OUT:-fae}"

ensure_rust_target() {
  local target="$1"

  if ! command -v rustup >/dev/null 2>&1; then
    return
  fi

  if ! rustup target list --installed | grep -qx "${target}"; then
    echo "Installing Rust target: ${target}"
    rustup target add "${target}"
  fi
}

build_command() {
  local target="$1"

  if [[ "$(uname -s)" == "Darwin" && "${target}" == *-unknown-linux-gnu ]]; then
    if command -v cargo-zigbuild >/dev/null 2>&1 && command -v zig >/dev/null 2>&1; then
      echo "cargo zigbuild"
      return
    fi
  fi

  echo "cargo build"
}

build_target() {
  local target="$1"
  local output_dir="$2"
  local output_name="$3"
  local command_line
  local binary_path
  local output_path

  ensure_rust_target "${target}"
  command_line="$(build_command "${target}")"

  echo "Building ${PACKAGE_NAME} for ${target} with ${command_line}"
  read -r -a command_parts <<< "${command_line}"
  (
    cd "${REPO_ROOT}"
    "${command_parts[@]}" --release -p "${PACKAGE_NAME}" --target "${target}"
  )

  binary_path="${REPO_ROOT}/target/${target}/release/${BIN_NAME}"
  output_path="${output_dir}/${output_name}"

  if [[ ! -x "${binary_path}" ]]; then
    echo "Expected binary not found: ${binary_path}" >&2
    exit 1
  fi

  mkdir -p "${output_dir}"
  cp "${binary_path}" "${output_path}"
  chmod 755 "${output_path}"
  echo "Wrote ${output_path}"
}

build_target "${MAC_TARGET}" "${MAC_OUT_DIR}" "${MAC_OUT}"
build_target "${LINUX_TARGET}" "${LINUX_OUT_DIR}" "${LINUX_OUT}"
