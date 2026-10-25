//! Qoder 客户端设备风控凭据（`Cosy-MachineToken` / `Cosy-MachineCode` / `Cosy-MachineType`）。
//!
//! 国际版（openapi.qoder.sh）的「每日 100 Credits」签到要求携带官方客户端生成的
//! 设备风控三头；国内版不需要。三头由本机 Qoder 客户端自带的
//! `<resources>/umid/runtime-info` 子程序生成（协议来源：CreditDaddy 项目静态取证，
//! MIT；落档于 `docs/qoder-endpoints.md` §5，待真机只读探测确认）：
//!
//! - 命令行：`runtime-info <env> --account-stdin`，向 stdin 写 `{"account":"<uid>"}\n`
//!   （Linux 分支不带 stdin 参数，与客户端一致）
//! - `env` 取值：客户端自带组件 国际版=3 / 国内版=0；qodercli 解压的组件（客户端会
//!   落在 `~/.qoder[|-cn]/.bin/umid-win32-*`）国际版=4 / 国内版=0 —— 两套取值不同，
//!   必须随发现来源携带
//! - 输出：stdout 第一行 JSON，取 `machineToken` / `machineCode` / `machineType`
//!   （字符串，各 ≤4096 字符）；整体输出 >1MB 判失败
//!
//! 缓存与刷新：客户端自身每 60±5 分钟刷新一次风控身份，这里保守取 50 分钟 TTL，
//! 按 `variant:uid` 进程内惰性缓存，不落盘；任何失败都不缓存 —— 调用方拿到 None 后
//! 降级为**不带风控头继续请求**并在结果里如实标注（服务端对无风控请求的表现是
//! "没有每日积分活动"，绝不编造成功）。
//!
//! 范围：Windows/macOS 桌面场景。Linux 无客户端环境需要从 `@qoder-ai/qodercli`
//! 提取 UMID 组件（CreditDaddy 的 `umid install`），不在本项目范围；发现链里的
//! `~/.qoder[|-cn]/.bin` 兜底只认 qodercli 自己解压好的现成组件。
//!
//! 红线：三头是设备凭据，日志只允许出现长度与哈希前缀（`QS_RISK_DEBUG=1` 的
//! 诊断输出同样遵守）。

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use serde_json::Value;

use crate::modules::config::PathRoots;
use crate::modules::process::hide_console;
use crate::modules::variant::{cli_dir, desktop_dir, QoderVariant};

/// 客户端自身每 60±5 分钟刷新风控身份，保守取 50 分钟（CreditDaddy 同款取舍）。
const RISK_TTL: Duration = Duration::from_secs(50 * 60);
/// runtime-info 的输出上限与单字段上限（与客户端/CreditDaddy 的校验一致）。
const OUTPUT_CAP: usize = 1024 * 1024;
const FIELD_CAP: usize = 4096;
const RUN_TIMEOUT: Duration = Duration::from_secs(25);

/// 一次 runtime-info 调用产出的设备风控三头。
#[derive(Clone, PartialEq, Eq)]
pub struct RiskIdentity {
    pub machine_token: String,
    pub machine_code: String,
    pub machine_type: String,
}

// 手写 Debug 而非派生：三个字段都是设备凭据，{:?} 一旦被后人加进日志就会把
// 设备身份打出机外。只露长度与哈希前缀，与全库「只输出长度/键名/哈希前缀」一致。
impl std::fmt::Debug for RiskIdentity {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RiskIdentity")
            .field("machine_token", &fingerprint(&self.machine_token))
            .field("machine_code", &fingerprint(&self.machine_code))
            .field("machine_type", &fingerprint(&self.machine_type))
            .finish()
    }
}

fn fingerprint(s: &str) -> String {
    use sha2::{Digest, Sha256};
    let hex: String = Sha256::digest(s.as_bytes())
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    format!("len={}, sha256={}..", s.len(), &hex[..16.min(hex.len())])
}

