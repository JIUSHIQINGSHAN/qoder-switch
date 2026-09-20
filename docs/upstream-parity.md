# 上游功能对齐审计（2026-09-21，M8）

对照物：`changexbc/workbuddy-switch`（MIT，只读克隆于 `../reference/workbuddy-switch`，
不入库）。判定依据：前端为上游原样副本，每个能力的生死由 `src/lib/api.ts` 的
`QODER_UNAVAILABLE`/`QODER_EMPTY` 门控 + 后端实际注册命令决定
（桌面端 `src-tauri/src/lib.rs:31-53`，webui `crates/qs-switch-server/src/router.rs:379-395`）。

状态含义：**A** 功能可用 · **B** 保留版式+标注不适用（Qoder 侧确无） ·
**C** 整条删除 · **D** 后端未接管（我们的真缺口，与 Qoder 无关）。

## 三列表

| 能力 | 上游形态 | 状态 | 证据 |
| --- | --- | --- | --- |
| 主切换（备份/回滚/重启） | 账号卡设为当前，重启客户端 | A | api.ts:437-445；lib.rs:37；router.rs:446-465 |
| 账号列表/状态/删除 | get_status / get_accounts / delete_account | A | api.ts:308-315,397-399 |
| 导入本机账号（认领登录态） | import_local + 空状态引导 | A | api.ts:411-413；router.rs:413-424 |
| 账号包导出/导入/预览 | JSON+base64 | A | api.ts:415-435 |
| Token 到期展示/排序 | 解 expiresAt，临期高亮 | A | auth_codec；accounts 视图 |
| 自动轮换（建议+手动+日志） | 按剩余天数轮换 | A（刻意不自动切 IDE） | api.ts:644-677；SettingsPage.tsx:403-494 |
| 切换进度 | switch_progress | A | api.ts:448-450 |
| 每日签到 | CodeBuddy 签到接口 | B（Qoder 无签到接口） | api.ts:192-195,266-273；README |
| 额度/积分用量 | 官方额度接口 | **B→已取证待升级**：`/api/v2/quota/usage` 已取证（见 qoder-endpoints.md §2.1） | api.ts:231-241 |
| Token 用量统计（整页 1713 行） | 扫本地日志聚合 | **D**：后端恒返回占位；api.ts 说"未实现"与 App.tsx:1699"没有数据源"口径打架 | api.ts:242 |
| 会话列表/跨账号复制 | list/copy/preview | B（会话不按账号归属；弹窗已加互见提示） | api.ts:199-200,243,246-253 |
| 模型限额台账+hook | 写 CLI/IDE 配置 | B（hook 注入点 CodeBuddy 专属） | api.ts:201-203,254,257-265 |
| CLI 接入/切换 | 写 settings.json+helper | B（CN CLI 不落盘凭据） | api.ts:205-206,274-284 |
| CN IDE 独立切换 | 独立命令 | B（=主切换） | api.ts:207-209 |
| 国际版 IDE 独立切换 | 同上 | B（尚无独立状态命令） | api.ts:210-212 |
| OAuth 设备码登录 | oauth_start/status+对话框 | **不适用修正**：桌面端登录不走设备码（端点取证 qoder-endpoints.md §2.2）；CLI 配对流程待探 | api.ts:196-197 |
| 主动刷新主 token | refresh_account_token | **不适用（实证）**：主 token 无刷新端点（qoder-endpoints.md §2.3） | api.ts:198 |
| 自动更新 | github config+check+install+relaunch | **D**：发布源未配置（M10） | api.ts:204,213-214,218 |
| 开机自启 | set/get_launch_at_login | **D**：UI 开关在，后端"未接管"（M9 首项） | api.ts:219,272 |
| 通知存档落盘 | record/list/clear | **D（轻）**：命令在但恒空占位；README 未列边界，易误读 | compat.rs:274-289；view.rs:377-389 |
| macOS 权限自检/Finder | check_auth_permission 等 | B（Windows 无此限制） | api.ts:215-217 |
| Buddy 旅行（整条玩法） | 成长中心+旅行 chip+轮询 | C 整条删除 | README:153；本项目零残留 |
| VS Code 扩展切换（含会话复制） | 403 行对话框+类型+mark | C 整条删除（Qoder 无 VS Code 扩展） | 上游独有文件，本项目不存在 |

## 前端偏离排查结论

逐文件 diff（忽略 CRLF）后**无任何无法解释的改动**。全部差异落在四类：
品牌替换、C 类删除、B 类诚实文案、4 处有注释背书的刻意行为调整
（① 首启不静默 importLocal；② 签到配置/日志改串行取；③ `generatedAt===0`
守卫；④ Token 统计空态文案如实化）。另有一处刻意增补：切换弹窗的
"会话历史不按账号隔离"提示（2026-09-21 加）。

## 口径修正待办（M9 前落）

1. README "未实现"桶里的"额度/积分用量"改为"端点已取证，M9 实现"（消除与 api.ts 的口径打架）。
2. README 已知边界补充"通知存档未落盘"一条。
3. Token 统计：查明 Qoder 本地日志有无用量数据后，统一 api.ts 与 App.tsx 的措辞。
