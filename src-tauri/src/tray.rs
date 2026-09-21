//! 系统托盘：常驻 + 关窗不退进程 + 账号快捷切换。
//!
//! 托盘里的账号项直接走 `switch::execute(Actor::Real)` —— 托管判定在 execute 内部，
//! 所以这条路径不可能绕过安全门：从 Qoder 会话内启动时它同样会拒绝。

use std::path::PathBuf;

use tauri::menu::{Menu, MenuItem, PredefinedMenuItem};
use tauri::tray::TrayIconBuilder;
use tauri::{AppHandle, Emitter, Manager};

use qs_switch_core::modules::config::PathRoots;
use qs_switch_core::modules::variant::{QoderTarget, QoderVariant};
use qs_switch_core::modules::{bundle, switch};

/// 托盘项 id 前缀，便于在事件里反解出要切的账号。
const PREFIX: &str = "acct:";

/// 托盘图标全局固定 ID
pub const TRAY_ID: &str = "main-tray";

/// 静默启动参数：注册开机自启时由插件注入（见 lib.rs 的 autostart 初始化）。
pub const SILENT_STARTUP_ARG: &str = "--hidden";

/// 判断本次启动是否携带精确的 `--hidden` 参数（系统自启触发）。
///
/// 必须整参相等，禁止子串匹配，避免 `--hidden-x`、`x--hidden` 等误入静默模式。
pub fn is_silent_startup(args: impl IntoIterator<Item = impl AsRef<str>>) -> bool {
    args.into_iter()
        .any(|arg| arg.as_ref() == SILENT_STARTUP_ARG)
}

/// 第二次启动是否需要把既有实例唤醒到前台。
///
/// 静默启动（精确 `--hidden`，自启重复触发）不打扰用户：不弹既有窗口。
pub fn should_activate_on_second_launch(args: impl IntoIterator<Item = impl AsRef<str>>) -> bool {
    !is_silent_startup(args)
}

/// 启动可见性：普通启动立即显示主窗口；静默启动窗口保持配置的不可见，只留托盘。
pub fn setup_startup_visibility(app: &AppHandle, silent: bool) {
    if !silent {
        show_main(app);
    }
}

/// 构建最新账号列表对应的托盘菜单
pub fn build_menu<R: tauri::Runtime>(app: &AppHandle<R>) -> tauri::Result<Menu<R>> {
    let menu = Menu::new(app)?;
    menu.append(&MenuItem::with_id(app, "open", "显示主界面", true, None::<&str>)?)?;
    menu.append(&PredefinedMenuItem::separator(app)?)?;

    // 只对桌面目标提供快捷切换：CLI 不落盘凭据、QoderWork 是另一套文件。
    let store = qs_switch_core::modules::config::switch_root();
    let desktop: Vec<_> = bundle::list_all(&store)
        .into_iter()
        .filter(|b| b.target == QoderTarget::Desktop)
        .collect();
    if desktop.is_empty() {
        menu.append(&MenuItem::with_id(
            app,
            "none",
            "（还没有已认领的账号）",
            false,
            None::<&str>,
        )?)?;
    }
    for b in &desktop {
        let tail = match b.identity.token_days_left() {
            Some(d) => format!("·token 剩 {d} 天"),
            None => "·未解出到期".into(),
        };
        menu.append(&MenuItem::with_id(
            app,
            format!("{PREFIX}{}|{:?}", b.account_id, b.variant),
            format!("切到 {} {}", b.display_label(), tail),
            true,
            None::<&str>,
        )?)?;
    }
    menu.append(&PredefinedMenuItem::separator(app)?)?;
    menu.append(&MenuItem::with_id(app, "quit", "退出 Qoder Switch", true, None::<&str>)?)?;
    Ok(menu)
}

/// 刷新托盘菜单（账号库有变动时由 UI 或内部调用）。
pub fn refresh_tray_menu<R: tauri::Runtime>(app: &AppHandle<R>) -> tauri::Result<()> {
    if let Some(tray) = app.tray_by_id(TRAY_ID) {
        let menu = build_menu(app)?;
        tray.set_menu(Some(menu))?;
    }
    Ok(())
}

