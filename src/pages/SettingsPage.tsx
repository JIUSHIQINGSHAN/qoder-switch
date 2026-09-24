import { useCallback, useEffect, useRef, useState, type ReactElement, type ReactNode } from "react";
import { ArrowUpCircle, CircleCheck, ExternalLink, Loader2, RefreshCw, Save } from "lucide-react";
import { toast } from "sonner";

import { Alert, AlertDescription, AlertTitle } from "@/components/ui/alert";
import { Button } from "@/components/ui/button";
import { Card, CardContent } from "@/components/ui/card";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import { Select, SelectContent, SelectItem, SelectTrigger, SelectValue } from "@/components/ui/select";
import { Switch } from "@/components/ui/switch";
import * as api from "@/lib/api";
import { getThemePreference, setThemePreference, type ThemePreference } from "@/lib/theme";
import type {
  AppNotification,
  AutoRotateConfig,
  CheckinConfig,
  CheckinLog,
  GithubConfig,
  RotateLog,
  RotateStatus,
  SwitchJournal,
  UpdateInfo,
} from "@/lib/types";
import { GITHUB_RELEASE_URL, GITHUB_REPOSITORY_URL, openReleaseUrl } from "@/lib/update";
import { cn } from "@/lib/utils";
import { UpdateInstallDialog } from "@/components/update-install-dialog";
import { DemoAction } from "@/components/demo-action";
import { useAccountsStore } from "@/stores/accounts";

interface SettingsGroupProps {
  id: string;
  title: string;
  children: ReactNode;
}

function SettingsGroup({ id, title, children }: SettingsGroupProps) {
  return (
    <section className="min-w-0 space-y-2.5" aria-labelledby={id}>
      <div className="px-1">
        <h2 id={id} className="text-[13px] font-medium leading-5">
          {title}
        </h2>
      </div>
      <Card className="min-w-0 gap-0 overflow-hidden rounded-xl py-0 shadow-none">{children}</Card>
    </section>
  );
}

function SettingsRow({ children, className }: { children: ReactNode; className?: string }) {
  return (
    <div
      className={cn(
        "mx-4 flex min-w-0 items-center justify-between gap-3 border-b border-border/50 px-0 py-2.5 sm:mx-5",
        className,
      )}
    >
      {children}
    </div>
  );
}

interface SettingsFieldRowProps {
  label: ReactNode;
  description?: ReactNode;
  htmlFor?: string;
  children: ReactNode;
  className?: string;
  operational?: boolean;
}

function SettingsFieldRow({
  label,
  description,
  htmlFor,
  children,
  className,
  operational = false,
}: SettingsFieldRowProps) {
  return (
    <SettingsRow className={cn("flex-col items-stretch gap-2 sm:flex-row sm:items-center", className)}>
      <div className="min-w-0 flex-1">
        {htmlFor ? (
          <Label htmlFor={htmlFor} className="text-[13px] leading-4">
            {label}
          </Label>
        ) : (
          <div className="text-[13px] font-medium leading-4">{label}</div>
        )}
        {description && (
          <p className="mt-0.5 text-xs leading-4 text-muted-foreground/75">{description}</p>
        )}
      </div>
      <div className="flex min-w-0 w-full shrink-0 justify-end sm:w-auto">
        {operational ? <DemoAction className="w-full sm:w-auto">{children as ReactElement}</DemoAction> : children}
      </div>
    </SettingsRow>
  );
}

function formatTime(ts: number): string {
  try {
    return new Date(ts).toLocaleString("zh-CN", {
      month: "2-digit",
      day: "2-digit",
      hour: "2-digit",
      minute: "2-digit",
      second: "2-digit",
    });
  } catch {
    return String(ts);
  }
}

function logLabel(result: string): { text: string; tone: "success" | "warning" | "error" } {
  switch (result) {
    case "success":
      return { text: "签到成功", tone: "success" };
    case "already":
      return { text: "已签到", tone: "warning" };
    default:
      return { text: "失败", tone: "error" };
  }
}