/// 一条可尝试的风控凭据来源：二进制路径 + 它所属体系的区域 env 取值。
struct RiskCandidate {
    exe: PathBuf,
    env: &'static str,
    /// 该 resources 目录的 build-manifest.json（读 Cosy-Version 用，可缺）。
    manifest: Option<PathBuf>,
}

fn client_env_code(variant: QoderVariant) -> &'static str {
    match variant {
        QoderVariant::Cn => "0",
        // 客户端组件的区域枚举：3 = 国际（CreditDaddy qoderApp.js 实测取值）。
        QoderVariant::Global => "3",
    }
}

fn cli_env_code(variant: QoderVariant) -> &'static str {
    match variant {
        QoderVariant::Cn => "0",
        // qodercli 组件的区域枚举与客户端不同：4 = SINGAPORE。
        QoderVariant::Global => "4",
    }
}

fn exe_name() -> &'static str {
    if cfg!(windows) {
        "runtime-info.exe"
    } else {
        "runtime-info"
    }
}

/// 安装目录候选（Windows）。CreditDaddy 还会查卸载注册表与 Squirrel 目录；这里
/// 先不引注册表依赖也不为此起 PowerShell 子进程 —— `%LOCALAPPDATA%\Programs`
/// 是 Electron 安装器的标准落点，加上 Program Files 与 MSIX WindowsApps 已覆盖
/// 绝大多数安装；查不到就走 `.bin` 兜底，降级路径本来就是一等公民。
/// 测试注入：mock 测试把安装目录候选换成受控集合（或空），保证在装着真实
/// Qoder 客户端的机器上测试也保持封闭（不会真的 spawn 官方组件）。三个平台
/// 的 `install_base_dirs` 都先查这里。
#[cfg(test)]
static TEST_INSTALL_BASES: Mutex<Option<Vec<PathBuf>>> = Mutex::new(None);

#[cfg(test)]
pub(crate) fn set_test_install_bases(bases: Option<Vec<PathBuf>>) {
    *TEST_INSTALL_BASES
        .lock()
        .unwrap_or_else(|p| p.into_inner()) = bases;
}

#[cfg(test)]
fn test_install_bases() -> Option<Vec<PathBuf>> {
    TEST_INSTALL_BASES
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .clone()
}

#[cfg(windows)]
fn install_base_dirs() -> Vec<PathBuf> {
    #[cfg(test)]
    if let Some(bases) = test_install_bases() {
        return bases;
    }
    let mut bases = Vec::new();
    let mut push_env = |name: &str, sub: &[&str]| {
        if let Ok(root) = std::env::var(name) {
            for s in sub {
                bases.push(PathBuf::from(&root).join(s));
            }
        }
    };
    push_env("LOCALAPPDATA", &["Programs\\Qoder", "Programs\\Qoder CN"]);
    push_env("ProgramFiles", &["Qoder", "Qoder CN"]);
    push_env("ProgramFiles(x86)", &["Qoder", "Qoder CN"]);
    push_env("LOCALAPPDATA", &["Microsoft\\WindowsApps\\Qoder"]);
    bases
}

#[cfg(target_os = "macos")]
fn install_base_dirs() -> Vec<PathBuf> {
    #[cfg(test)]
    if let Some(bases) = test_install_bases() {
        return bases;
    }
    let home = std::env::var("HOME").unwrap_or_default();
    ["/Applications", "/~/Applications"]
        .iter()
        .filter_map(|d| {
            if d.starts_with("/~") {
                (!home.is_empty()).then(|| PathBuf::from(home.clone()).join(&d[2..]))
            } else {
                Some(PathBuf::from(d))
            }
        })
        .flat_map(|base| ["Qoder.app", "Qoder CN.app"].map(|a| base.join(a).join("Contents/Resources")))
        .collect()
}

/// Linux 生产环境没有客户端安装布局（UMID 组件是 Linux-only 的另一条机制，
/// 不在本项目范围）；测试通过 `set_test_install_bases` 注入受控候选。
#[cfg(not(any(windows, target_os = "macos")))]
fn install_base_dirs() -> Vec<PathBuf> {
    #[cfg(test)]
    if let Some(bases) = test_install_bases() {
        return bases;
    }
    Vec::new()
}

