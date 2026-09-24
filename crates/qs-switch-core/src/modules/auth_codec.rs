//! Qoder 桌面凭据文件的编解码。
//!
//! **Windows** 实测方案（2026-09-20 本机验证）：
//! - `%APPDATA%\<app>\Local State` 的 `os_crypt.encrypted_key` 是
//!   base64(`"DPAPI"` + DPAPI blob)，DPAPI(CURRENT_USER) 解出来是 **32 字节** AES-256 密钥。
//! - `auth.v1.dat` = `b"v10"` + 12 字节 IV + AES-256-GCM 密文（末尾 16 字节 tag）。
//!
//! **macOS** 实测方案（2026-09-23，macOS 15.6.1 / arm64，`Qoder CN.app` 0.3.4）：
//! - `Local State` 只有 57 字节、内容是 `{"uninstall_metrics":…}` —— **没有
//!   `os_crypt`**，主密钥不在文件里。
//! - 主密钥口令在**登录钥匙串**：国内版 `svce="Qoder CN App Safe Storage"` /
//!   `acct="Qoder CN App Key"`（实测 24 字符口令），国际版 `svce="Qoder Safe Storage"` /
//!   `acct="Qoder Key"`。
//! - `auth.v1.dat` = `b"v10"` + **AES-128-CBC** 密文，
//!   key = `PBKDF2-HMAC-SHA1(口令, salt="saltysalt", iter=1003, len=16)`，
//!   **IV 是固定 16 字节 0x20、不写进文件**，PKCS7 填充。这正是 Chromium/Electron
//!   在 macOS 上 safeStorage 的标准方案（Windows 侧则是 DPAPI+GCM，两套并存）。
//!   实测 403 字节样本 = 3 + 400，解出 384 字节明文。
//!
//! 两侧明文 JSON 完全同形：`{schemaVersion:1, token, refreshToken, expiresAt,
//! refreshTokenExpiresAt, user:{id,name,email,phone,avatarUrl}}`。token 只有 27 字符，
//! 是不透明串而不是 JWT，所以到期时间只能靠那两个字段 —— `parse_auth` 因此无需分平台。
//!
//! 系统调用一律走子进程而不是引入平台 crate（Windows 用 PowerShell 做 DPAPI，macOS 用
//! `/usr/bin/security` 读钥匙串），这样不必为一次调用拖进整个 `windows` 或
//! `security-framework` 依赖树。密钥只在进程内存里中转，绝不落盘、绝不打印。
//!
//! **一个 macOS 特有的代价**：钥匙串条目由 Qoder 创建，本工具去读会触发系统授权弹窗
//! （一次性的，用户点「始终允许」后不再打扰）。所以派生结果按版本缓存在进程内，
//! 避免每次刷新状态都弹一次框。

use std::path::Path;
use std::process::Command;

use aes_gcm::aead::{Aead, Payload};
use aes_gcm::{Aes256Gcm, Key, KeyInit, Nonce};
use base64::Engine as _;
use serde::{Deserialize, Serialize};

use crate::modules::config::{read_bytes, PathRoots};
use crate::modules::variant::{credentials, FileRole, QoderVariant, QoderTarget};
use crate::Result;

pub const MAGIC: &[u8; 3] = b"v10";
const IV_LEN: usize = 12;
const KEY_LEN: usize = 32;

/// macOS 分支的参数（2026-09-23 实测命中，见模块头）。
#[cfg(target_os = "macos")]
pub const MAC_KEY_LEN: usize = 16;
#[cfg(target_os = "macos")]
const MAC_SALT: &[u8] = b"saltysalt";
#[cfg(target_os = "macos")]
const MAC_ITER: u32 = 1003;
/// 固定 IV：16 个 0x20（空格）。不存进 blob，所以加解密两侧都写死同一个值。
#[cfg(target_os = "macos")]
const MAC_IV: [u8; 16] = [0x20; 16];

/// 桌面主密钥。两个平台的**算法与长度都不同**，所以用类型把差异收在这里，
/// 而不是让调用方各自传 `&[u8]` 再猜长度 —— 长度猜错的后果是"解不开"，
/// 而"解不开"在认领路径上会被静默降级成明文回显，很难查。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MasterKey {
    /// Windows：Local State → DPAPI 解出的 32 字节密钥，blob 走 AES-256-GCM。
    Windows([u8; KEY_LEN]),
    /// macOS：钥匙串口令 → PBKDF2 派生的 16 字节密钥，blob 走 AES-128-CBC。
    #[cfg(target_os = "macos")]
    MacOS([u8; MAC_KEY_LEN]),
}

