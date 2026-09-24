//! Qoder 家族「版本轴 × 目标轴」的单一事实来源。
//!
//! 版本轴 `QoderVariant` = 国内版 / 国际版；目标轴 `QoderTarget` = 同一台机器上
//! 三个各自持有登录态的客户端形态。任何路径、进程镜像名、域名只允许从这里取。
//!
//! 所有取路径的函数都接受 `&PathRoots`，因此可在沙箱目录里整棵重建（见
//! `PathRoots::sandbox`），测试永远不会写到真实产品目录。
//!
//! 本机实测依据（2026-09-20，Windows 11 26200）：
//! - `%APPDATA%\com.qodercn.app.stable` 与 `com.qoder.app.stable` 均含
//!   `auth.v1.dat` / `auth.machine-id` / `Local State` / `channel-activation.v1.json`，
//!   CN 侧另有瞬时 `lockfile`；两目录都**没有** `auth-profile-overlays.v1.dat`。
//! - `~/.qoder/.auth/user` 存在但 mtime 停在 7-27，而同目录
//!   `.qoder-app-status.json` 的 `snapshot_at` 是 9-19 且 `writer:"main"` ——
//!   推断：CLI 自持的凭据文件已成遗留，登录态实际由桌面端注入。
//! - `~/.qoder-cn/.auth/` 只有 `machine_id`，**没有** `user`，与上述推断一致。
//! - `%APPDATA%\QoderWork CN` 含 `auth.dat` + `auth-v2.dat`；`%APPDATA%\QoderWork`
//!   无 auth 文件（未登录，与 `~/.qoderwork/.status.json logged_in=false` 一致）。
//!
//! macOS 实测依据（2026-09-23，macOS 15.6.1 / arm64，装了 `Qoder CN.app` 0.3.4）：
//! - `~/Library/Application Support/com.qodercn.app.stable` —— **叶子名与 Windows
//!   完全相同**，所以 `desktop_dir` 只靠 `dirs::config_dir()` 就够了，不需要按平台
//!   改文件布局。目录内含 `auth.v1.dat` / `auth.machine-id` / `Local State` /
//!   `channel-activation.v1.json`，与 Windows 同一组角色。
//! - 但 `Local State` 在这台 mac 上只有 57 字节、内容是 `{"uninstall_metrics":…}`，
//!   **没有 `os_crypt`** —— 主密钥不在文件里，在登录钥匙串（见 `auth_codec`）。
//! - `~/.qoder-cn/.auth/` 同样只有 `machine_id`（外加一个 0 字节
//!   `.credential-transaction`），**没有** `user` —— 与 Windows 一致地印证"CN CLI
//!   不落盘凭据、由桌面端注入"，所以切换单元绑桌面端这条判断在 mac 上同样成立。
//! - 可执行文件：`/Applications/Qoder CN.app/Contents/MacOS/Qoder CN`
//!   （CFBundleName 与镜像名一致，故 `desktop_images()` 无需按平台分叉）。
//!   Windows 那套 `%LOCALAPPDATA%\…\Launcher\state.ini` 在这里不存在。

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::modules::config::{ini_get, PathRoots};

/// 版本轴。`Cn` 为缺省，与用户实际在用的版本一致。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum QoderVariant {
    Cn,
    Global,
}

/// 目标轴：同机三个各自持有登录态的客户端形态。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum QoderTarget {
    /// Qoder 桌面客户端（Electron，`com.qoder*.app.stable`）——实测的凭据权威源。
    Desktop,
    /// Qoder CLI（`qodercli` / `qoderclicn`）。
    Cli,
    /// QoderWork 桌面客户端。
    Work,
}

