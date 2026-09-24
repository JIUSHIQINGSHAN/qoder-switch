//! 签到台账与积分快照：Qoder 服务端既没有签到日志也没有用量统计，
//! 这两类数据全部由本机记录（`~/.qs-switch` 下三份 JSON），并据此聚合出
//! 前端统计页（CreditStatistics 契约）与自动签到调度。
//!
//! 不打印任何凭据：这里只落账号 id / 邮箱 / 数值与结果枚举。

use std::collections::{BTreeMap, HashSet};
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use serde_json::{json, Value};

use crate::modules::auth_codec;
use crate::modules::bundle;
use crate::modules::config::{atomic_write_bytes, PathRoots};
use crate::modules::quota;
use crate::modules::variant::{QoderTarget, QoderVariant};

pub const RETENTION_DAYS: i64 = 30;
const LOGS_CAP: usize = 500;
const SNAPSHOTS_CAP: usize = 5000;
/// 同一账号两条配额快照的最小间隔：账号页会轮询/刷新积分，不节流会把
/// 快照文件写成请求日志。10 分钟足够统计页画日粒度趋势。
const SNAPSHOT_MIN_GAP_SECS: i64 = 600;

static LEDGER_GATE: Mutex<()> = Mutex::new(());

fn checkin_config_path(store: &Path) -> PathBuf {
    store.join("checkin-config.json")
}

fn checkin_logs_path(store: &Path) -> PathBuf {
    store.join("checkin-logs.json")
}

fn snapshots_path(store: &Path) -> PathBuf {
    store.join("credit-snapshots.jsonl")
}

/// 前端 `CheckinConfig` 契约（snake_case 沿用上游）。缺省关闭 —— 自动打网络请求
/// 的开关必须默认关闭。惰性刷新给 6 小时这个可用缺省，不是 0：0 会让设置页刚打开
/// 就显示"每小时无条件刷新"这种从来没发生过的语义。
///
/// 上游还带过 `keepalive_days`（保活天数）与 `start_hour`/`end_hour`，本项目的调度
/// 从未读取过它们。签到是每日一次的幂等领取，「今天签没签」由本机日志即可判定，
/// 不需要第二个时间维度 —— 因此只持久化真正生效的字段，不再留下永不生效的开关。
pub fn read_checkin_config(store: &Path) -> Value {
    let raw: Value = std::fs::read(checkin_config_path(store))
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or_else(|| json!({}));
    json!({
        "enabled": raw.get("enabled").and_then(|x| x.as_bool()).unwrap_or(false),
        "lazy_refresh_hours": raw.get("lazy_refresh_hours").and_then(|x| x.as_u64()).unwrap_or(6),
    })
}

/// 合并部分字段并落盘。数值按前端输入的 min/max 收口，负数/超界不会写进文件。
pub fn write_checkin_config(store: &Path, patch: &Value) -> crate::Result<Value> {
    let _gate = LEDGER_GATE.lock().unwrap_or_else(|p| p.into_inner());
    let mut cur = read_checkin_config(store);
    if let Some(b) = patch.get("enabled").and_then(|x| x.as_bool()) {
        cur["enabled"] = json!(b);
    }
    if let Some(n) = patch.get("lazy_refresh_hours").and_then(|x| x.as_u64()) {
        cur["lazy_refresh_hours"] = json!(n.clamp(1, 72));
    }
    let text = serde_json::to_vec_pretty(&cur).map_err(|e| e.to_string())?;
    atomic_write_bytes(&checkin_config_path(store), &text)
        .map_err(|e| format!("写签到配置失败: {e}"))?;
    Ok(cur)
}

/// 一条签到日志。`result` 只会是 success / already / error；inactive（官方未开放）
/// 什么都没发生，不记 —— 否则自动签到的每次轮询都会刷出一条噪声。
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct CheckinLogEntry {
    pub ts: i64,
    #[serde(default)]
    pub account_id: Option<String>,
    #[serde(default)]
    pub email: String,
    pub result: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    #[serde(default)]
    pub variant: String,
}

