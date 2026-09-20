import { useEffect, useState } from "react";
import { ChevronRight, CircleAlert, Link2, Loader2, RotateCw } from "lucide-react";

import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { Checkbox } from "@/components/ui/checkbox";
import { Collapsible, CollapsibleContent, CollapsibleTrigger } from "@/components/ui/collapsible";
import { Tooltip, TooltipContent, TooltipTrigger } from "@/components/ui/tooltip";
import * as api from "@/lib/api";
import { cn } from "@/lib/utils";
import { accountVariant } from "@/lib/variant";
import type {
  AccountMeta,
  SessionLinkPreviewGroup,
  SessionLinksPreview,
  SessionSyncMode,
  SessionSyncSelection,
  SessionSyncVerdict,
} from "@/lib/types";

interface Props {
  /** 目标账号（来源账号身份由后端从该档位登录态读取，前端不传） */
  account: AccountMeta | null;
  /** 弹窗是否打开：打开时拉取一次预览 */
  open: boolean;
  /** 切换进行中：禁止继续交互 */
  disabled?: boolean;
  /** 勾选结果变化：父组件据此提交 `syncSelections`，并用于结果反馈里回显会话名 */
  onChange: (state: { selections: SessionSyncSelection[]; groups: SessionLinkPreviewGroup[] }) => void;
  /** 预览状态上报：父组件用于 tab 徽标与常驻提示，避免把错误藏进 tab 里 */
  onMetaChange?: (meta: SessionLinksMeta) => void;
}

/** 关联会话区块对父组件暴露的状态（tab 徽标 / 常驻提示用）。 */
export interface SessionLinksMeta {
  /** 该区块是否应渲染（国际版或存储不支持时为 false，父组件不渲染本 tab） */
  available: boolean;
  /** 可同步的会话数量（tab 徽标数字，0 时父组件不显示徽标） */
  groupCount: number;
  /** 预览请求失败的原因；非空时父组件在 tab 之上常驻提示 */
  error: string;
  /** 关联存储状态；unavailable 时父组件常驻提示原因 */
  storeStatus: SessionLinksPreview["storeStatus"] | null;
  storeError: string;
}

/** 判定结果的中文标签（与 core 的 verdict 一一对应，只表达状态，动作交给摘要句）。 */
const VERDICT_LABEL: Record<SessionSyncVerdict, string> = {
  fastForward: "可同步",
  diverge: "双方都有更新",
  ahead: "目标账号有更新",
  identical: "内容一致",
  unknown: "无法确认",
};

const VERDICT_BADGE: Record<SessionSyncVerdict, "success" | "warning" | "outline" | "secondary"> = {
  fastForward: "success",
  diverge: "warning",
  ahead: "warning",
  identical: "secondary",
  unknown: "outline",
};

/** 可勾选的模式：判定只允许一个模式，取后端给出的第一个。 */
function primaryMode(group: SessionLinkPreviewGroup): SessionSyncMode | null {
  return group.availableModes.length > 0 ? group.availableModes[0] : null;
}

function isActionable(group: SessionLinkPreviewGroup): boolean {
  return primaryMode(group) !== null && Boolean(group.previewToken);
}

/** 可直接同步项：全选只作用于这类会话，覆盖必须单独勾选。 */
function isDirectlySyncable(group: SessionLinkPreviewGroup): boolean {
  return isActionable(group) && primaryMode(group) === "fastForward";
}

/**
 * 组装提交给后端的同步选择：只有「用户勾选 + 后端给出模式与预览凭据」的组才发送。
 * 前端不推断模式，也不为禁选项补默认值。
 */
function buildSelections(
  groups: SessionLinkPreviewGroup[],
  checked: Set<string>,
): SessionSyncSelection[] {
  return groups.flatMap((group) => {
    if (!checked.has(group.groupId)) return [];
    const mode = primaryMode(group);
    if (!mode || !group.previewToken) return [];
    return [{ groupId: group.groupId, previewToken: group.previewToken, mode }];
  });
}

