//! `--self-check`：不开窗口，直接把宿主层的 command 逐个跑一遍。
//!
//! 存在的理由：GUI 需要可见 surface 才能人工驱动，而宿主层（9 个 command 的接线、
//! serde 形状、真实路径解析）恰恰是最难靠 core 单元测试覆盖的一层。这里跑的是与前端
//! invoke 完全相同的函数，因此它通过就等于 IPC 那半边通了。
//!
//! release 版是 Windows GUI 子系统（`windows_subsystem = "windows"`），stdout 不接
//! 任何控制台，所以报告同时写进 `<账号库>/selfcheck.log` —— 报告里会印出该路径。
//!
//! 只调用读取型与写自己账号库的操作 —— 绝不调用 switch_now（那会真换号）。

use serde_json::json;

use crate::commands;

struct Report {
    lines: Vec<String>,
    failed: Vec<String>,
}

impl Report {
    fn say(&mut self, s: impl AsRef<str>) {
        let l = s.as_ref().to_string();
        println!("{l}");
        self.lines.push(l);
    }

    fn step(&mut self, name: &str, f: impl FnOnce() -> Result<String, String>) {
        match f() {
            Ok(summary) => self.say(format!("  OK   {name:<18} {summary}")),
            Err(e) => {
                self.say(format!("  FAIL {name:<18} {e}"));
                self.failed.push(format!("{name}: {e}"));
            }
        }
    }

    fn flush(self, path: &std::path::Path) {
        let body = self.lines.join("\n") + "\n";
        if let Err(e) = std::fs::write(path, body) {
            eprintln!("写自检报告 {path:?} 失败: {e}");
        }
    }
}

pub fn run() -> i32 {
    let store = commands::store_dir();
    if let Some(parent) = std::path::Path::new(&store).parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let _ = std::fs::create_dir_all(&store);
    let report_path = std::path::Path::new(&store).join("selfcheck.log");

    let mut r = Report { lines: Vec::new(), failed: Vec::new() };
    r.say("Qoder Switch 自检（无头，不会切换任何账号）");
    r.say(format!("  账号库: {store}"));

    r.step("probe_all", || {
        let axes = commands::probe_all();
        if axes.len() != 6 {
            return Err(format!("应有 6 个 (版本·目标) 组合，实得 {}", axes.len()));
        }
        let mut parts = Vec::new();
        for a in &axes {
            let have = a.credentials.iter().filter(|c| c.exists).count();
            let verdict = match &a.hosted {
                qs_switch_core::modules::process::Hosted::Yes(_) => "托管",
                qs_switch_core::modules::process::Hosted::Unknown(_) => "未知",
                qs_switch_core::modules::process::Hosted::No => "未托管",
            };
            parts.push(format!(
                "{:?}·{:?} 在跑{} 凭据{}/{} {verdict}",
                a.variant,
                a.target,
                a.running_pids.len(),
                have,
                a.credentials.len()
            ));
        }
        Ok(parts.join(" | "))
    });

    let mut accounts = Vec::new();
    r.step("list_accounts", || {
        accounts = commands::list_accounts();
        if accounts.is_empty() {
            return Ok("（空，先在客户端登录一个账号再认领）".to_string());
        }
        Ok(format!(
            "{} 个分片: {}",
            accounts.len(),
            accounts
                .iter()
                .map(|b| format!("{}({:?}·{:?})", b.account_id, b.variant, b.target))
                .collect::<Vec<_>>()
                .join(", ")
        ))
    });

    if let Some(b) = accounts.first() {
        let (id, v, t) = (b.account_id.clone(), b.variant, b.target);
        r.step("preview", || {
            let pv = commands::preview(id.clone(), v, t)?;
            Ok(json!({
                "writes": pv.writes,
                "running": pv.running_pids.len(),
                "warnings": pv.warnings.len(),
            })
            .to_string())
        });
        r.step("export+格式门", || {
            let text = commands::export_account_text(id.clone())?;
            match commands::import_account_text(
                "{\"format\":999,\"exported_at\":\"\",\"bundles\":[]}".into(),
                false,
            ) {
                Ok(got) => Err(format!("假版本号的导入居然成功了: {got:?}")),
                Err(e) if e.contains("格式版本") => Ok(format!(
                    "导出 {} 字节，导入侧格式门生效",
                    text.len()
                )),
                Err(e) => Err(format!("导入失败原因不对，应为格式版本: {e}")),
            }
        });
    }

    r.step("unfinished", || {
        Ok(format!("{} 条未收尾切换", commands::unfinished()?.len()))
    });
    r.step("snapshot_now", || {
        let s = commands::snapshot_now()?;
        Ok(format!("{} 检出 {} 项变化", s.taken_at, s.changes.len()))
    });

    let ok = r.failed.is_empty();
    if ok {
        r.say("SELF-CHECK OK");
    } else {
        let list = r.failed.clone();
        r.say(format!("SELF-CHECK FAIL: {} 项", list.len()));
        for f in &list {
            r.say(format!("  - {f}"));
        }
    }
    r.say(format!("报告: {report_path:?}"));
    r.flush(&report_path);
    if ok {
        0
    } else {
        1
    }
}