/// 凭据文件的角色。快照与备份按角色而非路径比对，便于跨版本复用逻辑。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FileRole {
    /// 桌面 `auth.v1.dat` / Work `auth.dat`：magic `v10`。
    /// Windows 走 safeStorage→DPAPI+AES-256-GCM；macOS 走 safeStorage→钥匙串
    /// +AES-128-CBC（2026-09-23 本机实测，见 `auth_codec` 模块头）。
    AuthMain,
    /// Work `auth-v2.dat`。
    AuthV2,
    /// 按 account_id 存的 name/avatar（可选，本机目前不存在）。
    ProfileOverlays,
    /// 桌面 `auth.machine-id`（明文 UUID）。
    DesktopMachineId,
    /// `Local State`：**Windows 上**含 `os_crypt.encrypted_key`（DPAPI CURRENT_USER
    /// 保护的主密钥）。macOS 上实测该文件只有 `uninstall_metrics`，主密钥在钥匙串，
    /// 所以它在 mac 上只是观测对象。
    LocalState,
    /// `channel-activation.v1.json`：渠道激活信息，观测用。
    ChannelActivation,
    /// CLI `~/.qoder*/.auth/user`：WASM AES，密钥取同目录 `machine_id` 前 16 字符。
    CliUser,
    /// CLI `~/.qoder*/.auth/machine_id`：**它就是解密密钥**，必须与 `CliUser` 成组。
    CliMachineId,
    /// 明文登录回显（`.qoder-app-status.json` / `.status.json`）：账号标签的零解密来源。
    StatusEcho,
}

/// 一个需要纳入快照/备份/替换集合的文件。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CredentialFile {
    pub variant: QoderVariant,
    pub target: QoderTarget,
    pub role: FileRole,
    pub path: PathBuf,
    /// true = 只要它存在就必须与主凭据成组替换，否则会出现"半换号"。
    pub critical: bool,
}

impl CredentialFile {
    pub fn exists(&self) -> bool {
        self.path.is_file()
    }

    /// bundle 内的扁平文件名（角色可区分，避免同名互相覆盖）。
    pub fn stored_name(&self) -> String {
        self.role.stored_name()
    }
}

impl FileRole {
    /// bundle 内的扁平文件名。**决定包建成后 restore 能不能读到文件的那一个约定** ——
    /// 想按角色写包都必须经由它，不要自己另起名字（OAuth 曾因此建出读不到的包）。
    pub fn stored_name(self) -> String {
        format!("{self:?}").to_lowercase()
    }
}

impl QoderVariant {
    pub const ALL: [QoderVariant; 2] = [QoderVariant::Cn, QoderVariant::Global];

    pub fn label(self) -> &'static str {
        match self {
            Self::Cn => "Qoder 国内版",
            Self::Global => "Qoder 国际版",
        }
    }

    /// 明文回显里的 `product` 取值。
    pub fn product_id(self) -> &'static str {
        match self {
            Self::Cn => "qodercn",
            Self::Global => "qoder",
        }
    }

    pub fn work_product_id(self) -> &'static str {
        match self {
            Self::Cn => "qoderworkcn",
            Self::Global => "qoderwork",
        }
    }

    /// 桌面客户端的进程镜像名（不含 `.exe`）。`Qoder` 与 `Qoder CN` 是两个不同
    /// 镜像名，按名精确终止即可互不误伤。
    pub fn desktop_images(self) -> &'static [&'static str] {
        match self {
            Self::Cn => &["Qoder CN"],
            Self::Global => &["Qoder"],
        }
    }

    pub fn cli_images(self) -> &'static [&'static str] {
        match self {
            Self::Cn => &["qoderclicn"],
            Self::Global => &["qodercli"],
        }
    }

    pub fn work_images(self) -> &'static [&'static str] {
        match self {
            Self::Cn => &["QoderWork CN"],
            Self::Global => &["QoderWork"],
        }
    }

    /// 登录页基址。CN 侧 `qoder.com.cn`（头像 URL 实测主机）与 `qoder.cn`
    /// （asar 提取）并存，M3 接入 device flow 前需实测确认哪一个承载它。
    pub fn auth_base_url(self) -> &'static str {
        match self {
            Self::Cn => "https://qoder.com.cn",
            Self::Global => "https://qoder.com",
        }
    }

    pub fn openapi_base_url(self) -> &'static str {
        match self {
            Self::Cn => "https://openapi.qoder.com.cn",
            Self::Global => "https://openapi.qoder.sh",
        }
    }

    pub fn region_code(self) -> &'static str {
        match self {
            Self::Cn => "cn",
            Self::Global => "global",
        }
    }
}