/** 自动签到配置 + 一键签到 + 日志。 */
function AutoCheckinCard() {
  const [cfg, setCfg] = useState<CheckinConfig | null>(null);
  const [logs, setLogs] = useState<CheckinLog[]>([]);
  const [saving, setSaving] = useState(false);
  const [busy, setBusy] = useState(false);
  const [msg, setMsg] = useState<{ type: "ok" | "err"; text: string } | null>(null);

  useEffect(() => {
    void load();
  }, []);

  async function load() {
    try {
      // 配置与日志分两步取：签到日志在 Qoder 侧不适用会抛错，放进 Promise.all
      // 会让 setCfg 永远执行不到，卡片就停在"加载配置中…"——原因已经在 msg 里说过了。
      setCfg(await api.getAutoCheckinConfig());
      const l = await api.getCheckinLogs();
      setLogs(l.logs);
    } catch (e) {
      setMsg({ type: "err", text: api.asError(e) });
    }
  }

  async function save() {
    if (!cfg) return;
    // 防重入：`disabled` 要到 React 下一次渲染才生效，同一帧内的两次点击仍会
    // 触发两次并发保存（写盘是 read-modify-write，会丢改动）。账号页早已这么做，
    // 设置页三处保存此前漏了。
    if (saving) return;
    setSaving(true);
    setMsg(null);
    try {
      const saved = await api.saveAutoCheckinConfig(cfg);
      setCfg(saved);
      setMsg({ type: "ok", text: "配置已保存" });
    } catch (e) {
      setMsg({ type: "err", text: api.asError(e) });
    } finally {
      setSaving(false);
    }
  }

  /**
   * 同步防重入：`busy` 是异步 state，同一帧内连点两次仍会触发两次并发请求。
   * 后端虽然已有进程级互斥门（第二次会返回 already_running），但这里先拦一道，
   * 免得用户看到一条本可避免的「正在进行」提示。与同文件 `save()` 的 `saving` 同理。
   */
  const checkinBusyRef = useRef(false);

  async function checkinAllNow() {
    if (checkinBusyRef.current) return;
    checkinBusyRef.current = true;
    setBusy(true);
    setMsg(null);
    try {
      const res = await api.checkinAll();
      if (res.status === "skipped" && res.reason === "already_running") {
        setMsg({ type: "err", text: "签到任务正在进行，请稍后再试" });
        return;
      }
      // 与账号页保持同一容错口径：缺 accounts 不能静默当成"没有账号"，
      // 也不该直接 TypeError（旧写法 `res.accounts.filter` 在缺键时会抛）。
      if (!Array.isArray(res.accounts)) {
        setMsg({ type: "err", text: "签到接口返回异常（缺少 accounts 字段）" });
        return;
      }
      const ok = res.accounts.filter((a) => a.result === "success").length;
      const already = res.accounts.filter((a) => a.result === "already").length;
      const inactive = res.accounts.filter((a) => a.result === "inactive" || a.inactive === true).length;
      const err = res.accounts.filter((a) => a.result === "error").length;
      const detail = res.accounts
        .filter((a) => a.result === "error")
        .map((a) => `${a.email}（${a.error}）`)
        .join("；");
      const parts = [`成功 ${ok}`, `已签 ${already}`, `失败 ${err}`];
      if (inactive > 0) parts.splice(2, 0, `未开放 ${inactive}`);
      setMsg({
        type: err > 0 && ok + already === 0 ? "err" : "ok",
        text: `签到完成：${parts.join("，")}${detail ? `。${detail}` : ""}`,
      });
      void load();
    } catch (e) {
      setMsg({ type: "err", text: api.asError(e) });
    } finally {
      checkinBusyRef.current = false;
      setBusy(false);
    }
  }

  function setNum(key: keyof CheckinConfig, value: string) {
    if (!cfg) return;
    setCfg({ ...cfg, [key]: Number(value) });
  }

  return (
    <SettingsGroup
      id="settings-auto-checkin"
      title="自动签到"
    >
      <CardContent className="space-y-0 p-0">
        {cfg ? (
          <>
            <SettingsFieldRow
              label="启用自动签到"
              description="启动时立即核验服务端状态，未签到账号会自动补签"
              htmlFor="ac-enabled"
              operational
            >
              <Switch
                id="ac-enabled"
                checked={cfg.enabled}
                onCheckedChange={(v) => setCfg({ ...cfg, enabled: v })}
              />
            </SettingsFieldRow>

            <SettingsFieldRow label="惰性刷新" description="小时" htmlFor="ac-lazy" operational>
              <Input
                id="ac-lazy"
                className="w-full sm:w-48"
                type="number"
                min={1}
                max={72}
                value={cfg.lazy_refresh_hours}
                onChange={(e) => setNum("lazy_refresh_hours", e.target.value)}
              />
            </SettingsFieldRow>

            <div className="flex flex-wrap gap-2 border-b-0 border-border/60 px-4 py-3 sm:px-5">
              <DemoAction><Button size="sm" onClick={save} disabled={saving}>
                {saving ? <Loader2 className="animate-spin" /> : <Save />}保存配置
              </Button></DemoAction>
              <DemoAction><Button size="sm" variant="outline" onClick={checkinAllNow} disabled={busy}>
                {busy ? <Loader2 className="animate-spin" /> : <CircleCheck />}全部立即签到
              </Button></DemoAction>
            </div>
          </>
        ) : (
          <p className="px-4 py-3 text-sm text-muted-foreground sm:px-5">加载配置中…</p>
        )}

        {msg && (
          <Alert
            variant={msg.type === "err" ? "destructive" : "default"}
            className="!w-auto mx-4 my-4 sm:mx-5"
          >
            <AlertDescription>{msg.text}</AlertDescription>
          </Alert>
        )}

        <div className="px-4 py-3 sm:px-5">
          <p className="mb-2 text-[13px] font-medium">签到日志（最近 30 天）</p>
          {logs.length === 0 ? (
            <p className="py-3 text-center text-sm text-muted-foreground">暂无签到记录</p>
          ) : (
            <div className="max-h-64 overflow-y-auto pr-1">
              {[...logs].reverse().map((l, i) => {
                const tone = logLabel(l.result);
                return (
                  <div
                    key={i}
                    className="flex items-center justify-between border-b border-border/60 py-2 text-xs last:border-b-0"
                  >
                    <div className="min-w-0 flex-1 truncate">
                      <span className="font-medium">{l.email}</span>
                      {l.error && <span className="text-destructive">（{l.error}）</span>}
                    </div>
                    <div className="ml-2 flex shrink-0 items-center gap-2">
                      <span
                        className={
                          tone.tone === "error"
                            ? "text-destructive"
                            : tone.tone === "warning"
                              ? "text-amber-600"
                              : "text-emerald-600"
                        }
                      >
                        {tone.text}
                      </span>
                      <span className="text-muted-foreground">{formatTime(l.ts)}</span>
                    </div>
                  </div>
                );
              })}
            </div>
          )}
        </div>
      </CardContent>
    </SettingsGroup>
  );
}

