//! 应用内通知（toast）存档：最近 [`NOTIFICATION_LIMIT`] 条，落盘在工具存储根。
//!
//! 用途：提示是一次性的（sonner toast 存活几秒），事后无法回看。存档让用户与排障者
//! 能核对「应用当时到底提示了什么」——例如切号成功后是否出现过可还原路径、清理失败
//! 的待清理提示等。存档是尽力而为：写入失败不影响提示本身，读取失败不阻塞界面。
//!
//! 边界：只存本机（`switch_root()/notifications.json`），不做云同步；内容可能包含账号
//! 昵称与本地路径，属本机明文数据，UI 里如实说明。

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

use crate::modules::config::{atomic_write_bytes, now_ms, switch_root};

/// 保留的最近通知条数。
pub const NOTIFICATION_LIMIT: usize = 100;
/// 存档格式版本；读到其它版本视为不可用（保留原文件，不覆盖）。
pub const NOTIFICATION_VERSION: u32 = 1;
/// 标题/描述的长度上限（按字符截断，避免一条异常提示把存档撑爆）。
const TITLE_LIMIT: usize = 200;
const DESCRIPTION_LIMIT: usize = 2000;
/// 同一提示在该窗口内重复出现时只记一条（去抖；不改变 toast 本身的显示）。
const DEDUPE_WINDOW_MS: i64 = 2_000;
const STORE_FILE_NAME: &str = "notifications.json";

/// 一条通知：级别、标题、可选描述与发生时间。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NotificationEntry {
    /// `success` | `error` | `warning` | `info`。
    pub level: String,
    pub title: String,
    #[serde(default)]
    pub description: Option<String>,
    pub at: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct NotificationStore {
    version: u32,
    #[serde(default)]
    items: Vec<NotificationEntry>,
}

impl Default for NotificationStore {
    fn default() -> Self {
        Self {
            version: NOTIFICATION_VERSION,
            items: Vec::new(),
        }
    }
}

fn store_file_at(root: &Path) -> PathBuf {
    root.join(STORE_FILE_NAME)
}

/// 读取存档；缺失返回空，损坏/版本不符返回 Err（不覆盖原文件）。
fn load_at(root: &Path) -> Result<NotificationStore, String> {
    let file = store_file_at(root);
    let text = match std::fs::read_to_string(&file) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(NotificationStore::default())
        }
        Err(error) => return Err(format!("通知存档不可读：{error}")),
    };
    let store: NotificationStore =
        serde_json::from_str(&text).map_err(|error| format!("通知存档内容损坏：{error}"))?;
    if store.version != NOTIFICATION_VERSION {
        return Err(format!(
            "通知存档版本 {} 不受支持（当前支持 {}），已保留原文件",
            store.version, NOTIFICATION_VERSION
        ));
    }
    Ok(store)
}

/// 按字符截断（超长时补省略号），避免单条提示过大。
fn trim_text(text: &str, limit: usize) -> String {
    if text.chars().count() <= limit {
        return text.to_string();
    }
    let mut trimmed: String = text.chars().take(limit).collect();
    trimmed.push('…');
    trimmed
}

