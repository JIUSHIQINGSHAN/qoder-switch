import { invoke } from "@tauri-apps/api/core";
import type {
  AccountMeta,
  AccountRecord,
  AppNotification,
  AppStatus,
  AutoRotateConfig,
  CodeBuddyCliInstallResult,
  CodeBuddyCliStatus,
  CodeBuddyCliSwitchResult,
  CodeBuddyCnIdeStatus,
  CodeBuddyCnIdeSwitchResult,
  CheckinConfig,
  CheckinLog,
  CheckinResult,
  CreditExpiry,
  CreditStatistics,
  TokenStatistics,
  GithubConfig,
  ImportPreviewAccount,
  ImportResult,
  OAuthPollResult,
  OAuthStartResult,
  RateLimitConfig,
  RateLimitHookStatus,
  RateLimitsPayload,
  RotateLog,
  RotateStatus,
  Session,
  SessionCopyReport,
  SessionLinksPreview,
  SessionSyncSelection,
  SwitchJournal,
  SwitchResult,
  UpdateInfo,
  WbVariant,
} from "./types";
import { DEMO_UNAVAILABLE_MESSAGE, demoModeEnabled } from "./demo-mode";
import { screenshotDemoResponse } from "./screenshot-demo";

/**
 * 双通道适配层：
 * - 桌面 App（Tauri）：`invoke` 调用 Rust commands
 * - webui（浏览器）：HTTP fetch 调用本地 qs-switch-server 服务（127.0.0.1）
 */
// 本项目的 HTTP 宿主（qs-switch-server）默认 57891；刻意与参考实现的 57890 错开，
// 因为本机可能同时装着 workbuddy-switch 的 webui。
const API_BASE =
  (typeof import.meta !== "undefined" && (import.meta as any).env?.VITE_API_BASE) ||
  "http://127.0.0.1:57891";

const DEMO_READ_COMMANDS = new Set([
  "get_status", "get_accounts", "get_codebuddy_cli_status", "get_codebuddy_cn_ide_status", "get_codebuddy_ide_status", "get_checkin_status",
  "get_credit_expiry", "get_credit_statistics", "get_auto_checkin_config",
  "get_token_statistics",
  "get_checkin_logs", "get_auto_rotate_config", "rotate_status", "get_rotate_logs",
  "get_github_config", "check_update", "get_launch_at_login_enabled", "switch_progress",
  "get_rate_limits",
  "get_rate_limit_hook_status", "get_rate_limit_config",
]);

export function isDemoMode(): boolean {
  return demoModeEnabled;
}

export function isWebui(): boolean {
  return typeof window !== "undefined" && !("__TAURI_INTERNALS__" in window);
}

/** Tauri mobile 也注入内部 API；用现有平台 UA 约定把桌面宿主与移动宿主区分开。 */
function isMobilePlatform(): boolean {
  if (typeof navigator === "undefined") return false;
  const ua = navigator.userAgent;
  return (
    /Android|iPhone|iPad|iPod/i.test(ua) ||
    (ua.includes("Macintosh") && navigator.maxTouchPoints > 1)
  );
}

/** 是否为提供桌面专属能力的 Tauri 宿主。 */
export function isDesktop(): boolean {
  return !isWebui() && !isMobilePlatform();
}

/**
 * 是否 macOS 宿主。上游版式里「完全磁盘访问 / App 管理 / 在 Finder 中显示」是
 * macOS 专属操作；Windows 上这些按钮点下去只会拿到"不适用"，所以在 Windows 隐藏它们，
 * 只保留 Windows 也真实有效的「认证目录写探针」。
 */
export function isMacHost(): boolean {
  if (typeof navigator === "undefined") return false;
  return navigator.userAgent.includes("Macintosh");
}

type Route = { method: "GET" | "POST"; path: string };