/// 一个安装目录下的 resources 候选，按版本从新到旧。0.3+ 启动器把实际版本放在
/// `<安装目录>\.qoder-versions\<版本>\resources`，顶层 `resources` 只是首装残留兜底。
/// 版本比较按**完整数值段**（0.10.0 > 0.3.7，字符串序会排反）。
fn resources_under_install(install: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let versions_dir = install.join(".qoder-versions");
    let mut versions: Vec<(Vec<u64>, PathBuf)> = std::fs::read_dir(&versions_dir)
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|e| {
            let name = e.file_name().to_string_lossy().to_string();
            // 每一段都按数值比较；非数字段当 0，只为不 panic（临时目录等脏名）。
            let segs: Vec<u64> = name
                .split('.')
                .map(|s| s.parse().unwrap_or(0))
                .collect();
            (!segs.is_empty()).then_some((segs, e.path()))
        })
        .collect();
    versions.sort_by(|a, b| b.0.cmp(&a.0).then(b.1.cmp(&a.1)));
    for (_, v) in versions {
        out.push(v.join("resources"));
    }
    out.push(install.join("resources"));
    out.retain(|p| p.is_dir());
    out
}

/// `~/.qoder[|-cn]/.bin/umid-win32-*` 兜底：qodercli 会把 UMID 组件解压到这里。
/// 这些组件的区域 env 取值与客户端组件不同（见 `cli_env_code`）。
fn dotbin_candidates(roots: &PathRoots, variant: QoderVariant) -> Vec<RiskCandidate> {
    let bin = cli_dir(roots, variant).join(".bin");
    let mut out = Vec::new();
    if let Ok(entries) = std::fs::read_dir(&bin) {
        let mut dirs: Vec<PathBuf> = entries
            .flatten()
            .map(|e| e.path())
            .filter(|p| {
                p.is_dir()
                    && p.file_name()
                        .map(|n| n.to_string_lossy().starts_with("umid-"))
                        .unwrap_or(false)
            })
            .collect();
        dirs.sort();
        for d in dirs {
            let exe = d.join(exe_name());
            if exe.is_file() {
                out.push(RiskCandidate {
                    exe,
                    env: cli_env_code(variant),
                    manifest: None,
                });
            }
        }
    }
    out
}

/// 发现链：客户端安装目录（env=3/0）优先，其次 qodercli 解压目录（env=4/0）。
/// 两条来源都按 variant 匹配，但**不互斥** —— 只装了另一档客户端时也会用它
/// （runtime-info 的输出是设备身份，跨档可用，CreditDaddy 同款策略）。
fn risk_candidates(roots: &PathRoots, variant: QoderVariant) -> Vec<RiskCandidate> {
    let mut out: Vec<RiskCandidate> = Vec::new();
    let mut seen: Vec<PathBuf> = Vec::new();
    for base in install_base_dirs() {
        for resources in resources_under_install(&base) {
            let exe = resources.join("umid").join(exe_name());
            if !exe.is_file() || seen.contains(&exe) {
                continue;
            }
            seen.push(exe.clone());
            out.push(RiskCandidate {
                exe,
                env: client_env_code(variant),
                manifest: {
                    let m = resources.join("build-manifest.json");
                    m.is_file().then_some(m)
                },
            });
        }
    }
    // 兜底来源排在客户端之后：客户端组件的 env 语义与官方安装布局更可信。
    for c in dotbin_candidates(roots, variant) {
        if !seen.contains(&c.exe) {
            seen.push(c.exe.clone());
            out.push(c);
        }
    }
    out
}

