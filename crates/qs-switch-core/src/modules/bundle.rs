//! 凭据包（bundle）：一个账号在某个目标上的**文件级副本**，外加明文回显里读出的标签。
//!
//! 这是与参考实现最关键的差异。workbuddy-switch 的账号记录存的是 token 明文
//! （`~/.wb-switch/accounts.json` 里 `access_token` / `refresh_token`），因为它的
//! 目标接受 token 注入；而 Qoder 的三处登录态分别是 safeStorage(DPAPI+AES-256-GCM)
//! 与 WASM AES 密文文件。存 token 就得先复刻那两套加密，存**整组文件副本**则完全
//! 不需要知道密码学细节 —— 因此本模块只做字节搬移，加密逆向留到"从 token 造文件"
//! 这条可选支线上。
//!
//! 账号标签取自 `StatusEcho`（明文，含 name/email/plan/avatar），同样零解密。

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::modules::config::{
    atomic_write_bytes, now_ts, read_bytes, sha256_hex, PathRoots,
};
use crate::modules::variant::{bundle_prefix, credentials, FileRole, QoderTarget, QoderVariant};
use crate::Result;

/// 包内一个文件成员。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Member {
    pub role: FileRole,
    /// bundle 目录内的扁平文件名。
    pub file_name: String,
    pub sha256: String,
    pub size: u64,
    /// 捕获时它在真实路径上是否 critical（决定 restore 是否强制要求存在）。
    pub critical: bool,
}

/// 从明文回显或解密后的登录态读出的账号身份，用于列表展示；不含任何 token。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Identity {
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub email: Option<String>,
    #[serde(default)]
    pub plan: Option<String>,
    #[serde(default)]
    pub product: Option<String>,
    #[serde(default)]
    pub logged_in: Option<bool>,
    #[serde(default)]
    pub snapshot_at: Option<String>,
    /// 以下三项只有成功解密 `auth.v1.dat` 时才有 —— 明文回显里没有 uid 与到期时间。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub uid: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub refresh_expires_at: Option<String>,
    /// 独立代理配置（例如 http://127.0.0.1:7890 或 socks5://127.0.0.1:1080）
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub proxy: Option<String>,
}

/// RFC3339（`2026-10-19T06:19:41Z`）→ 毫秒。前端契约里的时间戳是毫秒数。
pub fn iso_to_ms(iso: &str) -> Option<u64> {
    chrono::DateTime::parse_from_rfc3339(iso)
        .ok()
        .map(|t| t.timestamp_millis().max(0) as u64)
}

/// 本工具自己的紧凑时间戳（`%Y%m%dT%H%M%SZ`）→ 毫秒。与 `iso_to_ms` 不互认。
pub fn compact_to_ms(s: &str) -> Option<u64> {
    chrono::NaiveDateTime::parse_from_str(s, "%Y%m%dT%H%M%SZ")
        .ok()
        .map(|t| t.and_utc().timestamp_millis().max(0) as u64)
}

/// ISO8601（`2026-10-19T06:19:41Z`）到"还剩几天"。解析不了返回 None，不猜。
pub fn days_until(iso: &str) -> Option<i64> {
    let then = chrono::DateTime::parse_from_rfc3339(iso)
        .map(|t| t.with_timezone(&chrono::Utc))
        .or_else(|_| {
            chrono::NaiveDateTime::parse_from_str(iso, "%Y-%m-%dT%H:%M:%SZ")
                .map(|t| t.and_utc())
        })
        .ok()?;
    Some((then - chrono::Utc::now()).num_days())
}

impl Identity {
    /// 用解密出来的登录态补齐身份。已有字段不覆盖成空串。
    pub fn merge_auth(&mut self, a: &crate::modules::auth_codec::DesktopAuth) {
        self.uid = Some(a.user.id.clone());
        self.expires_at = Some(a.expires_at.clone());
        self.refresh_expires_at = Some(a.refresh_expires_at.clone());
        if !a.user.name.trim().is_empty() {
            self.name = Some(a.user.name.clone());
        }
        if !a.user.email.trim().is_empty() {
            self.email = Some(a.user.email.clone());
        }
        self.logged_in = Some(true);
    }

