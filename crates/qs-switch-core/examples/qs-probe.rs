//! 现场探针：`cargo run --example qs-probe`。
//!
//! 打印每个 (版本·目标) 当前在跑的进程数，以及"从这里发起换号会不会把自己杀掉"。
//! 换号前的自检就是这个判断，值得单独看一眼。

use qs_switch_core::modules::{process, variant::*};

fn main() {
    let roots = qs_switch_core::modules::config::PathRoots::real();

    let host = process::host_env();
    println!(
        "环境标记: {:?} → 版本 {:?}，session_type {:?}",
        host.keys, host.variant, host.session_type
    );
    match process::ancestor_chain() {
        Ok(c) => println!(
            "父链({}): {}",
            if c.complete { "完整" } else { "断链" },
            c.names.join(" ← ")
        ),
        Err(e) => println!("父链探测失败: {e}"),
    }
    println!();
    println!("{:<10} {:<9} {:<22} {:>6}  {}", "版本", "目标", "镜像名", "在跑", "托管判定");
    for (v, t) in all_axes() {
        let images = t.images(v);
        let n = process::running_pids(images).map(|p| p.len()).unwrap_or(0);
        let verdict = match process::hosted_by(v, t) {
            process::Hosted::Yes(why) => format!("会（拒绝执行）— {why}"),
            process::Hosted::No => "不会".to_string(),
            process::Hosted::Unknown(why) => format!("未知（同样拒绝）— {why}"),
        };
        println!(
            "{:<10} {:<9} {:<22} {:>4}  {}",
            match v {
                QoderVariant::Cn => "cn",
                QoderVariant::Global => "global",
            },
            match t {
                QoderTarget::Desktop => "desktop",
                QoderTarget::Cli => "cli",
                QoderTarget::Work => "work",
            },
            images.join("+"),
            n,
            verdict
        );
    }
    if let Some(e) = executable(&roots, QoderVariant::Cn, QoderTarget::Desktop) {
        println!("\n桌面端可执行文件（取自 Launcher state.ini）：{e:?}");
    }
}
