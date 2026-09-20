//! 展示层视图：把 core 的数据整成前端既成契约的形状（camelCase）。
//!
//! 为什么在 core 而不是宿主里：桌面端与 webui 两个宿主必须返回**同一份** JSON。
//! 放在任一宿主里都会逼另一个复制一遍，而这两份迟早会因为字段增删而分叉。
//! 前端的形状来自 workbuddy-switch 的 `src/lib/types.ts`，改名会破坏"复刻"，
//! 所以这里迁就它，而不是反过来。

use serde_json::{Value, json};

use crate::modules::config::{PathRoots, now_ts, switch_root};
use crate::modules::variant::{
    QoderTarget, QoderVariant, cli_dir, credentials, executable, FileRole,
};
use crate::modules::{auth_codec, bundle, export_import, process, rotate, switch};

/// 前端 `WbVariant`：国内版 `cn`、国际版 `ai`。
pub fn variant_key(v: QoderVariant) -> &'static str {
    match v {
        QoderVariant::Cn => "cn",
        QoderVariant::Global => "ai",
    }
}

/// "导入本机账号"的缺省包名。前端只带档位、不带 id（它那时还不知道本机登的是谁），
/// 所以 id 必须由档位推出 —— 否则这个按钮在两个宿主里都会以"缺 accountId"失败。
pub fn local_account_id(v: QoderVariant) -> String {
    format!("local-{}", variant_key(v))
}

pub fn variant_from_key(s: Option<&str>) -> QoderVariant {
    match s {
        Some("ai") | Some("global") => QoderVariant::Global,
        _ => QoderVariant::Cn,
    }
}

fn auth_file_path(roots: &PathRoots, v: QoderVariant) -> String {
    credentials(roots, v, QoderTarget::Desktop)
        .into_iter()
        .find(|f| f.role == FileRole::AuthMain)
        .map(|f| f.path.display().to_string())
        .unwrap_or_default()
}

/// 桌面端版本号：取明文回显里的 `version`（桌面端自己写的，比猜安装目录权威）。
fn desktop_version(roots: &PathRoots, v: QoderVariant) -> String {
    let path = cli_dir(roots, v).join(".qoder-app-status.json");
    std::fs::read_to_string(path)
        .ok()
        .and_then(|t| serde_json::from_str::<Value>(&t).ok())
        .and_then(|j| j.get("version").and_then(|x| x.as_str()).map(String::from))
        .unwrap_or_default()
}

/// 前端 `AppStatus`。
pub fn app_status(roots: &PathRoots, v: QoderVariant) -> Value {
    let auth = auth_codec::read_desktop_auth(roots, v).ok();
    json!({
        "running": !process::running_pids(QoderTarget::Desktop.images(v)).is_empty(),
        "authFile": auth_file_path(roots, v),
        "current": auth.as_ref().map(|a| json!({
            "uid": a.user.id,
            "nickname": a.user.name,
            "email": a.user.email,
        })),
        "appPath": executable(roots, v, QoderTarget::Desktop)
            .map(|p| p.display().to_string())
            .unwrap_or_default(),
        "version": desktop_version(roots, v),
        "variant": variant_key(v),
    })
}

/// 前端 `AccountMeta`。`expiresAt` 一类是毫秒时间戳。
pub fn account_meta(b: &bundle::Bundle) -> Value {
    json!({
        "id": b.account_id,
        "uid": b.identity.uid,
        "email": b.identity.email,
        "nickname": b.identity.name,
        // Qoder 侧没有企业账号概念，保留键以符合契约。
        "enterpriseName": null,
        "expiresAt": b.identity.expires_at.as_deref().and_then(bundle::iso_to_ms),
        "refreshExpiresAt": b.identity.refresh_expires_at.as_deref().and_then(bundle::iso_to_ms),
        "refreshedAt": null,
        "createdAt": bundle::compact_to_ms(&b.created_at),
        // 解不出登录态的包等同于要重新登录：可能是跨 Windows 用户搬过来的包。
        "needsRelogin": b.identity.uid.is_none(),
        "needsReloginReason": b.identity.uid.is_none().then(|| {
            "认领时未能解密登录态（跨 Windows 用户或 Local State 不匹配），需在本机重新登录一次"
                .to_string()
        }),
        "variant": variant_key(b.variant),
    })
}

pub fn accounts(roots: &PathRoots) -> Value {
    let list = bundle::list_all(&switch_root());
    json!({
        "accounts": list.iter().map(account_meta).collect::<Vec<_>>(),
        // 前端只读 accounts；status 单独取，避免一次请求里混两种形状。
        "status": app_status(roots, QoderVariant::Cn),
    })
}

