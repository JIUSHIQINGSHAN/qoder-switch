//! Qoder OAuth 设备码登录流程。
//!
//! 取证自 10router（techysy/10router）：
//! - 授权落地页：`https://qoder.cn/device/selectAccounts` 或 `https://qoder.com/device/selectAccounts`
//! - 轮询端点：`GET /api/v1/deviceToken/poll?nonce=...&verifier=...&challenge_method=S256`
//! - 用户信息：`GET /api/v1/userinfo`

use std::collections::HashMap;
use std::path::Path;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use base64::Engine as _;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use crate::modules::auth_codec;
use crate::modules::bundle;
use crate::modules::config::{now_ts, PathRoots};
use crate::modules::variant::{desktop_dir, FileRole, QoderTarget, QoderVariant};

struct OAuthSession {
    variant: QoderVariant,
    nonce: String,
    verifier: String,
    machine_id: String,
    created_at: Instant,
}

static SESSIONS: Mutex<Option<HashMap<String, OAuthSession>>> = Mutex::new(None);

fn sessions_map() -> &'static Mutex<Option<HashMap<String, OAuthSession>>> {
    &SESSIONS
}

fn base64_url(bytes: &[u8]) -> String {
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes)
}

fn pkce_challenge(verifier: &str) -> String {
    let mut h = Sha256::new();
    h.update(verifier.as_bytes());
    base64_url(&h.finalize())
}

/// 从服务端 `user_id` 取一段安全前缀作为包名后缀。
///
/// **按字符取，不按字节**：`&id[..8]` 在 id 含多字节字符时会切在 UTF-8 边界内部
/// 直接 panic（`auth_codec::DesktopAuth::label` 修过同一个坑）。这里再过滤成
/// `[A-Za-z0-9_-]`，顺带满足 `validate_account_id` 的白名单。
fn user_id_prefix(user_id: &str) -> String {
    let clean: String = user_id
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '_')
        .take(8)
        .collect();
    if clean.is_empty() {
        "user".to_string()
    } else {
        clean
    }
}

pub fn login_base_url(variant: QoderVariant) -> &'static str {
    match variant {
        QoderVariant::Cn => "https://qoder.cn/device/selectAccounts",
        QoderVariant::Global => "https://qoder.com/device/selectAccounts",
    }
}

pub fn openapi_base_url(variant: QoderVariant) -> &'static str {
    match variant {
        QoderVariant::Cn => "https://openapi.qoder.com.cn",
        QoderVariant::Global => "https://openapi.qoder.sh",
    }
}

/// 发起 OAuth 登录：生成 PKCE 对，返回登录 URL 和内部轮询 ID。
pub fn oauth_start(variant: QoderVariant) -> Value {
    let mut rnd_bytes = [0u8; 32];
    for b in &mut rnd_bytes {
        *b = (uuid::Uuid::new_v4().as_u128() & 0xFF) as u8;
    }
    let verifier = base64_url(&rnd_bytes);
    let challenge = pkce_challenge(&verifier);
    let nonce = uuid::Uuid::new_v4().to_string();
    let machine_id = uuid::Uuid::new_v4().to_string();

    let landing_url = format!(
        "{}?challenge={}&challenge_method=S256&machine_id={}&nonce={}",
        login_base_url(variant),
        challenge,
        machine_id,
        nonce
    );

    let login_id = uuid::Uuid::new_v4().to_string();

    let mut lock = sessions_map().lock().unwrap();
    let map = lock.get_or_insert_with(HashMap::new);

    // 清理 10 分钟前的过期会话
    map.retain(|_, s| s.created_at.elapsed() < Duration::from_secs(600));

    map.insert(
        login_id.clone(),
        OAuthSession {
            variant,
            nonce,
            verifier,
            machine_id,
            created_at: Instant::now(),
        },
    );

    json!({
        "loginId": login_id,
        "verificationUri": landing_url,
        "expiresIn": 300
    })
}