    /// token 剩余天数（无解密结果时 None）。
    pub fn token_days_left(&self) -> Option<i64> {
        self.expires_at.as_deref().and_then(days_until)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Bundle {
    pub account_id: String,
    pub variant: QoderVariant,
    pub target: QoderTarget,
    pub created_at: String,
    pub members: Vec<Member>,
    pub identity: Identity,
}

impl Bundle {
    /// bundle 在 store 下的实际目录。store 由调用方显式给出，避免测试依赖
    /// 进程级环境变量而在并行时互相踩。
    pub fn dir_in(&self, store: &Path) -> PathBuf {
        bundle_dir_in(store, &self.account_id, self.variant, self.target)
    }

    /// 该包是否真的有料（CLI 目标在 CN 版上可能一个凭据文件都没有）。
    pub fn is_empty(&self) -> bool {
        self.members.is_empty()
    }

    pub fn member(&self, role: FileRole) -> Option<&Member> {
        self.members.iter().find(|m| m.role == role)
    }

    pub fn display_label(&self) -> String {
        if let Some(e) = &self.identity.email {
            return e.clone();
        }
        if let Some(n) = &self.identity.name {
            return n.clone();
        }
        format!("{}·{}", self.variant.label(), self.target.label())
    }
}

pub fn accounts_root_in(store: &Path) -> PathBuf {
    store.join("accounts")
}

/// 账号名的唯一合法形态。它是 store 路径的组成部分，而 delete 端点会对解析出的
/// 目录执行 `remove_dir_all` —— 含路径分隔符、`..`、盘符的 id 会把删除/写入带出
/// `store/accounts/`。在 capture / load / import / delete 的入口统一收口。
pub fn validate_account_id(account_id: &str) -> Result<()> {
    let ok = !account_id.is_empty()
        && account_id.len() <= 64
        && !account_id.starts_with('.')
        && !account_id.ends_with('.')
        && account_id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'));
    if !ok {
        return Err(format!(
            "账号名 {account_id:?} 不合法：只允许字母/数字/点/下划线/连字符（1–64 位，不以点开头或结尾）"
        ));
    }

    // Windows 保留设备名称检测（避免在 Windows 文件系统上创建无法访问/删除的保留字目录）
    let stem = account_id.split('.').next().unwrap_or(account_id);
    let upper = stem.to_ascii_uppercase();
    const RESERVED_NAMES: &[&str] = &[
        "CON", "PRN", "AUX", "NUL", "COM1", "COM2", "COM3", "COM4", "COM5", "COM6", "COM7",
        "COM8", "COM9", "LPT1", "LPT2", "LPT3", "LPT4", "LPT5", "LPT6", "LPT7", "LPT8", "LPT9",
    ];
    if RESERVED_NAMES.contains(&upper.as_str()) {
        return Err(format!(
            "账号名 {account_id:?} 不合法：不能使用 Windows 系统保留设备名（如 CON, NUL, AUX 等）"
        ));
    }

    Ok(())
}

pub fn bundle_dir_in(
    store: &Path,
    account_id: &str,
    variant: QoderVariant,
    target: QoderTarget,
) -> PathBuf {
    accounts_root_in(store)
        .join(account_id)
        .join(bundle_prefix(variant, target))
}

/// 把某个 (版本,目标) 当前真实存在的凭据文件收进 bundle。
/// 只读产品目录，写只发生在 `store/accounts/` 下。
pub fn capture(
    roots: &PathRoots,
    store: &Path,
    account_id: &str,
    variant: QoderVariant,
    target: QoderTarget,
) -> Result<Bundle> {
    validate_account_id(account_id)?;
    let files = credentials(roots, variant, target);
    let dir = bundle_dir_in(store, account_id, variant, target);
    std::fs::create_dir_all(&dir)
        .map_err(|e| format!("创建 {} 失败: {e}", dir.display()))?;

    let mut members = Vec::new();
    let mut identity = Identity::default();
    for f in &files {
        if f.role == FileRole::StatusEcho {
            identity = read_identity(&f.path).unwrap_or_default();
        }
        if !f.exists() {
            continue;
        }
        let bytes = read_bytes(&f.path).map_err(|e| {
            format!("读取 {:?} 失败（进程占用或权限不足）: {e}", f.path)
        })?;
        // 目标进程可能正在重写该文件（Qoder 会话期持续重写 auth，见 restore 的注释）。
        // 读两次、哈希一致才收：单次读盘可能拿到撕裂的半写文件，而它的摘要会被记成
        // 正台账 —— 此后包内校验、写回校验、导出校验全按这份坏摘要比对，一路绿灯。
        let bytes2 = read_bytes(&f.path).map_err(|e| {
            format!("复核读取 {:?} 失败: {e}", f.path)
        })?;
        if sha256_of(&bytes) != sha256_of(&bytes2) {
            return Err(format!(
                "{:?} 正在被写入（两次读取内容不一致），请先关闭目标客户端再认领",
                f.path
            ));
        }
        let file_name = f.stored_name();
        atomic_write_bytes(&dir.join(&file_name), &bytes)
            .map_err(|e| format!("写入包内文件 {file_name} 失败: {e}"))?;
        members.push(Member {
            role: f.role,
            file_name,
            // 用刚读到的字节算，避免二次读盘时撞上进程占用而写入空摘要。
            sha256: sha256_of(&bytes),
            size: bytes.len() as u64,
            critical: f.critical,
        });
    }

    // 桌面目标还能解出 uid 与到期时间。解不开（沙箱假文件、跨 Windows 用户、
    // DPAPI 不可用）就退回明文回显 —— 认领本身绝不该因此失败。
    if target == QoderTarget::Desktop {
        if let Ok(auth) = crate::modules::auth_codec::read_desktop_auth(roots, variant) {
            identity.merge_auth(&auth);
        }
    }

    let bundle = Bundle {
        account_id: account_id.to_string(),
        variant,
        target,
        created_at: now_ts(),
        members,
        identity,
    };
    write_meta(store, &bundle)?;
    Ok(bundle)
}

pub(crate) fn write_meta(store: &Path, bundle: &Bundle) -> Result<()> {
    let path = bundle.dir_in(store).join("bundle.json");
    let json = serde_json::to_vec_pretty(bundle).map_err(|e| e.to_string())?;
    atomic_write_bytes(&path, &json).map_err(|e| format!("写 bundle.json 失败: {e}"))
}

/// 读回一个已存在的 bundle；目录或元数据缺失返回 Err。
pub fn load(
    store: &Path,
    account_id: &str,
    variant: QoderVariant,
    target: QoderTarget,
) -> Result<Bundle> {
    validate_account_id(account_id)?;
    let path = bundle_dir_in(store, account_id, variant, target).join("bundle.json");
    let bytes = read_bytes(&path).map_err(|e| format!("读 {:?} 失败: {e}", path))?;
    serde_json::from_slice(&bytes).map_err(|e| format!("{path:?} 解析失败: {e}"))
}

/// 更新指定账号的代理配置并持久化到 bundle.json。
pub fn set_proxy(
    store: &Path,
    account_id: &str,
    variant: QoderVariant,
    target: QoderTarget,
    proxy: Option<String>,
) -> Result<Bundle> {
    let mut b = load(store, account_id, variant, target)?;
    let clean = proxy.map(|p| p.trim().to_string()).filter(|p| !p.is_empty());
    b.identity.proxy = clean;
    write_meta(store, &b)?;
    Ok(b)
}

/// 把 bundle 写回真实路径。调用方负责在此之前终止目标进程 —— 本模块不杀进程，
/// 因为发起方可能正是目标进程的子进程（见 `process::self_is_descendant_of_target`）。
///
/// 流程：整组先备份 → 逐个原子写 → 全部读回比对 sha256 → 任一不符则整组回滚。
/// 参考实现只备份不回滚，这里补上回滚，因为 Qoder 桌面端会持续重写 auth 文件。
pub fn restore(
    roots: &PathRoots,
    store: &Path,
    bundle: &Bundle,
    backup_dir: &Path,
) -> Result<RestoreOutcome> {
    let files = credentials(roots, bundle.variant, bundle.target);

    // 覆盖性检查：现场存在、但包里缺位的 critical 文件会造成"半换号"（例如只换了
    // auth.v1.dat 却没换 Local State），这种包宁可不写。
    let uncovered: Vec<String> = files
        .iter()
        .filter(|f| f.critical && f.exists() && bundle.member(f.role).is_none())
        .map(|f| format!("{:?}({})", f.role, f.path.display()))
        .collect();
    if !uncovered.is_empty() {
        return Err(format!(
            "bundle {} 缺少现场存在的 critical 文件，拒绝写入以避免半换号: {}",
            bundle.account_id,
            uncovered.join(", ")
        ));
    }

    let dir = bundle.dir_in(store);
    let mut staging: Vec<(CredentialTarget, Vec<u8>)> = Vec::new();
    for m in &bundle.members {
        let f = files
            .iter()
            .find(|f| f.role == m.role)
            .ok_or_else(|| format!("布局里找不到角色 {:?}", m.role))?;
        let bytes = read_bytes(&dir.join(&m.file_name))
            .map_err(|e| format!("读包内文件 {:?} 失败: {e}", m.file_name))?;
        let actual = sha256_of(&bytes);
        if actual != m.sha256 {
            return Err(format!(
                "包内 {:?} 哈希不符（期望 {}.. 实际 {}..），拒绝写入",
                m.file_name,
                &m.sha256[..8.min(m.sha256.len())],
                &actual[..8.min(actual.len())]
            ));
        }
        staging.push((
            CredentialTarget {
                role: m.role,
                path: f.path.clone(),
                want_sha: m.sha256.clone(),
            },
            bytes,
        ));
    }
    if staging.is_empty() {
        return Err("bundle 是空的，没有可写入的文件".into());
    }

    std::fs::create_dir_all(backup_dir)
        .map_err(|e| format!("创建备份目录 {backup_dir:?} 失败: {e}"))?;

    // 1) 备份现场。原本不存在的记为 None，回滚时按"应删除"处理。
    let mut items: Vec<BackupItem> = Vec::new();
    for (t, _) in &staging {
        let saved_name = if t.path.exists() {
            let bytes = read_bytes(&t.path)
                .map_err(|e| format!("备份读取 {:?} 失败（尚未写入任何东西）: {e}", t.path))?;
            let name = t.role_file_name();
            atomic_write_bytes(&backup_dir.join(&name), &bytes)
                .map_err(|e| format!("备份写入失败: {e}"))?;
            Some(name)
        } else {
            None
        };
        let sha256 = saved_name.as_ref().and_then(|name| {
            sha256_hex(&backup_dir.join(name)).ok()
        });
        items.push(BackupItem {
            role: t.role,
            path: t.path.clone(),
            saved_name,
            sha256,
        });
    }
    // 清单必须先于任何写入落盘：进程在这中间被杀掉，事后才有依据退回去。
    let manifest = BackupManifest {
        variant: bundle.variant,
        target: bundle.target,
        taken_at: now_ts(),
        items: items.clone(),
    };
    write_json(&backup_dir.join(MANIFEST_FILE), &manifest)?;
    write_json(&backup_dir.join("_meta.json"), bundle)?;

    // 2) 写入。
    for (t, bytes) in &staging {
        if let Some(parent) = t.path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| format!("创建 {:?} 失败: {e}", parent))?;
        }
        if let Err(e) = atomic_write_bytes(&t.path, bytes) {
            let detail = rollback(backup_dir, &items);
            return Err(format!("写入 {:?} 失败: {e}；回滚{detail}", t.path));
        }
    }