/// 从 runtime-info 的 stdout 首行解析三头。纯函数，测试直接喂样例。
fn parse_runtime_info_output(stdout: &str) -> Result<RiskIdentity, String> {
    if stdout.len() > OUTPUT_CAP {
        return Err(format!("runtime-info 输出过大（{} 字节），判定不可信", stdout.len()));
    }
    let first = stdout.lines().next().unwrap_or("");
    let v: Value = serde_json::from_str(first).map_err(|e| format!("runtime-info 输出不是 JSON: {e}"))?;
    let field = |name: &str| -> Result<String, String> {
        let s = v
            .get(name)
            .and_then(|x| x.as_str())
            .filter(|s| !s.is_empty())
            .ok_or_else(|| format!("runtime-info 输出缺少 {name} 字段"))?;
        if s.len() > FIELD_CAP {
            return Err(format!("{name} 超长（{} 字符），判定不可信", s.len()));
        }
        Ok(s.to_string())
    };
    Ok(RiskIdentity {
        machine_token: field("machineToken")?,
        machine_code: field("machineCode")?,
        machine_type: field("machineType")?,
    })
}

type Runner<'a> = dyn Fn(&Path, &str, &str) -> std::io::Result<RiskIdentity> + 'a;

/// 真实执行 runtime-info：spawn、stdin 写账号、限时收 stdout。
/// 阻塞最长 RUN_TIMEOUT —— 调用方都在 spawn_blocking / 每请求线程 / 调度线程上。
fn run_runtime_info(exe: &Path, env: &str, uid: &str) -> std::io::Result<RiskIdentity> {
    use std::io::Write;
    use std::process::{Command, Stdio};

    let mut cmd = Command::new(exe);
    cmd.arg(env).stdout(Stdio::piped()).stderr(Stdio::null());
    let with_stdin = !cfg!(target_os = "linux");
    if with_stdin {
        cmd.arg("--account-stdin").stdin(Stdio::piped());
    }
    hide_console(&mut cmd);
    let mut child = cmd.spawn()?;

    // stdout 必须在独立线程读：输出超过管道缓冲区时，先 wait 再 read 会互相等死。
    let mut stdout = child.stdout.take();
    let (tx, rx) = std::sync::mpsc::channel();
    let reader = std::thread::spawn(move || {
        let mut buf = Vec::new();
        if let Some(s) = stdout.as_mut() {
            let mut chunk = [0u8; 8192];
            loop {
                match std::io::Read::read(s, &mut chunk) {
                    Ok(0) => break,
                    Ok(n) => {
                        buf.extend_from_slice(&chunk[..n]);
                        if buf.len() > OUTPUT_CAP {
                            break;
                        }
                    }
                    Err(_) => break,
                }
            }
        }
        let _ = tx.send(buf);
    });

    if with_stdin {
        if let Some(mut stdin) = child.stdin.take() {
            // 账号 id 来自本机账号库且过了白名单校验，这里再做成 JSON 字符串
            // 双保险；写入失败不影响判定 —— 组件可能根本不需要 stdin。
            let payload = serde_json::json!({ "account": uid }).to_string();
            let _ = stdin.write_all(payload.as_bytes());
            let _ = stdin.write_all(b"\n");
            drop(stdin);
        }
    }

    let deadline = Instant::now() + RUN_TIMEOUT;
    let status = loop {
        match child.try_wait()? {
            Some(st) => break st,
            None if Instant::now() >= deadline => {
                let _ = child.kill();
                let _ = child.wait();
                let _ = reader.join();
                return Err(std::io::Error::new(
                    std::io::ErrorKind::TimedOut,
                    format!("runtime-info 超过 {}s 未退出", RUN_TIMEOUT.as_secs()),
                ));
            }
            None => std::thread::sleep(Duration::from_millis(100)),
        }
    };
    let output = rx
        .recv_timeout(Duration::from_secs(2))
        .unwrap_or_default();
    let _ = reader.join();
    if !status.success() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::Other,
            format!("runtime-info 退出码 {:?}", status.code()),
        ));
    }
    let text = String::from_utf8_lossy(&output).to_string();
    parse_runtime_info_output(&text)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))
}

