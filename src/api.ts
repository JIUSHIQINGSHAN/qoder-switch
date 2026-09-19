import { invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import type {
  AxisStatus,
  Bundle,
  Journal,
  Preview,
  QoderTarget,
  QoderVariant,
  SnapshotReport,
} from "./types";

export const api = {
  probeAll: () => invoke<AxisStatus[]>("probe_all"),
  listAccounts: () => invoke<Bundle[]>("list_accounts"),
  capture: (account_id: string, variant: QoderVariant, target: QoderTarget) =>
    invoke<Bundle>("capture", { account_id, variant, target }),
  preview: (account_id: string, variant: QoderVariant, target: QoderTarget) =>
    invoke<Preview>("preview", { account_id, variant, target }),
  switchNow: (
    account_id: string,
    variant: QoderVariant,
    target: QoderTarget,
    restart: boolean,
    forced: boolean,
  ) => invoke<Journal>("switch_now", { account_id, variant, target, restart, forced }),
  unfinished: () => invoke<Journal[]>("unfinished"),
  recover: (journal: Journal) => invoke<string>("recover", { journal }),
  snapshotNow: () => invoke<SnapshotReport>("snapshot_now"),
  storeDir: () => invoke<string>("store_dir"),
  exportText: (account_id: string) => invoke<string>("export_account_text", { account_id }),
  importText: (text: string, overwrite: boolean) =>
    invoke<{ written: string[]; skipped: string[] }>("import_account_text", { text, overwrite }),
  onSwitchProgress: (cb: (msg: string) => void): Promise<UnlistenFn> =>
    listen<string>("switch-progress", (e) => cb(e.payload)),
};

/// 剩余天数。解析不了就返回 null，界面显示 "-"，不猜。
export function daysLeft(iso?: string | null): number | null {
  if (!iso) return null;
  const t = Date.parse(iso);
  if (Number.isNaN(t)) return null;
  return Math.floor((t - Date.now()) / 86_400_000);
}

/// 把 hosted 的三种 serde 形状收成一句人话。
export function hostedText(h: AxisStatus["hosted"]): { text: string; level: "ok" | "warn" | "bad" } {
  if (h === "No") return { text: "可安全终止目标", level: "ok" };
  if (typeof h === "object" && "Yes" in h) return { text: `会连同本会话终止 —— 已拒绝：${h.Yes}`, level: "bad" };
  if (typeof h === "object" && "Unknown" in h) return { text: `判不出来，同样拒绝执行：${h.Unknown}`, level: "warn" };
  return { text: "未知形状", level: "warn" };
}