impl MasterKey {
    pub fn platform(&self) -> &'static str {
        match self {
            Self::Windows(_) => "windows",
            #[cfg(target_os = "macos")]
            Self::MacOS(_) => "macos",
        }
    }
}

/// 一句话讲清"账号包为什么可能解不开"。界面与错误信息共用，避免两处文案分叉。
///
/// 两个平台的限制**同源但不同形**：Windows 卡在 DPAPI 的 CURRENT_USER 作用域，
/// macOS 卡在登录钥匙串 —— 都是"密钥不随包走"，但换机/换用户的具体说法不一样，
/// 写成一句笼统的话会让人照着错误的方向去排查。
pub fn portability_hint() -> &'static str {
    if cfg!(target_os = "macos") {
        "macOS 的桌面登录态由本机登录钥匙串里的口令加密，账号包只能在同一台 mac 的同一个钥匙串内复用"
    } else {
        "Windows 的桌面登录态由 DPAPI(CURRENT_USER) 加密，账号包只能在同一个 Windows 用户内复用"
    }
}

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
            // 按字符取前缀，不按字节：uid 若含多字节字符，`&id[..8]` 可能切在
            // UTF-8 边界内部直接 panic（selfcheck 会连整份报告一起丢）。
            let prefix: String = id.chars().take(8).collect();
            format!("uid:{prefix}")
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
    // GUI 宿主里这条调用每次刷状态都会跑，放任它建控制台就是"终端一直闪"。
    crate::modules::process::hide_console(&mut cmd);
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

#[cfg(all(test, windows))]
fn dpapi_protect(data: &[u8]) -> Result<Vec<u8>> {
    dpapi_via_powershell(data, true)
}

/// 从 `Local State` 取出 32 字节 AES-256 主密钥。**这是 Windows 专属布局**。
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

/// macOS：从登录钥匙串读 safeStorage 口令，再 PBKDF2 派生出 AES-128 主密钥。
///
/// 走 `/usr/bin/security` 而不是 `security-framework` crate，与本模块读 DPAPI 用
/// PowerShell 是同一条取舍。口令按 (服务名, 账户名) 候选逐个试，第一个命中的即用；
/// 全落空时报错里带上试过的服务名，便于新装版本改了命名时定位。
#[cfg(target_os = "macos")]
pub fn mac_master_key_from_keychain(variant: QoderVariant) -> Result<[u8; MAC_KEY_LEN]> {
    let candidates =
        crate::modules::variant::mac_keychain_service_candidates(variant, QoderTarget::Desktop);
    if candidates.is_empty() {
        return Err(format!("{:?} 在 macOS 上没有已取证的钥匙串条目名", variant));
    }
    let mut last = String::new();
    for (svc, acct) in &candidates {
        // -g 之外的 -w 只输出口令本身；不打印，只进内存。
        let mut cmd = Command::new("/usr/bin/security");
        cmd.args(["find-generic-password", "-s", svc, "-a", acct, "-w"]);
        crate::modules::process::hide_console(&mut cmd);
        let out = match cmd.output() {
            Ok(o) => o,
            Err(e) => return Err(format!("调用 /usr/bin/security 失败: {e}")),
        };
        if !out.status.success() {
            last = format!(
                "{svc}/{acct}: {}",
                String::from_utf8_lossy(&out.stderr).trim()
            );
            continue;
        }
        // 口令是文本，尾随换行不属于口令本身（实测 25 字节含换行 → 24 字符）。
        let secret: Vec<u8> = out
            .stdout
            .iter()
            .copied()
            .rev()
            .skip_while(|b| *b == b'\n' || *b == b'\r')
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .collect();
        if secret.is_empty() {
            last = format!("{svc}/{acct}: 钥匙串口令为空");
            continue;
        }
        return Ok(derive_mac_key(&secret));
    }
    Err(format!("登录钥匙串里没有匹配的 safeStorage 条目（{last}）"))
}

/// PBKDF2-HMAC-SHA1(口令, "saltysalt", 1003) → 16 字节 AES-128 密钥。
#[cfg(target_os = "macos")]
fn derive_mac_key(secret: &[u8]) -> [u8; MAC_KEY_LEN] {
    let mut out = [0u8; MAC_KEY_LEN];
    pbkdf2::pbkdf2_hmac::<sha1::Sha1>(secret, MAC_SALT, MAC_ITER, &mut out);
    out
}