/** Tauri command → HTTP 路由映射（webui 模式）。 */
const ROUTES: Record<string, Route> = {
  get_status: { method: "GET", path: "/api/status" },
  get_accounts: { method: "GET", path: "/api/accounts" },
  get_codebuddy_cli_status: { method: "GET", path: "/api/codebuddy-cli/status" },
  install_codebuddy_cli_helper: { method: "POST", path: "/api/codebuddy-cli/install-helper" },
  switch_codebuddy_cli_account: { method: "POST", path: "/api/codebuddy-cli/switch" },
  get_codebuddy_cn_ide_status: { method: "GET", path: "/api/codebuddy-cn-ide/status" },
  switch_codebuddy_cn_ide_account: { method: "POST", path: "/api/codebuddy-cn-ide/switch" },
  detect_codebuddy_cn_ide_account: { method: "POST", path: "/api/codebuddy-cn-ide/detect" },
  get_codebuddy_ide_status: { method: "GET", path: "/api/codebuddy-ide/status" },
  switch_codebuddy_ide_account: { method: "POST", path: "/api/codebuddy-ide/switch" },
  detect_codebuddy_ide_account: { method: "POST", path: "/api/codebuddy-ide/detect" },
  delete_account: { method: "POST", path: "/api/delete" },
  set_account_proxy: { method: "POST", path: "/api/set-proxy" },
  oauth_start: { method: "POST", path: "/api/oauth/start" },
  oauth_status: { method: "POST", path: "/api/oauth/status" },
  import_local: { method: "POST", path: "/api/import-local" },
  export_accounts: { method: "POST", path: "/api/export-accounts" },
  export_accounts_to_path: { method: "POST", path: "/api/export-accounts-to-path" },
  preview_import_accounts: { method: "POST", path: "/api/import/preview" },
  import_accounts: { method: "POST", path: "/api/import" },
  switch_account: { method: "POST", path: "/api/switch" },
  list_sessions: { method: "GET", path: "/api/sessions" },
  copy_sessions: { method: "POST", path: "/api/sessions/copy" },
  session_links_preview: { method: "POST", path: "/api/session-links/preview" },
  get_checkin_status: { method: "GET", path: "/api/checkin/status" },
  get_credit_expiry: { method: "POST", path: "/api/credits" },
  get_credit_statistics: { method: "GET", path: "/api/credits/stats" },
  get_token_statistics: { method: "GET", path: "/api/token-stats" },
  get_rate_limits: { method: "GET", path: "/api/rate-limits" },
  get_rate_limit_hook_status: { method: "GET", path: "/api/rate-limits/hook-status" },
  install_rate_limit_hook: { method: "POST", path: "/api/rate-limits/install-hook" },
  uninstall_rate_limit_hook: { method: "POST", path: "/api/rate-limits/uninstall-hook" },
  get_rate_limit_config: { method: "GET", path: "/api/rate-limits/config" },
  save_rate_limit_config: { method: "POST", path: "/api/rate-limits/config" },
  checkin: { method: "POST", path: "/api/checkin" },
  checkin_all: { method: "POST", path: "/api/checkin/all" },
  get_auto_checkin_config: { method: "GET", path: "/api/checkin/config" },
  save_auto_checkin_config: { method: "POST", path: "/api/checkin/config" },
  get_checkin_logs: { method: "GET", path: "/api/checkin/logs" },
  check_auth_permission: { method: "POST", path: "/api/check-auth-permission" },
  open_permission_settings: { method: "POST", path: "/api/open-permission-settings" },
  reveal_app_in_finder: { method: "POST", path: "/api/reveal-app-in-finder" },  list_notifications: { method: "GET", path: "/api/notifications" },
  record_notification: { method: "POST", path: "/api/notifications/record" },
  clear_notifications: { method: "POST", path: "/api/notifications/clear" },
  get_auto_rotate_config: { method: "GET", path: "/api/rotate/config" },
  save_auto_rotate_config: { method: "POST", path: "/api/rotate/config" },
  rotate_status: { method: "GET", path: "/api/rotate/status" },
  run_rotate: { method: "POST", path: "/api/rotate/run" },
  get_rotate_logs: { method: "GET", path: "/api/rotate/logs" },
  refresh_account_token: { method: "POST", path: "/api/refresh-token" },
  get_github_config: { method: "GET", path: "/api/update/config" },
  save_github_config: { method: "POST", path: "/api/update/config" },
  check_update: { method: "GET", path: "/api/update/check" },
  switch_progress: { method: "GET", path: "/api/switch/progress" },
};

/**
 * 档位参数只在国际版时下发：缺省（国内版）保持改造前的请求体逐字一致，
 * Tauri 走 `invoke(cmd, undefined)`，HTTP 走无 query 的路径。
 */
function variantArgs(variant?: WbVariant): Record<string, unknown> | undefined {
  return variant === "ai" ? { variant } : undefined;
}

function queryString(args?: Record<string, unknown>): string {
  if (!args) return "";
  const params = new URLSearchParams();
  for (const [key, value] of Object.entries(args)) {
    if (value === undefined || value === null) continue;
    params.set(key, String(value));
  }
  const text = params.toString();
  return text ? `?${text}` : "";
}