impl QoderTarget {
    pub const ALL: [QoderTarget; 3] = [
        QoderTarget::Desktop,
        QoderTarget::Cli,
        QoderTarget::Work,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Self::Desktop => "桌面客户端",
            Self::Cli => "CLI",
            Self::Work => "QoderWork",
        }
    }

    /// 该目标需要终止的进程镜像名。
    pub fn images(self, variant: QoderVariant) -> &'static [&'static str] {
        match self {
            Self::Desktop => variant.desktop_images(),
            Self::Cli => variant.cli_images(),
            Self::Work => variant.work_images(),
        }
    }
}

fn cli_home_name(v: QoderVariant) -> &'static str {
    match v {
        QoderVariant::Cn => ".qoder-cn",
        QoderVariant::Global => ".qoder",
    }
}

fn work_cli_home_name(v: QoderVariant) -> &'static str {
    match v {
        QoderVariant::Cn => ".qoderworkcn",
        QoderVariant::Global => ".qoderwork",
    }
}

fn desktop_app_data_name(v: QoderVariant) -> &'static str {
    match v {
        QoderVariant::Cn => "com.qodercn.app.stable",
        QoderVariant::Global => "com.qoder.app.stable",
    }
}

fn work_app_data_name(v: QoderVariant) -> &'static str {
    match v {
        QoderVariant::Cn => "QoderWork CN",
        QoderVariant::Global => "QoderWork",
    }
}

/// `.app` 包的显示名（macOS 实测 `/Applications/Qoder CN.app`，CFBundleName 同名）。
fn mac_app_bundle_name(v: QoderVariant) -> &'static str {
    match v {
        QoderVariant::Cn => "Qoder CN",
        QoderVariant::Global => "Qoder",
    }
}

/// QoderWork 的 `.app` 包名。与 Windows 的 `work_app_data_name` 同源。
fn mac_work_bundle_name(v: QoderVariant) -> &'static str {
    match v {
        QoderVariant::Cn => "QoderWork CN",
        QoderVariant::Global => "QoderWork",
    }
}

/// Electron `safeStorage` 在 macOS 上把主密钥放在**登录钥匙串**，而不是文件里。
///
/// 本机实测（2026-09-23，`security dump-keychain`）两个条目都在：
/// - 国内版：`svce="Qoder CN App Safe Storage"` / `acct="Qoder CN App Key"`
/// - 国际版：`svce="Qoder Safe Storage"` / `acct="Qoder Key"`
///
/// 命名规律是 `"<基名> Safe Storage"` + `"<基名> Key"`，所以基名单点维护在这里，
/// 与 `desktop_app_data_name` 一样不许散落到调用方。
fn mac_safe_storage_base(v: QoderVariant) -> &'static str {
    match v {
        QoderVariant::Cn => "Qoder CN App",
        QoderVariant::Global => "Qoder",
    }
}

/// 钥匙串条目所属的"应用显示名"候选（按优先级尝试）。
///
/// 桌面端两个基名都已实测；QoderWork 在本机没有安装，无从取证，只能按同一命名
/// 规律给出候选 —— 取不到时调用方会退回"解不开就按明文回显展示"的既有降级路径。
pub fn mac_keychain_service_candidates(v: QoderVariant, t: QoderTarget) -> Vec<(String, String)> {
    let bases: &[&str] = match t {
        QoderTarget::Desktop => &[mac_safe_storage_base(v)],
        QoderTarget::Work => &[mac_work_bundle_name(v)],
        // CLI 在 macOS 上同样不落盘凭据（实测 ~/.qoder-cn/.auth 只有 machine_id），
        // 没有独立的桌面式 safeStorage 条目。
        QoderTarget::Cli => &[],
    };
    bases
        .iter()
        .map(|b| (format!("{b} Safe Storage"), format!("{b} Key")))
        .collect()
}

pub fn desktop_dir(roots: &PathRoots, v: QoderVariant) -> PathBuf {
    roots.roaming.join(desktop_app_data_name(v))
}

pub fn work_dir(roots: &PathRoots, v: QoderVariant) -> PathBuf {
    roots.roaming.join(work_app_data_name(v))
}

pub fn cli_dir(roots: &PathRoots, v: QoderVariant) -> PathBuf {
    roots.home.join(cli_home_name(v))
}

pub fn work_cli_dir(roots: &PathRoots, v: QoderVariant) -> PathBuf {
    roots.home.join(work_cli_home_name(v))
}

