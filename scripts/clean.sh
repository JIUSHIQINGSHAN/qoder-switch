#!/usr/bin/env bash
# 清理 scripts/build.sh（以及 scripts/pack-npm.sh）留下的构建缓存与构建产物。
#
# 为什么单独一个脚本：build.sh 会往六个地方写东西 —— cargo 的 target 目录、vite 的 dist、
# tauri-build 生成的 src-tauri/gen/schemas、tauri icon 的备份目录，以及 pack-npm 暂存的
# npm/webui_dist 与 npm/platform/*/bin。只删 target 会留下后面几处的陈旧产物，
# 下次构建可能拿旧 dist 或旧二进制打包。这里把全部落点集中成一张表，不靠记忆手敲 rm -rf。
#
# 用法:
#   bash scripts/clean.sh                # 列出清单，确认后清理（保留 node_modules）
#   bash scripts/clean.sh --dry-run      # 只列出将删除的路径与体积，不动任何文件
#   bash scripts/clean.sh --yes          # 免确认（非交互环境必须显式带）
#   bash scripts/clean.sh --all          # 连 node_modules 一起删（最彻底，之后需 npm install）
#   bash scripts/clean.sh -n -a          # 预览 --all 的效果
set -euo pipefail

usage() {
  cat <<'EOF'
用法: bash scripts/clean.sh [选项]

  -n, --dry-run   只列出待清理清单与体积，不删除任何文件
  -y, --yes       跳过确认（stdin 非交互时必须带，否则脚本拒绝删除）
  -a, --all       连同 node_modules 一起清理（之后需要重新 npm install）
  -h, --help      显示本帮助

默认清理 cargo/tauri 构建目录、dist、tauri 生成 schema、pack-npm 暂存产物、
vite 预构建缓存与 TypeScript 增量编译信息；保留 node_modules。
EOF
}

DRY_RUN=0
ASSUME_YES=0
WITH_NODE_MODULES=0

while [ $# -gt 0 ]; do
  case "$1" in
    -n|--dry-run) DRY_RUN=1 ;;
    -y|--yes)     ASSUME_YES=1 ;;
    -a|--all)     WITH_NODE_MODULES=1 ;;
    -h|--help)    usage; exit 0 ;;
    *) echo "未知参数: $1" >&2; usage >&2; exit 2 ;;
  esac
  shift
done

case "$(uname -s)" in
  MINGW*|MSYS*|CYGWIN*) HOST=windows ;;
  Darwin)               HOST=macos ;;
  *)                    HOST=linux ;;
esac

cd "$(dirname "$0")/.."
ROOT="$PWD"

# 必须是仓库根：防呆，避免在别处误删同名目录。
for f in Cargo.toml package.json scripts/build.sh; do
  [ -f "$f" ] || { echo "当前目录不像 qoder-switch 仓库根（缺 ${f}）: ${ROOT}" >&2; exit 1; }
done

# cargo target 目录的推导必须与 build.sh 完全同源，否则清了半天没清到真正的构建目录。
TARGET_DIR="${CARGO_TARGET_DIR:-$ROOT/target}"
if [ "$HOST" = windows ] && command -v cygpath >/dev/null 2>&1; then
  # Windows 上 build.sh 会把 target 钉在 E:（形如 E:/qs-target），MSYS 下要转成 POSIX 路径。
  TARGET_DIR="$(cygpath -u "$TARGET_DIR")"
fi
case "$TARGET_DIR" in
  /*) ;;
  *) TARGET_DIR="$ROOT/$TARGET_DIR" ;;
esac

human_size() { # $1 = KB
  awk -v kb="$1" 'BEGIN {
    if (kb >= 1048576)      printf "%.2f GB", kb / 1048576;
    else if (kb >= 1024)    printf "%.1f MB", kb / 1024;
    else                    printf "%d KB", kb;
  }'
}

declare -a M_DESC=() M_PATH=() M_KB=()
total_kb=0

record() { # $1 = 说明，$2 = 路径
  local desc="$1" p="$2" kb
  [ -e "$p" ] || [ -L "$p" ] || return 0
  kb="$(du -sk "$p" 2>/dev/null | awk '{print $1}')"
  kb="${kb:-0}"
  M_DESC+=("$desc")
  M_PATH+=("$p")
  M_KB+=("$kb")
  total_kb=$((total_kb + kb))
}

record_glob() { # $1 = 说明，$2 = 含通配符的模式
  local desc="$1" pat="$2" p
  while IFS= read -r p; do
    [ -n "$p" ] || continue
    record "$desc" "$p"
  done < <(compgen -G "$pat" || true)
}

# ── cargo / tauri 构建目录 ────────────────────────────────────────────────────
record "cargo/tauri 构建目录（含 release bundle、增量与依赖缓存）" "$TARGET_DIR"
if [ "$TARGET_DIR" != "$ROOT/target" ]; then
  record "cargo 构建目录（仓库内默认位置，环境变量指向了别处）" "$ROOT/target"
fi
record "tauri 旧布局构建目录" "$ROOT/src-tauri/target"

# ── 前端与 tauri 生成物 ──────────────────────────────────────────────────────
record "前端构建产物（vite build 的 outDir）" "$ROOT/dist"
record "tauri-build 生成的 ACL 与能力 schema" "$ROOT/src-tauri/gen/schemas"
record "tauri icon 的备份目录" "$ROOT/src-tauri/icons-backup"

# ── pack-npm.sh 的暂存产物 ───────────────────────────────────────────────────
record "pack-npm 暂存的前端资源" "$ROOT/npm/webui_dist"
record_glob "pack-npm 暂存的平台二进制" "$ROOT/npm/platform/*/bin"
record "pack-npm 旧布局的服务端二进制" "$ROOT/npm/bin/qs-switch-server"
record "pack-npm 旧布局的服务端二进制（.exe）" "$ROOT/npm/bin/qs-switch-server.exe"

