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
use crate::modules::risk;
use crate::modules::variant::{QoderTarget, QoderVariant};
use crate::Result;

pub fn openapi_base(variant: QoderVariant) -> &'static str {
    match variant {
        QoderVariant::Cn => "https://openapi.qoder.com.cn",
        QoderVariant::Global => "https://openapi.qoder.sh",
    }
}

/// 测试注入点：HTTP mock 测试把基址指到本地 TcpListener。进程级而不是
/// thread-local —— block_on/block_in_place 会跨线程跳，thread-local 不可靠；
/// 使用方（测试）自己持锁串行化。
#[cfg(test)]
static MOCK_BASE: std::sync::Mutex<Option<String>> = std::sync::Mutex::new(None);

#[cfg(test)]
pub(crate) fn set_mock_base(base: Option<String>) {
    *MOCK_BASE
        .lock()
        .unwrap_or_else(|p| p.into_inner()) = base;
}

fn api_base(variant: QoderVariant) -> String {
    #[cfg(test)]
    if let Some(b) = MOCK_BASE
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .clone()
    {
        return b;
    }
    openapi_base(variant).to_string()
}

/// 单账号命令的档位自愈：账号包目录本身就带档位前缀，这里按账号 id 在两档里
/// 定位桌面包。此前 `variant_of(None)=Cn` 的缺省贯穿两个宿主 —— 前端没传 variant
/// 时对 Global 账号发单账号签到会打错域名、找错包。显式给出的档位仍然优先
/// （尊重调用方；包真不在该档位时由后续 load 报错，文案会指路）。
pub fn resolve_variant_for_account(
    store: &Path,
    account_id: &str,
    explicit: Option<QoderVariant>,
) -> QoderVariant {
    if let Some(v) = explicit {
        return v;
    }
    for v in QoderVariant::ALL {
        if bundle::load(store, account_id, v, QoderTarget::Desktop)
            .map(|b| !b.is_empty())
            .unwrap_or(false)
        {
            return v;
        }
    }
    QoderVariant::Cn
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

fn decrypt_from_bundle_dir(
    dir: &Path,
    roots: &PathRoots,
    variant: QoderVariant,
) -> Result<(String, String, String)> {
    // 凭据命名兼容、密钥载体与解码方案全部收在 auth_codec::decrypt_bundle_auth 里：
    // Windows 用包内 Local State（DPAPI），macOS 用本机钥匙串（PBKDF2+CBC）。
    let auth = auth_codec::decrypt_bundle_auth(dir, roots, variant)?;
    if auth.token.trim().is_empty() {
        return Err("解析得到的登录 Token 为空".to_string());
    }
    Ok((auth.token, auth.user.email, auth.user.id))
}

/// 与 `resolve_token` 同一取值顺序，但把邮箱一并带出。
pub fn resolve_identity(
    roots: &PathRoots,
    store: &Path,
    account_id: &str,
    variant: QoderVariant,
) -> Result<(String, String)> {
    let (token, email, _, _) = resolve_identity_full(roots, store, account_id, variant)?;
    Ok((token, email))
}

/// 与 `resolve_identity` 一致，但同时返回账号 uid（风控 stdin 与缓存键需要）
/// 和账号绑定的独立代理（如有）。
pub fn resolve_identity_full(
    roots: &PathRoots,
    store: &Path,
    account_id: &str,
    variant: QoderVariant,
) -> Result<(String, String, String, Option<String>)> {
    // 1. 若账号包存在于存储中：凭据必须且只能来源于此包，解密失败报错，绝不静默回退现场以防串号
    if let Ok(bundle) = bundle::load(store, account_id, variant, QoderTarget::Desktop) {
        let proxy = bundle.identity.proxy.clone();
        let email = bundle
            .identity
            .email
            .clone()
            .filter(|e| !e.trim().is_empty());
        let uid = bundle
            .identity
            .uid
            .clone()
            .filter(|s| !s.trim().is_empty());
        let dir = bundle.dir_in(store);
        let (token, auth_email, auth_uid) =
            decrypt_from_bundle_dir(&dir, roots, variant).map_err(|e| {
                format!(
                    "账号 {account_id} 凭据解密失败（{}，需在本机重新登录一次）: {e}",
                    auth_codec::portability_hint()
                )
            })?;
        // uid 优先用包元数据里留的，缺了再回退解密结果 —— 两者都来自解密时的
        // 同一份登录态，只是写入时机不同（capture 即存 vs 后来补齐）。
        let uid = uid.unwrap_or(auth_uid);
        return Ok((token, email.unwrap_or(auth_email), uid, proxy));
    }

    // 2. 账号包不存在：仅当目标账号明确为当前桌面现场账号时，才允许从现场读取
    let is_local_id = account_id == crate::modules::view::local_account_id(variant)
        || account_id == "local-cn"
        || account_id == "local-ai"
        || account_id == "local-global";

    if is_local_id {
        if let Ok(auth) = auth_codec::read_desktop_auth(roots, variant) {
            if !auth.token.trim().is_empty() {
                return Ok((
                    auth.token.clone(),
                    email_from_live(&auth),
                    auth.user.id.clone(),
                    None,
                ));
            }
        }
    }

    // 走到这里说明桌面轴没有可用的包。但"读不到"其实有三种成因，处置方式完全不同，
    // 不能含糊成同一句话让用户去猜：
    //   - 库里任何目标都没有它 → 卡片是**列表加载之后**才被删掉的幽灵，唯一该做的是
    //     刷新列表（前端据 accountMissing 自愈），而不是反复重登；
    //   - 只有 CLI / Work 目标的包 → 积分与签到只认桌面客户端凭据（CLI 不落盘可解密的
    //     登录态），这是能力边界，不是故障；
    //   - 桌面包在、只是解不开 → 已在上面单独报错，不会走到这里。
    // 文案保留「无法读取账号 …」前缀：它是对外契约，调用方与测试都按它判类别。
    Err(match account_presence(store, account_id) {
        AccountPresence::Missing => format!(
            "无法读取账号 {account_id} 的有效登录凭据：该账号包已不在账号库中（可能已被删除或移走）。\
             请刷新账号列表；若要重新使用它，请重新导入本机账号或重新扫码登录。"
        ),
        AccountPresence::OtherTargets(targets) => format!(
            "无法读取账号 {account_id} 的有效登录凭据：账号库里只有 {} 的账号包，没有桌面客户端凭据。\
             积分与签到只支持桌面客户端登录态。",
            targets.join("、")
        ),
    })
}

/// 账号在库里"还剩什么"。只做 `bundle::load` 判存在，**不解密** —— 这里回答的是
/// "包在不在、在哪个目标上"，与"能不能解开"是两件事。
enum AccountPresence {
    /// 任何 (档位·目标) 组合下都没有这个账号的包。
    Missing,
    /// 只有非桌面目标的包（人话标签，如「Qoder 国内版·Qoder CLI」）。
    OtherTargets(Vec<String>),
}

fn account_presence(store: &Path, account_id: &str) -> AccountPresence {
    if bundle::validate_account_id(account_id).is_err() {
        return AccountPresence::Missing;
    }
    let others: Vec<String> = crate::modules::variant::all_axes()
        .into_iter()
        .filter(|(_, t)| *t != QoderTarget::Desktop)
        .filter(|(v, t)| {
            bundle::load(store, account_id, *v, *t)
                .map(|b| !b.is_empty())
                .unwrap_or(false)
        })
        .map(|(v, t)| format!("{}·{}", v.label(), t.label()))
        .collect();
    if others.is_empty() {
        AccountPresence::Missing
    } else {
        AccountPresence::OtherTargets(others)
    }
}

/// 账号包是否已从库里彻底消失（前端据此刷新列表、清掉幽灵卡片）。
pub fn account_is_missing(store: &Path, account_id: &str) -> bool {
    matches!(account_presence(store, account_id), AccountPresence::Missing)
}

fn email_from_live(auth: &auth_codec::DesktopAuth) -> String {
    auth.user.email.clone()
}

/// `RELATIVE_DAYS` 资源包的到期毫秒时间戳：`startAt(秒) * 1000 + days * 86400_000`。
///
/// 必须全程 checked：服务端若把 `startAt` 填成毫秒（于是又乘 1000），或把 `days`
/// 放成天文数字，i64 会溢出 —— release 下回绕成**负数**，而下游判据是
/// `is_expired = t <= now`，负时间戳会让**所有**有效资源包显示成"已过期"。
/// 溢出时返回 None（"到期时间未知"），绝不返回一个错误的负数。
///
/// `days` 另有上界钳制：10 年（3650 天）以外的包在业务上不存在，
/// 钳掉可避免"极大但未溢出"的值把到期时间推到几百年后。
fn relative_days_expire_ms(start_at_secs: i64, days: i64) -> Option<i64> {
    if start_at_secs <= 0 {
        return None;
    }
    let ms = start_at_secs.checked_mul(1000)?;
    let span = days.clamp(0, 3650).checked_mul(86_400_000)?;
    let total = ms.checked_add(span)?;
    (total > 0).then_some(total)
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
    // 端点取证自 Windows，但把这个值写死成 "windows" 会在 mac 上**继续工作同时说谎**
    // —— 服务端若按 OS 指纹做风控或统计，我们只会看到 200，看不到后果。按宿主报真值。
    h.insert("Cosy-MachineOS".into(), machine_os_tag().into());
    h.insert("User-Agent".into(), "Qoder".into());
    h.insert("Accept".into(), "application/json".into());
    h
}

/// `Cosy-MachineOS` 的取值。与 Qoder 客户端自身上报的字符串对齐（见 `docs/qoder-endpoints.md`
/// 的实测记录）；拿不准时宁可报真实平台，也不要报另一个平台的值。
fn machine_os_tag() -> &'static str {
    if cfg!(target_os = "macos") {
        "macos"
    } else if cfg!(target_os = "linux") {
        "linux"
    } else {
        "windows"
    }
}