struct CachedRisk {
    value: RiskIdentity,
    at: Instant,
}

/// `HashMap::new` 不是 const，静态缓存用 OnceLock 惰性初始化（与 config.rs 的
/// 既有约定一致）。
static RISK_CACHE: std::sync::OnceLock<Mutex<HashMap<(QoderVariant, String), CachedRisk>>> =
    std::sync::OnceLock::new();

fn risk_cache() -> &'static Mutex<HashMap<(QoderVariant, String), CachedRisk>> {
    RISK_CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

#[cfg(test)]
pub(crate) fn clear_risk_cache() {
    risk_cache()
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .clear();
}

/// 测试互斥：TEST_INSTALL_BASES、风险缓存都是进程级静态，并行测试会互相
/// 踩（一个测试 set(None) 会把另一个正在跑的发现链清空）。所有碰这些全局
/// 的测试（含 quota 的 HTTP mock 测试）都必须持锁。
#[cfg(test)]
pub(crate) static RISK_TEST_LOCK: Mutex<()> = Mutex::new(());

fn debug_note(msg: &str) {
    if std::env::var("QS_RISK_DEBUG").as_deref() == Ok("1") {
        // 诊断出口同样不打印任何凭据值，只报告候选与失败原因。
        eprintln!("[qs-risk] {msg}");
    }
}

/// 取设备风控三头；任何失败返回 None（调用方降级为无风控头并如实标注）。
pub fn get_risk_identity(roots: &PathRoots, variant: QoderVariant, uid: &str) -> Option<RiskIdentity> {
    get_risk_identity_with(roots, variant, uid, &run_runtime_info)
}

/// 同上，但执行器可注入（测试用假 runner，不碰真实组件）。
fn get_risk_identity_with(
    roots: &PathRoots,
    variant: QoderVariant,
    uid: &str,
    runner: &Runner,
) -> Option<RiskIdentity> {
    let key = (variant, uid.to_string());
    if let Some(cached) = risk_cache().lock().unwrap_or_else(|p| p.into_inner()).get(&key) {
        if cached.at.elapsed() < RISK_TTL {
            return Some(cached.value.clone());
        }
    }
    for cand in risk_candidates(roots, variant) {
        debug_note(&format!(
            "尝试 runtime-info: {} (env={})",
            cand.exe.display(),
            cand.env
        ));
        match runner(&cand.exe, cand.env, uid) {
            Ok(id) => {
                debug_note("runtime-info 成功");
                risk_cache()
                    .lock()
                    .unwrap_or_else(|p| p.into_inner())
                    .insert(key, CachedRisk { value: id.clone(), at: Instant::now() });
                return Some(id);
            }
            Err(e) => debug_note(&format!("runtime-info 失败: {e}")),
        }
    }
    None
}

/// `Cosy-MachineId`：优先客户端 userData 下的 `auth.machine-id`（设备真实身份，
/// 包体系已把该文件按 `FileRole::DesktopMachineId` 随包备份）；客户端不在时回退
/// 账号库里的按档位持久化 UUID —— 换设备时文件跟着 `~/.qs-switch` 走。
pub fn machine_id_for(roots: &PathRoots, store: &Path, variant: QoderVariant) -> String {
    let valid = |s: &str| uuid::Uuid::parse_str(s.trim()).is_ok();
    let live = desktop_dir(roots, variant).join("auth.machine-id");
    if let Ok(s) = std::fs::read_to_string(&live) {
        if valid(&s) {
            return s.trim().to_string();
        }
    }
    let path = store.join(format!("machine-id-{}", variant.region_code()));
    if let Ok(s) = std::fs::read_to_string(&path) {
        if valid(&s) {
            return s.trim().to_string();
        }
    }
    let fresh = uuid::Uuid::new_v4().to_string();
    if let Err(e) = crate::modules::config::atomic_write_bytes(&path, fresh.as_bytes()) {
        debug_note(&format!("machine-id 落盘失败（用一次性值顶替）: {e}"));
    }
    fresh
}

