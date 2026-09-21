//! 凭据文件快照与差分。它同时承担两个角色：
//! 1. 门控实验的观测手段 —— 换号前后各拍一张，diff 出"哪些文件构成换号的充分集"；
//! 2. 产品备份/回滚的前身 —— 备份单元就是这份清单里的 `critical` 集合。
//!
//! 本模块**只读**：绝不写入任何 Qoder 产品目录，快照只落在 `~/.qs-switch/snapshots/`。

use std::collections::BTreeMap;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::modules::config::{
    mtime_ms, now_ts, read_bytes, sha256_hex, snapshots_dir, PathRoots,
};
use crate::modules::variant::{credentials, QoderTarget, QoderVariant};

/// 快照目录最多保留的历史记录数量，防止无界刷盘堆积磁盘。
pub const MAX_SNAPSHOTS_RETAINED: usize = 30;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Entry {
    pub variant: QoderVariant,
    pub target: QoderTarget,
    pub role: crate::modules::variant::FileRole,
    pub path: PathBuf,
    pub critical: bool,
    pub exists: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub size: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mtime_ms: Option<u64>,
    /// 小文件全文哈希。读失败（进程占用/权限）时记在 `error` 而不是伪造摘要。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sha256: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Snapshot {
    pub taken_at: String,
    pub entries: Vec<Entry>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ChangeKind {
    /// 换号后新出现的凭据文件。
    Appeared,
    Disappeared,
    Modified,
    /// 仅 mtime 变了，内容哈希一致（Qoder 会高频重写回显文件，属噪声）。
    TouchOnly,
    Unchanged,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Change {
    pub kind: ChangeKind,
    pub variant: QoderVariant,
    pub target: QoderTarget,
    pub role: crate::modules::variant::FileRole,
    pub path: PathBuf,
    pub critical: bool,
}

fn key(e: &Entry) -> (QoderVariant, QoderTarget, crate::modules::variant::FileRole) {
    (e.variant, e.target, e.role)
}

impl Snapshot {
    /// 对本机全部 (版本 × 目标) 组合拍快照。
    pub fn take() -> Self {
        Self::take_with(&PathRoots::real())
    }

    /// 快照全部 (版本 × 目标) 组合。沙箱演练时传入 `PathRoots::sandbox`。
    pub fn take_with(roots: &PathRoots) -> Self {
        let mut entries = Vec::new();
        for v in QoderVariant::ALL {
            for t in QoderTarget::ALL {
                for f in credentials(roots, v, t) {
                    entries.push(entry_of(&f));
                }
            }
        }
        Snapshot {
            taken_at: now_ts(),
            entries,
        }
    }

    pub fn save(&self) -> std::io::Result<PathBuf> {
        self.save_in(&snapshots_dir())
    }

    /// 保存快照到指定目录，并自动修剪超过 `MAX_SNAPSHOTS_RETAINED` 的历史快照。
    pub fn save_in(&self, dir: &std::path::Path) -> std::io::Result<PathBuf> {
        std::fs::create_dir_all(dir)?;
        let path = dir.join(format!("{}.json", self.taken_at));
        // 与全库其它写入同一条原子路径：快照中途被杀不能留下截断的 json，
        // 否则 latest() 从此每次都失败（含每次 selfcheck）。
        crate::modules::config::atomic_write_bytes(
            &path,
            &serde_json::to_vec_pretty(self).unwrap_or_default(),
        )?;

        // 修剪旧快照：按文件名字典序升序（最旧的排在前面），超出上限则删除多余文件
        if let Ok(entries) = std::fs::read_dir(dir) {
            let mut files: Vec<PathBuf> = entries
                .filter_map(|e| e.ok().map(|e| e.path()))
                .filter(|p| p.extension().map(|x| x == "json").unwrap_or(false))
                .collect();
            files.sort();
            if files.len() > MAX_SNAPSHOTS_RETAINED {
                let to_remove = files.len() - MAX_SNAPSHOTS_RETAINED;
                for p in files.into_iter().take(to_remove) {
                    let _ = std::fs::remove_file(p);
                }
            }
        }

        Ok(path)
    }

    pub fn load(path: &PathBuf) -> std::io::Result<Self> {
        let bytes = read_bytes(path)?;
        serde_json::from_slice(&bytes)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))
    }

    /// 最新一张快照（按文件名里的 UTC 时间戳，字典序即时间序）。
    ///
    /// 解析失败的文件（历史残留/截断）跳过而不是硬失败：快照是辅助观测，
    /// 不该因为一张坏文件让 snapshot_now / selfcheck 永久报错。
    pub fn latest() -> std::io::Result<Option<Self>> {
        Self::latest_in(&snapshots_dir())
    }

    /// 从指定目录获取最新快照（单测沙箱与隔离使用）。
    pub fn latest_in(dir: &std::path::Path) -> std::io::Result<Option<Self>> {
        if !dir.is_dir() {
            return Ok(None);
        }
        let mut names: Vec<PathBuf> = std::fs::read_dir(dir)?
            .filter_map(|e| e.ok().map(|e| e.path()))
            .filter(|p| p.extension().map(|x| x == "json").unwrap_or(false))
            .collect();
        names.sort();
        while let Some(p) = names.pop() {
            match Self::load(&p) {
                Ok(s) => return Ok(Some(s)),
                Err(_) => {
                    // 坏文件静默跳过继续找前一张，不污染控制台输出
                }
            }
        }
        Ok(None)
    }

    /// `self` 为前，`other` 为后。只报有变化的项，`Unchanged` 不出现。
    pub fn diff(&self, other: &Snapshot) -> Vec<Change> {
        let a: BTreeMap<_, _> = self.entries.iter().map(|e| (key(e), e)).collect();
        let b: BTreeMap<_, _> = other.entries.iter().map(|e| (key(e), e)).collect();
        let mut out = Vec::new();
        let mut keys: std::collections::BTreeSet<_> = std::collections::BTreeSet::new();
        keys.extend(a.keys().cloned());
        keys.extend(b.keys().cloned());
        for k in keys {
            let (va, vb) = (a.get(&k), b.get(&k));
            let (variant, target, role) = k;
            let critical = vb.or(va).map(|e| e.critical).unwrap_or(false);
            let path = vb
                .or(va)
                .map(|e| e.path.clone())
                .unwrap_or_else(PathBuf::new);
            let kind = match (va, vb) {
                (None, Some(n)) if n.exists => ChangeKind::Appeared,
                (Some(p), None) if p.exists => ChangeKind::Disappeared,
                (Some(p), Some(n)) => {
                    if p.exists != n.exists {
                        if n.exists {
                            ChangeKind::Appeared
                        } else {
                            ChangeKind::Disappeared
                        }
                    } else if !p.exists {
                        // 两边都不存在：无变化，跳过。
                        continue;
                    } else if p.sha256 != n.sha256 {
                        ChangeKind::Modified
                    } else if p.mtime_ms != n.mtime_ms || p.size != n.size {
                        ChangeKind::TouchOnly
                    } else {
                        continue;
                    }
                }
                _ => continue,
            };
            out.push(Change {
                kind,
                variant,
                target,
                role,
                path,
                critical,
            });
        }
        out.sort_by_key(|c| (c.kind != ChangeKind::Modified, !c.critical, c.target, c.variant));
        out
    }
}