/// 签到叠加头里 `Cosy-MachineOS` 的口径：客户端 `createAuthorizedHeaders` 用的是
/// 「架构_平台」形（`x86_64_win32` / `aarch64_darwin`…），与基础头的 `windows`
/// 是两套口径（CreditDaddy 对客户端的逐头对照）。Windows ARM 的取值未实测，
/// 按同一构词法给出，真机探测时一并确认。
fn machine_os_tag_overlay() -> &'static str {
    if cfg!(target_os = "macos") {
        if cfg!(target_arch = "aarch64") {
            "aarch64_darwin"
        } else {
            "x86_64_darwin"
        }
    } else if cfg!(target_os = "linux") {
        if cfg!(target_arch = "aarch64") {
            "aarch64_linux"
        } else {
            "x86_64_linux"
        }
    } else if cfg!(target_arch = "aarch64") {
        "aarch64_win32"
    } else {
        "x86_64_win32"
    }
}

/// 签到/活动链路的请求头。
///
/// 国内版保持既有最小头集**原样不动** —— 它在工作，不给它叠加任何东西。
/// 国际版在其上叠加客户端 `createAuthorizedHeaders` 的设备身份：真实客户端版本
/// （`build-manifest.json` 的 productVersion）、`架构_平台` 口径的 MachineOS、
/// MachineId/Hostname，以及设备风控三头；风控凭据取不到时**降级为不带风控头
/// 继续请求**，`risk_error` 把原因带回给调用方进日志/提示 —— 服务端的表现是
/// "没有每日积分活动"，绝不编造成功。
fn build_campaign_headers(
    roots: &PathRoots,
    store: &Path,
    token: &str,
    variant: QoderVariant,
    uid: &str,
) -> (HashMap<String, String>, Option<String>) {
    let mut h = build_headers(token);
    if variant == QoderVariant::Cn {
        return (h, None);
    }
    if let Some(ver) = risk::client_product_version(roots, variant) {
        h.insert("Cosy-Version".into(), ver);
    }
    h.insert("Cosy-MachineOS".into(), machine_os_tag_overlay().into());
    h.insert("Cosy-MachineId".into(), risk::machine_id_for(roots, store, variant));
    if let Some(host) = risk::machine_hostname() {
        h.insert("Cosy-MachineHostname".into(), host);
    }
    match uid.is_empty() {
        true => (
            h,
            Some("设备风控凭据不可用：账号缺少 uid，无法生成设备身份".to_string()),
        ),
        false => match risk::get_risk_identity(roots, variant, uid) {
            Some(r) => {
                h.insert("Cosy-MachineToken".into(), r.machine_token);
                h.insert("Cosy-MachineCode".into(), r.machine_code);
                h.insert("Cosy-MachineType".into(), r.machine_type);
                (h, None)
            }
            None => (
                h,
                Some(
                    "设备风控凭据不可用（未找到本机 Qoder 客户端的 runtime-info 组件）——\
                     本次请求未带风控身份，服务端可能不发放每日积分活动"
                        .to_string(),
                ),
            ),
        },
    }
}