fn read_logs_raw(store: &Path) -> Vec<CheckinLogEntry> {
    std::fs::read(checkin_logs_path(store))
        .ok()
        .and_then(|b| serde_json::from_slice::<Vec<CheckinLogEntry>>(&b).ok())
        .unwrap_or_default()
}

fn write_logs_raw(store: &Path, logs: &[CheckinLogEntry]) -> std::io::Result<()> {
    let text = serde_json::to_vec_pretty(logs)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e))?;
    atomic_write_bytes(&checkin_logs_path(store), &text)
}

/// 记录一条签到结果（新的在前；30 天外与超量裁剪）。
pub fn record_checkin_log(
    store: &Path,
    account_id: &str,
    email: &str,
    variant: QoderVariant,
    result: &str,
    error: Option<&str>,
) {
    let _gate = LEDGER_GATE.lock().unwrap_or_else(|p| p.into_inner());
    let mut logs = read_logs_raw(store);
    logs.insert(
        0,
        CheckinLogEntry {
            ts: chrono::Utc::now().timestamp_millis(),
            account_id: Some(account_id.to_string()),
            email: email.to_string(),
            result: result.to_string(),
            error: error.map(String::from),
            variant: super::view::variant_key(variant).to_string(),
        },
    );
    let cutoff = chrono::Utc::now().timestamp_millis() - RETENTION_DAYS * 86_400_000;
    logs.retain(|l| l.ts >= cutoff);
    logs.truncate(LOGS_CAP);
    let _ = write_logs_raw(store, &logs);
}

/// 前端 `get_checkin_logs` 契约：`{ logs: [...] }`。
pub fn read_checkin_logs(store: &Path) -> Value {
    json!({ "logs": read_logs_raw(store) })
}

/// 一条配额快照（jsonl 行）。
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct CreditSnapshot {
    pub ts: i64,
    #[serde(default)]
    pub account_id: String,
    #[serde(default)]
    pub email: String,
    #[serde(default)]
    pub variant: String,
    #[serde(default)]
    pub total_capacity: f64,
    #[serde(default)]
    pub total_remaining: f64,
    #[serde(default)]
    pub expiring_soon_remaining: f64,
    #[serde(default)]
    pub soonest_expire_at: Option<i64>,
}

/// 配额获取成功后落一条快照。同一账号 10 分钟内只落一条（按文件内最近一条判）。
pub fn append_credit_snapshot(store: &Path, snap: &CreditSnapshot) {
    let _gate = LEDGER_GATE.lock().unwrap_or_else(|p| p.into_inner());
    let path = snapshots_path(store);
    let last_for_account = read_snapshots_raw(&path)
        .into_iter()
        .rev()
        .find(|s| s.account_id == snap.account_id);
    if let Some(prev) = last_for_account {
        if snap.ts - prev.ts < SNAPSHOT_MIN_GAP_SECS * 1000 {
            return;
        }
    }
    let mut all = read_snapshots_raw(&path);
    all.push(snap.clone());
    let cutoff = chrono::Utc::now().timestamp_millis() - RETENTION_DAYS * 86_400_000;
    all.retain(|s| s.ts >= cutoff);
    if all.len() > SNAPSHOTS_CAP {
        all = all.split_off(all.len() - SNAPSHOTS_CAP);
    }
    let mut buf = Vec::new();
    for s in &all {
        if let Ok(line) = serde_json::to_string(s) {
            buf.extend_from_slice(line.as_bytes());
            buf.push(b'\n');
        }
    }
    let _ = atomic_write_bytes(&path, &buf);
}

fn read_snapshots_raw(path: &Path) -> Vec<CreditSnapshot> {
    let Ok(f) = std::fs::File::open(path) else {
        return Vec::new();
    };
    BufReader::new(f)
        .lines()
        .map_while(|l| l.ok())
        .filter_map(|l| serde_json::from_str(&l).ok())
        .collect()
}

fn read_snapshots(store: &Path) -> Vec<CreditSnapshot> {
    read_snapshots_raw(&snapshots_path(store))
}

