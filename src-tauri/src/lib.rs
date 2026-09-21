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
    let mut builder = tauri::Builder::default()
        // 单实例必须最先注册：第二个进程会在建窗口与托盘之前退出。
        .plugin(tauri_plugin_single_instance::init(|app, argv, _cwd| {
            // 自启重复触发（静默）不弹既有窗口，其余第二实例照常唤醒到前台。
            if tray::should_activate_on_second_launch(argv) {
                tray::show_main(app);
            }
        }))
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_dialog::init())
        .manage(compat::ProgressCell::default());

    #[cfg(desktop)]
    {
        // 自启拉起时带 --hidden：配合窗口配置的 visible:false 实现静默驻留托盘。
        builder = builder.plugin(tauri_plugin_autostart::init(
            tauri_plugin_autostart::MacosLauncher::LaunchAgent,
            Some(vec![tray::SILENT_STARTUP_ARG]),
        ));
        // 应用内更新：端点与签名公钥在 tauri.conf.json 的 plugins.updater。
        builder = builder.plugin(tauri_plugin_updater::Builder::new().build());
    }

    builder
        .setup(|app| {
            tray::install(&app.handle().clone())?;
            // 主窗口由配置创建为不可见；这里按是否静默启动决定要不要立刻显示。
            tray::setup_startup_visibility(
                app.handle(),
                tray::is_silent_startup(std::env::args()),
            );
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
            compat::delete_account,
            compat::switch_progress,
            compat::get_auto_rotate_config,
            compat::save_auto_rotate_config,
            compat::rotate_status,
            compat::run_rotate,
            compat::get_rotate_logs,
            compat::export_accounts,
            compat::export_accounts_to_path,
            compat::preview_import_accounts,
            compat::import_accounts,
            compat::get_capabilities,
            compat::list_notifications,
            compat::record_notification,
            compat::clear_notifications,
            commands::get_launch_at_login_enabled,
            commands::set_launch_at_login_enabled,
            compat::get_github_config,
            compat::save_github_config,
            compat::check_update,
            compat::relaunch_app
        ])
        .run(tauri::generate_context!())
        .expect("Qoder Switch 启动失败");
}
