//! 轮换建议：按 token 到期紧迫度挑出"该切到哪个账号"。
//!
//! **刻意不自动执行。** 参考实现的轮换只改 CodeBuddy CLI 的默认账号指针，改完不动
//! 任何进程；而 Qoder 这边换号的权威动作是替换桌面端凭据，必须先终止整个 IDE 进程树。
//! 让定时器去重启用户正在用的 IDE 是不可接受的副作用，所以这里只产出建议 + 理由，
//! 由人确认后执行。判定顺序沿用原作的七步策略，去掉本机不适用的两项（心跳存活、
//! 剩余积分价值过滤 —— 后者需要额度接口，尚未取证）。

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::modules::config::{now_ts, PathRoots};
use crate::modules::variant::{QoderTarget, QoderVariant};
use crate::modules::{auth_codec, bundle, process, switch};
use crate::Result;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RotateConfig {
    /// 剩余天数超过该值就不算紧迫，不产生建议。
    pub min_urgency_days: i64,
    /// 目标比当前账号晚到期不到该天数就不切（防来回抖）。
    pub min_gap_days: i64,
}

impl Default for RotateConfig {
    fn default() -> Self {
        Self { min_urgency_days: 7, min_gap_days: 3 }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Candidate {
    pub account_id: String,
    /// token 剩余天数；None = 解不出到期，不参与排序。
    pub days_left: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Decision {
    pub switch_to: Option<String>,
    pub reason: String,
}

impl Decision {
    fn hold(reason: impl Into<String>) -> Self {
        Self { switch_to: None, reason: reason.into() }
    }
}

/// 纯判定。`candidates` 未排序也可以。
pub fn decide(
    candidates: &[Candidate],
    current_days: Option<i64>,
    cfg: &RotateConfig,
) -> Decision {
    let mut usable: Vec<Candidate> = candidates
        .iter()
        .filter(|c| matches!(c.days_left, Some(d) if d > 0))
        .cloned()
        .collect();
    if usable.is_empty() {
        return Decision::hold("没有可判定的账号（都解不出到期时间或已过期）");
    }
    usable.sort_by_key(|c| c.days_left.unwrap_or(i64::MAX));
    let best = &usable[0];
    let best_days = best.days_left.unwrap_or(i64::MAX);

    if best_days > cfg.min_urgency_days {
        return Decision::hold(format!(
            "最紧迫的账号也还剩 {best_days} 天，未达阈值 {} 天，不切",
            cfg.min_urgency_days
        ));
    }
    let cur = match current_days {
        Some(d) => d,
        // 当前账号解不出到期：给建议但讲明依据不足。
        None => {
            return Decision {
                switch_to: Some(best.account_id.clone()),
                reason: format!(
                    "当前账号到期时间未知，最紧迫可用账号 {} 剩 {best_days} 天（建议，依据不足）",
                    best.account_id
                ),
            }
        }
    };
    if cur <= best_days {
        return Decision::hold(format!("当前账号剩 {cur} 天，已是最紧迫或并列，不切"));
    }
    let gap = cur - best_days;
    if gap < cfg.min_gap_days {
        return Decision::hold(format!(
            "目标早 {gap} 天，未达防抖阈值 {} 天，不切",
            cfg.min_gap_days
        ));
    }
    Decision {
        switch_to: Some(best.account_id.clone()),
        reason: format!("当前账号剩 {cur} 天，建议切到 {}（剩 {best_days} 天）", best.account_id),
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct RotateState {
    /// 上次产出建议的时间戳，仅用于展示与去抖日志。
    pub last_suggested_at: Option<String>,
    pub last_suggested_account: Option<String>,
    pub history: Vec<(String, String, String)>,
}

fn state_path(store: &Path) -> PathBuf {
    store.join("rotate_state.json")
}

/// 现场登录态：uid + token 剩余天数（同一次解密，保证两者属于同一个账号）。
pub fn current_scene(roots: &PathRoots, variant: QoderVariant) -> (Option<String>, Option<i64>) {
    match auth_codec::read_desktop_auth(roots, variant) {
        Ok(auth) => {
            let uid = Some(auth.user.id.clone()).filter(|s| !s.trim().is_empty());
            (uid, bundle::days_until(&auth.expires_at))
        }
        Err(_) => (None, None),
    }
}

/// 读当前桌面登录态的 token 剩余天数（用于"当前账号"这一侧的判定）。
pub fn current_days(roots: &PathRoots, variant: QoderVariant) -> Option<i64> {
    current_scene(roots, variant).1
}

/// 收集某版本下所有桌面账号包的到期紧迫度。
pub fn candidates(roots: &PathRoots, variant: QoderVariant) -> Vec<Candidate> {
    let mut out = Vec::new();
    let (live_uid, live_days) = current_scene(roots, variant);
    for b in bundle::list_all(&crate::modules::config::switch_root()) {
        if b.variant != variant || b.target != QoderTarget::Desktop {
            continue;
        }
        let days = b.identity.token_days_left().or_else(|| {
            // 只有确认这个包就是现场登录的那个账号，才允许读现场文件补到期；
            // 否则陌生包会被塞进"当前账号"的天数，decide() 据此把它当成紧迫候选。
            if live_uid.is_some() && b.identity.uid == live_uid {
                live_days
            } else {
                None
            }
        });
        out.push(Candidate { account_id: b.account_id.clone(), days_left: days });
    }
    out
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Suggestion {
    pub variant: QoderVariant,
    pub decision: Decision,
    pub candidates: Vec<Candidate>,
    /// 现场是否可安全执行：目标在跑 + 被托管 → 不能自动做。
    pub executable_from_here: bool,
    pub note: String,
    pub checked_at: String,
}

/// 产出一次建议。只读：不写产品目录，不杀进程。
pub fn suggest(roots: &PathRoots, store: &Path, variant: QoderVariant, cfg: &RotateConfig) -> Result<Suggestion> {
    let cands = candidates(roots, variant);
    let cur = current_days(roots, variant);
    let decision = decide(&cands, cur, cfg);
    // 探测失败按"在跑"处理（保守方向）：建议只会更谨慎，不会催着用户切。
    let running = process::running_pids(QoderTarget::Desktop.images(variant))
        .map_or(true, |p| !p.is_empty());
    let hosted = matches!(
        process::hosted_by(variant, QoderTarget::Desktop),
        process::Hosted::No
    );
    let note = if !running {
        "目标没在跑，可以直接切".into()
    } else if !hosted {
        "本进程被目标客户端托管，从这里执行会连同会话一起终止".into()
    } else {
        let n = process::running_pids(QoderTarget::Desktop.images(variant))
            .map(|p| p.len())
            .unwrap_or(0);
        format!("切换会终止并重开 {:?} 客户端（共 {n} 个进程在跑），需人工确认", variant)
    };
    let s = Suggestion {
        variant,
        decision,
        candidates: cands,
        executable_from_here: hosted,
        note,
        checked_at: now_ts(),
    };
    if let Some(acc) = &s.decision.switch_to {
        let path = state_path(store);
        let mut st = read_state(store).unwrap_or_default();
        st.last_suggested_at = Some(s.checked_at.clone());
        st.last_suggested_account = Some(acc.clone());
        st.history.push((s.checked_at.clone(), acc.clone(), s.decision.reason.clone()));
        if st.history.len() > 50 {
            let cut = st.history.len() - 50;
            st.history.drain(0..cut);
        }
        let json = serde_json::to_vec_pretty(&st).map_err(|e| e.to_string())?;
        crate::modules::config::atomic_write_bytes(&path, &json)
            .map_err(|e| format!("写轮换状态失败: {e}"))?;
    }
    Ok(s)
}

pub fn read_state(store: &Path) -> Result<RotateState> {
    let path = state_path(store);
    match std::fs::read(&path) {
        Ok(bytes) => serde_json::from_slice(&bytes).map_err(|e| format!("轮换状态解析失败: {e}")),
        Err(_) => Ok(RotateState::default()),
    }
}

/// 用户确认后执行建议。走的就是普通切换路径，所有安全门一个不少。
pub fn apply(roots: &PathRoots, store: &Path, s: &Suggestion, restart: bool) -> Result<switch::Journal> {
    let acc = s
        .decision
        .switch_to
        .clone()
        .ok_or_else(|| "当前没有可执行的轮换建议".to_string())?;
    let req = switch::Request {
        account_id: acc,
        variant: s.variant,
        target: QoderTarget::Desktop,
        restart,
    };
    switch::execute(roots, store, &req, switch::Actor::Real, &mut |_| {})
}

#[cfg(test)]
mod tests {
    use super::*;

    fn c(id: &str, d: Option<i64>) -> Candidate {
        Candidate { account_id: id.into(), days_left: d }
    }

    const CFG: RotateConfig = RotateConfig { min_urgency_days: 7, min_gap_days: 3 };

    fn hold_reason(d: &Decision) -> String {
        assert!(d.switch_to.is_none(), "不该建议切: {:?}", d.switch_to);
        d.reason.clone()
    }

    #[test]
    fn no_candidates_holds() {
        let d = decide(&[], Some(3), &CFG);
        assert!(hold_reason(&d).contains("没有可判定"));
    }

    #[test]
    fn not_urgent_holds_even_if_another_account_is_less_urgent() {
        let d = decide(&[c("a", Some(30)), c("b", Some(20))], Some(40), &CFG);
        assert!(hold_reason(&d).contains("未达阈值"), "{}", d.reason);
    }

    #[test]
    fn expired_and_undecodable_candidates_are_ignored() {
        let d = decide(&[c("过期", Some(-2)), c("未知", None)], Some(1), &CFG);
        assert!(hold_reason(&d).contains("没有可判定"), "{}", d.reason);
    }

    #[test]
    fn urgent_switch_is_suggested() {
        let d = decide(&[c("急", Some(1)), c("缓", Some(40))], Some(40), &CFG);
        assert_eq!(d.switch_to.as_deref(), Some("急"));
        assert!(d.reason.contains("建议切到 急"), "{}", d.reason);
    }

    #[test]
    fn already_target_holds() {
        let d = decide(&[c("a", Some(2)), c("b", Some(9))], Some(2), &CFG);
        assert!(hold_reason(&d).contains("已是最紧迫"), "{}", d.reason);
    }

    #[test]
    fn anti_flapping_gap_holds() {
        // 当前 5 天、目标 3 天：只差 2 天 < min_gap_days 3 → 不切。
        let d = decide(&[c("a", Some(3))], Some(5), &CFG);
        assert!(hold_reason(&d).contains("防抖"), "{}", d.reason);
    }

    #[test]
    fn unknown_current_still_suggests_with_caveat() {
        let d = decide(&[c("a", Some(2))], None, &CFG);
        assert_eq!(d.switch_to.as_deref(), Some("a"));
        assert!(d.reason.contains("依据不足"), "{}", d.reason);
    }

    #[test]
    fn state_roundtrip_and_history_cap() {
        let dir = std::env::temp_dir().join(format!("qs-rotate-{}", uuid::Uuid::new_v4().simple()));
        std::fs::create_dir_all(&dir).unwrap();
        assert!(read_state(&dir).unwrap().history.is_empty(), "无状态文件应回退默认");

        let st = RotateState {
            last_suggested_at: Some("t".into()),
            last_suggested_account: Some("a".into()),
            history: (0..60).map(|i| (format!("t{i}"), "a".into(), "r".into())).collect(),
        };
        let json = serde_json::to_vec(&st).unwrap();
        crate::modules::config::atomic_write_bytes(&state_path(&dir), &json).unwrap();
        let back = read_state(&dir).unwrap();
        assert_eq!(back.last_suggested_account.as_deref(), Some("a"));
        assert_eq!(back.history.len(), 60);
        std::fs::remove_dir_all(dir).ok();
    }
}