pub fn install(app: &AppHandle) -> tauri::Result<()> {
    let menu = build_menu(app)?;

    let mut builder = TrayIconBuilder::with_id(TRAY_ID)
        .menu(&menu)
        .tooltip("Qoder Switch · 关窗后仍在托盘常驻")
        .on_menu_event(move |app, event| {
            let id = event.id().as_ref().to_string();
            if id == "open" {
                show_main(app);
            } else if id == "quit" {
                app.exit(0);
            } else if let Some(payload) = id.strip_prefix(PREFIX) {
                spawn_switch(app.clone(), payload.to_string());
            }
        });
    if let Some(icon) = app.default_window_icon() {
        builder = builder.icon(icon.clone());
    }
    builder.build(app)?;
    Ok(())
}

/// `acct:<id>|<Variant>` → 后台线程执行切换，进度走同一个 `switch-progress` 事件。
fn spawn_switch(app: AppHandle, payload: String) {
    let Some((account_id, variant)) = payload.split_once('|') else {
        return;
    };
    let variant = match variant {
        "Cn" => QoderVariant::Cn,
        "Global" => QoderVariant::Global,
        other => {
            let _ = app.emit("switch-progress", format!("未知版本 {other}，已忽略"));
            return;
        }
    };

    // 检查 ProgressCell，避免在已有切号进行时并发重入
    if let Some(cell) = app.try_state::<crate::compat::ProgressCell>() {
        let g = cell.0.lock().unwrap_or_else(|p| p.into_inner());
        if g.running {
            let _ = app.emit("switch-progress", "已有切换正在进行中，请稍候".to_string());
            return;
        }
    }

    let req = switch::Request {
        account_id: account_id.to_string(),
        variant,
        target: QoderTarget::Desktop,
        restart: true,
    };
    std::thread::spawn(move || {
        let roots = PathRoots::real();
        let store: PathBuf = qs_switch_core::modules::config::switch_root();
        let handle = app.clone();
        if let Some(cell) = handle.try_state::<crate::compat::ProgressCell>() {
            cell.set(true, Some("托盘正在切号".into()));
        }
        let result = switch::execute(&roots, &store, &req, switch::Actor::Real, &mut |m| {
            let _ = handle.emit("switch-progress", m.to_string());
        });
        if let Some(cell) = app.try_state::<crate::compat::ProgressCell>() {
            cell.set(false, None);
        }
        match result {
            Ok(j) => {
                let _ = app.emit(
                    "switch-progress",
                    format!("托盘切换完成：{} → {:?}", j.account_id, j.phase),
                );
                let _ = refresh_tray_menu(&app);
            }
            Err(e) => {
                let _ = app.emit("switch-progress", format!("托盘切换失败：{e}"));
            }
        }
    });
}

pub fn show_main(app: &AppHandle) {
    if let Some(w) = app.get_webview_window("main") {
        let _ = w.unminimize();
        let _ = w.show();
        let _ = w.set_focus();
    }
}

#[cfg(test)]
mod silent_startup_tests {
    use super::{is_silent_startup, should_activate_on_second_launch, SILENT_STARTUP_ARG};

    #[test]
    fn silent_arg_matches_exactly() {
        assert!(is_silent_startup(["--hidden"]));
        assert!(is_silent_startup(["qoder-switch.exe", "--hidden"]));
        assert!(!is_silent_startup(Vec::<&str>::new()));
    }

    #[test]
    fn silent_arg_never_matches_substrings() {
        assert!(!is_silent_startup(["--hidden-x"]));
        assert!(!is_silent_startup(["x--hidden"]));
        assert!(!is_silent_startup(["--hiddenextra"]));
        assert!(!is_silent_startup(["--debug"]));
    }

    #[test]
    fn second_launch_activates_unless_exact_silent_arg() {
        assert!(should_activate_on_second_launch(["qoder-switch.exe"]));
        assert!(!should_activate_on_second_launch([SILENT_STARTUP_ARG]));
        assert!(should_activate_on_second_launch(["--hidden-x"]));
    }

    #[test]
    fn tray_id_constant_is_fixed() {
        use super::TRAY_ID;
        assert_eq!(TRAY_ID, "main-tray");
    }
}