/// macOS 派生密钥的进程内缓存。
///
/// 为什么必须缓存：读别人 App 创建的钥匙串条目会触发系统授权弹窗，用户点「允许」
/// 只对这一次放行（点「始终允许」才永久放行，但不能假定用户会）。而
/// `read_desktop_auth` 在状态刷新里是被反复调的 —— 不缓存就等于每隔几秒弹一次框。
///
/// 为什么带 TTL 而不是永久缓存：Qoder 重装会换掉钥匙串口令，永久缓存会让本进程
/// 此后所有凭据读取**永久**失败且无从解释。10 分钟是自愈周期，也把弹窗频率封顶。
#[cfg(target_os = "macos")]
fn mac_master_key_cached(variant: QoderVariant) -> Result<[u8; MAC_KEY_LEN]> {
    use std::collections::HashMap;
    use std::sync::{Mutex, OnceLock};
    use std::time::{Duration, Instant};

    const TTL: Duration = Duration::from_secs(600);
    static CACHE: OnceLock<Mutex<HashMap<&'static str, ([u8; MAC_KEY_LEN], Instant)>>> =
        OnceLock::new();
    let cache = CACHE.get_or_init(|| Mutex::new(HashMap::new()));
    let slot = match variant {
        QoderVariant::Cn => "cn",
        QoderVariant::Global => "global",
    };
    // 锁中毒（另一线程在持锁时 panic）不该让整个凭据读取挂掉：退回直接重算。
    if let Ok(g) = cache.lock() {
        if let Some((key, at)) = g.get(slot) {
            if at.elapsed() < TTL {
                return Ok(*key);
            }
        }
    }
    let key = mac_master_key_from_keychain(variant)?;
    if let Ok(mut g) = cache.lock() {
        g.insert(slot, (key, Instant::now()));
    }
    Ok(key)
}

/// 本机某版本桌面端的 master key。
///
/// 两个平台的**密钥载体根本不同**：Windows 在账号目录里的 `Local State`（因此
/// 账号包自带一份、跨用户解不开），macOS 在钥匙串里（**机器级、按版本一份**，
/// 所有账号包共用同一把，所以 mac 上切号只需换 `auth.v1.dat`）。
pub fn master_key(roots: &PathRoots, variant: QoderVariant) -> Result<MasterKey> {
    #[cfg(target_os = "macos")]
    {
        let _ = roots;
        Ok(MasterKey::MacOS(mac_master_key_cached(variant)?))
    }
    #[cfg(not(target_os = "macos"))]
    {
        let files = credentials(roots, variant, QoderTarget::Desktop);
        let path = files
            .iter()
            .find(|f| f.role == FileRole::LocalState)
            .map(|f| f.path.clone())
            .ok_or_else(|| "布局里缺 LocalState".to_string())?;
        Ok(MasterKey::Windows(aes_key_from_local_state(&path)?))
    }
}

/// 解出某账号包目录里的桌面登录态（quota / 签到 / 轮换共用的权威入口）。
///
/// Windows 上密钥必须来自**包内**那份 `Local State`（跨用户解不开是特性不是 bug）；
/// macOS 上包内根本没有密钥，改用本机钥匙串。历史包有三种凭据命名与两种 key 命名，
/// 兼容逻辑保留原样。
pub fn decrypt_bundle_auth(
    dir: &Path,
    roots: &PathRoots,
    variant: QoderVariant,
) -> Result<DesktopAuth> {
    let auth_path = ["auth_main", "authmain", "auth.v1.dat"]
        .iter()
        .map(|n| dir.join(n))
        .find(|p| p.is_file())
        .ok_or_else(|| "凭据文件不存在".to_string())?;
    let key = if cfg!(target_os = "macos") {
        master_key(roots, variant)?
    } else {
        let key_path = ["local_state", "localstate"]
            .iter()
            .map(|n| dir.join(n))
            .find(|p| p.is_file())
            .ok_or_else(|| "Local State 密钥文件不存在".to_string())?;
        MasterKey::Windows(
            aes_key_from_local_state(&key_path)
                .map_err(|e| format!("读取 Local State 密钥失败: {e}"))?,
        )
    };
    let blob = read_bytes(&auth_path).map_err(|e| format!("读取凭据文件失败: {e}"))?;
    parse_auth(&decrypt_blob(&key, &blob)?)
}