/** 自动轮换配置（Qoder CLI）+ 手动检查 + 日志。 */
function AutoRotateCard() {
  const [cfg, setCfg] = useState<AutoRotateConfig | null>(null);
  const [status, setStatus] = useState<RotateStatus | null>(null);
  const [logs, setLogs] = useState<RotateLog[]>([]);
  const [saving, setSaving] = useState(false);
  const [busy, setBusy] = useState(false);
  const [msg, setMsg] = useState<{ type: "ok" | "err"; text: string } | null>(null);

  useEffect(() => {
    void load();
  }, []);

  async function load() {
    try {
      const [c, s, l] = await Promise.all([
        api.getAutoRotateConfig(),
        api.getRotateStatus(),
        api.getRotateLogs(),
      ]);
      setCfg(c);
      setStatus(s);
      setLogs(l.logs);
    } catch (e) {
      setMsg({ type: "err", text: api.asError(e) });
    }
  }

  async function save() {
    if (!cfg) return;
    if (saving) return; // 防重入，见 AutoCheckinCard::save 的说明。
    setSaving(true);
    setMsg(null);
    try {
      const saved = await api.saveAutoRotateConfig(cfg);
      setCfg(saved);
      setMsg({ type: "ok", text: "配置已保存" });
    } catch (e) {
      setMsg({ type: "err", text: api.asError(e) });
    } finally {
      setSaving(false);
    }
  }

  async function runNow() {
    setBusy(true);
    setMsg(null);
    try {
      const res = await api.runRotate();
      // webui 没有事件通道：手动检查的推迟提示只能从返回值里取（桌面端由
      // `rotate-deferred` 事件统一弹出，避免同一件事弹两次）。
      if (api.isWebui() && res.notify?.body) {
        toast.warning("自动轮换已推迟", { description: res.notify.body, duration: 10_000 });
      }
      setMsg({
        type: res.status === "error" ? "err" : "ok",
        text:
          res.status === "switched"
            ? `已切换到 ${res.to ?? "目标账号"}`
            : res.status === "disabled"
              ? "自动轮换未启用（请在下方开启后重试）"
              : (res.reason ?? `检查完成：${res.status}`),
      });
      void load();
    } catch (e) {
      setMsg({ type: "err", text: api.asError(e) });
    } finally {
      setBusy(false);
    }
  }

  function setNum(key: keyof AutoRotateConfig, value: string) {
    if (!cfg) return;
    setCfg({ ...cfg, [key]: Number(value) });
  }

  function actionLabel(action: string): { text: string; tone: "success" | "warning" | "error" } {
    switch (action) {
      case "switched":
        return { text: "已切换", tone: "success" };
      case "skipped":
        return { text: "未切换", tone: "warning" };
      case "disabled":
        return { text: "未启用", tone: "warning" };
      case "error":
        return { text: "出错", tone: "error" };
      default:
        return { text: action, tone: "warning" };
    }
  }

  return (
    <SettingsGroup
      id="settings-auto-rotate"
      title="Qoder CLI 自动轮换"
    >
      <CardContent className="space-y-0 p-0">
        {status && (
          <div className="flex flex-wrap items-center gap-x-4 gap-y-1 border-b border-border/60 bg-muted/25 px-4 py-3 text-xs text-muted-foreground sm:px-5">
            <span>
              当前 CLI 账号：
              <b className="text-foreground">{status.activeAccountName ?? "未配置"}</b>
            </span>
            {status.lastCheckAt && <span>上次检查 {formatTime(status.lastCheckAt)}</span>}
            {status.lastSwitchAt && <span>上次切换 {formatTime(status.lastSwitchAt)}</span>}
            {!status.cliConfigured && (
              <span className="text-destructive">
                Qoder CLI 无独立账号指针（换桌面端登录态后 CLI 下次启动即生效）
              </span>
            )}
          </div>
        )}

        {cfg ? (
          <>
            <SettingsFieldRow
              label="启用自动轮换"
              description="开启后按下方间隔自动检查并切换 Qoder CLI 账号"
              htmlFor="ar-enabled"
              operational
            >
              <Switch
                id="ar-enabled"
                checked={cfg.enabled}
                onCheckedChange={(v) => setCfg({ ...cfg, enabled: v })}
              />
            </SettingsFieldRow>

            <SettingsFieldRow label="检查间隔" description="分钟" htmlFor="ar-interval" operational>
              <Input
                id="ar-interval"
                className="w-full sm:w-48"
                type="number"
                min={1}
                max={1440}
                value={cfg.check_interval_minutes}
                onChange={(e) => setNum("check_interval_minutes", e.target.value)}
              />
            </SettingsFieldRow>
            <SettingsFieldRow label="切换冷却" description="分钟" htmlFor="ar-cooldown" operational>
              <Input
                id="ar-cooldown"
                className="w-full sm:w-48"
                type="number"
                min={1}
                max={1440}
                value={cfg.cooldown_minutes}
                onChange={(e) => setNum("cooldown_minutes", e.target.value)}
              />
            </SettingsFieldRow>
            <SettingsFieldRow label="到期差异阈值" description="小时" htmlFor="ar-gap" operational>
              <Input
                id="ar-gap"
                className="w-full sm:w-48"
                type="number"
                min={0}
                max={720}
                value={cfg.min_gap_hours}
                onChange={(e) => setNum("min_gap_hours", e.target.value)}
              />
            </SettingsFieldRow>
            <SettingsFieldRow label="到期紧迫阈值" description="小时" htmlFor="ar-urgency" operational>
              <Input
                id="ar-urgency"
                className="w-full sm:w-48"
                type="number"
                min={0}
                max={720}
                value={cfg.min_urgency_hours}
                onChange={(e) => setNum("min_urgency_hours", e.target.value)}
              />
            </SettingsFieldRow>
            <SettingsFieldRow label="最小剩余积分" description="低于此值时不切换" htmlFor="ar-min" operational>
              <Input
                id="ar-min"
                className="w-full sm:w-48"
                type="number"
                min={0}
                value={cfg.min_remaining_credits}
                onChange={(e) => setNum("min_remaining_credits", e.target.value)}
              />
            </SettingsFieldRow>
            <p className="border-b border-border/60 px-4 py-3 text-[13px] leading-5 text-muted-foreground sm:px-5">
              切换时机：目标账号剩余到期时间少于「紧迫阈值」且比当前账号早超过「差异阈值」，且目标剩余积分不低于「最小剩余积分」。检测到有 Qoder CLI 会话在运行时，本次轮换会跳过并在当日最多提示 5 次；重启 CLI 后新账号才会生效。
            </p>

            <div className="flex flex-wrap gap-2 border-b-0 border-border/60 px-4 py-3 sm:px-5">
              <DemoAction><Button size="sm" onClick={save} disabled={saving}>
                {saving ? <Loader2 className="animate-spin" /> : <Save />}保存配置
              </Button></DemoAction>
              <DemoAction><Button size="sm" variant="outline" onClick={runNow} disabled={busy}>
                {busy ? <Loader2 className="animate-spin" /> : <RefreshCw />}立即检查一次
              </Button></DemoAction>
            </div>
          </>
        ) : (
          <p className="px-4 py-3 text-sm text-muted-foreground sm:px-5">加载配置中…</p>
        )}

        {msg && (
          <Alert
            variant={msg.type === "err" ? "destructive" : "default"}
            className="!w-auto mx-4 my-4 sm:mx-5"
          >
            <AlertDescription>{msg.text}</AlertDescription>
          </Alert>
        )}

        <div className="px-4 py-3 sm:px-5">
          <p className="mb-2 text-[13px] font-medium">轮换日志（最近 200 条）</p>
          {logs.length === 0 ? (
            <p className="py-3 text-center text-sm text-muted-foreground">暂无轮换记录</p>
          ) : (
            <div className="max-h-64 overflow-y-auto pr-1">
              {logs.map((l, i) => {
                const tone = actionLabel(l.action);
                return (
                  <div
                    key={i}
                    className="flex items-center justify-between border-b border-border/60 py-2 text-xs last:border-b-0"
                  >
                    <div className="min-w-0 flex-1 truncate">
                      {l.action === "switched" && l.from && l.to && (
                        <span className="font-medium">
                          {l.from.name ?? l.from.id} → {l.to.name ?? l.to.id}
                        </span>
                      )}
                      {l.reason && <span className="text-muted-foreground">（{l.reason}）</span>}
                    </div>
                    <div className="ml-2 flex shrink-0 items-center gap-2">
                      <span
                        className={
                          tone.tone === "error"
                            ? "text-destructive"
                            : tone.tone === "success"
                              ? "text-emerald-600"
                              : "text-amber-600"
                        }
                      >
                        {tone.text}
                      </span>
                      <span className="text-muted-foreground">{formatTime(l.ts)}</span>
                    </div>
                  </div>
                );
              })}
            </div>
          )}
        </div>
      </CardContent>
    </SettingsGroup>
  );
}

