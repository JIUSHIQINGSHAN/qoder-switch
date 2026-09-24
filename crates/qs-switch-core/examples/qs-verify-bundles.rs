//! 只读账号包体检：`cargo run --example qs-verify-bundles`。
//!
//! 严格只读 —— 不写盘、不切号、不打印任何凭据值（只输出角色名、文件大小与布尔判定）。
//! 用途：验证「扫码登录（OAuth）」建出的包是否满足 `restore` 的契约，
//! 即成员表是否与 `credentials()` 布局一致、包内文件名是否走 `stored_name()`。
//!
//! 背景：0.1.5 的 OAuth 落包曾只登记 `AuthMain` 且文件名为 `auth.v1.dat`，
//! 与 `restore` 的三处约定都不符 —— 账号建得出来却切不过去。

use qs_switch_core::modules::config::PathRoots;
use qs_switch_core::modules::variant::credentials;
use qs_switch_core::modules::{bundle, switch};

fn main() {
    let roots = PathRoots::real();
    let store = qs_switch_core::modules::config::switch_root();

    println!("账号库: {}", store.display());
    println!();

    let accounts = bundle::list_all(&store);
    if accounts.is_empty() {
        println!("库里没有账号。");
        return;
    }

    let mut problems = 0usize;
    for b in &accounts {
        println!("=== {} ({:?}·{:?}) ===", b.account_id, b.variant, b.target);
        println!("  成员数: {}", b.members.len());

        // 1. 包内文件名必须等于 FileRole::stored_name()（restore 按它找文件）。
        let mut naming_bad = Vec::new();
        for m in &b.members {
            let expected = m.role.stored_name();
            if m.file_name != expected {
                naming_bad.push(format!("{:?} 实为 {:?}，应为 {:?}", m.role, m.file_name, expected));
            }
        }
        if naming_bad.is_empty() {
            println!("  ✓ 包内文件名全部符合 stored_name() 约定");
        } else {
            problems += 1;
            for x in &naming_bad {
                println!("  ✗ 文件名不符: {x}");
            }
        }

        // 2. 现场存在的 critical 角色必须都在包里（restore 的覆盖性检查）。
        let missing: Vec<String> = credentials(&roots, b.variant, b.target)
            .iter()
            .filter(|f| f.critical && f.exists() && b.member(f.role).is_none())
            .map(|f| format!("{:?}", f.role))
            .collect();
        if missing.is_empty() {
            println!("  ✓ 现场 critical 角色齐备（不会被判半换号）");
        } else {
            problems += 1;
            println!("  ✗ 缺现场 critical 角色: {}", missing.join(", "));
            println!("    → 切换会被拒：'bundle 缺少现场存在的 critical 文件'");
        }

        // 3. 只读预演，确认没有阻断性告警。
        let req = switch::Request {
            account_id: b.account_id.clone(),
            variant: b.variant,
            target: b.target,
            restart: false,
        };
        match switch::preview(&roots, &store, &req) {
            Ok(pv) => {
                let blocking: Vec<&String> =
                    pv.warnings.iter().filter(|w| w.contains("拒写") || w.contains("缺位")).collect();
                println!("  预演: 写回 {} 个文件，告警 {} 条", pv.writes.len(), pv.warnings.len());
                for w in &pv.warnings {
                    println!("    · {w}");
                }
                if !blocking.is_empty() {
                    problems += 1;
                }
            }
            Err(e) => {
                problems += 1;
                println!("  预演失败: {e}");
            }
        }
        println!();
    }

    if problems == 0 {
        println!(">>> 结论: 全部账号包通过 restore 契约检查");
    } else {
        println!(">>> 结论: 存在 {problems} 处问题（见上）");
        std::process::exit(1);
    }
}
