//! 账号包导出 / 导入。
//!
//! 单文件格式 = JSON + 每个凭据文件的 base64 副本。刻意不引 zip：包体都是
//! 几百字节的密文文件，JSON 更好排查，也不需要多一个依赖。
//!
//! 跨机器搬运的限制必须在 UI 上讲清楚：桌面凭据是 DPAPI(CURRENT_USER) 加密，
//! 主密钥在各 appdata 的 `Local State` 里。包内已带 `Local State`，但它同样是
//! 按 Windows 用户加密的 —— 因此**换机器或换 Windows 账号后导入会静默变成未登录**，
//! 只能在同一 Windows 用户内复用。

use std::path::{Path, PathBuf};

use base64::Engine as _;
use serde::{Deserialize, Serialize};

use crate::modules::variant::{QoderTarget, QoderVariant};
use crate::modules::{bundle, variant};
use crate::Result;

pub const FORMAT_VERSION: u32 = 1;

/// 单个凭据文件最大允许导入 10MB（真实凭据文件通常在几十 KB 以内，防止 OOM 资源耗尽攻击）。
pub const MAX_IMPORT_FILE_BYTES: usize = 10 * 1024 * 1024;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExportedFile {
    /// 角色名（`FileRole` 的 Debug 形态，与 bundle 内文件名一致）。
    pub role: String,
    pub sha256: String,
    pub data: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExportedBundle {
    pub account_id: String,
    pub variant: QoderVariant,
    pub target: QoderTarget,
    pub created_at: String,
    pub identity: bundle::Identity,
    pub files: Vec<ExportedFile>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Export {
    pub format: u32,
    pub exported_at: String,
    pub bundles: Vec<ExportedBundle>,
}

/// 导出一个账号名下的全部分片（桌面 / CLI / Work 各一份）。
pub fn export_account(store: &Path, account_id: &str) -> Result<Export> {
    let mut bundles = Vec::new();
    for (v, t) in variant::all_axes() {
        let Ok(b) = bundle::load(store, account_id, v, t) else {
            continue;
        };
        if b.is_empty() {
            continue;
        }
        let dir = b.dir_in(store);
        let mut files = Vec::new();
        for m in &b.members {
            let bytes = std::fs::read(dir.join(&m.file_name))
                .map_err(|e| format!("读包内 {:?} 失败: {e}", m.file_name))?;
            let actual = sha256_of(&bytes);
            if actual != m.sha256 {
                // bundle.json 可能被截断出短哈希，别让错误消息本身先 panic。
                return Err(format!(
                    "包内 {:?} 已损坏（期望 {}.. 实际 {}..），拒绝导出",
                    m.file_name,
                    &m.sha256[..m.sha256.len().min(8)],
                    &actual[..actual.len().min(8)]
                ));
            }
            files.push(ExportedFile {
                role: format!("{:?}", m.role),
                sha256: m.sha256.clone(),
                data: base64::engine::general_purpose::STANDARD.encode(&bytes),
            });
        }
        bundles.push(ExportedBundle {
            account_id: account_id.to_string(),
            variant: v,
            target: t,
            created_at: b.created_at.clone(),
            identity: b.identity.clone(),
            files,
        });
    }
    if bundles.is_empty() {
        return Err(format!("账号 {account_id:?} 没有任何可导出的分片"));
    }
    Ok(Export {
        format: FORMAT_VERSION,
        exported_at: crate::modules::config::now_ts(),
        bundles,
    })
}

/// 序列化为可直接落盘的字节。
pub fn to_bytes(export: &Export) -> Result<Vec<u8>> {
    serde_json::to_vec_pretty(export).map_err(|e| e.to_string())
}

#[derive(Debug)]
pub struct ImportReport {
    pub written: Vec<(String, QoderVariant, QoderTarget, usize)>,
    pub skipped: Vec<String>,
}

/// 导入。默认不覆盖同名已存在的分片 —— 覆盖账号包等于覆盖一个可登录身份，
/// 需要调用方显式 `overwrite`。
pub fn import(store: &Path, raw: &[u8], overwrite: bool) -> Result<ImportReport> {
    let export: Export = serde_json::from_slice(raw)
        .map_err(|e| format!("导出文件解析失败: {e}"))?;
    if export.format != FORMAT_VERSION {
        return Err(format!(
            "导出格式版本 {} 不被支持（本程序为 {FORMAT_VERSION}）",
            export.format
        ));
    }
    let mut written = Vec::new();
    let mut skipped = Vec::new();

    for b in &export.bundles {
        // account_id 来自被导入的外部文件，是 store 路径的组成部分 —— 不校验就是
        // "导入一个分享文件 = 在任意目录种凭据副本"的写原语。
        bundle::validate_account_id(&b.account_id)
            .map_err(|e| format!("导出文件含非法账号名: {e}"))?;
        let dir = bundle::bundle_dir_in(store, &b.account_id, b.variant, b.target);
        if !overwrite && dir.join("bundle.json").is_file() {
            skipped.push(format!(
                "{}/{}/{} 已存在（要覆盖请显式确认）",
                b.account_id,
                variant_label(b.variant),
                target_label(b.target)
            ));
            continue;
        }
        std::fs::create_dir_all(&dir)
            .map_err(|e| format!("创建 {:?} 失败: {e}", dir))?;

        let mut members = Vec::new();
        let mut seen_roles = std::collections::HashSet::new();
        for f in &b.files {
            let role = parse_role(&f.role)?;
            if !seen_roles.insert(role) {
                return Err(format!("分片含重复角色 {:?}，拒绝导入", role));
            }
            let bytes = base64::engine::general_purpose::STANDARD
                .decode(&f.data)
                .map_err(|e| format!("{:?} base64 解码失败: {e}", f.role))?;
            if bytes.len() > MAX_IMPORT_FILE_BYTES {
                return Err(format!(
                    "{:?} 超过单文件最大限制（{} 字节 > {} 字节）",
                    role,
                    bytes.len(),
                    MAX_IMPORT_FILE_BYTES
                ));
            }
            let actual = sha256_of(&bytes);
            if actual != f.sha256 {
                return Err(format!(
                    "{:?} 哈希不符（期望 {}.. 实际 {}..），整个导入中止",
                    role,
                    &f.sha256[..f.sha256.len().min(8)],
                    &actual[..actual.len().min(8)]
                ));
            }
            let file_name = format!("{:?}", role).to_lowercase();
            crate::modules::config::atomic_write_bytes(&dir.join(&file_name), &bytes)
                .map_err(|e| format!("写包内文件失败: {e}"))?;
            members.push(bundle::Member {
                role,
                file_name,
                sha256: f.sha256.clone(),
                size: bytes.len() as u64,
                critical: variant::role_is_critical(role),
            });
        }
        if b.target == QoderTarget::Desktop && !members.iter().any(|m| m.critical) {
            return Err(format!(
                "分片 {}/{}/{} 缺少关键凭据文件（无任何 critical 文件），拒绝导入",
                b.account_id,
                variant_label(b.variant),
                target_label(b.target)
            ));
        }
        let nb = bundle::Bundle {
            account_id: b.account_id.clone(),
            variant: b.variant,
            target: b.target,
            created_at: b.created_at.clone(),
            members,
            identity: b.identity.clone(),
        };
        bundle::write_meta(store, &nb)?;
        written.push((b.account_id.clone(), b.variant, b.target, nb.members.len()));
    }
    if written.is_empty() && skipped.is_empty() {
        return Err("导出文件里没有任何分片".into());
    }
    Ok(ImportReport { written, skipped })
}

fn variant_label(v: QoderVariant) -> &'static str {
    match v {
        QoderVariant::Cn => "cn",
        QoderVariant::Global => "global",
    }
}

fn target_label(t: QoderTarget) -> &'static str {
    match t {
        QoderTarget::Desktop => "desktop",
        QoderTarget::Cli => "cli",
        QoderTarget::Work => "work",
    }
}

