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
if [[ -z "${install_dir}" ]]; then
  for candidate in /usr/local/bin /opt/homebrew/bin "${HOME}/.local/bin" "${HOME}/bin"; do
    if path_has "${candidate}" && [[ -d "${candidate}" && -w "${candidate}" ]]; then
      install_dir="${candidate}"
      break
    fi
  done
fi

if [[ -z "${install_dir}" ]]; then
  IFS=":" read -r -a path_dirs <<< "${PATH:-}"
  for candidate in "${path_dirs[@]}"; do
    if [[ -n "${candidate}" && -d "${candidate}" && -w "${candidate}" ]]; then
      install_dir="${candidate}"
      break
    fi
  done
fi

if [[ -z "${install_dir}" ]]; then
  if path_has "/usr/local/bin"; then
    install_dir="/usr/local/bin"
  else
    install_dir="${HOME}/.local/bin"
  fi
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
  if command -v curl >/dev/null 2>&1; then
    curl -fsSL "${url}" -o "${tmp_bin}"
  elif command -v wget >/dev/null 2>&1; then
    wget -qO "${tmp_bin}" "${url}"
  else
    die "curl or wget is required to download ${url}"
  fi
fi

target="${install_dir}/${BIN_NAME}"
if [[ -d "${install_dir}" && -w "${install_dir}" ]]; then
  install -m 755 "${tmp_bin}" "${target}"
elif mkdir -p "${install_dir}" 2>/dev/null && [[ -w "${install_dir}" ]]; then
  install -m 755 "${tmp_bin}" "${target}"
elif command -v sudo >/dev/null 2>&1; then
  sudo mkdir -p "${install_dir}"
  sudo install -m 755 "${tmp_bin}" "${target}"
else
  die "cannot write to ${install_dir}; set INSTALL_DIR to a writable directory in PATH"
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
