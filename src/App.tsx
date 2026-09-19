import { useCallback, useEffect, useMemo, useState } from "react";
import { api, daysLeft, hostedText } from "./api";
import type {
  AxisStatus,
  Bundle,
  Journal,
  Preview,
  QoderTarget,
  QoderVariant,
  SnapshotReport,
} from "./types";

const VARIANTS: QoderVariant[] = ["cn", "global"];
const TARGETS: QoderTarget[] = ["desktop", "cli", "work"];
const TARGET_CN: Record<QoderTarget, string> = {
  desktop: "桌面客户端",
  cli: "CLI",
  work: "QoderWork",
};
const VARIANT_CN: Record<QoderVariant, string> = { cn: "国内版", global: "国际版" };

const key = (b: { account_id: string; variant: QoderVariant; target: QoderTarget }) =>
  `${b.account_id}|${b.variant}|${b.target}`;

export default function App() {
  const [axes, setAxes] = useState<AxisStatus[]>([]);
  const [bundles, setBundles] = useState<Bundle[]>([]);
  const [selected, setSelected] = useState<string>("");
  const [pv, setPv] = useState<Preview | null>(null);
  const [stranded, setStranded] = useState<Journal[]>([]);
  const [logs, setLogs] = useState<string[]>([]);
  const [snap, setSnap] = useState<SnapshotReport | null>(null);
  const [storeDir, setStoreDir] = useState("");
  const [draftId, setDraftId] = useState("");
  const [draftVariant, setDraftVariant] = useState<QoderVariant>("cn");
  const [draftTarget, setDraftTarget] = useState<QoderTarget>("desktop");
  const [restart, setRestart] = useState(true);
  const [forced, setForced] = useState(false);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState("");
  const [exportText, setExportText] = useState("");
  const [importText, setImportText] = useState("");
  const [importOverwrite, setImportOverwrite] = useState(false);

  const log = useCallback((m: string) => {
    setLogs((prev) => [...prev.slice(-199), `${new Date().toLocaleTimeString()} ${m}`]);
  }, []);

  const refresh = useCallback(async () => {
    try {
      const [a, b, u, dir] = await Promise.all([
        api.probeAll(),
        api.listAccounts(),
        api.unfinished(),
        api.storeDir(),
      ]);
      setAxes(a);
      setBundles(b);
      setStranded(u);
      setStoreDir(dir);
      setError("");
    } catch (e) {
      setError(String(e));
    }
  }, []);

  useEffect(() => {
    void refresh();
    let un: (() => void) | undefined;
    api
      .onSwitchProgress((m) => log(`进度 · ${m}`))
      .then((f) => {
        un = f;
      })
      .catch((e) => log(`事件订阅失败：${e}`));
    return () => un?.();
  }, [refresh, log]);

  const chosen = useMemo(() => bundles.find((b) => key(b) === selected) ?? null, [bundles, selected]);

  // 临期优先排列：token 快过期的账号排前面，无到期信息的落到最后。
  const ordered = useMemo(() => {
    const rank = (b: Bundle) => {
      const d = daysLeft(b.identity.expires_at);
      return d === null ? Number.MAX_SAFE_INTEGER : d;
    };
    return [...bundles].sort((a, b) => rank(a) - rank(b));
  }, [bundles]);

  useEffect(() => {
    if (!chosen) {
      setPv(null);
      return;
    }
    api
      .preview(chosen.account_id, chosen.variant, chosen.target)
      .then((p) => {
        setPv(p);
        setError("");
      })
      .catch((e) => setError(String(e)));
  }, [chosen]);

  async function run<T>(what: string, fn: () => Promise<T>, after?: (v: T) => void) {
    setBusy(true);
    setError("");
    try {
      const v = await fn();
      log(what);
      after?.(v);
      await refresh();
    } catch (e) {
      setError(String(e));
      log(`${what} 失败：${e}`);
    } finally {
      setBusy(false);
    }
  }

  const axisOf = (v: QoderVariant, t: QoderTarget) =>
    axes.find((a) => a.variant === v && a.target === t);

  const blocked = chosen ? hostedText(axisOf(chosen.variant, chosen.target)?.hosted ?? "No").level !== "ok" : true;

  return (
    <div className="app">
      <header>
        <div>
          <h1>Qoder Switch</h1>
          <p className="sub">
            多账号凭据包切换 · 账号库存于 <code>{storeDir || "~/.qs-switch"}</code>
          </p>
        </div>
        <div className="row">
          <button disabled={busy} onClick={() => run("快照已比对", api.snapshotNow, setSnap)}>
            快照比对
          </button>
          <button disabled={busy} onClick={() => void refresh()}>
            刷新
          </button>
        </div>
      </header>

      {error && <div className="err">{error}</div>}

      <section className="banner">
        <h2>现场探针</h2>
        <table className="tbl">
          <thead>
            <tr>
              <th>版本</th>
              <th>目标</th>
              <th>镜像名</th>
              <th>在跑</th>
              <th>凭据文件</th>
              <th>托管判定</th>
            </tr>
          </thead>
          <tbody>
            {axes.map((a) => {
              const h = hostedText(a.hosted);
              const have = a.credentials.filter((c) => c.exists).length;
              return (
                <tr key={key({ account_id: a.variant, variant: a.variant, target: a.target })}>
                  <td>{VARIANT_CN[a.variant]}</td>
                  <td>{TARGET_CN[a.target]}</td>
                  <td className="mono">{a.images.join(" + ")}</td>
                  <td>{a.running_pids.length}</td>
                  <td>
                    {have}/{a.credentials.length}
                  </td>
                  <td className={`lvl-${h.level}`}>{h.text}</td>
                </tr>
              );
            })}
          </tbody>
        </table>
        <p className="hint">
          判定为「会连同本会话终止」或「判不出来」时，正常档一律拒绝杀进程 —— 从资源管理器独立启动
          qoder-switch 才会得到可安全终止的结论。
        </p>
      </section>

      <div className="cols">
        <section>
          <h2>已认领账号</h2>
          <div className="row capture">
            <input
              placeholder="账号名，如 work-1"
              value={draftId}
              onChange={(e) => setDraftId(e.target.value)}
            />
            <select value={draftVariant} onChange={(e) => setDraftVariant(e.target.value as QoderVariant)}>
              {VARIANTS.map((v) => (
                <option key={v} value={v}>
                  {VARIANT_CN[v]}
                </option>
              ))}
            </select>
            <select value={draftTarget} onChange={(e) => setDraftTarget(e.target.value as QoderTarget)}>
              {TARGETS.map((t) => (
                <option key={t} value={t}>
                  {TARGET_CN[t]}
                </option>
              ))}
            </select>
            <button
              disabled={busy || !draftId.trim()}
              onClick={() =>
                run(`已认领 ${draftId.trim()} · ${TARGET_CN[draftTarget]}`, () =>
                  api.capture(draftId.trim(), draftVariant, draftTarget),
                )
              }
            >
              认领当前登录态
            </button>
          </div>
          <table className="tbl">
            <thead>
              <tr>
                <th>账号名</th>
                <th>版本·目标</th>
                <th>身份</th>
                <th>token 到期</th>
                <th>文件</th>
                <th>认领时间</th>
              </tr>
            </thead>
            <tbody>
              {bundles.length === 0 && (
                <tr>
                  <td colSpan={6} className="hint">
                    还没有任何账号包。先在 Qoder 里登录一个账号，再点「认领当前登录态」。
                  </td>
                </tr>
              )}
              {ordered.map((b) => {
                const d = daysLeft(b.identity.expires_at);
                return (
                <tr
                  key={key(b)}
                  className={key(b) === selected ? "sel" : ""}
                  onClick={() => setSelected(key(b))}
                >
                  <td>{b.account_id}</td>
                  <td>
                    {VARIANT_CN[b.variant]}·{TARGET_CN[b.target]}
                  </td>
                  <td>{b.identity.email ?? b.identity.name ?? "-"}</td>
                  <td
                    className={
                      d === null ? "hint" : d <= 7 ? "lvl-bad" : d <= 21 ? "lvl-warn" : ""
                    }
                    title={b.identity.expires_at ?? "未解密，无到期时间"}
                  >
                    {d === null ? "—" : `${d} 天`}
                  </td>
                  <td>{b.members.length}</td>
                  <td className="mono">{b.created_at}</td>
                </tr>
                );
              })}
            </tbody>
          </table>

          <h2>导出 / 导入账号包</h2>
          <div className="row">
            <button
              disabled={busy || !chosen}
              onClick={() => {
                if (!chosen) return;
                void run(
                  `已导出 ${chosen.account_id} 的账号包`,
                  () => api.exportText(chosen.account_id),
                  setExportText,
                );
              }}
            >
              导出所选账号
            </button>
            <button
              disabled={!exportText}
              onClick={() => {
                navigator.clipboard
                  ?.writeText(exportText)
                  .then(() => log("导出文本已复制到剪贴板"))
                  .catch(() => log("复制失败，请手动全选文本框内容"));
              }}
            >
              复制
            </button>
          </div>
          {exportText && (
            <textarea
              className="io"
              rows={4}
              readOnly
              value={exportText}
              onFocus={(e) => e.currentTarget.select()}
            />
          )}
          <textarea
            className="io"
            rows={3}
            placeholder="把另一台机器导出的 JSON 粘到这里"
            value={importText}
            onChange={(e) => setImportText(e.target.value)}
          />
          <div className="row">
            <label>
              <input
                type="checkbox"
                checked={importOverwrite}
                onChange={(e) => setImportOverwrite(e.target.checked)}
              />
              允许覆盖同名分片
            </label>
            <button
              disabled={busy || !importText.trim()}
              onClick={() =>
                run("账号包导入完成", () => api.importText(importText, importOverwrite), (r) => {
                  r.written.forEach((w) => log(`导入 · ${w}`));
                  r.skipped.forEach((s) => log(`跳过 · ${s}`));
                  if (r.written.length) setImportText("");
                })
              }
            >
              导入
            </button>
          </div>
          <p className="hint">
            导出的是凭据文件的密文副本，受 DPAPI（按 Windows 用户）保护：换机器或换
            Windows 账号后导入会静默变成未登录，只能在同一 Windows 用户内搬运。
          </p>

          {stranded.length > 0 && (
            <>
              <h2>待恢复的切换</h2>
              {stranded.map((j) => (
                <div className="stranded" key={j.id}>
                  <div>
                    <b>{j.account_id}</b> · {j.phase} · <code>{j.backup_dir}</code>
                    {j.note && <div className="hint">{j.note}</div>}
                  </div>
                  <button
                    disabled={busy}
                    onClick={() => run(`已回滚 ${j.id}`, () => api.recover(j))}
                  >
                    退回切换前
                  </button>
                </div>
              ))}
            </>
          )}
        </section>

        <section>
          <h2>切换面板</h2>
          {!chosen && <p className="hint">左侧选一个账号包。</p>}
          {chosen && pv && (
            <>
              <div className="kv">
                <span>目标</span>
                <b>
                  {VARIANT_CN[chosen.variant]} · {TARGET_CN[chosen.target]}
                </b>
                <span>写回文件</span>
                <b>{pv.writes.join(", ") || "无"}</b>
                <span>在跑进程</span>
                <b>{axisOf(chosen.variant, chosen.target)?.running_pids.length ?? 0}</b>
                <span>托管判定</span>
                <b className={`lvl-${hostedText(axisOf(chosen.variant, chosen.target)?.hosted ?? "No").level}`}>
                  {hostedText(axisOf(chosen.variant, chosen.target)?.hosted ?? "No").text}
                </b>
              </div>

              {pv.warnings.length > 0 && (
                <ul className="warns">
                  {pv.warnings.map((w, i) => (
                    <li key={i}>{w}</li>
                  ))}
                </ul>
              )}

              <table className="tbl">
                <thead>
                  <tr>
                    <th>角色</th>
                    <th>路径</th>
                    <th>此刻</th>
                    <th>本次写回</th>
                  </tr>
                </thead>
                <tbody>
                  {pv.layout.map(([role, path, exists]) => (
                    <tr key={role}>
                      <td className="mono">{role}</td>
                      <td className="mono path" title={path}>
                        {path}
                      </td>
                      <td>{exists ? "存在" : "缺"}</td>
                      <td>{pv.writes.includes(role) ? "是" : "—"}</td>
                    </tr>
                  ))}
                </tbody>
              </table>

              <div className="row switch">
                <label>
                  <input type="checkbox" checked={restart} onChange={(e) => setRestart(e.target.checked)} />
                  换完重启目标
                </label>
                <label className={blocked ? "bad" : ""}>
                  <input type="checkbox" checked={forced} onChange={(e) => setForced(e.target.checked)} />
                  强制档（明知会连同本会话终止）
                </label>
                <button
                  className="primary"
                  disabled={busy || (blocked && !forced)}
                  onClick={() =>
                    run(
                      `已切到 ${chosen.account_id} · ${TARGET_CN[chosen.target]}`,
                      () => api.switchNow(chosen.account_id, chosen.variant, chosen.target, restart, forced),
                    )
                  }
                >
                  切换到此账号
                </button>
              </div>
              {blocked && !forced && (
                <p className="hint">
                  当前判定不允许杀进程。可以只换文件不关进程吗？不行 —— 桌面端在会话期会持续重写
                  auth 文件，不先终止就等于白写。
                </p>
              )}
            </>
          )}

          {snap && (
            <>
              <h2>最近一次快照比对</h2>
              <p className="hint">
                {snap.taken_at} · {snap.path}
              </p>
              {snap.changes.length === 0 ? (
                <p className="hint">与上一张无差异。</p>
              ) : (
                <table className="tbl">
                  <thead>
                    <tr>
                      <th>变化</th>
                      <th>版本·目标</th>
                      <th>角色</th>
                      <th>性质</th>
                    </tr>
                  </thead>
                  <tbody>
                    {snap.changes.map((c, i) => (
                      <tr key={i}>
                        <td>{c.kind}</td>
                        <td>
                          {VARIANT_CN[c.variant]}·{TARGET_CN[c.target]}
                        </td>
                        <td className="mono">{c.role}</td>
                        <td>{c.critical ? "换号必替" : "观测"}</td>
                      </tr>
                    ))}
                  </tbody>
                </table>
              )}
            </>
          )}
        </section>
      </div>

      <section className="logs">
        <h2>操作日志</h2>
        <pre>{logs.join("\n") || "（暂无）"}</pre>
      </section>
    </div>
  );
}