    // 3) 读回比对。Qoder 桌面端会在会话期持续重写 auth 文件，这一步是唯一能
    //    发现"写完就被覆盖"的手段。
    let mut mismatches = Vec::new();
    for (t, _) in &staging {
        match sha256_hex(&t.path) {
            Ok(h) if h == t.want_sha => {}
            Ok(h) => mismatches.push(format!(
                "{:?} 期望 {}.. 实际 {}..",
                t.role,
                &t.want_sha[..t.want_sha.len().min(8)],
                &h[..h.len().min(8)]
            )),
            Err(e) => mismatches.push(format!("{:?} 读回失败: {e}", t.role)),
        }
    }
    if !mismatches.is_empty() {
        let detail = rollback(backup_dir, &items);
        return Err(format!(
            "写入后校验未通过（目标进程很可能还在覆盖写入）: {}；回滚{detail}",
            mismatches.join("; ")
        ));
    }

    Ok(RestoreOutcome {
        written: staging.iter().map(|(t, _)| t.role).collect(),
        backup_dir: backup_dir.to_path_buf(),
        items,
    })
}

#[derive(Debug, Clone)]
struct CredentialTarget {
    role: FileRole,
    path: PathBuf,
    want_sha: String,
}

impl CredentialTarget {
    fn role_file_name(&self) -> String {
        format!("{:?}", self.role).to_lowercase()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct BackupItem {
    role: FileRole,
    path: PathBuf,
    /// Some(name) = 备份前现场存在，文件存于备份目录下的 `name`；
    /// None = 备份前不存在，回滚时应删除我们创建出来的文件。
    saved_name: Option<String>,
    /// 备份文件的期望哈希。Option 只为兼容旧清单（字段缺失按不校验处理）：
    /// 断电后备份文件可能截断，没有这道校验，恢复会把损坏凭据当"原状"写回现场。
    #[serde(default)]
    sha256: Option<String>,
}

/// 落盘的备份清单。有了它，回滚就不再依赖内存 —— 进程被杀或断电后，
/// 任何一次启动都能凭 `_restore.json` 把现场退回去。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BackupManifest {
    pub variant: QoderVariant,
    pub target: QoderTarget,
    pub taken_at: String,
    items: Vec<BackupItem>,
}

pub const MANIFEST_FILE: &str = "_restore.json";

#[derive(Debug, Clone)]
pub struct RestoreOutcome {
    pub written: Vec<FileRole>,
    pub backup_dir: PathBuf,
    items: Vec<BackupItem>,
}

impl RestoreOutcome {
    /// 用户反悔时把现场退回 restore 之前。
    pub fn undo(&self) -> Result<()> {
        restore_from_backup(&self.backup_dir, &self.items)
    }
}

/// 凭磁盘清单回滚（崩溃恢复入口，不需要内存上下文）。
pub fn undo_backup(backup_dir: &Path) -> Result<()> {
    let bytes = read_bytes(&backup_dir.join(MANIFEST_FILE))
        .map_err(|e| format!("读 {:?} 失败: {e}", backup_dir.join(MANIFEST_FILE)))?;
    let m: BackupManifest = serde_json::from_slice(&bytes)
        .map_err(|e| format!("备份清单解析失败: {e}"))?;
    restore_from_backup(backup_dir, &m.items)
}

fn restore_from_backup(backup_dir: &Path, backups: &[BackupItem]) -> Result<()> {
    let mut errs = Vec::new();
    for b in backups {
        if let Some(name) = &b.saved_name {
            let write_back = |bytes: &[u8]| -> Result<()> {
                // 断电后备份文件可能截断/为空：带期望哈希的清单必须先把损坏的备份
                // 拦下，绝不能把损坏凭据当"原状"无声写回现场。旧清单无哈希则照旧。
                if let Some(want) = &b.sha256 {
                    let actual = crate::modules::config::sha256_hex_bytes(bytes);
                    if actual != *want {
                        return Err(format!(
                            "备份文件 {name} 已损坏（期望 {}.. 实际 {}..），拒绝按它恢复",
                            &want[..8.min(want.len())],
                            &actual[..8.min(actual.len())]
                        ));
                    }
                }
                atomic_write_bytes(&b.path, bytes)
                    .map_err(|e| format!("写回 {:?} 失败: {e}", b.path))
            };
            match read_bytes(&backup_dir.join(name)).and_then(|bytes| {
                write_back(&bytes).map_err(std::io::Error::other)
            }) {
                Ok(()) => {}
                Err(e) => errs.push(format!("{:?}: {e}", b.path)),
            }
        } else if b.path.exists() {
            // 这个文件是我们这次才创建出来的，删掉才算退回原状。
            if let Err(e) = std::fs::remove_file(&b.path) {
                errs.push(format!("{:?}: {e}", b.path));
            }
        }
    }
    if errs.is_empty() {
        Ok(())
    } else {
        Err(format!("回滚不完整: {}", errs.join(" | ")))
    }
}

/// 回滚并生成可直接拼进错误信息的说明。
fn rollback(backup_dir: &Path, backups: &[BackupItem]) -> String {
    match restore_from_backup(backup_dir, backups) {
        Ok(()) => "成功".to_string(),
        Err(e) => format!("失败: {e}（现场可能处于半换号状态，备份在 {backup_dir:?}）"),
    }
}

fn sha256_of(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    let mut h = Sha256::new();
    h.update(bytes);
    hex::encode(h.finalize())
}

fn write_json<T: Serialize>(path: &Path, value: &T) -> Result<()> {
    let json = serde_json::to_vec_pretty(value).map_err(|e| e.to_string())?;
    atomic_write_bytes(path, &json).map_err(|e| format!("写 {path:?} 失败: {e}"))
}

/// 列出 store 下所有已认领的包（含每个 (版本·目标) 分片）。
pub fn list_all(store: &Path) -> Vec<Bundle> {
    let root = accounts_root_in(store);
    let Ok(read) = std::fs::read_dir(&root) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for acc in read.flatten() {
        if !acc.path().is_dir() {
            continue;
        }
        let id = acc.file_name().to_string_lossy().to_string();
        for (v, t) in crate::modules::variant::all_axes() {
            if let Ok(b) = load(store, &id, v, t) {
                if !b.is_empty() {
                    out.push(b);
                }
            }
        }
    }
    out.sort_by(|a, b| a.account_id.cmp(&b.account_id).then_with(|| a.target.cmp(&b.target)));
    out
}

/// 读明文登录回显拿账号身份。字段缺失或文件不存在都返回 None（不是错误）。
pub fn read_identity(path: &Path) -> Option<Identity> {
    let text = std::fs::read_to_string(path).ok()?;
    let v: serde_json::Value = serde_json::from_str(&text).ok()?;
    let get = |k: &str| {
        v.get(k)
            .and_then(|x| x.as_str())
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
    };
    Some(Identity {
        name: get("name"),
        email: get("email"),
        plan: get("plan"),
        product: get("product"),
        logged_in: v.get("logged_in").and_then(|x| x.as_bool()),
        snapshot_at: get("snapshot_at"),
        // uid 与到期时间只能从解密后的登录态拿，明文回显里没有。
        ..Identity::default()
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::modules::variant::{cli_dir, desktop_dir};

    fn fixture() -> (PathRoots, PathBuf, PathBuf) {
        let root = std::env::temp_dir().join(format!(
            "qs-bundle-{}",
            uuid::Uuid::new_v4().simple()
        ));
        let roots = PathRoots::sandbox(&root);
        let store = root.join("store");
        std::fs::create_dir_all(&store).unwrap();
        (roots, store, root)
    }

    /// 造一个假的"已登录"现场：桌面三件套 + CLI 明文回显。
    fn seed(roots: &PathRoots, auth: &[u8], key: &[u8], machine: &[u8], email: &str) {
        let d = desktop_dir(roots, QoderVariant::Cn);
        std::fs::create_dir_all(&d).unwrap();
        std::fs::write(d.join("auth.v1.dat"), auth).unwrap();
        std::fs::write(d.join("Local State"), key).unwrap();
        std::fs::write(d.join("auth.machine-id"), machine).unwrap();
        let cli = cli_dir(roots, QoderVariant::Cn);
        std::fs::create_dir_all(&cli).unwrap();
        std::fs::write(
            cli.join(".qoder-app-status.json"),
            format!("{{\"email\":\"{email}\",\"name\":\"n\",\"plan\":\"Free\",\"product\":\"qodercn\",\"logged_in\":true}}"),
        )
        .unwrap();
    }

    fn live(roots: &PathRoots, name: &str) -> Vec<u8> {
        std::fs::read(desktop_dir(roots, QoderVariant::Cn).join(name)).unwrap()
    }

    /// 每次切换一个独立备份目录，与 switch.rs 的命名规则一致。
    fn next_backup(store: &Path) -> PathBuf {
        store
            .join("backups")
            .join(format!("cn.desktop.{}", uuid::Uuid::new_v4().simple()))
    }

    #[test]
    fn capture_grabs_every_existing_file_and_reads_identity() {
        let (roots, store, tmp) = fixture();
        seed(&roots, b"authA", b"keyA", b"m1", "a@x.com");
        let b = capture(&roots, &store, "acct-a", QoderVariant::Cn, QoderTarget::Desktop).unwrap();

        assert_eq!(b.members.len(), 4, "应收到 auth/LocalState/machineId/回显");
        assert!(b.member(FileRole::AuthMain).is_some());
        assert!(b.member(FileRole::StatusEcho).is_some(), "回显属桌面目标");
        assert!(b.member(FileRole::ProfileOverlays).is_none(), "不存在的不该收");
        assert_eq!(b.identity.email.as_deref(), Some("a@x.com"));
        assert_eq!(b.identity.plan.as_deref(), Some("Free"));
        assert_eq!(b.display_label(), "a@x.com");
        assert!(b.dir_in(&store).join("bundle.json").is_file());
        std::fs::remove_dir_all(tmp).ok();
    }

    #[test]
    fn empty_bundle_is_reported_not_silently_accepted() {
        let (roots, store, tmp) = fixture();
        // CN CLI 本机不落盘凭据 → 捕获结果应为空包。
        std::fs::create_dir_all(cli_dir(&roots, QoderVariant::Cn).join(".auth")).unwrap();
        let b = capture(&roots, &store, "acct-cli", QoderVariant::Cn, QoderTarget::Cli).unwrap();
        assert!(b.is_empty());
        std::fs::remove_dir_all(tmp).ok();
    }

    #[test]
    fn restore_swaps_whole_group_and_undo_puts_it_back() {
        let (roots, store, tmp) = fixture();
        // 先登录 A 并认领，再在客户端里换成 B —— 现场与包不再是同一份内容，
        // 这样 restore 才算真的"换号"，undo 也有东西可退。
        seed(&roots, b"authA", b"keyA", b"mA", "a@x.com");
        let a = capture(&roots, &store, "acct-a", QoderVariant::Cn, QoderTarget::Desktop).unwrap();

        seed(&roots, b"authB", b"keyB", b"mB", "b@x.com");
        let b = capture(&roots, &store, "acct-b", QoderVariant::Cn, QoderTarget::Desktop).unwrap();
        assert_eq!(b.display_label(), "b@x.com", "包标签取自当时的明文回显");
        assert_eq!(live(&roots, "auth.v1.dat"), b"authB", "现场应是 B");

        let out = restore(&roots, &store, &a, &next_backup(&store)).unwrap();
        assert_eq!(live(&roots, "auth.v1.dat"), b"authA");
        assert_eq!(live(&roots, "Local State"), b"keyA", "主密钥必须成组换");
        assert_eq!(live(&roots, "auth.machine-id"), b"mA");
        assert!(out.backup_dir.is_dir());
        assert!(
            out.backup_dir.join(MANIFEST_FILE).is_file(),
            "清单必须先落盘，崩溃后才能退回"
        );

        out.undo().unwrap();
        assert_eq!(live(&roots, "auth.v1.dat"), b"authB", "undo 应退回切换前现场");
        assert_eq!(live(&roots, "Local State"), b"keyB");

        // 再切一次，然后只用磁盘上的备份目录做恢复（模拟进程被杀后重启）。
        let bk2 = next_backup(&store);
        let out2 = restore(&roots, &store, &a, &bk2).unwrap();
        assert_eq!(live(&roots, "auth.v1.dat"), b"authA");
        drop(out2);
        undo_backup(&bk2).unwrap();
        assert_eq!(
            live(&roots, "auth.v1.dat"),
            b"authB",
            "凭清单也应能恢复，不依赖内存"
        );

        // 备份目录里四个成员都该在，供事后人工恢复。
        assert!(out.backup_dir.join("authmain").is_file());
        assert!(out.backup_dir.join("localstate").is_file());
        std::fs::remove_dir_all(tmp).ok();
    }

    #[test]
    fn restore_refuses_half_swap() {
        let (roots, store, tmp) = fixture();
        seed(&roots, b"authA", b"keyA", b"mA", "a@x.com");
        let mut b =
            capture(&roots, &store, "acct-a", QoderVariant::Cn, QoderTarget::Desktop).unwrap();
        // 模拟"包只带了 auth.v1.dat，没带 Local State"。
        b.members.retain(|m| m.role != FileRole::LocalState);

        let err = restore(&roots, &store, &b, &store.join("backups").join("bk")).unwrap_err();
        assert!(err.contains("半换号"), "错误信息该说明拒写原因: {err}");
        assert_eq!(live(&roots, "auth.v1.dat"), b"authA", "拒写后现场不该被动过");
        std::fs::remove_dir_all(tmp).ok();
    }

    #[test]
    fn restore_refuses_corrupt_member_bytes() {
        let (roots, store, tmp) = fixture();
        seed(&roots, b"authA", b"keyA", b"mA", "a@x.com");
        let b = capture(&roots, &store, "acct-a", QoderVariant::Cn, QoderTarget::Desktop).unwrap();

        // 包外篡改：直接改 bundle 目录里的副本字节。
        std::fs::write(b.dir_in(&store).join("authmain"), b"tampered").unwrap();
        seed(&roots, b"authB", b"keyB", b"mB", "b@x.com");

        let err = restore(&roots, &store, &b, &store.join("backups").join("bk")).unwrap_err();
        assert!(err.contains("哈希不符"), "应检出包内损坏: {err}");
        assert_eq!(live(&roots, "auth.v1.dat"), b"authB");
        // 哈希校验必须在备份之前就拦住，否则会留下空备份目录。
        assert!(!store.join("backups").join("bk").exists());
        std::fs::remove_dir_all(tmp).ok();
    }

    #[test]
    fn days_until_and_merge_auth_fill_expiry_view() {
        assert!(days_until("不是时间").is_none());
        assert!(days_until("2099-01-01T00:00:00Z").unwrap() > 0);

        let mut id = Identity::default();
        id.name = Some("回显名".into());
        let mut doc = serde_json::Map::new();
        doc.insert("schemaVersion".into(), 1.into());
        doc.insert("token".into(), "t".repeat(27).into());
        doc.insert("refreshToken".into(), "r".repeat(28).into());
        doc.insert("expiresAt".into(), "2099-01-01T00:00:00Z".into());
        doc.insert("refreshTokenExpiresAt".into(), "2099-09-01T00:00:00Z".into());
        let mut u = serde_json::Map::new();
        u.insert("id".into(), "019f0000-0000-7000-8000-000000000001".into());
        u.insert("name".into(), "解密名".into());
        doc.insert("user".into(), u.into());
        let text = serde_json::to_vec(&serde_json::Value::Object(doc)).unwrap();
        let auth = crate::modules::auth_codec::parse_auth(&text).unwrap();
        id.merge_auth(&auth);

        assert_eq!(
            id.uid.as_deref(),
            Some("019f0000-0000-7000-8000-000000000001")
        );
        assert_eq!(id.name.as_deref(), Some("解密名"), "解密结果更权威");
        assert!(id.token_days_left().unwrap() > 0);
    }

    #[test]
    fn validate_account_id_catches_reserved_and_edge_cases() {
        assert!(validate_account_id("my-account_1").is_ok());
        assert!(validate_account_id("user.test").is_ok());

        // 边界：点开头或结尾
        assert!(validate_account_id(".hidden").is_err());
        assert!(validate_account_id("trailing.").is_err());
        assert!(validate_account_id("").is_err());

        // Windows 系统保留名（不分大小写）
        assert!(validate_account_id("con").is_err());
        assert!(validate_account_id("CON").is_err());
        assert!(validate_account_id("prn").is_err());
        assert!(validate_account_id("aux").is_err());
        assert!(validate_account_id("nul").is_err());
        assert!(validate_account_id("com1").is_err());
        assert!(validate_account_id("lpt3").is_err());
        assert!(validate_account_id("con.backup").is_err());
    }

    /// 真机：认领 CN 桌面账号必须带出 uid 与到期时间。
    #[test]
    #[cfg(windows)]
    fn capture_on_real_machine_carries_expiry() {
        let tmp =
            std::env::temp_dir().join(format!("qs-cap-real-{}", uuid::Uuid::new_v4().simple()));
        let store = tmp.join("store");
        std::fs::create_dir_all(&store).unwrap();
        let roots = PathRoots::real();
        if desktop_dir(&roots, QoderVariant::Cn)
            .join("auth.v1.dat")
            .is_file()
        {
            let b =
                capture(&roots, &store, "real-cn", QoderVariant::Cn, QoderTarget::Desktop).unwrap();
            assert!(b.identity.uid.is_some(), "真机应能解出 uid");
            assert!(b.identity.expires_at.is_some(), "真机应能解出到期时间");
            assert!(
                b.identity.token_days_left().unwrap_or(0) > 0,
                "到期时间应在未来"
            );
        }
        std::fs::remove_dir_all(tmp).ok();
    }

    #[test]
    fn rollback_restores_saved_and_removes_files_we_created() {
        let (_roots, _store, tmp) = fixture();
        let dir = tmp.join("bk");
        std::fs::create_dir_all(&dir).unwrap();
        let pre = dir.join("saved");
        std::fs::write(&pre, b"original").unwrap();

        let restored_path = tmp.join("live.dat");
        let created_path = tmp.join("created.dat");
        std::fs::write(&restored_path, b"swapped").unwrap();
        std::fs::write(&created_path, b"we-made-this").unwrap();

        restore_from_backup(
            &dir,
            &[
                BackupItem {
                    role: FileRole::AuthMain,
                    path: restored_path.clone(),
                    saved_name: Some("saved".into()),
                    sha256: None,
                },
                BackupItem {
                    role: FileRole::CliUser,
                    path: created_path.clone(),
                    saved_name: None,
                    sha256: None,
                },
            ],
        )
        .unwrap();

        assert_eq!(std::fs::read(&restored_path).unwrap(), b"original");
        assert!(!created_path.exists(), "原本不存在的文件回滚时应删除");
        std::fs::remove_dir_all(tmp).ok();
    }
}