/// 角色名从导出文件里来，必须与 bundle 的命名一致；未知角色直接拒。
fn parse_role(raw: &str) -> Result<crate::modules::variant::FileRole> {
    use crate::modules::variant::FileRole as R;
    let r = match raw.trim().to_ascii_lowercase().as_str() {
        "authmain" => R::AuthMain,
        "authv2" => R::AuthV2,
        "profileoverlays" => R::ProfileOverlays,
        "desktopmachineid" => R::DesktopMachineId,
        "localstate" => R::LocalState,
        "channelactivation" => R::ChannelActivation,
        "cliuser" => R::CliUser,
        "climachineid" => R::CliMachineId,
        "statusecho" => R::StatusEcho,
        other => return Err(format!("未知凭据角色: {other}")),
    };
    Ok(r)
}

fn sha256_of(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    let mut h = Sha256::new();
    h.update(bytes);
    hex::encode(h.finalize())
}

/// 自动备份保留的份数。
pub const BACKUP_KEEP: usize = 5;

/// 备份文件名特征。前后缀一起用于识别"这是本工具的整库备份" —— 清理旧备份时
/// 按它过滤，同目录下用户的其它文件一律不碰。
const BACKUP_PREFIX: &str = "qoder-switch-accounts-";
const BACKUP_SUFFIX: &str = ".json";

