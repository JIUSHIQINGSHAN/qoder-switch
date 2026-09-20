//! 暴露给前端的 command。刻意保持薄：所有判定与写盘都在 qs-switch-core 里。

use serde::Serialize;
use tauri::{AppHandle, Emitter};

use qs_switch_core::modules::config::{switch_root, PathRoots};
use qs_switch_core::modules::variant::{QoderTarget, QoderVariant};
use qs_switch_core::modules::{bundle, export_import, process, rotate, snapshot, switch};

/// 一个 (版本·目标) 的现场状态，供 UI 的安全横幅与账号列表使用。
#[derive(Serialize)]
pub struct AxisStatus {
    pub variant: QoderVariant,
    pub target: QoderTarget,
    pub variant_label: &'static str,
    pub target_label: &'static str,
    pub images: &'static [&'static str],
    pub running_pids: Vec<u32>,
    pub hosted: process::Hosted,
    pub exe: Option<String>,
    pub credentials: Vec<CredentialView>,
}

#[derive(Serialize)]
pub struct CredentialView {
    pub role: String,
    pub path: String,
    pub critical: bool,
    pub exists: bool,
    pub size: Option<u64>,
}

#[derive(Serialize)]
pub struct SnapshotReport {
    pub taken_at: String,
    pub path: String,
    pub changes: Vec<snapshot::Change>,
}

#[tauri::command]
pub fn probe_all() -> Vec<AxisStatus> {
    let roots = PathRoots::real();
    let mut out = Vec::new();
    for (variant, target) in QoderVariant::ALL.into_iter().flat_map(|v| {
        QoderTarget::ALL
            .into_iter()
            .map(move |t| (v, t))
    }) {
        out.push(AxisStatus {
            running_pids: process::running_pids(target.images(variant)),
            hosted: process::hosted_by(variant, target),
            exe: qs_switch_core::modules::variant::executable(&roots, variant, target)
                .map(|p| p.display().to_string()),
            credentials: qs_switch_core::modules::variant::credentials(&roots, variant, target)
                .into_iter()
                .map(|f| CredentialView {
                    role: format!("{:?}", f.role),
                    path: f.path.display().to_string(),
                    critical: f.critical,
                    exists: f.exists(),
                    size: std::fs::metadata(&f.path).ok().map(|m| m.len()),
                })
                .collect(),
            variant,
            target,
            variant_label: variant.label(),
            target_label: target.label(),
            images: target.images(variant),
        });
    }
    out
}

#[tauri::command]
pub fn list_accounts() -> Vec<bundle::Bundle> {
    bundle::list_all(&switch_root())
}

#[tauri::command]
pub fn capture(account_id: String, variant: QoderVariant, target: QoderTarget) -> Result<bundle::Bundle, String> {
    let roots = PathRoots::real();
    bundle::capture(&roots, &switch_root(), account_id.trim(), variant, target)
}

#[tauri::command]
pub fn preview(account_id: String, variant: QoderVariant, target: QoderTarget) -> Result<switch::Preview, String> {
    let roots = PathRoots::real();
    switch::preview(
        &roots,
        &switch_root(),
        &switch::Request { account_id, variant, target, restart: true },
    )
}

/// 执行切换。阻塞在 spawn_blocking 里跑，过程经 `switch-progress` 事件回流。
#[tauri::command]
pub async fn switch_now(
    app: AppHandle,
    account_id: String,
    variant: QoderVariant,
    target: QoderTarget,
    restart: bool,
    forced: bool,
) -> Result<switch::Journal, String> {
    let req = switch::Request { account_id, variant, target, restart };
    let roots = PathRoots::real();
    let store = switch_root();
    let actor = if forced { switch::Actor::RealForced } else { switch::Actor::Real };
    let handle = app.clone();
    tauri::async_runtime::spawn_blocking(move || {
        switch::execute(&roots, &store, &req, actor, &mut |m| {
            let _ = handle.emit("switch-progress", m.to_string());
        })
    })
    .await
    .map_err(|e| format!("切换任务异常终止: {e}"))?
}

#[tauri::command]
pub fn unfinished() -> Result<Vec<switch::Journal>, String> {
    switch::unfinished(&switch_root())
}

#[tauri::command]
pub fn recover(journal: switch::Journal) -> Result<String, String> {
    let store = switch_root();
    switch::recover(&store, &journal).map(|p| format!("{p:?}"))
}

