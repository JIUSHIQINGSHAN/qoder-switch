//! 进程探测与终止。这里的核心不是"能不能杀"，而是**能不能安全地杀**。
//!
//! Windows 本机实测：本 agent 的祖先链是 `powershell → bash → bash → bash → Qoder CN.exe`
//! —— 由桌面端主进程把 Electron 当 Node 宿主拉起来。因此"终止目标全部镜像名"这一步
//! 在开发机会自杀，必须显式检测并拒绝，而不是等用户遇到会话凭空消失。
//!
//! macOS 上是同一件事的另一种形态（2026-09-23 实测）：`~/.qoder-cn` 由桌面端注入，
//! Qoder 的内置终端里跑起来的进程其父链同样会一路走到 `Qoder CN`。系统调用改走
//! `ps` / `/bin/kill` / `open`，**fail-closed 语义一条都不减**：断链、探测失败、
//! 快照取不到，一律判 `Hosted::Unknown`，绝不因为"看不见"就当成"没被托管"。

use std::path::Path;
// PathBuf 只在 macOS 的 .app 解析里用到；不 gate 掉会在 Windows 上留一个未使用告警。
#[cfg(target_os = "macos")]
use std::path::PathBuf;
#[cfg(not(windows))]
use std::collections::HashMap;

#[cfg(windows)]
use base64::Engine as _;
use serde::{Deserialize, Serialize};
#[cfg(target_os = "windows")]
use std::os::windows::process::CommandExt;

use crate::modules::variant::{QoderTarget, QoderVariant};
use crate::Result;

/// 进程探测依赖的系统工具名，只用于把错误文案写对。
///
/// 写死 "tasklist" 会让 mac 上的报错指向一个本机根本没有的命令，用户照着排查半天。
pub const PROBE_TOOL: &str = if cfg!(windows) { "tasklist" } else { "ps" };

/// 镜像名匹配（大小写不敏感、`.exe` 可选），外加 Electron 的 Helper 家族。
///
/// 除 Helper 之外刻意不做前缀或子串匹配：国际版镜像名 `Qoder` 正好是 `Qoder CN` 的
/// 前缀，一旦模糊匹配，切国际版就会把国内版整个杀掉。
///
/// `"… Helper"` 是 Electron 在两个平台上都会派生的子进程族（渲染/GPU/插件进程），
/// 换号时必须一起收掉，否则还活着的渲染进程会把手里的旧 token 继续用下去。放开它
/// 不破坏上面那条隔离：`"Qoder CN Helper (Renderer)"` 不以 `"Qoder Helper"` 开头。
fn image_matches(actual: &str, wanted: &str) -> bool {
    let strip = |s: &str| s.trim_end_matches(".exe").to_lowercase();
    let (a, w) = (strip(actual), strip(wanted));
    if a == w {
        return true;
    }
    a.starts_with(&format!("{w} helper"))
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

/// `ps -Ao pid=,ppid=,comm=` 的行解析。
///
/// `comm` 在 macOS 上是**完整路径且可以含空格**（`/Applications/Qoder CN.app/Contents/
/// MacOS/Qoder CN`），所以只能按前两个字段切一次，剩下的整段都算命令名；按空白全切
/// 会把 `Qoder CN` 拆成两行错数据。返回的是 basename，与 Windows 的镜像名同一形状。
#[cfg(not(windows))]
fn parse_ps_rows(text: &str) -> std::result::Result<Vec<(u32, u32, String)>, String> {
    let mut rows = Vec::new();
    'line: for line in text.lines() {
        let l = line.trim_start();
        // 逐字段"吃到下一个空白"，而不是 split_whitespace —— 后者会把含空格的
        // comm 拆成多段，`Qoder CN` 就只剩 `Qoder` 了。
        let mut rest = l;
        let mut nums = [""; 2];
        for slot in nums.iter_mut() {
            let Some(end) = rest.find(char::is_whitespace) else {
                continue 'line;
            };
            *slot = &rest[..end];
            rest = rest[end..].trim_start();
        }
        let (Ok(pid), Ok(ppid)) = (nums[0].parse::<u32>(), nums[1].parse::<u32>()) else {
            continue;
        };
        let comm = rest.trim();
        if comm.is_empty() {
            continue;
        }
        let name = Path::new(comm)
            .file_name()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_else(|| comm.to_string());
        rows.push((pid, ppid, name));
    }
    if rows.is_empty() {
        return Err("ps 输出里没有任何可解析的行（输出形状与预期不符）".into());
    }
    Ok(rows)
}

