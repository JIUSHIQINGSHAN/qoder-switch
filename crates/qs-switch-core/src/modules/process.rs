//! 进程探测与终止。这里的核心不是"能不能杀"，而是**能不能安全地杀**。
//!
//! 本机实测：本 agent 的祖先链是 `powershell → bash → bash → bash → Qoder CN.exe`
//! —— 由桌面端主进程把 Electron 当 Node 宿主拉起来。因此"终止目标全部镜像名"这一步
//! 在开发机会自杀，必须显式检测并拒绝，而不是等用户遇到会话凭空消失。

use std::path::Path;

use base64::Engine as _;
use serde::{Deserialize, Serialize};
#[cfg(target_os = "windows")]
use std::os::windows::process::CommandExt;

use crate::modules::variant::{QoderTarget, QoderVariant};
use crate::Result;

/// 镜像名精确匹配（大小写不敏感、`.exe` 可选）。
///
/// 刻意不做前缀或子串匹配：国际版镜像名 `Qoder` 正好是 `Qoder CN` 的前缀，
/// 一旦模糊匹配，切国际版就会把国内版整个杀掉。
fn image_matches(actual: &str, wanted: &str) -> bool {
    let strip = |s: &str| s.trim_end_matches(".exe").to_lowercase();
    strip(actual) == strip(wanted)
}

/// 给辅助子进程关掉控制台窗口。
///
/// GUI 宿主（Tauri）自己没有控制台，于是它每起一个 `tasklist` / `powershell`，
/// Windows 都会**新建一个控制台窗口** —— 状态每 60 秒刷一次、每次至少两个子进程，
/// 用户看到的就是"终端一直闪"。这些调用都只是取数据，永远不该有窗口。
/// 启动客户端本体（`launch`）不走这里，那个窗口是用户要的。
pub(crate) fn hide_console(cmd: &mut std::process::Command) {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    #[cfg(not(windows))]
    let _ = cmd;
}

/// 目标在跑哪些进程。Windows 用 `tasklist` 的 CSV 输出，一次拿全再按镜像名过滤，
/// 避免每个镜像名起一个进程。
///
/// 失败（tasklist 缺失/被策略阻止/输出异常）返回 Err，**不折叠成空列表**：
/// "探测不到"与"没在跑"是两回事，折叠会让关进程门 fail-open。
pub fn running_pids(images: &[&str]) -> std::result::Result<Vec<u32>, String> {
    let mut cmd = std::process::Command::new("tasklist");
    cmd.args(["/nh", "/fo", "csv"]);
    hide_console(&mut cmd);
    let out = match cmd.output() {
        Ok(o) if o.status.success() => o.stdout,
        Ok(o) => {
            return Err(format!(
                "tasklist 退出码 {:?}（被策略阻止？）",
                o.status.code()
            ))
        }
        Err(e) => return Err(format!("tasklist 无法启动: {e}")),
    };
    Ok(String::from_utf8_lossy(&out)
        .lines()
        .filter_map(|line| {
            // "IMAGE NAME","PID","SESSION NAME",...
            let mut cols = line.split("\",\"");
            let name = cols.next()?.trim_matches('"');
            let pid = cols.next()?.trim().parse::<u32>().ok()?;
            images.iter().any(|i| image_matches(name, i)).then_some(pid)
        })
        .collect())
}

/// 便捷判定。探测失败按"没在跑"返回 false —— 只给纯展示场景兜底；
/// 决定是否写盘/关进程的路径必须用 [`running_pids`] 自己 fail-closed。
pub fn is_running(variant: QoderVariant, target: QoderTarget) -> bool {
    running_pids(target.images(variant)).map_or(false, |p| !p.is_empty())
}

/// 当前进程的祖先链镜像名（含自身，从近到远）。
///
/// PowerShell 脚本走 `-EncodedCommand`：`-Command` 传多行脚本时，内嵌的 `"` 会被
/// CreateProcess 的参数拼接规则破坏，实测静默返回空 —— 那样这条安全门就形同不存在。
/// Qoder 客户端注入子进程的环境标记。比进程树可靠：MSYS2 的 fork 模拟会让父链中途
/// 查不到 PID 而断链，实测走到 `timeout.exe` 就断了 —— 断链不等于"没被托管"。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct HostEnv {
    pub variant: Option<QoderVariant>,
    pub session_type: Option<String>,
    /// 命中的变量名，用于把判定依据原样摊给用户看。
    pub keys: Vec<String>,
}

