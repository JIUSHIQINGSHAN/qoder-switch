//! 账号包导出 / 导入。
//!
//! 单文件格式 = JSON + 每个凭据文件的 base64 副本。刻意不引 zip：包体都是
//! 几百字节的密文文件，JSON 更好排查，也不需要多一个依赖。
//!
//! 跨机器搬运的限制必须在 UI 上讲清楚：桌面凭据是 DPAPI(CURRENT_USER) 加密，
//! 主密钥在各 appdata 的 `Local State` 里。包内已带 `Local State`，但它同样是
//! 按 Windows 用户加密的 —— 因此**换机器或换 Windows 账号后导入会静默变成未登录**，
//! 只能在同一 Windows 用户内复用。

use std::path::Path;

use base64::Engine as _;
use serde::{Deserialize, Serialize};

use crate::modules::variant::{QoderTarget, QoderVariant};
use crate::modules::{bundle, variant};
use crate::Result;

pub const FORMAT_VERSION: u32 = 1;

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
        for f in &b.files {
            let role = parse_role(&f.role)?;
            let bytes = base64::engine::general_purpose::STANDARD
                .decode(&f.data)
                .map_err(|e| format!("{:?} base64 解码失败: {e}", f.role))?;
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
}