/** 卡片摘要句：一句人话说清发生了什么；条数与原始原因收进「查看详情」。 */
function summarySentence(group: SessionLinkPreviewGroup, targetLabel: string): string {
  switch (group.verdict) {
    case "fastForward":
      return `将当前账号的新内容同步到「${targetLabel}」`;
    case "diverge":
      return "勾选将用当前账号内容覆盖目标全文。";
    case "ahead":
      return "保留目标内容，本次不同步";
    case "identical":
      return "无需同步";
    case "unknown":
      return "暂时无法确认两边内容，本次不会同步";
  }
}

const RECORD_COUNT_HINT = "按会话内容的条数统计，不是对话轮数；条数相同也不代表内容顺序完全一致。";

/**
 * 切号弹窗「关联会话」tab 的内容：说明卡 + 会话列表（判定徽标 + 一句摘要 + 折叠详情）。
 *
 * - 默认勾选与可选模式全部来自后端：`defaultChecked` 为 true 才预先勾选，
 *   `availableModes` 为空（identical / ahead / unknown / 预览凭据不可用）一律禁选。
 * - `diverge` 默认不勾，需用户显式选择覆盖；覆盖风险常驻行内，不依赖展开或勾选。
 * - 错误与存储不可用由父组件在 tab 之上常驻提示，本组件只保留对应的空态与重试入口。
 */
