# qoder-switch（npm 形态）

Qoder 多账号切换工具的 **webui 分发形态**：`npm install -g qoder-switch` 后获得
本地 HTTP 服务端（`qs-switch-server` / Windows 上 `qs-switch-server.exe`）+ 随包前端
（`webui_dist`），在浏览器里操作账号导入/切换/回滚。桌面 App（托盘、开机自启、
应用内更新）请用
[GitHub Releases](https://github.com/JIUSHIQINGSHAN/qoder-switch/releases) 的安装包，
两者共用同一份凭据存储（`~/.qs-switch/`），不要同时操作。

## 使用

```bash
npm install -g qoder-switch
qoder-switch          # 打印可直接复制的启动命令（默认 127.0.0.1:57891）
```

平台分包模式：二进制在按平台命名的可选依赖包里（optionalDependencies 自动安装），
postinstall 只做文件复制，不联网下载。已发布：

| 平台包 | 二进制 |
| --- | --- |
| `qoder-switch-win32-x64` | `qs-switch-server.exe` |
| `qoder-switch-darwin-arm64` | `qs-switch-server`（Apple 硅） |
| `qoder-switch-darwin-x64` | `qs-switch-server`（Intel） |

**macOS 上首次用会弹一次「钥匙串」授权框** —— 桌面凭据的主密钥在登录钥匙串里，
请选「始终允许」，否则每次刷新状态都会再弹一次。拒绝授权时账号仍能认领，但解不开
登录态，配额/签到/到期时间会退回明文回显。

## 安全边界

- 启动器不透传任何外部输入（不 spawn 子进程），只打印启动命令——本仓库的写入
  安全门无条件拦截 JS 子进程调用。
- postinstall 只做 `fs.copyFileSync`；非 Windows 上补一次 `chmod 0755`，
  否则装完直接 Permission denied。
- 凭据存档在 `~/.qs-switch/`，永不入库、永不外发。

## 发布（维护者）

需要 npm 账号并在 npmjs 创建发布令牌。打包脚本：

```bash
# 一次只能出当前宿主能编译的平台；Windows 造不出 darwin 二进制，反之亦然。
bash scripts/pack-npm.sh                                        # 只打宿主这一个平台
bash scripts/pack-npm.sh aarch64-apple-darwin x86_64-apple-darwin   # 在 mac 上出两个 arch

(cd npm/platform/qoder-switch-win32-x64 && npm publish --access public)
(cd npm/platform/qoder-switch-darwin-arm64 && npm publish --access public)
(cd npm/platform/qoder-switch-darwin-x64 && npm publish --access public)
(cd npm && npm publish --access public)
```

主包的 `optionalDependencies` 里三个平台包版本必须与主版本一致，缺哪个包那个平台
的 `npm install` 就会在 postinstall 阶段静默跳过（不报错，但装不出二进制）。

版本号与仓库主版本保持一致（`package.json` × 2 + `npm/platform/*` × 3 +
主版本三处 `Cargo.toml` + `tauri.conf.json`）；`pack-npm.sh` 会在打包前逐处核对，
任一不符直接终止。
