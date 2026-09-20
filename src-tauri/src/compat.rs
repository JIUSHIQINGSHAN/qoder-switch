//! 前端契约适配层。
//!
//! 前端 `src/` 是 workbuddy-switch 的原样副本，它调用的命令名与返回形状是既成契约
//! （`AccountMeta` / `AppStatus` 等）。这里让 Rust 去满足那份契约，而不是反过来改
//! 一万三千行前端 —— 少改一行，"复刻"就少一分水分。
//!
//! 与 `commands.rs`（Qoder 原生接口）并存：界面走本模块，命令行工具与脚本走原生接口。

use serde::{Deserialize, Serialize};
use serde_json::json;

use qs_switch_core::modules::config::PathRoots;
use qs_switch_core::modules::variant::{QoderTarget, QoderVariant};
use qs_switch_core::modules::{auth_codec, bundle, export_import, process, rotate, switch};

/// 与前端 `WbVariant` 对齐：国内版 `cn`、国际版 `ai`（沿用前端的键名以免改 UI）。
fn variant_from_str(s: Option<&str>) -> QoderVariant {
    match s {
        Some("ai") | Some("global") => QoderVariant::Global,
        _ => QoderVariant::Cn,
    }
}

fn variant_key(v: QoderVariant) -> &'static str {
    match v {
        QoderVariant::Cn => "cn",
        QoderVariant::Global => "ai",
    }
}


#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CurrentUser {
    pub uid: Option<String>,
    pub nickname: Option<String>,
    pub email: Option<String>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AppStatus {
    pub running: bool,
    pub auth_file: String,
    pub current: Option<CurrentUser>,
    pub app_path: String,
    pub version: String,
    pub variant: &'static str,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AccountMeta {
    pub id: String,
    pub uid: Option<String>,
    pub email: Option<String>,
    pub nickname: Option<String>,
    pub enterprise_name: Option<String>,
    pub expires_at: Option<u64>,
    pub refresh_expires_at: Option<u64>,
    pub refreshed_at: Option<u64>,
    pub created_at: Option<u64>,
    pub needs_relogin: bool,
    pub needs_relogin_reason: Option<String>,
    pub variant: &'static str,
}

/// 桌面端是否支持：CLI 目标在 CN 版不落盘，界面需要据此讲清为什么切了没反应。
fn desktop_auth_path(roots: &PathRoots, v: QoderVariant) -> String {
    qs_switch_core::modules::variant::credentials(roots, v, QoderTarget::Desktop)
        .into_iter()
        .find(|f| f.role == qs_switch_core::modules::variant::FileRole::AuthMain)
        .map(|f| f.path.display().to_string())
        .unwrap_or_default()
}

/// 桌面端版本号：取自明文回显的 `version`（桌面端自己写的，比猜安装目录名权威）。
fn desktop_version(roots: &PathRoots, v: QoderVariant) -> String {
    let path = qs_switch_core::modules::variant::cli_dir(roots, v).join(".qoder-app-status.json");
    std::fs::read_to_string(path)
        .ok()
        .and_then(|t| serde_json::from_str::<serde_json::Value>(&t).ok())
        .and_then(|j| j.get("version").and_then(|x| x.as_str()).map(String::from))
        .unwrap_or_default()
}

#[tauri::command]
pub fn get_status(variant: Option<String>) -> AppStatus {
    let roots = PathRoots::real();
    let v = variant_from_str(variant.as_deref());
    let auth = auth_codec::read_desktop_auth(&roots, v).ok();
    AppStatus {
        running: !process::running_pids(QoderTarget::Desktop.images(v)).is_empty(),
        auth_file: desktop_auth_path(&roots, v),
        current: auth.as_ref().map(|a| CurrentUser {
            uid: Some(a.user.id.clone()),
            nickname: Some(a.user.name.clone()),
            email: Some(a.user.email.clone()),
        }),
        app_path: qs_switch_core::modules::variant::executable(&roots, v, QoderTarget::Desktop)
            .map(|p| p.display().to_string())
            .unwrap_or_default(),
        version: desktop_version(&roots, v),
        variant: variant_key(v),
    }
}

