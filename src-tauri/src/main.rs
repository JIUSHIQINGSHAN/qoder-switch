// release 版不附带控制台窗口；`--self-check` 因此把报告同时写进账号库日志文件。
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    if std::env::args().any(|a| a == "--self-check") {
        std::process::exit(qoder_switch_lib::self_check());
    }
    qoder_switch_lib::run()
}