async function httpCall<T>(cmd: string, args?: Record<string, unknown>): Promise<T> {
  // 有些调用点（签到/会话预览的按账号查询）绕过 call() 直接打 httpCall，
  // 空值门控必须也在这里生效，否则浏览器里会对不存在的端点发真请求、控制台刷 404。
  const empty = QODER_EMPTY[cmd];
  if (empty) return empty() as T;
  const route = ROUTES[cmd];
  if (!route) throw new Error(`webui 模式暂不支持该操作: ${cmd}`);
  let res: Response;
  try {
    const url =
      route.method === "GET"
        ? `${API_BASE}${route.path}${queryString(args)}`
        : `${API_BASE}${route.path}`;
    res = await fetch(url, {
      method: route.method,
      headers: {
        "Content-Type": "application/json",
        // 与服务端的头门配套：跨站表单/img 发不出自定义头，fetch 带自定义头会触发
        // 预检而服务端从不回 ACAO —— 这条头就是 webui 的 CSRF 防线。
        "x-qoder-switch": "1",
      },
      body: route.method === "POST" ? JSON.stringify(args ?? {}) : undefined,
    });
  } catch (err) {
    const detail = err instanceof Error ? `: ${err.message}` : "";
    throw new Error(`无法连接 qoder-switch 服务（${API_BASE}）${detail}，请先运行 \`qs-switch-server\``);
  }
  const data = await res.json().catch(() => ({}));
  if (!res.ok) {
    throw new Error(data.message || data.error || `请求失败 (${res.status})`);
  }
  return data as T;
}

/**
 * Qoder 侧不存在的能力。保留参考实现的版式与按钮，但点下去要说清楚为什么没反应，
 * 而不是假装成功或静默失败 —— 宁可不给数据，也不给假数据。
 * 依据见 README 的「当前能力 / 已知边界」。
 */
const QODER_UNAVAILABLE: Record<string, string> = {
  refresh_account_token: "刷新接口未取证",
  copy_sessions: "Qoder 会话不按账号归属，跨账号复制会串数据",
  session_links_preview: "Qoder 会话不按账号归属，跨账号复制会串数据",
  install_rate_limit_hook: "限速钩子未实现",
  uninstall_rate_limit_hook: "限速钩子未实现",
  save_rate_limit_config: "限速钩子未实现",
  install_codebuddy_cli_helper: "Qoder CLI 不落盘凭据，无独立账号指针",
  switch_codebuddy_cli_account: "Qoder CLI 不落盘凭据，改桌面端即随之生效",
  get_codebuddy_cn_ide_status: "对应 Qoder 桌面端，请用主切换按钮",
  switch_codebuddy_cn_ide_account: "对应 Qoder 桌面端，请用主切换按钮",
  detect_codebuddy_cn_ide_account: "对应 Qoder 桌面端，请用主切换按钮",
  get_codebuddy_ide_status: "国际版桌面端尚无独立状态命令",
  switch_codebuddy_ide_account: "国际版桌面端尚无独立状态命令",
  detect_codebuddy_ide_account: "国际版桌面端尚无独立状态命令",
  // open_permission_settings / reveal_app_in_finder 以前挂在这里（当时只有 Windows）。
  // 现在两个宿主命令都已实现并按平台分流，挂在这里会让 macOS 上那几个已经放出来
  // 的按钮必然抛错，所以移除；平台差异由 isMacHost() 那侧的显隐负责。
};

/**
 * 仅桌面端可用的命令（webui 宿主拦截）。更新检查/配置/重启与开机自启都绑定
 * 桌面 App 本体：浏览器 webui 不代装桌面更新包，也不接管宿主进程。
 */
const DESKTOP_ONLY_REASONS: Record<string, string> = {
  check_update: "更新检查与安装在 webui 不可用，请到 GitHub Release 页手动下载",
  get_github_config: "更新源配置仅桌面端需要",
  save_github_config: "更新源配置仅桌面端需要",
  relaunch_app: "webui 宿主请直接重启 qs-switch-server 进程",
  get_launch_at_login_enabled: "开机自启仅桌面端有意义",
  set_launch_at_login_enabled: "开机自启仅桌面端有意义",
};

/**
 * 只读命令返回类型正确的空值。
 *
 * 这些面板对应的能力 Qoder 根本没有，也不打算补；但参考前端的渲染路径会直接对
 * `creditMap[id].resources` 之类的字段调 `.filter()` —— 抛异常会让调用方 catch 后
 * 留下 undefined，渲染期抛 TypeError，React 直接把整棵树拆成白屏（实测就是这样）。
 * 所以读类命令给空集合，动作类命令才抛"不适用"。
 */