export function SessionSyncSection({ account, open, disabled, onChange, onMetaChange }: Props) {
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState("");
  const [preview, setPreview] = useState<SessionLinksPreview | null>(null);
  /** 用户逐项勾选状态；仅在可勾选项上生效。 */
  const [checked, setChecked] = useState<Set<string>>(new Set());
  /** 手动重试计数：用于「重新检查」。 */
  const [reloadToken, setReloadToken] = useState(0);

  useEffect(() => {
    if (!open || !account) {
      setPreview(null);
      setError("");
      setChecked(new Set());
      setLoading(false);
      return;
    }
    let cancelled = false;
    setLoading(true);
    setError("");
    api
      .sessionLinksPreview(account.id, accountVariant(account))
      .then((res) => {
        if (cancelled) return;
        setPreview(res);
        // 默认勾选值来自后端 defaultChecked，前端不扩大权限。
        const defaults = new Set(
          res.groups.filter((group) => group.defaultChecked && isActionable(group)).map((g) => g.groupId),
        );
        setChecked(defaults);
        onChange({ selections: buildSelections(res.groups, defaults), groups: res.groups });
      })
      .catch((e) => {
        if (cancelled) return;
        setPreview(null);
        setError(api.asError(e));
        onChange({ selections: [], groups: [] });
      })
      .finally(() => {
        if (!cancelled) setLoading(false);
      });
    return () => {
      cancelled = true;
    };
    // 预览拉取只看「弹窗开关 / 目标账号 / 手动重试」；onChange 只做状态回写，
    // 不进依赖，否则父组件每次重渲染都会重新拉预览。
  }, [open, account, reloadToken]);

  const groups = preview?.groups ?? [];
  // 国际版能力判定不通过：整块不可用（后端执行时仍会强制检查能力）。
  const unsupported = Boolean(preview && (!preview.supported || preview.storeStatus === "unsupported"));
  const targetLabel = account?.nickname || account?.email || account?.uid || "目标账号";

  // 状态上报：父组件据此渲染 tab 徽标与常驻提示。
  useEffect(() => {
    onMetaChange?.({
      available: !unsupported,
      groupCount: groups.length,
      error,
      storeStatus: preview?.storeStatus ?? null,
      storeError: preview?.storeError ?? "",
    });
    // onMetaChange 只做状态回写，不进依赖。
  }, [unsupported, groups.length, error, preview?.storeStatus, preview?.storeError]);

  const syncable = groups.filter(isDirectlySyncable);
  const selectedCount = groups.filter((group) => isActionable(group) && checked.has(group.groupId)).length;
  const selectedSyncableCount = syncable.filter((group) => checked.has(group.groupId)).length;
  const allSyncableSelected = syncable.length > 0 && selectedSyncableCount === syncable.length;

  /** 全选/取消全选：只作用于可直接同步的会话；覆盖项必须单独勾选。 */
  function toggleAllSyncable() {
    const updated = new Set(checked);
    if (allSyncableSelected) syncable.forEach((group) => updated.delete(group.groupId));
    else syncable.forEach((group) => updated.add(group.groupId));
    setChecked(updated);
    onChange({ selections: buildSelections(groups, updated), groups });
  }

  function toggleGroup(group: SessionLinkPreviewGroup, next: boolean) {
    const updated = new Set(checked);
    if (next) updated.add(group.groupId);
    else updated.delete(group.groupId);
    setChecked(updated);
    onChange({ selections: buildSelections(groups, updated), groups });
  }

  if (unsupported) return null;

  const pending = loading || (!preview && !error);
  const storeUnavailable = preview?.storeStatus === "unavailable";

  return (
    <section className="space-y-3" aria-label="关联会话">
      <div className="flex items-start gap-3 rounded-md border bg-muted/30 px-3 py-3">
        <span className="flex size-8 shrink-0 items-center justify-center rounded-md border bg-background text-muted-foreground">
          <Link2 className="size-4" />
        </span>
        <div className="min-w-0 space-y-0.5">
          <div className="text-sm font-medium">什么是关联会话？</div>
          <p className="text-xs text-muted-foreground">
            通过本工具复制到其他账号的会话，会自动建立关联。切换时，可选择将当前账号的后续内容同步到对应会话。
          </p>
        </div>
      </div>

      {pending && (
        <div className="flex items-center gap-2 px-1 py-2 text-xs text-muted-foreground">
          <Loader2 className="size-3.5 animate-spin" />
          正在检查会话…
        </div>
      )}

      {!pending && (error || storeUnavailable) && (
        <div className="flex items-center justify-between gap-3 rounded-md border px-3 py-2.5">
          <span className="min-w-0 flex-1 text-xs text-muted-foreground">
            {error ? "暂时无法检查会话，本次不能同步" : "同步记录不可用，本次不会同步"}
          </span>
          <Button
            variant="outline"
            size="sm"
            className="shrink-0"
            disabled={disabled}
            onClick={() => setReloadToken((token) => token + 1)}
          >
            <RotateCw />
            重新检查
          </Button>
        </div>
      )}

      {!pending && !error && preview?.storeStatus === "missing" && (
        <p className="px-1 py-1 text-xs text-muted-foreground">
          还没有可以同步的会话：先在「复制会话」里复制一次，之后切换回来就能在这里同步新内容。
        </p>
      )}

      {!pending && !error && !storeUnavailable && preview?.storeStatus === "ready" && groups.length === 0 && (
        <p className="px-1 py-1 text-xs text-muted-foreground">
          这两个账号还没有共同复制过的会话（只处理双方都有的，不涉及其他账号）。
        </p>
      )}

      {!pending && !error && groups.length > 0 && (
        <div className="space-y-2">
          <div className="flex items-center gap-2.5 px-1">
            <Checkbox
              checked={allSyncableSelected ? true : selectedSyncableCount > 0 ? "indeterminate" : false}
              disabled={disabled || syncable.length === 0}
              onCheckedChange={() => toggleAllSyncable()}
              aria-label="全选可直接同步的会话"
            />
            <span className="min-w-0 flex-1 text-sm font-medium">全选可直接同步的会话</span>
            <span className="shrink-0 text-xs text-muted-foreground tabular-nums">
              {`已选 ${selectedCount} / ${groups.length}`}
            </span>
          </div>
          <div className="max-h-[min(24rem,50vh)] divide-y overflow-y-auto rounded-md border">
            {groups.map((group) => (
              <SessionLinkCard
                key={group.groupId}
                group={group}
                targetLabel={targetLabel}
                checked={checked.has(group.groupId) && isActionable(group)}
                disabled={disabled}
                onToggle={(next) => toggleGroup(group, next)}
              />
            ))}
          </div>
          <p className="px-1 text-xs text-muted-foreground">全选仅包含可直接同步的会话，覆盖需单独勾选。</p>
        </div>
      )}
    </section>
  );
}