/// 存档读-改-写的进程内串行闸。
///
/// record/clear 都是 load→append→全量覆盖写；桌面端与 webui 服务端各自的工作线程
/// 会并发跑（两条 toast 同时到、或 record 与 clear 交错），没有这道闸就是后写者覆盖
/// 前者、静默丢掉审计条目——而"事后能核对应用当时提示了什么"正是本模块的存在意义。
/// 跨进程（桌面 + 独立服务端同时在写）不在本闸范围，靠 atomic 写保证至少不留撕裂文件。
static NOTIFY_GATE: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// 记录一条通知（可注入存储根，供单测使用）。
///
/// 同一条提示在 [`DEDUPE_WINDOW_MS`] 内重复出现时只保留一条。返回 Err 表示没写进去
/// （存档损坏、权限失败等）——调用方应忽略并保证提示照常显示。
pub fn record_at(
    root: &Path,
    level: &str,
    title: &str,
    description: Option<&str>,
) -> Result<(), String> {
    let title = title.trim();
    if title.is_empty() {
        return Ok(());
    }
    // 持锁跨越整个 load→改→写；毒锁照常放行（上一位 panic 不该永久卡死存档）。
    let _gate = NOTIFY_GATE.lock().unwrap_or_else(|p| p.into_inner());
    let mut store = match load_at(root) {
        Ok(s) => s,
        Err(_) => {
            // 损坏自愈：将损坏文件重命名隔离，不阻塞后续通知写入
            let file = store_file_at(root);
            if file.is_file() {
                let corrupt = root.join(format!(
                    "{STORE_FILE_NAME}.corrupt-{}",
                    uuid::Uuid::new_v4().simple()
                ));
                let _ = std::fs::rename(&file, &corrupt);
            }
            NotificationStore::default()
        }
    };
    let entry = NotificationEntry {
        level: level.to_string(),
        title: trim_text(title, TITLE_LIMIT),
        description: description
            .map(str::trim)
            .filter(|text| !text.is_empty())
            .map(|text| trim_text(text, DESCRIPTION_LIMIT)),
        at: now_ms(),
    };
    let duplicate = store.items.last().is_some_and(|last| {
        last.level == entry.level
            && last.title == entry.title
            && last.description == entry.description
            && entry.at.saturating_sub(last.at) < DEDUPE_WINDOW_MS
    });
    if duplicate {
        return Ok(());
    }
    store.items.push(entry);
    if store.items.len() > NOTIFICATION_LIMIT {
        let drop_count = store.items.len() - NOTIFICATION_LIMIT;
        store.items.drain(0..drop_count);
    }
    let content = serde_json::to_string_pretty(&store).map_err(|error| error.to_string())?;
    atomic_write_bytes(&store_file_at(root), content.as_bytes())
        .map_err(|error| format!("通知存档写入失败：{error}"))
}

/// 读取最近的通知（新的在前）。
pub fn list_at(root: &Path) -> Result<Vec<NotificationEntry>, String> {
    let mut items = load_at(root)?.items;
    items.reverse();
    Ok(items)
}

/// 清空存档（只保留版本字段）。与 record 共用同一把闸，避免"清空被并发的写入复活"。
pub fn clear_at(root: &Path) -> Result<(), String> {
    let _gate = NOTIFY_GATE.lock().unwrap_or_else(|p| p.into_inner());
    let content =
        serde_json::to_string_pretty(&NotificationStore::default()).map_err(|e| e.to_string())?;
    atomic_write_bytes(&store_file_at(root), content.as_bytes())
        .map_err(|error| format!("通知存档写入失败：{error}"))
}

/// 生产入口：记录到工具存储根。
pub fn record(level: &str, title: &str, description: Option<&str>) -> Result<(), String> {
    record_at(&switch_root(), level, title, description)
}

/// 生产入口：读取通知存档（新的在前）。
pub fn list() -> Result<Vec<NotificationEntry>, String> {
    list_at(&switch_root())
}

/// 生产入口：清空通知存档。
pub fn clear() -> Result<(), String> {
    clear_at(&switch_root())
}

#[cfg(test)]
mod tests {
    use super::*;

    struct TempDir(PathBuf);