/// 查询账号的积分/配额详细情况（对齐前端 CreditExpiry 契约）。
pub async fn fetch_credit_expiry(
    roots: &PathRoots,
    store: &Path,
    account_id: &str,
    variant: QoderVariant,
) -> Value {
    let (token, email, _uid, proxy) = match resolve_identity_full(roots, store, account_id, variant) {
        Ok(t) => t,
        // `accountMissing` 是给前端的结构化信号：账号包在列表加载之后被删掉时，
        // 页面上的卡片已经是幽灵 —— 重拉一次列表就能自愈。只靠 error 文案判断太脆
        // （文案会随迭代改），所以标志位单独给。
        Err(e) => {
            return json!({
                "ok": false,
                "error": e,
                "resources": [],
                "accountMissing": account_is_missing(store, account_id),
            })
        }
    };

    let base = api_base(variant);
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
                    relative_days_expire_ms(start_at, days)
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
    let (token, _email, uid, proxy) = match resolve_identity_full(roots, store, account_id, variant) {
        Ok(t) => t,
        Err(e) => {
            return json!({ "ok": false, "error": e, "todayCheckedIn": false, "variant": crate::modules::view::variant_key(variant) })
        }
    };

    let base = api_base(variant);
    let client = http_client_with_proxy(proxy.as_deref());
    let (headers, risk_error) = build_campaign_headers(roots, store, &token, variant, &uid);
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
                // 必须**正面**看到 CLAIMED 才算已签到。此前用 `has_campaign && !has_claimable`，
                // 把任何未知 claimStatus（EXPIRED / LOCKED / 官方新增枚举）都折叠成"已签到" ——
                // 用户看到绿色成功提示，实际什么也没领到。这是本模块反复出现的
                // "把未知状态折叠成最乐观答案" 的老毛病，这里改成只认已知的好状态。
                let has_claimed = benefit_campaigns.iter().any(|c| {
                    c.get("claimStatus").and_then(|v| v.as_str()) == Some("CLAIMED")
                });

                let is_checked_in = has_campaign && has_claimed && !has_claimable;

                let mut out = json!({
                    "ok": true,
                    "todayCheckedIn": is_checked_in,
                    "variant": vk,
                    "accounts": [],
                    "resources": []
                });
                if let Some(re) = &risk_error {
                    out["riskError"] = json!(re);
                }
                out
            } else {
                let mut out = json!({ "ok": true, "todayCheckedIn": false, "variant": vk, "accounts": [], "resources": [] });
                if let Some(re) = &risk_error {
                    out["riskError"] = json!(re);
                }
                out
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
    let (token, email, uid, proxy) = match resolve_identity_full(roots, store, account_id, variant) {
        Ok(t) => t,
        Err(e) => {
            crate::modules::ledger::record_checkin_log(store, account_id, "", variant, "error", Some(&e));
            return json!({ "result": "error", "error": e });
        }
    };

    // 国际版「每台设备每日限领一个账号」：名额被同机其它账号占着时，连活动查询
    // 都可以省 —— 直接如实标记 limited（服务端权威，备忘只是省请求 + 给出人话）。
    if variant == QoderVariant::Global {
        if let Some(claim) = crate::modules::ledger::read_device_claim(store) {
            if claim.day == crate::modules::ledger::qoder_day_key() && claim.account_id != account_id {
                let msg = format!(
                    "本机今日国际版额度已由「{}」领取（Qoder 每台设备每天限领一个国际版账号）",
                    claim.display_name()
                );
                crate::modules::ledger::record_checkin_log(store, account_id, &email, variant, "limited", None);
                return json!({ "result": "limited", "limited": true, "message": msg });
            }
        }
    }

    let base = api_base(variant);
    let client = http_client_with_proxy(proxy.as_deref());
    let (headers, risk_error) = build_campaign_headers(roots, store, &token, variant, &uid);
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
    let mut has_claimed = false;

    if let Some(list) = campaigns {
        for c in list {
            if c.get("actionType").and_then(|v| v.as_str()) == Some("CLAIM_BENEFIT") {
                has_benefit = true;
                match c.get("claimStatus").and_then(|v| v.as_str()) {
                    Some("CLAIMABLE") => {
                        if let Some(id) = c.get("campaignId").and_then(|v| v.as_str()) {
                            claimable_ids.push(id.to_string());
                        }
                    }
                    // 正面证据：确实领过了。
                    Some("CLAIMED") => has_claimed = true,
                    // 其它枚举（EXPIRED / LOCKED / 官方新增）既不是"可领"，也不能当作
                    // "已领" —— 落到下面走"状态未知"分支，不编造成功。
                    _ => {}
                }
            }
        }
    }

    // 官方未开放签到活动：不写成功日志、不计入失败重试。前端据此弹「未开放」而非「已签到」。
    // 国际版下「没有活动」常常就是风控拒绝/设备名额问题的对外表现：把风控降级
    // 信息如实带上，别让用户以为只是官方没开活动。
    if !has_benefit {
        let mut out = json!({ "result": "inactive", "inactive": true, "message": "官方未开放签到活动" });
        if let Some(re) = &risk_error {
            out["riskError"] = json!(re);
        }
        return out;
    }

    if claimable_ids.is_empty() {
        // 只有正面看到 CLAIMED 才报"今日已签到"。此前只判"没有可领的"就一律说已签到，
        // 把 EXPIRED/LOCKED/未知枚举也折叠成成功 —— 用户拿到绿色提示但没领到东西。
        if has_claimed {
            crate::modules::ledger::record_checkin_log(store, account_id, &email, variant, "already", None);
            return json!({ "result": "already", "message": "今日已签到" });
        }
        let mut out = json!({
            "result": "inactive",
            "inactive": true,
            "message": "当前没有可领取的签到奖励（活动状态未知或已结束）"
        });
        if let Some(re) = &risk_error {
            out["riskError"] = json!(re);
        }
        return out;
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

    // 国际版：领取成功即占用本机今日名额，写备忘让后续账号直接判 limited，
    // 不再徒劳打 claim（服务端的权威判定仍在，备忘只是客户端的省请求账本）。
    if variant == QoderVariant::Global {
        crate::modules::ledger::write_device_claim(
            store,
            &crate::modules::ledger::DeviceClaim {
                day: crate::modules::ledger::qoder_day_key(),
                account_id: account_id.to_string(),
                email: email.clone(),
            },
        );
    }

    crate::modules::ledger::record_checkin_log(store, account_id, &email, variant, "success", None);
    let mut out = json!({
        "result": "success",
        "message": format!("成功领取 {total_claimed} Credits"),
        "claimedAmount": total_claimed
    });
    if let Some(re) = &risk_error {
        out["riskError"] = json!(re);
    }
    out
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
        // 国际版放开（v0.2 起接入 CreditDaddy 取证的风控签到）：不再按档位过滤，
        // only_variant 为 None 时覆盖两档；国际版的设备限领语义由 `checkin` 内部
        // 的备忘处理，批量按 list_all 序即"列表靠前优先"。
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
        if let Some(re) = res.get("riskError").and_then(|e| e.as_str()) {
            entry["riskError"] = json!(re);
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
        // 国际版放开：同 get_checkin_status_all；国际版的「每设备每日一领」由
        // `checkin` 内部的名额备忘处理 —— list_all 序即"列表靠前优先"。
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
        if let Some(msg) = res.get("message").and_then(|m| m.as_str()) {
            entry["message"] = json!(msg);
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

    /// RELATIVE_DAYS 到期时间必须抗溢出：宁可"未知"，也不给负数（负数会让所有
    /// 有效包被判定为已过期）。
    #[test]
    fn relative_days_expire_never_overflows_to_negative() {
        // 正常值：2026-01-01T00:00:00Z + 30 天。
        let normal = relative_days_expire_ms(1_767_225_600, 30).unwrap();
        assert!(normal > 0);
        assert_eq!(normal, 1_767_225_600 * 1000 + 30 * 86_400_000);

        // startAt 被误填成毫秒 → 乘 1000 溢出 → None（而不是负数）。
        assert_eq!(relative_days_expire_ms(i64::MAX, 30), None);
        // days 天文数字：先被钳到 3650 天，因此不会溢出，结果仍是有限正数。
        // （钳制发生在 checked_mul 之前，这是刻意的 —— 脏数据应被收敛而非整条丢弃。）
        let huge_days = relative_days_expire_ms(1_767_225_600, i64::MAX).unwrap();
        assert_eq!(huge_days, 1_767_225_600 * 1000 + 3650 * 86_400_000);
        assert!(huge_days > 0);
        // days 超上界被钳到 3650 天，结果仍是正数且在合理范围内。
        let clamped = relative_days_expire_ms(1_767_225_600, 100_000).unwrap();
        assert_eq!(clamped, 1_767_225_600 * 1000 + 3650 * 86_400_000);
        // 负数 days（脏数据）钳到 0，退化为 startAt 本身，不退化成负数。
        assert_eq!(relative_days_expire_ms(1_767_225_600, -5).unwrap(), 1_767_225_600 * 1000);
        // startAt 非正 → 无到期时间。
        assert_eq!(relative_days_expire_ms(0, 30), None);
        assert_eq!(relative_days_expire_ms(-1, 30), None);
    }

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

    /// 「读不到桌面凭据」的两种成因必须分开报，且 `accountMissing` 只在真的没包时为真：
    /// - 包已被删（界面上的幽灵卡片）→ 文案指向"刷新列表"，标志为 true，前端据此自愈；
    /// - 只有 CLI 目标的包 → 说清是能力边界，标志为 false（否则前端会白刷一次列表）。
    ///
    /// 回归背景：账号包在列表加载之后被删掉时，卡片会一直挂着一条用户无法处置的报错；
    /// 而 CLI 轴包被 `list_all` 列出、桌面轴查询失败时，用户会误以为账号坏了。
    #[test]
    fn missing_bundle_and_cli_only_bundle_are_reported_differently() {
        let tmp = std::env::temp_dir().join(format!(
            "qs-quota-presence-{}",
            uuid::Uuid::new_v4().simple()
        ));
        let roots = PathRoots::sandbox(&tmp);
        let store = tmp.join("store");
        std::fs::create_dir_all(&store).unwrap();

        // 1) 库里任何 (档位·目标) 都没有这个账号 → Missing。
        let err = resolve_identity(&roots, &store, "ghost-account", QoderVariant::Cn).unwrap_err();
        assert!(err.contains("无法读取账号"), "前缀是对外契约: {err}");
        assert!(err.contains("刷新账号列表"), "要给出可操作的下一步: {err}");
        assert!(account_is_missing(&store, "ghost-account"));

        // 2) 只有 CLI 目标的包 → 是能力边界，不能说成"包丢了"。
        let dir = bundle::bundle_dir_in(&store, "cli-only", QoderVariant::Cn, QoderTarget::Cli);
        std::fs::create_dir_all(&dir).unwrap();
        let b = bundle::Bundle {
            account_id: "cli-only".into(),
            variant: QoderVariant::Cn,
            target: QoderTarget::Cli,
            created_at: crate::modules::config::now_ts(),
            members: vec![bundle::Member {
                role: crate::modules::variant::FileRole::CliUser,
                file_name: "cliuser".into(),
                sha256: "00".into(),
                size: 1,
                critical: true,
            }],
            identity: bundle::Identity {
                name: Some("cli 账号".into()),
                ..Default::default()
            },
        };
        bundle::write_meta(&store, &b).unwrap();

        let err = resolve_identity(&roots, &store, "cli-only", QoderVariant::Cn).unwrap_err();
        assert!(err.contains("桌面客户端"), "要点明缺的是桌面凭据: {err}");
        assert!(!err.contains("已不在账号库中"), "有包就不能说包丢了: {err}");
        assert!(!account_is_missing(&store, "cli-only"), "有 CLI 包不算丢失");

        let _ = std::fs::remove_dir_all(&tmp);
    }

    // ---------- HTTP mock（零依赖：手写 TcpListener 应答，不加 dev HTTP 框架） ----------

    use std::io::Read as IoRead;
    use std::io::Write as IoWrite;
    use std::net::TcpListener;

    /// mock 测试互斥：MOCK_BASE 与 risk 的测试注入都是进程级静态，测试并行会串场。
    static MOCK_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    struct MockServer {
        addr: String,
        requests: std::sync::mpsc::Receiver<(String, Vec<(String, String)>)>,
    }

    /// 起本地 HTTP 服务，依序回放 canned 响应，并把每个请求（请求行 + 小写化头）
    /// 发回给测试断言。签到链路的请求都无 body，这里不实现 body 读取。
    fn start_mock(responses: Vec<(u16, String)>) -> MockServer {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap().to_string();
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            for (code, body) in responses {
                let Ok((mut stream, _)) = listener.accept() else { break };
                let mut buf = [0u8; 65536];
                let n = stream.read(&mut buf).unwrap_or(0);
                let raw = String::from_utf8_lossy(&buf[..n]).to_string();
                let mut lines = raw.split("\r\n");
                let request_line = lines.next().unwrap_or("").to_string();
                let mut headers = Vec::new();
                for l in lines {
                    if l.is_empty() {
                        break;
                    }
                    if let Some((k, v)) = l.split_once(':') {
                        headers.push((k.trim().to_ascii_lowercase(), v.trim().to_string()));
                    }
                }
                let _ = tx.send((request_line, headers));
                let reason = match code {
                    200 => "OK",
                    401 => "Unauthorized",
                    403 => "Forbidden",
                    _ => "Status",
                };
                let resp = format!(
                    "HTTP/1.1 {code} {reason}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                    body.len(),
                    body
                );
                let _ = stream.write_all(resp.as_bytes());
                let _ = stream.flush();
            }
        });
        MockServer { addr, requests: rx }
    }

    impl MockServer {
        fn last_headers(&self) -> Vec<(String, String)> {
            let (_, h) = self.requests.recv().expect("mock 服务应收到请求");
            h
        }
    }

    fn campaigns_body(uid: &str, campaigns: Value) -> String {
        json!({ "uid": uid, "campaigns": campaigns }).to_string()
    }

    fn claimable(id: &str) -> Value {
        json!({ "campaignId": id, "campaignKey": "k", "actionType": "CLAIM_BENEFIT", "claimStatus": "CLAIMABLE", "benefit": { "amount": 100.0 } })
    }

    fn claimed(id: &str) -> Value {
        json!({ "campaignId": id, "campaignKey": "k", "actionType": "CLAIM_BENEFIT", "claimStatus": "CLAIMED", "benefit": { "amount": 100.0 } })
    }

    fn temp_store_dir() -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("qs-quota-mock-{}", uuid::Uuid::new_v4().simple()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// 造一个**可解密**的桌面包：测试随机密钥 + Local State 以 DPAPI(CurrentUser)
    /// 封装（与真实布局同构），meta 里带 identity.uid（风控 stdin/缓存键需要）。
    /// Windows 专属：DPAPI 是 Windows 侧方案。
    #[cfg(windows)]
    fn make_decryptable_bundle(
        store: &Path,
        account_id: &str,
        variant: QoderVariant,
        uid: &str,
        email: &str,
    ) {
        use crate::modules::auth_codec::MasterKey;
        let a = *uuid::Uuid::new_v4().as_bytes();
        let b = *uuid::Uuid::new_v4().as_bytes();
        let mut key = [0u8; 32];
        key[..16].copy_from_slice(&a);
        key[16..].copy_from_slice(&b);
        let dir = bundle::bundle_dir_in(store, account_id, variant, QoderTarget::Desktop);
        std::fs::create_dir_all(&dir).unwrap();
        let auth = json!({
            "schemaVersion": 1,
            "token": format!("tok-{account_id}-0000000000"),
            "refreshToken": "rtok-test",
            "expiresAt": "2099-01-01T00:00:00Z",
            "user": { "id": uid, "name": "tester", "email": email }
        });
        let pt = serde_json::to_vec(&auth).unwrap();
        let blob = auth_codec::encrypt_blob(&MasterKey::Windows(key), &pt).unwrap();
        std::fs::write(dir.join("authmain"), &blob).unwrap();
        std::fs::write(
            dir.join("localstate"),
            auth_codec::test_local_state_json(&key).unwrap(),
        )
        .unwrap();
        let vk = if variant == QoderVariant::Cn { "cn" } else { "global" };
        let meta = json!({
            "account_id": account_id,
            "variant": vk,
            "target": "desktop",
            "created_at": crate::modules::config::now_ts(),
            "members": [ {
                "role": "auth_main", "file_name": "authmain", "sha256": "00",
                "size": blob.len(), "critical": true
            } ],
            "identity": { "uid": uid, "email": email }
        });
        std::fs::write(dir.join("bundle.json"), serde_json::to_vec_pretty(&meta).unwrap()).unwrap();
    }

    fn block<F: std::future::Future>(fut: F) -> F::Output {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(fut)
    }

    /// 国际版签到全链：CLAIMABLE → claim 成功 → 写设备名额备忘；
    /// 同机另一账号再签 → 不发任何请求直接 limited。
    #[test]
    #[cfg(windows)]
    fn global_checkin_claims_then_locks_other_accounts_out() {
        let _g = MOCK_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let _rg = risk::RISK_TEST_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        risk::clear_risk_cache();
        // 风控发现链封闭 → 必然降级（下面的头断言验证"无 token 但有 MachineId"）。
        risk::set_test_install_bases(Some(vec![]));
        let tmp = temp_store_dir();
        let roots = PathRoots::sandbox(&tmp);
        let store = tmp.join("store");
        std::fs::create_dir_all(&store).unwrap();
        make_decryptable_bundle(&store, "ai-acc-a", QoderVariant::Global, "uid-aaa", "a@x.com");
        make_decryptable_bundle(&store, "ai-acc-b", QoderVariant::Global, "uid-bbb", "b@x.com");

        let server = start_mock(vec![
            (200, campaigns_body("uid-aaa", json!([claimable("camp-1")]))),
            (200, json!({ "benefit": { "amount": 100.0 } }).to_string()),
        ]);
        set_mock_base(Some(format!("http://{}", server.addr)));

        let res = block(checkin(&roots, &store, "ai-acc-a", QoderVariant::Global));
        assert_eq!(res["result"], "success", "{res}");
        assert_eq!(res["claimedAmount"], 100.0, "{res}");
        assert!(
            res.get("riskError").is_some(),
            "风控降级必须如实标注: {res}"
        );

        // 备忘落盘：当日、账号 A。
        let claim = crate::modules::ledger::read_device_claim(&store).expect("成功领取应写名额备忘");
        assert_eq!(claim.day, crate::modules::ledger::qoder_day_key());
        assert_eq!(claim.account_id, "ai-acc-a");

        // 账号 B 再签：不发请求直接 limited（mock 已无 canned 响应，若发了请求会挂起/失败）。
        let res_b = block(checkin(&roots, &store, "ai-acc-b", QoderVariant::Global));
        assert_eq!(res_b["result"], "limited", "{res_b}");
        assert!(
            res_b["message"]
                .as_str()
                .unwrap_or("")
                .contains("a@x.com"),
            "limited 提示要点名占用者: {res_b}"
        );
        // mock 收到的请求应恰好是 A 的两个（campaigns + claim）：B 的 limited
        // 是名额备忘的本地判定，不得再发任何网络请求。
        let mut seen = 0;
        while server.requests.try_recv().is_ok() {
            seen += 1;
        }
        assert_eq!(seen, 2, "B 的 limited 分支不得发请求，A 恰好两个请求");

        // 签到日志：A success / B limited，都是 ai 档。
        let logs = crate::modules::ledger::read_checkin_logs(&store)["logs"]
            .as_array()
            .cloned()
            .unwrap();
        assert_eq!(logs.len(), 2, "{logs:?}");
        assert_eq!(logs[0]["result"], "limited");
        assert_eq!(logs[0]["variant"], "ai");
        assert_eq!(logs[1]["result"], "success");

        set_mock_base(None);
        risk::clear_risk_cache();
        risk::set_test_install_bases(None);
        let _ = std::fs::remove_dir_all(&tmp);
    }

    /// 请求头契约：CN 保持既有最小头集（不叠风控头）；Global 叠加客户端身份
    /// （真实版本号回退 0.3.3、架构_平台口径的 MachineOS、MachineId），风控
    /// 降级时不带 Cosy-MachineToken。
    #[test]
    #[cfg(windows)]
    fn campaign_headers_cn_stays_minimal_and_global_gets_overlay() {
        let _g = MOCK_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let _rg = risk::RISK_TEST_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        risk::clear_risk_cache();
        risk::set_test_install_bases(Some(vec![]));
        let tmp = temp_store_dir();
        let roots = PathRoots::sandbox(&tmp);
        let store = tmp.join("store");
        std::fs::create_dir_all(&store).unwrap();
        make_decryptable_bundle(&store, "cn-acc", QoderVariant::Cn, "uid-cn", "cn@x.com");
        make_decryptable_bundle(&store, "ai-acc", QoderVariant::Global, "uid-g", "g@x.com");

        // CN：campaigns 一发即走，头应保持最小集。
        let server = start_mock(vec![(200, campaigns_body("uid-cn", json!([])))]);
        set_mock_base(Some(format!("http://{}", server.addr)));
        let res = block(get_checkin_status(&roots, &store, "cn-acc", QoderVariant::Cn));
        assert_eq!(res["ok"], true, "{res}");
        let _ = server.last_headers(); // 消费掉这次请求
        set_mock_base(None);

        // Global：同一路由，头应有叠加、无风控 token、带 riskError 说明。
        let server = start_mock(vec![(200, campaigns_body("uid-g", json!([])))]);
        set_mock_base(Some(format!("http://{}", server.addr)));
        let res = block(get_checkin_status(&roots, &store, "ai-acc", QoderVariant::Global));
        assert_eq!(res["ok"], true, "{res}");
        assert!(res.get("riskError").is_some(), "{res}");
        let headers = server.last_headers();
        let get = |name: &str| {
            headers
                .iter()
                .find(|(k, _)| k == name)
                .map(|(_, v)| v.clone())
        };
        assert_eq!(get("cosy-version").as_deref(), Some("0.3.3"), "无 build-manifest 时回退 0.3.3");
        assert_eq!(get("cosy-machineos").as_deref(), Some(machine_os_tag_overlay()));
        assert!(get("cosy-machineid").is_some(), "Global 必须带 MachineId");
        assert!(get("cosy-machinetoken").is_none(), "风控降级时不得带 token 头");
        set_mock_base(None);
        risk::clear_risk_cache();
        risk::set_test_install_bases(None);
        let _ = std::fs::remove_dir_all(&tmp);
    }

    /// 状态机形状：CLAIMED-only → already；无 CLAIM_BENEFIT → inactive（国际版带
    /// 风控降级说明）；campaigns 401 → error。
    #[test]
    #[cfg(windows)]
    fn checkin_result_shapes_are_honest() {
        let _g = MOCK_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let _rg = risk::RISK_TEST_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        risk::clear_risk_cache();
        risk::set_test_install_bases(Some(vec![]));
        let tmp = temp_store_dir();
        let roots = PathRoots::sandbox(&tmp);
        let store = tmp.join("store");
        std::fs::create_dir_all(&store).unwrap();
        make_decryptable_bundle(&store, "ai-acc", QoderVariant::Global, "uid-s", "s@x.com");

        // CLAIMED-only → already。
        let server = start_mock(vec![(200, campaigns_body("uid-s", json!([claimed("c1")])))]);
        set_mock_base(Some(format!("http://{}", server.addr)));
        let res = block(checkin(&roots, &store, "ai-acc", QoderVariant::Global));
        assert_eq!(res["result"], "already", "{res}");
        set_mock_base(None);

        // 无 CLAIM_BENEFIT → inactive + riskError（降级说明必须让人看懂）。
        let server = start_mock(vec![(200, campaigns_body("uid-s", json!([])))]);
        set_mock_base(Some(format!("http://{}", server.addr)));
        let res = block(checkin(&roots, &store, "ai-acc", QoderVariant::Global));
        assert_eq!(res["result"], "inactive", "{res}");
        assert!(res.get("riskError").is_some(), "{res}");
        set_mock_base(None);

        // 401 → error，文案指明重新登录。
        let server = start_mock(vec![(401, json!({ "error": "unauthorized" }).to_string())]);
        set_mock_base(Some(format!("http://{}", server.addr)));
        let res = block(checkin(&roots, &store, "ai-acc", QoderVariant::Global));
        assert_eq!(res["result"], "error", "{res}");
        assert!(
            res["error"].as_str().unwrap_or("").contains("401"),
            "{res}"
        );
        set_mock_base(None);
        risk::clear_risk_cache();
        risk::set_test_install_bases(None);
        let _ = std::fs::remove_dir_all(&tmp);
    }

    /// 档位自愈：包在哪个档位就解析出哪个档位；显式档位优先；找不到回 Cn
    /// （保持既有"local 现场账号"语义）。
    #[test]
    fn resolve_variant_for_account_derives_from_bundle() {
        let tmp = temp_store_dir();
        let store = tmp.join("store");
        std::fs::create_dir_all(&store).unwrap();
        #[cfg(windows)]
        {
            make_decryptable_bundle(&store, "acc-global", QoderVariant::Global, "u1", "g@x.com");
            make_decryptable_bundle(&store, "acc-cn", QoderVariant::Cn, "u2", "c@x.com");
            assert_eq!(
                resolve_variant_for_account(&store, "acc-global", None),
                QoderVariant::Global
            );
            assert_eq!(
                resolve_variant_for_account(&store, "acc-cn", None),
                QoderVariant::Cn
            );
            // 显式档位优先（尊重调用方）。
            assert_eq!(
                resolve_variant_for_account(&store, "acc-global", Some(QoderVariant::Cn)),
                QoderVariant::Cn
            );
        }
        // 任何档位都没有 → 回 Cn（local 现场账号路径的既有缺省）。
        assert_eq!(
            resolve_variant_for_account(&store, "local-cn", None),
            QoderVariant::Cn
        );
        let _ = std::fs::remove_dir_all(&tmp);
    }
}


