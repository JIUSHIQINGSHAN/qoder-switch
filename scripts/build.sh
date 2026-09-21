#!/usr/bin/env bash
# Qoder Switch 的一键构建。工具链与产物位置都在这一个文件里，不要靠记忆推环境变量。
set -euo pipefail

export RUSTUP_HOME="${RUSTUP_HOME:-E:/rustup}"
export CARGO_HOME="${CARGO_HOME:-E:/cargo}"
# target-dir 由 .cargo/config.toml 兜底；这里显式覆盖以防 env 里有残留值。
export CARGO_TARGET_DIR="${CARGO_TARGET_DIR:-E:/qs-target}"

# PATH 必须是 MSYS 的 POSIX 形式。写成 E:/cargo/bin 的话，npx tauri 派生的
# cargo 子进程会找不到命令（rustup 自己的 RUSTUP_HOME/CARGO_HOME 反而要 Windows 形式）。
if command -v cygpath >/dev/null 2>&1; then
  cargo_bin="$(cygpath -u "$CARGO_HOME")/bin"
else
  cargo_bin="$CARGO_HOME/bin"
fi
export PATH="$cargo_bin:$PATH"
command -v cargo >/dev/null || { echo "找不到 cargo（CARGO_HOME=$CARGO_HOME → $cargo_bin）"; exit 1; }

cd "$(dirname "$0")/.."

case "${1:-all}" in
  deps)
    npm install --no-audit --no-fund
    ;;
  icons)
    # Windows 资源编译要求 src-tauri/icons/ 里的文件真实存在，缺 icon 会直接编译失败。
    [ -f public/app-icon.png ] || { echo "缺 public/app-icon.png（1024x1024 源图）"; exit 1; }
    npx tauri icon public/app-icon.png
    rm -rf src-tauri/icons/android src-tauri/icons/ios
    ;;
  test)
    # CI 只跑常规集；本机再补跑被 #[ignore] 的真机证据测试（绑定本机 Qoder 布局）。
    cargo test --workspace
    cargo test --workspace -- --ignored
    ;;
  web)
    npm run build
    ;;
  debug)
    # Windows 构建前先杀旧进程，防 LNK1104 占用拒绝访问（见 HANDOFF §7 第 10 条）
    taskkill //IM qoder-switch.exe //F >/dev/null 2>&1 || true
    cargo build -p qoder-switch
    echo "产物: $CARGO_TARGET_DIR/debug/qoder-switch.exe"
    ;;
  release)
    # Windows 构建前先杀旧进程，防 LNK1104 占用拒绝访问（见 HANDOFF §7 第 10 条）
    taskkill //IM qoder-switch.exe //F >/dev/null 2>&1 || true
    # 会自己跑 beforeBuildCommand（npm run build），不必先 ./build.sh web
    npx tauri build
    find "$CARGO_TARGET_DIR/release/bundle" -name '*.exe' -print
    ;;
  all)
    "$0" deps && "$0" icons && "$0" test && "$0" release
    ;;
  *)
    echo "用法: $0 [deps|icons|test|web|debug|release|all]" >&2
    exit 2
    ;;
esac
