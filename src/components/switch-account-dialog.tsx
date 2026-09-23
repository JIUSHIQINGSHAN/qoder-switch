import { useEffect, useState } from "react";
import { ChevronDown, ChevronRight, CircleAlert, ExternalLink, Folder, Loader2 } from "lucide-react";
import { listen } from "@tauri-apps/api/event";
import { toast } from "sonner";

import { Alert, AlertDescription, AlertTitle } from "@/components/ui/alert";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from "@/components/ui/dialog";
import { Tabs, TabsContent, TabsList, TabsTrigger } from "@/components/ui/tabs";
import { SessionSyncSection, type SessionLinksMeta } from "@/components/session-sync-section";
import * as api from "@/lib/api";
import { accountVariant, variantAppName } from "@/lib/variant";
import type {
  AccountMeta,
  Session,
  SessionLinkPreviewGroup,
  SessionSyncSelection,
  TemporaryFileInfo,
} from "@/lib/types";

interface Props {
  open: boolean;
  onOpenChange: (open: boolean) => void;
  /** 目标账号 */
  account: AccountMeta | null;
  /** 切换完成后刷新列表 */
  onDone?: () => void;
}

/** 设计稿的 tab 样式：下划线指示 + 可选计数徽标。 */
const TAB_TRIGGER_CLASS =
  "-mb-px h-9 flex-none rounded-none border-b-2 border-transparent px-0.5 pb-2 text-sm font-medium text-muted-foreground hover:text-foreground data-[state=active]:border-primary data-[state=active]:bg-transparent data-[state=active]:text-foreground data-[state=active]:shadow-none";

/** tab 计数徽标：0 时不显示，避免出现空的「0」。 */
function tabCount(count: number) {
  return count > 0 ? (
    <span className="ml-1 text-xs text-muted-foreground tabular-nums">{count}</span>
  ) : null;
}

/** 临时备份残留的可读描述：优先标题，其次会话 id，最后操作 id。 */
function describeTemporaryFile(item: TemporaryFileInfo): string {
  const label = item.title || item.sessionId || item.operationId;
  return `${label}：${item.reason}`;
}

/** 同一操作只保留一条：复制 / 同步 / 恢复三个报告可能重复上报同一残留。 */
function dedupeTemporaryFiles(items: TemporaryFileInfo[]): TemporaryFileInfo[] {
  const seen = new Map<string, TemporaryFileInfo>();
  for (const item of items) {
    const key = `${item.operationId}:${item.state}`;
    const existing = seen.get(key);
    if (!existing || (existing.state === "cleanupPending" && item.state === "needsRecovery")) {
      seen.set(key, item);
    }
  }
  return [...seen.values()];
}

