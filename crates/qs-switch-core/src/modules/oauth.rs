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
use crate::modules::config::{atomic_write_bytes, now_ts, sha256_hex_bytes, PathRoots};
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
        format!("oauth-{}", &clean[..clean.len().min(16)])
    } else {
        format!("oauth-{}", &user_id[..user_id.len().min(8)])
    };

    let dir = bundle::bundle_dir_in(store, &account_id, variant, QoderTarget::Desktop);
    if let Err(e) = std::fs::create_dir_all(&dir) {
        return json!({ "done": true, "error": format!("创建账号目录失败: {e}") });
    }

    // 写入 auth.v1.dat
    if let Err(e) = atomic_write_bytes(&dir.join("auth.v1.dat"), &encrypted_auth) {
        return json!({ "done": true, "error": format!("写入 auth 文件失败: {e}") });
    }

    // 写入 Local State 副本
    if let Ok(ls_bytes) = std::fs::read(&local_state_path) {
        let _ = atomic_write_bytes(&dir.join("local_state"), &ls_bytes);
    }

    // 写入 machine-id
    let machine_id_bytes = machine_id.as_bytes();
    let _ = atomic_write_bytes(&dir.join("auth.machine-id"), machine_id_bytes);

    // 写入 bundle.json
    let nb = bundle::Bundle {
        account_id: account_id.clone(),
        variant,
        target: QoderTarget::Desktop,
        created_at: now_ts(),
        members: vec![
            bundle::Member {
                role: FileRole::AuthMain,
                file_name: "auth.v1.dat".into(),
                sha256: sha256_hex_bytes(&encrypted_auth),
                size: encrypted_auth.len() as u64,
                critical: true,
            }
        ],
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
}