const QODER_EMPTY: Record<string, () => unknown> = {
  get_token_statistics: () => ({
    generatedAt: 0,
    sources: [],
    error: "Qoder 侧没有 Token 用量统计的数据源（本地日志无 token 键，实测 2026-09-21）",
  }),
  list_sessions: () => ({ sessions: [], current: null }),
  // 契约自带 supported / "unsupported" 状态位：这就是"不支持"的正规表达，
  // 既不会让渲染期拿到 undefined，也不必编造任何数据。
  session_links_preview: () => ({
    supported: false,
    storeStatus: "unsupported",
    storeError: "Qoder 会话不按账号归属",
    sourceUid: "",
    targetUid: "",
    groups: [],
  }),
  get_rate_limits: () => ({ scannedAt: 0, windowDays: 0, accounts: [] }),
  // 空值要按契约把**每个**键给齐：设置页的限额卡片直接对 `status.targets` 调 `.filter()`，
  // 少给一个键就是整页白屏（无 ErrorBoundary，React 会把树拆掉）。
  get_rate_limit_hook_status: () => ({
    scriptPath: "",
    scriptExists: false,
    eventsPath: "",
    installed: false,
    lastEventAt: null,
    targets: [],
  }),
  get_rate_limit_config: () => ({ enabled: false, hookOptOut: false, scanIdeLogs: false }),
  // 开机自启的读写已由后端接管（tauri-plugin-autostart），不再在此占位。
  // webui 宿主不渲染这张卡片，也不会发起同名调用。
  // （get_checkin_status 的空值占位已并入上方 list_sessions 处：webui 分支绕过 call()
  // 直打 httpCall，两处必须给同一个形状，分开写早晚会漂移。）
  get_codebuddy_cli_status: () => ({
    configured: false,
    settingsPresent: false,
    helperPresent: false,
    helperSupportsAccountIds: false,
    activeIndex: null,
    activeAccountId: null,
    activeAccountName: null,
    accountCount: 0,
    statePath: "",
  }),
};

/**
 * 破坏性命令：会删凭据、覆盖登录态或改动现场。webui 的 HTTP 后端（router.rs 的
 * `DESTRUCTIVE`）要求每个都带知情标记 `confirm:<cmd>`（POST 走 body，因为
 * `httpCall` 对 POST 不拼 query）。集中在这里注入，避免逐个调用点漏配 ——
 * 历史上 `switch_account` 单独带过，`delete_account` 就漏了，等于没有门。
 *
 * 桌面端（Tauri）会忽略这个多余键，带上无害。
 */
const CONFIRM_COMMANDS = new Set([
  "switch_account",
  "delete_account",
  "import_local",
  "import_accounts",
  "checkin",
  "checkin_all",
  "run_rotate",
  "clear_notifications",
]);

async function call<T>(cmd: string, args?: Record<string, unknown>): Promise<T> {
  // demo 是编译期构建形态，必须先于空值/不适用短路判定：否则 screenshot-demo
  // 的整套演示 fixture 会被 QODER_EMPTY/QODER_UNAVAILABLE 架在前面而不可达，
  // 演示页每张卡都渲染成"Qoder 无额度接口"的错误态。真实模式零变化。
  if (demoModeEnabled) {
    if (cmd === "get_credit_statistics" && args?.refresh === true) {
      throw new Error(DEMO_UNAVAILABLE_MESSAGE);
    }
    // 演示模式下允许"保存代理"这个动作：截图流程要能展示代理配置交互，
    // 而它不属于只读命令。返回当前账号的回显即可（demo fixture 已带 proxy 键）。
    if (cmd === "set_account_proxy") {
      const demo = screenshotDemoResponse("get_accounts", undefined) as {
        accounts?: AccountMeta[];
      };
      const found = demo.accounts?.find((a) => a.id === args?.accountId);
      return (found ?? null) as T;
    }
    if (!DEMO_READ_COMMANDS.has(cmd)) throw new Error(DEMO_UNAVAILABLE_MESSAGE);
    return screenshotDemoResponse(cmd, args) as T;
  }
  const desktopOnly = DESKTOP_ONLY_REASONS[cmd];
  if (desktopOnly && isWebui()) {
    throw new Error(`此项仅在桌面端可用：${desktopOnly}`);
  }
  const empty = QODER_EMPTY[cmd];
  if (empty) return empty() as T;
  const why = QODER_UNAVAILABLE[cmd];
  if (why) throw new Error(`此项在 Qoder 侧不适用：${why}`);
  // 破坏性命令的知情门：webui 后端按 `confirm` 校验；命令名去掉 `_account`/`_accounts`
  // 之类的后缀差异，统一用后端认的短名。
  const withConfirm =
    CONFIRM_COMMANDS.has(cmd) && isWebui()
      ? { ...(args ?? {}), confirm: confirmToken(cmd) }
      : args;
  if (!isWebui()) return invoke<T>(cmd, withConfirm);
  return httpCall<T>(cmd, withConfirm);
}