/** 会话行：勾选框 + 标题 + 一句摘要 + 判定徽标 + 折叠箭头（路径 / 条数 / 原因）。 */
function SessionLinkCard({
  group,
  targetLabel,
  checked,
  disabled,
  onToggle,
}: {
  group: SessionLinkPreviewGroup;
  targetLabel: string;
  checked: boolean;
  disabled?: boolean;
  onToggle: (next: boolean) => void;
}) {
  const [open, setOpen] = useState(false);
  const canSelect = isActionable(group);
  const overwrite = primaryMode(group) === "overwrite";
  const title = group.title || "(无标题)";

  return (
    <Collapsible open={open} onOpenChange={setOpen} className="px-3 py-2.5">
      <div className="flex items-start gap-2.5">
        {canSelect ? (
          <Checkbox
            className="mt-0.5"
            checked={checked}
            disabled={disabled}
            onCheckedChange={(state) => onToggle(state === true)}
            aria-label={`同步会话 ${title}`}
          />
        ) : (
          <span className="mt-0.5 size-3.5 shrink-0" aria-hidden />
        )}
        <div className="min-w-0 flex-1 space-y-0.5">
          <div className="flex items-center gap-2">
            <span className="min-w-0 flex-1 truncate text-sm font-medium" title={group.title}>
              {title}
            </span>
            <Badge variant={VERDICT_BADGE[group.verdict]} className="shrink-0 text-[10px]">
              {VERDICT_LABEL[group.verdict]}
            </Badge>
            <CollapsibleTrigger asChild>
              <Button
                variant="ghost"
                size="icon"
                className="size-6 shrink-0 text-muted-foreground"
                aria-label="查看详情"
              >
                <ChevronRight className={cn("size-4 transition-transform", open && "rotate-90")} />
              </Button>
            </CollapsibleTrigger>
          </div>
          <p className="text-xs text-muted-foreground">{summarySentence(group, targetLabel)}</p>
          {overwrite && (
            <p className="flex items-start gap-1 text-xs text-amber-700 dark:text-amber-400">
              <CircleAlert className="mt-px size-3.5 shrink-0" />
              <span>{`目标独有 ${group.extraB} 条记录将被替换，无法通过本工具撤销。`}</span>
            </p>
          )}
        </div>
      </div>

      <CollapsibleContent className="space-y-1.5 pt-1 text-xs text-muted-foreground">
        {group.cwd && (
          <span className="block truncate" title={group.cwd}>
            {group.cwd}
          </span>
        )}
        <Tooltip>
          <TooltipTrigger asChild>
            <span className="block w-fit cursor-help" tabIndex={0}>
              {`内容条数：当前账号 ${group.recordCount.source} 条 · 目标账号 ${group.recordCount.target} 条`}
              {group.recordCount.baseline !== null && ` · 上次一致 ${group.recordCount.baseline} 条`}
              {group.extraB > 0 && ` · 目标账号独有 ${group.extraB} 条`}
            </span>
          </TooltipTrigger>
          <TooltipContent side="top" className="max-w-xs">
            {RECORD_COUNT_HINT}
          </TooltipContent>
        </Tooltip>
        <span className="block">{group.reason}</span>
      </CollapsibleContent>
    </Collapsible>
  );
}
