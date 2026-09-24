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
///
/// 刻意保持同步（留在主线程）：它只读一个进程内 cell，没有文件或进程 IO。
/// 前端每 600ms 轮询一次，若为它走 spawn_blocking，反而每次都要跨线程调度，
/// 得不偿失。真正要挪出主线程的是**有 IO** 的那些读命令（见本文件其它 `off_main`）。
#[tauri::command]
pub fn switch_progress(cell: tauri::State<'_, ProgressCell>) -> Value {
    let g = cell.0.lock().unwrap_or_else(|p| p.into_inner());
    json!({ "running": g.running, "progress": g.progress })
}

/// 前端签名：switchAccount({ accountId, restart, shareSessions, variant })。
/// 会话复制（shareSessions）在 Qoder 侧没有可用的归属机制，故意忽略并在 message 里说明。
///
/// 参数必须是**命名参数**：Tauri v2 按参数名从 invoke 载荷里逐键取值，`args: Value`
/// 会要求载荷里有个叫 "args" 的键 —— 前端发的是扁平对象，之前这样写等于桌面端切换
/// 永远报 `missing required key args`（测试测不到宏参数绑定，真机一点就炸）。
/// snake_case 在 Tauri 侧自动对上 camelCase 键（accountId 等）。
#[tauri::command]
pub async fn switch_account(
    app: tauri::AppHandle,
    cell: tauri::State<'_, ProgressCell>,
    account_id: String,
    restart: Option<bool>,
    forced: Option<bool>,
    share_sessions: Option<bool>,
    target: Option<String>,
    variant: Option<String>,
) -> Result<Value, String> {
    use tauri::Emitter;
    let variant = variant_of(variant.as_deref());
    let target = target_of(target.as_deref());
    let restart = restart.unwrap_or(true);
    let forced = forced.unwrap_or(false);
    let ignored_session = share_sessions.unwrap_or(false);

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
        Ok(Ok(j)) => {
            let _ = crate::tray::refresh_tray_menu(&app);
            j
        }
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
    app: tauri::AppHandle,
    account_id: Option<String>,
    variant: Option<String>,
    target: Option<String>,
) -> Result<Value, String> {
    let res = off_main(move || {
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
        // 账号库刚多了一个包 —— 立刻存一份到文档目录。备份失败不影响认领本身。
        let _ = qs_switch_core::modules::export_import::auto_backup_default(&store);
        Ok(json!({ "ok": true, "account": view::account_meta(&b) }))
    })
    .await?;
    let _ = crate::tray::refresh_tray_menu(&app);
    Ok(res)
}

#[tauri::command]
pub async fn delete_account(app: tauri::AppHandle, account_id: String) -> Result<Value, String> {
    let res = off_main(move || {
        // account_id 是 store 路径的组成部分，delete 又是 remove_dir_all ——
        // 不校验就是"一次 invoke 删任意目录"的原语（绝对路径 join 会整体替换基目录）。
        bundle::validate_account_id(&account_id)?;
        let dir = bundle::accounts_root_in(&switch_root()).join(&account_id);
        // 深度防御：即便 id 本身合法，也拒绝删除符号链接/junction（防止库内条目
        // 被换成指向别处的链接后被整树删除）。
        let meta = std::fs::symlink_metadata(&dir)
            .map_err(|e| format!("账号目录不存在: {e}"))?;
        if meta.is_symlink() || !meta.is_dir() {
            return Err(format!("账号目录不存在或不是真实目录: {}", dir.display()));
        }
        // **先备份再删**：删账号不可逆，而账号包是不可再生的凭据副本（现场只保留
        // 当前登录的那一个，其余丢了只能重新扫码）。这一份备份里还带着即将被删的包。
        let _ = qs_switch_core::modules::export_import::auto_backup_default(&switch_root());
        std::fs::remove_dir_all(&dir).map_err(|e| format!("删除 {} 失败: {e}", dir.display()))?;
        Ok(json!({ "ok": true }))
    })
    .await?;
    let _ = crate::tray::refresh_tray_menu(&app);
    Ok(res)
}

#[tauri::command]
pub async fn set_account_proxy(
    account_id: String,
    proxy: Option<String>,
    variant: Option<String>,
) -> Result<Value, String> {
    off_main(move || {
        let store = switch_root();
        // 档位必须由调用方下发：`set_proxy` 按 (accountId, variant, target) 定位账号包，
        // 写死 Cn 会把国际版账号的代理写到国内版那份包上，或直接报"账号不存在"。
        let v = variant_of(variant.as_deref());
        let b = bundle::set_proxy(&store, &account_id, v, QoderTarget::Desktop, proxy)?;
        Ok(json!({ "ok": true, "account": view::account_meta(&b) }))
    })
    .await
}

#[tauri::command]
pub async fn get_auto_rotate_config() -> Result<UiRotateConfig, String> {
    off_main(|| Ok(view::read_ui_config(&switch_root()))).await
}