/// 默认备份目录：`<用户文档目录>/QoderSwitch-AccountBackups`。
///
/// 刻意放在 store **之外**：store 一旦被整体替换（本仓库真实遇到过的情形），
/// 放在 store 里的备份会跟着一起消失，等于没备份。放文档目录还有个额外好处 ——
/// 重装、迁移甚至换机器时，它都会自然被带走。
pub fn backup_dir() -> Option<PathBuf> {
    dirs::document_dir().map(|d| d.join("QoderSwitch-AccountBackups"))
}

/// 导出 store 里**全部**账号包，合成一份整库快照。
///
/// 与 [`export_account`] 的分工：那个按单个账号导，供界面上的「导出」按钮按需取；
/// 这个是整库导，供自动备份用。账号包是不可再生的凭据副本 —— 现场只保留当前登录
/// 的那一个，其余账号一旦包丢了就只能重新扫码，所以整库必须有独立副本。
pub fn export_all(store: &Path) -> Result<Export> {
    let mut bundles = Vec::new();
    // 同一账号在多个轴上都有包时只导一次：export_account 内部已遍历全部轴。
    let mut seen = std::collections::BTreeSet::new();
    for b in bundle::list_all(store) {
        if !seen.insert(b.account_id.clone()) {
            continue;
        }
        // 单个包损坏不该让整库备份失败 —— 能备份多少算多少，坏的留给用户单独处置。
        if let Ok(mut e) = export_account(store, &b.account_id) {
            bundles.append(&mut e.bundles);
        }
    }
    if bundles.is_empty() {
        return Err("账号库里没有任何可备份的账号包".into());
    }
    Ok(Export {
        format: FORMAT_VERSION,
        exported_at: crate::modules::config::now_ts(),
        bundles,
    })
}

/// 账号库的内容指纹：只看"有哪些包、每个包里有哪些文件、各自哈希多少"，
/// **刻意不含 `exported_at`** —— 否则每次导出的字节都不同，去重就永远不命中。
fn export_fingerprint(e: &Export) -> String {
    use sha2::{Digest, Sha256};
    let mut rows: Vec<String> = Vec::new();
    for b in &e.bundles {
        for f in &b.files {
            rows.push(format!(
                "{}|{:?}|{:?}|{}|{}",
                b.account_id, b.variant, b.target, f.role, f.sha256
            ));
        }
    }
    rows.sort();
    let mut h = Sha256::new();
    for r in &rows {
        h.update(r.as_bytes());
        h.update(b"\n");
    }
    hex::encode(h.finalize())
}

/// 读回一份备份文件（恢复前预览、以及去重比对都要用）。
pub fn load_backup(path: &Path) -> Result<Export> {
    let bytes =
        std::fs::read(path).map_err(|e| format!("读备份 {} 失败: {e}", path.display()))?;
    serde_json::from_slice(&bytes)
        .map_err(|e| format!("解析备份 {} 失败: {e}", path.display()))
}

