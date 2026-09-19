//! 暴露给前端的 command。刻意保持薄：所有判定与写盘都在 qs-switch-core 里。

use serde::Serialize;
use tauri::{AppHandle, Emitter};

use qs_switch_core::modules::config::{switch_root, PathRoots};
use qs_switch_core::modules::variant::{QoderTarget, QoderVariant};
use qs_switch_core::modules::{bundle, process, snapshot, switch};

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
fn probe_all() -> Vec<AxisStatus> {
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
fn list_accounts() -> Vec<bundle::Bundle> {
    bundle::list_all(&switch_root())
}

#[tauri::command]
fn capture(account_id: String, variant: QoderVariant, target: QoderTarget) -> Result<bundle::Bundle, String> {
    let roots = PathRoots::real();
    bundle::capture(&roots, &switch_root(), account_id.trim(), variant, target)
}

#[tauri::command]
fn preview(account_id: String, variant: QoderVariant, target: QoderTarget) -> Result<switch::Preview, String> {
    let roots = PathRoots::real();
    switch::preview(
        &roots,
        &switch_root(),
        &switch::Request { account_id, variant, target, restart: true },
    )
}

/// 执行切换。阻塞在 spawn_blocking 里跑，过程经 `switch-progress` 事件回流。
#[tauri::command]
async fn switch_now(
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
fn unfinished() -> Result<Vec<switch::Journal>, String> {
    switch::unfinished(&switch_root())
}

#[tauri::command]
fn recover(journal: switch::Journal) -> Result<String, String> {
    let store = switch_root();
    switch::recover(&store, &journal).map(|p| format!("{p:?}"))
}

#[tauri::command]
fn snapshot_now() -> Result<SnapshotReport, String> {
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
fn store_dir() -> String {
    switch_root().display().to_string()
}