/** 权限检测卡片：确认本 App 是否有权写入 Qoder 认证文件（探针与展示路径同档位）。 */
function PermissionCheckCard() {
  const authFile = useAuthFile();
  const variant = useAccountsStore((s) => s.variant);
  const [checking, setChecking] = useState(false);
  const [result, setResult] = useState<null | { ok: boolean; text: string }>(null);

  async function runCheck() {
    setChecking(true);
    setResult(null);
    try {
      const res = await api.checkAuthPermission(variant);
      setResult({
        ok: res.ok,
        text: res.ok
          ? res.message ?? "认证目录可写，权限正常"
          : `${res.error}（${res.dir ?? ""}）`,
      });
    } catch (e) {
      setResult({ ok: false, text: api.asError(e) });
    } finally {
      setChecking(false);
    }
  }

  const isMac = api.isMacHost();

  return (
    <SettingsGroup
      id="settings-permission"
      title="权限检测"
    >
      <CardContent className="space-y-0 p-0">
        <div className="break-all border-b border-border/60 bg-muted/25 px-4 py-3 font-mono text-[11px] leading-5 text-muted-foreground sm:px-5">
          {authFile || "认证文件路径未获取"}
        </div>
        <div className="flex flex-wrap gap-2 border-b-0 border-border/60 px-4 py-3 sm:px-5">
          <DemoAction><Button size="sm" onClick={runCheck} disabled={checking}>
            {checking ? "检测中…" : "检测权限"}
          </Button></DemoAction>
          {isMac && (
            <>
              <DemoAction><Button
                size="sm"
                variant="outline"
                onClick={() => void api.openPermissionSettings("all_files")}
              >
                打开完全磁盘访问
              </Button></DemoAction>
              <DemoAction><Button
                size="sm"
                variant="outline"
                onClick={() => void api.openPermissionSettings("app_management")}
              >
                打开 App 管理
              </Button></DemoAction>
              <DemoAction><Button size="sm" variant="outline" onClick={() => void api.revealAppInFinder()}>
                在 Finder 中显示
              </Button></DemoAction>
            </>
          )}
        </div>

        {result && (
          <Alert variant={result.ok ? "default" : "destructive"} className="!w-auto mx-4 my-4 sm:mx-5">
            <AlertDescription>{result.text}</AlertDescription>
          </Alert>
        )}
        {isMac && result && !result.ok && (
          <div className="mx-4 mb-4 border-l-2 border-destructive/50 bg-muted/30 px-3 py-2.5 text-xs text-muted-foreground sm:mx-5">
            <p className="mb-1 font-medium text-foreground">如何授权（拖拽方式）：</p>
            <ol className="list-decimal space-y-1 pl-4">
              <li>点上方「打开完全磁盘访问」</li>
              <li>再点「在 Finder 中显示」打开 qoder-switch 所在位置</li>
              <li>
                把 <b>qoder-switch.app</b> 从 Finder <b>直接拖进</b>完全磁盘访问的列表区域
                （即使没有提示框，拖入即生效），然后打开它的开关
              </li>
              <li>回到本页点「检测权限」，或直接重试切换</li>
            </ol>
          </div>
        )}
      </CardContent>
    </SettingsGroup>
  );
}