/// Launcher 的 `state.ini` —— **Windows 上**查 exe 路径的权威来源（版本目录随升级而变）。
///
/// macOS 没有这层 Launcher 目录（`%LOCALAPPDATA%\Qoder CN\Qoder CN Launcher\state.ini`
/// 是 Windows 特有布局），那边改走 `mac_app_exe`，所以本函数在 macOS 上返回一个
/// 注定不存在的路径，`launcher_exe` 自然得到 None。
pub fn launcher_state_ini(roots: &PathRoots, v: QoderVariant) -> PathBuf {
    let (root, leaf) = match v {
        QoderVariant::Cn => ("Qoder CN", "Qoder CN Launcher"),
        QoderVariant::Global => ("Qoder", "Qoder Launcher"),
    };
    roots.local.join(root).join(leaf).join("state.ini")
}

/// 从 `state.ini` 解析 `installDir` + `appExecutable` 得到 exe 绝对路径。
///
/// 两个值都可能被手改：`appExecutable` 若是绝对路径或根相对（`\x.exe`），
/// `join` 会整段丢掉 `installDir` 或换掉叶子，最终从任意位置拉起可执行文件。
/// 解析完必须校验它仍在 `installDir` 之下，越界就当没解析出来（restart 降级为
/// "请手动打开"，与 executable() 返回 None 的既有行为一致）。
pub fn launcher_exe(roots: &PathRoots, v: QoderVariant) -> Option<PathBuf> {
    let text = std::fs::read_to_string(launcher_state_ini(roots, v)).ok()?;
    let install_dir_raw = ini_get(&text, "installDir")?;
    let exe_rel_raw = ini_get(&text, "appExecutable")?;
    let install_dir = install_dir_raw.trim_matches('"');
    let exe_rel = exe_rel_raw.trim_matches('"');
    let exe = PathBuf::from(install_dir).join(exe_rel);
    // installDir 与解析出的 exe 都必须规范化到同一根下才可比。
    let exe_canon = exe.canonicalize().ok()?;
    let dir_canon = PathBuf::from(install_dir).canonicalize().ok()?;
    // Windows 与 macOS（APFS 默认大小写不敏感）都按大小写不敏感比较，且可能带
    // \\?\ 前缀，规范化后统一小写再比前缀。
    let exe_str = exe_canon.to_string_lossy().to_ascii_lowercase();
    let mut dir_str = dir_canon.to_string_lossy().to_ascii_lowercase();
    // 分隔符必须按宿主平台取：原先无条件 push('\\') 让 POSIX 路径永远匹配不上，
    // 于是 macOS 上这个函数恒为 None，自动重启被静默降级成"请手动打开"。
    if !dir_str.ends_with(std::path::MAIN_SEPARATOR_STR) {
        dir_str.push_str(std::path::MAIN_SEPARATOR_STR);
    }
    if !exe_str.starts_with(&dir_str) && exe_canon != dir_canon {
        return None;
    }
    exe_canon.is_file().then_some(exe_canon)
}

/// macOS：`.app` 包内的主可执行文件。
///
/// 实测 `/Applications/Qoder CN.app/Contents/MacOS/Qoder CN`（CFBundleExecutable
/// 与 CFBundleName 同名）。
///
/// 检索顺序是**用户级在前、系统级在后**，这不只是偏好：`roots.home` 参与第一顺位，
/// 沙箱测试才能在临时目录里造出一个完整的 `.app` 并断言解析结果，而不被开发机上
/// 真实安装的 `/Applications` 抢先命中 —— 那条"测试永远不碰真实产品目录"的不变量
/// 在这里同样要成立。
pub fn mac_app_exe(roots: &PathRoots, bundle_name: &str) -> Option<PathBuf> {
    mac_app_bundle(roots, bundle_name)
        .map(|b| b.join("Contents/MacOS").join(bundle_name))
        .filter(|p| p.is_file())
}

/// macOS：`open` 用的 app 包路径（比直接 exec Mach-O 更贴近用户点图标的行为，
/// 会走 LaunchServices，拿到正确的激活/菜单栏语义）。
///
/// 包名来自本模块的常量表，不接受外部输入，所以这里没有 `state.ini` 那条越界面。
pub fn mac_app_bundle(roots: &PathRoots, bundle_name: &str) -> Option<PathBuf> {
    [
        roots.home.join("Applications").join(format!("{bundle_name}.app")),
        PathBuf::from("/Applications").join(format!("{bundle_name}.app")),
    ]
    .into_iter()
    .find(|p| p.is_dir())
}