/// 导出记录：一条里同时给身份字段（预览要展示）和 `payload`（导入只用它）。
///
/// 两个宿主共用这个函数而不是各写一份 —— 桌面端原先自己拼了一遍记录，
/// 结果 `capabilities` 的措辞和轮换阈值的默认值都已经和 webui 那份分叉了。
pub fn export_records(store: &Path, ids: &[String]) -> Result<Value, String> {
    let mut records = Vec::new();
    let mut warnings = Vec::new();
    for id in ids {
        match export_import::export_account(store, id).and_then(|e| export_import::to_bytes(&e)) {
            Ok(bytes) => {
                let text = String::from_utf8_lossy(&bytes).to_string();
                let parsed: Value = serde_json::from_str(&text).unwrap_or_else(|_| json!({}));
                let bundle0 = parsed
                    .get("bundles")
                    .and_then(|b| b.as_array())
                    .and_then(|a| a.first().cloned())
                    .unwrap_or_else(|| json!({}));
                let ident = bundle0.get("identity").cloned().unwrap_or(json!({}));
                records.push(json!({
                    "id": id,
                    "uid": ident.get("uid"),
                    "nickname": ident.get("name"),
                    "email": ident.get("email"),
                    "variant": variant_key(variant_from_key(
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
            Err(e) => warnings.push(format!("{id}: {e}")),
        }
    }
    if records.is_empty() {
        return Err(if warnings.is_empty() {
            "没有可导出的账号".into()
        } else {
            warnings.join("; ")
        });
    }
    Ok(json!({ "ok": true, "accounts": records, "warnings": warnings }))
}

/// 备份文件里的记录数组：既接受裸数组，也接受 `{accounts:[...]}`。
fn importable_records(file_text: &str) -> Result<Vec<Value>, String> {
    let v: Value =
        serde_json::from_str(file_text).map_err(|e| format!("备份文件不是合法 JSON: {e}"))?;
    Ok(v
        .as_array()
        .cloned()
        .or_else(|| v.get("accounts").and_then(|x| x.as_array()).cloned())
        .unwrap_or_default())
}

/// 预览导入：只解析与校验，不写盘。
pub fn preview_import(file_text: &str) -> Result<Value, String> {
    let arr = importable_records(file_text)?;
    let accounts: Vec<Value> = arr
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

/// 执行导入。`indexes` 是用户在预览里勾选的下标，缺省表示全部。
pub fn import_records(
    store: &Path,
    file_text: &str,
    indexes: Option<&[usize]>,
) -> Result<Value, String> {
    let arr = importable_records(file_text)?;
    let picked: Vec<&Value> = match indexes {
        Some(ix) => ix.iter().filter_map(|i| arr.get(*i)).collect(),
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
            .map(|id| bundle::accounts_root_in(store).join(id).is_dir())
            .unwrap_or(false);
        let r = export_import::import(store, payload.as_bytes(), true)
            .map_err(|e| format!("导入失败: {e}"))?;
        imported += r.written.len();
        skipped += r.skipped.len();
        if existed {
            overwritten += 1;
        }
    }
    Ok(json!({
        "ok": imported > 0,
        "imported": imported,
        "skipped": skipped,
        "overwritten": overwritten,
    }))
}

/// 切换结果。`shareSessions` 在 Qoder 侧没有对应机制，必须在 message 里说清"没做"，
/// 而不是收下参数静默忽略 —— 用户会以为会话已经跟着迁走了。
pub fn switch_result(j: &switch::Journal, restart: bool, ignored_session: bool) -> Value {
    let mut message = format!("已切到 {}（{:?}）", j.account_id, j.phase);
    if ignored_session {
        message.push_str("；会话复制未执行 —— Qoder 的会话不按账号归属，跨账号复制会串数据");
    }
    json!({
        "ok": j.phase == switch::Phase::Completed,
        "accountId": j.account_id,
        "restarted": restart,
        "message": message,
    })
}

/// 前端 `AutoRotateConfig`。键是 snake_case —— 那是上游契约的原样。
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
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
        let d = rotate::RotateConfig::default();
        Self {
            enabled: true,
            check_interval_minutes: 60,
            cooldown_minutes: 120,
            min_gap_hours: (d.min_gap_days * 24).max(24) as u32,
            min_urgency_hours: (d.min_urgency_days * 24).max(24) as u32,
            active_guard_minutes: 0,
            min_remaining_credits: 0,
        }
    }
}

impl UiRotateConfig {
    /// 前端的"小时"阈值换成本地判定用的"天"。至少 1 天，免得换算成 0 后每次都想切。
    pub fn to_core(&self) -> rotate::RotateConfig {
        rotate::RotateConfig {
            min_urgency_days: (self.min_urgency_hours as i64).div_euclid(24).max(1),
            min_gap_days: (self.min_gap_hours as i64).div_euclid(24).max(1),
        }
    }
}

fn ui_config_path(store: &Path) -> std::path::PathBuf {
    store.join("auto_rotate_config.json")
}

pub fn read_ui_config(store: &Path) -> UiRotateConfig {
    std::fs::read(ui_config_path(store))
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or_default()
}

pub fn write_ui_config(store: &Path, cfg: &UiRotateConfig) -> crate::Result<()> {
    std::fs::create_dir_all(store).map_err(|e| e.to_string())?;
    let json = serde_json::to_vec_pretty(cfg).map_err(|e| e.to_string())?;
    std::fs::write(ui_config_path(store), json).map_err(|e| format!("写轮换配置失败: {e}"))
}

/// 把前端传来的部分字段并进现有配置并落盘，返回合并后的完整配置。
/// 键名沿用上游契约的 snake_case，所以这里按字面量逐个认。
pub fn merge_ui_config(store: &Path, patch: &Value) -> crate::Result<UiRotateConfig> {
    let mut cur = read_ui_config(store);
    if let Some(b) = patch.get("enabled").and_then(|x| x.as_bool()) {
        cur.enabled = b;
    }
    for key in [
        "check_interval_minutes",
        "cooldown_minutes",
        "min_gap_hours",
        "min_urgency_hours",
    ] {
        if let Some(n) = patch.get(key).and_then(|x| x.as_u64()) {
            let n = n as u32;
            match key {
                "check_interval_minutes" => cur.check_interval_minutes = n,
                "cooldown_minutes" => cur.cooldown_minutes = n,
                "min_gap_hours" => cur.min_gap_hours = n,
                _ => cur.min_urgency_hours = n,
            }
        }
    }
    write_ui_config(store, &cur)?;
    Ok(cur)
}

/// 前端 `RotateStatus`。`cliConfigured` 恒为 false：Qoder CLI 不落盘凭据，
/// 不存在"CLI 默认账号指针"这回事 —— 换桌面端即随之生效。
pub fn rotate_status(roots: &PathRoots, store: &Path, v: QoderVariant) -> Value {
    let cur = auth_codec::read_desktop_auth(roots, v).ok();
    json!({
        "config": read_ui_config(store),
        "cliConfigured": false,
        "activeAccountId": cur.as_ref().map(|a| a.user.id.clone()),
        "activeAccountName": cur.as_ref().map(|a| a.user.name.clone()),
        "lastCheckAt": bundle::compact_to_ms(&now_ts()),
        "lastSwitchAt": rotate::read_state(store)
            .ok()
            .and_then(|st| st.last_suggested_at)
            .and_then(|t| bundle::compact_to_ms(&t)),
    })
}

/// 轮换日志。`action` 只可能是 `suggest` —— 本实现不自动执行切换。
pub fn rotate_logs(store: &Path) -> Value {
    let logs = rotate::read_state(store)
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
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    json!({ "logs": logs })
}

/// 手动跑一次轮换检查。返回状态只允许 suggested / hold / error。
pub fn run_rotate(roots: &PathRoots, store: &Path, v: QoderVariant) -> Value {
    let cfg = read_ui_config(store).to_core();
    match rotate::suggest(roots, store, v, &cfg) {
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

/// 前端逐项确认"哪些能力在 Qoder 侧不存在"，用于把"不适用"写明而不是装作能用。
pub fn capabilities() -> Value {
    json!({
        "supported": [
            "账号包认领与列表",
            "一键切换（备份、写后校验、失败回滚）",
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

use std::path::Path;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn variant_keys_match_frontend_contract() {
        assert_eq!(variant_key(QoderVariant::Cn), "cn");
        assert_eq!(variant_key(QoderVariant::Global), "ai");
        assert_eq!(variant_from_key(Some("ai")), QoderVariant::Global);
        assert_eq!(variant_from_key(None), QoderVariant::Cn);
        assert_eq!(variant_from_key(Some("junk")), QoderVariant::Cn);
    }

    #[test]
    fn app_status_keys_are_camel_case() {
        let v = app_status(&PathRoots::real(), QoderVariant::Cn);
        for k in ["running", "authFile", "current", "appPath", "version", "variant"] {
            assert!(v.get(k).is_some(), "前端契约要求键 {k}");
        }
        assert!(v["authFile"].as_str().unwrap().ends_with("auth.v1.dat"));
    }

    #[test]
    fn account_meta_keys_are_camel_case() {
        let b = bundle::Bundle {
            account_id: "a".into(),
            variant: QoderVariant::Cn,
            target: QoderTarget::Desktop,
            created_at: "20260920T010101Z".into(),
            members: vec![],
            identity: bundle::Identity {
                uid: Some("u1".into()),
                expires_at: Some("2026-10-19T06:19:41Z".into()),
                name: Some("n".into()),
                ..Default::default()
            },
        };
        let v = account_meta(&b);
        for k in [
            "id",
            "uid",
            "email",
            "nickname",
            "enterpriseName",
            "expiresAt",
            "refreshExpiresAt",
            "refreshedAt",
            "createdAt",
            "needsRelogin",
            "needsReloginReason",
            "variant",
        ] {
            assert!(v.get(k).is_some(), "缺键 {k}");
        }
        assert_eq!(v["needsRelogin"], false);
        assert!(v["expiresAt"].as_u64().unwrap() > 1_700_000_000_000);
        assert!(v["createdAt"].as_u64().unwrap() > 1_700_000_000_000);
    }

    #[test]
    fn missing_uid_is_flagged_not_hidden() {
        let b = bundle::Bundle {
            account_id: "x".into(),
            variant: QoderVariant::Global,
            target: QoderTarget::Desktop,
            created_at: "20260920T010101Z".into(),
            members: vec![],
            identity: bundle::Identity::default(),
        };
        let v = account_meta(&b);
        assert_eq!(v["needsRelogin"], true);
        assert!(v["needsReloginReason"].is_string(), "要给出可解释的原因");
    }

    #[test]
    fn hour_thresholds_never_collapse_to_zero_days() {
        let cfg = UiRotateConfig {
            min_urgency_hours: 3,
            min_gap_hours: 1,
            ..Default::default()
        };
        let core = cfg.to_core();
        assert_eq!(core.min_urgency_days, 1, "不足一天也要按一天算");
        assert_eq!(core.min_gap_days, 1);
    }

    #[test]
    fn ui_config_roundtrips_to_disk() {
        let dir = std::env::temp_dir().join(format!("qs-view-{}", uuid::Uuid::new_v4().simple()));
        std::fs::create_dir_all(&dir).unwrap();
        assert_eq!(read_ui_config(&dir).min_gap_hours, UiRotateConfig::default().min_gap_hours, "无文件回退默认");
        let mut c = UiRotateConfig::default();
        c.enabled = false;
        c.min_urgency_hours = 48;
        write_ui_config(&dir, &c).unwrap();
        let back = read_ui_config(&dir);
        assert_eq!(back.enabled, false);
        assert_eq!(back.min_urgency_hours, 48);
        std::fs::remove_dir_all(dir).ok();
    }

    /// 轮换永不声称已经切换 —— 自动执行会重启用户的 IDE。
    #[test]
    fn run_rotate_only_ever_suggests() {
        let dir = std::env::temp_dir().join(format!("qs-rot-{}", uuid::Uuid::new_v4().simple()));
        std::fs::create_dir_all(&dir).unwrap();
        let v = run_rotate(&PathRoots::real(), &dir, QoderVariant::Cn);
        let s = v["status"].as_str().unwrap_or("");
        assert!(
            matches!(s, "suggested" | "hold" | "error"),
            "只允许建议，实得 {v}"
        );
        let l = rotate_logs(&dir);
        assert!(l["logs"].is_array());
        let st = rotate_status(&PathRoots::real(), &dir, QoderVariant::Cn);
        assert_eq!(st["cliConfigured"], false, "Qoder 无 CLI 指针机制");
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn capabilities_names_reasons() {
        let v = capabilities();
        assert!(v["unavailable"].as_array().unwrap().len() >= 6);
        assert!(v["supported"].as_array().unwrap().iter().any(|x| x
            .as_str()
            .unwrap_or("")
            .contains("切换")));
    }

    /// 真机：accounts 视图必须带上从密文里解出的到期时间。
    #[test]
    fn real_accounts_carry_expiry() {
        let v = accounts(&PathRoots::real());
        let arr = v["accounts"].as_array().cloned().unwrap_or_default();
        if arr.is_empty() {
            eprintln!("NOTE: 本机暂无账号包");
            return;
        }
        assert!(arr.iter().any(|a| a["expiresAt"].is_number()));
    }
}