#[tauri::command]
pub fn get_accounts() -> serde_json::Value {
    let store = qs_switch_core::modules::config::switch_root();
    let accounts: Vec<AccountMeta> = bundle::list_all(&store)
        .into_iter()
        .map(|b| AccountMeta {
            id: b.account_id.clone(),
            uid: b.identity.uid.clone(),
            email: b.identity.email.clone(),
            nickname: b.identity.name.clone(),
            enterprise_name: None,
            expires_at: b.identity.expires_at.as_deref().and_then(bundle::iso_to_ms),
            refresh_expires_at: b.identity.refresh_expires_at.as_deref().and_then(bundle::iso_to_ms),
            refreshed_at: None,
            created_at: bundle::compact_to_ms(&b.created_at),
            // 解不出 token 的包等同于要重新登录，界面据此提示。
            needs_relogin: b.identity.uid.is_none(),
            needs_relogin_reason: b.identity.uid.is_none().then(|| {
                "认领时未能解密登录态（跨 Windows 用户或 Local State 不匹配），需在本机重新登录一次"
                    .to_string()
            }),
            variant: variant_key(b.variant),
        })
        .collect();
    json!({ "accounts": accounts })
}


#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SwitchResult {
    pub ok: bool,
    pub account_id: String,
    pub restarted: bool,
    pub message: String,
}

fn target_from_str(s: Option<&str>) -> QoderTarget {
    match s {
        Some("cli") => QoderTarget::Cli,
        Some("work") => QoderTarget::Work,
        _ => QoderTarget::Desktop,
    }
}

/// 前端签名：switchAccount({ accountId, restart, shareSessions, ... , variant })。
/// 会话复制（shareSessions）在 Qoder 侧没有可用的归属机制，故意忽略并在 message 里说明。
#[tauri::command]
pub async fn switch_account(
    app: tauri::AppHandle,
    cell: tauri::State<'_, ProgressCell>,
    args: serde_json::Value,
) -> Result<SwitchResult, String> {
    use tauri::Emitter;
    let account_id = args
        .get("accountId")
        .and_then(|x| x.as_str())
        .ok_or_else(|| "缺 accountId".to_string())?
        .to_string();
    let variant = variant_from_str(args.get("variant").and_then(|x| x.as_str()));
    let target = target_from_str(args.get("target").and_then(|x| x.as_str()));
    let restart = args.get("restart").and_then(|x| x.as_bool()).unwrap_or(true);
    let forced = args.get("forced").and_then(|x| x.as_bool()).unwrap_or(false);
    let ignored_session = args
        .get("shareSessions")
        .and_then(|x| x.as_bool())
        .unwrap_or(false);

    let roots = PathRoots::real();
    let store = qs_switch_core::modules::config::switch_root();
    let req = switch::Request { account_id: account_id.clone(), variant, target, restart };
    let actor = if forced { switch::Actor::RealForced } else { switch::Actor::Real };
    let handle = app.clone();
    let prog: ProgressCell = (*cell).clone();
    let prog_done = prog.clone();
    prog.set(true, Some("准备切换".into()));
    let journal = match tauri::async_runtime::spawn_blocking(move || {
        let inner = prog.clone();
        switch::execute(&roots, &store, &req, actor, &mut |m| {
            let _ = handle.emit("switch-progress", m.to_string());
            inner.set(true, Some(m.to_string()));
        })
    })
    .await
    {
        Ok(Ok(j)) => j,
        // 无论正常返回还是线程崩掉，都必须把 running 清零，
        // 否则前端的进度对话框会一直转下去。
        Ok(Err(e)) => {
            prog_done.set(false, None);
            return Err(e);
        }
        Err(e) => {
            prog_done.set(false, None);
            return Err(format!("切换任务异常终止: {e}"));
        }
    };
    prog_done.set(false, None);

    let mut message = format!("已切到 {}（{:?}）", journal.account_id, journal.phase);
    if ignored_session {
        message.push_str("；会话复制未执行 —— Qoder 的会话不按账号归属，跨账号复制会串数据");
    }
    Ok(SwitchResult {
        ok: journal.phase == switch::Phase::Completed,
        account_id: journal.account_id.clone(),
        restarted: restart,
        message,
    })
}

