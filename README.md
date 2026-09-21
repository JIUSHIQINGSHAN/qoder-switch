# Qoder Switch

Qoder 家族（桌面客户端 / QoderWork / CLI）的多账号切换桌面 App。Tauri v2 + Rust core + React 19。

思路与分层来自 [changexbc/workbuddy-switch](https://github.com/changexbc/workbuddy-switch)（MIT），
按 Qoder 的实际存储结构重做了凭据层。要做前端比对时自行拉一份只读副本：
`git clone --depth 1 https://github.com/changexbc/workbuddy-switch ../reference/workbuddy-switch`
（副本不入库，本仓库也不依赖它存在）。

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

## 下载与安装

Windows 10+ x64，需 WebView2 运行时（Win11 自带）。两个渠道：

- **[GitHub Releases](https://github.com/JIUSHIQINGSHAN/qoder-switch/releases/latest)**（推荐）：
  - `qoder-switch_<版本>_x64-setup.exe`：NSIS 安装包（带 minisign 签名，应用内更新走它）；
  - `qoder-switch_<版本>-portable-x64.zip`：免安装便携包（含桌面 exe 与 webui 服务端）；
  - 校验和见包内 `SHA256SUMS.txt`；应用会校验更新包签名，公钥在 `src-tauri/tauri.conf.json`。
- **npm（webui 形态）**：`npm install -g qoder-switch` 后按 `qoder-switch` 命令提示启动
  本地服务端，在浏览器里操作；凭据存储与桌面 App 同为 `~/.qs-switch/`，不要同时操作。

应用内更新：设置页「检查更新」→ 签名包下载安装 → 重启生效。更新源固定指向本仓库的
`releases/latest/download/latest.json`（打 `v*` tag 由 CI 自动发版）。

## 构建

本机构建把工具链与产物钉在 E:（C: 盘余量不足，一次 release target 实测吃掉约 7GB）。
产物目录由 `scripts/build.sh` 显式导出的 `CARGO_TARGET_DIR=E:/qs-target` 兜底
（GitHub Actions 的 runner 没有 E: 盘，所以不在 `.cargo/config.toml` 里钉死）：

```bash
bash scripts/build.sh deps      # npm install（registry 走 npmmirror）
bash scripts/build.sh icons     # 由 public/app-icon.png 生成 src-tauri/icons/
bash scripts/build.sh test      # cargo test --workspace
bash scripts/build.sh release   # npx tauri build → release exe + NSIS 安装包
bash scripts/build.sh all       # deps → icons → test → release
```

`RUSTUP_HOME` / `CARGO_HOME` 若不在默认位置，脚本会读环境变量或按 `E:/rustup`、
`E:/cargo` 取值。`E:/cargo/config.toml` 需配 rsproxy.cn 的 sparse index 源替换，
否则拉索引会超时。

产物落在 `E:/qs-target/`（由 `.cargo/config.toml` 钉在 C 盘之外，构建时自动创建，随时可删）：

- `debug/qoder-switch.exe` —— 开发调试
- `release/qoder-switch.exe` —— 免安装单文件
- `release/bundle/nsis/qoder-switch_<版本>_x64-setup.exe` —— Windows 安装包

## 命令行工具（examples）

```bash
cargo run --example qs-probe    --            # 现场探针：进程 + 托管判定
cargo run --example qs-snapshot -- take      # 凭据文件快照（只读）
cargo run --example qs-snapshot -- diff      # 比对最近两张快照
cargo run --example qs-account  -- capture <名字> [cn|global] [desktop|cli|work]
cargo run --example qs-account  -- list
cargo test --workspace                       # 108 项测试全绿（core 76 / server 19 / 桌面宿主 13）
```

## 无头自检

主程序带 `--self-check`：不开窗口，直接把宿主层的 command 逐个跑一遍（绝不调用
`switch_now`，所以不会真换号）。GUI 版本没有控制台，报告同时写进
`~/.qs-switch/selfcheck.log`。

```bash
qoder-switch.exe --self-check && echo OK
```

它覆盖的是 core 单元测试覆盖不到的那半边：command 接线、serde 形状、真实路径解析、
账号库读写。输出逐行 `OK`/`FAIL`，末尾 `SELF-CHECK OK` 且退出码 0 才算通过。

## 凭据编解码（`auth_codec`）

本机实测确认的方案，代码在 `crates/qs-switch-core/src/modules/auth_codec.rs`，
探针脚本在 `scripts/probe-auth-codec.py`（只读，输出全脱敏）：

```
%APPDATA%\<app>\Local State
  → os_crypt.encrypted_key = base64( "DPAPI" + DPAPI(CURRENT_USER) blob )
  → DPAPI 解出 32 字节 AES-256 主密钥

%APPDATA%\<app>\auth.v1.dat
  = b"v10" + 12 字节 IV + AES-256-GCM 密文（末尾 16 字节 tag）
  明文 JSON: { schemaVersion:1, token, refreshToken, expiresAt,
               refreshTokenExpiresAt, user:{id,name,email,phone,avatarUrl} }
```

`token` 只有 27 字符，是不透明串而不是 JWT —— 所以到期时间只能靠 `expiresAt` 字段，
不能从 token 里解。DPAPI 通过 PowerShell 子进程调用（要先
`Add-Type -AssemblyName System.Security`，否则 `ProtectedData` 类型找不到），
这样不必为一次系统调用拖进整个 `windows` crate；主密钥只在内存里以 base64 中转，不落盘。

## 安全模型

五条硬规则，都有对应测试：

1. **破坏性动作 fail-closed。** 终止目标进程前先判定"本会话是否由目标客户端托管"。
   依据一为 Qoder 注入子进程的环境标记（`QODER_PRODUCT_ID`、`QODERCN_CLI`、
   `QODERCN_SESSION_TYPE=app`），依据二为父进程链。链判不出来时按「不许」处理 ——
   实测 MSYS2 的 fork 模拟会让父链在 `timeout.exe` 处断链，"没看到目标"不等于"没被托管"。
   父链探测走 PowerShell `-EncodedCommand`：`-Command` 传多行脚本时内嵌引号会被
   CreateProcess 的参数拼接破坏，静默返回空值，安全门会形同不存在。
2. **写前先落盘可恢复依据。** 切换 journal 与备份清单 `_restore.json` 都先于任何写入落盘；
   进程中途被杀，下次启动 `unfinished()` + `recover()` 能凭盘上依据退回。
3. **写后读回比 sha256，任一不符整组回滚。** 这一步是唯一能发现"写完没生效"的手段。
   别指望客户端自己覆盖回来：实测桌面端在空闲会话期并不重写这几个文件（12 个进程存活
   9 小时，四份凭据文件哈希零变化），所以写完不校验就等于把失败留到用户下次打开客户端。
4. **拒绝半换号。** 现场存在、但账号包里缺位的 critical 文件（如只带 `auth.v1.dat`
   没带 `Local State`）直接拒写，不做部分生效的切换。
5. **服务端请求头防护（防御 CSRF 与浏览器跨域携带）。** `qs-switch-server` 的 `/api/*`
   端点强制校验自定义头 `x-qoder-switch: 1`（浏览器原生 `form` 无法跨站静默设置该自定义头），
   且只允许安全方法（GET/POST/HEAD/OPTIONS），杜绝简单请求 CSRF 与非法参数注入：
   ```bash
   curl -H "x-qoder-switch: 1" http://127.0.0.1:57891/api/status
   ```

账号库存于 `~/.qs-switch/`：`accounts/<名>/<版本>.<目标>/{bundle.json, 凭据副本…}`、
`backups/`、`journal/`、`snapshots/`。副本是**真实凭据的密文文件**， `.gitignore` 已把
`accounts-export/` 与 `.qs-switch/` 挡在库外，不要把包目录提交或同步出去。

## 当前能力

已可用：

- 现场探针：每个 (版本·目标) 的在跑进程、凭据文件存在性、exe 路径、托管判定
- 账号包：认领当前登录态 → 列表 → 逐角色查看 → 删除（手工删目录即可）
- 身份与到期：认领时用 DPAPI + AES-256-GCM 解开 `auth.v1.dat`，取出 `user.id` 与
  `expiresAt` / `refreshTokenExpiresAt`，界面临期高亮并按到期升序排列（解不开时退回
  明文回显，认领本身不会失败）
- 切换：预览（含逐角色「此刻/本次写回」）→ 终止目标 → 整组备份 → 写回 → 读回校验 → 重启
- 崩溃恢复：启动时列出未收尾的 journal，一键退回切换前现场
- 账号包导出 / 导入（JSON + base64，默认不覆盖，哈希不符整体中止）
- 托盘常驻 + 快捷切换：托盘列出所有桌面账号包（带 token 剩余天数），点一下就走完整
  切换流程 —— 托管判定与备份回滚一个都不绕过
- 轮换建议：按各账号包解出的 token 剩余天数判定该切到谁（阈值 / 防抖 / 已是最优 /
  依据不足 四类理由都会讲明）。**刻意不自动执行** —— 原作轮换只是改一个 CLI 指针，
  而这里换号要重启用户正在用的 IDE，必须由人确认
- 凭据快照与差分；关窗只隐藏不退进程
- 浏览器形态（webui）：`qs-switch-server` 只监听 127.0.0.1:57891，托管 `dist/` 并把同一套
  core 能力以 JSON 暴露。两个宿主共用 `core::modules::view` 生成返回体 —— 展示形状放在任一
  宿主里都会逼另一个复制一份，而这两份迟早分叉（分叉过一次：`capabilities` 措辞与轮换阈值
  默认值）。前端把 HTTP body 原样当作 `invoke` 的返回值，所以这里返回**裸契约对象**，
  不套 `{ok,data}` 信封；多包一层会让 `{accounts}` 解成 `undefined`、整页空白，
  而网络面板里全是 200。`router.rs` 里有对应的回归测试。

## 已知边界

- 会话历史不按账号隔离：桌面 `main.sqlite` 的 `chat_sessions` 无 `account_id` 列，
  `~/.qoder*/projects/` 按工作目录命名。换号后两个账号会互见历史，界面上会提示。
- DPAPI 按 Windows 用户生效：账号包只在同一 Windows 用户内可复用，跨机器或跨用户无效。
- 未实现（相对参考实现仍缺）：device flow 扫码添加账号（**桌面端实证不适用**：登录不走
  设备码，见 `docs/qoder-endpoints.md` §2.2；CLI 侧配对流程待探）、PAT 旁路、
  额度/积分用量查询（**端点已取证**，`docs/qoder-endpoints.md` §2.1，待实现）、
  会话跨账号迁移、webui 双形态里的 **npm 发布**（包结构就绪，待 npm 账号后发布）。
- **主动刷新主 token 实证不适用**（不是"尚未取证"）：主 accessToken 没有任何刷新端点，
  桌面端到期即走网页重登（`docs/qoder-endpoints.md` §2.3）。
- **自动更新已配置**（v0.1.4 起）：打 `v*` tag 触发 GitHub Actions 构建并发布
  带签名的安装包 + `latest.json`，桌面端应用内直接升级；npm 分发形态见 `npm/README.md`。
- WorkBuddy 的每日签到在 Qoder 无对应接口：保留版式，动作与读类接口都写明"不适用"。
- **Buddy 旅行已整体删除**（不是标"不适用"）：Qoder 没有这个玩法，界面入口、类型、
  契约路由与演示数据一并去掉 —— 留一个永远点不动的按钮比删掉它更误导人。
- 这些"没有的能力"在前端保留版式并写明不适用（`src/lib/api.ts` 的 `QODER_EMPTY` /
  `QODER_UNAVAILABLE`）：读类命令返回**契约里每个键都齐**的类型正确空值（少一个键就会让
  渲染期对 undefined 调 `.filter()`，无 ErrorBoundary 时整页白屏），动作类命令抛原因。
  直接走 `httpCall` 的那几个按账号查询也会被同一道门拦住，不会对本机服务发真请求。

## FAQ

- **杀软报毒？** 安装包没有做代码签名证书（EV 证书成本原因），NSIS 安装器可能被
  误报。可以在 Release 页核对 `SHA256SUMS.txt`，或从源码自行构建；应用内更新只接受
  minisign 签名（公钥在仓库里），不接受未签名产物。
- **换号后历史会串吗？** Qoder 的会话历史不按账号隔离（`main.sqlite` 无 `account_id`），
  切换后本机历史会话可能在新账号下可见——切换弹窗里有提示。本工具不做静默迁移。
- **能跨机器/跨 Windows 用户恢复账号包吗？** 不能直接用：凭据经 DPAPI(CURRENT_USER)
  加密，跨机器或跨用户解不开。账号包只在同一 Windows 用户内可复用。
- **切换会不会丢数据？** 切换前整组凭据先备份到 `~/.qs-switch/`，写入后校验、失败自动
  回滚；未收尾的切换会留在「待处理」列表里可恢复。
- **CLI / QoderWork 会跟着换号吗？** 桌面端是登录态权威源。CN 版 CLI 不落盘凭据、
  由桌面端注入；QoderWork 是独立文件。换号对它们的实际影响见 `docs/qoder-endpoints.md`
  与 `docs/upstream-parity.md` 的实测记录。

## 许可

MIT。凭据布局与切换流程的设计参考 workbuddy-switch（MIT, © changexbc）。
