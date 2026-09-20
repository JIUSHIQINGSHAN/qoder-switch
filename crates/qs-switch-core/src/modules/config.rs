//! 路径解析、原子写、哈希与时间戳。档位相关字面量一律不在本模块出现。

use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};

/// 工具私有数据根。刻意与参考实现的 `~/.wb-switch` 分开，避免与已安装的
/// workbuddy-switch 串数据。
pub fn switch_root() -> PathBuf {
    if let Ok(v) = std::env::var("QS_SWITCH_ROOT") {
        if !v.trim().is_empty() {
            return PathBuf::from(v);
        }
    }
    home_dir().join(".qs-switch")
}

pub fn home_dir() -> PathBuf {
    dirs::home_dir().unwrap_or_else(|| PathBuf::from("."))
}

/// Windows 上是 `%APPDATA%`（Roaming），macOS 是 `~/Library/Application Support`。
pub fn roaming_app_data() -> PathBuf {
    dirs::config_dir().unwrap_or_else(|| home_dir().join("AppData").join("Roaming"))
}

/// Windows 上是 `%LOCALAPPDATA%`，其它平台退化为 config_dir。
pub fn local_app_data() -> PathBuf {
    #[cfg(target_os = "windows")]
    {
        if let Ok(v) = std::env::var("LOCALAPPDATA") {
            if !v.trim().is_empty() {
                return PathBuf::from(v);
            }
        }
    }
    dirs::config_dir().unwrap_or_else(|| home_dir().join(".local"))
}

pub fn snapshots_dir() -> PathBuf {
    switch_root().join("snapshots")
}

pub fn backups_dir() -> PathBuf {
    switch_root().join("backups")
}

/// UTC 时间戳，用于备份文件命名；不含 `:` 与 `.`，跨平台文件名安全。
pub fn now_ts() -> String {
    chrono::Utc::now().format("%Y%m%dT%H%M%SZ").to_string()
}

/// 原子写：同目录临时文件 + rename。目标目录由调用方保证存在。
pub fn atomic_write_bytes(path: &Path, content: &[u8]) -> std::io::Result<()> {
    let stem = path
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| "file".to_string());
    let tmp = path.with_file_name(format!("{stem}.tmp-{}", uuid::Uuid::new_v4().simple()));
    std::fs::write(&tmp, content).map_err(|e| {
        let _ = std::fs::remove_file(&tmp);
        e
    })?;
    std::fs::rename(&tmp, path).map_err(|e| {
        let _ = std::fs::remove_file(&tmp);
        e
    })
}

pub fn read_bytes(path: &Path) -> std::io::Result<Vec<u8>> {
    std::fs::read(path)
}

/// 流式无关的小文件哈希；读不到（锁/权限）返回 Err 而不是伪造摘要。
pub fn sha256_hex(path: &Path) -> std::io::Result<String> {
    let bytes = std::fs::read(path)?;
    let mut h = Sha256::new();
    h.update(&bytes);
    Ok(hex::encode(h.finalize()))
}

pub fn mtime_ms(path: &Path) -> std::io::Result<u64> {
    let t = std::fs::metadata(path)?.modified()?;
    Ok(t.duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0))
}

/// 当前 Unix 毫秒时间戳（通知存档等本地时间戳用）。
pub fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// 路径根的三个轴。生产用 `real()`，测试与演练用 `sandbox()`，
/// 这样凭据布局可以在临时目录里整棵重建，绝不会碰到真实产品目录。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PathRoots {
    pub home: PathBuf,
    pub roaming: PathBuf,
    pub local: PathBuf,
}

impl PathRoots {
    pub fn real() -> Self {
        Self {
            home: home_dir(),
            roaming: roaming_app_data(),
            local: local_app_data(),
        }
    }

    /// 把 home / roaming / local 全塞进一个目录，便于做端到端演练。
    pub fn sandbox(root: &Path) -> Self {
        Self {
            home: root.to_path_buf(),
            roaming: root.to_path_buf(),
            local: root.to_path_buf(),
        }
    }
}

/// 在 `ini` 文本里取键值（Launcher 的 state.ini 无多 section，故只按 `key=value`
/// 扫描）。容忍 `=` 两侧空格与 CRLF，跳过 `;`/`#` 注释 —— state.ini 可能被手改。
pub fn ini_get(ini: &str, key: &str) -> Option<String> {
    ini.lines().find_map(|line| {
        let line = line.trim();
        if line.is_empty() || line.starts_with(';') || line.starts_with('#') || line.starts_with('[') {
            return None;
        }
        let (k, v) = line.split_once('=')?;
        (k.trim() == key)
            .then(|| v.trim().to_string())
            .filter(|v| !v.is_empty())
    })
}
