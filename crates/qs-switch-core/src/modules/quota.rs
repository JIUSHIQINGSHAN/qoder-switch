//! Qoder 配额/积分与每日签到模块。
//!
//! 基于 10router 取证成果与 Qoder 生产环境 OpenAPI：
//! - 配额与总览：`GET /api/v2/quota/usage`
//! - 活动与资源包：`GET /sash/api/v1/me/campaigns?clientType=10`
//! - 每日 Credits 领取（签到）：`POST /sash/api/v1/me/campaigns/{id}/claim`

use std::collections::HashMap;
use std::path::Path;

use serde_json::{json, Value};

use crate::modules::auth_codec;
use crate::modules::bundle;
use crate::modules::config::PathRoots;
use crate::modules::variant::{QoderTarget, QoderVariant};
use crate::Result;

pub fn openapi_base(variant: QoderVariant) -> &'static str {
    match variant {
        QoderVariant::Cn => "https://openapi.qoder.com.cn",
        QoderVariant::Global => "https://openapi.qoder.sh",
    }
}

/// 解析指定账号的有效 Bearer Token（优先读取账号包解密结果，次级读取现场文件）。
pub fn resolve_token(
    roots: &PathRoots,
    store: &Path,
    account_id: &str,
    variant: QoderVariant,
) -> Result<String> {
    // 1. 尝试从账号包中读取并解密
    let b = bundle::load(store, account_id, variant, QoderTarget::Desktop);
    if let Ok(bundle) = b {
        let dir = bundle.dir_in(store);
        let auth_path = dir.join("auth_main");
        let key_path = dir.join("local_state");
        if auth_path.is_file() && key_path.is_file() {
            if let Ok(key) = auth_codec::aes_key_from_local_state(&key_path) {
                if let Ok(blob) = std::fs::read(&auth_path) {
                    if let Ok(dec) = auth_codec::decrypt_blob(&key, &blob) {
                        if let Ok(auth) = auth_codec::parse_auth(&dec) {
                            if !auth.token.trim().is_empty() {
                                return Ok(auth.token);
                            }
                        }
                    }
                }
            }
        }
    }

    // 2. 尝试从当前桌面现场读取
    if let Ok(auth) = auth_codec::read_desktop_auth(roots, variant) {
        if !auth.token.trim().is_empty() {
            return Ok(auth.token);
        }
    }

    Err(format!("无法读取账号 {account_id} 的有效登录凭据"))
}

fn http_client() -> reqwest::Client {
    reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(15))
        .build()
        .unwrap_or_default()
}

fn build_headers(token: &str) -> HashMap<String, String> {
    let mut h = HashMap::new();
    h.insert("Authorization".into(), format!("Bearer {token}"));
    h.insert("Cosy-ClientType".into(), "10".into());
    h.insert("Cosy-Version".into(), "0.3.3".into());
    h.insert("Cosy-MachineOS".into(), "windows".into());
    h.insert("User-Agent".into(), "Qoder".into());
    h.insert("Accept".into(), "application/json".into());
    h
}