/// 轮询一次 OAuth 登录状态。若用户已在浏览器完成授权，自动采集凭据入库。
pub async fn oauth_status(login_id: &str, roots: &PathRoots, store: &Path) -> Value {
    let session_opt = {
        let mut lock = sessions_map().lock().unwrap();
        let map = match lock.as_mut() {
            Some(m) => m,
            None => return json!({ "done": true, "error": "登录会话已过期" }),
        };
        map.get(login_id).map(|s| (s.variant, s.nonce.clone(), s.verifier.clone(), s.machine_id.clone()))
    };

    let (variant, nonce, verifier, machine_id) = match session_opt {
        Some(s) => s,
        None => return json!({ "done": true, "error": "未找到对应的登录会话，请重新发起" }),
    };

    let base = openapi_base_url(variant);
    let poll_url = format!(
        "{}/api/v1/deviceToken/poll?nonce={}&verifier={}&challenge_method=S256",
        base,
        urlencoding::encode(&nonce),
        urlencoding::encode(&verifier)
    );

    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(10))
        .build()
        .unwrap_or_default();

    let resp = client
        .get(&poll_url)
        .header("Accept", "application/json")
        .header("User-Agent", "Qoder")
        .send()
        .await;

    let (status, text) = match resp {
        Ok(res) => {
            let st = res.status().as_u16();
            let body = res.text().await.unwrap_or_default();
            (st, body)
        }
        Err(e) => return json!({ "done": false, "error": format!("网络请求失败: {e}") }),
    };

    // 202 或 404 说明用户尚未在浏览器授权，继续等待
    if status == 202 || status == 404 {
        return json!({ "done": false });
    }

    if status != 200 {
        return json!({
            "done": true,
            "error": format!("授权服务返回异常 (HTTP {status}): {text}")
        });
    }

    let poll_body: Value = match serde_json::from_str(&text) {
        Ok(b) => b,
        Err(e) => return json!({ "done": true, "error": format!("解析授权响应失败: {e}") }),
    };

    let token = match poll_body.get("token").and_then(|v| v.as_str()) {
        Some(t) if !t.is_empty() => t.to_string(),
        _ => return json!({ "done": true, "error": "授权成功但未返回有效 token" }),
    };

    let refresh_token = poll_body
        .get("refresh_token")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();

    let user_id = poll_body
        .get("user_id")
        .and_then(|v| v.as_str())
        .unwrap_or("unknown-user")
        .to_string();

    let expires_at = poll_body
        .get("expires_at")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
        .unwrap_or_else(|| {
            let in_secs = poll_body.get("expires_in").and_then(|v| v.as_i64()).unwrap_or(30 * 86400);
            (chrono::Utc::now() + chrono::Duration::seconds(in_secs)).to_rfc3339()
        });

    // 拉取用户信息（name / email）
    let userinfo_url = format!("{base}/api/v1/userinfo");
    let mut name = String::new();
    let mut email = String::new();
    if let Ok(ures) = client
        .get(&userinfo_url)
        .header("Authorization", format!("Bearer {token}"))
        .header("Accept", "application/json")
        .header("User-Agent", "Qoder")
        .send()
        .await
    {
        if ures.status().is_success() {
            if let Ok(ubody) = ures.json::<Value>().await {
                if let Some(n) = ubody.get("name").or_else(|| ubody.get("username")).and_then(|v| v.as_str()) {
                    name = n.trim().to_string();
                }
                if let Some(em) = ubody.get("email").and_then(|v| v.as_str()) {
                    email = em.trim().to_string();
                }
            }
        }
    }

    // 存入本地账号库
    let local_state_path = desktop_dir(roots, variant).join("Local State");
    let key = match auth_codec::aes_key_from_local_state(&local_state_path) {
        Ok(k) => k,
        Err(e) => return json!({ "done": true, "error": format!("无法读取本机加密主密钥: {e}") }),
    };

    let auth_doc = json!({
        "schemaVersion": 1,
        "token": token,
        "refreshToken": refresh_token,
        "expiresAt": expires_at,
        "refreshTokenExpiresAt": expires_at,
        "user": {
            "id": user_id,
            "name": if name.is_empty() { "Qoder用户".to_string() } else { name.clone() },
            "email": email,
            "phone": "",
            "avatarUrl": ""
        }
    });

    let auth_bytes = serde_json::to_vec(&auth_doc).unwrap_or_default();
    let encrypted_auth = match auth_codec::encrypt_blob(&key, &auth_bytes) {
        Ok(b) => b,
        Err(e) => return json!({ "done": true, "error": format!("加密登录态失败: {e}") }),
    };

    let account_id = if !email.is_empty() {
        let clean: String = email.chars().filter(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '_').collect();
        if clean.is_empty() {
            // 非 ASCII 邮箱（如 用户@例子.中国）过滤后可能为空 —— 退回 user_id 前缀，
            // 否则所有这类账号都会塌成同一个 "oauth-" 目录互相覆盖。
            format!("oauth-{}", user_id_prefix(&user_id))
        } else {
            let prefix: String = clean.chars().take(16).collect();
            format!("oauth-{prefix}")
        }
    } else {
        format!("oauth-{}", user_id_prefix(&user_id))
    };

    let dir = bundle::bundle_dir_in(store, &account_id, variant, QoderTarget::Desktop);
    if let Err(e) = std::fs::create_dir_all(&dir) {
        return json!({ "done": true, "error": format!("创建账号目录失败: {e}") });
    }

    // 自造的登录态按 capture 的扁平命名（authmain）落盘，并登记 sha256 ——
    // restore 读包只认 member.file_name 与 member.sha256，文件名一错就整包作废。
    let mut members: Vec<bundle::Member> = Vec::new();
    if let Err(e) = bundle::stage_member(&dir, &mut members, FileRole::AuthMain, true, &encrypted_auth) {
        return json!({ "done": true, "error": format!("写入 auth 文件失败: {e}") });
    }

    // 补齐现场存在的 critical 成员（Local State / profile-overlays）。缺了它们，
    // restore 的覆盖性检查会以"防半换号"为由整组拒写 —— 账号建得出来却切不过去。
    if let Err(e) = bundle::collect_live_critical_members(
        roots,
        &dir,
        &mut members,
        variant,
        QoderTarget::Desktop,
    ) {
        return json!({ "done": true, "error": format!("收集现场凭据失败: {e}") });
    }

    // machine-id 非 critical，restore 不强制；但它决定解密密钥的可用性，
    // 一并收进包里（失败不致命，缺了也不影响覆盖性检查）。
    if let Err(e) = bundle::stage_member(
        &dir,
        &mut members,
        FileRole::DesktopMachineId,
        false,
        machine_id.as_bytes(),
    ) {
        let _ = e; // 非关键文件，静默继续
    }

    // 写入 bundle.json
    let nb = bundle::Bundle {
        account_id: account_id.clone(),
        variant,
        target: QoderTarget::Desktop,
        created_at: now_ts(),
        members,
        identity: bundle::Identity {
            name: Some(name.clone()),
            email: if email.is_empty() { None } else { Some(email.clone()) },
            plan: Some("Free".into()),
            product: Some("qoder".into()),
            logged_in: Some(true),
            snapshot_at: Some(now_ts()),
            uid: Some(user_id.clone()),
            expires_at: Some(expires_at.clone()),
            refresh_expires_at: Some(expires_at.clone()),
            proxy: None,
        },
    };


    if let Err(e) = bundle::write_meta(store, &nb) {
        return json!({ "done": true, "error": format!("写入 bundle 元数据失败: {e}") });
    }

    // 移除已完成的会话
    {
        let mut lock = sessions_map().lock().unwrap();
        if let Some(m) = lock.as_mut() {
            m.remove(login_id);
        }
    }

    let meta = crate::modules::view::account_meta(&nb);
    json!({
        "done": true,
        "result": meta
    })
}

