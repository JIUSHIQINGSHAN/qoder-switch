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
    Ok(resolve_identity(roots, store, account_id, variant)?.0)
}

fn decrypt_from_bundle_dir(dir: &Path) -> Result<(String, String)> {
    // 兼容三种历史命名：auth_main（带下划线）、authmain（无下划线）、auth.v1.dat（DPAPI 包格式）
    let auth_path = ["auth_main", "authmain", "auth.v1.dat"]
        .iter()
        .map(|n| dir.join(n))
        .find(|p| p.is_file())
        .ok_or_else(|| "凭据文件不存在".to_string())?;

    // 兼容两种 key 文件命名：local_state（带下划线）、localstate（无下划线）
    let key_path = ["local_state", "localstate"]
        .iter()
        .map(|n| dir.join(n))
        .find(|p| p.is_file())
        .ok_or_else(|| "Local State 密钥文件不存在".to_string())?;

    let key = auth_codec::aes_key_from_local_state(&key_path)
        .map_err(|e| format!("读取 Local State 密钥失败: {e}"))?;
    let blob = std::fs::read(&auth_path)
        .map_err(|e| format!("读取凭据文件失败: {e}"))?;
    let dec = auth_codec::decrypt_blob(&key, &blob)
        .map_err(|e| format!("解密凭据失败: {e}"))?;
    let auth = auth_codec::parse_auth(&dec)
        .map_err(|e| format!("解析凭据失败: {e}"))?;
    if auth.token.trim().is_empty() {
        return Err("解析得到的登录 Token 为空".to_string());
    }
    Ok((auth.token, auth.user.email))
}

/// 与 `resolve_token` 同一取值顺序，但把邮箱一并带出。
pub fn resolve_identity(
    roots: &PathRoots,
    store: &Path,
    account_id: &str,
    variant: QoderVariant,
) -> Result<(String, String)> {
    let (token, email, _) = resolve_identity_full(roots, store, account_id, variant)?;
    Ok((token, email))
}

/// 与 `resolve_identity` 一致，但同时返回账号绑定的独立代理（如有）。
pub fn resolve_identity_full(
    roots: &PathRoots,
    store: &Path,
    account_id: &str,
    variant: QoderVariant,
) -> Result<(String, String, Option<String>)> {
    // 1. 若账号包存在于存储中：凭据必须且只能来源于此包，解密失败报错，绝不静默回退现场以防串号
    if let Ok(bundle) = bundle::load(store, account_id, variant, QoderTarget::Desktop) {
        let proxy = bundle.identity.proxy.clone();
        let email = bundle
            .identity
            .email
            .clone()
            .filter(|e| !e.trim().is_empty());
        let dir = bundle.dir_in(store);
        let (token, auth_email) = decrypt_from_bundle_dir(&dir).map_err(|e| {
            format!("账号 {account_id} 凭据解密失败（跨 Windows 用户或 Local State 不匹配，需在本机重新登录一次）: {e}")
        })?;
        return Ok((token, email.unwrap_or(auth_email), proxy));
    }

    // 2. 账号包不存在：仅当目标账号明确为当前桌面现场账号时，才允许从现场读取
    let is_local_id = account_id == crate::modules::view::local_account_id(variant)
        || account_id == "local-cn"
        || account_id == "local-ai"
        || account_id == "local-global";

    if is_local_id {
        if let Ok(auth) = auth_codec::read_desktop_auth(roots, variant) {
            if !auth.token.trim().is_empty() {
                return Ok((auth.token.clone(), email_from_live(&auth), None));
            }
        }
    }

    Err(format!("无法读取账号 {account_id} 的有效登录凭据"))
}

fn email_from_live(auth: &auth_codec::DesktopAuth) -> String {
    auth.user.email.clone()
}

pub fn http_client_with_proxy(proxy_url: Option<&str>) -> reqwest::Client {
    let mut builder = reqwest::Client::builder()
        .connect_timeout(std::time::Duration::from_secs(6))
        .timeout(std::time::Duration::from_secs(15));
    if let Some(p) = proxy_url.filter(|s| !s.trim().is_empty()) {
        if let Ok(proxy) = reqwest::Proxy::all(p.trim()) {
            builder = builder.proxy(proxy);
        }
    }
    builder.build().unwrap_or_default()
}