function useAuthFile(): string | undefined {
  return useAccountsStore((s) => s.status?.authFile);
}

/** 自动更新：检查公开 GitHub Releases 源 + 安装签名更新。 */
function UpdateCard() {
  const version = useAccountsStore((s) => s.status?.version);
  const [info, setInfo] = useState<UpdateInfo | null>(null);
  const [checking, setChecking] = useState(false);
  const [installOpen, setInstallOpen] = useState(false);
  const [msg, setMsg] = useState<{ type: "ok" | "err"; text: string } | null>(null);
  const [githubConfig, setGithubConfig] = useState<GithubConfig>({});
  const [proxyUrl, setProxyUrl] = useState("");
  const [proxySaving, setProxySaving] = useState(false);

  useEffect(() => {
    let cancelled = false;
    void api
      .getGithubConfig()
      .then((config) => {
        if (cancelled) return;
        setGithubConfig(config);
        setProxyUrl(config.proxy ?? "");
      })
      .catch((e) => {
        if (!cancelled) setMsg({ type: "err", text: api.asError(e) });
      });
    return () => {
      cancelled = true;
    };
  }, []);

  async function check() {
    setChecking(true);
    setMsg(null);
    try {
      const r = await api.checkUpdate(proxyUrl, true);
      setInfo(r);
      if (!r.ok) {
        setMsg({ type: "err", text: r.message || r.error || "检查失败" });
      }
    } catch (e) {
      setMsg({ type: "err", text: api.asError(e) });
    } finally {
      setChecking(false);
    }
  }

  async function saveProxy() {
    if (proxySaving) return; // 防重入，见 AutoCheckinCard::save 的说明。
    const value = proxyUrl.trim();
    if (value) {
      try {
        const parsed = new URL(value);
        if (!parsed.hostname || !["http:", "https:"].includes(parsed.protocol)) {
          throw new Error("unsupported proxy protocol");
        }
      } catch {
        setMsg({ type: "err", text: "代理地址格式不正确，请填写 HTTP/HTTPS 地址，例如 http://127.0.0.1:7897" });
        return;
      }
    }

    setProxySaving(true);
    setMsg(null);
    try {
      const saved = await api.saveGithubConfig({ ...githubConfig, proxy: value });
      setGithubConfig(saved);
      setProxyUrl(saved.proxy ?? "");
      setMsg({ type: "ok", text: value ? "更新代理已保存" : "已关闭更新代理" });
    } catch (e) {
      setMsg({ type: "err", text: api.asError(e) });
    } finally {
      setProxySaving(false);
    }
  }

  return (
    <SettingsGroup
      id="settings-updates"
      title="自动更新"
    >
      <CardContent className="space-y-0 p-0">
        <div className="border-b border-border/60 px-4 py-3 text-sm sm:px-5">
          当前版本：<span className="font-mono">v{version || "?"}</span>
        </div>

        <div className="flex min-w-0 items-center justify-between gap-3 border-b border-border/60 bg-muted/25 px-4 py-3 text-sm sm:px-5">
          <div className="min-w-0 flex-1">
            <div className="font-medium">公开更新源</div>
            <div className="truncate text-xs text-muted-foreground">{GITHUB_REPOSITORY_URL}</div>
          </div>
          <DemoAction><Button
            variant="ghost"
            size="icon"
            title="打开 GitHub Release"
            onClick={() => void openReleaseUrl(GITHUB_RELEASE_URL)}
          >
            <ExternalLink />
          </Button></DemoAction>
        </div>

        <SettingsFieldRow
          label="更新代理地址"
          description="仅用于 GitHub 更新检查和安装包下载；留空表示关闭显式代理。"
          htmlFor="update-proxy"
          className="bg-muted/25"
          operational
        >
          <Input
            id="update-proxy"
            className="w-full sm:w-80"
            value={proxyUrl}
            onChange={(event) => setProxyUrl(event.target.value)}
            placeholder="例如 http://127.0.0.1:7897"
            spellCheck={false}
            autoComplete="off"
          />
        </SettingsFieldRow>

        <div className="flex flex-wrap gap-2 border-b border-border/60 bg-muted/25 px-4 py-3 sm:px-5">
          <DemoAction><Button size="sm" variant="outline" onClick={() => void saveProxy()} disabled={proxySaving}>
            {proxySaving ? <Loader2 className="animate-spin" /> : <Save />}
            保存代理
          </Button></DemoAction>
        </div>

        <div className="flex flex-wrap gap-2 border-b-0 border-border/60 px-4 py-3 sm:px-5">
          <DemoAction><Button size="sm" variant="outline" onClick={check} disabled={checking}>
            {checking ? <Loader2 className="animate-spin" /> : <RefreshCw />}
            检查更新
          </Button></DemoAction>
        </div>

        {info?.ok && (
          <Alert variant="default" className={cn("!w-auto mx-4 my-4 sm:mx-5", info.hasUpdate && "border-primary/35 bg-primary/[0.06]")}>
            {info.hasUpdate && <ArrowUpCircle className="text-primary" />}
            <AlertDescription className="space-y-2">
              <AlertTitle className={cn(info.hasUpdate && "text-primary")}>{info.hasUpdate ? "发现新版本" : "更新检查完成"}</AlertTitle>
              <div className="text-sm">
                {info.hasUpdate
                  ? `发现新版本 v${info.latest}（当前 v${info.current || version || "0.1.4"}）`
                  : `已是最新版本 v${info.current || version || "0.1.4"}`}
                {info.releaseName && <span className="text-muted-foreground"> · {info.releaseName}</span>}
              </div>
              {info.hasUpdate && (
                <DemoAction><Button size="sm" onClick={() => setInstallOpen(true)}>
                  <ArrowUpCircle />
                  立即升级
                </Button></DemoAction>
              )}
              {info.releaseUrl && (
                <DemoAction><Button
                  variant="link"
                  size="sm"
                  className="h-auto p-0"
                  onClick={() => void openReleaseUrl(info.releaseUrl)}
                >
                  打开 GitHub Release
                </Button></DemoAction>
              )}
            </AlertDescription>
          </Alert>
        )}
        {msg && (
          <Alert
            variant={msg.type === "err" ? "destructive" : "default"}
            className="!w-auto mx-4 my-4 sm:mx-5"
          >
            <AlertDescription>{msg.text}</AlertDescription>
          </Alert>
        )}
        <UpdateInstallDialog
          open={installOpen}
          onOpenChange={setInstallOpen}
          update={info}
        />
      </CardContent>
    </SettingsGroup>
  );
}

