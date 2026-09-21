# Qoder 端点取证（静态）

> 取证方式：**纯静态**。证据来自本机安装的 Qoder CN 桌面客户端
> `/e/Qoder CN/resources/app.asar`（1.3.9 时期，2026-09-17 构建）与 CN CLI
> `~/.qoder-cn/bin/qoderclicn/qoderclicn-1.1.5.exe` 的字符串/代码段检索。
> 取证过程**零网络请求**（边界经用户 2026-09-21 批准：静态 + 只读探测；
> 刷新/写类调用一律禁止实测）。
> 本文是原计划 M0 就要求的 `docs/qoder-endpoints.md`。

## 1. 域名与渠道

| 渠道 | openApiBaseUrl | 证据 |
| --- | --- | --- |
| CN 生产 | `https://openapi.qoder.com.cn` | asar 内 `xl(releaseChannel).openApiBaseUrl` 解析表 |
| 国际生产 | `https://openapi.qoder.sh` | 同上 |
| 测试 | `test-openapi.qoder.com.cn` / `test-openapi.qoder.sh` | 同上 |

其他在案主机：`gateway.qoder.com.cn`（含 test-）、`api.qoder.com`（国际 cloud）、
`static.qoder.com.cn`（客户端/CLI 自更新源，`app-update.yml` 亦证）。
下表路径省略 host 时均指 openApiBaseUrl。

## 2. 端点清单（按用途）

### 2.1 已取证、与我们产品直接相关

| 方法与路径 | 鉴权 | 请求 | 响应/行为 | 证据 | 置信度 |
| --- | --- | --- | --- | --- | --- |
| `POST /api/v1/me/jobToken` | `Authorization: Bearer <主 accessToken>` | JSON `{clientId}` | 签发子系统 jobToken | asar：`new URL("/api/v1/me/jobToken",e.openApiBaseUrl)` | 高 |
| `POST /api/v1/jobToken/refresh` | 未带主 token | JSON `{refresh_token}` | 续期 jobToken；400 有专门分支 | asar 两处（成功/400） | 高 |
| `GET /api/v2/quota/usage?outerProviders=cmcc` | 经 nativeHttpService（全局注入登录态） | query `outerProviders=cmcc` | 含 `usageLogLink`/`purchaseLink` 等字段；诊断日志含 host/path | asar `J5e` 函数 | 高（响应全形待一次只读探测） |
| `GET /api/v2/user/plan` | 同上 | — | 套餐信息（UI plan 字段来源） | asar+CLI 均有 | 中高 |
| `GET /api/v1/userinfo` | 同上 | — | 身份信息 | asar+CLI 均有 | 高 |

### 2.2 已取证、但用途需要修正认知

| 方法与路径 | 实际用途 | 关键证据 |
| --- | --- | --- |
| `GET /api/v1/deviceToken/poll?nonce=&verifier=&challenge_method=S256` | **设备配对轮询**：客户端本地 `generateNonce()` 生成 PKCE 对，轮询至超时；404=未确认→继续；2xx 且 JSON 含 `token`+`refresh_token`（均为字符串）=配对成功。`QODER_AUTH_DEBUG=1` 打开调试日志 | asar `zje` 函数全文 |
| `POST /api/v1/deviceToken/refresh` | 续期上轮拿到的 deviceToken；过期分支会 `expireSession` | asar |

**修正结论（推翻先前假设）**：`deviceToken` 这套**不是账号登录的设备码流程**。
桌面端上下文里它服务"允许 Qoder Mobile 控制此设备"的远程控制配对
（UI 字符串 `allowRemoteControl` / `remoteControl` 一节）；CLI 里同名端点更可能服务
CLI 登录配对（CLI 登录方式枚举：`job_token`/`yunxiao_token`/`ide_login`，另有
`loginWithPAT`）。桌面端**账号登录不走设备码**——中文 UI 检索只有
"Qoder Mobile 下载二维码"，无任何"扫码登录账号"文案。

### 2.3 明确不存在（检索无果）

- **主 accessToken 的刷新端点**：`/api/v1|v2/(auth|token|session)*` 全部无果。
  与实测一致：主 token 约 30 天到期后桌面端走网页重登，不做 API 刷新。
  → 上游"主动刷新 token"功能在 Qoder 侧**永久不适用**，理由从此有实证。
- **桌面端扫码添加账号的设备码起点**：无起点调用（poll 有、issue/start 无）。

## 3. 对成熟化路线的影响

| 上游能力 | M8 判定 | M9 动作 |
| --- | --- | --- |
| 额度/积分查询 | 从"Qoder 没有"升级为"**端点已取证**" | 只读探测补全响应形 → 实现 CreditStatsPage 数据源（需先经用户确认探测） |
| 套餐名 | 端点已取证 | 随额度一起实现 |
| OAuth 设备码添加账号 | 桌面端不适用（无此流程） | 探 CLI 的 deviceToken 配对授权方；若仅服务 CLI 自身，整条标"不适用"并写明 |
| 主动刷新主 token | **不适用**（端点不存在，§2.3） | 无 |
| Token 用量统计 | 本地日志源待查（api.ts 与 App.tsx 口径打架） | 查 Qoder 本地日志是否含用量；无则整页维持占位并统一口径 |
| 自动更新发布源 | **已落地（M10 / v0.1.4）** | tauri-plugin-updater + minisign 验签 + GitHub Releases 自动化发版 |

## 4. 探测纪律（M9 执行时仍有效）

1. 只允许 §2.1 表内只读 GET 的最小探测；探测前备份 `~/.qs-switch/`，探测不改任何远端状态。
2. jobToken/deviceToken 的签发与续期**不发真实请求**（会写远端会话状态）。
3. 探测使用已解出的本机真实凭据时，日志与输出只允许出现长度/键名/哈希前缀。
4. 任何不在此表的 URL 一律禁止猜测调用（红线 §6）。