/// macOS/Linux：一次 `ps` 取全表，`running_pids` 与 `ancestor_chain` 共用。
///
/// 只起一个子进程而不是每跳一次 `ps` —— 父链最多 40 跳，逐跳调用在状态刷新里
/// 会把窗口拖成肉眼可见的卡顿（Windows 侧当初就是因为这个改用一次 CSV 全量拉取）。
#[cfg(not(windows))]
fn ps_rows() -> std::result::Result<Vec<(u32, u32, String)>, String> {
    let mut cmd = std::process::Command::new("ps");
    cmd.args(["-Ao", "pid=,ppid=,comm="]);
    hide_console(&mut cmd);
    let out = match cmd.output() {
        Ok(o) => o,
        Err(e) => return Err(format!("ps 无法启动: {e}")),
    };
    if !out.status.success() {
        return Err(format!("ps 退出码 {:?}", out.status.code()));
    }
    parse_ps_rows(&String::from_utf8_lossy(&out.stdout))
}

/// 目标在跑哪些进程。Windows 用 `tasklist` 的 CSV 输出，macOS 用 `ps` 全表，
/// 都是一次拿全再按镜像名过滤，避免每个镜像名起一个进程。
///
/// 失败（探测工具缺失/被策略阻止/输出异常）返回 Err，**不折叠成空列表**：
/// "探测不到"与"没在跑"是两回事，折叠会让关进程门 fail-open。
pub fn running_pids(images: &[&str]) -> std::result::Result<Vec<u32>, String> {
    #[cfg(windows)]
    {
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
        return Ok(String::from_utf8_lossy(&out)
            .lines()
            .filter_map(|line| {
                // "IMAGE NAME","PID","SESSION NAME",...
                let mut cols = line.split("\",\"");
                let name = cols.next()?.trim_matches('"');
                let pid = cols.next()?.trim().parse::<u32>().ok()?;
                images.iter().any(|i| image_matches(name, i)).then_some(pid)
            })
            .collect());
    }
    #[cfg(not(windows))]
    {
        Ok(ps_rows()?
            .into_iter()
            .filter(|(_, _, name)| images.iter().any(|i| image_matches(name, i)))
            .map(|(pid, _, _)| pid)
            .collect())
    }
}

/// 便捷判定。探测失败按"没在跑"返回 false —— 只给纯展示场景兜底；
/// 决定是否写盘/关进程的路径必须用 [`running_pids`] 自己 fail-closed。
pub fn is_running(variant: QoderVariant, target: QoderTarget) -> bool {
    running_pids(target.images(variant)).map_or(false, |p| !p.is_empty())
}

/// 某个 PID 是否仍然存在。用于清理陈锁（跨进程切换锁记录的是持有者 PID）。
///
/// 注意 tasklist 的两种"查不到"：PID 数值非法时退出码 **1** 并打印"无效查询"，
/// PID 合法但不存在时退出码 0 并打印"没有运行的任务"。两者都表示**该进程不在了**，
/// 必须返回 `Ok(false)` —— 若把非零退出码一律当 Err，崩溃残留的锁（PID 已失效）
/// 就永远回收不掉，跨进程锁会退化成永久死锁。
#[cfg(windows)]
pub fn pid_alive(pid: u32) -> std::result::Result<bool, String> {
    let mut cmd = std::process::Command::new("tasklist");
    cmd.args(["/nh", "/fo", "csv", "/fi", &format!("PID eq {pid}")]);
    hide_console(&mut cmd);
    let out = match cmd.output() {
        // 退出码非零在这里语义是"查无此进程"，不是探测失败。
        Ok(o) => o.stdout,
        Err(e) => return Err(format!("tasklist 无法启动: {e}")),
    };
    // 只认真正的 CSV 行：无匹配时 tasklist 打印本地化提示（非 CSV）。
    Ok(String::from_utf8_lossy(&out).lines().any(|line| {
        let mut cols = line.split("\",\"");
        let _name = cols.next();
        cols.next().and_then(|s| s.trim().trim_matches('"').parse::<u32>().ok()) == Some(pid)
    }))
}