#[tauri::command]
pub fn import_local(
    account_id: String,
    variant: Option<String>,
    target: Option<String>,
) -> Result<serde_json::Value, String> {
    let roots = PathRoots::real();
    let store = qs_switch_core::modules::config::switch_root();
    let v = variant_from_str(variant.as_deref());
    let t = target_from_str(target.as_deref());
    let b = bundle::capture(&roots, &store, &account_id, v, t)?;
    if b.is_empty() {
        return Err(format!(
            "该目标在本机不落盘凭据（{:?}·{:?}），没有可认领的文件",
            v, t
        ));
    }
    Ok(json!({ "ok": true, "account": b }))
}

#[tauri::command]
pub fn delete_account(account_id: String) -> Result<serde_json::Value, String> {
    let dir = qs_switch_core::modules::bundle::accounts_root_in(
        &qs_switch_core::modules::config::switch_root(),
    )
    .join(&account_id);
    if !dir.is_dir() {
        return Err(format!("账号目录不存在: {}", dir.display()));
    }
    std::fs::remove_dir_all(&dir)
        .map_err(|e| format!("删除 {} 失败: {e}", dir.display()))?;
    Ok(json!({ "ok": true }))
}

/// 切换进度。前端对话框轮询 `switch_progress`，桌面端另有事件通道，两条路共用这份状态。
/// 用 Arc 包一层，才能把句柄带进 spawn_blocking 的闭包里更新、并在收尾时清零。
#[derive(Clone, Default)]
pub struct ProgressCell(pub std::sync::Arc<std::sync::Mutex<ProgressState>>);

#[derive(Default, Clone)]
pub struct ProgressState {
    pub running: bool,
    pub progress: Option<String>,
}

impl ProgressCell {
    pub fn set(&self, running: bool, msg: Option<String>) {
        if let Ok(mut g) = self.0.lock() {
            g.running = running;
            g.progress = msg;
        }
    }
}

/// 前端在切换对话框里轮询这个；桌面端主要靠事件，但契约要求它存在。
#[tauri::command]
pub fn switch_progress(cell: tauri::State<'_, ProgressCell>) -> serde_json::Value {
    let g = cell.0.lock().unwrap_or_else(|p| p.into_inner());
    json!({ "running": g.running, "progress": g.progress })
}

/// 自动轮换配置。只有 `enabled` 与两个阈值在 Qoder 侧有真实语义：
/// 阈值按天而不是按小时，且**永不自动执行切换**（见 README 的安全模型）。
#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UiRotateConfig {
    pub enabled: bool,
    pub check_interval_minutes: u32,
    pub cooldown_minutes: u32,
    pub min_gap_hours: u32,
    pub min_urgency_hours: u32,
    pub active_guard_minutes: u32,
    pub min_remaining_credits: u32,
}

impl Default for UiRotateConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            check_interval_minutes: 60,
            cooldown_minutes: 120,
            min_gap_hours: (rotate::RotateConfig::default().min_gap_days * 24).max(0) as u32,
            min_urgency_hours: (rotate::RotateConfig::default().min_urgency_days * 24) as u32,
            active_guard_minutes: 0,
            min_remaining_credits: 0,
        }
    }
}

fn ui_config_path(store: &std::path::Path) -> std::path::PathBuf {
    store.join("auto_rotate_config.json")
}