/** 命令名 → 后端 `DESTRUCTIVE` 表里的短名（`confirm` 的取值）。 */
function confirmToken(cmd: string): string {
  switch (cmd) {
    case "switch_account":
      return "switch";
    case "delete_account":
      return "delete";
    case "import_local":
      return "import-local";
    case "import_accounts":
      return "import";
    case "checkin":
      return "checkin";
    case "checkin_all":
      return "checkin/all";
    case "run_rotate":
      return "rotate/run";
    case "clear_notifications":
      return "notifications/clear";
    default:
      return cmd;
  }
}

// ---------------------------------------------------------------------------
// 状态 / 账号
// ---------------------------------------------------------------------------

/** 运行状态 / 当前账号 / 应用路径；`variant` 缺省为国内版。 */
export function getStatus(variant?: WbVariant): Promise<AppStatus> {
  return call("get_status", variantArgs(variant));
}

/** 返回全部档位的账号，由调用方按 `variant` 过滤。 */
export function getAccounts(): Promise<{ accounts: AccountMeta[] }> {
  return call("get_accounts");
}

export function getCodebuddyCliStatus(): Promise<CodeBuddyCliStatus> {
  return call("get_codebuddy_cli_status");
}

export function installCodebuddyCliHelper(): Promise<CodeBuddyCliInstallResult> {
  return call("install_codebuddy_cli_helper");
}

/**
 * 切换 Qoder CLI 默认账号。
 *
 * @param closeRunningCli 已废弃：后端一律先关闭正在运行的 CLI 再写状态，该入参被忽略。
 *   仅为兼容既有调用方保留（HTTP 路径仍会原样发送）。
 */
export function switchCodebuddyCliAccount(
  accountId: string,
  closeRunningCli = false,
): Promise<CodeBuddyCliSwitchResult> {
  if (demoModeEnabled) {
    return new Promise((resolve, reject) => {
      window.setTimeout(() => {
        try {
          resolve(
            screenshotDemoResponse("switch_codebuddy_cli_account", {
              accountId,
              closeRunningCli,
            }) as CodeBuddyCliSwitchResult,
          );
        } catch (error) {
          reject(error);
        }
      }, 1200);
    });
  }
  return call("switch_codebuddy_cli_account", { accountId, closeRunningCli });
}

export function getCodebuddyCnIdeStatus(): Promise<CodeBuddyCnIdeStatus> {
  return call("get_codebuddy_cn_ide_status");
}

export function switchCodebuddyCnIdeAccount(
  accountId: string,
  restart = true,
): Promise<CodeBuddyCnIdeSwitchResult> {
  return call("switch_codebuddy_cn_ide_account", { accountId, restart });
}

export function detectCodebuddyCnIdeAccount(): Promise<{
  ok: boolean;
  found: boolean;
  matched?: boolean;
  accountId?: string;
  message?: string;
}> {
  return call("detect_codebuddy_cn_ide_account");
}

export function getCodebuddyIdeStatus(): Promise<CodeBuddyCnIdeStatus> {
  return call("get_codebuddy_ide_status");
}

export function switchCodebuddyIdeAccount(
  accountId: string,
  restart = true,
): Promise<CodeBuddyCnIdeSwitchResult> {
  return call("switch_codebuddy_ide_account", { accountId, restart });
}

export function detectCodebuddyIdeAccount(): Promise<{
  ok: boolean;
  found: boolean;
  matched?: boolean;
  accountId?: string;
  message?: string;
}> {
  return call("detect_codebuddy_ide_account");
}


export function deleteAccount(accountId: string): Promise<{ ok: boolean }> {
  return call("delete_account", { accountId });
}

export function setAccountProxy(
  accountId: string,
  proxy: string | null,
  variant?: WbVariant,
): Promise<{ ok: boolean; account: AccountMeta }> {
  // 必须下发账号自身档位：后端按 (accountId, variant) 定位账号包，缺省按国内版
  // 处理会把国际版账号的代理写到国内版那份包上（或报"账号不存在"）。
  return call("set_account_proxy", { accountId, proxy, ...variantArgs(variant) });
}

/** 发起登录：国内版为扫码授权，国际版为浏览器 Web 登录授权；`variant` 缺省为国内版（档位由后端记忆，轮询无需再传）。 */
export function oauthStart(variant?: WbVariant): Promise<OAuthStartResult> {
  return call("oauth_start", variantArgs(variant));
}

export function oauthStatus(loginId: string): Promise<OAuthPollResult> {
  return call("oauth_status", { loginId });
}

/** 导入本机当前登录态；`variant` 缺省为国内版（对应各自的登录态文件）。 */
export function importLocal(variant?: WbVariant): Promise<{ ok: boolean; account: AccountMeta }> {
  return call("import_local", variantArgs(variant));
}

export function exportAccounts(
  accountIds: string[],
): Promise<{ ok: boolean; accounts: AccountRecord[]; warnings?: string[] }> {
  return call("export_accounts", { accountIds });
}