/** 切换账号弹窗：可勾选当前账号的会话复制到目标账号（路径 B）。 */
export function SwitchAccountDialog({ open, onOpenChange, account, onDone }: Props) {
  const [sessions, setSessions] = useState<Session[]>([]);
  const [loadingSessions, setLoadingSessions] = useState(false);
  const [selected, setSelected] = useState<Set<string>>(new Set());
  /** 展开的节点：任务 / 空间 / 文件夹。默认全部收起。 */
  const [expanded, setExpanded] = useState<Set<string>>(new Set());
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState("");
  const [currentUid, setCurrentUid] = useState<string | null>(null);
  const [progress, setProgress] = useState("");
  /** 会话同步：勾选结果（默认值来自后端 defaultChecked）与预览组（结果反馈回显用）。 */
  const [syncSelections, setSyncSelections] = useState<SessionSyncSelection[]>([]);
  const [syncGroups, setSyncGroups] = useState<SessionLinkPreviewGroup[]>([]);
  /** 当前 tab：默认关联会话；该 tab 不可用时回落到复制会话。 */
  const [tab, setTab] = useState<"links" | "copy">("links");
  /** 关联会话区块上报的状态：tab 徽标与常驻提示用。 */
  const [linksMeta, setLinksMeta] = useState<SessionLinksMeta | null>(null);

  // 监听后端切换进度：桌面端走 Tauri 事件，webui 走 HTTP 轮询
  useEffect(() => {
    if (api.isWebui()) {
      const timer = setInterval(() => {
        void api.switchProgress().then((p) => {
          if (p.progress) setProgress(p.progress);
        });
      }, 600);
      return () => clearInterval(timer);
    }
    let unlisten: (() => void) | undefined;
    listen<unknown>("switch-progress", (e) => {
      const p = e.payload;
      if (typeof p === "string") {
        setProgress(p);
      } else if (p && typeof p === "object" && "message" in p) {
        setProgress(String((p as any).message));
      }
    }).then((fn) => {
      unlisten = fn;
    });
    return () => {
      unlisten?.();
    };
  }, []);

  // 打开时按目标账号档位加载当前账号会话（会话列表按档位取自各自的登录态）。
  // 守卫：关 A 开 B 时，A 的迟到响应不得写进 B 的弹窗（与 session-sync-section 同法）。
  useEffect(() => {
    let cancelled = false;
    if (open && account) {
      setSelected(new Set());
      setExpanded(new Set());
      setError("");
      setSyncSelections([]);
      setSyncGroups([]);
      setLinksMeta(null);
      setTab("links");
      setLoadingSessions(true);
      api
        .listSessions(accountVariant(account))
        .then((res) => {
          if (cancelled) return;
          setSessions(res.sessions);
          setCurrentUid(res.current);
        })
        .catch((e) => {
          if (!cancelled) setError(api.asError(e));
        })
        .finally(() => {
          if (!cancelled) setLoadingSessions(false);
        });
    }
    return () => {
      cancelled = true;
    };
  }, [open, account]);

  function toggleSession(id: string) {
    setSelected((prev) => {
      const next = new Set(prev);
      if (next.has(id)) next.delete(id);
      else next.add(id);
      return next;
    });
  }

  function toggleFolder(ids: string[]) {
    setSelected((prev) => {
      const next = new Set(prev);
      const allOn = ids.length > 0 && ids.every((id) => next.has(id));
      if (allOn) ids.forEach((id) => next.delete(id));
      else ids.forEach((id) => next.add(id));
      return next;
    });
  }

  function toggleExpanded(key: string) {
    setExpanded((prev) => {
      const next = new Set(prev);
      if (next.has(key)) next.delete(key);
      else next.add(key);
      return next;
    });
  }

  async function doSwitch(forced = false) {
    if (!account) return;
    setBusy(true);
    setProgress(forced ? "强制档：正在切换账号…" : "正在切换账号…");
    setError("");
    const requestedCopy = selected.size > 0;
    const requestedSync = syncSelections.length > 0;
    try {
      const res = await api.switchAccount({
        accountId: account.id,
        // 账号自身档位必须下发：后端缺省按国内版处理，Global 账号不传会切错档位的文件。
        // 国内版保持缺省（不带 variant），与改造前的请求体逐字一致。
        variant: accountVariant(account) === "ai" ? "ai" : undefined,
        copySessionIds: requestedCopy ? [...selected] : undefined,
        // 勾选绑定预览凭据；执行前后端会重新校验，版本变化则跳过该项。
        syncSelections: requestedSync ? syncSelections : undefined,
        // 强制档：跳过"被托管不许杀进程"的保护。只有用户在错误提示里点了
        // 「强制切换」按钮才会传 true —— 后端正常档的拒绝文案正在引导用户做这件事，
        // 但此前没有任何入口能把它传下去（契约声明了 forced，UI 从未接线）。
        forced: forced || undefined,
      });
      const nickname = account.nickname || account.email || account.uid || "该账号";
      const parts: string[] = [];
      const copyReport = res.sessionCopy;
      const copiedCount = copyReport?.copied?.length ?? 0;
      const linkedCount = copyReport?.alreadyLinked?.length ?? 0;
      const copyErrors = copyReport?.errors ?? [];
      if (copiedCount > 0) parts.push(`已复制 ${copiedCount} 个会话`);
      if (linkedCount > 0) parts.push(`已关联 ${linkedCount} 个之前复制过的会话`);
      if (copyErrors.length > 0) parts.push(`会话复制失败 ${copyErrors.length} 个`);
      const syncReport = res.sessionSync;
      const syncedItems = syncReport?.synced ?? [];
      const skippedItems = syncReport?.skipped ?? [];
      const syncErrors = syncReport?.errors ?? [];
      if (syncedItems.length > 0) parts.push(`已同步 ${syncedItems.length} 个会话`);
      if (skippedItems.length > 0) parts.push(`跳过 ${skippedItems.length} 个会话`);
      if (syncErrors.length > 0) parts.push(`会话同步失败 ${syncErrors.length} 个`);
      if (res.backup) parts.push(`备份: ${res.backup}`);
      // 成功清理：临时备份已回收，不再展示可还原路径；待清理项单独提示，不写进成功文案。
      toast.success(`已切换至「${nickname}」`, {
        description: parts.length ? parts.join("；") : `${variantAppName(accountVariant(account))} 已重启为目标账号。`,
      });
      // 复制失败或被后端跳过时必须显式提示，不能静默当成成功。
      if (copyReport?.error) {
        toast.error("会话复制未执行", { description: copyReport.error });
      } else if (copyErrors.length > 0) {
        // 失败提示必须带会话标识（能取标题时优先标题），不是只有原因。
        const sessionLabel = (id: string) =>
          sessions.find((item) => item.id === id)?.title || id;
        toast.error("部分会话未复制", {
          description: copyErrors
            .map((item) => `${sessionLabel(item.id)}：${item.error}`)
            .join("；"),
        });
      } else if (requestedCopy && !copyReport) {
        toast.warning("会话未复制", {
          description: "没有收到复制结果：当前版本可能不支持复制会话；账号已切换，但会话没有复制。",
        });
      }
      // 同步结果：成功、跳过、失败与恢复信息都要能看到，不能只显示成功数。
      const groupLabel = (groupId: string) =>
        syncGroups.find((group) => group.groupId === groupId)?.title || groupId;
      // 失败与跳过合并成一条提示：两者可能同时出现，不能只报其中一类。
      const syncIssues = [
        ...syncErrors.map(
          (item) => `${item.groupId ? groupLabel(item.groupId) : "全部会话"}：${item.error}`,
        ),
        // 预览过期/前置条件变化一律跳过并给出原因，绝不显示成已同步。
        ...skippedItems.map((item) => `${groupLabel(item.groupId)}：${item.message}`),
      ];
      if (syncIssues.length > 0) {
        if (syncErrors.length > 0) {
          toast.error("部分会话没有同步（失败或已跳过）", { description: syncIssues.join("；") });
        } else {
          toast.warning("有会话没有同步（已跳过）", { description: syncIssues.join("；") });
        }
      } else if (requestedSync && !syncReport) {
        toast.warning("会话同步未执行", {
          description: "没有收到同步结果：当前版本可能不支持同步会话，或本次切换没有重启应用。",
        });
      }
      if (syncReport?.needsRecovery) {
        toast.error("有会话同步没有完成", {
          description: "已保留操作记录与备份，下次切换会先恢复；恢复完成前不会再改动目标账号的内容。",
        });
      }
      // 未完成的会话写入：可重试项只提示，阻断项由后端直接返回错误。
      const recoveryIssues = res.sessionRecovery?.needsRecovery ?? [];
      if (recoveryIssues.length > 0) {
        toast.error("有会话操作没有完成", {
          description: recoveryIssues.map((issue) => issue.reason).join("；"),
        });
      }
      // 临时备份残留：待清理说明重试入口；待恢复说明必须保留材料并给出下一步。
      const temporaryFiles = dedupeTemporaryFiles([
        ...(copyReport?.temporaryFiles ?? []),
        ...(syncReport?.temporaryFiles ?? []),
        ...(res.sessionRecovery?.temporaryFiles ?? []),
      ]);
      const pendingFiles = temporaryFiles.filter((item) => item.state === "cleanupPending");
      const recoveryFiles = temporaryFiles.filter((item) => item.state === "needsRecovery");
      // 报告级 temporaryFiles 是权威来源；仅当旧后端没给该字段时，才回退到成功项上的 pending。
      const pendingCleanupReasons: string[] = [];
      if (temporaryFiles.length === 0) {
        for (const item of copyReport?.copied ?? []) {
          if (item.cleanupState === "pending") {
            pendingCleanupReasons.push(item.cleanupError ?? "临时文件待清理");
          }
        }
        for (const item of syncedItems) {
          if (item.cleanupState === "pending") {
            pendingCleanupReasons.push(item.cleanupError ?? "临时文件待清理");
          }
        }
      }
      if (pendingCleanupReasons.length > 0 || pendingFiles.length > 0) {
        const details = [...pendingCleanupReasons, ...pendingFiles.map(describeTemporaryFile)];
        toast.warning("会话已完成，部分临时文件待清理", {
          description: `${details.join("；")}。下次切号时会自动重试清理。`,
        });
      }
      if (recoveryFiles.length > 0) {
        toast.error("有临时备份需要确认", {
          description: `${recoveryFiles.map(describeTemporaryFile).join("；")}。已保留现场，请按提示处理后重试。`,
        });
      }
      onOpenChange(false);
      onDone?.();
    } catch (e) {
      setError(api.asError(e));
    } finally {
      setBusy(false);
      setProgress("");
    }
  }

  /** 打开系统设置授权面板（默认完全磁盘访问），供小白一键跳转。 */
  async function openPermissionSettings() {
    try {
      await api.openPermissionSettings("all_files");
    } catch (e) {
      // 打开失败时退化为提示
      setError(api.asError(e));
    }
  }

  /** 权限自检：确认完全磁盘访问是否生效（探针按目标账号档位）。 */
  const [permCheck, setPermCheck] = useState<string | null>(null);
  async function runPermissionCheck() {
    setPermCheck("检测中…");
    try {
      const res = await api.checkAuthPermission(accountVariant(account));
      setPermCheck(res.ok ? `✓ ${res.message}` : `✗ ${res.error}（${res.dir}）`);
    } catch (e) {
      setPermCheck(`✗ ${api.asError(e)}`);
    }
  }

  // 出现「无权限」错误时，自动每 2s 轮询一次授权状态；用户拖入 app 授权成功后自动恢复
  useEffect(() => {
    if (!error.includes("无权限")) return;
    let cancelled = false;
    let timer: number | undefined;
    const check = async () => {
      try {
        const res = await api.checkAuthPermission(accountVariant(account));
        if (res.ok) {
          if (!cancelled) {
            setPermCheck("✓ 授权成功，可以重新切换了");
            setError("");
          }
          return;
        }
      } catch {
        /* 忽略中间态 */
      }
      if (!cancelled) timer = window.setTimeout(check, 2000);
    };
    check();
    return () => {
      cancelled = true;
      if (timer) window.clearTimeout(timer);
    };
  }, [error]);

  const copyCount = selected.size;
  const syncCount = syncSelections.length;
  /** 覆盖项数量：底部摘要据此提示风险。 */
  const overwriteCount = syncSelections.filter((item) => item.mode === "overwrite").length;
  const linksAvailable = linksMeta?.available ?? true;
  const summaryMain =
    copyCount > 0 && syncCount > 0
      ? `将复制 ${copyCount} 个、同步 ${syncCount} 个关联会话`
      : copyCount > 0
        ? `将复制 ${copyCount} 个会话`
        : syncCount > 0
          ? `将同步 ${syncCount} 个关联会话`
          : "本次仅切换账号";
  const summarySub =
    overwriteCount > 0
      ? `其中 ${overwriteCount} 个会替换目标账号的完整内容`
      : copyCount === 0 && syncCount === 0
        ? "未选择复制或同步会话"
        : copyCount === 0
          ? "未选择复制会话"
          : syncCount === 0
            ? "未选择同步会话"
            : null;
  const needsPermission = error.includes("无权限");
  /** 正常档因"被托管/判不出"被拒时会给出这段文案，后端明确引导用户改走强制档。
   *  只有这种情形才显示「强制切换」按钮 —— 其它错误用强制档只会更错。 */
  const canForceSwitch = !busy && error.includes("强制档");
  const sessionsEmpty = !loadingSessions && sessions.length === 0;
  const copyHint = loadingSessions
    ? "正在加载会话…"
    : error && sessionsEmpty
      ? "无法加载会话列表，暂不能复制"
      : sessionsEmpty
        ? currentUid
          ? "当前账号暂无会话，无法复制"
          : "未检测到当前登录账号，无法列出会话"
        : "把当前账号勾选的会话复制给目标账号（复制后归属目标账号）；已经复制过的不会重复复制";

  // 关联 tab 不可用（国际版能力判定不通过）时回落到复制会话，避免停在空 tab。
  useEffect(() => {
    if (!linksAvailable && tab === "links") setTab("copy");
  }, [linksAvailable, tab]);

  // 「复制会话」tab 内容：勾选即意图，提交结果由底部摘要兜底确认。
  const copyTabContent = (
    <>
      {!loadingSessions && (
        <p
          className={
            sessionsEmpty
              ? "px-1 text-xs text-amber-700 dark:text-amber-400"
              : "px-1 text-xs text-muted-foreground"
          }
        >
          {copyHint}
        </p>
      )}
      {loadingSessions ? (
        <div className="flex items-center gap-2 py-4 text-sm text-muted-foreground">
          <Loader2 className="animate-spin" /> 加载会话…
        </div>
      ) : sessions.length === 0 ? (
        <p className="py-4 text-center text-sm text-muted-foreground">
          {currentUid ? "当前账号暂无会话" : "未检测到当前登录账号，无法列出会话"}
        </p>
      ) : (
        <div className="max-h-[min(22rem,45vh)] overflow-y-auto pr-1">
          {buildSessionTree(sessions).map((kind) => {
            const kindOpen = expanded.has(kind.key);
            const kindSel = selectionState(kind.sessions, selected);
            return (
              <div key={kind.key} className="mb-0.5">
                <div className="sticky top-0 z-10 flex items-center gap-1.5 rounded-md bg-background px-1.5 py-1">
                  <TreeCheckbox
                    allOn={kindSel.allOn}
                    someOn={kindSel.someOn}
                    onChange={() => toggleFolder(kind.sessions.map((s) => s.id))}
                    ariaLabel={`选择${kind.label}`}
                  />
                  <button
                    type="button"
                    className="flex min-w-0 flex-1 cursor-pointer items-center gap-1 rounded px-1 py-0.5 text-left hover:bg-accent/50"
                    onClick={() => toggleExpanded(kind.key)}
                    aria-expanded={kindOpen}
                    aria-label={`${kindOpen ? "折叠" : "展开"}${kind.label}`}
                  >
                    <span className="min-w-0 flex-1 truncate text-sm font-medium">
                      {kind.label}
                      <span className="ml-1 font-normal text-muted-foreground">
                        ({kind.count})
                      </span>
                    </span>
                    {kindOpen ? (
                      <ChevronDown className="size-3.5 shrink-0 text-muted-foreground" />
                    ) : (
                      <ChevronRight className="size-3.5 shrink-0 text-muted-foreground" />
                    )}
                  </button>
                </div>
                {kindOpen && kind.key === "task" &&
                  kind.sessions.map((s) => (
                    <SessionPickRow
                      key={s.id}
                      session={s}
                      checked={selected.has(s.id)}
                      indentClass="pl-7"
                      onToggle={() => toggleSession(s.id)}
                    />
                  ))}
                {kindOpen &&
                  kind.folders?.map((folder) => {
                    const folderOpen = expanded.has(folder.key);
                    const folderSel = selectionState(folder.sessions, selected);
                    return (
                      <div key={folder.key}>
                        <div className="flex items-center gap-1.5 px-1.5 py-0.5 pl-7">
                          <TreeCheckbox
                            allOn={folderSel.allOn}
                            someOn={folderSel.someOn}
                            onChange={() => toggleFolder(folder.sessions.map((s) => s.id))}
                            ariaLabel={`选择文件夹 ${folder.label}`}
                          />
                          <button
                            type="button"
                            className="flex min-w-0 flex-1 cursor-pointer items-center gap-1.5 rounded px-1 py-0.5 text-left hover:bg-accent/50"
                            onClick={() => toggleExpanded(folder.key)}
                            aria-expanded={folderOpen}
                            aria-label={`${folderOpen ? "折叠" : "展开"}文件夹 ${folder.label}`}
                          >
                            <Folder className="size-3.5 shrink-0 text-muted-foreground" />
                            <span className="min-w-0 flex-1 truncate text-sm">
                              {folder.label}
                            </span>
                            {folderOpen ? (
                              <ChevronDown className="size-3.5 shrink-0 text-muted-foreground" />
                            ) : (
                              <ChevronRight className="size-3.5 shrink-0 text-muted-foreground" />
                            )}
                          </button>
                        </div>
                        {folderOpen &&
                          folder.sessions.map((s) => (
                            <SessionPickRow
                              key={s.id}
                              session={s}
                              checked={selected.has(s.id)}
                              indentClass="pl-12"
                              onToggle={() => toggleSession(s.id)}
                            />
                          ))}
                      </div>
                    );
                  })}
              </div>
            );
          })}
        </div>
      )}
    </>
  );

  function handleOpenChange(nextOpen: boolean) {
    if (!nextOpen && !busy) {
      setError("");
      setProgress("");
    }
    onOpenChange(nextOpen);
  }

  return (
    <Dialog open={open} onOpenChange={handleOpenChange}>
      <DialogContent
        showCloseButton={!busy}
        className="flex max-h-[min(90vh,calc(100vh-2rem))] min-w-0 flex-col overflow-hidden"
      >
        <DialogHeader className="shrink-0">
          <DialogTitle>切换到「{account?.nickname || account?.email || account?.uid || "该账号"}」</DialogTitle>
          <DialogDescription>
            切换时将重启 {variantAppName(accountVariant(account))}。
            <br />
            注：Qoder 的会话历史不按账号隔离，切换后本机历史会话可能在新账号下可见。
          </DialogDescription>
        </DialogHeader>

        {busy && (
          <div className="absolute inset-0 z-50 flex flex-col items-center justify-center gap-3 rounded-lg bg-background/85 backdrop-blur-sm">
            <Loader2 className="size-8 animate-spin text-primary" />
            <p className="text-sm font-medium">{progress || "正在切换账号…"}</p>
            <p className="max-w-xs text-center text-xs text-muted-foreground">
              正在处理中，请勿关闭窗口
            </p>
          </div>
        )}

        {/* min-h-0：矮窗口下压缩本区而不是裁切页脚。列表按自身 max-h 滚动；矮窗口或权限大卡时 tab 内容区可滚，避免裁切。仅当常驻提示超过剩余空间时本层滚动。 */}
        <div className="flex min-h-0 flex-col gap-3 overflow-x-hidden overflow-y-auto">
          {/* 常驻提示区：切换错误 / 权限 / 关联检查失败不随 tab 切换隐藏。 */}
          {error && (
            <Alert variant={needsPermission ? "warning" : "destructive"} className="min-w-0 shrink-0 break-all">
              <AlertDescription className="min-w-0 break-all">
                <div className="min-w-0 break-all">{error}</div>
                {needsPermission && (
                  <div className="mt-2 space-y-2">
                    <div className="rounded-md border bg-muted/60 p-3 text-xs text-muted-foreground">
                      <p className="mb-1 font-medium text-foreground">如何授权（只需 3 步）：</p>
                      {api.isMacHost() ? (
                        /* macOS：写权限之外还有一道真正的门 —— 读 Qoder 创建的
                           登录钥匙串条目（桌面凭据的主密钥在那里，不在文件里）。 */
                        <ol className="list-decimal space-y-1 pl-4">
                          <li>点击下方「在访达中显示」，确认本 App 的位置与身份</li>
                          <li>
                            点「立即检测」，在弹出的<b>钥匙串授权</b>框里选<b>始终允许</b>
                            （只点「允许」的话下次还会再问）
                          </li>
                          <li>
                            若目录仍不可写，再进「系统设置 → 隐私与安全性 → 完全磁盘访问」，
                            把 <b>qoder-switch.app</b> 拖进列表并打开开关
                          </li>
                        </ol>
                      ) : (
                        <ol className="list-decimal space-y-1 pl-4">
                          <li>
                            确认本工具与 Qoder 跑在<b>同一个 Windows 用户</b>下
                            （凭据由 DPAPI 按用户加密，换用户既写不进也解不开）
                          </li>
                          <li>
                            若认证目录只读或被占用，先关掉 Qoder 再试；
                            杀软拦截时把本工具加入白名单
                          </li>
                          <li>点「立即检测」复查，无需其他操作</li>
                        </ol>
                      )}
                    </div>
                    <div className="flex flex-wrap gap-2">
                      {api.isMacHost() && (
                        <Button variant="outline" size="sm" onClick={openPermissionSettings}>
                          <ExternalLink />
                          打开完全磁盘访问
                        </Button>
                      )}
                      <Button
                        variant="outline"
                        size="sm"
                        onClick={() => void api.revealAppInFinder()}
                      >
                        {api.isMacHost() ? "在访达中显示" : "在资源管理器中显示"}
                      </Button>
                      <Button variant="secondary" size="sm" onClick={runPermissionCheck}>
                        立即检测
                      </Button>
                    </div>
                  </div>
                )}
                {permCheck && <div className="mt-2 text-xs">{permCheck}</div>}
                {canForceSwitch && (
                  <div className="mt-2 space-y-2">
                    <div className="rounded-md border bg-muted/60 p-3 text-xs text-muted-foreground">
                      <p className="mb-1 font-medium text-foreground">什么是强制切换？</p>
                      <p>
                        当前这个程序是被 Qoder 自己启动的，切换会把它连同本窗口一起关掉。
                        正常档为防误伤会拒绝执行。
                      </p>
                      <p className="mt-1">
                        点「强制切换」表示你已知情：
                        <b>窗口会关闭，请稍后从桌面图标或托盘重新打开，用新账号登录。</b>
                      </p>
                    </div>
                    <Button variant="destructive" size="sm" onClick={() => void doSwitch(true)}>
                      强制切换
                    </Button>
                  </div>
                )}
              </AlertDescription>
            </Alert>
          )}
          {linksMeta?.error && (
            <Alert variant="destructive" className="min-w-0 shrink-0">
              <CircleAlert />
              <AlertTitle>无法检查会话</AlertTitle>
              <AlertDescription className="min-w-0 break-all">{linksMeta.error}</AlertDescription>
            </Alert>
          )}
          {linksMeta?.storeStatus === "unavailable" && (
            <Alert variant="warning" className="min-w-0 shrink-0">
              <CircleAlert />
              <AlertTitle>同步记录不可用</AlertTitle>
              <AlertDescription className="min-w-0 break-all">
                {`${linksMeta.storeError || "原因未知"}；本次不会同步任何会话，请处理后重试。`}
              </AlertDescription>
            </Alert>
          )}

          {linksAvailable ? (
            <Tabs
              value={tab}
              onValueChange={(value) => setTab(value as "links" | "copy")}
              className="flex min-h-0 flex-col gap-3 overflow-hidden"
            >
              <TabsList className="h-auto w-full shrink-0 justify-start gap-5 rounded-none border-b border-border bg-transparent p-0">
                <TabsTrigger value="links" className={TAB_TRIGGER_CLASS}>
                  关联会话
                  {tabCount(linksMeta?.groupCount ?? 0)}
                </TabsTrigger>
                <TabsTrigger value="copy" className={TAB_TRIGGER_CLASS}>
                  复制会话
                  {tabCount(copyCount)}
                </TabsTrigger>
              </TabsList>
              {/* forceMount：切换 tab 不得卸载另一侧，否则关联勾选会被预览重拉重置。 */}
              <TabsContent
                value="links"
                forceMount
                className="min-h-0 overflow-y-auto data-[state=inactive]:hidden data-[state=active]:animate-in data-[state=active]:fade-in-0 data-[state=active]:duration-150"
              >
                <SessionSyncSection
                  open={open}
                  account={account}
                  disabled={busy}
                  onChange={(state) => {
                    setSyncSelections(state.selections);
                    setSyncGroups(state.groups);
                  }}
                  onMetaChange={setLinksMeta}
                />
              </TabsContent>
              <TabsContent
                value="copy"
                forceMount
                className="min-h-0 space-y-2 overflow-y-auto data-[state=inactive]:hidden data-[state=active]:animate-in data-[state=active]:fade-in-0 data-[state=active]:duration-150"
              >
                {copyTabContent}
              </TabsContent>
            </Tabs>
          ) : (
            <div className="min-h-0 space-y-2 overflow-y-auto">{copyTabContent}</div>
          )}
        </div>

        <DialogFooter className="shrink-0 sm:justify-between">
          <div className="min-w-0 space-y-0.5">
            <div className="text-sm font-medium">{summaryMain}</div>
            {summarySub && <div className="text-xs text-muted-foreground">{summarySub}</div>}
          </div>
          <div className="flex shrink-0 gap-2">
            <Button variant="outline" onClick={() => onOpenChange(false)} disabled={busy}>
              取消
            </Button>
            <Button onClick={() => void doSwitch()} disabled={busy}>
              {busy ? "切换中…" : "确认切换"}
            </Button>
          </div>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  );
}

