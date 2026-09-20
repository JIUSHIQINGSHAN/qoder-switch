import { cn } from "@/lib/utils";
import qoderMark from "@/assets/qoder-mark.png";

const appIconUrl = `${import.meta.env.BASE_URL}icon-transparent.png`;

interface MarkProps {
  size?: number;
  className?: string;
}

/**
 * Qoder 桌面客户端标记。
 *
 * 源图不是手画的：取自本机安装目录 `<安装根>\resources\app-icon.ico` 的 256px 帧
 * （ICO 内嵌的是 PNG，魔数得按大端读才认得出）。这里用**国际版那帧无角标的图**：
 * 国内版的图标右上角烤进了 "CN" 字样，再叠 IntlBadge 就会两个角标打架。
 * 档位区分完全交给 IntlBadge。
 */
export function WorkBuddyMark({ size = 32, className }: MarkProps) {
  return (
    <span
      aria-hidden
      className={cn("relative inline-flex shrink-0 overflow-hidden rounded-[22%]", className)}
      style={{ width: size, height: size }}
    >
      <img
        src={qoderMark}
        alt=""
        className="absolute left-1/2 top-1/2 size-full max-w-none -translate-x-1/2 -translate-y-1/2 object-cover"
      />
    </span>
  );
}

/**
 * 档位角标：复用官方图标 + 角标区分档位，不新画图形，
 * 保持与既有 Qoder / Qoder 标记同一套圆角与配色。
 * 角标文案为 `INTL`（4 字符，比圆形 badge 宽），故按内容撑成胶囊并收紧字号。
 * Qoder 与 Qoder IDE 的档位标记共用这一份实现。
 */
function IntlBadge({ size }: { size: number }) {
  const badge = Math.max(11, Math.round(size * 0.46));
  return (
    <span
      className="absolute -bottom-0.5 -right-0.5 inline-flex items-center justify-center rounded-full border border-card bg-foreground px-[3px] font-semibold leading-none text-background"
      style={{ minWidth: badge, height: badge, fontSize: Math.max(6, Math.round(badge * 0.5)) }}
    >
      INTL
    </span>
  );
}

/** Qoder 国际版标记。 */
export function WorkBuddyAiMark({ size = 32, className }: MarkProps) {
  return (
    <span
      aria-hidden
      className={cn("relative inline-flex shrink-0", className)}
      style={{ width: size, height: size }}
    >
      <WorkBuddyMark size={size} />
      <IntlBadge size={size} />
    </span>
  );
}

/** 应用自身的透明角色图标；桌面安装图标仍使用 public/icon.png。 */
export function AppIconMark({ size = 32, className }: MarkProps) {
  return (
    <span
      aria-hidden
      className={cn("inline-flex shrink-0", className)}
      style={{ width: size, height: size }}
    >
      <img src={appIconUrl} alt="" className="size-full object-contain" />
    </span>
  );
}

export function CodeBuddyMark({ size = 32, className }: MarkProps) {
  const icon = Math.max(10, Math.round(size));
  return (
    <span
      aria-hidden
      className={cn(
        "inline-flex shrink-0 items-center justify-center rounded-[22%] border border-white/10 bg-zinc-950 text-zinc-50 shadow-sm",
        className,
      )}
      style={{ width: size, height: size, fontSize: icon }}
    >
      <svg
        viewBox="0 0 24 24"
        fill="none"
        stroke="currentColor"
        strokeWidth="2.2"
        strokeLinecap="round"
        strokeLinejoin="round"
        className="size-[1em]"
      >
        <path d="M4.4 7.4 10.4 12 4.4 16.6" />
        <path d="M13 16.6h7" />
      </svg>
    </span>
  );
}

/**
 * Qoder IDE（桌面客户端）标记。
 *
 * 与上面同一份图：Qoder 侧"桌面客户端"和"IDE"指的是同一个客户端程序，
 * 所以两个标记位画同一个官方图标是准确的，不要为了看起来不同而另画图。
 */
export function CodeBuddyCnIdeMark({ size = 32, className }: MarkProps) {
  return (
    <span
      aria-hidden
      className={cn("relative inline-flex shrink-0 overflow-hidden", className)}
      style={{ width: size, height: size }}
    >
      <img
        src={qoderMark}
        alt=""
        className="absolute left-1/2 top-1/2 size-full max-w-none -translate-x-1/2 -translate-y-1/2 object-cover"
      />
    </span>
  );
}

/**
 * Qoder IDE 国际版标记：同一官方图标 + INTL 角标。
 * 依据 `variantUsesIntlCodebuddyIde()` —— 国际版档位下 IDE 切的是 Qoder.app，
 * 与国内版的 Qoder CN 是两个客户端，故用同一套角标区分。
 */
export function CodeBuddyAiIdeMark({ size = 32, className }: MarkProps) {
  return (
    <span
      aria-hidden
      className={cn("relative inline-flex shrink-0", className)}
      style={{ width: size, height: size }}
    >
      <CodeBuddyCnIdeMark size={size} />
      <IntlBadge size={size} />
    </span>
  );
}

export function StatusDot({ on, className }: { on: boolean; className?: string }) {
  return (
    <span
      aria-hidden
      className={cn("size-1.5 shrink-0 rounded-full", on ? "bg-primary" : "bg-muted-foreground/35", className)}
    />
  );
}