/// 该目标的可执行文件。
///
/// Windows 上桌面/Work 走 Launcher 的 `state.ini`；macOS 上没有 Launcher，直接找
/// `.app` 包内的主二进制。CLI 叶子名在 macOS 上没有 `.exe` 后缀。
pub fn executable(roots: &PathRoots, v: QoderVariant, t: QoderTarget) -> Option<PathBuf> {
    match t {
        QoderTarget::Desktop | QoderTarget::Work => {
            if cfg!(target_os = "macos") {
                let bundle = match t {
                    QoderTarget::Work => mac_work_bundle_name(v),
                    _ => mac_app_bundle_name(v),
                };
                mac_app_exe(roots, bundle)
            } else {
                launcher_exe(roots, v)
            }
        }
        QoderTarget::Cli => {
            let (dir, leaf) = match v {
                QoderVariant::Cn => ("qoderclicn", "qoderclicn"),
                QoderVariant::Global => ("qodercli", "qodercli"),
            };
            // Windows 的可执行文件带 .exe，macOS/Linux 不带。
            let leaf = if cfg!(windows) {
                format!("{leaf}.exe")
            } else {
                leaf.to_string()
            };
            let bin = cli_dir(roots, v).join("bin").join(dir).join(leaf);
            bin.is_file().then_some(bin)
        }
    }
}

/// macOS：`.app` 显示名，供 `open -a` 与界面展示用。
pub fn mac_bundle_name(v: QoderVariant, t: QoderTarget) -> &'static str {
    match t {
        QoderTarget::Work => mac_work_bundle_name(v),
        _ => mac_app_bundle_name(v),
    }
}

/// `(版本, 目标) → 该目标的全部凭据文件`。纯函数，不做 IO（`Work` 分支除外：
/// 它要先确认 QoderWork 的 CLI 目录存在，才决定是否纳入其回显文件）。
pub fn credentials(roots: &PathRoots, variant: QoderVariant, target: QoderTarget) -> Vec<CredentialFile> {
    let mk = |role: FileRole, path: PathBuf, critical: bool| CredentialFile {
        variant,
        target,
        role,
        path,
        critical,
    };
    match target {
        QoderTarget::Desktop => {
            let root = desktop_dir(roots, variant);
            vec![
                mk(FileRole::AuthMain, root.join("auth.v1.dat"), true),
                // macOS 上 Local State 不含主密钥（实测只有 uninstall_metrics），
                // 把它标成 critical 只会让"半换号"闸门凭白拒写。
                mk(
                    FileRole::LocalState,
                    root.join("Local State"),
                    !cfg!(target_os = "macos"),
                ),
                mk(
                    FileRole::ProfileOverlays,
                    root.join("auth-profile-overlays.v1.dat"),
                    true,
                ),
                mk(FileRole::DesktopMachineId, root.join("auth.machine-id"), false),
                mk(
                    FileRole::ChannelActivation,
                    root.join("channel-activation.v1.json"),
                    false,
                ),
                // 桌面端的明文回显。它虽然落在 CLI 家目录下，但 `version` 字段是
                // 桌面端版本（CN 0.3.4 / Global 0.3.3，而 CLI 是 1.1.5）且
                // `writer` 为 "main" —— 作者是桌面主进程，故归属桌面目标。
                mk(
                    FileRole::StatusEcho,
                    cli_dir(roots, variant).join(".qoder-app-status.json"),
                    false,
                ),
            ]
        }
        QoderTarget::Cli => {
            let auth = cli_dir(roots, variant).join(".auth");
            vec![
                mk(FileRole::CliUser, auth.join("user"), true),
                mk(FileRole::CliMachineId, auth.join("machine_id"), true),
            ]
        }
        QoderTarget::Work => {
            let root = work_dir(roots, variant);
            let mut files = vec![
                mk(FileRole::AuthMain, root.join("auth.dat"), true),
                mk(FileRole::AuthV2, root.join("auth-v2.dat"), true),
                mk(FileRole::LocalState, root.join("Local State"), true),
            ];
            let work_cli = work_cli_dir(roots, variant);
            if work_cli.is_dir() {
                files.push(mk(
                    FileRole::CliMachineId,
                    work_cli.join(".auth").join("machine_id"),
                    true,
                ));
                files.push(mk(FileRole::StatusEcho, work_cli.join(".status.json"), false));
            }
            files
        }
    }
}