/** 桌面端：把完整记录写入用户选择的路径（系统保存对话框产物）。 */
export function exportAccountsToPath(
  accountIds: string[],
  path: string,
): Promise<{ ok: boolean; path: string; exported: number; warnings?: string[] }> {
  return call("export_accounts_to_path", { accountIds, path });
}

export function previewImportAccounts(
  fileText: string,
): Promise<{ accounts: ImportPreviewAccount[]; total: number }> {
  return call("preview_import_accounts", { fileText });
}

export function importAccounts(fileText: string, indexes: number[]): Promise<ImportResult> {
  return call("import_accounts", { fileText, indexes });
}

export function switchAccount(args: {
  accountId: string;
  restart?: boolean;
  forced?: boolean;
  /** 账号自身档位；Global 账号必须下发，否则后端按国内版处理（切错档位文件）。 */
  variant?: WbVariant;
  target?: "desktop" | "cli" | "work";
  shareSessions?: boolean;
  copySessionIds?: string[];
  syncSelections?: SessionSyncSelection[];
}): Promise<SwitchResult> {
  return call("switch_account", {
    ...args,
    // webui 的知情门由 call() 按 CONFIRM_COMMANDS 统一注入（`confirm:"switch"`），
    // 不再在这里手写 —— 逐个写就是 delete 漏掉那次的成因。
  } as unknown as Record<string, unknown>);
}

/** 切换进度（webui 轮询用；桌面端走事件，此函数无副作用）。 */
export function switchProgress(): Promise<{ running: boolean; progress: string | null }> {
  return call("switch_progress");
}

/** 当前登录态的会话列表；`variant` 缺省为国内版。 */
export function listSessions(variant?: WbVariant): Promise<{
  sessions: Session[];
  current: string | null;
}> {
  return call("list_sessions", variantArgs(variant));
}

/** 把勾选会话复制到指定账号；返回 core 同形的复制报告（copied / alreadyLinked / errors）。 */
export function copySessions(
  targetAccountId: string,
  sessionIds: string[],
): Promise<SessionCopyReport & { variant?: WbVariant }> {
  return call("copy_sessions", { targetAccountId, sessionIds });
}

/**
 * 预览「当前账号 → 目标账号」可同步的关联会话（只读）。
 *
 * 默认勾选与可选模式都来自后端：前端只按 `defaultChecked` / `availableModes` 渲染，
 * 不自行扩大权限。`variant` 缺省由后端取目标账号自身档位。
 */
export function sessionLinksPreview(
  targetAccountId: string,
  variant?: WbVariant,
): Promise<SessionLinksPreview> {
  const args: Record<string, unknown> = { targetAccountId };
  if (variant === "ai") args.variant = variant;
  return call("session_links_preview", args);
}

/** 打开系统设置授权面板（桌面端专用；webui 模式由服务进程权限决定，无操作）。 */
export function openPermissionSettings(
  target?: "app_management" | "all_files",
): Promise<void> {
  if (demoModeEnabled) return Promise.reject(new Error(DEMO_UNAVAILABLE_MESSAGE));
  if (isWebui()) return Promise.resolve();
  return call("open_permission_settings", { target: target ?? "app_management" });
}

/** 权限自检：桌面端写探针（按档位写在对应登录态文件旁）；webui 模式由服务进程权限决定。 */
export function checkAuthPermission(variant?: WbVariant): Promise<{
  ok: boolean;
  message?: string;
  error?: string;
  dir?: string;
  hint?: string;
}> {
  if (demoModeEnabled) return Promise.reject(new Error(DEMO_UNAVAILABLE_MESSAGE));
  if (isWebui()) {
    return Promise.resolve({
      ok: true,
      message: "webui 模式由服务进程（终端启动）的权限决定，无需额外授权",
      hint: "",
    });
  }
  return call("check_auth_permission", variantArgs(variant));
}

/** 在 Finder 中显示当前 App（桌面端专用；webui 无操作）。 */
export function revealAppInFinder(): Promise<void> {
  if (demoModeEnabled) return Promise.reject(new Error(DEMO_UNAVAILABLE_MESSAGE));
  if (isWebui()) return Promise.resolve();
  return call("reveal_app_in_finder");
}

// ---------------------------------------------------------------------------
// 阶段 3：签到 + token 刷新
// ---------------------------------------------------------------------------