fn format_http_error(prefix: &str, status: reqwest::StatusCode) -> String {
    match status.as_u16() {
        401 => format!("{prefix}: 登录凭据已失效或过期 (HTTP 401)，需重新登录"),
        403 => format!("{prefix}: 访问受限无权限 (HTTP 403)"),
        429 => format!("{prefix}: 请求过于频繁已被限流 (HTTP 429)，请稍后重试"),
        code if code >= 500 => format!("{prefix}: 服务端异常 (HTTP {code})"),
        code => format!("{prefix}失败 (HTTP {code})"),
    }
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
    let (token, email, proxy) = match resolve_identity_full(roots, store, account_id, variant) {
        Ok(t) => t,
        Err(e) => return json!({ "ok": false, "error": e, "resources": [] }),
    };

    let base = openapi_base(variant);
    let client = http_client_with_proxy(proxy.as_deref());
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
                "error": format_http_error("配额查询", res.status()),
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

    crate::modules::ledger::append_credit_snapshot(
        store,
        &crate::modules::ledger::CreditSnapshot {
            ts: chrono::Utc::now().timestamp_millis(),
            account_id: account_id.to_string(),
            email,
            variant: crate::modules::view::variant_key(variant).to_string(),
            total_capacity,
            total_remaining,
            expiring_soon_remaining,
            soonest_expire_at,
        },
    );

    let mut out = json!({
        "ok": true,
        "accountId": account_id,
        "totalCapacity": total_capacity,
        "totalRemaining": total_remaining,
        "expiringSoonRemaining": expiring_soon_remaining,
        "expired": false,
        "soonestExpireAt": soonest_expire_at,
        "resources": resources
    });
    // 求和可能产出 -0.0（空资源包时），前端 Intl 渲染成 "-0"；统一归一。
    crate::modules::ledger::normalize_signed_zeros(&mut out);
    out
}

/// 获取今日签到状态。
pub async fn get_checkin_status(
    roots: &PathRoots,
    store: &Path,
    account_id: &str,
    variant: QoderVariant,
) -> Value {
    let (token, _email, proxy) = match resolve_identity_full(roots, store, account_id, variant) {
        Ok(t) => t,
        Err(e) => {
            return json!({ "ok": false, "error": e, "todayCheckedIn": false, "variant": crate::modules::view::variant_key(variant) })
        }
    };

    let base = openapi_base(variant);
    let client = http_client_with_proxy(proxy.as_deref());
    let headers = build_headers(&token);
    let camp_url = format!("{base}/sash/api/v1/me/campaigns?clientType=10");

    let mut req = client.get(&camp_url);
    for (k, v) in &headers {
        req = req.header(k, v);
    }

    let vk = crate::modules::view::variant_key(variant);
    match req.send().await {
        Ok(res) if res.status().is_success() => {
            let body: Value = res.json().await.unwrap_or_default();
            let campaigns = body.get("campaigns").and_then(|v| v.as_array());
            if let Some(list) = campaigns {
                let benefit_campaigns: Vec<_> = list
                    .iter()
                    .filter(|c| c.get("actionType").and_then(|v| v.as_str()) == Some("CLAIM_BENEFIT"))
                    .collect();
                // 没有任何可领取的活动 = 官方未开放签到，而不是"今天没签"。
                let has_campaign = !benefit_campaigns.is_empty();

                let has_claimable = benefit_campaigns.iter().any(|c| {
                    c.get("claimStatus").and_then(|v| v.as_str()) == Some("CLAIMABLE")
                });

                let is_checked_in = has_campaign && !has_claimable;

                json!({
                    "ok": true,
                    "todayCheckedIn": is_checked_in,
                    "variant": vk,
                    "accounts": [],
                    "resources": []
                })
            } else {
                json!({ "ok": true, "todayCheckedIn": false, "variant": vk, "accounts": [], "resources": [] })
            }
        }
        Ok(res) => json!({ "ok": false, "todayCheckedIn": false, "error": format_http_error("查询签到状态", res.status()), "variant": vk }),
        Err(e) => json!({ "ok": false, "todayCheckedIn": false, "error": format!("网络错误: {e}"), "variant": vk }),
    }
}

