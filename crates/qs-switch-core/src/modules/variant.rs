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
    /// 桌面 `auth.v1.dat` / Work `auth.dat`：magic `v10`，safeStorage→DPAPI+AES-256-GCM。
    AuthMain,
    /// Work `auth-v2.dat`。
    AuthV2,
    /// 按 account_id 存的 name/avatar（可选，本机目前不存在）。
    ProfileOverlays,
    /// 桌面 `auth.machine-id`（明文 UUID）。
    DesktopMachineId,
    /// `Local State`：含 `os_crypt.encrypted_key`（DPAPI CURRENT_USER 保护的主密钥）。
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
        format!("{:?}", self.role).to_lowercase()
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

/// Launcher 的 `state.ini` —— 查 exe 路径的权威来源（版本目录随升级而变）。
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
    // Windows 上路径大小写不敏感且可能带 \\?\ 前缀，规范化后按标准全小写前缀匹配
    let exe_str = exe_canon.to_string_lossy().to_ascii_lowercase();
    let mut dir_str = dir_canon.to_string_lossy().to_ascii_lowercase();
    if !dir_str.ends_with('\\') && !dir_str.ends_with('/') {
        dir_str.push('\\');
    }
    if !exe_str.starts_with(&dir_str) && exe_canon != dir_canon {
        return None;
    }
    exe_canon.is_file().then_some(exe_canon)
}

/// 该目标的可执行文件；桌面与 Work 目前共用 Launcher 解析路径。
pub fn executable(roots: &PathRoots, v: QoderVariant, t: QoderTarget) -> Option<PathBuf> {
    match t {
        QoderTarget::Desktop | QoderTarget::Work => launcher_exe(roots, v),
        QoderTarget::Cli => {
            let leaf = match v {
                QoderVariant::Cn => "qoderclicn/qoderclicn.exe",
                QoderVariant::Global => "qodercli/qodercli.exe",
            };
            let bin = cli_dir(roots, v).join("bin").join(leaf);
            bin.is_file().then_some(bin)
        }
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
                mk(FileRole::LocalState, root.join("Local State"), true),
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
    /// 绑定真实 Qoder 安装，他人机器/CI 上必然失败——默认忽略，
    /// 开发机用 `cargo test -- --ignored` 或 `scripts/build.sh test` 补跑。
    #[test]
    #[ignore = "绑定开发机的真实 Qoder 安装布局"]
    fn local_evidence_files_exist() {
        let roots = PathRoots::real();
        let expected = [
            (QoderVariant::Cn, QoderTarget::Desktop, FileRole::AuthMain),
            (QoderVariant::Cn, QoderTarget::Desktop, FileRole::LocalState),
            (QoderVariant::Global, QoderTarget::Desktop, FileRole::AuthMain),
            (QoderVariant::Global, QoderTarget::Cli, FileRole::CliUser),
            (QoderVariant::Global, QoderTarget::Cli, FileRole::CliMachineId),
            (QoderVariant::Cn, QoderTarget::Cli, FileRole::CliMachineId),
            (QoderVariant::Cn, QoderTarget::Work, FileRole::AuthV2),
        ];
        for (v, t, role) in expected {
            let f = credentials(&roots, v, t)
                .into_iter()
                .find(|c| c.role == role)
                .unwrap_or_else(|| panic!("{v:?} {t:?} 缺少 {role:?}"));
            assert!(f.exists(), "本机应存在 {v:?} {t:?} {role:?} -> {:?}", f.path);
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
    #[test]
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
