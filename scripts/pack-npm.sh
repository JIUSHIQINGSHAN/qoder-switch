#!/usr/bin/env bash
# 打包 npm 双形态：构建 server + 前端，把产物暂存进 npm/ 各包目录。
# 之后由维护者手动 npm publish（需要 npm 账号与发布令牌，见 npm/README.md）。
#
# 产物不入库（npm/.gitignore 已挡）；发布前用 `npm pack --dry-run` 核对文件清单。
set -euo pipefail

export RUSTUP_HOME="${RUSTUP_HOME:-E:/rustup}"
export CARGO_HOME="${CARGO_HOME:-E:/cargo}"
export CARGO_TARGET_DIR="${CARGO_TARGET_DIR:-E:/qs-target}"

if command -v cygpath >/dev/null 2>&1; then
  export PATH="$(cygpath -u "$CARGO_HOME")/bin:$PATH"
else
  export PATH="$CARGO_HOME/bin:$PATH"
fi

cd "$(dirname "$0")/.."
ROOT="$PWD"
VERSION=$(node -p "require('./package.json').version")
echo "== qoder-switch npm 打包 v$VERSION =="

echo "== 1/3 构建 webui 服务端（release）=="
cargo build --release -p qs-switch-server

echo "== 2/3 构建前端 dist =="
npm run build

echo "== 3/3 暂存产物 =="
# 平台包：二进制
mkdir -p npm/platform/qoder-switch-win32-x64/bin
cp "$CARGO_TARGET_DIR/release/qs-switch-server.exe" \
   npm/platform/qoder-switch-win32-x64/bin/qs-switch-server.exe

# 主包：前端 dist（npm pack 不看 .gitignore，files 字段会带上 webui_dist）
rm -rf npm/webui_dist
cp -r dist npm/webui_dist

echo "== 完成。核对清单后发布： =="
echo "  (cd npm/platform/qoder-switch-win32-x64 && npm pack --dry-run)"
echo "  (cd npm && npm pack --dry-run)"
echo "  版本一致性检查：package.json/npm×2、tauri.conf.json、Cargo.toml×3 应全为 $VERSION"
