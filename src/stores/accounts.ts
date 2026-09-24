import { create } from "zustand";
import * as api from "@/lib/api";
import { DEFAULT_VARIANT, accountVariant, normalizeVariant } from "@/lib/variant";
import type { AccountMeta, AppStatus, BackupStatus, CreditExpiry, WbVariant } from "@/lib/types";

/**
 * 已去掉国际版：账号列表面向 UI 的唯一闸口。库里若还留着历史国际版包
 * （升级前导入的 `local-ai` 等），在这里被滤掉，不渲染、不参与切换/签到；
 * 文件仍在磁盘（不物理删凭据），需要恢复国际版时放开此过滤即可。
 */
function onlyDomestic(accounts: AccountMeta[]): AccountMeta[] {
  return accounts.filter((a) => accountVariant(a) === "cn");
}

/** In-flight credit fetches, shared so a remount does not start a second round. */
const creditInflight = new Set<string>();
/** 状态查询按档位各留一个在途请求，避免切档位时复用另一档位的结果。 */
let statusInflight: { variant: WbVariant; promise: Promise<AppStatus> } | undefined;

function fetchStatus(variant: WbVariant): Promise<AppStatus> {
  if (!statusInflight || statusInflight.variant !== variant) {
    const promise = api.getStatus(variant).finally(() => {
      if (statusInflight?.promise === promise) statusInflight = undefined;
    });
    statusInflight = { variant, promise };
  }
  return statusInflight.promise;
}

/**
 * 备份现状：读失败一律降级成 null 而不是抛。
 * 旧后端没有这个命令（未知端点），备份是护栏 —— 读不到护栏状态不该让账号页报错。
 */
async function fetchBackupStatus(): Promise<BackupStatus | null> {
  try {
    return await api.getBackupStatus();
  } catch {
    return null;
  }
}

async function fetchCreditExpiry(id: string): Promise<CreditExpiry> {
  try {
    return await api.getCreditExpiry(id);
  } catch (e) {
    return { ok: false, error: api.asError(e) };
  }
}

interface AccountsState {
  accounts: AccountMeta[];
  /**
   * 全局当前档位：状态、本机导入、轮询都以它为准。
   * 缺省国内版，因此在未引入档位切换时行为与改造前一致。
   */
  variant: WbVariant;
  status: AppStatus | null;
  /**
   * 自动备份现状。旧后端没有 `get_backup_status` 命令时为 null ——
   * 界面按"没有备份信息"处理，不显示恢复提示条，也不报错。
   */
  backup: BackupStatus | null;
  loading: boolean;
  error: string | null;
  creditMap: Record<string, CreditExpiry>;
  creditLoadingMap: Record<string, boolean>;
  /** 账号 id -> 最近一次积分查询完成时间（成功/失败都记录） */
  creditUpdatedAtMap: Record<string, number>;
  refreshingCredits: boolean;
  lastCreditRefreshAt: number;
  setVariant: (variant: WbVariant) => void;
  fetchAll: () => Promise<void>;
  refreshStatus: (signal?: AbortSignal) => Promise<void>;
  deleteAccount: (id: string) => Promise<void>;
  setAccountProxy: (id: string, proxy: string | null, variant?: WbVariant) => Promise<void>;
  /** Fetch credits only for ids not already cached. */
  ensureCredits: (accountIds: string[]) => Promise<void>;
  /** Force-refresh credits. `silent` skips toolbar/card loading flicker (timer). */
  refreshCredits: (accountIds: string[], opts?: { silent?: boolean }) => Promise<void>;
  importLocal: () => Promise<AccountMeta>;
  reconcileAccounts: () => Promise<void>;
}

