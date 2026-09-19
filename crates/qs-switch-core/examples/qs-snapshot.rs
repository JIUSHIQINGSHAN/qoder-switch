//! M0 门控观测工具：`cargo run --example qs-snapshot -- <take|list|diff|show>`。
//!
//! 全程只读产品目录。用法：
//!   take            拍一张快照并落盘
//!   show [文件]     打印快照（缺省用最新一张）
//!   diff [前 后]    比对两张快照（缺省用最近两张）
//!   list            列出已存快照

use qs_switch_core::modules::snapshot::{ChangeKind, Snapshot};

fn main() {
    let mut args = std::env::args().skip(1);
    let cmd = args.next().unwrap_or_else(|| "take".into());
    let extra: Vec<String> = args.collect();
    let r = match cmd.as_str() {
        "take" => take(),
        "show" => show(extra),
        "diff" => diff(extra),
        "list" => list(),
        other => {
            eprintln!("未知子命令: {other}");
            Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "用法: take|show|diff|list",
            ))
        }
    };
    if let Err(e) = r {
        eprintln!("失败: {e}");
        std::process::exit(1);
    }
}

fn load_all() -> std::io::Result<Vec<std::path::PathBuf>> {
    let dir = qs_switch_core::modules::config::snapshots_dir();
    let mut v: Vec<_> = std::fs::read_dir(&dir)?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().map(|x| x == "json").unwrap_or(false))
        .collect();
    v.sort();
    Ok(v)
}

fn take() -> std::io::Result<()> {
    let s = Snapshot::take();
    let path = s.save()?;
    println!("快照已写入 {}", path.display());
    print_snapshot(&s);
    Ok(())
}

fn show(extra: Vec<String>) -> std::io::Result<()> {
    let s = match extra.first() {
        Some(p) => Snapshot::load(&std::path::PathBuf::from(p))?,
        None => Snapshot::latest()?.ok_or_else(|| not_found("还没有任何快照"))?,
    };
    print_snapshot(&s);
    Ok(())
}

fn diff(extra: Vec<String>) -> std::io::Result<()> {
    let (a, b) = if extra.len() >= 2 {
        (
            Snapshot::load(&std::path::PathBuf::from(&extra[0]))?,
            Snapshot::load(&std::path::PathBuf::from(&extra[1]))?,
        )
    } else {
        let all = load_all()?;
        if all.len() < 2 {
            return Err(not_found("至少需要两张快照才能比对"));
        }
        (
            Snapshot::load(&all[all.len() - 2])?,
            Snapshot::load(&all[all.len() - 1])?,
        )
    };
    println!("前: {}   后: {}", a.taken_at, b.taken_at);
    let changes = a.diff(&b);
    if changes.is_empty() {
        println!("  （无变化）");
        return Ok(());
    }
    for c in &changes {
        println!(
            "  {:<11} {:<10} {:<8} {:<20} {}",
            kind_label(c.kind),
            format!("{:?}", c.variant),
            format!("{:?}", c.target),
            format!("{:?}", c.role),
            if c.critical { "★换号必替" } else { "  观测" }
        );
    }
    let crit_modified = changes
        .iter()
        .filter(|c| c.critical && matches!(c.kind, ChangeKind::Modified | ChangeKind::Appeared | ChangeKind::Disappeared))
        .count();
    println!("\n变化 {} 项，其中 critical {} 项。", changes.len(), crit_modified);
    Ok(())
}

fn list() -> std::io::Result<()> {
    for p in load_all()? {
        println!("{}", p.display());
    }
    Ok(())
}

fn print_snapshot(s: &Snapshot) {
    println!("taken_at = {}", s.taken_at);
    for e in &s.entries {
        let state = if !e.exists {
            match &e.error {
                Some(_) => "读不到",
                None => "不存在",
            }
        } else {
            "存在"
        };
        println!(
            "  {:<10} {:<8} {:<20} {:<8} size={:<10} {}",
            format!("{:?}", e.variant),
            format!("{:?}", e.target),
            format!("{:?}", e.role),
            state,
            e.size.map(|v| v.to_string()).unwrap_or_else(|| "-".into()),
            match &e.sha256 {
                Some(h) => format!("sha256={}..", &h[..12]),
                None => String::new(),
            }
        );
        if let Some(err) = &e.error {
            println!("             └ 读取失败: {err}");
        }
    }
}

fn kind_label(k: ChangeKind) -> &'static str {
    match k {
        ChangeKind::Appeared => "出现",
        ChangeKind::Disappeared => "消失",
        ChangeKind::Modified => "内容变化",
        ChangeKind::TouchOnly => "仅时间戳",
        ChangeKind::Unchanged => "无变化",
    }
}

fn not_found(msg: &str) -> std::io::Error {
    std::io::Error::new(std::io::ErrorKind::NotFound, msg)
}