# ── 各类缓存 ─────────────────────────────────────────────────────────────────
record "vite 依赖预构建缓存" "$ROOT/node_modules/.vite"
record "vite 依赖预构建缓存（临时）" "$ROOT/node_modules/.vite-temp"
record "node 工具链缓存" "$ROOT/node_modules/.cache"

# TypeScript 增量编译信息：tsc --noEmit 一般不产生，但一旦开了 incremental 会散落在各处。
while IFS= read -r f; do
  record "TypeScript 增量编译信息" "$f"
done < <(find "$ROOT" -name '*.tsbuildinfo' \
           -not -path '*/node_modules/*' -not -path '*/target/*' -print 2>/dev/null || true)

if [ "$WITH_NODE_MODULES" = 1 ]; then
  record "前端依赖目录（清理后需重新 npm install）" "$ROOT/node_modules"
fi

if [ "${#M_PATH[@]}" -eq 0 ]; then
  echo "没有发现需要清理的构建产物 —— 工作区已经是干净的。"
  exit 0
fi

echo "== 待清理清单（仓库根: ${ROOT}）=="
for i in "${!M_PATH[@]}"; do
  p="${M_PATH[$i]}"
  tag=""
  case "$p" in
    "$ROOT"/*) ;;
    *) tag="  ⚠ 位于仓库外" ;;
  esac
  printf '  %-9s %s%s\n' "$(human_size "${M_KB[$i]}")" "$p" "$tag"
  printf '            └ %s\n' "${M_DESC[$i]}"
done
echo "  合计可释放: $(human_size "$total_kb")，共 ${#M_PATH[@]} 项"

if [ "$DRY_RUN" = 1 ]; then
  echo "== --dry-run：以上内容均未删除 =="
  exit 0
fi

if [ "$ASSUME_YES" != 1 ]; then
  if [ -t 0 ]; then
    printf '确认删除以上 %d 项？[y/N] ' "${#M_PATH[@]}"
    read -r ans
    case "$ans" in
      y|Y|yes|YES) ;;
      *) echo "已取消，未删除任何文件。"; exit 0 ;;
    esac
  else
    echo "非交互环境：请显式加 --yes 确认删除，或先用 --dry-run 预览。未删除任何文件。" >&2
    exit 1
  fi
fi

# 有构建正在进行时拒绝删除。
#
# 为什么需要这道检查：`rm -rf` 掉几 GB 要数十秒，而 cargo 会在编译中途创建
# target/debug/deps/rmetaXXXX。目录被抽走后 rustc 报 ENOENT（它建临时目录用
# create_dir 而非 create_dir_all），现场看起来像"target 凭空消失"或"文件系统坏了"，
# 实际是清理与构建交叠。本仓库真实踩过一次：磁盘满 → 删 target 腾空间 →
# 删除还没落地就跑了 build.sh。
#
# 判据用 build.sh 留下的锁，而不是扫进程名：进程名分不清是哪个仓库，而且 cargo 的
# 命令行里并不含仓库路径（cwd 不在 argv 里），按路径扫必然漏检 —— 那正是最该拦住的
# 情形。PID 已不存在则视为上次异常退出留下的陈旧锁，放行。
BUILD_LOCK="$TARGET_DIR/.qs-build-lock"
if [ -e "$BUILD_LOCK" ]; then
  holder="$(cat "$BUILD_LOCK/pid" 2>/dev/null || true)"
  if [ -n "$holder" ] && kill -0 "$holder" 2>/dev/null; then
    echo "检测到构建正在进行（PID ${holder}），拒绝清理。" >&2
    echo "边构建边清理会把 target/debug/deps 抽走，让 cargo 报出迷惑性的 ENOENT。" >&2
    echo "等它结束再重跑本脚本。锁文件：${BUILD_LOCK}" >&2
    exit 1
  fi
fi

# 硬保护：绝不动仓库根、家目录、文件系统根，以及仓库根的任一祖先目录。
guard() {
  local p="$1"
  case "$p" in
    ""|"/"|"$HOME"|"$ROOT") echo "拒绝删除受保护路径: $p" >&2; return 1 ;;
  esac
  case "$ROOT" in
    "$p"/*) echo "拒绝删除仓库根的祖先目录: $p" >&2; return 1 ;;
  esac
  return 0
}

removed=0
for p in "${M_PATH[@]}"; do
  guard "$p" || exit 1
  rm -rf -- "$p"
  removed=$((removed + 1))
done

# 复核：删完再确认一遍，避免出现"报成功但其实还在"。
leftover=0
for p in "${M_PATH[@]}"; do
  if [ -e "$p" ] || [ -L "$p" ]; then
    echo "⚠ 未能删除: $p" >&2
    leftover=$((leftover + 1))
  fi
done

if [ "$leftover" -gt 0 ]; then
  echo "== 清理未完全成功：仍有 ${leftover} 项残留（可能被进程占用或权限不足）==" >&2
  exit 1
fi

echo "== 清理完成：删除 ${removed} 项，释放约 $(human_size "$total_kb") =="
echo "   保留项: src-tauri/icons/（已入库的源图标）、Cargo.lock、package-lock.json 等版本控制文件"
if [ "$WITH_NODE_MODULES" = 1 ]; then
  echo "   提示: node_modules 已删除，下次构建前先跑 scripts/build.sh deps（或 npm install）"
else
  echo "   提示: 想连 node_modules 一起清，加 --all"
fi