/// 切换单元的备份文件名（版本×目标×时间可区分，避免互相覆盖）。
pub fn backup_stem(variant: QoderVariant, target: QoderTarget, ts: &str) -> String {
    format!("{}.{ts}", bundle_prefix(variant, target))
}

/// 账号 bundle 目录名的前缀部分。
pub fn bundle_prefix(variant: QoderVariant, target: QoderTarget) -> String {
    let v = match variant {
        QoderVariant::Cn => "cn",
        QoderVariant::Global => "global",
    };
    let t = match target {
        QoderTarget::Desktop => "desktop",
        QoderTarget::Cli => "cli",
        QoderTarget::Work => "work",
    };
    format!("{v}.{t}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paths_are_absolute_and_roles_unique_per_target() {
        let roots = PathRoots::real();
        for v in QoderVariant::ALL {
            for t in QoderTarget::ALL {
                let files = credentials(&roots, v, t);
                assert!(!files.is_empty(), "{v:?} {t:?} 布局为空");
                for f in &files {
                    assert!(f.path.is_absolute(), "{:?} 不是绝对路径", f.path);
                    assert_eq!((f.variant, f.target), (v, t));
                }
                let mut sorted: Vec<_> = files.iter().map(|f| f.role).collect();
                sorted.sort();
                let n = sorted.len();
                sorted.dedup();
                assert_eq!(sorted.len(), n, "{v:?} {t:?} 存在重复角色");
            }
        }
    }

    #[test]
    fn cn_and_global_do_not_collide() {
        let roots = PathRoots::real();
        for t in QoderTarget::ALL {
            let a: Vec<_> = credentials(&roots, QoderVariant::Cn, t)
                .into_iter()
                .map(|f| f.path)
                .collect();
            let b: Vec<_> = credentials(&roots, QoderVariant::Global, t)
                .into_iter()
                .map(|f| f.path)
                .collect();
            for p in &a {
                assert!(!b.contains(p), "{p:?} 两版本共用，换号会互相踩");
            }
        }
    }

    /// 本机证据测试：这些文件在开发机上必须真实存在，否则说明布局写错了。
    ///
    /// 分两档，因为现在有两台取证机（Windows 11 与 macOS 15.6），装的东西不一样：
    /// - **硬断言**：CN 桌面那一组 —— 两平台都实测存在，缺一个就是布局写错。
    /// - **软提示**：国际版桌面/CLI 与 QoderWork —— 取决于本机装没装、登没登录，
    ///   目录不存在时只提示。对"没装的东西"断言存在，只会让另一台开发机上必然红。
    ///
    /// macOS 额外硬断言登录钥匙串里的 safeStorage 条目 —— 那边主密钥不在文件里，
    /// 只断言文件存在会漏掉真正承载解密的这一环。
    #[test]
    #[ignore = "绑定开发机的真实 Qoder 安装布局"]
    fn local_evidence_files_exist() {
        let roots = PathRoots::real();
        for role in [FileRole::AuthMain, FileRole::LocalState, FileRole::DesktopMachineId] {
            let f = credentials(&roots, QoderVariant::Cn, QoderTarget::Desktop)
                .into_iter()
                .find(|c| c.role == role)
                .unwrap_or_else(|| panic!("CN 桌面布局缺少 {role:?}"));
            assert!(f.exists(), "本机应存在 CN 桌面 {role:?} -> {:?}", f.path);
        }

        #[cfg(target_os = "macos")]
        {
            let (svc, acct) = crate::modules::variant::mac_keychain_service_candidates(
                QoderVariant::Cn,
                QoderTarget::Desktop,
            )
            .remove(0);
            let out = std::process::Command::new("/usr/bin/security")
                .args(["find-generic-password", "-s", &svc, "-a", &acct])
                .output()
                .expect("macOS 上 /usr/bin/security 必须可用");
            assert!(
                out.status.success(),
                "登录钥匙串里找不到 CN 桌面端的 safeStorage 条目 {svc}/{acct}：{}",
                String::from_utf8_lossy(&out.stderr).trim()
            );
            // 只断言条目存在，绝不打印口令。
        }

        let soft = [
            (QoderVariant::Global, QoderTarget::Desktop, FileRole::AuthMain),
            (QoderVariant::Global, QoderTarget::Cli, FileRole::CliUser),
            (QoderVariant::Global, QoderTarget::Cli, FileRole::CliMachineId),
            (QoderVariant::Cn, QoderTarget::Cli, FileRole::CliMachineId),
            (QoderVariant::Cn, QoderTarget::Work, FileRole::AuthV2),
        ];
        let mut missing = Vec::new();
        for (v, t, role) in soft {
            let f = credentials(&roots, v, t)
                .into_iter()
                .find(|c| c.role == role)
                .unwrap_or_else(|| panic!("{v:?} {t:?} 缺少 {role:?}"));
            if !f.exists() {
                missing.push(format!("{v:?} {t:?} {role:?} -> {:?}", f.path));
            }
        }
        if !missing.is_empty() {
            eprintln!("NOTE: 本机未装/未登录，以下证据项缺席（不算失败）:\n  {}", missing.join("\n  "));
        }
    }

    /// 反面证据：CN CLI 至今不落 `user`，这是"CLI 先行"必须改绑桌面的根因。
    /// 若官方将来改持久化策略，本测试转为提示而非失败。
    #[test]
    fn cn_cli_user_file_is_absent_on_this_machine() {
        let roots = PathRoots::real();
        let f = credentials(&roots, QoderVariant::Cn, QoderTarget::Cli)
            .into_iter()
            .find(|c| c.role == FileRole::CliUser)
            .expect("CN CLI 应有 user 布局项");
        if f.exists() {
            eprintln!("NOTE: CN CLI 现在会落盘 {:?}", f.path);
        }
    }

    #[test]
    fn launcher_exe_resolves_from_state_ini() {
        let roots = PathRoots::real();
        let v = QoderVariant::Cn;
        if !launcher_state_ini(&roots, v).is_file() {
            return;
        }
        let exe = executable(&roots, v, QoderTarget::Desktop)
            .expect("state.ini 应解析出存在的 exe");
        assert!(exe.is_file(), "{exe:?}");
        assert!(exe.to_string_lossy().contains("Qoder CN.exe"));
    }

    /// state.ini 可被手改：`appExecutable` 写成绝对路径或 `..\` 根相对路径时，
    /// `join` 会整段丢掉/替换 installDir，最终从任意位置拉起 exe。必须校验解析结果
    /// 仍在 installDir 之下，越界按"解析不出"降级（restart 变为提示手动打开）。
    ///
    /// 这个注入面是 `state.ini` 特有的，macOS 上没有 Launcher（`mac_app_exe` 只从
    /// 两个固定根拼出 `<Name>.app/Contents/MacOS/<Name>`，名字来自本模块常量表，
    /// 不接受任何外部输入），所以本测试只在 Windows 侧成立。
    #[test]
    #[cfg(windows)]
    fn launcher_exe_rejects_paths_escaping_install_dir() {
        let tmp = std::env::temp_dir().join(format!("qs-launcher-{}", uuid::Uuid::new_v4().simple()));
        let roots = PathRoots::sandbox(&tmp);
        let v = QoderVariant::Cn;
        let install = tmp.join("install");
        std::fs::create_dir_all(&install).unwrap();
        std::fs::create_dir_all(launcher_state_ini(&roots, v).parent().unwrap()).unwrap();
        // installDir 内一个真实存在的 exe。
        std::fs::write(install.join("ok.exe"), b"MZ").unwrap();
        // installDir 外一个真实存在的 exe（越界目标）。
        std::fs::write(tmp.join("evil.exe"), b"MZ").unwrap();

        let write_ini = |app_exec: &str| {
            std::fs::write(
                launcher_state_ini(&roots, v),
                format!("installDir={install}\nappExecutable={app_exec}\n", install = install.display()),
            )
            .unwrap();
        };

        // 正常的相对路径：解析得到、且规范路径就是 install 内那个。
        write_ini("ok.exe");
        let exe = executable(&roots, v, QoderTarget::Desktop).expect("合法相对路径应解析出来");
        assert!(exe.ends_with("ok.exe"), "{exe:?}");

        // 绝对路径越界。
        write_ini(&tmp.join("evil.exe").display().to_string());
        assert_eq!(
            executable(&roots, v, QoderTarget::Desktop),
            None,
            "绝对 appExecutable 指向 installDir 之外，必须拒绝"
        );

        // 根相对 / `..` 逃逸。
        write_ini("..\\evil.exe");
        assert_eq!(
            executable(&roots, v, QoderTarget::Desktop),
            None,
            "相对但越界的 appExecutable 必须拒绝"
        );
        std::fs::remove_dir_all(tmp).ok();
    }

    /// macOS 侧的等价保证：`.app` 解析只认固定形状，且**用户级安装优先**，
    /// 这样沙箱测试能自证解析结果而不是被真机的 /Applications 抢先命中。
    #[test]
    #[cfg(target_os = "macos")]
    fn mac_app_exe_resolves_from_sandbox_and_only_from_the_fixed_shape() {
        let tmp = std::env::temp_dir().join(format!("qs-mac-app-{}", uuid::Uuid::new_v4().simple()));
        let roots = PathRoots::sandbox(&tmp);
        let exe = roots
            .home
            .join("Applications/Qoder CN.app/Contents/MacOS/Qoder CN");
        std::fs::create_dir_all(exe.parent().unwrap()).unwrap();
        std::fs::write(&exe, b"MACHO").unwrap();
        assert_eq!(
            executable(&roots, QoderVariant::Cn, QoderTarget::Desktop).as_deref(),
            Some(exe.as_path()),
            "用户级 .app 必须优先于系统级被解析出来"
        );

        // 只建了包、没建里面的二进制 → 视为没装（不能返回一个不存在的路径去 launch）。
        let other = roots.home.join("Applications/Other.app");
        std::fs::create_dir_all(other.join("Contents/MacOS")).unwrap();
        assert_eq!(mac_app_exe(&roots, "Other"), None);

        // 完全没有 .app 的版本 → None（真机上国际版未安装时就是这个分支）。
        std::fs::remove_dir_all(&tmp).ok();
    }

    #[test]
    fn ini_parsing_is_tolerant_of_spaces_and_crlf() {
        let ini = "[launcher]\r\ninstallDir=E:\\Qoder CN\r\n appExecutable = .qoder-versions\\0.3.4\\Qoder CN.exe \r\n";
        assert_eq!(ini_get(ini, "installDir").as_deref(), Some("E:\\Qoder CN"));
        assert_eq!(
            ini_get(ini, "appExecutable").as_deref(),
            Some(".qoder-versions\\0.3.4\\Qoder CN.exe")
        );
        assert_eq!(ini_get(ini, "missing"), None);
    }

    #[test]
    fn backup_stem_is_distinct_per_axis() {
        let ts = "20260920T040000Z";
        let mut seen = std::collections::HashSet::new();
        for v in QoderVariant::ALL {
            for t in QoderTarget::ALL {
                assert!(seen.insert(backup_stem(v, t, ts)));
            }
        }
        assert_eq!(seen.len(), 6);
    }
}

/// 角色是否属于"换号必须成组替换"集合。
///
/// 真相仍然只在 `credentials()` 的表里：这里取该角色在所有 (版本·目标) 下出现过的
/// critical 标记的并集，避免调用方各自复制一份判断。并集与机器无关（critical 是
/// 每个 (role,target) 的静态属性，credentials 的分支也不看文件是否存在），故
/// 进程内算一次缓存起来 —— import 对每个成员都会调它，别每次重走一遍全轴。
pub fn role_is_critical(role: FileRole) -> bool {
    static SET: std::sync::OnceLock<std::collections::HashSet<FileRole>> =
        std::sync::OnceLock::new();
    SET.get_or_init(|| {
        let roots = PathRoots::real();
        all_axes()
            .into_iter()
            .flat_map(|(v, t)| credentials(&roots, v, t))
            .filter(|f| f.critical)
            .map(|f| f.role)
            .collect()
    })
    .contains(&role)
}

/// 供上层按 `(版本,目标)` 组合遍历。
pub fn all_axes() -> Vec<(QoderVariant, QoderTarget)> {
    let mut out = Vec::new();
    for v in QoderVariant::ALL {
        for t in QoderTarget::ALL {
            out.push((v, t));
        }
    }
    out
}