/// `-0.0` 会被 `Intl.NumberFormat` 渲染成 `"-0"`：0 就是 0，不该带符号。
/// 配额 API 里出现过 -0 形态（实测 2026-09-21），所以两份宿主输出前统一走一遍。
pub fn normalize_signed_zeros(v: &mut Value) {
    match v {
        Value::Number(n) => {
            if let Some(f) = n.as_f64() {
                if f == 0.0 && f.is_sign_negative() {
                    *v = Value::from(0.0);
                }
            }
        }
        Value::Array(items) => items.iter_mut().for_each(normalize_signed_zeros),
        Value::Object(map) => map.values_mut().for_each(normalize_signed_zeros),
        _ => {}
    }
}

fn date_key_ms(ts: i64) -> String {
    use chrono::TimeZone;
    chrono::Local
        .timestamp_millis_opt(ts)
        .single()
        .map(|d| d.format("%Y-%m-%d").to_string())
        .unwrap_or_default()
}

fn today_key() -> String {
    chrono::Local::now().format("%Y-%m-%d").to_string()
}

fn minus_days(today: &str, days: i32) -> String {
    use chrono::NaiveDate;
    NaiveDate::parse_from_str(today, "%Y-%m-%d")
        .ok()
        .and_then(|d| d.checked_sub_signed(chrono::Duration::days(days as i64)))
        .map(|d| d.format("%Y-%m-%d").to_string())
        .unwrap_or_else(|| today.to_string())
}

struct UsageTotals {
    today: f64,
    seven_days: f64,
    month: f64,
}

/// 逐账号的日粒度观察消耗：相邻快照的 remaining 下降量按日期归账。
/// 签到领取带来的上升不计为消耗（usage 取 max(0, 下降量)）。
fn account_usage(
    snaps: &[CreditSnapshot],
    account_id: &str,
    today: &str,
    week_start: &str,
    month_prefix: &str,
) -> (UsageTotals, BTreeMap<String, f64>) {
    let mut by_date: BTreeMap<String, f64> = BTreeMap::new();
    let mut mine: Vec<&CreditSnapshot> = snaps
        .iter()
        .filter(|s| s.account_id == account_id)
        .collect();
    mine.sort_by_key(|s| s.ts);
    // 遇到 remaining 上升（签到领取/账号切换/bug修复后跳变）时重置参考基准：
    // 以最后一次上升后的 remaining 为新起点，上升前的历史不再与之做差，
    // 避免「旧高值 → 上升点后新低值」被错误计入消耗。
    let mut ref_remaining = mine.first().map(|s| s.total_remaining).unwrap_or(0.0);
    for snap in mine.iter().skip(1) {
        if snap.total_remaining > ref_remaining {
            // remaining 上升（领取/充值/重置）：更新基准，不计消耗
            ref_remaining = snap.total_remaining;
        } else {
            let drop = ref_remaining - snap.total_remaining;
            if drop > 0.0 {
                let d = date_key_ms(snap.ts);
                *by_date.entry(d).or_insert(0.0) += drop;
            }
            ref_remaining = snap.total_remaining;
        }
    }
    let totals = UsageTotals {
        today: by_date.get(today).copied().unwrap_or(0.0),
        seven_days: by_date
            .iter()
            .filter(|(d, _)| d.as_str() >= week_start && d.as_str() <= today)
            .map(|(_, v)| v)
            .sum(),
        month: by_date
            .iter()
            .filter(|(d, _)| d.starts_with(month_prefix))
            .map(|(_, v)| v)
            .sum(),
    };
    (totals, by_date)
}