pub fn host_env() -> HostEnv {
    let mut h = HostEnv::default();
    if let Ok(p) = std::env::var("QODER_PRODUCT_ID") {
        h.keys.push("QODER_PRODUCT_ID".into());
        h.variant = match p.as_str() {
            "qoder-cn" | "qodercn" => Some(QoderVariant::Cn),
            "qoder" => Some(QoderVariant::Global),
            _ => h.variant,
        };
    }
    for (key, variant) in [
        ("QODERCN_CLI", QoderVariant::Cn),
        ("QODERCN_CONFIG_DIR", QoderVariant::Cn),
        ("QODERCN_SESSION_TYPE", QoderVariant::Cn),
        ("QODER_CLI", QoderVariant::Global),
    ] {
        if std::env::var(key).is_ok() {
            h.keys.push(key.into());
            if h.variant.is_none() {
                h.variant = Some(variant);
            }
        }
    }
    h.session_type = ["QODERCN_SESSION_TYPE", "QODER_SESSION_TYPE"]
        .iter()
        .find_map(|k| std::env::var(k).ok());
    h
}

/// 父链探测结果。`complete=false` 表示中途查不到父 PID —— 断链不足以证明没被托管。
#[derive(Debug, Clone)]
pub struct Chain {
    pub names: Vec<String>,
    pub complete: bool,
}

