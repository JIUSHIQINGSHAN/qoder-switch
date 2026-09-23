#!/usr/bin/env bash
# 打包 npm 多平台形态：构建 server + 前端，把产物暂存进 npm/ 各平台包目录。
# 之后由维护者手动 npm publish（需要 npm 账号与发布令牌，见 npm/README.md）。
#
# 用法：
#   bash scripts/pack-npm.sh                      # 只打当前宿主这一个平台包
#   bash scripts/pack-npm.sh aarch64-apple-darwin x86_64-apple-darwin
#                                                 # 在 mac 上同时出 Apple 硅与 Intel 两个包
#
# 一次跑不了全部平台：Windows 上造不出 darwin 二进制，macOS 上也造不出 .exe。
# 所以每个平台各跑一次本脚本，最后一起 publish。缺哪个平台包，那平台的
# `npm install -g qoder-switch` 就会在 postinstall 阶段静默跳过（见 install.js）。
#
# 产物不入库（npm/.gitignore 已挡）；发布前用 `npm pack --dry-run` 核对文件清单。
set -euo pipefail

case "$(uname -s)" in
  MINGW*|MSYS*|CYGWIN*) HOST=windows ;;
  Darwin) HOST=macos ;;
  *) HOST=linux ;;
esac

if [ "$HOST" = windows ]; then
  export RUSTUP_HOME="${RUSTUP_HOME:-E:/rustup}"
  export CARGO_HOME="${CARGO_HOME:-E:/cargo}"
  export CARGO_TARGET_DIR="${CARGO_TARGET_DIR:-E:/qs-target}"
else
  export CARGO_TARGET_DIR="${CARGO_TARGET_DIR:-$PWD/target}"
fi

if [ "$HOST" = windows ] && command -v cygpath >/dev/null 2>&1; then
  export PATH="$(cygpath -u "${CARGO_HOME:-$HOME/.cargo}")/bin:$PATH"
else
  export PATH="${CARGO_HOME:-$HOME/.cargo}/bin:$PATH"
fi

cd "$(dirname "$0")/.."
VERSION=$(node -p "require('./package.json').version")
echo "== qoder-switch npm 打包 v${VERSION}（宿主: ${HOST}）=="

# 强制核对版本一致性，任一不符直接终止，防止发错版本包
node -e "
  const fs = require('fs');
  const expected = '$VERSION';
  const checks = [
    ['src-tauri/tauri.conf.json', () => require('./src-tauri/tauri.conf.json').version],
    ['npm/package.json', () => require('./npm/package.json').version],
    ...fs.readdirSync('npm/platform').map(d => [
      'npm/platform/' + d + '/package.json',
      () => require('./npm/platform/' + d + '/package.json').version,
    ]),
  ];
  for (const [f, getV] of checks) {
    if (getV() !== expected) {
      console.error(\`版本不一致: \${f} 为 \${getV()}，期望 \${expected}\`);
      process.exit(1);
    }
  }
  const cargoFiles = ['crates/qs-switch-core/Cargo.toml', 'crates/qs-switch-server/Cargo.toml', 'src-tauri/Cargo.toml'];
  for (const cf of cargoFiles) {
    const content = fs.readFileSync(cf, 'utf8');
    const m = content.match(/^version\s*=\s*\"([^\"]+)\"/m);
    if (!m || m[1] !== expected) {
      console.error(\`版本不一致: \${cf} 声明为 \${m ? m[1] : 'null'}，期望 \${expected}\`);
      process.exit(1);
    }
  }
  console.log(\`所有 \${checks.length + cargoFiles.length} 处版本声明严格一致: \` + expected);
"

echo "== 1/3 构建 webui 服务端（release）=="
# rust target → (npm 平台包目录, 二进制文件名)。新增平台只改这张表。
target_key() {
  case "$1" in
    x86_64-pc-windows-msvc) echo "qoder-switch-win32-x64 qs-switch-server.exe" ;;
    aarch64-apple-darwin)   echo "qoder-switch-darwin-arm64 qs-switch-server" ;;
    x86_64-apple-darwin)    echo "qoder-switch-darwin-x64 qs-switch-server" ;;
    *) return 1 ;;
  esac
}

HOST_TARGETS="$(rustc -vV | sed -n 's/^host: //p')"
TARGETS=("$@")
[ ${#TARGETS[@]} -eq 0 ] && TARGETS=("$HOST_TARGETS")

BUILT=()
for t in "${TARGETS[@]}"; do
  if ! spec="$(target_key "$t")"; then
    echo "跳过 ${t}：这张表里没有对应的 npm 平台包" >&2
    continue
  fi
  # shellcheck read -r pkg bin
  read -r pkg bin <<<"$spec"
  if [ "$t" != "$HOST_TARGETS" ]; then
    rustup target add "$t" >/dev/null 2>&1 || true
    cargo build --release -p qs-switch-server --target "$t"
    # 显式带 --target 时 cargo 会多插一层 target triple：
    # $CARGO_TARGET_DIR/<triple>/release/，不是 release/<triple>/。
    src="$CARGO_TARGET_DIR/$t/release/$bin"
  else
    cargo build --release -p qs-switch-server
    src="$CARGO_TARGET_DIR/release/$bin"
  fi
  [ -f "$src" ] || { echo "构建后找不到产物 $src" >&2; exit 1; }
  mkdir -p "npm/platform/$pkg/bin"
  cp "$src" "npm/platform/$pkg/bin/$bin"
  # macOS/Linux 上必须保住可执行位，否则装完直接 Permission denied。
  [ "$HOST" = windows ] || chmod 0755 "npm/platform/$pkg/bin/$bin"
  BUILT+=("$pkg")
  echo "  → npm/platform/$pkg/bin/$bin"
done

echo "== 2/3 构建前端 dist =="
npm run build

echo "== 3/3 暂存主包前端资源 =="
# 主包：前端 dist（npm pack 不看 .gitignore，files 字段会带上 webui_dist）
rm -rf npm/webui_dist
cp -r dist npm/webui_dist

echo "== 完成。已产出平台包：${BUILT[*]:-（无）} =="
for p in "${BUILT[@]}"; do
  echo "  (cd npm/platform/$p && npm pack --dry-run)"
done
echo "  (cd npm && npm pack --dry-run)"
echo "其余平台请在对应宿主上再各跑一次本脚本。"