/** 开机自启（仅桌面端渲染）：开关直接反映系统自启注册状态，切换立即生效。 */
function StartupCard() {
  const [enabled, setEnabled] = useState<boolean | null>(null);
  const [busy, setBusy] = useState(false);
  const [msg, setMsg] = useState<{ type: "ok" | "err"; text: string } | null>(null);

  useEffect(() => {
    let cancelled = false;
    void api
      .getLaunchAtLoginEnabled()
      .then((value) => {
        if (!cancelled) setEnabled(value);
      })
      .catch((e) => {
        if (!cancelled) setMsg({ type: "err", text: api.asError(e) });
      });
    return () => {
      cancelled = true;
    };
  }, []);

  async function onToggle(value: boolean) {
    if (busy || enabled === null) return;
    const previous = enabled;
    setBusy(true);
    setMsg(null);
    try {
      // 后端回读 OS 权威状态；即使与请求一致，也以回读值显示。
      const authoritative = await api.setLaunchAtLoginEnabled(value);
      setEnabled(authoritative);
      const text = authoritative ? "已开启开机自启" : "已关闭开机自启";
      setMsg({ type: "ok", text });
      toast.success(text);
    } catch (e) {
      // 失败时恢复到最后一次确认的状态，并显示可读错误。
      setEnabled(previous);
      const text = api.asError(e);
      setMsg({ type: "err", text });
      toast.error("开机自启设置失败", { description: text });
    } finally {
      setBusy(false);
    }
  }

  return (
    <SettingsGroup
      id="settings-startup"
      title="启动设置"
    >
      <CardContent className="space-y-0 p-0">
        <SettingsFieldRow
          className="border-b-0"
          label="开机时静默启动到托盘"
          description="开关直接反映系统登录项状态；之后可从托盘「打开主界面」恢复"
          htmlFor="startup-silent"
          operational
        >
          <Switch
            id="startup-silent"
            checked={enabled ?? false}
            disabled={busy || enabled === null}
            onCheckedChange={(v) => void onToggle(v)}
            aria-label="开机时静默启动到托盘"
          />
        </SettingsFieldRow>

        {msg && (
          <Alert
            variant={msg.type === "err" ? "destructive" : "default"}
            className="!w-auto mx-4 my-4 sm:mx-5"
          >
            <AlertDescription>{msg.text}</AlertDescription>
          </Alert>
        )}
      </CardContent>
    </SettingsGroup>
  );
}