/// 查询账号的积分/配额详细情况（对齐前端 CreditExpiry 契约）。
pub async fn fetch_credit_expiry(
    roots: &PathRoots,
    store: &Path,
    account_id: &str,
    variant: QoderVariant,
) -> Value {
    let token = match resolve_token(roots, store, account_id, variant) {
        Ok(t) => t,
        Err(e) => return json!({ "ok": false, "error": e, "resources": [] }),
    };

    let base = openapi_base(variant);
    let client = http_client();
    let headers = build_headers(&token);

    // 1. 获取配额总览
    let quota_url = format!("{base}/api/v2/quota/usage");
    let mut req = client.get(&quota_url);
    for (k, v) in &headers {
        req = req.header(k, v);
    }
    let quota_val: Value = match req.send().await {
        Ok(res) if res.status().is_success() => res.json().await.unwrap_or_default(),
        Ok(res) => {
            return json!({
                "ok": false,
                "error": format!("配额查询失败 (HTTP {})", res.status()),
                "resources": []
            });
        }
        Err(e) => {
            return json!({
                "ok": false,
                "error": format!("网络请求失败: {e}"),
                "resources": []
            });
        }
    };

    // 2. 获取活动与资源包
    let camp_url = format!("{base}/sash/api/v1/me/campaigns?clientType=10");
    let mut req2 = client.get(&camp_url);
    for (k, v) in &headers {
        req2 = req2.header(k, v);
    }
    let camp_val: Value = match req2.send().await {
        Ok(res) if res.status().is_success() => res.json().await.unwrap_or_default(),
        _ => json!({}),
    };

    let user_quota = &quota_val["userQuota"];
    let add_on_quota = &quota_val["addOnQuota"];

    let user_total = user_quota.get("total").and_then(|v| v.as_f64()).unwrap_or(0.0);
    let user_rem = user_quota.get("remaining").and_then(|v| v.as_f64()).unwrap_or(0.0);
    let _user_used = user_quota.get("used").and_then(|v| v.as_f64()).unwrap_or(0.0);

    let add_on_total = add_on_quota.get("total").and_then(|v| v.as_f64()).unwrap_or(0.0);
    let add_on_rem = add_on_quota.get("remaining").and_then(|v| v.as_f64()).unwrap_or(0.0);
    let add_on_used = add_on_quota.get("used").and_then(|v| v.as_f64()).unwrap_or(0.0);

    let total_capacity = user_total + add_on_total;
    let total_remaining = user_rem + add_on_rem;

    // 解析资源包列表
    let mut resources = Vec::new();
    let campaigns = camp_val.get("campaigns").and_then(|v| v.as_array());

    let now_ms = chrono::Utc::now().timestamp_millis();
    let mut used_left = add_on_used;

    if let Some(list) = campaigns {
        for c in list {
            let claim_status = c.get("claimStatus").and_then(|v| v.as_str()).unwrap_or("");
            let benefit = &c["benefit"];
            let kind = benefit.get("kind").and_then(|v| v.as_str()).unwrap_or("");
            if claim_status == "CLAIMED" && kind == "CREDITS" {
                let amount = benefit.get("amount").and_then(|v| v.as_f64()).unwrap_or(0.0);
                let val_obj = &benefit["validity"];
                let mode = val_obj.get("mode").and_then(|v| v.as_str()).unwrap_or("");

                let expire_at_ms: Option<i64> = if mode == "FIXED_END" {
                    val_obj
                        .get("fixedEnd")
                        .and_then(|v| v.as_str())
                        .and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok())
                        .map(|dt| dt.timestamp_millis())
                } else if mode == "RELATIVE_DAYS" {
                    let days = val_obj.get("days").and_then(|v| v.as_i64()).unwrap_or(30);
                    let start_at = c.get("startAt").and_then(|v| v.as_i64()).unwrap_or(0);
                    if start_at > 0 {
                        Some(start_at * 1000 + days * 86_400_000)
                    } else {
                        None
                    }
                } else {
                    None
                };

                let pack_used = used_left.min(amount);
                used_left -= pack_used;
                let pack_rem = (amount - pack_used).max(0.0);

                let is_expired = expire_at_ms.map_or(false, |t| t <= now_ms);
                let is_expiring_soon = expire_at_ms
                    .map_or(false, |t| t > now_ms && t - now_ms <= 7 * 86_400_000);

                // 取标题
                let mut title = "赠送积分包".to_string();
                if let Some(placements) = c.get("placements").and_then(|v| v.as_array()) {
                    for p in placements {
                        if let Some(t) = p
                            .get("content")
                            .and_then(|cnt| cnt.get("zh"))
                            .and_then(|zh| zh.get("title"))
                            .and_then(|v| v.as_str())
                        {
                            if !t.is_empty() {
                                title = t.to_string();
                                break;
                            }
                        }
                    }
                }

                resources.push(json!({
                    "packageCode": c.get("campaignKey").and_then(|v| v.as_str()),
                    "packageName": title,
                    "total": amount,
                    "remaining": pack_rem,
                    "used": pack_used,
                    "status": 1,
                    "expireAt": expire_at_ms,
                    "expired": is_expired,
                    "expiringSoon": is_expiring_soon
                }));
            }
        }
    }

    // 按照到期时间升序排序
    resources.sort_by(|a, b| {
        let ta = a["expireAt"].as_i64().unwrap_or(i64::MAX);
        let tb = b["expireAt"].as_i64().unwrap_or(i64::MAX);
        ta.cmp(&tb)
    });

    let soonest_expire_at = resources.first().and_then(|r| r["expireAt"].as_i64());
    let expiring_soon_remaining: f64 = resources
        .iter()
        .filter(|r| r["expiringSoon"].as_bool().unwrap_or(false))
        .filter_map(|r| r["remaining"].as_f64())
        .sum();

    json!({
        "ok": true,
        "accountId": account_id,
        "totalCapacity": total_capacity,
        "totalRemaining": total_remaining,
        "expiringSoonRemaining": expiring_soon_remaining,
        "expired": false,
        "soonestExpireAt": soonest_expire_at,
        "resources": resources
    })
}