pub fn oauth_status_sync(login_id: &str, roots: &PathRoots, store: &Path) -> Value {
    if let Ok(handle) = tokio::runtime::Handle::try_current() {
        tokio::task::block_in_place(|| handle.block_on(oauth_status(login_id, roots, store)))
    } else {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(oauth_status(login_id, roots, store))
    }
}

mod urlencoding {
    pub fn encode(s: &str) -> String {
        let mut out = String::new();
        for b in s.bytes() {
            if b.is_ascii_alphanumeric() || b == b'-' || b == b'_' || b == b'.' || b == b'~' {
                out.push(b as char);
            } else {
                out.push_str(&format!("%{:02X}", b));
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn oauth_start_generates_valid_url_and_session() {
        let res_cn = oauth_start(QoderVariant::Cn);
        assert!(res_cn["loginId"].is_string());
        let uri_cn = res_cn["verificationUri"].as_str().unwrap();
        assert!(uri_cn.starts_with("https://qoder.cn/device/selectAccounts?challenge="));
        assert!(uri_cn.contains("challenge_method=S256"));
        assert!(uri_cn.contains("nonce="));

        let res_global = oauth_start(QoderVariant::Global);
        let uri_global = res_global["verificationUri"].as_str().unwrap();
        assert!(uri_global.starts_with("https://qoder.com/device/selectAccounts?challenge="));
    }

    /// 服务端 `user_id` 可能是任意字符串：既不能按字节切片 panic（多字节字符），
    /// 也不能让非法字符混进包名（会过不了 validate_account_id）。
    #[test]
    fn user_id_prefix_is_byte_safe_and_path_safe() {
        // 多字节：旧写法 &id[..8] 会切在 UTF-8 边界内部 panic。
        let p = user_id_prefix("用户12345678");
        assert!(p.is_ascii(), "前缀必须全是 ASCII: {p:?}");
        assert!(p.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_'));

        // 全非法字符（例如纯中文 id）不能产出空串，否则包名塌成 "oauth-" 互相覆盖。
        assert_eq!(user_id_prefix("全部是中文"), "user");

        // 正常内容原样取前 8。
        assert_eq!(user_id_prefix("abc123XYZ-tail"), "abc123XY");

        // 拼出的包名必须能过账号名校验。
        let id = format!("oauth-{}", user_id_prefix("用户12345678"));
        assert!(crate::modules::bundle::validate_account_id(&id).is_ok(), "{id}");
    }
}