#[tauri::command]
pub fn snapshot_now() -> Result<SnapshotReport, String> {
    let previous = snapshot::Snapshot::latest().map_err(|e| e.to_string())?;
    let now = snapshot::Snapshot::take();
    let path = now.save().map_err(|e| e.to_string())?;
    let changes = previous.map(|p| p.diff(&now)).unwrap_or_default();
    Ok(SnapshotReport {
        taken_at: now.taken_at.clone(),
        path: path.display().to_string(),
        changes,
    })
}

#[tauri::command]
pub fn store_dir() -> String {
    switch_root().display().to_string()
}

/// 导出为可直接粘贴的 JSON 文本。副本是真实凭据的密文文件，只应在同一 Windows
/// 用户内搬运 —— 跨机器导入会静默变成未登录，这点由界面负责讲清楚。
#[tauri::command]
pub fn export_account_text(account_id: String) -> Result<String, String> {
    let e = export_import::export_account(&switch_root(), account_id.trim())?;
    let bytes = export_import::to_bytes(&e)?;
    String::from_utf8(bytes).map_err(|e| format!("导出结果不是合法 UTF-8: {e}"))
}

#[derive(Debug, Serialize)]
pub struct ImportResult {
    pub written: Vec<String>,
    pub skipped: Vec<String>,
}

#[tauri::command]
pub fn import_account_text(text: String, overwrite: bool) -> Result<ImportResult, String> {
    let rep = export_import::import(&switch_root(), text.as_bytes(), overwrite)?;
    Ok(ImportResult {
        written: rep
            .written
            .into_iter()
            .map(|(id, v, t, n)| format!("{id} · {} · {} · {n} 个文件", v.label(), t.label()))
            .collect(),
        skipped: rep.skipped,
    })
}

#[derive(Serialize)]
pub struct RotationView {
    pub suggestion: rotate::Suggestion,
    pub state: rotate::RotateState,
}

/// 轮换建议。只读：不写产品目录、不动进程。
#[tauri::command]
pub fn rotation_suggestion(variant: QoderVariant) -> Result<RotationView, String> {
    let roots = PathRoots::real();
    let store = switch_root();
    let cfg = rotate::RotateConfig::default();
    Ok(RotationView {
        suggestion: rotate::suggest(&roots, &store, variant, &cfg)?,
        state: rotate::read_state(&store).unwrap_or_default(),
    })
}

/// 执行建议。走的就是普通切换路径，托管判定与备份回滚一个都不绕过。
#[tauri::command]
pub fn apply_rotation(variant: QoderVariant, restart: bool) -> Result<switch::Journal, String> {
    let roots = PathRoots::real();
    let store = switch_root();
    let s = rotate::suggest(&roots, &store, variant, &rotate::RotateConfig::default())?;
    rotate::apply(&roots, &store, &s, restart)
}

// ---------------------------------------------------------------------------
// 开机自启（参考实现同名命令的原样搬运：OS 注册状态是唯一事实源）
// ---------------------------------------------------------------------------

/// 查询系统当前的开机自启注册状态。
///
/// 以 tauri-plugin-autostart 的 OS 状态为唯一事实来源，不另存本地布尔值。
#[tauri::command]
pub fn get_launch_at_login_enabled(_app: AppHandle) -> Result<bool, String> {
    #[cfg(desktop)]
    {
        use tauri_plugin_autostart::ManagerExt;
        return _app
            .autolaunch()
            .is_enabled()
            .map_err(|e| format!("查询开机自启状态失败：{e}"));
    }
    #[cfg(not(desktop))]
    {
        let _ = _app;
        Err("当前平台不支持开机自启".to_string())
    }
}

/// 注册 / 移除系统开机自启，并回读权威状态。
///
/// 回读结果与请求值不一致时按失败处理并返回当前真实状态，避免假装设置成功。
#[tauri::command]
pub fn set_launch_at_login_enabled(_app: AppHandle, enabled: bool) -> Result<bool, String> {
    #[cfg(desktop)]
    {
        use tauri_plugin_autostart::ManagerExt;
        let autostart = _app.autolaunch();
        let action = if enabled { "开启" } else { "关闭" };
        let result = if enabled {
            autostart.enable()
        } else {
            autostart.disable()
        };
        if let Err(e) = result {
            return Err(format!("{action}开机自启失败：{e}"));
        }
        let authoritative = autostart
            .is_enabled()
            .map_err(|e| format!("开机自启设置后回读状态失败：{e}"))?;
        if authoritative != enabled {
            return Err(format!(
                "{action}开机自启未生效（系统当前状态：{}），请稍后重试",
                if authoritative {
                    "已开启"
                } else {
                    "未开启"
                }
            ));
        }
        Ok(authoritative)
    }
    #[cfg(not(desktop))]
    {
        let _ = (_app, enabled);
        Err("当前平台不支持开机自启".to_string())
    }
}