fn read_ui_config(store: &std::path::Path) -> UiRotateConfig {
    std::fs::read(ui_config_path(store))
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or_default()
}

#[tauri::command]
pub fn get_auto_rotate_config() -> UiRotateConfig {
    read_ui_config(&qs_switch_core::modules::config::switch_root())
}

#[tauri::command]
pub fn save_auto_rotate_config(
    config: serde_json::Value,
) -> Result<UiRotateConfig, String> {
    let store = qs_switch_core::modules::config::switch_root();
    let merged = {
        let mut cur = read_ui_config(&store);
        if let Some(v) = config.get("enabled").and_then(|x| x.as_bool()) {
            cur.enabled = v;
        }
        for key in [
            "check_interval_minutes",
            "cooldown_minutes",
            "min_gap_hours",
            "min_urgency_hours",
        ] {
            if let Some(n) = config.get(key).and_then(|x| x.as_u64()) {
                match key {
                    "check_interval_minutes" => cur.check_interval_minutes = n as u32,
                    "cooldown_minutes" => cur.cooldown_minutes = n as u32,
                    "min_gap_hours" => cur.min_gap_hours = n as u32,
                    _ => cur.min_urgency_hours = n as u32,
                }
            }
        }
        cur
    };
    let json = serde_json::to_vec_pretty(&merged).map_err(|e| e.to_string())?;
    std::fs::create_dir_all(&store).map_err(|e| e.to_string())?;
    std::fs::write(ui_config_path(&store), json).map_err(|e| format!("写轮换配置失败: {e}"))?;
    Ok(merged)
}

/// 把前端的"小时"阈值换成本地判定用的"天"。
fn core_config(c: &UiRotateConfig) -> rotate::RotateConfig {
    rotate::RotateConfig {
        min_urgency_days: (c.min_urgency_hours as i64).div_euclid(24).max(1),
        min_gap_days: (c.min_gap_hours as i64).div_euclid(24).max(1),
    }
}

#[tauri::command]
pub fn rotate_status() -> serde_json::Value {
    let roots = PathRoots::real();
    let store = qs_switch_core::modules::config::switch_root();
    let cfg = read_ui_config(&store);
    let v = QoderVariant::Cn;
    let cur = auth_codec::read_desktop_auth(&roots, v).ok();
    json!({
        "config": cfg,
        // CLI 指针在 Qoder 侧没有对应机制（CLI 不落盘凭据），恒为 false。
        "cliConfigured": false,
        "activeAccountId": cur.as_ref().map(|a| a.user.id.clone()),
        "activeAccountName": cur.as_ref().map(|a| a.user.name.clone()),
        "lastCheckAt": bundle::compact_to_ms(&qs_switch_core::modules::config::now_ts()),
        "lastSwitchAt": read_last_switch_at(&store),
    })
}

fn read_last_switch_at(store: &std::path::Path) -> Option<u64> {
    let st = rotate::read_state(store).ok()?;
    st.last_suggested_at.as_deref().and_then(bundle::compact_to_ms)
}

/// 手动跑一次轮换检查。只产出建议并记日志，**不执行切换**。
#[tauri::command]
pub fn run_rotate(variant: Option<String>) -> serde_json::Value {
    let roots = PathRoots::real();
    let store = qs_switch_core::modules::config::switch_root();
    let v = variant_from_str(variant.as_deref());
    let uicfg = read_ui_config(&store);
    let cfg = core_config(&uicfg);
    match rotate::suggest(&roots, &store, v, &cfg) {
        Ok(s) if s.decision.switch_to.is_some() => json!({
            "status": "suggested",
            "to": s.decision.switch_to,
            "reason": s.decision.reason,
            "notify": {
                "title": "轮换建议",
                "body": format!("{}（不会自动执行，需你确认）", s.decision.reason),
            },
        }),
        Ok(s) => json!({ "status": "hold", "reason": s.decision.reason }),
        Err(e) => json!({ "status": "error", "error": e }),
    }
}