export const useAccountsStore = create<AccountsState>((set, get) => ({
  accounts: [],
  variant: DEFAULT_VARIANT,
  status: null,
  backup: null,
  loading: false,
  error: null,
  creditMap: {},
  creditLoadingMap: {},
  creditUpdatedAtMap: {},
  refreshingCredits: false,
  lastCreditRefreshAt: 0,

  setVariant(variant) {
    const next = normalizeVariant(variant);
    if (get().variant === next) return;
    set({ variant: next });
    // 状态卡（运行中 / 当前账号 / 应用路径）随档位整体换一份。
    void get().fetchAll();
  },

  async fetchAll() {
    const variant = get().variant;
    set({ loading: true, error: null });
    try {
      const [status, { accounts }, backup] = await Promise.all([
        fetchStatus(variant),
        api.getAccounts(),
        fetchBackupStatus(),
      ]);
      // 迟到结果不得覆盖已切换档位的状态。
      if (get().variant !== variant) return;
      set({ status, accounts: onlyDomestic(accounts), backup, loading: false });
    } catch (e) {
      if (get().variant !== variant) return;
      set({ error: api.asError(e), loading: false });
    }
  },

  async refreshStatus(signal) {
    const variant = get().variant;
    try {
      const status = await fetchStatus(variant);
      if (!signal?.aborted && get().variant === variant) set({ status });
    } catch {
      // 后台探测失败时保留最后一次成功状态，下一轮轮询继续尝试。
    }
  },

  async deleteAccount(id: string) {
    await api.deleteAccount(id);
    creditInflight.delete(id);
    const { creditMap, creditLoadingMap, creditUpdatedAtMap } = get();
    const nextCredits = { ...creditMap };
    const nextLoading = { ...creditLoadingMap };
    const nextUpdatedAt = { ...creditUpdatedAtMap };
    delete nextCredits[id];
    delete nextLoading[id];
    delete nextUpdatedAt[id];
    // 删除会在后端触发一次"删前备份"，顺手刷新备份现状，让提示条能立刻出现。
    const backup = await fetchBackupStatus();
    set({
      accounts: get().accounts.filter((a) => a.id !== id),
      creditMap: nextCredits,
      creditLoadingMap: nextLoading,
      creditUpdatedAtMap: nextUpdatedAt,
      backup,
    });
  },

  async setAccountProxy(id: string, proxy: string | null, variant?: WbVariant) {
    const res = await api.setAccountProxy(id, proxy, variant);
    // 代理变了，之前用旧代理拉到的积分结果就不再可信 —— 清掉该账号的积分缓存，
    // 否则卡片会一直显示旧代理下的失败态（最长到下一次定时刷新）。
    const nextCredits = { ...get().creditMap };
    const nextUpdatedAt = { ...get().creditUpdatedAtMap };
    delete nextCredits[id];
    delete nextUpdatedAt[id];
    set({
      accounts: get().accounts.map((a) => (a.id === id ? res.account : a)),
      creditMap: nextCredits,
      creditUpdatedAtMap: nextUpdatedAt,
    });
  },

  async ensureCredits(accountIds) {
    await loadCredits(accountIds, false, false);
  },

  async refreshCredits(accountIds, opts) {
    await loadCredits(accountIds, true, opts?.silent === true);
  },

  async importLocal() {
    // 本机导入按当前档位读取对应登录态文件。
    const res = await api.importLocal(get().variant);
    await get().reconcileAccounts();
    return res.account;
  },

  async reconcileAccounts() {
    // 账号列表与备份现状一起刷新：认领/导入都会立刻产生一份新备份，
    // 提示条的状态必须跟着走。
    const [{ accounts }, backup] = await Promise.all([api.getAccounts(), fetchBackupStatus()]);
    set({ accounts: onlyDomestic(accounts), backup });
  },
}));

async function loadCredits(accountIds: string[], force: boolean, silent: boolean) {
  const ids = [...new Set(accountIds.filter(Boolean))];
  if (ids.length === 0) return;

  const state = useAccountsStore.getState();
  const toFetch = force
    ? ids
    : ids.filter((id) => state.creditMap[id] === undefined && !creditInflight.has(id));
  if (toFetch.length === 0) return;

  for (const id of toFetch) creditInflight.add(id);
  /** 本轮是否命中「账号包已不在库里」，命中则收尾时重拉一次列表。 */
  let sawMissingAccount = false;
  if (!silent) {
    useAccountsStore.setState((s) => {
      const creditLoadingMap = { ...s.creditLoadingMap };
      for (const id of toFetch) creditLoadingMap[id] = true;
      return {
        creditLoadingMap,
        refreshingCredits: force ? true : s.refreshingCredits,
      };
    });
  }

  await Promise.all(
    toFetch.map(async (id) => {
      const result = await fetchCreditExpiry(id);
      creditInflight.delete(id);
      if (result.accountMissing) sawMissingAccount = true;
      useAccountsStore.setState((s) => ({
        creditMap: { ...s.creditMap, [id]: result },
        creditUpdatedAtMap: { ...s.creditUpdatedAtMap, [id]: Date.now() },
        creditLoadingMap: silent ? s.creditLoadingMap : { ...s.creditLoadingMap, [id]: false },
      }));
    }),
  );

  // 账号包在列表加载之后被删/被移走时，卡片已经是"幽灵"：按 id 的操作只会一直失败，
  // 界面上留一条用户无法处置的报错。重拉一次列表把它清掉。
  // 不会形成「查询 → 重拉 → 再查询」的循环：包没了，重拉后的列表里不再有这个 id。
  if (sawMissingAccount) {
    try {
      await useAccountsStore.getState().reconcileAccounts();
    } catch {
      // 列表刷新失败不该盖掉已经拿到的积分结果，下一轮刷新会再试。
    }
  }

  useAccountsStore.setState((s) => ({
    lastCreditRefreshAt: Date.now(),
    refreshingCredits: silent ? s.refreshingCredits : force ? false : s.refreshingCredits,
  }));
}
