#!/usr/bin/env bash
# Qoder Switch 的一键构建。工具链与产物位置都在这一个文件里，不要靠记忆推环境变量。
#
# 支持 Windows（Git-Bash/MSYS）与 macOS/Linux 两类宿主，行为按宿主分叉：
# - Windows 保持原样：工具链与 target 钉在 E:（C: 盘装不下一次 release target，实测约 7GB），
#   构建前杀旧进程防 LNK1104 占用拒绝访问。
# - macOS/Linux 用默认工具链位置，产物名没有 .exe。
set -euo pipefail

# 先切到仓库根，再推导任何相对路径。下面 `CARGO_TARGET_DIR` 的默认值依赖 $PWD，
# 若把它留在切目录之前求值，从仓库外调用本脚本时 target 会落到调用者的目录去 ——
# 构建锁与 clean.sh 的 TARGET_DIR 也随之错位，护栏等于失效。
cd "$(dirname "$0")/.."

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
command -v cargo >/dev/null || { echo "找不到 cargo（CARGO_HOME=${CARGO_HOME:-默认} → ${cargo_bin}）"; exit 1; }

ROOT="$PWD"

# ── 磁盘空间护栏 ─────────────────────────────────────────────────────────────
#
# 空间不足时 cargo 会在编译中途失败，而报错常常**不是**"磁盘已满"：rustc 建临时目录用的是
# create_dir 而非 create_dir_all，父目录写不进去（或被别的进程删掉）都表现为 ENOENT ——
# `couldn't create a temp dir: No such file or directory at path .../target/debug/deps/rmetaXXXX`，
# 现场看起来像"target 被谁删了"或"文件系统坏了"，很难往磁盘上想。
#
# 本仓库真实踩过一次：磁盘 95% 满，用户为腾空间先删了 target 再立刻构建，删除还没落地、
# cargo 已经建好 deps 开始编译，于是编译到一半目录被抽走。这里把"跑到一半炸"提前成
# "一开始就说清楚"。
#
# 阈值取实测需求的两倍：Rust 全量编译 peak 之外还要留系统 swap、编辑器索引与系统更新的空间。
# 实测（macOS/aarch64，从零构建）：`test` 让 target 长到 2.5GB，`release` 再加约 3GB，
# 所以下面按 test=3 / release=4 / all=6 给数，两倍后分别是 6G / 8G / 12G。
require_disk() { # $1 = 本次构建实测所需 GB
  local need="$1" avail
  avail="$(df -k "$ROOT" 2>/dev/null | awk 'NR==2 {print int($4/1048576)}')"
  [ -n "$avail" ] || return 0   # 取不到可用空间就不拦，避免在非常规文件系统上误伤
  [ "$avail" -ge "$((need * 2))" ] && return 0
  echo "磁盘可用空间不足：约 ${avail}G；本次构建实测需要约 ${need}G，留一倍余量需 $((need * 2))G。" >&2
  echo "现在继续大概率会在编译中途失败，且报错多为 ENOENT 而非磁盘已满，很难排查。" >&2
  echo "先释放空间再重跑。常见可回收项（体积请自行 du -sh 确认）：" >&2
  echo "  go clean -modcache          # ~/go/pkg/mod" >&2
  echo "  npm cache clean --force     # ~/.npm" >&2
  echo "  微信 → 设置 → 通用 → 存储空间管理" >&2
  echo "  Docker Desktop → Troubleshoot → 清理不用的镜像与卷" >&2
  exit 1
}

# ── 构建互斥锁 ───────────────────────────────────────────────────────────────
#
# clean.sh 会 `rm -rf` 掉 target（几 GB，要数十秒）。若与构建交叠，cargo 已经建好的
# target/debug/deps 会被抽走，rustc 随即报 `couldn't create a temp dir: No such file
# or directory` —— 看起来像"target 凭空消失"。本仓库真实踩过一次。
# 锁放在 target 目录里，clean.sh 删 target 之前会先查这把锁。
#
# 用环境变量做递归守卫：`all` 会依次调用本脚本的其它子命令，子进程继承该变量后不再加锁，
# 由最外层持有到整个流程结束。PID 已不存在则视为陈旧锁，直接接管。
BUILD_LOCK="${CARGO_TARGET_DIR}/.qs-build-lock"
if [ -z "${QS_BUILD_LOCKED:-}" ]; then
  mkdir -p "$CARGO_TARGET_DIR" 2>/dev/null || true
  if [ -e "$BUILD_LOCK" ]; then
    holder="$(cat "$BUILD_LOCK/pid" 2>/dev/null || true)"
    if [ -n "$holder" ] && kill -0 "$holder" 2>/dev/null; then
      echo "已有构建正在进行（PID ${holder}），拒绝并行启动。" >&2
      echo "并行构建会互相踩 target 目录，并让 cargo 报出迷惑性的 ENOENT。" >&2
      exit 1
    fi
    rm -rf -- "$BUILD_LOCK"
  fi
  mkdir "$BUILD_LOCK" || { echo "无法创建构建锁: ${BUILD_LOCK}" >&2; exit 1; }
  echo $$ > "$BUILD_LOCK/pid"
  trap 'rm -rf -- "$BUILD_LOCK"' EXIT INT TERM
  export QS_BUILD_LOCKED=1
fi

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
    require_disk 3
    # `cargo test` 要编译 src-tauri，而它的 `frontendDist`（../dist）必须真实存在，
    # 否则 tauri 的 generate_context! 在编译期直接 panic。`all` 里 test 排在 release
    # 之前，dist 要等 release 的 beforeBuildCommand 才生成；clean.sh 也会删掉 dist。
    # 两种情形下本子命令都会先炸在编译期，所以缺了就补一次前端构建。
    [ -f dist/index.html ] || npm run build
    # CI 只跑常规集；本机再补跑被 #[ignore] 的真机证据测试（绑定本机 Qoder 布局）。
    cargo test --workspace
    cargo test --workspace -- --ignored
    ;;
  web)
    npm run build
    ;;
  debug)
    require_disk 3
    kill_previous
    cargo build -p qoder-switch
    echo "产物: $CARGO_TARGET_DIR/debug/qoder-switch$EXE"
    ;;
  release)
    require_disk 4
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
    # debug 与 release 两套 target 都要落地，实测合计约 6GB。
    require_disk 6
    "$0" deps && "$0" icons && "$0" test && "$0" release
    ;;
  *)
    echo "用法: $0 [deps|icons|test|web|debug|release|all]（当前宿主: ${HOST}，bundles: ${BUNDLES}）" >&2
    exit 2
    ;;
esac