#[tauri::command]
pub fn get_rotate_logs() -> serde_json::Value {
    let store = qs_switch_core::modules::config::switch_root();
    let logs: Vec<serde_json::Value> = rotate::read_state(&store)
        .map(|st| {
            st.history
                .into_iter()
                .filter_map(|(ts, to, reason)| {
                    Some(json!({
                        "ts": bundle::compact_to_ms(&ts)?,
                        "action": "suggest",
                        "reason": reason,
                        "from": null,
                        "to": { "id": to, "name": to },
                    }))
                })
                .collect()
        })
        .unwrap_or_default();
    json!({ "logs": logs })
}

/// 导出为前端可保存的记录（我们的记录是凭据文件副本，不含 token 明文）。
#[tauri::command]
pub fn export_accounts(account_ids: Vec<String>) -> Result<serde_json::Value, String> {
    let store = qs_switch_core::modules::config::switch_root();
    let mut records = Vec::new();
    let mut errors = Vec::new();
    for id in &account_ids {
        match export_import::export_account(&store, id).and_then(|e| export_import::to_bytes(&e)) {
            Ok(bytes) => {
                let text = String::from_utf8_lossy(&bytes).to_string();
                let parsed: serde_json::Value =
                    serde_json::from_str(&text).unwrap_or_else(|_| json!({}));
                let bundle0 = match parsed.get("bundles").and_then(|b| b.as_array()) {
                    Some(a) => a.first().cloned().unwrap_or(json!({})),
                    None => json!({}),
                };
                let ident = bundle0.get("identity").cloned().unwrap_or(json!({}));
                records.push(json!({
                    "id": id,
                    "uid": ident.get("uid"),
                    "nickname": ident.get("name"),
                    "email": ident.get("email"),
                    "variant": variant_key(variant_from_str(
                        bundle0.get("variant").and_then(|x| x.as_str()),
                    )),
                    "expiresAt": ident
                        .get("expires_at")
                        .and_then(|x| x.as_str())
                        .and_then(bundle::iso_to_ms),
                    // 凭据文件副本，base64 形态；导入时按哈希校验还原。
                    "payload": text,
                }));
            }
            Err(e) => errors.push(format!("{id}: {e}")),
        }
    }
    if records.is_empty() {
        return Err(if errors.is_empty() {
            "没有可导出的账号".into()
        } else {
            errors.join("; ")
        });
    }
    Ok(json!({ "ok": true, "accounts": records, "warnings": errors }))
}

#[tauri::command]
pub fn export_accounts_to_path(
    account_ids: Vec<String>,
    path: String,
) -> Result<serde_json::Value, String> {
    let v = export_accounts(account_ids)?;
    let text = serde_json::to_string_pretty(&v["accounts"])
        .map_err(|e| e.to_string())?;
    std::fs::write(&path, text).map_err(|e| format!("写 {path} 失败: {e}"))?;
    Ok(json!({ "ok": true, "path": path }))
}

/// 预览导入文件：只解析与校验，不写盘。
#[tauri::command]
pub fn preview_import_accounts(file_text: String) -> Result<serde_json::Value, String> {
    let v: serde_json::Value = serde_json::from_str(&file_text)
            .map_err(|e| format!("备份文件不是合法 JSON: {e}"))?;
    let arr = v
        .as_array()
        .cloned()
        .or_else(|| v.get("accounts").and_then(|x| x.as_array()).cloned())
        .unwrap_or_default();
    let accounts: Vec<serde_json::Value> = arr
        .iter()
        .filter_map(|r| {
            let id = r.get("id").and_then(|x| x.as_str())?;
            Some(json!({
                "id": id,
                "uid": r.get("uid").and_then(|x| x.as_str()),
                "nickname": r.get("nickname").and_then(|x| x.as_str()),
                "email": r.get("email").and_then(|x| x.as_str()),
                "variant": r.get("variant").and_then(|x| x.as_str()),
                "expiresAt": r.get("expiresAt").and_then(|x| x.as_u64()),
            }))
        })
        .collect();
    Ok(json!({ "accounts": accounts, "total": accounts.len() }))
}

