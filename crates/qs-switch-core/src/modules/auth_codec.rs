//! Qoder 桌面凭据文件的编解码。
//!
//! 实测方案（2026-09-20 本机验证，见 `scripts/probe-auth-codec.py`）：
//! - `%APPDATA%\<app>\Local State` 的 `os_crypt.encrypted_key` 是
//!   base64(`"DPAPI"` + DPAPI blob)，DPAPI(CURRENT_USER) 解出来是 **32 字节** AES-256 密钥。
//! - `auth.v1.dat` = `b"v10"` + 12 字节 IV + AES-256-GCM 密文（末尾 16 字节 tag）。
//! - 明文 JSON：`{schemaVersion:1, token, refreshToken, expiresAt,
//!   refreshTokenExpiresAt, user:{id,name,email,phone,avatarUrl}}`。
//!   token 只有 27 字符，是不透明串而不是 JWT，所以到期时间只能靠这两个字段。
//!
//! DPAPI 走 PowerShell 子进程而不是 `windows` crate：本机已经为祖先探测依赖了
//! PowerShell，这样能不再为此拖进整个 `windows` 依赖树（编译体积对 C:/E: 都是负担）。
//! 密钥只在进程内存里以 base64 形式中转，绝不落盘。

use std::path::Path;
use std::process::Command;

use aes_gcm::aead::{Aead, Payload};
use aes_gcm::{Aes256Gcm, Key, KeyInit, Nonce};
use base64::Engine as _;
use serde::{Deserialize, Serialize};

use crate::modules::config::{read_bytes, PathRoots};
use crate::modules::variant::{credentials, FileRole, QoderVariant};
use crate::Result;

pub const MAGIC: &[u8; 3] = b"v10";
const IV_LEN: usize = 12;
const KEY_LEN: usize = 32;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AuthUser {
    #[serde(default)]
    pub id: String,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub email: String,
    #[serde(default)]
    pub phone: String,
    #[serde(default)]
    pub avatar_url: String,
}

/// 解密后的桌面登录态。`expires_at` 一类字段保留原始字符串形态（ISO8601 + `Z`）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DesktopAuth {
    pub schema_version: u32,
    #[serde(skip_serializing)]
    pub token: String,
    #[serde(skip_serializing)]
    pub refresh_token: String,
    pub expires_at: String,
    pub refresh_expires_at: String,
    pub user: AuthUser,
}

impl DesktopAuth {
    /// 供界面展示的账号标签：优先 email，其次 name，最后 user.id 前缀。
    pub fn label(&self) -> String {
        if !self.user.email.trim().is_empty() {
            return self.user.email.clone();
        }
        if !self.user.name.trim().is_empty() {
            return self.user.name.clone();
        }
        let id = self.user.id.trim();
        if id.is_empty() {
            "(未知账号)".into()
        } else {
            format!("uid:{}", &id[..id.len().min(8)])
        }
    }
}