/// `Cosy-MachineHostname`：可打印 ASCII ≤96；脏值清洗后追加短哈希以便区分
/// 「清洗前的原名」。拿不到主机名返回 None（该头可选，直接不带）。
pub fn machine_hostname() -> Option<String> {
    let raw = if cfg!(windows) {
        std::env::var("COMPUTERNAME").ok()?
    } else {
        std::env::var("HOSTNAME")
            .ok()
            .or_else(|| std::fs::read_to_string("/etc/hostname").ok())
            .map(|s| s.trim().to_string())?
    };
    let cleaned: String = raw.chars().filter(|c| c.is_ascii_graphic()).collect();
    if cleaned.is_empty() {
        return None;
    }
    if cleaned == raw && cleaned.len() <= 96 {
        return Some(cleaned);
    }
    let truncated: String = cleaned.chars().take(87).collect();
    Some(format!("{}-{}", truncated, &fingerprint(&raw)[9..17]))
}

/// 客户端版本号（`Cosy-Version` 的真实取值来源）：resources 目录的
/// `build-manifest.json` → `productVersion`。不硬编码 —— 版本漂移是这组头里
/// 最容易发生的事。
pub fn client_product_version(roots: &PathRoots, variant: QoderVariant) -> Option<String> {
    for cand in risk_candidates(roots, variant) {
        if let Some(manifest) = cand.manifest {
            if let Ok(bytes) = std::fs::read(&manifest) {
                if let Ok(v) = serde_json::from_slice::<Value>(&bytes) {
                    if let Some(s) = v.get("productVersion").and_then(|x| x.as_str()) {
                        if !s.trim().is_empty() {
                            return Some(s.trim().to_string());
                        }
                    }
                }
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;
    use std::rc::Rc;

    fn sandbox() -> (PathBuf, PathRoots, PathBuf) {
        let tmp = std::env::temp_dir().join(format!("qs-risk-{}", uuid::Uuid::new_v4().simple()));
        let roots = PathRoots::sandbox(&tmp);
        let store = tmp.join("store");
        std::fs::create_dir_all(&store).unwrap();
        (tmp, roots, store)
    }

    #[test]
    fn parse_accepts_valid_shape_and_rejects_missing_or_oversized() {
        let ok = parse_runtime_info_output(
            r#"{"machineToken":"tok-1","machineCode":"code-1","machineType":"win"}"#,
        )
        .unwrap();
        assert_eq!(ok.machine_token, "tok-1");
        assert_eq!(ok.machine_type, "win");

        // 缺字段 / 非 JSON / 空串 → 报错，绝不半填充。
        assert!(parse_runtime_info_output(r#"{"machineToken":"t"}"#).is_err());
        assert!(parse_runtime_info_output("not json").is_err());
        assert!(parse_runtime_info_output("").is_err());

        // 超长字段 → 拒绝（输出里混入意外内容时宁可失败走降级）。
        let long = "x".repeat(FIELD_CAP + 1);
        let oversized = format!(r#"{{"machineToken":"{long}","machineCode":"c","machineType":"t"}}"#);
        assert!(parse_runtime_info_output(&oversized).is_err());

        // 首行即 JSON（与组件行为一致）；首行之后的行不影响解析。
        let noisy = "{\"machineToken\":\"a\",\"machineCode\":\"b\",\"machineType\":\"c\"}\nWARN: trailing noise\n";
        assert_eq!(parse_runtime_info_output(noisy).unwrap().machine_code, "b");
    }

    #[test]
    fn discovery_prefers_client_install_and_falls_back_to_dotbin() {
        let _g = RISK_TEST_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let (tmp, roots, _store) = sandbox();
        // 封闭安装目录候选（真机可能装着真实 Qoder，不能让测试漂出去）。
        set_test_install_bases(Some(vec![]));
        // 1) 空环境：无安装、无 .bin → 零候选（调用方降级）。
        assert!(risk_candidates(&roots, QoderVariant::Global).is_empty());

        // 2) 客户端安装目录候选 → env 用客户端口径（国际 3 / 国内 0），
        //    且带 build-manifest 路径（Cosy-Version 来源）。
        let install = tmp.join("Qoder");
        let resources = install.join(".qoder-versions").join("1.2.3").join("resources");
        std::fs::create_dir_all(resources.join("umid")).unwrap();
        std::fs::write(resources.join("umid").join(exe_name()), b"fake").unwrap();
        std::fs::write(
            resources.join("build-manifest.json"),
            r#"{"productVersion":"1.2.3"}"#,
        )
        .unwrap();
        set_test_install_bases(Some(vec![install.clone()]));
        let cands = risk_candidates(&roots, QoderVariant::Global);
        assert_eq!(cands.len(), 1);
        assert_eq!(cands[0].env, "3", "客户端组件的国际版 env 必须是 3");
        assert_eq!(
            client_product_version(&roots, QoderVariant::Global).as_deref(),
            Some("1.2.3"),
            "Cosy-Version 必须来自 build-manifest.json，不许硬编码"
        );

        // 3) qodercli 解压目录（sandbox home 下的 .qoder/.bin）→ env 用 cli 口径（4）。
        set_test_install_bases(Some(vec![]));
        let dotbin = cli_dir(&roots, QoderVariant::Global).join(".bin").join("umid-win32-x64-1.0.0");
        std::fs::create_dir_all(&dotbin).unwrap();
        std::fs::write(dotbin.join(exe_name()), b"fake").unwrap();
        let cands = risk_candidates(&roots, QoderVariant::Global);
        assert_eq!(cands.len(), 1);
        assert_eq!(cands[0].env, "4", "qodercli 解压组件的国际版 env 必须是 4");
        assert!(cands[0].manifest.is_none());

        // 4) CN 侧同型，env=0。
        let dotbin_cn = cli_dir(&roots, QoderVariant::Cn).join(".bin").join("umid-win32-x64-1.0.0");
        std::fs::create_dir_all(&dotbin_cn).unwrap();
        std::fs::write(dotbin_cn.join(exe_name()), b"fake").unwrap();
        let cands_cn = risk_candidates(&roots, QoderVariant::Cn);
        assert_eq!(cands_cn.len(), 1);
        assert_eq!(cands_cn[0].env, "0");

        set_test_install_bases(None);
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn version_sorting_prefers_newer_qoder_versions_layout() {
        let tmp = std::env::temp_dir().join(format!("qs-risk-ver-{}", uuid::Uuid::new_v4().simple()));
        let install = tmp.join("Qoder");
        for v in ["0.2.9", "0.10.0", "0.3.7"] {
            let r = install.join(".qoder-versions").join(v).join("resources");
            std::fs::create_dir_all(r).unwrap();
        }
        std::fs::create_dir_all(install.join("resources")).unwrap();
        let rs = resources_under_install(&install);
        assert_eq!(rs.len(), 4);
        // 版本数值降序：0.10.0 > 0.3.7 > 0.2.9（字符串序会把 0.10 排在 0.3 前）。
        let s = |p: &Path| p.to_string_lossy().replace('\\', "/");
        assert!(s(&rs[0]).ends_with("0.10.0/resources"));
        assert!(s(&rs[2]).ends_with("0.2.9/resources"));
        // 顶层兜底永远在最后。
        assert!(s(&rs[3]).ends_with("/resources") && !s(&rs[3]).contains(".qoder-versions"));
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn identity_is_cached_per_uid_within_ttl() {
        let _g = RISK_TEST_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let (tmp, roots, _store) = sandbox();
        // 给一条受控的候选路径（文件只需存在，runner 是假的），保证发现链在任何
        // 机器上都能走到 runner。
        let install = tmp.join("Qoder");
        let r = install.join(".qoder-versions").join("1.0.0").join("resources");
        std::fs::create_dir_all(r.join("umid")).unwrap();
        std::fs::write(r.join("umid").join(exe_name()), b"fake").unwrap();
        set_test_install_bases(Some(vec![install]));
        clear_risk_cache();
        let calls = Rc::new(Cell::new(0u32));
        let calls2 = calls.clone();
        let runner = move |_exe: &Path, _env: &str, _uid: &str| -> std::io::Result<RiskIdentity> {
            calls2.set(calls2.get() + 1);
            Ok(RiskIdentity {
                machine_token: "t".into(),
                machine_code: "c".into(),
                machine_type: "win".into(),
            })
        };
        let a = get_risk_identity_with(&roots, QoderVariant::Global, "uid-1", &runner).unwrap();
        // 同 uid 第二次：命中缓存，不再执行组件。
        let b = get_risk_identity_with(&roots, QoderVariant::Global, "uid-1", &runner).unwrap();
        assert_eq!(a, b);
        assert_eq!(calls.get(), 1);
        // 另一个 uid 是另一份缓存键：会重新执行（换号必须重新生成设备身份）。
        let _ = get_risk_identity_with(&roots, QoderVariant::Global, "uid-2", &runner).unwrap();
        assert_eq!(calls.get(), 2);
        clear_risk_cache();
        set_test_install_bases(None);
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn runner_failure_is_never_cached_and_returns_none() {
        let _g = RISK_TEST_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let (tmp, roots, _store) = sandbox();
        let install = tmp.join("Qoder");
        let r = install.join(".qoder-versions").join("1.0.0").join("resources");
        std::fs::create_dir_all(r.join("umid")).unwrap();
        std::fs::write(r.join("umid").join(exe_name()), b"fake").unwrap();
        set_test_install_bases(Some(vec![install]));
        clear_risk_cache();
        let calls = Rc::new(Cell::new(0u32));
        let calls2 = calls.clone();
        let runner = move |_e: &Path, _v: &str, _u: &str| -> std::io::Result<RiskIdentity> {
            calls2.set(calls2.get() + 1);
            Err(std::io::Error::new(std::io::ErrorKind::Other, "boom"))
        };
        assert!(get_risk_identity_with(&roots, QoderVariant::Global, "uid-x", &runner).is_none());
        assert!(get_risk_identity_with(&roots, QoderVariant::Global, "uid-x", &runner).is_none());
        // 失败不缓存：每次调用都重试（客户端可能中途装好/恢复）。
        assert_eq!(calls.get(), 2);
        clear_risk_cache();
        set_test_install_bases(None);
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn machine_id_falls_back_to_persisted_uuid_and_stays_stable() {
        let (tmp, roots, store) = sandbox();
        let a = machine_id_for(&roots, &store, QoderVariant::Global);
        assert!(uuid::Uuid::parse_str(&a).is_ok());
        let b = machine_id_for(&roots, &store, QoderVariant::Global);
        assert_eq!(a, b, "回退值必须持久化，不能每次调用换一个设备身份");
        // 两档各自独立 —— 设备指纹按档位分开，防止跨档关联。
        let c = machine_id_for(&roots, &store, QoderVariant::Cn);
        assert_ne!(a, c);
        // 真机客户端存在 auth.machine-id 时优先用它的：只验证「文件在→值等于文件」。
        let live = desktop_dir(&roots, QoderVariant::Global).join("auth.machine-id");
        std::fs::create_dir_all(desktop_dir(&roots, QoderVariant::Global)).unwrap();
        std::fs::write(&live, "01234567-89ab-cdef-0123-456789abcdef").unwrap();
        assert_eq!(machine_id_for(&roots, &store, QoderVariant::Global), "01234567-89ab-cdef-0123-456789abcdef");
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn hostname_is_sanitized_or_absent() {
        // 不假设环境里有主机名：有就要求是合法形态，没有就 None，都不算失败。
        match machine_hostname() {
            Some(h) => {
                assert!(!h.is_empty());
                assert!(h.chars().all(|c| c.is_ascii_graphic()));
                assert!(h.len() <= 96);
            }
            None => {}
        }
    }
}