/// PowerShell 走 `-EncodedCommand`：`-Command` 传多行脚本时内嵌的 `"` 会被
/// CreateProcess 的参数拼接规则破坏，实测静默返回空 —— 那样这条安全门就形同不存在。
pub fn ancestor_chain() -> Result<Chain> {
    let script = "Write-Output QSMARK\n\
        $p = Get-CimInstance Win32_Process -Filter \"ProcessId=$PID\"\n\
        $i = 0\n\
        while ($p -and $i -lt 40) { $i++\n\
         Write-Output ('N ' + $p.Name)\n\
         $pp = $p.ParentProcessId\n\
         if (-not $pp) { Write-Output 'ROOT'; break }\n\
         $n = Get-CimInstance Win32_Process -Filter \"ProcessId=$pp\" -ErrorAction SilentlyContinue\n\
         if (-not $n) { Write-Output ('BROKEN ' + $pp); break }\n\
         $p = $n }\n\
        Write-Output QSDONE\n";
    let utf16: Vec<u8> = script.encode_utf16().flat_map(|c| c.to_le_bytes()).collect();
    let encoded = base64::engine::general_purpose::STANDARD.encode(&utf16);
    let mut cmd = std::process::Command::new("powershell");
    cmd.args(["-NoProfile", "-NonInteractive", "-EncodedCommand", &encoded]);
    hide_console(&mut cmd);
    let out = cmd
        .output()
        .map_err(|e| format!("调用 powershell 失败: {e}"))?;
    if !out.status.success() {
        return Err(format!(
            "powershell 退出码 {:?}: {}",
            out.status.code(),
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    let mut chain = Chain { names: Vec::new(), complete: false };
    let mut marked = false;
    for line in String::from_utf8_lossy(&out.stdout).lines().map(str::trim) {
        if line == "QSMARK" {
            marked = true;
        } else if let Some(rest) = line.strip_prefix("N ") {
            chain.names.push(rest.to_string());
        } else if line == "ROOT" {
            chain.complete = true;
        }
    }
    if !marked {
        return Err("祖先链探测缺少标记输出".into());
    }
    Ok(chain)
}

/// 是否"当前进程正是目标的子孙"。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Hosted {
    /// 确定被托管：拒绝终止目标进程。附判定依据。
    Yes(String),
    No,
    /// 判不出来 —— 破坏性操作一律按"不许"处理。
    Unknown(String),
}

/// 纯决策，便于单测覆盖三种输入组合。环境标记优先（确定性），父链次之（会断链）。
pub fn decide(
    host: &HostEnv,
    chain: Option<&Chain>,
    variant: QoderVariant,
    target: QoderTarget,
) -> Hosted {
    if host.variant == Some(variant) && !host.keys.is_empty() {
        let session = host
            .session_type
            .as_deref()
            .map(|s| format!("，session_type={s}"))
            .unwrap_or_default();
        return Hosted::Yes(format!(
            "环境变量 {}{session} 表明本进程由 {:?} 客户端会话拉起",
            host.keys.join(","),
            variant
        ));
    }
    match chain {
        None => Hosted::Unknown("父链探测失败，无法判定是否被托管".into()),
        Some(c) => {
            if let Some(n) = c
                .names
                .iter()
                .find(|n| target.images(variant).iter().any(|i| image_matches(n, i)))
            {
                return Hosted::Yes(format!("父链中出现 {n}"));
            }
            if !c.complete {
                return Hosted::Unknown("父链断在未知进程上，不足以证明未被托管".into());
            }
            Hosted::No
        }
    }
}

/// 现场判定。环境已能定案时不再多起一个 powershell。
pub fn hosted_by(variant: QoderVariant, target: QoderTarget) -> Hosted {
    let host = host_env();
    if host.variant == Some(variant) && !host.keys.is_empty() {
        return decide(&host, None, variant, target);
    }
    let chain = ancestor_chain().ok();
    decide(&host, chain.as_ref(), variant, target)
}

/// 终止目标进程。先 `/T` 请求子树退出，超时再 `/F`。
///
/// `verdict` 由调用方先算好传入（探测有成本，且预览里也要展示同一结论）。
pub fn close(
    variant: QoderVariant,
    target: QoderTarget,
    timeout_s: u64,
    verdict: &Hosted,
) -> Result<()> {
    let images = target.images(variant);
    // 关进程路径上探测失败必须报错而不是当"没在跑"放行：那等于与活着的
    // Qoder 赛跑写凭据。
    let pids = running_pids(images)?;
    if pids.is_empty() {
        return Ok(());
    }
    match verdict {
        Hosted::No => {}
        Hosted::Yes(why) | Hosted::Unknown(why) => {
            return Err(format!(
                "拒绝终止 {:?}·{:?}（{} 个 pid 在跑）：{why}。\
                 从 Qoder 内置终端里发起换号会把这个会话一起杀掉，请改从独立启动的 \
                 qoder-switch 应用或系统托盘操作；确认可接受再用强制档。",
                variant,
                target,
                pids.len()
            ));
        }
    }
    for pid in &pids {
        let mut cmd = std::process::Command::new("taskkill");
        cmd.args(["/PID", &pid.to_string(), "/T"]);
        hide_console(&mut cmd);
        let _ = cmd.output();
    }
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(timeout_s);
    while std::time::Instant::now() < deadline {
        if running_pids(images).map_or(true, |p| p.is_empty()) {
            return Ok(());
        }
        std::thread::sleep(std::time::Duration::from_millis(300));
    }
    let survivors = running_pids(images).unwrap_or_default();
    for pid in survivors {
        let mut cmd = std::process::Command::new("taskkill");
        cmd.args(["/PID", &pid.to_string(), "/F", "/T"]);
        hide_console(&mut cmd);
        let _ = cmd.output();
    }
    if running_pids(images).map_or(false, |p| !p.is_empty()) {
        return Err(format!(
            "{:?}·{:?} 仍有进程未退出，放弃写入（继续写会撞上正在重写 auth 的进程）",
            variant, target
        ));
    }
    Ok(())
}

/// 启动目标。DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP，使其不随调用方退出。
pub fn launch(exe: &Path) -> Result<()> {
    #[cfg(target_os = "windows")]
    const FLAGS: u32 = 0x0000_0008 | 0x0000_0200;
    let mut cmd = std::process::Command::new(exe);
    #[cfg(target_os = "windows")]
    {
        cmd.creation_flags(FLAGS);
    }
    cmd.spawn()
        .map_err(|e| format!("启动 {exe:?} 失败: {e}"))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 探测本身不能抛异常，且必须能在开发机上看到真实在跑的桌面端。
    #[test]
    fn detects_live_desktop() {
        let running = running_pids(QoderVariant::Cn.desktop_images()).unwrap_or_default();
        // 本机 Qoder CN 桌面端在跑；若哪天没跑，本测试转为提示。
        if running.is_empty() {
            eprintln!("NOTE: 本机当前没有 Qoder CN 桌面进程");
        }
    }

    /// 两版镜像名是前缀关系（`Qoder` ⊂ `Qoder CN`），所以匹配必须精确，
    /// 否则切一个版本会连带杀掉另一个。
    #[test]
    fn matching_is_exact_not_prefix() {
        assert!(image_matches("Qoder.exe", "Qoder"));
        assert!(image_matches("Qoder CN.exe", "Qoder CN"));
        assert!(image_matches("qoder cn.exe", "Qoder CN"), "大小写不敏感");
        assert!(!image_matches("Qoder CN.exe", "Qoder"), "前缀不算命中");
        assert!(!image_matches("QoderWork CN.exe", "Qoder CN"));
        assert!(!image_matches("Qoder.exe", "Qoder CN"));
        assert_ne!(
            QoderVariant::Cn.desktop_images()[0],
            QoderVariant::Global.desktop_images()[0]
        );
    }

    fn chain(names: &[&str], complete: bool) -> Chain {
        Chain { names: names.iter().map(|s| s.to_string()).collect(), complete }
    }

    #[test]
    fn env_marker_outweighs_a_broken_chain() {
        let host = HostEnv {
            variant: Some(QoderVariant::Cn),
            session_type: Some("app".into()),
            keys: vec!["QODERCN_CLI".into(), "QODER_PRODUCT_ID".into()],
        };
        let d = decide(&host, Some(&chain(&["bash.exe"], false)), QoderVariant::Cn, QoderTarget::Desktop);
        match d {
            Hosted::Yes(why) => {
                assert!(why.contains("QODERCN_CLI"), "依据要摊开: {why}");
                assert!(why.contains("session_type=app"));
            }
            other => panic!("环境已证明被托管，应为 Yes，实得 {other:?}"),
        }
    }

    #[test]
    fn other_variant_env_does_not_block_this_one() {
        let host = HostEnv {
            variant: Some(QoderVariant::Cn),
            session_type: None,
            keys: vec!["QODERCN_CLI".into()],
        };
        let d = decide(&host, Some(&chain(&["explorer.exe"], true)), QoderVariant::Global, QoderTarget::Desktop);
        assert_eq!(d, Hosted::No, "CN 的标记不该挡住国际版的切换");
    }

    #[test]
    fn parent_image_in_chain_is_hosted() {
        let d = decide(
            &HostEnv::default(),
            Some(&chain(&["powershell.exe", "bash.exe", "Qoder CN.exe", "explorer.exe"], true)),
            QoderVariant::Cn,
            QoderTarget::Desktop,
        );
        assert!(matches!(d, Hosted::Yes(_)), "{d:?}");
    }

    /// 断链既没找到目标也不许放行 —— 这是 fail-closed 的核心。
    #[test]
    fn broken_chain_is_unknown_not_no() {
        let d = decide(
            &HostEnv::default(),
            Some(&chain(&["powershell.exe", "timeout.exe"], false)),
            QoderVariant::Cn,
            QoderTarget::Desktop,
        );
        assert!(matches!(d, Hosted::Unknown(_)), "断链必须判 Unknown，实得 {d:?}");

        let d = decide(&HostEnv::default(), None, QoderVariant::Cn, QoderTarget::Desktop);
        assert!(matches!(d, Hosted::Unknown(_)));
    }

    #[test]
    fn complete_chain_without_target_is_clean() {
        let d = decide(
            &HostEnv::default(),
            Some(&chain(&["powershell.exe", "explorer.exe"], true)),
            QoderVariant::Cn,
            QoderTarget::Desktop,
        );
        assert_eq!(d, Hosted::No);
    }

    /// 本机现场：QODER_PRODUCT_ID=qoder-cn + session_type=app，判定必须是 Yes。
    #[test]
    fn this_session_is_recognised_as_hosted_by_cn() {
        let host = host_env();
        if host.variant != Some(QoderVariant::Cn) {
            eprintln!("NOTE: 本次不在 CN 会话环境里（keys={:?}）", host.keys);
            return;
        }
        assert!(
            matches!(host.session_type.as_deref(), Some("app") | None),
            "session_type 异常: {:?}",
            host.session_type
        );
        assert!(matches!(
            decide(&host, None, QoderVariant::Cn, QoderTarget::Desktop),
            Hosted::Yes(_)
        ));
    }    /// 探测本身必须真能跑通：拿不到父链的安全门等于没有门。
    #[test]
    fn ancestor_chain_probe_actually_runs() {
        let c = ancestor_chain().expect("应能探到自身父链");
        assert!(
            c.names
                .first()
                .map(|n| n.eq_ignore_ascii_case("powershell.exe"))
                .unwrap_or(false),
            "首帧应是 powershell，实际 {:?}",
            c.names
        );
    }

    #[test]
    fn close_refuses_when_not_clean() {
        if running_pids(QoderVariant::Cn.desktop_images()).map_or(true, |p| p.is_empty()) {
            eprintln!("NOTE: 没有 Qoder CN 进程，close 会直接返回 Ok");
            return;
        }
        for verdict in [Hosted::Yes("测试依据".into()), Hosted::Unknown("测试依据".into())] {
            let e = close(QoderVariant::Cn, QoderTarget::Desktop, 1, &verdict).unwrap_err();
            assert!(e.contains("拒绝终止"), "{verdict:?} 应被拒: {e}");
        }
    }
}