    impl TempDir {
        fn new(name: &str) -> Self {
            let dir = std::env::temp_dir().join(format!(
                "qs_switch_notify_test_{}_{name}",
                uuid::Uuid::new_v4().simple()
            ));
            std::fs::create_dir_all(&dir).unwrap();
            Self(dir)
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn trim_text_truncates_on_char_boundary() {
        assert_eq!(trim_text("短文本", 10), "短文本");
        let trimmed = trim_text(&"汉".repeat(20), 5);
        assert_eq!(trimmed.chars().count(), 6, "5 个字符 + 省略号");
        assert!(trimmed.ends_with('…'));
    }

    #[test]
    fn record_appends_newest_first_and_ignores_empty_titles() {
        let dir = TempDir::new("append");
        record_at(&dir.0, "success", "第一条", None).unwrap();
        record_at(&dir.0, "error", "第二条", Some("原因")).unwrap();
        record_at(&dir.0, "info", "   ", None).unwrap();

        let items = list_at(&dir.0).unwrap();
        assert_eq!(items.len(), 2, "空标题不记录");
        assert_eq!(items[0].title, "第二条");
        assert_eq!(items[0].description.as_deref(), Some("原因"));
        assert_eq!(items[1].title, "第一条");
        assert!(items[0].at >= items[1].at);
    }

    #[test]
    fn identical_notice_within_window_is_deduped() {
        let dir = TempDir::new("dedupe");
        record_at(&dir.0, "warning", "轮换推迟", Some("渠道限流")).unwrap();
        record_at(&dir.0, "warning", "轮换推迟", Some("渠道限流")).unwrap();
        assert_eq!(list_at(&dir.0).unwrap().len(), 1);

        // 内容不同或窗口外照常记录。
        record_at(&dir.0, "warning", "轮换推迟", Some("另一种原因")).unwrap();
        assert_eq!(list_at(&dir.0).unwrap().len(), 2);
    }

    #[test]
    fn ring_keeps_only_the_latest_entries() {
        let dir = TempDir::new("ring");
        for index in 0..NOTIFICATION_LIMIT + 5 {
            record_at(&dir.0, "info", &format!("提示 {index}"), None).unwrap();
        }
        let items = list_at(&dir.0).unwrap();
        assert_eq!(items.len(), NOTIFICATION_LIMIT);
        assert_eq!(items[0].title, format!("提示 {}", NOTIFICATION_LIMIT + 4));
        assert_eq!(items.last().unwrap().title, "提示 5");
    }

    #[test]
    fn long_text_is_trimmed() {
        let dir = TempDir::new("trim");
        let long = "汉".repeat(TITLE_LIMIT + 50);
        record_at(
            &dir.0,
            "info",
            &long,
            Some(&"描".repeat(DESCRIPTION_LIMIT + 10)),
        )
        .unwrap();
        let items = list_at(&dir.0).unwrap();
        assert_eq!(items[0].title.chars().count(), TITLE_LIMIT + 1);
        assert_eq!(
            items[0].description.as_ref().unwrap().chars().count(),
            DESCRIPTION_LIMIT + 1
        );
    }

    #[test]
    fn damaged_store_is_quarantined_and_healed() {
        let dir = TempDir::new("damaged");
        let file = store_file_at(&dir.0);
        std::fs::write(&file, b"{not json").unwrap();
        // 损坏的旧文件自愈：原损坏文件被隔离重命名，写入成功自愈
        let res = record_at(&dir.0, "info", "新提示", None);
        assert!(res.is_ok(), "通知记录应能从损坏文件中自愈");
        let list = list_at(&dir.0).unwrap();
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].title, "新提示");

        // 检查隔离的 corrupt 文件是否存在
        let has_corrupt = std::fs::read_dir(&dir.0)
            .unwrap()
            .filter_map(|e| e.ok())
            .any(|e| e.file_name().to_string_lossy().contains("corrupt"));
        assert!(has_corrupt, "原损坏文件必须被隔离保留");
    }

    #[test]
    fn clear_empties_the_archive() {
        let dir = TempDir::new("clear");
        record_at(&dir.0, "success", "提示", None).unwrap();
        clear_at(&dir.0).unwrap();
        assert!(list_at(&dir.0).unwrap().is_empty());
    }

    #[test]
    fn concurrent_record_under_notify_gate_does_not_corrupt() {
        use std::sync::Arc;
        let dir = Arc::new(TempDir::new("concurrent"));
        let mut handles = Vec::new();
        for i in 0..10 {
            let dir_clone = Arc::clone(&dir);
            handles.push(std::thread::spawn(move || {
                let _ = record_at(&dir_clone.0, "info", &format!("并发提示_{i}"), None);
            }));
        }
        for h in handles {
            let _ = h.join();
        }
        let items = list_at(&dir.0).expect("并发写入后存档依然合法");
        assert!(!items.is_empty(), "至少记录了一条通知");
    }
}
