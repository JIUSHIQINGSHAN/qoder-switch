# qoder-switch（npm 形态）

Qoder 多账号切换工具的 **webui 分发形态**：`npm install -g qoder-switch` 后获得
本地 HTTP 服务端（qs-switch-server.exe）+ 随包前端（webui_dist），在浏览器里操作
账号导入/切换/回滚。桌面 App（托盘、开机自启、应用内更新）请用
[GitHub Releases](https://github.com/JIUSHIQINGSHAN/qoder-switch/releases) 的安装包，
两者共用同一份凭据存储（`~/.qs-switch/`），不要同时操作。

## 使用

```bash
npm install -g qoder-switch
qoder-switch          # 打印可直接复制的启动命令（默认 127.0.0.1:57891）
```

平台分包模式：二进制在 `qoder-switch-win32-x64`（optionalDependencies 自动安装），
postinstall 只做文件复制，不联网下载。当前仅发布 win32-x64。

## 安全边界

- 启动器不透传任何外部输入（不 spawn 子进程），只打印启动命令——本仓库的写入
  安全门无条件拦截 JS 子进程调用。
- postinstall 只做 `fs.copyFileSync`。
- 凭据存档在 `~/.qs-switch/`，永不入库、永不外发。

## 发布（维护者）

需要 npm 账号并在 npmjs 创建发布令牌。打包脚本：

```bash
bash scripts/pack-npm.sh          # 构建 server + dist 并暂存进 npm/ 各包目录
cd npm/platform/qoder-switch-win32-x64 && npm publish
cd ../../ && npm publish
```

版本号与仓库主版本保持一致（package.json × 2 + 主版本三处 Cargo.toml + tauri.conf）。
