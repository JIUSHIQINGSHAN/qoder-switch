#!/usr/bin/env bash
# Qoder Switch 的一键构建。工具链与产物位置都在这一个文件里，不要靠记忆推环境变量。
#
# 支持 Windows（Git-Bash/MSYS）与 macOS/Linux 两类宿主，行为按宿主分叉：
# - Windows 保持原样：工具链与 target 钉在 E:（C: 盘装不下一次 release target，实测约 7GB），
#   构建前杀旧进程防 LNK1104 占用拒绝访问。
# - macOS/Linux 用默认工具链位置，产物名没有 .exe。
set -euo pipefail

case "$(uname -s)" in
  MINGW*|MSYS*|CYGWIN*) HOST=windows ;;
  Darwin) HOST=macos ;;
  *) HOST=linux ;;
esac

if [ "$HOST" = windows ]; then
  export RUSTUP_HOME="${RUSTUP_HOME:-E:/rustup}"
  export CARGO_HOME="${CARGO_HOME:-E:/cargo}"
  # target-dir 由 .cargo/config.toml 兜底；这里显式覆盖以防 env 里有残留值。
  export CARGO_TARGET_DIR="${CARGO_TARGET_DIR:-E:/qs-target}"
  EXE=.exe
else
  # 非 Windows 宿主不覆盖 rustup/cargo 的默认位置；只给 target 一个默认值。
  export CARGO_TARGET_DIR="${CARGO_TARGET_DIR:-$PWD/target}"
  EXE=""
fi

# PATH 必须是当前宿主的 POSIX 形式。在 MSYS 下写成 E:/cargo/bin 的话，npx tauri 派生的
# cargo 子进程会找不到命令（rustup 自己的 RUSTUP_HOME/CARGO_HOME 反而要 Windows 形式）。
if [ "$HOST" = windows ] && command -v cygpath >/dev/null 2>&1; then
  cargo_bin="$(cygpath -u "${CARGO_HOME:-$HOME/.cargo}")/bin"
else
  cargo_bin="${CARGO_HOME:-$HOME/.cargo}/bin"
fi
export PATH="$cargo_bin:$PATH"
command -v cargo >/dev/null || { echo "找不到 cargo（CARGO_HOME=${CARGO_HOME:-默认} → $cargo_bin）"; exit 1; }

cd "$(dirname "$0")/.."

# 各宿主的 tauri bundle 目标：不写在命令行里靠记忆，集中在这一处。
case "$HOST" in
  windows) BUNDLES="nsis" ;;
  macos)   BUNDLES="app,dmg" ;;
  *)       BUNDLES="deb,appimage" ;;
esac

# 构建前先收掉上一次留下的进程，否则链接期会因文件占用失败。
# Windows 是 LNK1104；macOS 上运行中的 .app 二进制不可覆盖（会被 SIGKILL 后由
# 内核拒绝写入），同样要先停。
kill_previous() {
  case "$HOST" in
    windows) taskkill //IM "qoder-switch.exe" //F >/dev/null 2>&1 || true ;;
    macos)   pkill -x "qoder-switch" >/dev/null 2>&1 || true ;;
    *)       pkill -x "qoder-switch" >/dev/null 2>&1 || true ;;
  esac
}

case "${1:-all}" in
  deps)
    npm install --no-audit --no-fund
    ;;
  icons)
    # 资源编译要求 src-tauri/icons/ 里的文件真实存在，缺 icon 会直接编译失败。
    # Windows 要 .ico、macOS 的 .app 要 .icns（tauri.conf.json 的 bundle.icon 两个都列了）。
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
    kill_previous
    cargo build -p qoder-switch
    echo "产物: $CARGO_TARGET_DIR/debug/qoder-switch$EXE"
    ;;
  release)
    kill_previous
    if [ -z "${TAURI_SIGNING_PRIVATE_KEY:-}" ] && [ -f "$HOME/.tauri/qoder-switch.key" ]; then
      export TAURI_SIGNING_PRIVATE_KEY="$(cat "$HOME/.tauri/qoder-switch.key")"
      export TAURI_SIGNING_PRIVATE_KEY_PASSWORD=""
    fi
    # 没有发布私钥时不能让它炸在最后一步：tauri 会先把 .app/.dmg/NSIS 全部产出，
    # 再在 updater 签名那一步以非 0 退出 —— 于是"包其实做好了，脚本却报失败"，
    # 而且 set -e 让后面的产物清单根本不打印。本机 mac 开发机就没有那把私钥
    # （它在 Windows 开发机上，正式签名走 CI secret），所以这里按有没有密钥分流。
    if [ -z "${TAURI_SIGNING_PRIVATE_KEY:-}" ]; then
      echo "提示: 未找到 TAURI_SIGNING_PRIVATE_KEY（也没有 ~/.tauri/qoder-switch.key）。" >&2
      echo "      本次跳过 updater 签名产物（.sig / .app.tar.gz），安装包本体照常产出。" >&2
      echo "      正式发版请不要用这条路径 —— 交给 CI（tag 触发，私钥在仓库 Secrets）。" >&2
      npx tauri build --bundles "$BUNDLES" \
        --config '{"bundle":{"createUpdaterArtifacts":false}}'
    else
      # 会自己跑 beforeBuildCommand（npm run build），不必先 ./build.sh web
      npx tauri build --bundles "$BUNDLES"
    fi
    # 产物清单按宿主取：macOS 是 .app/.dmg，Windows 是 NSIS 的 .exe。
    find "$CARGO_TARGET_DIR/release/bundle" \( -name "*.app" -o -name "*.dmg" -o -name "*.exe" \) -print
    ;;
  all)
    "$0" deps && "$0" icons && "$0" test && "$0" release
    ;;
  *)
    echo "用法: $0 [deps|icons|test|web|debug|release|all]（当前宿主: ${HOST}，bundles: ${BUNDLES}）" >&2
    exit 2
    ;;
esac
