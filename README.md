# Qoder Switch

Qoder 家族（桌面客户端 / QoderWork / CLI）的多账号切换桌面 App。Tauri v2 + Rust core + React 19。

思路与分层来自 [changexbc/workbuddy-switch](https://github.com/changexbc/workbuddy-switch)（MIT），
按 Qoder 的实际存储结构重做了凭据层。参考实现以只读方式放在同级 `../reference/workbuddy-switch/`。

## 为什么不是照抄：账号载体换成了「凭据包」

原作把 token 明文存进 `accounts.json`，因为它的目标接受 token 注入。Qoder 不是：

| 位置 | 形态 | 加密 |
| --- | --- | --- |
| `%APPDATA%\com.qodercn.app.stable\auth.v1.dat` | 桌面登录态 | magic `v10`：Electron safeStorage → `Local State` 的 `os_crypt.encrypted_key` → DPAPI(CURRENT_USER) + AES-256-GCM |
| `%APPDATA%\QoderWork CN\auth.dat` / `auth-v2.dat` | QoderWork 登录态 | 同上 |
| `~/.qoder{,-cn}/.auth/user` | CLI 凭据 | WASM `credential_storage_encrypt`，密钥取同目录 `machine_id` 前 16 字符 |

要存 token 就得先复刻这两套加密。本项目改为**存整组凭据文件的字节副本**（bundle），
完全不需要碰密码学细节；账号标签则从桌面端自己写下的明文回显
（`~/.qoder-cn/.qoder-app-status.json` 的 `name` / `email` / `plan`）里读。

另一条关键实测：**CN 版 CLI 不落盘凭据** —— `~/.qoder-cn/.auth/` 只有 `machine_id`，
国际版那份 `.auth/user` 的 mtime 也停在两个月前，而回显文件是当天新写的、`writer` 为 `main`。
所以登录态的权威源是桌面端，CLI 由它注入。切换单元因此绑桌面端四件套，而不是 CLI 目录。

## 构建

本机没有预装 Rust，工具链与构建产物全部钉在 E:（C: 盘余量不足）：

```bash
export RUSTUP_HOME='E:/rustup' CARGO_HOME='E:/cargo' \
       CARGO_TARGET_DIR='E:/qs-target' PATH="/e/cargo/bin:$PATH"

npm install                         # registry 走 npmmirror
npx tauri icon public/app-icon.png  # Windows 资源编译要求 icons/ 真实存在
npm run build                       # tsc --noEmit && vite build → dist/
cargo build -p qoder-switch         # 调试产物 E:/qs-target/debug/qoder-switch.exe
npx tauri build                     # 安装包（nsis）
```

`E:/cargo/config.toml` 已配 rsproxy.cn 的 sparse index 源替换；`RUSTUP_DIST_SERVER`
同样指 rsproxy，否则装工具链会超时。

## 命令行工具（examples）

```bash
cargo run --example qs-probe    --            # 现场探针：进程 + 托管判定
cargo run --example qs-snapshot -- take      # 凭据文件快照（只读）
cargo run --example qs-snapshot -- diff      # 比对最近两张快照
cargo run --example qs-account  -- capture <名字> [cn|global] [desktop|cli|work]
cargo run --example qs-account  -- list
cargo test -p qs-switch-core                  # 33 项
```

## 安全模型

四条硬规则，都有对应测试：

1. **破坏性动作 fail-closed。** 终止目标进程前先判定"本会话是否由目标客户端托管"。
   依据一为 Qoder 注入子进程的环境标记（`QODER_PRODUCT_ID`、`QODERCN_CLI`、
   `QODERCN_SESSION_TYPE=app`），依据二为父进程链。链判不出来时按「不许」处理 ——
   实测 MSYS2 的 fork 模拟会让父链在 `timeout.exe` 处断链，"没看到目标"不等于"没被托管"。
   父链探测走 PowerShell `-EncodedCommand`：`-Command` 传多行脚本时内嵌引号会被
   CreateProcess 的参数拼接破坏，静默返回空值，安全门会形同不存在。
2. **写前先落盘可恢复依据。** 切换 journal 与备份清单 `_restore.json` 都先于任何写入落盘；
   进程中途被杀，下次启动 `unfinished()` + `recover()` 能凭盘上依据退回。
3. **写后读回比 sha256，任一不符整组回滚。** 桌面端在会话期会持续重写 auth 文件，
   这一步是唯一能发现"写完就被覆盖"的手段。
4. **拒绝半换号。** 现场存在、但账号包里缺位的 critical 文件（如只带 `auth.v1.dat`
   没带 `Local State`）直接拒写，不做部分生效的切换。

账号库存于 `~/.qs-switch/`：`accounts/<名>/<版本>.<目标>/{bundle.json, 凭据副本…}`、
`backups/`、`journal/`、`snapshots/`。副本是**真实凭据的密文文件**， `.gitignore` 已把
`accounts-export/` 与 `.qs-switch/` 挡在库外，不要把包目录提交或同步出去。

## 已知边界

- 会话历史不按账号隔离：桌面 `main.sqlite` 的 `chat_sessions` 无 `account_id` 列，
  `~/.qoder*/projects/` 按工作目录命名。换号后两个账号会互见历史，界面上会提示。
- DPAPI 按 Windows 用户生效：账号包只在同一 Windows 用户内可复用，跨机器或跨用户无效。
- 未实现（相对参考实现仍缺）：device flow 扫码添加账号与 PAT 旁路、token 保活与到期提醒、
  额度/积分查询、会话跨账号迁移、自动轮换、托盘菜单里的快捷切换、webui/npm 双形态、自动更新。
- WorkBuddy 的每日签到与 Buddy 旅行在 Qoder 无对应接口，不移植。

## 许可

MIT。凭据布局与切换流程的设计参考 workbuddy-switch（MIT, © changexbc）。