/** 外观：主题选择（持久化到 localStorage）。 */
const NOTIFICATION_LEVEL_LABEL: Record<AppNotification["level"], string> = {
  success: "成功",
  error: "错误",
  warning: "警告",
  info: "提示",
};

const NOTIFICATION_LEVEL_DOT: Record<AppNotification["level"], string> = {
  success: "bg-primary",
  error: "bg-destructive",
  warning: "bg-amber-500",
  info: "bg-muted-foreground/60",
};

/** 通知时间：当天只显示时分秒，更早显示完整时间。 */
function formatNotificationTime(at: number): string {
  const date = new Date(at);
  const sameDay = date.toDateString() === new Date().toDateString();
  return sameDay
    ? date.toLocaleTimeString("zh-CN", { hour12: false })
    : date.toLocaleString("zh-CN", { hour12: false });
}

/** 通知历史：最近 100 条应用内提示，供事后核对。 */
function NotificationHistoryCard() {
  const [open, setOpen] = useState(false);
  const [items, setItems] = useState<AppNotification[] | null>(null);
  const [error, setError] = useState("");

  useEffect(() => {
    if (!open) return;
    let cancelled = false;
    setError("");
    api
      .listNotifications()
      .then((res) => {
        if (!cancelled) setItems(res.items);
      })
      .catch((e) => {
        if (cancelled) return;
        setItems(null);
        setError(api.asError(e));
      });
    return () => {
      cancelled = true;
    };
  }, [open]);

  async function clearHistory() {
    try {
      await api.clearNotifications();
      setItems([]);
      toast.success("通知历史已清空");
    } catch (e) {
      toast.error("清空通知历史失败", { description: api.asError(e) });
    }
  }

  return (
    <SettingsGroup id="settings-notifications" title="通知历史">
      <CardContent className="space-y-0 p-0">
        <SettingsFieldRow
          className={open ? undefined : "border-b-0"}
          label="应用内提示存档"
          description="保留最近 100 条，便于事后核对；本机明文保存，可能含账号昵称与本地路径。"
        >
          <div className="flex items-center gap-2">
            <Button variant="outline" size="sm" onClick={() => setOpen((value) => !value)}>
              {open ? "收起" : "查看"}
            </Button>
            <Button
              variant="ghost"
              size="sm"
              disabled={!items || items.length === 0}
              onClick={clearHistory}
            >
              清空
            </Button>
          </div>
        </SettingsFieldRow>
        {open && (
          <div className="border-t border-border/50 px-4 py-1.5 sm:px-5">
            {error ? (
              <p className="py-2 text-xs text-destructive">{error}</p>
            ) : !items ? (
              <p className="py-2 text-xs text-muted-foreground">正在读取…</p>
            ) : items.length === 0 ? (
              <p className="py-2 text-xs text-muted-foreground">还没有记录到任何提示。</p>
            ) : (
              <ul className="max-h-72 divide-y divide-border/40 overflow-auto">
                {items.map((item, index) => (
                  <li key={`${item.at}-${index}`} className="py-1.5">
                    <div className="flex items-center gap-1.5 text-[11px] leading-4 text-muted-foreground">
                      <span
                        className={cn(
                          "size-1.5 shrink-0 rounded-full",
                          NOTIFICATION_LEVEL_DOT[item.level],
                        )}
                        aria-hidden
                      />
                      <span>{NOTIFICATION_LEVEL_LABEL[item.level]}</span>
                      <span aria-hidden>·</span>
                      <span>{formatNotificationTime(item.at)}</span>
                    </div>
                    <div className="mt-0.5 text-[13px] leading-5">{item.title}</div>
                    {item.description && (
                      <div className="mt-0.5 break-all text-xs leading-5 text-muted-foreground">
                        {item.description}
                      </div>
                    )}
                  </li>
                ))}
              </ul>
            )}
          </div>
        )}
      </CardContent>
    </SettingsGroup>
  );
}

