//! 前端契约适配层（桌面宿主）。
//!
//! 前端 `src/` 是 workbuddy-switch 的原样副本，它调用的命令名与返回形状是既成契约
//! （`AccountMeta` / `AppStatus` 等）。这里让 Rust 去满足那份契约，而不是反过来改
//! 一万三千行前端 —— 少改一行，"复刻"就少一分水分。
//!
//! **本模块不自己拼 JSON 形状**：所有展示数据一律走 `core::view`，与 webui 宿主
//! （`crates/qs-switch-server/src/router.rs` 的 compat 路由）返回同一份对象。
//! 之前两边各写了一遍，`capabilities` 的措辞和轮换阈值的默认值已经悄悄分叉。
//! 本模块只剩桌面宿主特有的部分：命令注册、进度状态、事件推送。
//!
//! 与 `commands.rs`（Qoder 原生接口）并存：界面走本模块，命令行工具与脚本走原生接口。

use serde_json::{Value, json};

use qs_switch_core::modules::config::{PathRoots, switch_root};
use qs_switch_core::modules::variant::{QoderTarget, QoderVariant};
use qs_switch_core::modules::{bundle, notifications, switch, update, view};

pub use view::UiRotateConfig;

fn variant_of(s: Option<&str>) -> QoderVariant {
    view::variant_from_key(s)
}

fn target_of(s: Option<&str>) -> QoderTarget {
    match s {
        Some("cli") => QoderTarget::Cli,
        Some("work") => QoderTarget::Work,
        _ => QoderTarget::Desktop,
    }
}

/// 把阻塞式采集挪出主线程。
///
/// Tauri 的非 `async` 命令在**主线程**上执行，而 `get_status` / `get_accounts` 每次都要
/// 起 `tasklist` 与 PowerShell 做 DPAPI（本机实测各约 700ms）。同步写法的直接后果是
/// 窗口在那 0.7 秒里完全不收输入 —— 界面看起来就是"卡死"。
async fn off_main<T: Send + 'static>(
    f: impl FnOnce() -> Result<T, String> + Send + 'static,
) -> Result<T, String> {
    tauri::async_runtime::spawn_blocking(f)
        .await
        .map_err(|e| format!("后台采集任务异常终止: {e}"))?
}

#[tauri::command]
pub async fn get_status(variant: Option<String>) -> Result<Value, String> {
    off_main(move || Ok(view::app_status(&PathRoots::real(), variant_of(variant.as_deref())))).await
}