type FolderGroup = { key: string; label: string; sessions: Session[] };
type KindGroup = {
  key: "task" | "space";
  label: string;
  count: number;
  sessions: Session[];
  folders?: FolderGroup[];
};

function selectionState(sessions: Session[], selected: Set<string>) {
  const ids = sessions.map((s) => s.id);
  const n = ids.filter((id) => selected.has(id)).length;
  return { allOn: n === ids.length && ids.length > 0, someOn: n > 0 && n < ids.length };
}

function TreeCheckbox({
  allOn,
  someOn,
  onChange,
  ariaLabel,
}: {
  allOn: boolean;
  someOn: boolean;
  onChange: () => void;
  ariaLabel: string;
}) {
  return (
    <input
      type="checkbox"
      className="size-3.5 shrink-0 cursor-pointer accent-primary"
      checked={allOn}
      ref={(el) => {
        if (el) el.indeterminate = someOn;
      }}
      onChange={onChange}
      aria-label={ariaLabel}
    />
  );
}

function SessionPickRow({
  session,
  checked,
  indentClass,
  onToggle,
}: {
  session: Session;
  checked: boolean;
  indentClass: string;
  onToggle: () => void;
}) {
  return (
    <label
      className={`flex cursor-pointer items-center gap-2.5 rounded-md py-1.5 pr-2 hover:bg-accent/50 ${indentClass}`}
    >
      <input
        type="checkbox"
        className="size-3.5 shrink-0 cursor-pointer accent-primary"
        checked={checked}
        onChange={onToggle}
      />
      <span className="min-w-0 flex-1 truncate text-sm" title={session.title}>
        {session.title}
      </span>
      {session.hasHistory && (
        <Badge variant="outline" className="shrink-0 text-[10px]">
          有内容
        </Badge>
      )}
    </label>
  );
}