fn entry_of(f: &crate::modules::variant::CredentialFile) -> Entry {
    let mut e = Entry {
        variant: f.variant,
        target: f.target,
        role: f.role,
        path: f.path.clone(),
        critical: f.critical,
        exists: false,
        size: None,
        mtime_ms: None,
        sha256: None,
        error: None,
    };
    match std::fs::metadata(&f.path) {
        Ok(md) => {
            e.exists = md.is_file();
            e.size = Some(md.len());
            e.mtime_ms = mtime_ms(&f.path).ok();
        }
        Err(err) => {
            e.error = Some(err.to_string());
            return e;
        }
    }
    if e.exists {
        match sha256_hex(&f.path) {
            Ok(h) => e.sha256 = Some(h),
            Err(err) => e.error = Some(err.to_string()),
        }
    }
    e
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::modules::variant::desktop_dir;

    #[test]
    fn take_and_diff_roundtrip_detects_no_change() {
        let root = std::env::temp_dir().join(format!("qs-snap-test-{}", now_ts()));
        std::env::set_var("QS_SWITCH_ROOT", &root);
        let a = Snapshot::take();
        assert!(!a.entries.is_empty());
        assert!(a.diff(&a).is_empty(), "同一快照自比不应有变化");
        std::env::remove_var("QS_SWITCH_ROOT");
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn diff_reports_appeared_modified_and_touch_only() {
        let entry = |exists: bool, sha: Option<&str>, size: Option<u64>| Entry {
            variant: QoderVariant::Cn,
            target: QoderTarget::Desktop,
            role: crate::modules::variant::FileRole::AuthMain,
            path: PathBuf::from("auth.v1.dat"),
            critical: true,
            exists,
            size,
            mtime_ms: Some(1),
            sha256: sha.map(|s| s.to_string()),
            error: None,
        };
        let mk = |v: Entry| Snapshot {
            taken_at: "t".into(),
            entries: vec![v],
        };

        let d = mk(entry(false, None, None)).diff(&mk(entry(true, Some("aa"), Some(3))));
        assert!(matches!(d.first().map(|c| c.kind), Some(ChangeKind::Appeared)));

        let d = mk(entry(true, Some("aa"), Some(3))).diff(&mk(entry(true, Some("bb"), Some(3))));
        assert!(matches!(d.first().map(|c| c.kind), Some(ChangeKind::Modified)));

        let d = mk(entry(true, Some("aa"), Some(3))).diff(&mk(entry(true, Some("aa"), Some(4))));
        assert!(matches!(
            d.first().map(|c| c.kind),
            Some(ChangeKind::TouchOnly)
        ));

        let d = mk(entry(true, Some("aa"), Some(3))).diff(&mk(entry(false, None, None)));
        assert!(matches!(
            d.first().map(|c| c.kind),
            Some(ChangeKind::Disappeared)
        ));
    }

    /// 换号充分集的候选集必须全部落在 critical 标记上，观测项不得混入。
    #[test]
    fn status_echo_is_never_critical() {
        let roots = PathRoots::real();
        for v in QoderVariant::ALL {
            for t in QoderTarget::ALL {
                for f in credentials(&roots, v, t) {
                    if matches!(
                        f.role,
                        crate::modules::variant::FileRole::StatusEcho
                            | crate::modules::variant::FileRole::ChannelActivation
                            | crate::modules::variant::FileRole::DesktopMachineId
                    ) {
                        assert!(!f.critical, "{:?} 不应是 critical", f.role);
                    }
                }
            }
        }
    }

    #[test]
    fn save_prunes_old_snapshots_beyond_capacity() {
        let tmp = std::env::temp_dir().join(format!("qs-snap-test-{}", uuid::Uuid::new_v4().simple()));
        std::fs::create_dir_all(&tmp).unwrap();

        // 模拟连续存入 35 张快照
        for i in 0..35 {
            let snap = Snapshot {
                taken_at: format!("20260921T1200{:02}Z", i),
                entries: vec![],
            };
            snap.save_in(&tmp).unwrap();
        }

        let files: Vec<_> = std::fs::read_dir(&tmp)
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| e.path().extension().map(|x| x == "json").unwrap_or(false))
            .collect();
        assert_eq!(
            files.len(),
            MAX_SNAPSHOTS_RETAINED,
            "快照文件数量应被严格限制在上限内"
        );

        // 最新的快照（20260921T120034Z）应存在，最早的（00..04）应已被修剪
        let latest = Snapshot::latest_in(&tmp).unwrap().expect("有最新快照");
        assert_eq!(latest.taken_at, "20260921T120034Z");

        let _ = std::fs::remove_dir_all(tmp);
    }

    /// 沙箱里造一个"换号"：只动 critical 文件就必须被判定为 Modified。
    #[test]
    fn sandbox_detects_credential_swap() {
        let dir = std::env::temp_dir().join(format!("qs-sandbox-{}", now_ts()));
        let roots = PathRoots::sandbox(&dir);
        std::fs::create_dir_all(desktop_dir(&roots, QoderVariant::Cn)).unwrap();
        let auth = desktop_dir(&roots, QoderVariant::Cn).join("auth.v1.dat");
        std::fs::write(&auth, b"account-A").unwrap();

        let before = Snapshot::take_with(&roots);
        std::fs::write(&auth, b"account-B").unwrap();
        let after = Snapshot::take_with(&roots);

        let d = before.diff(&after);
        let hit = d
            .iter()
            .find(|c| c.role == crate::modules::variant::FileRole::AuthMain)
            .expect("应检出 auth.v1.dat 变化");
        assert_eq!(hit.kind, ChangeKind::Modified);
        assert!(hit.critical);

        std::fs::remove_dir_all(&dir).ok();
    }
}