/// 前端 `CreditStatistics` 契约。`refresh=true` 时先对全部国内版账号各拉一次真实配额
/// （顺带落快照），再聚合 —— 语义是"统计页的刷新按钮"，不是后台定时任务。
///
/// 已去掉国际版：本函数只统计国内版（`Cn`）。库里若还留着历史国际版账号包（如升级前
/// 导入的 `local-ai`），它不进快照聚合、不进账号明细 —— 界面上"没有国际版"这条要在
/// 数据侧也成立，不能只靠前端筛。
pub fn credit_statistics(roots: &PathRoots, store: &Path, refresh: bool) -> Value {
    if refresh {
        for b in bundle::list_all(store) {
            if b.target == QoderTarget::Desktop && b.variant == QoderVariant::Cn {
                let _ = quota::fetch_credit_expiry_sync(roots, store, &b.account_id, b.variant);
            }
        }
    }

    let snaps: Vec<CreditSnapshot> = read_snapshots(store)
        .into_iter()
        .filter(|s| s.variant == "cn")
        .collect();
    let logs: Vec<CheckinLogEntry> = read_logs_raw(store)
        .into_iter()
        .filter(|l| l.variant == "cn")
        .collect();
    let bundles: Vec<_> = bundle::list_all(store)
        .into_iter()
        .filter(|b| b.variant == QoderVariant::Cn)
        .collect();

    let today = today_key();
    let week_start = minus_days(&today, 6);
    let month_prefix = today.chars().take(7).collect::<String>();

    // 当前登录 uid（只看国内版），用于 isCurrent。
    let current_uids: Vec<String> = auth_codec::read_desktop_auth(roots, QoderVariant::Cn)
        .ok()
        .map(|a| vec![a.user.id.clone()])
        .unwrap_or_default();

    // 账号全集 = 国内版账号包 ∪ 国内版快照里出现过的账号（已过滤掉非国内版）。
    let mut ids: Vec<String> = bundles.iter().map(|b| b.account_id.clone()).collect();
    for s in &snaps {
        if !ids.contains(&s.account_id) {
            ids.push(s.account_id.clone());
        }
    }

    let mut global_daily: BTreeMap<String, f64> = BTreeMap::new();
    let mut accounts = Vec::new();
    let mut summary_current_remaining = 0.0;
    let mut summary_current_capacity = 0.0;
    let mut usage_today = 0.0;
    let mut usage_7 = 0.0;
    let mut usage_month = 0.0;
    let mut today_checked_in_accounts = 0usize;
    let coverage_start: Option<i64> = snaps.iter().map(|s| s.ts).min();

    for id in &ids {
        let b = bundles.iter().find(|b| &b.account_id == id);
        let latest = snaps
            .iter()
            .filter(|s| &s.account_id == id)
            .max_by_key(|s| s.ts);
        let (totals, by_date) = account_usage(&snaps, id, &today, &week_start, &month_prefix);

        let today_log = logs
            .iter()
            .find(|l| l.account_id.as_deref() == Some(id.as_str()) && date_key_ms(l.ts) == today);
        let checked_in_today = today_log.map(|l| l.result == "success" || l.result == "already");
        let last_log = logs.iter().find(|l| l.account_id.as_deref() == Some(id.as_str()));

        if checked_in_today == Some(true) {
            today_checked_in_accounts += 1;
        }

        usage_today += totals.today;
        usage_7 += totals.seven_days;
        usage_month += totals.month;
        for (d, v) in &by_date {
            *global_daily.entry(d.clone()).or_insert(0.0) += v;
        }
        if let Some(s) = latest {
            summary_current_remaining += s.total_remaining;
            summary_current_capacity += s.total_capacity;
        }

        let mut daily_points = Vec::new();
        for (d, usage) in &by_date {
            daily_points.push(json!({ "date": d, "usage": usage }));
        }
        accounts.push(json!({
            "accountId": id,
            "accountName": b.and_then(|b| b.identity.name.clone())
                .or_else(|| latest.and_then(|s| (!s.email.is_empty()).then(|| s.email.clone())))
                .unwrap_or_else(|| id.clone()),
            "isCurrent": b.and_then(|b| b.identity.uid.clone()).map(|u| current_uids.contains(&u)).unwrap_or(false),
            "currentRemaining": latest.map(|s| s.total_remaining),
            "totalCapacity": latest.map(|s| s.total_capacity),
            "lastSnapshotAt": latest.map(|s| s.ts),
            "usageToday": totals.today,
            "usage7Days": totals.seven_days,
            "usageThisMonth": totals.month,
            "checkedInToday": checked_in_today,
            "checkinStatusToday": today_log.map(|l| l.result.clone()),
            "lastCheckinAt": last_log.map(|l| l.ts),
            "lastCheckinResult": last_log.map(|l| l.result.clone()),
            "daily": daily_points,
            "variant": b
                .map(|b| super::view::variant_key(b.variant).to_string())
                .or_else(|| latest.map(|s| s.variant.clone()))
                .unwrap_or_else(|| "cn".to_string()),
        }));
    }

    // 日历连续的 30 天趋势（含 0），图表才不会把缺口画成断线。
    let mut daily = Vec::new();
    if let Some(start) = coverage_start {
        let start_date = date_key_ms(start);
        if let Ok(d0) = chrono::NaiveDate::parse_from_str(&start_date, "%Y-%m-%d") {
            let now = chrono::Local::now().date_naive();
            let mut d = d0;
            while d <= now {
                let key = d.format("%Y-%m-%d").to_string();
                daily.push(json!({
                    "date": key,
                    "usage": global_daily.get(&key).copied().unwrap_or(0.0),
                }));
                d += chrono::Duration::days(1);
            }
        }
        if daily.len() > RETENTION_DAYS as usize {
            let drop = daily.len() - RETENTION_DAYS as usize;
            daily.drain(0..drop);
        }
    }

    let mut today_success = 0usize;
    let mut today_already = 0usize;
    let mut today_failed = 0usize;
    let mut events = Vec::new();
    for l in &logs {
        if date_key_ms(l.ts) == today {
            match l.result.as_str() {
                "success" => today_success += 1,
                "already" => today_already += 1,
                _ => today_failed += 1,
            }
        }
        events.push(json!({
            "kind": "checkin",
            "ts": l.ts,
            "date": date_key_ms(l.ts),
            "accountId": l.account_id,
            "accountName": if l.email.is_empty() { l.account_id.clone().unwrap_or_else(|| "未知账号".into()) } else { l.email.clone() },
            "result": l.result,
            "error": l.error,
            "variant": l.variant,
        }));
    }

    let mut out = json!({
        "generatedAt": chrono::Utc::now().timestamp_millis(),
        "retentionDays": RETENTION_DAYS,
        "coverageStartAt": coverage_start,
        "summary": {
            "currentRemaining": summary_current_remaining,
            "currentCapacity": summary_current_capacity,
            "usageToday": usage_today,
            "usage7Days": usage_7,
            "usageThisMonth": usage_month,
            "todayCheckedInAccounts": today_checked_in_accounts,
            "todaySuccess": today_success,
            "todayAlready": today_already,
            "todayFailed": today_failed,
        },
        "daily": daily,
        "accounts": accounts,
        "events": events,
    });
    normalize_signed_zeros(&mut out);
    out
}