/** Qoder 侧栏文件夹名：cwd 最后一段。 */
function sessionFolderLabel(cwd: string): string {
  const normalized = cwd.trim().replace(/[\\/]+$/, "");
  if (!normalized) return "未分组";
  const parts = normalized.split(/[\\/]/);
  return parts[parts.length - 1] || normalized;
}

/** 按工作目录分组，文件夹顺序跟会话一样按最近活动排。 */
function groupSessionsByFolder(sessions: Session[]): FolderGroup[] {
  const groups = new Map<string, Session[]>();
  const order: string[] = [];
  for (const session of sessions) {
    const key = session.cwd.trim() || "__none__";
    let list = groups.get(key);
    if (!list) {
      list = [];
      groups.set(key, list);
      order.push(key);
    }
    list.push(session);
  }
  return order.map((key) => ({
    key,
    label: key === "__none__" ? "未分组" : sessionFolderLabel(key),
    sessions: groups.get(key) ?? [],
  }));
}

/** 对齐 Qoder 侧栏：任务（playground）平铺，空间按文件夹分组。 */
function buildSessionTree(sessions: Session[]): KindGroup[] {
  const tasks = sessions.filter((s) => s.isPlayground);
  const spaces = sessions.filter((s) => !s.isPlayground);
  const groups: KindGroup[] = [];
  if (tasks.length > 0) {
    groups.push({ key: "task", label: "任务", count: tasks.length, sessions: tasks });
  }
  if (spaces.length > 0) {
    const folders = groupSessionsByFolder(spaces);
    groups.push({
      key: "space",
      label: "空间",
      count: folders.length,
      sessions: spaces,
      folders,
    });
  }
  return groups;
}
