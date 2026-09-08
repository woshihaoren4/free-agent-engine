#!/usr/bin/env bash
set -euo pipefail

BIN_NAME="${BIN_NAME:-fae}"
BASE_URL="${FAE_INSTALL_BASE_URL:-https://woshihaoren4.github.io/free-agent-engine/bin}"
SKILLS_BASE_URL="${FAE_SKILLS_BASE_URL:-${BASE_URL%/bin}/skills}"

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

download() {
  local url="$1"
  local output="$2"

  if command -v curl >/dev/null 2>&1; then
    if [[ "${url}" == https://* ]]; then
      curl --proto '=https' --tlsv1.2 -fsSL "${url}" -o "${output}"
    else
      curl -fsSL "${url}" -o "${output}"
    fi
  elif command -v wget >/dev/null 2>&1; then
    wget -qO "${output}" "${url}"
  else
    die "curl or wget is required"
  fi
}

sha256() {
  if command -v sha256sum >/dev/null 2>&1; then
    sha256sum "$1" | awk '{ print $1 }'
  elif command -v shasum >/dev/null 2>&1; then
    shasum -a 256 "$1" | awk '{ print $1 }'
  else
    die "sha256sum or shasum is required to verify the download"
  fi
}

expand_home() {
  case "$1" in
    "~") printf '%s\n' "${HOME}" ;;
    "~/"*) printf '%s/%s\n' "${HOME}" "${1#\~/}" ;;
    *) printf '%s\n' "$1" ;;
  esac
}

install_dir="${INSTALL_DIR:-}"
fae_host="$(expand_home "${FAE_HOST:-${HOME}/.fae}")"
skills_dir="$(expand_home "${FAE_SKILLS_DIR:-${fae_host}/skills}")"
skill_files=(
  "fae-agent/SKILL.md"
  "fae-agent/references/agent-api.md"
  "fae-agent/references/recipes.md"
  "fae-workflow/SKILL.md"
  "fae-workflow/references/recipes.md"
  "fae-workflow/references/workflow-api.md"
  "weather/SKILL.md"
)

if [[ -z "${install_dir}" ]]; then
  IFS=":" read -r -a path_dirs <<< "${PATH:-}"
  for candidate in "${path_dirs[@]}"; do
    if [[ -n "${candidate}" && -d "${candidate}" && -w "${candidate}" ]] &&
      { [[ "${candidate}" == "${HOME}/"* ]] ||
        [[ "${candidate}" == "/usr/local/bin" ]] ||
        [[ "${candidate}" == "/opt/homebrew/bin" ]]; }; then
      install_dir="${candidate}"
      break
    fi
  done

  install_dir="${install_dir:-${HOME}/.local/bin}"
fi

tmp_dir="$(mktemp -d)"
trap 'rm -rf "${tmp_dir}"' EXIT
tmp_bin="${tmp_dir}/${BIN_NAME}"
tmp_checksum="${tmp_dir}/${BIN_NAME}.sha256"

script_path="${BASH_SOURCE[0]:-}"
if [[ -n "${script_path}" && -f "${script_path}" ]]; then
  script_dir="$(cd -- "$(dirname -- "${script_path}")" && pwd)"
  local_bin="${script_dir}/${platform}/${BIN_NAME}"
  local_skills_dir="${script_dir}/../skills"
else
  local_bin=""
  local_skills_dir=""
fi

if [[ -n "${local_bin}" && -f "${local_bin}" ]]; then
  cp "${local_bin}" "${tmp_bin}"
  local_checksum="${local_bin}.sha256"
  [[ -f "${local_checksum}" ]] ||
    die "checksum file not found: ${local_checksum}"
  cp "${local_checksum}" "${tmp_checksum}"
else
  url="${BASE_URL}/${platform}/${BIN_NAME}"
  echo "Downloading ${url}"
  download "${url}" "${tmp_bin}" ||
    die "failed to download ${url}"
  download "${url}.sha256" "${tmp_checksum}" ||
    die "failed to download ${url}.sha256"
fi

expected_hash="$(awk 'NF { print $1; exit }' "${tmp_checksum}")"
[[ "${expected_hash}" =~ ^[[:xdigit:]]{64}$ ]] ||
  die "invalid checksum received for ${BIN_NAME}"
actual_hash="$(sha256 "${tmp_bin}")"
[[ "${actual_hash}" == "${expected_hash}" ]] ||
  die "checksum verification failed for ${BIN_NAME}"

tmp_skills_dir="${tmp_dir}/skills"
for skill_file in "${skill_files[@]}"; do
  tmp_skill="${tmp_skills_dir}/${skill_file}"
  mkdir -p "$(dirname -- "${tmp_skill}")"
  if [[ -n "${local_skills_dir}" && -f "${local_skills_dir}/${skill_file}" ]]; then
    cp "${local_skills_dir}/${skill_file}" "${tmp_skill}"
  else
    download "${SKILLS_BASE_URL}/${skill_file}" "${tmp_skill}" ||
      die "failed to download skill file ${skill_file}"
  fi
  [[ -s "${tmp_skill}" ]] ||
    die "downloaded skill file is empty: ${skill_file}"
done

mkdir -p "${install_dir}" ||
  die "cannot create ${install_dir}; set INSTALL_DIR to a writable directory"
[[ -w "${install_dir}" ]] ||
  die "${install_dir} is not writable; set INSTALL_DIR to a writable directory"
target="${install_dir}/${BIN_NAME}"
install -m 755 "${tmp_bin}" "${target}"

for skill_file in "${skill_files[@]}"; do
  skill_target="${skills_dir}/${skill_file}"
  mkdir -p "$(dirname -- "${skill_target}")" ||
    die "cannot create skill directory under ${skills_dir}"
  install -m 644 "${tmp_skills_dir}/${skill_file}" "${skill_target}" ||
    die "cannot install skill file ${skill_target}"
done

echo "Installed ${BIN_NAME} to ${target} (checksum verified)"
echo "Installed bundled skills to ${skills_dir}"

if ! path_has "${install_dir}"; then
  echo "Notice: ${install_dir} is not in your current PATH."
  echo "Add it first, for example: export PATH=\"${install_dir}:\$PATH\""
fi

cat <<EOF

Configure ${fae_host}/agents/fae_config.json and fae_prompt.txt,
then set OPENAI_API_KEY and run:
  fae
EOF