export async function getCheckinStatus(accountId: string): Promise<{
  ok: boolean;
  todayCheckedIn: boolean;
  error?: string;
  raw?: unknown;
  /** 该行所属档位（档位取账号自身）；缺省按国内版处理。 */
  variant?: WbVariant;
}> {
  if (demoModeEnabled) {
    return screenshotDemoResponse("get_checkin_status", { accountId }) as {
      ok: boolean;
      todayCheckedIn: boolean;
      error?: string;
      raw?: unknown;
    };
  }
  if (isWebui()) {
    // webui 端为批量接口，按 accountId 过滤
    const all = await httpCall<{
      accounts: {
        accountId: string;
        email: string;
        ok: boolean;
        todayCheckedIn: boolean;
        error?: string;
        raw?: unknown;
        variant?: WbVariant;
      }[];
    }>("get_checkin_status");
    // 批量形状缺 `accounts` 时不能直接 .find —— 会 TypeError，而调用方（账号页的
    // 今日签到 chip 拉取）把它包在 try/catch 里，异常会被静默吞掉，chip 永久消失。
    const one = (all?.accounts ?? []).find((a) => a.accountId === accountId);
    return one
      ? {
          ok: one.ok,
          todayCheckedIn: one.todayCheckedIn,
          error: one.error,
          raw: one.raw,
          variant: one.variant,
        }
      : { ok: false, todayCheckedIn: false, error: "未找到账号" };
  }
  return call("get_checkin_status", { accountId });
}

export function getCreditExpiry(accountId: string): Promise<CreditExpiry> {
  return call("get_credit_expiry", { accountId });
}

export function getCreditStatistics(refresh = false): Promise<CreditStatistics> {
  return call("get_credit_statistics", refresh ? { refresh: true } : undefined);
}

export function getTokenStatistics(days?: number): Promise<TokenStatistics> { return call("get_token_statistics", days ? { days } : undefined); }

/**
 * 模型限额台账：一次返回**全部账号**当前受限的模型与官方恢复时刻。
 *
 * 不传档位：扫描本身就是全局的（两档位各扫一遍）。后端把 hook 信号与日志扫描
 * 合并后返回，日志扫描按 5 分钟节流（`scannedAt` 是最近一次真实扫描时刻）。
 */
export function getRateLimits(): Promise<RateLimitsPayload> {
  return call("get_rate_limits");
}

/** 限额 hook 安装状态（脚本 + 三处客户端配置）。 */
export function getRateLimitHookStatus(): Promise<RateLimitHookStatus> {
  return call("get_rate_limit_hook_status");
}

/** 安装限额 hook（幂等、写前备份；返回安装后的状态）。 */
export function installRateLimitHook(): Promise<RateLimitHookStatus> {
  return call("install_rate_limit_hook");
}

/** 卸载限额 hook（移除注册条目并尽量逐字节还原配置）。 */
export function uninstallRateLimitHook(): Promise<RateLimitHookStatus> {
  return call("uninstall_rate_limit_hook");
}

/** 限额监听开关（关闭后不扫日志、不渲染限额 chip）。 */
export function getRateLimitConfig(): Promise<RateLimitConfig> {
  return call("get_rate_limit_config");
}

export function saveRateLimitConfig(config: RateLimitConfig): Promise<RateLimitConfig> {
  return call("save_rate_limit_config", {
    config: config as unknown as Record<string, unknown>,
  });
}

export function checkin(accountId: string): Promise<CheckinResult> {
  return call("checkin", { accountId });
}

/**
 * 批量签到：不传档位时覆盖全部档位；显式传入时只处理该档位。
 *
 * 这里**不能**用 `variantArgs`：`checkin_all` 的缺省语义是「全部档位」，国内版若
 * 缺省不传参，账号页在国内版 Tab 触发的批量签到会打到国际版账号。显式下发 `cn`
 * 与改造前等价（改造前账号库里只有国内版账号）。
 */
export function checkinAll(variant?: WbVariant): Promise<{
  accounts: { accountId: string; email: string; result: string; error?: string; inactive?: boolean }[];
  status?: string;
  reason?: string;
}> {
  return call("checkin_all", variant ? { variant } : undefined);
}

export function getAutoCheckinConfig(): Promise<CheckinConfig> {
  return call("get_auto_checkin_config");
}

export function saveAutoCheckinConfig(config: CheckinConfig): Promise<CheckinConfig> {
  return call("save_auto_checkin_config", {
    config: config as unknown as Record<string, unknown>,
  });
}

export function getCheckinLogs(): Promise<{ logs: CheckinLog[] }> {
  return call("get_checkin_logs");
}

export function getAutoRotateConfig(): Promise<AutoRotateConfig> {
  return call("get_auto_rotate_config");
}

export function saveAutoRotateConfig(config: AutoRotateConfig): Promise<AutoRotateConfig> {
  return call("save_auto_rotate_config", {
    config: config as unknown as Record<string, unknown>,
  });
}

export function getRotateStatus(): Promise<RotateStatus> {
  return call("rotate_status");
}