/// 今天该账号是否已有某类结果的日志。自动流程用它做两件事：
/// 失败按天去重（否则每小时一轮会把一个坏账号刷成日志墙），以及「当天已签短路」。
pub fn has_today_log(store: &Path, account_id: &str, result: &str) -> bool {
    let today = today_key();
    read_logs_raw(store).iter().any(|l| {
        l.account_id.as_deref() == Some(account_id) && l.result == result && date_key_ms(l.ts) == today
    })
}

/// 今天已成功签到的账号集合（`success` 与 `already` 都算已签）。
/// 自动签到据此短路：本地日志已有今天的结论，就不必再问一次服务端。
fn checked_in_today(store: &Path) -> HashSet<String> {
    let today = today_key();
    read_logs_raw(store)
        .into_iter()
        .filter(|l| {
            date_key_ms(l.ts) == today && (l.result == "success" || l.result == "already")
        })
        .filter_map(|l| l.account_id)
        .collect()
}

/// 自动签到的一次核验：启动时与每 `lazy_refresh_hours` 间隔调用。
/// 只做幂等的"未签则签"，绝不碰切换（换号是另一条红线，由用户亲手决定）。
pub fn run_auto_checkin_once(roots: &PathRoots, store: &Path) -> Value {
    let cfg = read_checkin_config(store);
    if !cfg["enabled"].as_bool().unwrap_or(false) {
        return json!({ "status": "disabled" });
    }
    // 与手动批量签到共用同一道门：抢不到说明手动那一轮正在跑，让出即可。
    let Some(_gate) = quota::try_acquire_checkin() else {
        return json!({ "status": "skipped", "reason": "already_running" });
    };
    // 一轮开始时读一次今天的日志：已签的账号直接跳过网络查询。
    // 签到是每日一次的活动，这条短路能把「每轮 N 次查询」压到「每轮只查未签的」。
    let done_today = checked_in_today(store);
    let mut checked = 0u32;
    let (mut success, mut already, mut inactive, mut error) = (0u32, 0u32, 0u32, 0u32);
    for b in bundle::list_all(store) {
        // 已去掉国际版：自动签到只碰国内版账号包。
        if b.target != QoderTarget::Desktop || b.variant != QoderVariant::Cn {
            continue;
        }
        // 本地已有今天的成功结论 → 短路，省一次 campaigns 查询。
        if done_today.contains(&b.account_id) {
            already += 1;
            continue;
        }
        let st = quota::get_checkin_status_sync(roots, store, &b.account_id, b.variant);
        if st.get("ok").and_then(|x| x.as_bool()) != Some(true) {
            // 失败必须可见：此前只累加计数，而返回值又被调用方 `let _ =` 丢弃，
            // 凭据解不开 / 网络错误这些情况在界面上一个字都看不到。
            // 按天去重 —— 同一账号今天已记过 error 就不再写，否则每小时一轮会刷成日志墙。
            let msg = st
                .get("error")
                .and_then(|e| e.as_str())
                .unwrap_or("查询签到状态失败");
            if !has_today_log(store, &b.account_id, "error") {
                let email = b.identity.email.clone().unwrap_or_default();
                record_checkin_log(store, &b.account_id, &email, b.variant, "error", Some(msg));
            }
            error += 1;
            continue;
        }
        if st.get("todayCheckedIn").and_then(|x| x.as_bool()) == Some(true) {
            already += 1;
            continue;
        }
        checked += 1;
        match quota::checkin_sync(roots, store, &b.account_id, b.variant)
            .get("result")
            .and_then(|x| x.as_str())
            .unwrap_or("error")
        {
            "success" => success += 1,
            "already" => already += 1,
            "inactive" => inactive += 1,
            _ => error += 1,
        }
    }
    json!({
        "status": "done",
        "checked": checked,
        "success": success,
        "already": already,
        "inactive": inactive,
        "error": error,
    })
}