function AppearanceCard() {
  const [theme, setTheme] = useState<ThemePreference>(getThemePreference);

  function onThemeChange(value: string) {
    if (value !== "system" && value !== "light" && value !== "dark") return;
    setThemePreference(value);
    setTheme(value);
  }

  return (
    <SettingsGroup
      id="settings-appearance"
      title="外观"
    >
      <CardContent className="space-y-0 p-0">
        <SettingsFieldRow
          className="border-b-0"
          label="主题"
          description="选择浅色、深色，或跟随系统外观自动切换"
          htmlFor="appearance-theme"
        >
          <Select value={theme} onValueChange={onThemeChange}>
            <SelectTrigger id="appearance-theme" size="sm" className="w-full sm:w-40" aria-label="主题">
              <SelectValue />
            </SelectTrigger>
            <SelectContent>
              <SelectItem value="system">系统</SelectItem>
              <SelectItem value="light">浅色</SelectItem>
              <SelectItem value="dark">深色</SelectItem>
            </SelectContent>
          </Select>
        </SettingsFieldRow>
      </CardContent>
    </SettingsGroup>
  );
}

/**
 * 未收尾的切换：进程被杀/断电可能让一次换号停在半途（现场是混合态）。
 * 这是**唯一**能发现它的入口 —— 此前 core 提供了 unfinished/recover，
 * 但前端零调用，等于这份恢复能力对用户不存在。
 *
 * 有记录（或有读取告警）时才渲染，平时不占版面。
 */
function UnfinishedSwitchCard() {
  const [journals, setJournals] = useState<SwitchJournal[]>([]);
  const [warnings, setWarnings] = useState<string[]>([]);
  const [busy, setBusy] = useState<string | null>(null);
  const [loaded, setLoaded] = useState(false);

  const reload = useCallback(async () => {
    try {
      const res = await api.listUnfinished();
      setJournals(res.journals);
      setWarnings(res.warnings);
    } catch (e) {
      setWarnings([api.asError(e)]);
    } finally {
      setLoaded(true);
    }
  }, []);

  useEffect(() => {
    void reload();
  }, [reload]);

  async function recoverOne(j: SwitchJournal) {
    setBusy(j.id);
    try {
      await api.recoverSwitch(j);
      toast.success("已退回切换前的登录态", { description: `账号 ${j.account_id}` });
      await reload();
    } catch (e) {
      toast.error("恢复失败", { description: api.asError(e) });
    } finally {
      setBusy(null);
    }
  }

  // 没有任何未收尾记录、也没有读取告警 → 不显示这张卡。
  if (!loaded || (journals.length === 0 && warnings.length === 0)) return null;

  return (
    <SettingsGroup id="settings-unfinished" title="未完成的切换">
      <CardContent className="space-y-3 p-4 sm:p-5">
        {warnings.length > 0 && (
          <Alert variant="warning">
            <AlertDescription>
              {warnings.map((w) => (
                <div key={w} className="break-all">{w}</div>
              ))}
            </AlertDescription>
          </Alert>
        )}
        {journals.length > 0 && (
          <>
            <p className="text-xs leading-5 text-muted-foreground">
              检测到上次切换没有正常收尾（可能是程序被关闭或断电）。此时登录态可能处于
              中途状态。建议退回切换前的账号，再重新操作一次。
            </p>
            <ul className="space-y-2">
              {journals.map((j) => (
                <li
                  key={j.id}
                  className="flex min-w-0 flex-wrap items-center justify-between gap-2 rounded-md border px-3 py-2"
                >
                  <div className="min-w-0">
                    <div className="truncate text-[13px] font-medium">{j.account_id}</div>
                    <div className="truncate text-xs text-muted-foreground">
                      {j.started_at} · 阶段 {j.phase}
                      {j.note ? ` · ${j.note}` : ""}
                    </div>
                  </div>
                  <Button
                    size="sm"
                    variant="outline"
                    disabled={busy === j.id}
                    onClick={() => void recoverOne(j)}
                  >
                    {busy === j.id ? "恢复中…" : "退回切换前"}
                  </Button>
                </li>
              ))}
            </ul>
          </>
        )}
      </CardContent>
    </SettingsGroup>
  );
}

/** 设置页：自动签到配置 / 权限检测 / 更新配置。 */
export default function SettingsPage() {
  return (
    <div className="mx-auto min-w-0 w-full max-w-3xl px-4 py-6 sm:px-6 sm:py-8">
      <header className="mb-10 sm:mb-12">
        <h1 className="text-2xl font-semibold tracking-tight">设置</h1>
        <p className="mt-2 text-sm leading-6 text-muted-foreground">自动签到、权限检测与自动更新配置。</p>
      </header>

      <div className="min-w-0 space-y-12">
        <AppearanceCard />
        {api.isDesktop() || api.isDemoMode() ? <UnfinishedSwitchCard /> : null}
        <PermissionCheckCard />
        <AutoCheckinCard />
        <AutoRotateCard />
        {api.isDesktop() || api.isDemoMode() ? <StartupCard /> : null}
        <NotificationHistoryCard />
        {api.isWebui() && !api.isDemoMode() ? null : <UpdateCard />}
      </div>
    </div>
  );
}