/// 解密 `v10` blob，按密钥所属平台走各自的方案。
///
/// Windows 的 GCM 带认证 tag，篡改任一密文字节都会报错；macOS 的 CBC **没有完整性
/// 保护**（Chromium 在 mac 上就是这样），所以那边只能靠"解出来是不是合法 JSON"兜底。
pub fn decrypt_blob(key: &MasterKey, blob: &[u8]) -> Result<Vec<u8>> {
    if &blob[..blob.len().min(MAGIC.len())] != MAGIC {
        return Err(format!(
            "magic 是 {:?} 而不是 {MAGIC:?}",
            &blob[..blob.len().min(MAGIC.len())]
        ));
    }
    match key {
        MasterKey::Windows(k) => {
            let ct = &blob[MAGIC.len()..];
            if ct.len() < IV_LEN + 16 {
                return Err(format!("blob 只有 {} 字节，短于 v10 最小长度", blob.len()));
            }
            let cipher = Aes256Gcm::new(Key::<Aes256Gcm>::from_slice(k));
            let nonce = Nonce::from_slice(&ct[..IV_LEN]);
            cipher
                .decrypt(
                    nonce,
                    Payload {
                        msg: &ct[IV_LEN..],
                        aad: &[],
                    },
                )
                .map_err(|e| format!("AES-256-GCM 解密失败（密钥不符或文件被改过）: {e}"))
        }
        #[cfg(target_os = "macos")]
        MasterKey::MacOS(k) => {
            use cbc::cipher::{block_padding::Pkcs7, BlockDecryptMut, KeyIvInit};
            let ct = &blob[MAGIC.len()..];
            if ct.is_empty() || ct.len() % 16 != 0 {
                return Err(format!(
                    "macOS 的 v10 密文必须是 16 字节块的整数倍，实际 {} 字节",
                    ct.len()
                ));
            }
            let dec = cbc::Decryptor::<aes::Aes128>::new(k.as_slice().into(), &MAC_IV.into());
            dec.decrypt_padded_vec_mut::<Pkcs7>(ct)
                .map_err(|e| format!("AES-128-CBC 解密失败（钥匙串口令不符或文件被改过）: {e}"))
        }
    }
}

