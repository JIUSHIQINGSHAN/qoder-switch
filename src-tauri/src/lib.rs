mod commands;
mod tray;

use tauri::Manager;

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
            commands::store_dir
        ])
        .run(tauri::generate_context!())
        .expect("Qoder Switch 启动失败");
}