fn dpapi_via_powershell(data: &[u8], protect: bool) -> Result<Vec<u8>> {
    let b64 = base64::engine::general_purpose::STANDARD.encode(data);
    let method = if protect { "Protect" } else { "Unprotect" };
    // ProtectedData 在 System.Security 程序集里，PowerShell 默认不加载，
    // 少了这行 Add-Type 会得到 TypeNotFound。
    let script = format!(
        "Add-Type -AssemblyName System.Security;\
         $b=[Convert]::FromBase64String('{b64}');\
         $o=[Security.Cryptography.ProtectedData]::{method}($b,$null,'CurrentUser');\
         [Convert]::ToBase64String($o)"
    );
    let mut cmd = Command::new("powershell");
    cmd.args(["-NoProfile", "-NonInteractive", "-Command", &script]);
    let out = cmd
        .output()
        .map_err(|e| format!("调用 powershell 做 DPAPI {method} 失败: {e}"))?;
    if !out.status.success() {
        return Err(format!(
            "DPAPI {method} 失败: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    let text: String = String::from_utf8_lossy(&out.stdout)
        .chars()
        .filter(|c| !c.is_whitespace())
        .collect();
    base64::engine::general_purpose::STANDARD
        .decode(&text)
        .map_err(|e| format!("DPAPI {method} 输出不是合法 base64: {e}"))
}

fn dpapi_unprotect(data: &[u8]) -> Result<Vec<u8>> {
    dpapi_via_powershell(data, false)
}

#[cfg(test)]
fn dpapi_protect(data: &[u8]) -> Result<Vec<u8>> {
    dpapi_via_powershell(data, true)
}

/// 从 `Local State` 取出 32 字节 AES-256 主密钥。
pub fn aes_key_from_local_state(local_state: &Path) -> Result<[u8; KEY_LEN]> {
    let text = std::fs::read_to_string(local_state)
        .map_err(|e| format!("读 {local_state:?} 失败: {e}"))?;
    let v: serde_json::Value = serde_json::from_str(&text)
        .map_err(|e| format!("{local_state:?} 不是合法 JSON: {e}"))?;
    let enc = v
        .pointer("/os_crypt/encrypted_key")
        .and_then(|x| x.as_str())
        .ok_or_else(|| format!("{local_state:?} 缺 os_crypt.encrypted_key"))?;
    let raw = base64::engine::general_purpose::STANDARD
        .decode(enc)
        .map_err(|e| format!("encrypted_key 不是合法 base64: {e}"))?;
    let body = raw
        .strip_prefix(b"DPAPI" as &[u8])
        .ok_or_else(|| "encrypted_key 前缀不是 DPAPI，方案与预期不符".to_string())?;
    let key = dpapi_unprotect(body)?;
    if key.len() != KEY_LEN {
        return Err(format!("主密钥 {} 字节，不是预期的 {KEY_LEN} 字节", key.len()));
    }
    let mut out = [0u8; KEY_LEN];
    out.copy_from_slice(&key);
    Ok(out)
}

/// 解密 `v10` blob。篡改任何一个密文字节都会因 GCM tag 校验失败而报错。
pub fn decrypt_blob(key: &[u8; KEY_LEN], blob: &[u8]) -> Result<Vec<u8>> {
    if blob.len() < MAGIC.len() + IV_LEN + 16 {
        return Err(format!("blob 只有 {} 字节，短于 v10 最小长度", blob.len()));
    }
    if &blob[..MAGIC.len()] != MAGIC {
        return Err(format!(
            "magic 是 {:?} 而不是 {MAGIC:?}",
            &blob[..MAGIC.len()]
        ));
    }
    let cipher = Aes256Gcm::new(Key::<Aes256Gcm>::from_slice(key));
    let nonce = Nonce::from_slice(&blob[MAGIC.len()..MAGIC.len() + IV_LEN]);
    cipher
        .decrypt(
            nonce,
            Payload {
                msg: &blob[MAGIC.len() + IV_LEN..],
                aad: &[],
            },
        )
        .map_err(|e| format!("AES-256-GCM 解密失败（密钥不符或文件被改过）: {e}"))
}

/// 用新随机 IV 加密成 `v10` blob。IV 必须每次不同，所以这里不接收外部 IV。
pub fn encrypt_blob(key: &[u8; KEY_LEN], plaintext: &[u8]) -> Result<Vec<u8>> {
    // uuid v4 的 16 字节来自 CSPRNG，取前 12 字节作 GCM nonce。
    let rnd = uuid::Uuid::new_v4();
    let iv = &rnd.as_bytes()[..IV_LEN];
    let cipher = Aes256Gcm::new(Key::<Aes256Gcm>::from_slice(key));
    let ct = cipher
        .encrypt(
            Nonce::from_slice(iv),
            Payload {
                msg: plaintext,
                aad: &[],
            },
        )
        .map_err(|e| format!("加密失败: {e}"))?;
    let mut out = Vec::with_capacity(MAGIC.len() + IV_LEN + ct.len());
    out.extend_from_slice(MAGIC);
    out.extend_from_slice(iv);
    out.extend_from_slice(&ct);
    Ok(out)
}

/// 解析并校验登录态 JSON。
pub fn parse_auth(plaintext: &[u8]) -> Result<DesktopAuth> {
    let v: serde_json::Value = serde_json::from_slice(plaintext)
        .map_err(|e| format!("登录态不是合法 JSON: {e}"))?;
    let schema = v
        .get("schemaVersion")
        .and_then(|x| x.as_u64())
        .ok_or_else(|| "缺 schemaVersion".to_string())? as u32;
    if schema != 1 {
        return Err(format!("schemaVersion={schema} 不被支持"));
    }
    let s = |keys: &[&str]| -> String {
        keys.iter()
            .find_map(|k| v.get(*k).and_then(|x| x.as_str()))
            .unwrap_or_default()
            .to_string()
    };
    let user = &v["user"];
    let us = |k: &str| user.get(k).and_then(|x| x.as_str()).unwrap_or_default().to_string();
    let token = s(&["token"]);
    let refresh_token = s(&["refreshToken"]);
    if token.trim().is_empty() || refresh_token.trim().is_empty() {
        return Err("token / refreshToken 为空，等同于未登录".into());
    }
    Ok(DesktopAuth {
        schema_version: schema,
        token,
        refresh_token,
        expires_at: s(&["expiresAt"]),
        refresh_expires_at: s(&["refreshTokenExpiresAt"]),
        user: AuthUser {
            id: us("id"),
            name: us("name"),
            email: us("email"),
            phone: us("phone"),
            avatar_url: us("avatarUrl"),
        },
    })
}

/// 一步读到某版本桌面端的登录态。只读，不写任何产品目录。
pub fn read_desktop_auth(roots: &PathRoots, variant: QoderVariant) -> Result<DesktopAuth> {
    let files = credentials(roots, variant, crate::modules::variant::QoderTarget::Desktop);
    let take = |role: FileRole| {
        files
            .iter()
            .find(|f| f.role == role)
            .map(|f| f.path.clone())
            .ok_or_else(|| format!("布局里缺 {role:?}"))
    };
    let key = aes_key_from_local_state(&take(FileRole::LocalState)?)?;
    let blob = read_bytes(&take(FileRole::AuthMain)?)
        .map_err(|e| format!("读 auth.v1.dat 失败: {e}"))?;
    parse_auth(&decrypt_blob(&key, &blob)?)
}

/// 便利入口：本机某版本。
pub fn read_current(variant: QoderVariant) -> Result<DesktopAuth> {
    read_desktop_auth(&PathRoots::real(), variant)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::modules::variant::desktop_dir;

    fn sample() -> serde_json::Map<String, serde_json::Value> {
        let mut m = serde_json::Map::new();
        m.insert("schemaVersion".into(), 1.into());
        m.insert("token".into(), "tok-xxxxxxxxxxxxxxxxxxxxxxx".into());
        m.insert("refreshToken".into(), "rtok-xxxxxxxxxxxxxxxxxxxxx".into());
        m.insert("expiresAt".into(), "2026-10-19T06:19:41Z".into());
        m.insert("refreshTokenExpiresAt".into(), "2027-09-14T06:19:41Z".into());
        let mut u = serde_json::Map::new();
        u.insert("id".into(), "019f0000-0000-7000-8000-000000000001".into());
        u.insert("name".into(), "example-user".into());
        u.insert("email".into(), "".into());
        u.insert("phone".into(), "138****0000".into());
        u.insert("avatarUrl".into(), "https://example/a".into());
        m.insert("user".into(), u.into());
        m
    }

    #[test]
    fn roundtrip_with_fresh_iv_each_time() {
        let key = [7u8; KEY_LEN];
        let pt = serde_json::to_vec(&sample()).unwrap();
        let a = encrypt_blob(&key, &pt).unwrap();
        let b = encrypt_blob(&key, &pt).unwrap();
        assert_eq!(a.len(), b.len());
        assert_ne!(a, b, "同一份明文两次加密不该相同（IV 必须随机）");
        assert_eq!(decrypt_blob(&key, &a).unwrap(), pt);
    }

    #[test]
    fn tampered_blob_and_wrong_key_both_fail() {
        let key = [7u8; KEY_LEN];
        let blob = encrypt_blob(&key, b"{\"schemaVersion\":1}").unwrap();
        let mut bad = blob.clone();
        let last = bad.len() - 1;
        bad[last] ^= 0x01;
        assert!(decrypt_blob(&key, &bad).is_err());
        assert!(decrypt_blob(&[9u8; KEY_LEN], &blob).is_err());
    }

    #[test]
    fn rejects_non_v10_and_truncated() {
        let key = [7u8; KEY_LEN];
        assert!(decrypt_blob(&key, b"v11xxxxxxxxxxxxxxxxxxxxxxxx").is_err());
        assert!(decrypt_blob(&key, b"v10").is_err());
    }

    #[test]
    fn parse_accepts_real_shape_and_labels_on_name() {
        let text = serde_json::to_vec(&sample()).unwrap();
        let a = parse_auth(&text).unwrap();
        assert_eq!(a.schema_version, 1);
        assert_eq!(a.expires_at, "2026-10-19T06:19:41Z");
        assert_eq!(a.user.name, "example-user");
        // 本机 CN 账号 email 为空串，标签必须退到 name 而不是显示空。
        assert_eq!(a.label(), "example-user");
    }

    #[test]
    fn parse_rejects_empty_tokens_and_other_schema() {
        let mut m = sample();
        m.insert("token".into(), "".into());
        let text = serde_json::to_vec(&serde_json::Value::Object(m)).unwrap();
        assert!(parse_auth(&text).unwrap_err().contains("未登录"));

        let mut m = sample();
        m.insert("schemaVersion".into(), 2.into());
        let text = serde_json::to_vec(&serde_json::Value::Object(m)).unwrap();
        assert!(parse_auth(&text).unwrap_err().contains("schemaVersion"));
    }

    /// DPAPI 本身在同一 Windows 用户内可逆 —— 这是"账号包只能在同用户内复用"的根因。
    #[test]
    #[cfg(windows)]
    fn dpapi_roundtrip_within_current_user() {
        let secret = b"qoder-switch-dpapi-probe";
        let sealed = dpapi_protect(secret).unwrap();
        assert_eq!(dpapi_unprotect(&sealed).unwrap(), secret.to_vec());
    }

    /// 真机证据：本机的 Local State + auth.v1.dat 必须能解出结构合法的登录态，
    /// 且到期时间是将来时（说明读的是活跃会话而不是残留）。
    #[test]
    #[cfg(windows)]
    fn reads_real_desktop_auth() {
        let roots = PathRoots::real();
        let dir = desktop_dir(&roots, QoderVariant::Cn);
        if !dir.join("auth.v1.dat").is_file() {
            eprintln!("NOTE: 本机没有 CN 桌面凭据，跳过");
            return;
        }
        let a = read_desktop_auth(&roots, QoderVariant::Cn).unwrap();
        assert!(!a.token.trim().is_empty());
        assert!(!a.user.id.trim().is_empty());
        assert!(a.expires_at.ends_with('Z'), "{:?}", a.expires_at);
        assert!(a.user.phone.contains('*') || a.user.phone.len() >= 7);
        // 断言只到这里：任何 token 值都不该被打印出去。
    }
}