#[tauri::command]
pub async fn save_auto_rotate_config(config: Value) -> Result<UiRotateConfig, String> {
    off_main(move || view::merge_ui_config(&switch_root(), &config)).await
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
pub async fn get_rotate_logs() -> Result<Value, String> {
    off_main(|| Ok(view::rotate_logs(&switch_root()))).await
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
        let p = std::path::Path::new(&path);
        if path.trim().is_empty() {
            return Err("导出文件路径不能为空".into());
        }
        if let Some(parent) = p.parent() {
            if !parent.as_os_str().is_empty() {
                std::fs::create_dir_all(parent).map_err(|e| format!("创建目标目录失败: {e}"))?;
            }
        }
        qs_switch_core::modules::config::atomic_write_bytes(p, text.as_bytes())
            .map_err(|e| format!("写 {path} 失败: {e}"))?;
        // 部分账号解包失败会进 warnings（records 非空则整体不算错）；必须原样回传，
        // 否则"勾 3 备 2"被报成成功导出 3 个 —— 对凭据备份等于静默少备。
        let exported = v["accounts"].as_array().map(|a| a.len()).unwrap_or(0);
        Ok(json!({
            "ok": true,
            "path": path,
            "exported": exported,
            "warnings": v["warnings"].clone(),
        }))
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
    app: tauri::AppHandle,
    file_text: String,
    indexes: Option<Vec<usize>>,
) -> Result<Value, String> {
    let res = off_main(move || {
        let store = switch_root();
        let r = view::import_records(&store, &file_text, indexes.as_deref());
        // 无论成败都备份：导入中途失败会留下写了一半的分片，那个状态同样值得留档。
        let _ = qs_switch_core::modules::export_import::auto_backup_default(&store);
        r
    })
    .await?;
    let _ = crate::tray::refresh_tray_menu(&app);
    Ok(res)
}

/// 备份现状。账号库空了但备份里还有账号时，前端据此显示"可从备份恢复"的提示条。
#[tauri::command]
pub async fn get_backup_status() -> Result<Value, String> {
    off_main(|| Ok(view::backup_status(&switch_root()))).await
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
pub async fn get_credit_expiry(account_id: String, variant: Option<String>) -> Value {
    let roots = PathRoots::real();
    let store = switch_root();
    let v = variant_of(variant.as_deref());
    qs_switch_core::modules::quota::fetch_credit_expiry(&roots, &store, &account_id, v).await
}

#[tauri::command]
pub async fn get_checkin_status(account_id: String, variant: Option<String>) -> Value {
    let roots = PathRoots::real();
    let store = switch_root();
    let v = variant_of(variant.as_deref());
    qs_switch_core::modules::quota::get_checkin_status(&roots, &store, &account_id, v).await
}

#[tauri::command]
pub async fn checkin(account_id: String, variant: Option<String>) -> Value {
    let roots = PathRoots::real();
    let store = switch_root();
    let v = variant_of(variant.as_deref());
    qs_switch_core::modules::quota::checkin(&roots, &store, &account_id, v).await
}

#[tauri::command]
pub async fn checkin_all(variant: Option<String>) -> Value {
    let roots = PathRoots::real();
    let store = switch_root();
    // 不传档位 = 全部档位（设置页的"全部立即签到"覆盖两档），传了就只处理该档。
    let only = variant.as_deref().map(|v| variant_of(Some(v)));
    qs_switch_core::modules::quota::checkin_all(&roots, &store, only).await
}

#[tauri::command]
pub fn oauth_start(variant: Option<String>) -> Value {
    let v = variant_of(variant.as_deref());
    qs_switch_core::modules::oauth::oauth_start(v)
}

#[tauri::command]
pub async fn oauth_status(login_id: String) -> Value {
    let roots = PathRoots::real();
    let store = switch_root();
    qs_switch_core::modules::oauth::oauth_status(&login_id, &roots, &store).await
}

/// 积分统计：本机配额快照聚合（无官方用量端点）。`refresh=true` 先拉一轮真实配额。
#[tauri::command]
pub async fn get_credit_statistics(refresh: Option<bool>) -> Value {
    let roots = PathRoots::real();
    let store = switch_root();
    off_main(move || Ok(qs_switch_core::modules::ledger::credit_statistics(&roots, &store, refresh.unwrap_or(false))))
        .await
        .unwrap_or_else(|_| json!({ "error": "统计聚合任务异常终止" }))
}

#[tauri::command]
pub async fn get_auto_checkin_config() -> Result<Value, String> {
    off_main(|| Ok(qs_switch_core::modules::ledger::read_checkin_config(&switch_root()))).await
}

#[tauri::command]
pub async fn save_auto_checkin_config(config: Value) -> Result<Value, String> {
    off_main(move || {
        qs_switch_core::modules::ledger::write_checkin_config(&switch_root(), &config)
            .map_err(|e| e.to_string())
    })
    .await
}

#[tauri::command]
pub async fn get_checkin_logs() -> Result<Value, String> {
    off_main(|| Ok(qs_switch_core::modules::ledger::read_checkin_logs(&switch_root()))).await
}

/// 权限自检：认证目录写探针（Windows）+ 钥匙串可读性（macOS）。
#[tauri::command]
pub fn check_auth_permission(variant: Option<String>) -> Value {
    view::auth_permission_probe(&PathRoots::real(), variant_of(variant.as_deref()))
}

/// 打开系统授权面板。macOS 上是「完全磁盘访问」；其他平台没有这一步，
/// 明确报"不适用"而不是静默无反应。
#[tauri::command]
pub fn open_permission_settings(pane: Option<String>) -> Result<Value, String> {
    qs_switch_core::modules::process::open_system_settings_pane(pane.as_deref().unwrap_or(""))?;
    Ok(serde_json::json!({ "ok": true }))
}

/// 在系统文件管理器里定位本 App（macOS 访达 / Windows 资源管理器）。
#[tauri::command]
pub fn reveal_app_in_finder() -> Result<Value, String> {
    let exe = std::env::current_exe().map_err(|e| format!("取不到自身路径: {e}"))?;
    qs_switch_core::modules::process::reveal_in_file_manager(&exe)?;
    Ok(serde_json::json!({ "ok": true }))
}

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
        assert!(v["unavailable"].as_array().unwrap().len() >= 4);
        let names: Vec<String> = v["unavailable"]
            .as_array()
            .unwrap()
            .iter()
            .map(|x| x["name"].as_str().unwrap_or_default().to_string())
            .collect();
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
        assert!(tauri::async_runtime::block_on(get_rotate_logs()).unwrap()["logs"].is_array());
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
        assert_eq!(
            tauri::async_runtime::block_on(get_rotate_logs()).unwrap(),
            view::rotate_logs(&switch_root()));
    }

    /// 绑定漂移门禁（P0 级历史缺陷）：Tauri v2 按**参数名**从 invoke 载荷逐键取值，
    /// `args: Value` 要求载荷里有 `"args"` 键 —— 前端发的是扁平对象，当年桌面切换
    /// 因此从未跑通过，而任何行为测试都碰不到宏的参数绑定（编译与运行都正常，
    /// 一点就报 missing required key）。这里用源码对拍钉死这条盲区：
    /// 前端 switchAccount 声明的每个键，必须是命令认识的参数或刻意忽略的附加键。
    #[test]
    fn frontend_switch_payload_matches_command_params() {
        let manifest = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
        let api = std::fs::read_to_string(manifest.join("../src/lib/api.ts"))
            .expect("读前端 api.ts 失败");
        let compat = std::fs::read_to_string(manifest.join("src/compat.rs"))
            .expect("读本文件失败");

        // 前端：switchAccount(args: { ... }) 的参数对象键。
        let fe_block = api
            .split("export function switchAccount(args: {")
            .nth(1)
            .and_then(|rest| rest.split("}): Promise").next().map(String::from))
            .expect("api.ts 里找不到 switchAccount 签名");
        let mut fe_keys: Vec<String> = Vec::new();
        for line in fe_block.lines() {
            let t = line.trim();
            if let Some(k) = t.strip_suffix(";").map(|x| x.split(':').next().unwrap_or("")) {
                let k = k.trim_end_matches('?');
                if !k.is_empty() && !k.starts_with("//") {
                    fe_keys.push(k.to_string());
                }
            }
        }
        assert!(fe_keys.contains(&"accountId".to_string()), "{fe_keys:?}");

        // 命令：pub async fn switch_account(...) 的命名参数（snake→camel）。
        let cmd_block = compat
            .split("pub async fn switch_account(")
            .nth(1)
            .and_then(|rest| rest.split(") -> Result").next().map(String::from))
            .expect("compat.rs 里找不到 switch_account 签名");
        let params: Vec<String> = cmd_block
            .lines()
            .filter_map(|l| l.split(':').next())
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty() && *s != "app" && !s.starts_with("cell"))
            .map(|s| {
                let mut out = String::new();
                let mut up = false;
                for c in s.chars() {
                    if c == '_' {
                        up = true;
                    } else if up {
                        out.extend(c.to_uppercase());
                        up = false;
                    } else {
                        out.push(c);
                    }
                }
                out
            })
            .collect();

        // 刻意忽略的附加键：confirm 是 webui 知情门用的（桌面宏忽略未知键）；
        // 会话复制两键在 Qoder 侧无对应机制，收下后由 message 说明"没做"。
        const DELIBERATELY_IGNORED: &[&str] =
            &["confirm", "copySessionIds", "syncSelections"];
        for k in &fe_keys {
            assert!(
                params.iter().any(|p| p == k) || DELIBERATELY_IGNORED.contains(&k.as_str()),
                "前端键 {k} 既不是命令参数也不在刻意忽略清单里 —— \
                 要么改名对上，要么在 DELIBERATELY_IGNORED 里说明理由（并确认后果）"
            );
        }
        // 反向：命令的必填参数必须都在前端键里（缺一个就是运行时绑定失败）。
        for p in &params {
            assert!(
                fe_keys.contains(p) || *p == "forced" || *p == "target" || *p == "variant",
                "命令参数 {p} 在前端 switchAccount 里没有对应键: {fe_keys:?}"
            );
        }
    }
}