/// 获取今日签到状态。
pub async fn get_checkin_status(
    roots: &PathRoots,
    store: &Path,
    account_id: &str,
    variant: QoderVariant,
) -> Value {
    let token = match resolve_token(roots, store, account_id, variant) {
        Ok(t) => t,
        Err(e) => return json!({ "ok": false, "error": e, "todayCheckedIn": false }),
    };

    let base = openapi_base(variant);
    let client = http_client();
    let headers = build_headers(&token);
    let camp_url = format!("{base}/sash/api/v1/me/campaigns?clientType=10");

    let mut req = client.get(&camp_url);
    for (k, v) in &headers {
        req = req.header(k, v);
    }

    match req.send().await {
        Ok(res) if res.status().is_success() => {
            let body: Value = res.json().await.unwrap_or_default();
            let campaigns = body.get("campaigns").and_then(|v| v.as_array());
            if let Some(list) = campaigns {
                let benefit_campaigns: Vec<_> = list
                    .iter()
                    .filter(|c| c.get("actionType").and_then(|v| v.as_str()) == Some("CLAIM_BENEFIT"))
                    .collect();

                let has_claimable = benefit_campaigns.iter().any(|c| {
                    c.get("claimStatus").and_then(|v| v.as_str()) == Some("CLAIMABLE")
                });

                let is_checked_in = !benefit_campaigns.is_empty() && !has_claimable;

                json!({
                    "ok": true,
                    "todayCheckedIn": is_checked_in,
                    "accounts": [],
                    "resources": []
                })
            } else {
                json!({ "ok": true, "todayCheckedIn": false, "accounts": [], "resources": [] })
            }
        }
        Ok(res) => json!({ "ok": false, "todayCheckedIn": false, "error": format!("HTTP {}", res.status()) }),
        Err(e) => json!({ "ok": false, "todayCheckedIn": false, "error": format!("网络错误: {e}") }),
    }
}

/// 执行签到（领取今日 Credits）。
pub async fn checkin(
    roots: &PathRoots,
    store: &Path,
    account_id: &str,
    variant: QoderVariant,
) -> Value {
    let token = match resolve_token(roots, store, account_id, variant) {
        Ok(t) => t,
        Err(e) => return json!({ "result": "error", "error": e }),
    };

    let base = openapi_base(variant);
    let client = http_client();
    let headers = build_headers(&token);
    let camp_url = format!("{base}/sash/api/v1/me/campaigns?clientType=10");

    let mut req = client.get(&camp_url);
    for (k, v) in &headers {
        req = req.header(k, v);
    }

    let camp_body: Value = match req.send().await {
        Ok(res) if res.status().is_success() => res.json().await.unwrap_or_default(),
        Ok(res) => {
            return json!({
                "result": "error",
                "error": format!("获取签到活动失败 (HTTP {})", res.status())
            });
        }
        Err(e) => {
            return json!({
                "result": "error",
                "error": format!("网络错误: {e}")
            });
        }
    };

    let campaigns = camp_body.get("campaigns").and_then(|v| v.as_array());
    let mut claimable_ids = Vec::new();

    if let Some(list) = campaigns {
        for c in list {
            if c.get("actionType").and_then(|v| v.as_str()) == Some("CLAIM_BENEFIT")
                && c.get("claimStatus").and_then(|v| v.as_str()) == Some("CLAIMABLE")
            {
                if let Some(id) = c.get("campaignId").and_then(|v| v.as_str()) {
                    claimable_ids.push(id.to_string());
                }
            }
        }
    }

    if claimable_ids.is_empty() {
        return json!({ "result": "already", "message": "今日已领取或无待领活动" });
    }

    let mut total_claimed = 0;
    for cid in claimable_ids {
        let claim_url = format!("{base}/sash/api/v1/me/campaigns/{cid}/claim");
        let mut creq = client.post(&claim_url);
        for (k, v) in &headers {
            creq = creq.header(k, v);
        }
        if let Ok(res) = creq.send().await {
            if res.status().is_success() {
                let res_json: Value = res.json().await.unwrap_or_default();
                let amt = res_json
                    .get("benefit")
                    .and_then(|b| b.get("amount"))
                    .and_then(|v| v.as_i64())
                    .unwrap_or(100);
                total_claimed += amt;
            }
        }
    }

    json!({
        "result": "success",
        "message": format!("成功领取 {total_claimed} Credits"),
        "claimedAmount": total_claimed
    })
}

fn block_on<F: std::future::Future>(future: F) -> F::Output {
    if let Ok(handle) = tokio::runtime::Handle::try_current() {
        tokio::task::block_in_place(|| handle.block_on(future))
    } else {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(future)
    }
}

pub fn fetch_credit_expiry_sync(
    roots: &PathRoots,
    store: &Path,
    account_id: &str,
    variant: QoderVariant,
) -> Value {
    block_on(fetch_credit_expiry(roots, store, account_id, variant))
}

pub fn get_checkin_status_sync(
    roots: &PathRoots,
    store: &Path,
    account_id: &str,
    variant: QoderVariant,
) -> Value {
    block_on(get_checkin_status(roots, store, account_id, variant))
}

pub fn checkin_sync(
    roots: &PathRoots,
    store: &Path,
    account_id: &str,
    variant: QoderVariant,
) -> Value {
    block_on(checkin(roots, store, account_id, variant))
}

pub fn checkin_all_sync(
    roots: &PathRoots,
    store: &Path,
    variant: QoderVariant,
) -> Value {
    let accounts = bundle::list_all(store);
    let mut success_count = 0;
    for b in accounts {
        if b.variant == variant && b.target == QoderTarget::Desktop {
            let res = checkin_sync(roots, store, &b.account_id, variant);
            if res.get("result").and_then(|r| r.as_str()) == Some("success") {
                success_count += 1;
            }
        }
    }
    json!({ "result": "success", "count": success_count })
}