/// 返回全部档位的账号，由前端按 `variant` 过滤（契约如此，不在宿主侧筛）。
#[tauri::command]
pub async fn get_accounts() -> Result<Value, String> {
    off_main(|| Ok(view::accounts(&PathRoots::real()))).await
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
pub fn switch_progress(cell: tauri::State<'_, ProgressCell>) -> Value {
    let g = cell.0.lock().unwrap_or_else(|p| p.into_inner());
    json!({ "running": g.running, "progress": g.progress })
}

/// 前端签名：switchAccount({ accountId, restart, shareSessions, ... , variant })。
/// 会话复制（shareSessions）在 Qoder 侧没有可用的归属机制，故意忽略并在 message 里说明。
#[tauri::command]
pub async fn switch_account(
    app: tauri::AppHandle,
    cell: tauri::State<'_, ProgressCell>,
    args: Value,
) -> Result<Value, String> {
    use tauri::Emitter;
    let account_id = args
        .get("accountId")
        .and_then(|x| x.as_str())
        .ok_or_else(|| "缺 accountId".to_string())?
        .to_string();
    let variant = variant_of(args.get("variant").and_then(|x| x.as_str()));
    let target = target_of(args.get("target").and_then(|x| x.as_str()));
    let restart = args.get("restart").and_then(|x| x.as_bool()).unwrap_or(true);
    let forced = args.get("forced").and_then(|x| x.as_bool()).unwrap_or(false);
    let ignored_session = args
        .get("shareSessions")
        .and_then(|x| x.as_bool())
        .unwrap_or(false);

    let roots = PathRoots::real();
    let store = switch_root();
    let req = switch::Request {
        account_id,
        variant,
        target,
        restart,
    };
    let actor = if forced {
        switch::Actor::RealForced
    } else {
        switch::Actor::Real
    };
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

    Ok(view::switch_result(&journal, restart, ignored_session))
}

/// 认领本机现有登录态为一个账号包。
/// `account_id` 可选：前端"导入本机账号"只送档位（它那时还不知道本机登的是谁），
/// 缺省包名由档位推出。
#[tauri::command]
pub async fn import_local(
    account_id: Option<String>,
    variant: Option<String>,
    target: Option<String>,
) -> Result<Value, String> {
    off_main(move || {
        let roots = PathRoots::real();
        let store = switch_root();
        let v = variant_of(variant.as_deref());
        let t = target_of(target.as_deref());
        let id = account_id.unwrap_or_else(|| view::local_account_id(v));
        let b = bundle::capture(&roots, &store, &id, v, t)?;
        if b.is_empty() {
            return Err(format!(
                "该目标在本机不落盘凭据（{:?}·{:?}），没有可认领的文件",
                v, t
            ));
        }
        Ok(json!({ "ok": true, "account": view::account_meta(&b) }))
    })
    .await
}

#[tauri::command]
pub async fn delete_account(account_id: String) -> Result<Value, String> {
    off_main(move || {
        let dir = bundle::accounts_root_in(&switch_root()).join(&account_id);
        if !dir.is_dir() {
            return Err(format!("账号目录不存在: {}", dir.display()));
        }
        std::fs::remove_dir_all(&dir).map_err(|e| format!("删除 {} 失败: {e}", dir.display()))?;
        Ok(json!({ "ok": true }))
    })
    .await
}

#[tauri::command]
pub fn get_auto_rotate_config() -> UiRotateConfig {
    view::read_ui_config(&switch_root())
}

#[tauri::command]
pub fn save_auto_rotate_config(config: Value) -> Result<UiRotateConfig, String> {
    view::merge_ui_config(&switch_root(), &config)
}

#[tauri::command]
pub async fn rotate_status(variant: Option<String>) -> Result<Value, String> {
    off_main(move || {
        Ok(view::rotate_status(
            &PathRoots::real(),
            &switch_root(),
            variant_of(variant.as_deref()),
        ))
    })
    .await
}

/// 手动跑一次轮换检查。只产出建议并记日志，**不执行切换**。
#[tauri::command]
pub async fn run_rotate(variant: Option<String>) -> Result<Value, String> {
    off_main(move || {
        Ok(view::run_rotate(
            &PathRoots::real(),
            &switch_root(),
            variant_of(variant.as_deref()),
        ))
    })
    .await
}

#[tauri::command]
pub fn get_rotate_logs() -> Value {
    view::rotate_logs(&switch_root())
}

#[tauri::command]
pub async fn export_accounts(account_ids: Vec<String>) -> Result<Value, String> {
    off_main(move || view::export_records(&switch_root(), &account_ids)).await
}

#[tauri::command]
pub async fn export_accounts_to_path(
    account_ids: Vec<String>,
    path: String,
) -> Result<Value, String> {
    off_main(move || {
        let v = view::export_records(&switch_root(), &account_ids)?;
        let text = serde_json::to_string_pretty(&v["accounts"]).map_err(|e| e.to_string())?;
        std::fs::write(&path, text).map_err(|e| format!("写 {path} 失败: {e}"))?;
        Ok(json!({ "ok": true, "path": path }))
    })
    .await
}

/// 预览导入文件：只解析与校验，不写盘。
#[tauri::command]
pub async fn preview_import_accounts(file_text: String) -> Result<Value, String> {
    off_main(move || view::preview_import(&file_text)).await
}

/// 导入。`indexes` 是用户在预览里勾选的下标。
#[tauri::command]
pub async fn import_accounts(
    file_text: String,
    indexes: Option<Vec<usize>>,
) -> Result<Value, String> {
    off_main(move || {
        view::import_records(&switch_root(), &file_text, indexes.as_deref())
    })
    .await
}

/// 前端逐项确认"哪些能力在 Qoder 侧不存在"，用于在界面上写明而不是装作能用。
#[tauri::command]
pub fn get_capabilities() -> Value {
    view::capabilities()
}

/// 通知存档（`~/.qs-switch/notifications.json`，最近 100 条，新的在前）。
/// 前端所有 toast 都会同步写一份；写入失败不影响提示本身（notify.ts 静默忽略）。
#[tauri::command]
pub async fn list_notifications() -> Result<Value, String> {
    Ok(view::notifications(notifications::list()?))
}

#[tauri::command]
pub async fn record_notification(
    level: String,
    title: String,
    description: Option<String>,
) -> Result<(), String> {
    notifications::record(&level, &title, description.as_deref())
}

#[tauri::command]
pub async fn clear_notifications() -> Result<(), String> {
    notifications::clear()
}

// ---------------------------------------------------------------------------
// 自动更新（版本检查走 core 的 update 模块；下载安装走前端 tauri-plugin-updater）
// ---------------------------------------------------------------------------

/// 更新源配置（owner/repo/proxy）。
#[tauri::command]
pub fn get_github_config() -> Value {
    update::load_github_config()
}

#[tauri::command]
pub fn save_github_config(config: Value) -> Result<Value, String> {
    update::save_github_config(&config).map_err(|e| e.to_string())?;
    Ok(update::load_github_config())
}

/// 检查 GitHub Releases 是否有新版本。force=true 绕过 6 小时缓存。
#[tauri::command]
pub async fn check_update(proxy: Option<String>, force: Option<bool>) -> Value {
    update::update_check(proxy.as_deref(), force.unwrap_or(false)).await
}

/// 更新安装完成后的立即重启。
///
/// 用框架受管的 [`tauri::AppHandle::restart`]，而不是手写 spawn+exit。
/// 已知边缘：restart 会原样保留 argv——若本次进程是自启带 `--hidden` 拉起的，
/// 重启后仍处于静默驻留（可从托盘唤起主界面），不影响更新本身。
#[tauri::command]
pub fn relaunch_app(_app: tauri::AppHandle) {
    _app.restart();
}

#[cfg(test)]
mod tests {
    use super::*;

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
        let r = tauri::async_runtime::block_on(run_rotate(Some("cn".into()))).unwrap();
        let status = r.get("status").and_then(|x| x.as_str()).unwrap_or("");
        assert!(
            matches!(status, "suggested" | "hold" | "error"),
            "轮换只允许产出建议，实得 {r}"
        );
        assert_ne!(status, "switched");
    }

    #[test]
    fn rotate_logs_and_status_are_wellformed() {
        let s = tauri::async_runtime::block_on(rotate_status(Some("cn".into()))).unwrap();
        assert_eq!(s["cliConfigured"], false, "Qoder 无 CLI 指针机制");
        assert!(s.get("config").is_some());
        assert!(get_rotate_logs()["logs"].is_array());
    }

    #[test]
    fn save_config_keeps_unmentioned_fields() {
        let dir = std::env::temp_dir().join(format!("qs-compat-cfg-{}", std::process::id()));
        std::fs::remove_dir_all(&dir).ok();
        std::fs::create_dir_all(&dir).unwrap();
        let base = view::read_ui_config(&dir);
        let merged = view::merge_ui_config(&dir, &json!({ "enabled": false })).unwrap();
        assert_eq!(merged.enabled, false);
        assert_eq!(
            merged.min_gap_hours, base.min_gap_hours,
            "没提到的字段不能被写没"
        );
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn preview_import_rejects_garbage() {
        assert!(tauri::async_runtime::block_on(preview_import_accounts("不是 JSON".into())).is_err());
        let ok = tauri::async_runtime::block_on(preview_import_accounts(r#"[{"id":"a","uid":"u"}]"#.into())).unwrap();
        assert_eq!(ok["total"], 1);
    }

    #[test]
    fn variant_keys_match_frontend_contract() {
        assert_eq!(view::variant_key(QoderVariant::Cn), "cn");
        assert_eq!(view::variant_key(QoderVariant::Global), "ai");
        assert_eq!(variant_of(Some("ai")), QoderVariant::Global);
        assert_eq!(variant_of(None), QoderVariant::Cn);
        assert_eq!(variant_of(Some("junk")), QoderVariant::Cn);
    }

    /// 本机现状必须能被 get_status 说清楚：在跑、认证文件路径、当前账号。
    #[test]
    fn status_reflects_local_reality() {
        let s = tauri::async_runtime::block_on(get_status(Some("cn".into()))).unwrap();
        assert_eq!(s["variant"], "cn");
        let file = s["authFile"].as_str().unwrap_or_default();
        assert!(file.ends_with("auth.v1.dat"), "{file}");
        if std::path::Path::new(file).is_file() {
            let uid = s["current"]["uid"].as_str().unwrap_or_default();
            assert!(!uid.is_empty(), "有凭据文件就该解出当前账号");
        }
    }

    #[test]
    fn accounts_list_carries_expiry() {
        let arr = tauri::async_runtime::block_on(get_accounts()).unwrap()["accounts"]
            .as_array()
            .cloned()
            .unwrap_or_default();
        // 本机已认领过账号，列表不该为空；到期时间是判断"该切谁"的依据。
        if !arr.is_empty() {
            assert!(arr.iter().any(|a| a.get("expiresAt").is_some()));
        }
    }

    /// 两个宿主必须逐字同源：桌面走本模块，webui 走 core::view，这里比对导出形状。
    #[test]
    fn desktop_and_webui_agree_on_contract_shapes() {
        let roots = PathRoots::real();
        assert_eq!(
            tauri::async_runtime::block_on(get_status(Some("ai".into()))).unwrap(),
            view::app_status(&roots, QoderVariant::Global));
        assert_eq!(get_capabilities(), view::capabilities());
        assert_eq!(get_rotate_logs(), view::rotate_logs(&switch_root()));
    }
}