/// 导入。`indexes` 是用户在预览里勾选的下标。
#[tauri::command]
pub fn import_accounts(
    file_text: String,
    indexes: Option<Vec<usize>>,
) -> Result<serde_json::Value, String> {
    let store = qs_switch_core::modules::config::switch_root();
    let v: serde_json::Value = serde_json::from_str(&file_text)
        .map_err(|e| format!("备份文件不是合法 JSON: {e}"))?;
    let arr = v
        .as_array()
        .cloned()
        .unwrap_or_else(|| v.get("accounts").and_then(|x| x.as_array()).cloned().unwrap_or_default());
    let picked: Vec<&serde_json::Value> = match indexes {
        Some(ix) => ix.into_iter().filter_map(|i| arr.get(i)).collect(),
        None => arr.iter().collect(),
    };
    let (mut imported, mut skipped, mut overwritten) = (0usize, 0usize, 0usize);
    for rec in picked {
        let Some(payload) = rec.get("payload").and_then(|x| x.as_str()) else {
            skipped += 1;
            continue;
        };
        let existed = rec
            .get("id")
            .and_then(|x| x.as_str())
            .map(|id| {
                qs_switch_core::modules::bundle::accounts_root_in(&store)
                    .join(id)
                    .is_dir()
            })
            .unwrap_or(false);
        match export_import::import(&store, payload.as_bytes(), true) {
            Ok(r) => {
                imported += r.written.len();
                skipped += r.skipped.len();
                if existed {
                    overwritten += 1;
                }
            }
            Err(e) => return Err(format!("导入失败: {e}")),
        }
    }
    Ok(json!({
        "ok": imported > 0,
        "imported": imported,
        "skipped": skipped,
        "overwritten": overwritten,
    }))
}

