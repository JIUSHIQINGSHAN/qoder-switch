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

## 5. 国际版风控签到（v0.2 新增，二手取证）

> 取证方式：**对 [CreditDaddy](https://github.com/techysy/CreditDaddy)（MIT, © techysy）
> 实现的静态取证**（2026-10-25，其 main 提交 `src/qoderClient.js` / `src/qoderApp.js` /
> `src/checkin.js` 等），本项目的 Rust 实现独立重写。**尚未做过真机只读探测**——
> 下表的"置信度"均以此为准；按 §4 纪律，`campaigns` 查询（只读）可在用户批准后
> 最小探测，`claim` 是写类调用，首测必须经用户显式批准。

### 5.1 端点（与国内版同形，仅基址不同）

| 方法与路径 | 鉴权/头 | 请求 | 响应/行为 | 置信度 |
| --- | --- | --- | --- | --- |
| `GET {base}/sash/api/v1/me/campaigns?clientType=10` | 基础头 + 签到叠加头（见 5.2） | — | `{ uid, campaigns: [{ campaignId, campaignKey, actionType, claimStatus, benefit: { amount } }] }`；`actionType == "CLAIM_BENEFIT"` 为积分活动，`CLAIMABLE` 可领 / `CLAIMED` 已领；401/403 = token 失效 | 中（CreditDaddy 生产使用中，未本机实测） |
| `POST {base}/sash/api/v1/me/campaigns/{encodeURIComponent(campaignId)}/claim` | 同上 | 无 body | 成功 `{ benefit: { amount } }`；失败**无错误码约定**，只能按状态码与形状推断（已领/未开放/风控拒绝对外都表现为"没有每日积分活动"） | 中（同上） |
| 基址 `https://openapi.qoder.sh` | — | — | 国内版 `openapi.qoder.com.cn` 同构（§1 已有 asar 证据）；国际版出口按 IP 风控，直连被拦时返回 403 风控页（需要账号级代理） | 中高（基址有 asar 证据；403 行为来自 CreditDaddy CHANGELOG） |

### 5.2 签到叠加头（对齐客户端 `createAuthorizedHeaders`）

| 头 | 取值 | 来源 |
| --- | --- | --- |
| `Cosy-Version` | 客户端真实版本：`<resources>/build-manifest.json` 的 `productVersion`；不可得时回退 `0.3.3` | CreditDaddy `qoderClient.js:145-167`（硬编码打底再覆盖）；**不许写死**——版本随客户端漂移 |
| `Cosy-MachineOS` | 叠加口径是 `架构_平台`（`x86_64_win32` / `aarch64_darwin`…），与基础头的 `windows` 是两套口径 | CreditDaddy `qoderApp.js:188-191`；Windows ARM 取值未实测，按构词法给出 |
| `Cosy-MachineId` | 优先客户端 userData 的 `auth.machine-id`（明文 UUID，已按 `FileRole::DesktopMachineId` 入包）；缺失时按档位持久化 UUIDv4 | CreditDaddy `qoderApp.js:209-226`；两档各自独立设备指纹 |
| `Cosy-MachineHostname` | 可打印 ASCII ≤96，脏值清洗后附短哈希；可选 | CreditDaddy `qoderApp.js:194-207` |
| `Cosy-MachineToken` / `Cosy-MachineCode` / `Cosy-MachineType` | **设备风控三头**：由本机 Qoder 客户端自带组件生成（见 5.3）。国际版必须携带才有"每日 100 Credits"活动；国内版不需要 | CreditDaddy `qoderClient.js:7-8` 注释 + `README`；头名大小写逐字对齐现网实现 |

### 5.3 设备风控凭据的生成（`runtime-info` 组件）

- **组件位置**（CreditDaddy `qoderApp.js:95-159`）：
  - 客户端安装目录：`%LOCALAPPDATA%\Programs\Qoder{, CN}`、`C:\Program Files[ (x86)]\Qoder{, CN}`、
    卸载注册表（`DisplayName -like 'Qoder*'`，**排除 "Qoder IDE"**——IDE 不带组件）、
    MSIX `%LOCALAPPDATA%\Microsoft\WindowsApps\Qoder`、Squirrel 目录；
  - 0.3+ 布局：`<安装目录>\.qoder-versions\<版本>\resources\umid\runtime-info.exe`
    （版本按**完整数值段**降序），顶层 `resources` 是首装残留兜底；
  - 兜底：qodercli 自解压的 `~\.qoder[|-cn]\.bin\umid-win32-<arch>-*\runtime-info.exe`。
- **调用方式**（`qoderApp.js:235-281`）：`runtime-info <env> --account-stdin`，
  stdin 写 `{"account":"<uid>"}\n`（Linux 分支无 stdin 参数）；stdout **第一行** JSON 取
  `machineToken / machineCode / machineType`（字符串，各 ≤4096）；输出 >1MB 判失败；
  超时 25s。`env` 取值：客户端组件 国际=3 / 国内=0；qodercli 解压组件 国际=4 / 国内=0。
- **刷新与缓存**：客户端自身每 60±5 分钟刷新；本项目按 `档位:uid` 进程内缓存
  **50 分钟**（保守取短）、惰性触发、不落盘；失败不缓存、下次重试。
- **降级语义**：凭据缺失/失败时**降级为不带风控头继续请求**并在结果里标注 ——
  服务端的表现是"没有每日积分活动"，不是报错；绝不编造成功。
- **业务约束**：国际版签到**每台设备每日限领一个账号**（服务端按风控身份判定）；
  本项目按列表序领取，首个成功写入名额备忘（业务日按 10:00 UTC+8 翻日，
  `(now_utc - 2h)` 取 UTC 日期），后续账号判 `limited` 不再徒劳请求。
- **Linux 无客户端环境**需要从 `@qoder-ai/qodercli` npm 包提取 UMID 组件
  （CreditDaddy 的 `umid install`，含 integrity 防投毒校验）——本项目不做，
  桌面场景用不到。