/// 加密成 `v10` blob，与 `decrypt_blob` 严格同方案。
///
/// Windows 的 GCM nonce 必须每次不同，所以随机取；macOS 的 IV 是**协议规定的固定值**
/// 且不写进文件，因此那边同一份明文每次加密结果相同 —— 这是与 Qoder 自己写入的字节
/// 兼容的前提，不是缺陷。
pub fn encrypt_blob(key: &MasterKey, plaintext: &[u8]) -> Result<Vec<u8>> {
    match key {
        MasterKey::Windows(k) => {
            // uuid v4 的 16 字节来自 CSPRNG，取前 12 字节作 GCM nonce。
            let rnd = uuid::Uuid::new_v4();
            let iv = &rnd.as_bytes()[..IV_LEN];
            let cipher = Aes256Gcm::new(Key::<Aes256Gcm>::from_slice(k));
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
        #[cfg(target_os = "macos")]
        MasterKey::MacOS(k) => {
            use cbc::cipher::{block_padding::Pkcs7, BlockEncryptMut, KeyIvInit};
            let enc = cbc::Encryptor::<aes::Aes128>::new(k.as_slice().into(), &MAC_IV.into());
            let ct = enc.encrypt_padded_vec_mut::<Pkcs7>(plaintext);
            let mut out = Vec::with_capacity(MAGIC.len() + ct.len());
            out.extend_from_slice(MAGIC);
            out.extend_from_slice(&ct);
            Ok(out)
        }
    }
}

/// 解析并校验登录态 JSON。
pub fn parse_auth(plaintext: &[u8]) -> Result<DesktopAuth> {
    let v: serde_json::Value = serde_json::from_slice(plaintext)
        .map_err(|e| format!("登录态不是合法 JSON: {e}"))?;
    let schema = v
        .get("schemaVersion")
        .and_then(|x| x.as_u64())
        .ok_or_else(|| "缺 schemaVersion".to_string())?;
    // 直接按 u64 比较：先前 `as u32` 会把 2^32+1 截断成 1，让一个 schema 声明异常
    // 的包被当作受支持的 v1 接受（校验器语义错误）。
    if schema != 1 {
        return Err(format!("schemaVersion={schema} 不被支持"));
    }
    let schema = schema as u32;
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
    let uid = us("id");
    if uid.trim().is_empty() {
        return Err("登录态缺少有效 user.id，数据不完整".into());
    }
    Ok(DesktopAuth {
        schema_version: schema,
        token,
        refresh_token,
        expires_at: s(&["expiresAt"]),
        refresh_expires_at: s(&["refreshTokenExpiresAt"]),
        user: AuthUser {
            id: uid,
            name: us("name"),
            email: us("email"),
            phone: us("phone"),
            avatar_url: us("avatarUrl"),
        },
    })
}

/// 一步读到某版本桌面端的登录态。只读，不写任何产品目录。
pub fn read_desktop_auth(roots: &PathRoots, variant: QoderVariant) -> Result<DesktopAuth> {
    let files = credentials(roots, variant, QoderTarget::Desktop);
    let take = |role: FileRole| {
        files
            .iter()
            .find(|f| f.role == role)
            .map(|f| f.path.clone())
            .ok_or_else(|| format!("布局里缺 {role:?}"))
    };
    let key = master_key(roots, variant)?;
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
        let key = MasterKey::Windows([7u8; KEY_LEN]);
        let pt = serde_json::to_vec(&sample()).unwrap();
        let a = encrypt_blob(&key, &pt).unwrap();
        let b = encrypt_blob(&key, &pt).unwrap();
        assert_eq!(a.len(), b.len());
        assert_ne!(a, b, "同一份明文两次加密不该相同（GCM nonce 必须随机）");
        assert_eq!(decrypt_blob(&key, &a).unwrap(), pt);
    }

    #[test]
    fn tampered_blob_and_wrong_key_both_fail() {
        let key = MasterKey::Windows([7u8; KEY_LEN]);
        let blob = encrypt_blob(&key, b"{\"schemaVersion\":1}").unwrap();
        let mut bad = blob.clone();
        let last = bad.len() - 1;
        bad[last] ^= 0x01;
        assert!(decrypt_blob(&key, &bad).is_err());
        assert!(decrypt_blob(&MasterKey::Windows([9u8; KEY_LEN]), &blob).is_err());
    }

    #[test]
    fn rejects_non_v10_and_truncated() {
        let key = MasterKey::Windows([7u8; KEY_LEN]);
        assert!(decrypt_blob(&key, b"v11xxxxxxxxxxxxxxxxxxxxxxxx").is_err());
        assert!(decrypt_blob(&key, b"v10").is_err());
    }

    /// macOS 的 CBC 方案与 Windows 的 GCM 有**两处形状差异**，必须各自钉住：
    /// IV 固定且不写进文件（所以同一明文加密结果恒定），以及密文必须块对齐。
    #[cfg(target_os = "macos")]
    #[test]
    fn mac_cbc_roundtrip_is_deterministic_and_block_aligned() {
        let key = MasterKey::MacOS([7u8; MAC_KEY_LEN]);
        let pt = serde_json::to_vec(&sample()).unwrap();
        let a = encrypt_blob(&key, &pt).unwrap();
        assert_eq!(a, encrypt_blob(&key, &pt).unwrap(), "固定 IV 下两次加密必须一致");
        assert_eq!(&a[..3], MAGIC);
        assert_eq!((a.len() - MAGIC.len()) % 16, 0, "CBC 密文必须是 16 的整数倍");
        assert_eq!(decrypt_blob(&key, &a).unwrap(), pt);
        assert!(decrypt_blob(&MasterKey::MacOS([9u8; MAC_KEY_LEN]), &a).is_err());
        // 长度不对齐（比如被截断）要报错，不能把垃圾当明文交上去。
        assert!(decrypt_blob(&key, &a[..a.len() - 5]).is_err());
    }

    /// PBKDF2 参数写错的后果是"永远解不开"且没有任何提示，所以把实测向量钉成测试。
    #[cfg(target_os = "macos")]
    #[test]
    fn mac_kdf_params_match_measured_scheme() {
        assert_eq!(MAC_SALT, b"saltysalt");
        assert_eq!(MAC_ITER, 1003);
        assert_eq!(MAC_KEY_LEN, 16);
        assert_eq!(MAC_IV, [0x20; 16]);
        // RFC 6070 风格自查：同一口令两次派生必须一致，不同口令必须不同。
        assert_eq!(derive_mac_key(b"abc"), derive_mac_key(b"abc"));
        assert_ne!(derive_mac_key(b"abc"), derive_mac_key(b"abd"));
    }

    /// 真机证据（macOS）：本机钥匙串 + auth.v1.dat 必须解出结构合法的登录态。
    /// 断言只到"字段形状"为止，任何 token 值都不落进输出。
    #[cfg(target_os = "macos")]
    #[test]
    #[ignore = "需要本机装了 Qoder CN 并已登录，且会触发一次钥匙串授权弹窗"]
    fn reads_real_desktop_auth_on_macos() {
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
        assert_eq!(a.schema_version, 1);
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

        // 缺少或空 user.id 必须拒绝
        let mut m = sample();
        let mut u = serde_json::Map::new();
        u.insert("id".into(), "".into());
        m.insert("user".into(), u.into());
        let text = serde_json::to_vec(&serde_json::Value::Object(m)).unwrap();
        assert!(parse_auth(&text).unwrap_err().contains("user.id"));
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