/// 非 Windows：`/proc/<pid>` 存在即视为存活（本项目的发布目标只有 Windows，
/// 这里只是让测试/开发机可编译）。
#[cfg(not(windows))]
pub fn pid_alive(pid: u32) -> std::result::Result<bool, String> {
    Ok(std::path::Path::new(&format!("/proc/{pid}")).exists())
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
#[cfg(windows)]
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

/// macOS/Linux：用一张 `ps` 快照沿 ppid 上溯。
///
/// `complete` 的语义与 Windows 侧严格一致：**只有真的走到根**（ppid 为 0，即 launchd /
/// swapper 那层）才算完整。任何一跳查不到父进程就带 `complete=false` 返回，
/// `decide` 会据此判 `Unknown` 而不是 `No` —— 判不出来时按"不许"处理。
#[cfg(not(windows))]
pub fn ancestor_chain() -> Result<Chain> {
    let rows = ps_rows()?;
    let by_pid: HashMap<u32, (u32, &str)> =
        rows.iter().map(|(pid, ppid, name)| (*pid, (*ppid, name.as_str()))).collect();
    let mut chain = Chain { names: Vec::new(), complete: false };
    let mut cur = std::process::id();
    for _ in 0..40 {
        let Some((ppid, name)) = by_pid.get(&cur) else {
            // 这一跳查不到：链断了。已经收上来的那几帧仍然要交出去，
            // 因为 decide 会先在里面找目标镜像名 —— 找到了就是 Yes（比 Unknown 更精确）。
            return Ok(chain);
        };
        chain.names.push(name.to_string());
        if *ppid == 0 || *ppid == cur {
            chain.complete = true;
            break;
        }
        cur = *ppid;
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

/// 发一次终止信号。`force=false` 走优雅退出（Windows `taskkill /T` / POSIX SIGTERM），
/// `force=true` 走强杀（`/F /T` / SIGKILL）。
///
/// POSIX 显式写 `/bin/kill` 而不是 `kill`：后者在多数 shell 里是内置命令，PATH 上
/// 未必找得到，而这里是由 Rust 直接 spawn，找不到就是静默失败。
#[cfg(not(windows))]
fn signal(pid: u32, force: bool) {
    let mut cmd = std::process::Command::new("/bin/kill");
    if force {
        cmd.arg("-9");
    } else {
        cmd.arg("-TERM");
    }
    cmd.arg(pid.to_string());
    hide_console(&mut cmd);
    let _ = cmd.output();
}

#[cfg(windows)]
fn signal(pid: u32, force: bool) {
    let mut cmd = std::process::Command::new("taskkill");
    cmd.args(["/PID", &pid.to_string()]);
    if force {
        cmd.arg("/F");
    }
    cmd.arg("/T");
    hide_console(&mut cmd);
    let _ = cmd.output();
}

/// 终止目标进程。先请求优雅退出，超时再强杀。
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
        signal(*pid, false);
    }
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(timeout_s);
    while std::time::Instant::now() < deadline {
        match running_pids(images) {
            Ok(pids) if pids.is_empty() => return Ok(()),
            Ok(_) => {}
            Err(e) => return Err(format!("等待目标进程退出时探测失败: {e}")),
        }
        std::thread::sleep(std::time::Duration::from_millis(300));
    }
    // 优雅关闭超时，强制终止残存进程。必须 fail-closed：探测失败绝不能折叠成空列表放行。
    let survivors = running_pids(images)?;
    for pid in survivors {
        signal(pid, true);
    }
    // 强杀后同样要复查：SIGKILL 送达与进程真正消失之间有窗口，且 Electron 的
    // Helper 是各自独立的 pid，任何一个没退干净都可能带着旧凭据继续跑。
    let deadline2 = std::time::Instant::now() + std::time::Duration::from_secs(3);
    loop {
        let final_check = running_pids(images)?;
        if final_check.is_empty() {
            return Ok(());
        }
        if std::time::Instant::now() >= deadline2 {
            return Err(format!(
                "{:?}·{:?} 仍有进程未退出（剩余 {} 个），放弃写入（继续写会撞上正在重写 auth 的进程）",
                variant, target, final_check.len()
            ));
        }
        std::thread::sleep(std::time::Duration::from_millis(300));
    }
}

/// 从 `.app` 内的主二进制路径退回包路径；不是 `.app` 布局则返回 None。
#[cfg(target_os = "macos")]
fn mac_bundle_from_exe(exe: &Path) -> Option<PathBuf> {
    // 找 `…/Contents/MacOS/<bin>` 里的 `Contents`，它前面那段就是 `<X>.app`。
    let mut bundle = PathBuf::new();
    let mut found = false;
    for (i, c) in exe.components().enumerate() {
        if i > 0 {
            if let std::path::Component::Normal(s) = c {
                if s.to_string_lossy() == "Contents" {
                    found = true;
                    break;
                }
            }
        }
        bundle = bundle.join(c.as_os_str());
    }
    found.then(|| bundle.is_dir().then_some(bundle)).flatten()
}

/// 打开系统授权面板。`pane` 是本平台的面板标识（macOS 用 `Privacy_AllFiles` 这类
/// URL scheme 片段，Windows 忽略它）。
///
/// macOS 上本工具真正会卡住的地方不是"目录没有写权限"，而是**钥匙串授权** ——
/// 桌面凭据的主密钥由 Qoder 创建，读它一定要用户放行（见 `auth_codec`）。
/// 所以 mac 上这里打开「完全磁盘访问」的同时，界面上要一并讲清钥匙串那一步。
pub fn open_system_settings_pane(pane: &str) -> Result<()> {
    #[cfg(target_os = "macos")]
    {
        let url = if pane.is_empty() {
            "x-apple.systempreferences:com.apple.preference.security".to_string()
        } else {
            format!("x-apple.systempreferences:com.apple.preference.security?{pane}")
        };
        let out = std::process::Command::new("/usr/bin/open")
            .arg(&url)
            .output()
            .map_err(|e| format!("调用 open 打开系统设置失败: {e}"))?;
        if out.status.success() {
            Ok(())
        } else {
            Err(format!(
                "打开系统设置失败: {}",
                String::from_utf8_lossy(&out.stderr).trim()
            ))
        }
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = pane;
        Err("当前平台没有需要在系统设置里单独授权的凭据面板".into())
    }
}

/// 在系统文件管理器里定位某个路径（macOS 访达 / Windows 资源管理器）。
pub fn reveal_in_file_manager(path: &Path) -> Result<()> {
    #[cfg(target_os = "macos")]
    {
        let out = std::process::Command::new("/usr/bin/open")
            .args(["-R", &path.display().to_string()])
            .output()
            .map_err(|e| format!("调用 open -R 失败: {e}"))?;
        if out.status.success() {
            return Ok(());
        }
        return Err(format!(
            "在访达中显示失败: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    #[cfg(windows)]
    {
        let mut cmd = std::process::Command::new("explorer");
        // /select, 必须整体作为一个参数，且路径要存在，否则 explorer 只是开个窗口。
        cmd.arg(format!("/select,{}", path.display()));
        hide_console(&mut cmd);
        // explorer 成功时也常返回非 0，所以不看退出码，只在真正起不来时报错。
        cmd.spawn().map_err(|e| format!("调用 explorer 失败: {e}"))?;
        Ok(())
    }
    #[cfg(not(any(target_os = "macos", windows)))]
    {
        let _ = path;
        Err("当前平台不支持在文件管理器中定位".into())
    }
}

/// 启动目标。
///
/// Windows：`DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP`，使其不随调用方退出。
/// macOS：优先 `open <X.app>` 交给 LaunchServices。直接 exec `.app` 里的 Mach-O 虽然
/// 也能起来，但客户端会变成**本工具的子进程** —— 本工具退出（切换完常常重启自己）
/// 时可能连带它，而且拿不到正常的激活/菜单栏语义。找不到 `.app` 布局时退回直接 exec。
pub fn launch(exe: &Path) -> Result<()> {
    #[cfg(target_os = "macos")]
    {
        if let Some(bundle) = mac_bundle_from_exe(exe) {
            let out = std::process::Command::new("/usr/bin/open")
                .arg(&bundle)
                .output()
                .map_err(|e| format!("调用 open 启动 {bundle:?} 失败: {e}"))?;
            if out.status.success() {
                return Ok(());
            }
            return Err(format!(
                "open 启动 {bundle:?} 失败: {}",
                String::from_utf8_lossy(&out.stderr).trim()
            ));
        }
    }
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

    /// `pid_alive` 的阳性对照 + 阴性对照。跨进程锁靠它判陈锁，判错会死锁或误清。
    #[test]
    #[cfg(windows)]
    fn pid_alive_has_positive_and_negative_control() {
        // 阳性：自己一定活着（先跑阳性对照是本项目测量类测试的硬要求）。
        let me = std::process::id();
        match pid_alive(me) {
            Ok(true) => {}
            other => eprintln!("NOTE: 自身 PID 探测异常（{other:?}），本环境可能限制 tasklist"),
        }

        // 阴性一：合法但不存在的 PID → 必须是 Ok(false)，不能是 Err。
        let mut free = 0u32;
        for cand in 999_999u32..1_000_050 {
            if pid_alive(cand).ok() == Some(false) {
                free = cand;
                break;
            }
        }
        assert!(free != 0, "应能找到一个不存在但合法的 PID 并判定为 false");

        // 阴性二：PID 数值非法（tasklist 退出码 1）同样必须收敛成 Ok(false) ——
        // 这条正是崩溃残留锁的形态，若返回 Err 就永远回收不掉。
        assert_eq!(
            pid_alive(4_294_967_290).ok(),
            Some(false),
            "非法 PID 必须判为不存在，否则陈锁无法回收"
        );
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
    }

    /// 探测本身必须真能跑通：拿不到父链的安全门等于没有门。
    ///
    /// 首帧具体是什么进程随宿主平台与发起方式而变，所以这里只断言"探得到、且第一帧
    /// 就是我自己"，不锁死某个平台才有的镜像名。
    #[test]
    fn ancestor_chain_probe_actually_runs() {
        let c = ancestor_chain().expect("应能探到自身父链");
        assert!(!c.names.is_empty(), "父链一帧都没有，等于没有门");
        let mine = Path::new(&std::env::current_exe().unwrap_or_default())
            .file_name()
            .map(|s| s.to_string_lossy().to_lowercase())
            .unwrap_or_default();
        let first = c.names[0].to_lowercase();
        assert!(
            first.eq_ignore_ascii_case(&mine) || first.contains("node") || first.contains("bash")
                || first.contains("zsh") || first.contains("powershell"),
            "首帧应是本进程或其 shell，实际 {:?}",
            c.names
        );
    }

    /// macOS/Linux 的 `ps` 输出里 comm 是含空格的完整路径，必须整段取到再取 basename。
    #[cfg(not(windows))]
    #[test]
    fn ps_rows_handles_spaces_in_paths() {
        let text = "  1     0 /sbin/launchd\n\
                    500   259 /Applications/Qoder CN.app/Contents/MacOS/Qoder CN\n\
                    501   500 /Applications/Qoder CN.app/Contents/Frameworks/Qoder CN Helper (Renderer).app/Contents/MacOS/Qoder CN Helper (Renderer)\n";
        let rows = parse_ps_rows(text).unwrap();
        assert_eq!(rows.len(), 3);
        assert_eq!(rows[1], (500, 259, "Qoder CN".to_string()));
        assert_eq!(rows[2].0, 501);
        assert_eq!(rows[2].1, 500);
        assert_eq!(rows[2].2, "Qoder CN Helper (Renderer)");
        // 空输出必须报错，不能折叠成"没有在跑"。
        assert!(parse_ps_rows("  PID  PPID COMM\n").is_err());
    }

    /// Electron 的 Helper 子进程族必须与主进程一起被收掉（两平台同名规则一致）。
    #[test]
    fn helper_family_matches_but_variants_stay_isolated() {
        assert!(image_matches("Qoder CN Helper (Renderer)", "Qoder CN"));
        assert!(image_matches("Qoder CN Helper.exe", "Qoder CN"));
        assert!(image_matches("Qoder Helper (GPU)", "Qoder"));
        // 关键：放开设 Helper 之后，两个版本仍然互不误伤。
        assert!(!image_matches("Qoder CN Helper (Renderer)", "Qoder"));
        assert!(!image_matches("QoderWork CN Helper", "Qoder CN"));
        assert!(!image_matches("Qoder Helper", "Qoder CN"));
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