/// 两个宿主共用一份自动签到调度：桌面端与 webui 都调用它，行为逐字一致，
/// 避免"同一份 UI、桌面能自动签到、webui 不能"的分叉。
///
/// 只在 `enabled=true` 时动作；每轮开始前重读配置，所以关开关下一轮即生效。
/// 首轮不等惰性刷新：进程一起来就核验一次服务端状态、给未签到的账号补签。
/// 只做签到（幂等的 Credits 领取），绝不碰切换 —— 换号会重启用户正在用的 IDE，
/// 那条红线是"必须用户亲手"。
pub fn spawn_scheduler() {
    std::thread::Builder::new()
        .name("qs-auto-checkin".into())
        .spawn(|| {
            let roots = PathRoots::real();
            loop {
                let store = crate::modules::config::switch_root();
                let cfg = read_checkin_config(&store);
                if cfg["enabled"].as_bool().unwrap_or(false) {
                    let _ = run_auto_checkin_once(&roots, &store);
                    let hours = cfg["lazy_refresh_hours"].as_u64().unwrap_or(6).clamp(1, 72);
                    for _ in 0..hours {
                        // 每小时醒一次复查开关：关掉自动签到后最多 1 小时内退出循环，
                        // 不会拖着一整段惰性刷新间隔还在打服务端。
                        if !read_checkin_config(&store)["enabled"].as_bool().unwrap_or(false) {
                            break;
                        }
                        std::thread::sleep(std::time::Duration::from_secs(3600));
                    }
                } else {
                    std::thread::sleep(std::time::Duration::from_secs(60));
                }
            }
        })
        .ok();
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_store() -> PathBuf {
        let dir = std::env::temp_dir().join(format!("qs-ledger-{}", uuid::Uuid::new_v4().simple()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn checkin_config_roundtrips_and_clamps() {
        let dir = temp_store();
        // 无文件回退默认：enabled 必须是 false（自动打网络的开关不能默认开）。
        let d0 = read_checkin_config(&dir);
        assert_eq!(d0["enabled"], false);
        let merged = write_checkin_config(&dir, &json!({ "enabled": true, "lazy_refresh_hours": 0 })).unwrap();
        assert_eq!(merged["enabled"], true);
        assert_eq!(merged["lazy_refresh_hours"], 1, "低于下限收口到 1");
        // 只写 patch 不能把没提到的字段冲掉。
        let merged = write_checkin_config(&dir, &json!({ "lazy_refresh_hours": 12 })).unwrap();
        assert_eq!(merged["enabled"], true, "未提及的 enabled 必须保留");
        assert_eq!(merged["lazy_refresh_hours"], 12);
        // 上游遗留字段不再持久化：写了也不落盘，界面上不会出现永不生效的开关。
        let merged = write_checkin_config(&dir, &json!({ "keepalive_days": 999 })).unwrap();
        assert!(merged.get("keepalive_days").is_none(), "{merged}");
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn checkin_logs_are_recorded_pruned_and_read_back() {
        let dir = temp_store();
        record_checkin_log(&dir, "acct-a", "a@x.com", QoderVariant::Cn, "success", None);
        record_checkin_log(&dir, "acct-b", "b@x.com", QoderVariant::Global, "error", Some("HTTP 500"));
        let logs = read_checkin_logs(&dir)["logs"].as_array().cloned().unwrap();
        assert_eq!(logs.len(), 2);
        assert_eq!(logs[0]["result"], "error", "新的在前");
        assert_eq!(logs[0]["variant"], "ai");
        assert_eq!(logs[1]["email"], "a@x.com");
        // 40 天前的条目在下一次写入时被裁掉。
        std::fs::write(
            checkin_logs_path(&dir),
            serde_json::to_vec(&[
                CheckinLogEntry { ts: chrono::Utc::now().timestamp_millis() - 40 * 86_400_000, account_id: Some("acct-old".into()), email: "old@x.com".into(), result: "success".into(), error: None, variant: "cn".into() },
                CheckinLogEntry { ts: chrono::Utc::now().timestamp_millis(), account_id: Some("acct-new".into()), email: "n@x.com".into(), result: "success".into(), error: None, variant: "cn".into() },
            ])
            .unwrap(),
        )
        .unwrap();
        record_checkin_log(&dir, "acct-c", "c@x.com", QoderVariant::Cn, "already", None);
        let logs = read_checkin_logs(&dir)["logs"].as_array().cloned().unwrap();
        assert_eq!(logs.len(), 2, "40 天前的条目必须被裁掉: {logs:?}");
        assert!(logs.iter().all(|l| l["accountId"] != "acct-old"));
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn snapshots_are_throttled_and_drive_statistics() {
        let dir = temp_store();
        let now = chrono::Utc::now().timestamp_millis();
        let day = 86_400_000i64;
        let mk = |ts: i64, remaining: f64| CreditSnapshot {
            ts,
            account_id: "acct-a".into(),
            email: "a@x.com".into(),
            variant: "cn".into(),
            total_capacity: 1000.0,
            total_remaining: remaining,
            expiring_soon_remaining: 0.0,
            soonest_expire_at: None,
        };
        // 节流：与上一条（按落盘顺序，即最近时刻）间隔 < 10 分钟的不落。
        append_credit_snapshot(&dir, &mk(now - 5 * day, 900.0));
        append_credit_snapshot(&dir, &mk(now - 5 * day + 60_000, 899.0));
        assert_eq!(read_snapshots(&dir).len(), 1, "60 秒内的重复快照必须被节流");

        append_credit_snapshot(&dir, &mk(now - 2 * day, 850.0));
        append_credit_snapshot(&dir, &mk(now, 750.0));
        let snaps = read_snapshots(&dir);
        assert_eq!(snaps.len(), 3, "{snaps:?}");

        // 观察消耗：900→850=50 归 2 天前，850→750=100 归今天；今天用量 = 100，近 7 天 = 150。
        let stats = credit_statistics(&PathRoots::real(), &dir, false);
        assert_eq!(stats["summary"]["usageToday"], 100.0, "{stats}");
        assert_eq!(stats["summary"]["usage7Days"], 150.0, "{stats}");
        assert_eq!(stats["summary"]["currentRemaining"], 750.0, "{stats}");
        let accs = stats["accounts"].as_array().cloned().unwrap_or_default();
        assert_eq!(accs.len(), 1, "{stats}");
        assert_eq!(accs[0]["accountId"], "acct-a");
        assert_eq!(accs[0]["currentRemaining"], 750.0);
        // 覆盖期从首条快照起，日粒度连续（5 天前 → 今天 = 6 天）。
        assert_eq!(stats["daily"].as_array().map(|d| d.len()), Some(6), "{stats}");
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn disabled_config_never_hits_the_network() {
        let dir = temp_store();
        // 无配置文件 = disabled：调度循环必须直接返回，一个账号都不碰。
        let r = run_auto_checkin_once(&PathRoots::real(), &dir);
        assert_eq!(r["status"], "disabled", "{r}");
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn today_log_lookup_is_day_and_result_scoped() {
        let dir = temp_store();
        record_checkin_log(&dir, "acct-a", "a@x.com", QoderVariant::Cn, "error", Some("HTTP 500"));
        assert!(has_today_log(&dir, "acct-a", "error"));
        assert!(!has_today_log(&dir, "acct-a", "success"), "结果类型必须区分");
        assert!(!has_today_log(&dir, "acct-b", "error"), "账号必须区分");

        // 昨天的 error 不能算今天 —— 否则今天的失败会被永久去重掉，用户再也看不到。
        std::fs::write(
            checkin_logs_path(&dir),
            serde_json::to_vec(&[CheckinLogEntry {
                ts: chrono::Utc::now().timestamp_millis() - 86_400_000,
                account_id: Some("acct-a".into()),
                email: "a@x.com".into(),
                result: "error".into(),
                error: None,
                variant: "cn".into(),
            }])
            .unwrap(),
        )
        .unwrap();
        assert!(!has_today_log(&dir, "acct-a", "error"), "昨天的日志不算今天");
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn checked_in_today_excludes_failures() {
        let dir = temp_store();
        assert!(checked_in_today(&dir).is_empty(), "无日志时短路集合为空");
        record_checkin_log(&dir, "acct-ok", "ok@x.com", QoderVariant::Cn, "success", None);
        record_checkin_log(&dir, "acct-already", "al@x.com", QoderVariant::Cn, "already", None);
        record_checkin_log(&dir, "acct-err", "err@x.com", QoderVariant::Cn, "error", Some("HTTP 500"));
        let set = checked_in_today(&dir);
        assert!(set.contains("acct-ok"));
        assert!(set.contains("acct-already"));
        // error 绝不能进短路集合：否则查询失败的账号会被当成"已签"永久跳过。
        assert!(!set.contains("acct-err"), "{set:?}");
        std::fs::remove_dir_all(dir).ok();
    }

    /// 互斥门与「启用但库里无账号」串在同一个测试里：两者都碰全局 `CHECKIN_GATE`，
    /// 拆成两个 `#[test]` 会在 cargo 的并行测试下互相抢锁而 flaky。
    #[test]
    fn checkin_gate_serializes_and_enabled_noop_is_done() {
        let first = quota::try_acquire_checkin().expect("首次必须抢到");
        assert!(quota::try_acquire_checkin().is_none(), "持锁期间第二次抢占必须失败");
        drop(first);
        assert!(quota::try_acquire_checkin().is_some(), "释放后必须能再抢到");

        // enabled 但没有账号：一轮跑完、计数全 0，证明门能被正常获取与释放。
        let dir = temp_store();
        write_checkin_config(&dir, &json!({ "enabled": true })).unwrap();
        let r = run_auto_checkin_once(&PathRoots::real(), &dir);
        assert_eq!(r["status"], "done", "{r}");
        assert_eq!(r["checked"], 0);
        assert_eq!(r["error"], 0);
        std::fs::remove_dir_all(dir).ok();
    }
}
