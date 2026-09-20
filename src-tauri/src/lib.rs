mod commands;
mod compat;
mod selfcheck;
mod tray;

/// `qoder-switch --self-check` 的入口：无头跑一遍宿主层。
pub fn self_check() -> i32 {
    selfcheck::run()
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        // 单实例必须最先注册：第二个进程会在建窗口与托盘之前退出。
        .plugin(tauri_plugin_single_instance::init(|app, _argv, _cwd| {
            tray::show_main(app);
        }))
        .plugin(tauri_plugin_opener::init())
        .setup(|app| {
            tray::install(&app.handle().clone())?;
            Ok(())
        })
        // 关窗只隐藏不退出：换号后还要能从托盘把界面拉回来。
        .on_window_event(|window, event| {
            if let tauri::WindowEvent::CloseRequested { api, .. } = event {
                api.prevent_close();
                let _ = window.hide();
            }
        })
        .invoke_handler(tauri::generate_handler![
            commands::probe_all,
            commands::list_accounts,
            commands::capture,
            commands::preview,
            commands::switch_now,
            commands::unfinished,
            commands::recover,
            commands::snapshot_now,
            commands::store_dir,
            commands::export_account_text,
            commands::import_account_text,
            commands::rotation_suggestion,
            commands::apply_rotation,
            // 前端契约（参考实现原样副本所调用的命令名与返回形状）
            compat::get_status,
            compat::get_accounts,
            compat::switch_account,
            compat::import_local,
            compat::delete_account
        ])
        .run(tauri::generate_context!())
        .expect("Qoder Switch 启动失败");
}