/// 把整库备份到 `dest`，并按文件名时间戳只保留最近 [`BACKUP_KEEP`] 份。
///
/// 账号库为空时返回 `Ok(None)` —— 那不是错误，只是没什么可备份的。
///
/// 内容与最新一份备份完全一致时**不落新文件**，直接复用那一份。否则"连点两次
/// 导入本机账号"会把 [`BACKUP_KEEP`] 个备份位占满成同一份，真正有区分度的历史
/// 反而被挤出去 —— 备份的价值全在"能回到更早的某个状态"。
///
/// `dest` 由调用方给定而不是内部直接取 [`backup_dir`]：测试必须能指向临时目录，
/// 否则跑一次测试就往用户的真实文档目录里写东西。
pub fn auto_backup(store: &Path, dest: &Path) -> Result<Option<PathBuf>> {
    let export = match export_all(store) {
        Ok(e) => e,
        Err(e) if e.contains("没有任何可备份") => return Ok(None),
        Err(e) => return Err(e),
    };
    let fingerprint = export_fingerprint(&export);
    if let Some(newest) = list_backups(dest).first() {
        let same = load_backup(newest)
            .map(|e| export_fingerprint(&e) == fingerprint)
            .unwrap_or(false);
        if same {
            // 跳过写入也必须执行修剪：否则"目录里已超过上限、内容又恰好没变"时
            // 保留策略永远不收敛（比如用户手工复制进来几份，或上限被调小过）。
            prune_backups(dest, BACKUP_KEEP)?;
            return Ok(Some(newest.clone()));
        }
    }
    std::fs::create_dir_all(dest)
        .map_err(|e| format!("创建备份目录 {} 失败: {e}", dest.display()))?;
    // 文件名带内容指纹：同一秒内的两份**不同**备份不能互相覆盖（只按秒命名会撞名，
    // 而 atomic_write_bytes 是覆盖写）。时间戳在前，字典序仍是时间序。
    let path = dest.join(format!(
        "{BACKUP_PREFIX}{}-{}{BACKUP_SUFFIX}",
        crate::modules::config::now_ts(),
        &fingerprint[..8]
    ));
    crate::modules::config::atomic_write_bytes(&path, &to_bytes(&export)?)
        .map_err(|e| format!("写备份文件失败: {e}"))?;
    prune_backups(dest, BACKUP_KEEP)?;
    Ok(Some(path))
}

/// 在**真实账号库**上做一次自动备份，尽力而为：返回 `None` 表示"这次没备"
/// （空库、取不到文档目录、或传入的路径不是真实账号库）。任何失败都不上抛 ——
/// 备份是护栏，护栏自己不能把用户的正常操作绊倒。
///
/// 只在 `store` 等于 [`crate::modules::config::switch_root`] 时动作：单测与沙箱演练
/// 传进来的临时 store 绝不能往用户的文档目录里写东西。这条守卫是本函数的契约，
/// 由 `auto_backup_default_refuses_a_sandbox_store` 钉死。
pub fn auto_backup_default(store: &Path) -> Option<PathBuf> {
    if store != crate::modules::config::switch_root().as_path() {
        return None;
    }
    let dest = backup_dir()?;
    auto_backup(store, &dest).ok().flatten()
}

/// 现有备份文件，**新的在前**。前端据此判断"账号库空了但备份还在"。
pub fn list_backups(dest: &Path) -> Vec<PathBuf> {
    let Ok(read) = std::fs::read_dir(dest) else {
        return Vec::new();
    };
    let mut files: Vec<PathBuf> = read
        .flatten()
        .map(|e| e.path())
        .filter(|p| is_backup_file(p))
        .collect();
    // 文件名里的时间戳是 `%Y%m%dT%H%M%SZ`，字典序即时间序。
    files.sort();
    files.reverse();
    files
}

fn is_backup_file(p: &Path) -> bool {
    p.file_name()
        .and_then(|n| n.to_str())
        .map(|n| n.starts_with(BACKUP_PREFIX) && n.ends_with(BACKUP_SUFFIX))
        .unwrap_or(false)
}