/**
 * 手动触发一次轮换检查。
 *
 * `notify`（可选）：因存活门控被推迟、但除门控外本来会切换时由 core 组装好的提示内容。
 * 桌面端由宿主（Rust）直接投递系统通知，这里只保留字段以描述完整返回契约；
 * 无头 server 不投递，调用方按需自行处理。
 */
export function runRotate(): Promise<{
  status: string;
  reason?: string;
  error?: string;
  to?: string;
  notify?: { title: string; body: string };
}> {
  return call("run_rotate");
}

export function getRotateLogs(): Promise<{ logs: RotateLog[] }> {
  return call("get_rotate_logs");
}

export function refreshAccountToken(accountId: string): Promise<AccountMeta> {
  return call("refresh_account_token", { accountId });
}

// ---------------------------------------------------------------------------
// 阶段 4：自动更新
// ---------------------------------------------------------------------------

export function getGithubConfig(): Promise<GithubConfig> {
  return call("get_github_config");
}

export function saveGithubConfig(config: GithubConfig): Promise<GithubConfig> {
  return call("save_github_config", {
    config: config as unknown as Record<string, unknown>,
  });
}

export function checkUpdate(proxy?: string, force?: boolean): Promise<UpdateInfo> {
  return call("check_update", { proxy: proxy?.trim() || null, force: force ?? false });
}

export function relaunchApp(): Promise<void> {
  return call("relaunch_app");
}

// ---------------------------------------------------------------------------
// 开机自启（仅桌面端；webui 不提供同名接口，卡片也不在 webui 渲染）
// ---------------------------------------------------------------------------

/** 查询系统当前的开机自启注册状态（桌面端）。 */
export function getLaunchAtLoginEnabled(): Promise<boolean> {
  if (demoModeEnabled) return call("get_launch_at_login_enabled");
  if (!isDesktop()) return Promise.resolve(false);
  return call("get_launch_at_login_enabled");
}

/** 注册 / 移除系统开机自启，返回回读后的权威状态（桌面端）。 */
export function setLaunchAtLoginEnabled(enabled: boolean): Promise<boolean> {
  if (demoModeEnabled) return Promise.reject(new Error(DEMO_UNAVAILABLE_MESSAGE));
  if (!isDesktop()) return Promise.resolve(false);
  return call("set_launch_at_login_enabled", { enabled });
}

// ---------------------------------------------------------------------------
// 未收尾的切换（仅桌面端；webui 没有同名接口，卡片也不在 webui 渲染）
// ---------------------------------------------------------------------------

/**
 * 查未收尾的切换记录。启动时调一次：进程被杀/断电可能留下停在半途的换号，
 * 唯一能发现它的入口就是这个。webui 不提供，返回空表（不抛错，免得页面报红）。
 */
export function listUnfinished(): Promise<{ journals: SwitchJournal[]; warnings: string[] }> {
  if (!isDesktop()) return Promise.resolve({ journals: [], warnings: [] });
  return call<{ journals: SwitchJournal[]; warnings: string[] }>("unfinished_report").then((res) => ({
    journals: res?.journals ?? [],
    warnings: res?.warnings ?? [],
  }));
}

/** 按记录把现场退回（撤销一次没收尾的切换）。仅桌面端。 */
export function recoverSwitch(journal: SwitchJournal): Promise<{ ok: boolean; phase: string }> {
  if (!isDesktop()) {
    return Promise.reject(new Error("未收尾切换的恢复仅在桌面端可用"));
  }
  return call<string>("recover", { journal }).then((phase) => ({ ok: true, phase }));
}

/** 把 Tauri command / HTTP 抛出的错误统一为 Error。 */
export function asError(e: unknown): string {
  if (typeof e === "string") return e;
  if (e instanceof Error) return e.message;
  return JSON.stringify(e ?? "未知错误");
}

// ---------------------------------------------------------------------------
// 通知存档（toast 事后可查）
// ---------------------------------------------------------------------------

/** 记录一条应用内提示（由 `lib/notify.ts` 统一调用；失败不影响提示本身）。 */
export function recordNotification(
  level: AppNotification["level"],
  title: string,
  description?: string,
): Promise<{ recorded: boolean }> {
  if (demoModeEnabled) return Promise.resolve({ recorded: false });
  return call("record_notification", { level, title, description });
}

/** 读取最近的通知（新的在前，最多 100 条）。 */
export function listNotifications(): Promise<{ items: AppNotification[] }> {
  if (demoModeEnabled) return Promise.resolve({ items: [] });
  return call("list_notifications");
}

/** 清空通知存档。 */
export function clearNotifications(): Promise<{ cleared: boolean }> {
  if (demoModeEnabled) return Promise.resolve({ cleared: false });
  return call("clear_notifications");
}