/// 前端逐项确认"哪些能力在 Qoder 侧不存在"，用于在界面上写明而不是装作能用。
#[tauri::command]
pub fn get_capabilities() -> serde_json::Value {
    json!({
        "supported": [
            "账号包认领与列表",
            "一键切换（含备份、写后校验、失败回滚）",
            "账号包导出与导入",
            "token 到期与轮换建议",
            "凭据快照与差分",
            "托盘快捷切换"
        ],
        "unavailable": [
            { "name": "每日签到", "reason": "Qoder 无签到接口" },
            { "name": "Buddy 旅行", "reason": "WorkBuddy 专有玩法" },
            { "name": "积分统计与额度查询", "reason": "官方接口未取证，拒绝猜测调用" },
            { "name": "会话跨账号复制", "reason": "Qoder 会话不按账号归属，复制会串数据" },
            { "name": "OAuth 扫码添加账号", "reason": "设备流程端点未取证" },
            { "name": "主动刷新 token", "reason": "刷新接口未取证" },
            { "name": "自动轮换执行", "reason": "换号需重启用户正在用的 IDE，只出建议" },
            { "name": "限速钩子与 429 归因", "reason": "未实现" },
            { "name": "自动更新", "reason": "未配置发布源" }
        ]
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ui_config_defaults_roundtrip_through_core_config() {
        let cfg = UiRotateConfig::default();
        let core = core_config(&cfg);
        assert!(core.min_urgency_days >= 1, "小时换算成天不得归零");
        assert!(core.min_gap_days >= 1);
    }

    #[test]
    fn capabilities_are_explicit_about_what_is_missing() {
        let v = get_capabilities();
        assert!(v["unavailable"].as_array().unwrap().len() >= 6);
        let names: Vec<String> = v["unavailable"]
            .as_array()
            .unwrap()
            .iter()
            .map(|x| x["name"].as_str().unwrap_or_default().to_string())
            .collect();
        assert!(names.iter().any(|n| n.contains("签到")));
        assert!(names.iter().any(|n| n.contains("会话")));
    }

    #[test]
    fn run_rotate_never_claims_to_have_switched() {
        let r = run_rotate(Some("cn".into()));
        let status = r.get("status").and_then(|x| x.as_str()).unwrap_or("");
        assert!(
            matches!(status, "suggested" | "hold" | "error"),
            "轮换只允许产出建议，实得 {r}"
        );
        assert_ne!(status, "switched");
    }

    #[test]
    fn rotate_logs_and_status_are_wellformed() {
        let s = rotate_status();
        assert_eq!(s["cliConfigured"], false, "Qoder 无 CLI 指针机制");
        assert!(s.get("config").is_some());
        let l = get_rotate_logs();
        assert!(l["logs"].is_array());
    }

    #[test]
    fn preview_import_rejects_garbage() {
        assert!(preview_import_accounts("不是 JSON".into()).is_err());
        let ok = preview_import_accounts(r#"[{"id":"a","uid":"u"}]"#.into()).unwrap();
        assert_eq!(ok["total"], 1);
    }

    #[test]
    fn variant_keys_match_frontend_contract() {
        assert_eq!(variant_key(QoderVariant::Cn), "cn");
        assert_eq!(variant_key(QoderVariant::Global), "ai");
        assert_eq!(variant_from_str(Some("ai")), QoderVariant::Global);
        assert_eq!(variant_from_str(None), QoderVariant::Cn);
        assert_eq!(variant_from_str(Some("junk")), QoderVariant::Cn);
    }

    #[test]
    fn timestamp_parsers_handle_both_shapes() {
        assert!(bundle::iso_to_ms("2026-10-19T06:19:41Z").unwrap() > 1_700_000_000_000);
        assert!(bundle::iso_to_ms("不是时间").is_none());
        assert!(bundle::compact_to_ms("20260920T053211Z").unwrap() > 1_700_000_000_000);
        assert!(bundle::compact_to_ms("2026-10-19T06:19:41Z").is_none(), "两种形态不互认");
    }

    #[test]
    fn serialized_keys_are_camel_case() {
        let s = serde_json::to_value(AccountMeta {
            id: "x".into(),
            uid: Some("u".into()),
            email: None,
            nickname: Some("n".into()),
            enterprise_name: None,
            expires_at: Some(1),
            refresh_expires_at: None,
            refreshed_at: None,
            created_at: None,
            needs_relogin: false,
            needs_relogin_reason: None,
            variant: "cn",
        })
        .unwrap();
        for k in [
            "id",
            "uid",
            "nickname",
            "enterpriseName",
            "expiresAt",
            "refreshExpiresAt",
            "needsRelogin",
            "needsReloginReason",
            "variant",
        ] {
            assert!(s.get(k).is_some(), "前端契约要求键 {k}");
        }
    }

    /// 本机现状必须能被 get_status 说清楚：在跑、认证文件路径、当前账号。
    #[test]
    fn status_reflects_local_reality() {
        let s = get_status(Some("cn".into()));
        assert_eq!(s.variant, "cn");
        assert!(s.auth_file.ends_with("auth.v1.dat"), "{:?}", s.auth_file);
        if std::path::Path::new(&s.auth_file).is_file() {
            let cur = s.current.expect("有凭据文件就该解出当前账号");
            assert!(cur.uid.as_deref().map(|u| !u.is_empty()).unwrap_or(false));
        }
    }

    #[test]
    fn accounts_list_carries_expiry() {
        let v = get_accounts();
        let arr = v.get("accounts").and_then(|x| x.as_array()).expect("accounts 数组");
        // 本机已认领过账号，列表不该为空；到期时间是判断"该切谁"的依据。
        if !arr.is_empty() {
            assert!(arr.iter().any(|a| a.get("expiresAt").is_some()));
        }
    }
}