/// 只保留最近 `keep` 份。按文件名排序而不是 mtime —— 复制与云同步会打乱 mtime，
/// 但改不了名字里的时间戳。
fn prune_backups(dest: &Path, keep: usize) -> Result<()> {
    let mut files: Vec<PathBuf> = std::fs::read_dir(dest)
        .map_err(|e| format!("读备份目录 {} 失败: {e}", dest.display()))?
        .flatten()
        .map(|e| e.path())
        .filter(|p| is_backup_file(p))
        .collect();
    files.sort();
    if files.len() <= keep {
        return Ok(());
    }
    for old in &files[..files.len() - keep] {
        std::fs::remove_file(old)
            .map_err(|e| format!("清理旧备份 {} 失败: {e}", old.display()))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::modules::config::PathRoots;
    use crate::modules::variant::{desktop_dir, FileRole, QoderVariant};
    use std::path::PathBuf;

    fn sandbox() -> (PathRoots, PathBuf, PathBuf) {
        let tmp = std::env::temp_dir().join(format!("qs-exp-{}", uuid::Uuid::new_v4().simple()));
        let roots = PathRoots::sandbox(&tmp);
        let store = tmp.join("store");
        std::fs::create_dir_all(&store).unwrap();
        (roots, store, tmp)
    }

    fn seed(roots: &PathRoots) {
        let d = desktop_dir(roots, QoderVariant::Cn);
        std::fs::create_dir_all(&d).unwrap();
        std::fs::write(d.join("auth.v1.dat"), b"authA").unwrap();
        std::fs::write(d.join("Local State"), b"keyA").unwrap();
        std::fs::write(d.join("auth.machine-id"), b"m1").unwrap();
        let cli = roots.home.join(".qoder-cn");
        std::fs::create_dir_all(&cli).unwrap();
        std::fs::write(
            cli.join(".qoder-app-status.json"),
            b"{\"email\":\"a@x.com\",\"product\":\"qodercn\"}",
        )
        .unwrap();
    }

    #[test]
    fn export_then_import_into_a_fresh_store() {
        let (roots, store, tmp) = sandbox();
        seed(&roots);
        bundle::capture(&roots, &store, "acct-a", QoderVariant::Cn, QoderTarget::Desktop).unwrap();

        let raw = to_bytes(&export_account(&store, "acct-a").unwrap()).unwrap();
        assert!(String::from_utf8_lossy(&raw).contains("a@x.com"));

        // 导进一个空 store：等价于把账号搬到另一台机器。
        let other = tmp.join("other-store");
        std::fs::create_dir_all(&other).unwrap();
        let rep = import(&other, &raw, false).unwrap();
        assert_eq!(rep.written.len(), 1);
        assert!(rep.skipped.is_empty());

        let b = bundle::load(&other, "acct-a", QoderVariant::Cn, QoderTarget::Desktop).unwrap();
        assert_eq!(b.members.len(), 4, "角色与 critical 标记应完整还原");
        assert_eq!(b.identity.email.as_deref(), Some("a@x.com"));
        assert!(b.member(FileRole::AuthMain).unwrap().critical);
        assert!(!b.member(FileRole::StatusEcho).unwrap().critical);
        std::fs::remove_dir_all(tmp).ok();
    }

    #[test]
    fn import_does_not_overwrite_without_consent() {
        let (roots, store, tmp) = sandbox();
        seed(&roots);
        bundle::capture(&roots, &store, "acct-a", QoderVariant::Cn, QoderTarget::Desktop).unwrap();
        let raw = to_bytes(&export_account(&store, "acct-a").unwrap()).unwrap();

        let rep = import(&store, &raw, false).unwrap();
        assert!(rep.written.is_empty());
        assert_eq!(rep.skipped.len(), 1, "同名分片应被跳过而非静默覆盖");

        assert!(!import(&store, &raw, true).unwrap().written.is_empty());
        std::fs::remove_dir_all(tmp).ok();
    }

    /// 导出文件被改过一个字节后，导入必须整体中止，不能留下半套账号。
    #[test]
    fn import_aborts_on_tampered_payload() {
        let (roots, store, tmp) = sandbox();
        seed(&roots);
        bundle::capture(&roots, &store, "acct-a", QoderVariant::Cn, QoderTarget::Desktop).unwrap();
        let mut e = export_account(&store, "acct-a").unwrap();
        let target = e
            .bundles
            .first_mut()
            .and_then(|b| b.files.first_mut())
            .expect("有文件");
        target.data = base64::engine::general_purpose::STANDARD.encode(b"tampered");
        let raw = to_bytes(&e).unwrap();

        let dest = tmp.join("dest");
        std::fs::create_dir_all(&dest).unwrap();
        let err = import(&dest, &raw, false).unwrap_err();
        assert!(err.contains("哈希不符"), "应检出篡改: {err}");
        std::fs::remove_dir_all(tmp).ok();
    }

    #[test]
    fn unknown_role_and_format_are_rejected() {
        assert!(parse_role("notAFile").is_err());
        assert!(parse_role("AuthMain").is_ok());
        let e = Export {
            format: 99,
            exported_at: "t".into(),
            bundles: vec![],
        };
        let raw = to_bytes(&e).unwrap();
        let dest = std::env::temp_dir().join(format!("qs-fmt-{}", uuid::Uuid::new_v4().simple()));
        std::fs::create_dir_all(&dest).unwrap();
        let err = import(&dest, &raw, false).unwrap_err();
        assert!(err.contains("格式版本"), "{err}");
        std::fs::remove_dir_all(dest).ok();
    }

    #[test]
    fn import_rejects_duplicate_role_in_bundle() {
        let (_roots, _store, tmp) = sandbox();
        let other = tmp.join("other-store");
        std::fs::create_dir_all(&other).unwrap();
        let bad_json = r#"{
            "format": 1,
            "exported_at": "2026-09-21T00:00:00Z",
            "bundles": [{
                "account_id": "dup-test",
                "variant": "cn",
                "target": "desktop",
                "created_at": "2026-09-21T00:00:00Z",
                "identity": {},
                "files": [
                    {"role": "AuthMain", "sha256": "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855", "data": ""},
                    {"role": "AuthMain", "sha256": "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855", "data": ""}
                ]
            }]
        }"#;
        let res = import(&other, bad_json.as_bytes(), false);
        assert!(res.is_err(), "重复角色必须拒绝");
        assert!(res.unwrap_err().contains("重复角色"));
        std::fs::remove_dir_all(tmp).ok();
    }

    #[test]
    fn import_rejects_desktop_without_critical_files() {
        let (_roots, _store, tmp) = sandbox();
        let other = tmp.join("other-store");
        std::fs::create_dir_all(&other).unwrap();
        let bad_json = r#"{
            "format": 1,
            "exported_at": "2026-09-21T00:00:00Z",
            "bundles": [{
                "account_id": "no-crit-test",
                "variant": "cn",
                "target": "desktop",
                "created_at": "2026-09-21T00:00:00Z",
                "identity": {},
                "files": [
                    {"role": "StatusEcho", "sha256": "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855", "data": ""}
                ]
            }]
        }"#;
        let res = import(&other, bad_json.as_bytes(), false);
        assert!(res.is_err(), "桌面端缺少关键凭据文件必须拒绝");
        assert!(res.unwrap_err().contains("缺少关键凭据文件"));
        std::fs::remove_dir_all(tmp).ok();
    }

    /// 整库备份：全部账号一起导，且只保留最近 N 份。
    #[test]
    fn auto_backup_writes_whole_store_and_keeps_recent() {
        let (roots, store, tmp) = sandbox();
        seed(&roots);
        bundle::capture(&roots, &store, "acct-a", QoderVariant::Cn, QoderTarget::Desktop).unwrap();

        let dest = tmp.join("backups");
        let p1 = auto_backup(&store, &dest).unwrap().expect("有包就该写出备份");
        assert!(p1.is_file(), "备份文件应存在: {p1:?}");
        // 备份必须落在 store 之外 —— 落在 store 里会随 store 一起消失，等于没备份。
        assert!(!p1.starts_with(&store), "备份不能写在 store 内部: {p1:?}");

        let parsed: Export = serde_json::from_slice(&std::fs::read(&p1).unwrap()).unwrap();
        assert_eq!(parsed.format, FORMAT_VERSION);
        assert!(
            parsed.bundles.iter().any(|b| b.account_id == "acct-a"),
            "整库备份里应含 acct-a"
        );

        // 塞进比保留份数更多的旧备份，再备份一次，最旧的应被清掉。
        for i in 1..(BACKUP_KEEP + 2) {
            std::fs::write(
                dest.join(format!("{BACKUP_PREFIX}2026010{i}T000000Z{BACKUP_SUFFIX}")),
                b"{}",
            )
            .unwrap();
        }
        auto_backup(&store, &dest).unwrap();

        let left = list_backups(&dest);
        assert_eq!(left.len(), BACKUP_KEEP, "应只保留 {BACKUP_KEEP} 份: {left:?}");
        // 新的在前。
        let name = |p: &PathBuf| p.file_name().unwrap().to_string_lossy().to_string();
        assert!(name(&left[0]) > name(&left[1]), "列表应是新的在前: {left:?}");

        std::fs::remove_dir_all(tmp).ok();
    }

    /// 空账号库不是错误，也不该产出空备份文件或凭空建目录。
    #[test]
    fn auto_backup_on_empty_store_is_a_noop() {
        let (_roots, store, tmp) = sandbox();
        let dest = tmp.join("backups");
        assert!(auto_backup(&store, &dest).unwrap().is_none());
        assert!(!dest.exists(), "空库连备份目录都不该创建");
        std::fs::remove_dir_all(tmp).ok();
    }

    /// 清理旧备份时，同目录下不属于本工具的文件一律不能碰。
    #[test]
    fn prune_only_touches_our_own_backups() {
        let (_roots, _store, tmp) = sandbox();
        let dest = tmp.join("backups");
        std::fs::create_dir_all(&dest).unwrap();
        let keep_me = dest.join("我的笔记.json");
        std::fs::write(&keep_me, b"do not touch").unwrap();
        for i in 0..(BACKUP_KEEP + 3) {
            std::fs::write(
                dest.join(format!("{BACKUP_PREFIX}2026010{i}T000000Z{BACKUP_SUFFIX}")),
                b"{}",
            )
            .unwrap();
        }

        prune_backups(&dest, BACKUP_KEEP).unwrap();

        assert!(keep_me.is_file(), "同目录下的其它文件不能被误删");
        assert_eq!(list_backups(&dest).len(), BACKUP_KEEP);
        std::fs::remove_dir_all(tmp).ok();
    }

    /// 库内容没变就不该再落一份新备份，否则 BACKUP_KEEP 个位置会被同一份占满，
    /// "能回到更早的状态"这个唯一价值就没了。
    #[test]
    fn auto_backup_skips_when_content_is_unchanged() {
        let (roots, store, tmp) = sandbox();
        seed(&roots);
        bundle::capture(&roots, &store, "acct-a", QoderVariant::Cn, QoderTarget::Desktop).unwrap();

        let dest = tmp.join("backups");
        let p1 = auto_backup(&store, &dest).unwrap().unwrap();
        let p2 = auto_backup(&store, &dest).unwrap().unwrap();
        assert_eq!(p1, p2, "内容没变应复用同一份备份");
        assert_eq!(list_backups(&dest).len(), 1, "不该多落文件");

        // 账号集合变了就必须落新的一份。两次备份在同一秒内完成 —— 文件名带内容
        // 指纹，所以不会撞名互相覆盖。
        bundle::capture(&roots, &store, "acct-b", QoderVariant::Cn, QoderTarget::Desktop).unwrap();
        let p3 = auto_backup(&store, &dest).unwrap().unwrap();
        assert_ne!(p3, p1, "账号集合变了必须落新备份");
        assert_eq!(list_backups(&dest).len(), 2);
        std::fs::remove_dir_all(tmp).ok();
    }

    /// 默认入口只认真实账号库 —— 沙箱 store 必须被拒，否则跑一次测试就往用户的
    /// 文档目录里写凭据副本。
    #[test]
    fn auto_backup_default_refuses_a_sandbox_store() {
        let (roots, store, tmp) = sandbox();
        seed(&roots);
        bundle::capture(&roots, &store, "acct-a", QoderVariant::Cn, QoderTarget::Desktop).unwrap();
        assert!(
            auto_backup_default(&store).is_none(),
            "沙箱 store 不是 switch_root()，绝不能触发默认备份"
        );
        std::fs::remove_dir_all(tmp).ok();
    }
}