/// 执行签到（领取今日 Credits）。
pub async fn checkin(
    roots: &PathRoots,
    store: &Path,
    account_id: &str,
    variant: QoderVariant,
) -> Value {
    let (token, email, proxy) = match resolve_identity_full(roots, store, account_id, variant) {
        Ok(t) => t,
        Err(e) => {
            crate::modules::ledger::record_checkin_log(store, account_id, "", variant, "error", Some(&e));
            return json!({ "result": "error", "error": e });
        }
    };

    let base = openapi_base(variant);
    let client = http_client_with_proxy(proxy.as_deref());
    let headers = build_headers(&token);
    let camp_url = format!("{base}/sash/api/v1/me/campaigns?clientType=10");

    let mut req = client.get(&camp_url);
    for (k, v) in &headers {
        req = req.header(k, v);
    }

    let camp_body: Value = match req.send().await {
        Ok(res) if res.status().is_success() => res.json().await.unwrap_or_default(),
        Ok(res) => {
            let e = format_http_error("获取签到活动", res.status());
            crate::modules::ledger::record_checkin_log(store, account_id, &email, variant, "error", Some(&e));
            return json!({ "result": "error", "error": e });
        }
        Err(e) => {
            let msg = format!("网络错误: {e}");
            crate::modules::ledger::record_checkin_log(store, account_id, &email, variant, "error", Some(&msg));
            return json!({ "result": "error", "error": msg });
        }
    };

    let campaigns = camp_body.get("campaigns").and_then(|v| v.as_array());
    let mut claimable_ids = Vec::new();
    let mut has_benefit = false;

    if let Some(list) = campaigns {
        for c in list {
            if c.get("actionType").and_then(|v| v.as_str()) == Some("CLAIM_BENEFIT") {
                has_benefit = true;
                if c.get("claimStatus").and_then(|v| v.as_str()) == Some("CLAIMABLE") {
                    if let Some(id) = c.get("campaignId").and_then(|v| v.as_str()) {
                        claimable_ids.push(id.to_string());
                    }
                }
            }
        }
    }

    // 官方未开放签到活动：不写成功日志、不计入失败重试。前端据此弹「未开放」而非「已签到」。
    if !has_benefit {
        return json!({ "result": "inactive", "inactive": true, "message": "官方未开放签到活动" });
    }

    if claimable_ids.is_empty() {
        crate::modules::ledger::record_checkin_log(store, account_id, &email, variant, "already", None);
        return json!({ "result": "already", "message": "今日已签到" });
    }

    let mut total_claimed = 0f64;
    let mut success_count = 0usize;
    let mut last_err: Option<String> = None;
    for cid in claimable_ids {
        let claim_url = format!("{base}/sash/api/v1/me/campaigns/{cid}/claim");
        let mut creq = client.post(&claim_url);
        for (k, v) in &headers {
            creq = creq.header(k, v);
        }
        match creq.send().await {
            Ok(res) if res.status().is_success() => {
                success_count += 1;
                let res_json: Value = res.json().await.unwrap_or_default();
                let amt = res_json
                    .get("benefit")
                    .and_then(|b| b.get("amount"))
                    .and_then(|v| v.as_f64())
                    .or_else(|| res_json.get("amount").and_then(|v| v.as_f64()))
                    .unwrap_or(0.0);
                total_claimed += amt;
            }
            Ok(res) => {
                last_err = Some(format_http_error("领取积分", res.status()));
            }
            Err(e) => {
                last_err = Some(format!("网络错误: {e}"));
            }
        }
    }

    if success_count == 0 {
        let e = last_err.unwrap_or_else(|| "领取失败".to_string());
        crate::modules::ledger::record_checkin_log(store, account_id, &email, variant, "error", Some(&e));
        return json!({ "result": "error", "error": e });
    }

    crate::modules::ledger::record_checkin_log(store, account_id, &email, variant, "success", None);
    json!({
        "result": "success",
        "message": format!("成功领取 {total_claimed} Credits"),
        "claimedAmount": total_claimed
    })
}

/// webui 批量接口：一次返回全部桌面账号的今日签到状态，形状对齐前端按 accountId 过滤。
pub async fn get_checkin_status_all(
    roots: &PathRoots,
    store: &Path,
    only_variant: Option<QoderVariant>,
) -> Value {
    let accounts = bundle::list_all(store);
    let mut out = Vec::new();
    for b in accounts {
        if b.target != QoderTarget::Desktop {
            continue;
        }
        // 已去掉国际版：批量接口只覆盖国内版；显式传 ai 也返回空（不是漏，是刻意）。
        if b.variant != QoderVariant::Cn {
            continue;
        }
        if let Some(v) = only_variant {
            if b.variant != v {
                continue;
            }
        }
        let email = b.identity.email.clone().unwrap_or_default();
        let res = get_checkin_status(roots, store, &b.account_id, b.variant).await;
        let mut entry = json!({
            "accountId": b.account_id,
            "email": email,
            "variant": crate::modules::view::variant_key(b.variant),
            "ok": res.get("ok").and_then(|x| x.as_bool()).unwrap_or(false),
            "todayCheckedIn": res.get("todayCheckedIn").and_then(|x| x.as_bool()).unwrap_or(false),
        });
        if let Some(err) = res.get("error").and_then(|e| e.as_str()) {
            entry["error"] = json!(err);
        }
        out.push(entry);
    }
    json!({ "accounts": out })
}

