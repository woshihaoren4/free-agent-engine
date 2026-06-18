#!/usr/bin/env bash
set -euo pipefail

BIN_NAME="${BIN_NAME:-fae}"
BASE_URL="${FAE_INSTALL_BASE_URL:-https://woshihaoren4.github.io/free-agent-engine/bin}"

die() {
  echo "fae install: $*" >&2
  exit 1
}

os="$(uname -s)"
arch="$(uname -m)"
case "${os}" in
  Darwin)
    [[ "${arch}" == "arm64" ]] || die "macOS package currently supports arm64 only; detected ${arch}"
    platform="mac"
    ;;
  Linux)
    [[ "${arch}" == "x86_64" ]] || die "Linux package currently supports x86_64 only; detected ${arch}"
    platform="linux"
    ;;
  *)
    die "unsupported system: ${os}"
    ;;
esac

path_has() {
  case ":${PATH:-}:" in
    *":$1:"*) return 0 ;;
    *) return 1 ;;
  esac
}

install_dir="${INSTALL_DIR:-}"
install_candidates=()

add_install_candidate() {
  local candidate="$1"
  local existing
  [[ -n "${candidate}" ]] || return 0
  if [[ "${#install_candidates[@]}" -gt 0 ]]; then
    for existing in "${install_candidates[@]}"; do
      [[ "${existing}" != "${candidate}" ]] || return 0
    done
  fi
  install_candidates+=("${candidate}")
}

if [[ -n "${install_dir}" ]]; then
  add_install_candidate "${install_dir}"
else
  # Prefer system-wide locations first. The installer can use sudo later if needed.
  add_install_candidate "/usr/local/bin"
  add_install_candidate "/usr/bin"
  if [[ "${platform}" == "mac" ]] && { [[ -d "/opt/homebrew/bin" ]] || path_has "/opt/homebrew/bin"; }; then
    add_install_candidate "/opt/homebrew/bin"
  fi
  add_install_candidate "${HOME}/.local/bin"
  add_install_candidate "${HOME}/bin"

  IFS=":" read -r -a path_dirs <<< "${PATH:-}"
  for candidate in "${path_dirs[@]}"; do
    if [[ -n "${candidate}" && -d "${candidate}" && -w "${candidate}" ]]; then
      add_install_candidate "${candidate}"
    fi
  done
fi

tmp_dir="$(mktemp -d)"
trap 'rm -rf "${tmp_dir}"' EXIT
tmp_bin="${tmp_dir}/${BIN_NAME}"

script_path="${BASH_SOURCE[0]:-}"
if [[ -n "${script_path}" && -f "${script_path}" ]]; then
  script_dir="$(cd -- "$(dirname -- "${script_path}")" && pwd)"
  local_bin="${script_dir}/${platform}/${BIN_NAME}"
else
  local_bin=""
fi

if [[ -n "${local_bin}" && -f "${local_bin}" ]]; then
  cp "${local_bin}" "${tmp_bin}"
else
  url="${BASE_URL}/${platform}/${BIN_NAME}"
  downloaded=0
  if command -v wget >/dev/null 2>&1; then
    if wget -qO "${tmp_bin}" "${url}"; then
      downloaded=1
    fi
  fi
  if [[ "${downloaded}" -ne 1 ]] && command -v curl >/dev/null 2>&1; then
    if curl -fsSL "${url}" -o "${tmp_bin}"; then
      downloaded=1
    fi
  fi
  if [[ "${downloaded}" -ne 1 ]]; then
    die "failed to download ${url} with wget or curl"
  fi
fi

try_install() {
  local candidate="$1"
  local target="${candidate}/${BIN_NAME}"

  if [[ -d "${candidate}" && -w "${candidate}" ]]; then
    install -m 755 "${tmp_bin}" "${target}"
  elif mkdir -p "${candidate}" 2>/dev/null && [[ -w "${candidate}" ]]; then
    install -m 755 "${tmp_bin}" "${target}"
  elif command -v sudo >/dev/null 2>&1; then
    if sudo mkdir -p "${candidate}" && sudo install -m 755 "${tmp_bin}" "${target}"; then
      return 0
    fi
    return 1
  else
    return 1
  fi
}

installed=0
for candidate in "${install_candidates[@]}"; do
  target="${candidate}/${BIN_NAME}"
  if try_install "${candidate}"; then
    install_dir="${candidate}"
    installed=1
    break
  fi
done

if [[ "${installed}" -ne 1 ]]; then
  die "cannot install ${BIN_NAME}; set INSTALL_DIR to a writable directory in PATH"
fi

echo "Installed ${BIN_NAME} to ${target}"

if ! path_has "${install_dir}"; then
  echo "Notice: ${install_dir} is not in your current PATH."
  echo "Add it first, for example: export PATH=\"${install_dir}:\$PATH\""
fi

cat <<'EOF'

Before running fae, set:
  export OPENAI_API_KEY="sk-..."
  export FAE_DEFAULT_MODEL="gpt-..."

Then initialize and start:
  fae --ws main init
  fae --ws main agent --chat
EOF
