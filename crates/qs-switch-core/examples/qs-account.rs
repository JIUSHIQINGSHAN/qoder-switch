//! 账号包工具：`cargo run --example qs-account -- <list|capture|show>`。
//!
//!   capture <账号名> [cn|global] [desktop|cli|work]   认领当前登录态（缺省 cn desktop）
//!   list                                             列出已认领的包
//!   show <账号名> [cn|global] [目标]                  打印某个包的成员与身份
//!
//! 只读产品目录；写入仅发生在 `~/.qs-switch/`。restore（真正换号）刻意不在本工具
//! 暴露 —— 它需要先终止目标进程，而发起方可能就是目标的子进程。

use qs_switch_core::modules::{bundle, config::PathRoots, variant::*};

fn main() {
    let mut args = std::env::args().skip(1);
    let cmd = args.next().unwrap_or_else(|| "list".into());
    let rest: Vec<String> = args.collect();
    let r = match cmd.as_str() {
        "capture" => capture(&rest),
        "list" => list(),
        "show" => show(&rest),
        other => Err(format!("未知子命令: {other}（可用: capture|list|show）")),
    };
    if let Err(e) = r {
        eprintln!("失败: {e}");
        std::process::exit(1);
    }
}

fn parse_variant(s: &str) -> Option<QoderVariant> {
    match s {
        "cn" => Some(QoderVariant::Cn),
        "global" => Some(QoderVariant::Global),
        _ => None,
    }
}

fn parse_target(s: &str) -> Option<QoderTarget> {
    match s {
        "desktop" => Some(QoderTarget::Desktop),
        "cli" => Some(QoderTarget::Cli),
        "work" => Some(QoderTarget::Work),
        _ => None,
    }
}

fn axes(rest: &[String]) -> std::result::Result<(String, QoderVariant, QoderTarget), String> {
    let id = rest
        .first()
        .filter(|s| !s.trim().is_empty())
        .ok_or("必须给账号名：capture <账号名> [cn|global] [目标]")?
        .clone();
    let v = match rest.get(1) {
        Some(s) => parse_variant(s).ok_or_else(|| format!("未知版本: {s}"))?,
        None => QoderVariant::Cn,
    };
    let t = match rest.get(2) {
        Some(s) => parse_target(s).ok_or_else(|| format!("未知目标: {s}"))?,
        None => QoderTarget::Desktop,
    };
    Ok((id, v, t))
}

fn capture(rest: &[String]) -> std::result::Result<(), String> {
    let (id, v, t) = axes(rest)?;
    let store = qs_switch_core::modules::config::switch_root();
    let roots = PathRoots::real();

    let present: Vec<String> = credentials(&roots, v, t)
        .iter()
        .map(|f| {
            format!(
                "  {:<20} {:<8} {}",
                format!("{:?}", f.role),
                if f.critical { "critical" } else { "观测" },
                if f.exists() { "存在" } else { "不存在" }
            )
        })
        .collect();
    println!("目标 {} · {} 的凭据布局:", v.label(), t.label());
    for l in &present {
        println!("{l}");
    }

    let b = bundle::capture(&roots, &store, &id, v, t)?;
    if b.is_empty() {
        return Err(format!(
            "没有可认领的文件 —— 该目标在本机不落盘凭据（CN CLI 就是这种）"
        ));
    }
    println!(
        "\n已认领为账号 {:?}：{} 个文件，身份={}，包目录 {}",
        id,
        b.members.len(),
        b.display_label(),
        b.dir_in(&store).display()
    );
    for m in &b.members {
        println!("  {:<20} {:>6} B  sha256={}..", format!("{:?}", m.role), m.size, &m.sha256[..12]);
    }
    Ok(())
}

fn list() -> std::result::Result<(), String> {
    let store = qs_switch_core::modules::config::switch_root();
    let root = bundle::accounts_root_in(&store);
    if !root.is_dir() {
        println!("还没有认领任何账号（store: {}）", root.display());
        return Ok(());
    }
    let mut n = 0;
    for acc in std::fs::read_dir(&root).map_err(|e| e.to_string())? {
        let acc = acc.map_err(|e| e.to_string())?;
        if !acc.path().is_dir() {
            continue;
        }
        let name = acc.file_name().to_string_lossy().to_string();
        for axis in all_axes() {
            if let Ok(b) = bundle::load(&store, &name, axis.0, axis.1) {
                n += 1;
                println!(
                    "{:<16} {:<8} {:<9} {:>2} 个文件  {}",
                    name,
                    match axis.0 {
                        QoderVariant::Cn => "cn",
                        QoderVariant::Global => "global",
                    },
                    match axis.1 {
                        QoderTarget::Desktop => "desktop",
                        QoderTarget::Cli => "cli",
                        QoderTarget::Work => "work",
                    },
                    b.members.len(),
                    b.display_label()
                );
            }
        }
    }
    if n == 0 {
        println!("账号目录下没有可用的包。");
    }
    Ok(())
}

fn show(rest: &[String]) -> std::result::Result<(), String> {
    let (id, v, t) = axes(rest)?;
    let store = qs_switch_core::modules::config::switch_root();
    let b = bundle::load(&store, &id, v, t)?;
    println!("{}", serde_json::to_string_pretty(&b.identity).unwrap_or_default());
    for m in &b.members {
        println!("  {:?} {} B", m.role, m.size);
    }
    Ok(())
}