pub fn get_checkin_status_all_sync(
    roots: &PathRoots,
    store: &Path,
    only_variant: Option<QoderVariant>,
) -> Value {
    block_on(get_checkin_status_all(roots, store, only_variant))
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

/// 批量签到：一次返回**每个**账号的结果，形状对齐前端 `checkinAll` 契约。
///
/// `only_variant = None` 时覆盖全部档位；账号页/设置页按当前档位传入。
/// 之前只回 `{result,count}`，前端 `res.accounts.filter` 直接崩。
pub async fn checkin_all(
    roots: &PathRoots,
    store: &Path,
    only_variant: Option<QoderVariant>,
) -> Value {
    let accounts = bundle::list_all(store);
    let mut out = Vec::new();
    for b in accounts {
        if b.target != QoderTarget::Desktop {
            continue;
        }
        // 已去掉国际版：批量接口只覆盖国内版；显式传 ai 也返回空（不是漏，是刻意）。
        if b.variant != QoderVariant::Cn {
            continue;
        }
        if let Some(v) = only_variant {
            if b.variant != v {
                continue;
            }
        }
        let email = b.identity.email.clone().unwrap_or_default();
        let res = checkin(roots, store, &b.account_id, b.variant).await;
        let mut entry = json!({
            "accountId": b.account_id,
            "email": email,
            "variant": crate::modules::view::variant_key(b.variant),
            "result": res.get("result").and_then(|r| r.as_str()).unwrap_or("error"),
            "inactive": res.get("inactive").and_then(|x| x.as_bool()).unwrap_or(false),
        });
        if let Some(err) = res.get("error").and_then(|e| e.as_str()) {
            entry["error"] = json!(err);
        }
        out.push(entry);
    }
    json!({ "accounts": out })
}

pub fn checkin_all_sync(
    roots: &PathRoots,
    store: &Path,
    only_variant: Option<QoderVariant>,
) -> Value {
    block_on(checkin_all(roots, store, only_variant))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::modules::config::PathRoots;

    #[test]
    fn resolve_identity_does_not_silently_fallback_to_live_for_saved_or_unknown_accounts() {
        let tmp = std::env::temp_dir().join(format!("qs-quota-test-{}", uuid::Uuid::new_v4().simple()));
        let roots = PathRoots::sandbox(&tmp);
        let store = tmp.join("store");
        std::fs::create_dir_all(&store).unwrap();

        // 构造一个模拟的现场桌面凭据（live）
        let d = crate::modules::variant::desktop_dir(&roots, QoderVariant::Cn);
        std::fs::create_dir_all(&d).unwrap();
        // 构造一段合法的明文 auth.v1.dat 或 DPAPI auth
        let live_auth = json!({
            "token": "live-secret-token-12345",
            "user": {
                "id": "live-user-1",
                "name": "Live User",
                "email": "live@example.com"
            }
        });
        std::fs::write(d.join("auth.v1.dat"), serde_json::to_vec(&live_auth).unwrap()).unwrap();

        // 1. 对于不存在的普通账号 ID，绝对不能读取 live 凭据
        let err = resolve_identity(&roots, &store, "random-non-existent-account", QoderVariant::Cn);
        assert!(err.is_err(), "不存在的账号不能成功返回凭据: {err:?}");
        let err_msg = err.unwrap_err();
        assert!(
            err_msg.contains("无法读取账号"),
            "错误信息应当指明无法读取指定账号，而不是静默返回 live 账号: {err_msg}"
        );

        // 2. 构造一个损坏/无法解密的账号包
        let acc_dir = bundle::bundle_dir_in(&store, "corrupted-acc", QoderVariant::Cn, QoderTarget::Desktop);
        std::fs::create_dir_all(&acc_dir).unwrap();
        std::fs::write(
            acc_dir.join("bundle.json"),
            r#"{"account_id":"corrupted-acc","variant":"cn","target":"desktop","created_at":"2026-09-23T00:00:00Z","members":[],"identity":{}}"#,
        ).unwrap();
        // 缺少 auth_main / key 文件
        let err2 = resolve_identity(&roots, &store, "corrupted-acc", QoderVariant::Cn);
        assert!(err2.is_err(), "损坏或缺少密钥的包必须报错，绝不能回退现场: {err2:?}");
        let err_msg2 = err2.unwrap_err();
        assert!(
            err_msg2.contains("解密失败") || err_msg2.contains("重新登录"),
            "应当明确提示解密失败重新登录: {err_msg2}"
        );

        // 3. 现场账号 local-cn 在真实桌面凭据存在时，应当允许读取 live
        let real_roots = PathRoots::real();
        let real_dir = crate::modules::variant::desktop_dir(&real_roots, QoderVariant::Cn);
        if real_dir.join("auth.v1.dat").is_file() {
            let local_res = resolve_identity(&real_roots, &store, "local-cn", QoderVariant::Cn);
            assert!(local_res.is_ok(), "现场账号 local-cn 应当允许读取 live: {local_res:?}");
            let (tok, _em) = local_res.unwrap();
            assert!(!tok.trim().is_empty());
        }

        let _ = std::fs::remove_dir_all(&tmp);
    }
}


