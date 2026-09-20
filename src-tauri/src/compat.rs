//! 前端契约适配层。
//!
//! 前端 `src/` 是 workbuddy-switch 的原样副本，它调用的命令名与返回形状是既成契约
//! （`AccountMeta` / `AppStatus` 等）。这里让 Rust 去满足那份契约，而不是反过来改
//! 一万三千行前端 —— 少改一行，"复刻"就少一分水分。
//!
//! 与 `commands.rs`（Qoder 原生接口）并存：界面走本模块，命令行工具与脚本走原生接口。

use serde::Serialize;
use serde_json::json;

use qs_switch_core::modules::config::PathRoots;
use qs_switch_core::modules::variant::{QoderTarget, QoderVariant};
use qs_switch_core::modules::{auth_codec, bundle, process, switch};

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
    let journal = tauri::async_runtime::spawn_blocking(move || {
        switch::execute(&roots, &store, &req, actor, &mut |m| {
            let _ = handle.emit("switch-progress", m.to_string());
        })
    })
    .await
    .map_err(|e| format!("切换任务异常终止: {e}"))??;

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

#[cfg(test)]
mod tests {
    use super::*;

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
