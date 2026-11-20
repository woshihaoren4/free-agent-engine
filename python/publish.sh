#!/usr/bin/env bash
# 打包并发布 fae-plugin-py。环境变量：
#   PYPI_API_TOKEN         PyPI API Token；发布时需要，可用 TWINE_PASSWORD 替代，
#                          均未设置时从终端隐藏输入；非交互运行必须设置其一。
#   TWINE_PASSWORD         优先于 PYPI_API_TOKEN；不写入文件、不作为命令行参数。
#   TWINE_USERNAME         可选，默认 __token__。
#   TWINE_REPOSITORY_URL   可选，默认 https://upload.pypi.org/legacy/；
#                          TestPyPI 使用 https://test.pypi.org/legacy/，
#                          并提供 TestPyPI 自己的 Token。
#   PYTHON_BIN             可选，默认 python3，需 Python >=3.10；缺少 ensurepip
#                          时通过该解释器的 pip 临时安装 virtualenv。
#   DIST_DIR               可选，默认脚本目录下 fae-plugin-py/dist。
#   PIP_INDEX_URL          可选，pip/build 使用的包索引地址，可配置可用镜像。
#   PIP_EXTRA_INDEX_URL    可选，pip/build 使用的额外包索引地址。
# 用法：
#   bash python/publish.sh --build-only  # 仅构建、校验，无需 Token
#   bash python/publish.sh               # 构建、校验并发布，缺少 Token 时隐藏输入
# 版本号从 fae-plugin-py/pyproject.toml 读取；每次发布先更新 version。
# 构建工具自动安装到临时虚拟环境，临时文件退出时清理。

set +x
set -euo pipefail

build_only=false
case "${1:-}" in
    "") ;;
    --build-only) build_only=true ;;
    -h|--help)
        printf '用法：%s [--build-only]\n环境变量说明见文件顶部。\n' "$0"
        exit 0
        ;;
    *) printf '未知参数：%s\n' "$1" >&2; exit 2 ;;
esac
if (( $# > 1 )); then
    printf '参数过多。\n' >&2
    exit 2
fi

script_dir="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
package_dir="$script_dir/fae-plugin-py"
python_bin="${PYTHON_BIN:-python3}"
dist_dir="${DIST_DIR:-$package_dir/dist}"

"$python_bin" -c 'import sys; sys.exit(0 if sys.version_info >= (3, 10) else "需要 Python >=3.10")'

if [[ "$build_only" == false ]]; then
    export TWINE_USERNAME="${TWINE_USERNAME:-__token__}"
    export TWINE_REPOSITORY_URL="${TWINE_REPOSITORY_URL:-https://upload.pypi.org/legacy/}"
    token="${TWINE_PASSWORD:-${PYPI_API_TOKEN:-}}"
    if [[ -z "$token" ]]; then
        if [[ ! -t 0 ]]; then
            printf '发布需要设置 PYPI_API_TOKEN 或 TWINE_PASSWORD。\n' >&2
            exit 1
        fi
        read -r -s -p '请输入目标仓库的 API Token：' token
        printf '\n'
    fi
    if [[ -z "$token" ]]; then
        printf 'API Token 不能为空。\n' >&2
        exit 1
    fi
    export TWINE_PASSWORD="$token"
    unset token
fi

work_dir="$(mktemp -d "${TMPDIR:-/tmp}/fae-plugin-publish.XXXXXXXX")"
trap 'rm -rf -- "$work_dir"' EXIT
if "$python_bin" -c 'import ensurepip' >/dev/null 2>&1; then
    "$python_bin" -m venv "$work_dir/venv"
else
    "$python_bin" -m pip install --disable-pip-version-check \
        --target "$work_dir/bootstrap" virtualenv
    PYTHONPATH="$work_dir/bootstrap${PYTHONPATH:+:$PYTHONPATH}" \
        "$python_bin" -m virtualenv "$work_dir/venv"
fi
build_python="$work_dir/venv/bin/python"
"$build_python" -m pip install --disable-pip-version-check build twine

# 从干净副本构建，避免在源码目录留下 build/、egg-info 等中间文件。
"$build_python" - "$package_dir" "$work_dir/source" <<'PY'
import shutil
import sys

shutil.copytree(
    sys.argv[1], sys.argv[2],
    ignore=shutil.ignore_patterns(
        "__pycache__", "*.pyc", "*.egg-info", "build", "dist", ".venv", ".git",
    ),
)
PY
"$build_python" -m build --outdir "$work_dir/dist" "$work_dir/source"
shopt -s nullglob
artifacts=("$work_dir/dist/"*.whl "$work_dir/dist/"*.tar.gz)
if (( ${#artifacts[@]} != 2 )); then
    printf '预期生成一个 wheel 和一个源码压缩包。\n' >&2
    exit 1
fi
"$build_python" -m twine check --strict "${artifacts[@]}"
mkdir -p -- "$dist_dir"
cp -- "${artifacts[@]}" "$dist_dir/"
printf '构建和校验通过，产物保存在：%s\n' "$dist_dir"

if [[ "$build_only" == false ]]; then
    "$build_python" -m twine upload --non-interactive \
        --repository-url "$TWINE_REPOSITORY_URL" "${artifacts[@]}"
    printf '发布完成。\n'
fi
