//! 路径解析、原子写、哈希与时间戳。档位相关字面量一律不在本模块出现。

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use sha2::{Digest, Sha256};

/// HTTP 请求的默认 UA：部分公开端点（如 GitHub 资产）会拒绝无 UA 的默认客户端。
pub const DEFAULT_HTTP_USER_AGENT: &str = "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/152.0.0.0 Safari/537.36";

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

/// 原子写：同目录临时文件 + rename。目标目录自动创建。
///
/// rename 前先把数据 `sync_all` 推到盘上：journal、备份清单这些崩溃恢复依据
/// 都走这里，断电后"原子"文件绝不能是 0 字节或截断的。
pub fn atomic_write_bytes(path: &Path, content: &[u8]) -> std::io::Result<()> {
    use std::io::Write;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let stem = path
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| "file".to_string());
    let tmp = path.with_file_name(format!("{stem}.tmp-{}", uuid::Uuid::new_v4().simple()));
    let write = || -> std::io::Result<()> {
        let mut f = std::fs::File::create(&tmp)?;
        f.write_all(content)?;
        f.sync_all()?;
        Ok(())
    };
    write().map_err(|e| {
        let _ = std::fs::remove_file(&tmp);
        e
    })?;
    let mut last_err = None;
    for attempt in 0..5 {
        match std::fs::rename(&tmp, path) {
            Ok(()) => return Ok(()),
            Err(e) => {
                // 如果目标文件存在且带只读属性（Windows常见），尝试清除只读位后重试
                if e.kind() == std::io::ErrorKind::PermissionDenied && path.is_file() {
                    if let Ok(mut perms) = std::fs::metadata(path).map(|m| m.permissions()) {
                        perms.set_readonly(false);
                        let _ = std::fs::set_permissions(path, perms);
                        if std::fs::rename(&tmp, path).is_ok() {
                            return Ok(());
                        }
                    }
                }
                last_err = Some(e);
                if attempt < 4 {
                    std::thread::sleep(std::time::Duration::from_millis(15 * (attempt + 1) as u64));
                }
            }
        }
    }
    let _ = std::fs::remove_file(&tmp);
    Err(last_err.unwrap())
}

pub fn read_bytes(path: &Path) -> std::io::Result<Vec<u8>> {
    std::fs::read(path)
}

/// 流式无关的小文件哈希；读不到（锁/权限）返回 Err 而不是伪造摘要。
pub fn sha256_hex(path: &Path) -> std::io::Result<String> {
    let bytes = std::fs::read(path)?;
    Ok(sha256_hex_bytes(&bytes))
}

/// 字节串的 SHA-256 十六进制摘要（校验内存里的数据时用，不必绕道磁盘）。
pub fn sha256_hex_bytes(bytes: &[u8]) -> String {
    let mut h = Sha256::new();
    h.update(bytes);
    hex::encode(h.finalize())
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

/// 当前 Unix 秒级时间戳（更新检查缓存用）。
pub fn now_secs() -> i64 {
    now_ms() / 1000
}

static HTTP_CLIENT: OnceLock<reqwest::Client> = OnceLock::new();

fn http_client_builder() -> reqwest::ClientBuilder {
    reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(30))
        .user_agent(DEFAULT_HTTP_USER_AGENT)
}

fn http_client() -> &'static reqwest::Client {
    HTTP_CLIENT.get_or_init(|| {
        http_client_builder()
            .build()
            .expect("failed to build reqwest client")
    })
}

/// 通用 HTTP 请求，可为单次请求显式指定 HTTP/HTTPS 代理。
///
/// 2xx：解析 body 为 JSON；HTTP 错误：body 可解析则返回其 JSON，
/// 否则 `{"code": <status>, "message": <body 前 500 字符>}`；网络错误：code=-1。
pub async fn http_request_with_proxy(
    url: &str,
    method: &str,
    body: Option<serde_json::Value>,
    headers: Option<&HashMap<String, String>>,
    proxy: Option<&str>,
) -> serde_json::Value {
    let method = reqwest::Method::from_bytes(method.as_bytes()).unwrap_or(reqwest::Method::GET);
    let client = match proxy.map(str::trim).filter(|value| !value.is_empty()) {
        Some(proxy) => match http_client_builder()
            .proxy(match reqwest::Proxy::all(proxy) {
                Ok(proxy) => proxy,
                Err(e) => return serde_json::json!({"code": -1, "message": format!("代理地址无效: {e}")}),
            })
            .build()
        {
            Ok(client) => client,
            Err(e) => {
                return serde_json::json!({"code": -1, "message": format!("代理客户端创建失败: {e}")})
            }
        },
        None => http_client().clone(),
    };
    let mut req = client.request(method, url);
    req = req.header("Content-Type", "application/json");
    if let Some(h) = headers {
        for (k, v) in h {
            req = req.header(k, v);
        }
    }
    if let Some(b) = body {
        req = req.json(&b);
    }
    match req.send().await {
        Ok(resp) => {
            let status = resp.status();
            let text = resp.text().await.unwrap_or_default();
            if status.is_success() {
                serde_json::from_str(&text).unwrap_or(serde_json::Value::Null)
            } else {
                serde_json::from_str(&text).unwrap_or_else(|_| {
                    serde_json::json!({
                        "code": status.as_u16(),
                        "message": text.chars().take(500).collect::<String>(),
                    })
                })
            }
        }
        Err(e) => serde_json::json!({"code": -1, "message": e.to_string()}),
    }
}

/// 通用 HTTP 请求，返回原始响应（状态码 + 响应头 + 响应体），可选是否跟随重定向。
///
/// 供需要读取响应头（如 302 的 `Location`）的场景使用。失败返回 `(0, 空, 错误信息)`。
pub async fn http_request_raw(
    url: &str,
    method: &str,
    body: Option<serde_json::Value>,
    headers: Option<&HashMap<String, String>>,
    proxy: Option<&str>,
    follow_redirects: bool,
) -> (u16, HashMap<String, String>, String) {
    let method = reqwest::Method::from_bytes(method.as_bytes()).unwrap_or(reqwest::Method::GET);
    let client = match proxy.map(str::trim).filter(|value| !value.is_empty()) {
        Some(proxy) => {
            let mut builder = http_client_builder().proxy(match reqwest::Proxy::all(proxy) {
                Ok(proxy) => proxy,
                Err(e) => return (0, HashMap::new(), format!("代理地址无效: {e}")),
            });
            if !follow_redirects {
                builder = builder.redirect(reqwest::redirect::Policy::none());
            }
            match builder.build() {
                Ok(client) => client,
                Err(e) => return (0, HashMap::new(), format!("客户端创建失败: {e}")),
            }
        }
        None => {
            if follow_redirects {
                http_client().clone()
            } else {
                match http_client_builder()
                    .redirect(reqwest::redirect::Policy::none())
                    .build()
                {
                    Ok(client) => client,
                    Err(e) => return (0, HashMap::new(), format!("客户端创建失败: {e}")),
                }
            }
        }
    };
    let mut req = client.request(method, url);
    req = req.header("Content-Type", "application/json");
    if let Some(h) = headers {
        for (k, v) in h {
            req = req.header(k, v);
        }
    }
    if let Some(b) = body {
        req = req.json(&b);
    }
    match req.send().await {
        Ok(resp) => {
            let status = resp.status().as_u16();
            let mut resp_headers = HashMap::new();
            for (k, v) in resp.headers() {
                if let Ok(vs) = v.to_str() {
                    resp_headers.insert(k.as_str().to_string(), vs.to_string());
                }
            }
            let text = resp.text().await.unwrap_or_default();
            (status, resp_headers, text)
        }
        Err(e) => (0, HashMap::new(), e.to_string()),
    }
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn atomic_write_bytes_creates_parents_and_overwrites_readonly() {
        let tmp = std::env::temp_dir().join(format!("qs-atomic-{}", uuid::Uuid::new_v4().simple()));
        let nested_target = tmp.join("deep").join("subdir").join("test.txt");

        // 父目录不存在时自动递归创建并成功写入
        assert!(atomic_write_bytes(&nested_target, b"hello").is_ok());
        assert_eq!(std::fs::read(&nested_target).unwrap(), b"hello");

        // 设为只读后再次原子写入
        let mut perms = std::fs::metadata(&nested_target).unwrap().permissions();
        perms.set_readonly(true);
        let _ = std::fs::set_permissions(&nested_target, perms);

        assert!(atomic_write_bytes(&nested_target, b"updated").is_ok());
        assert_eq!(std::fs::read(&nested_target).unwrap(), b"updated");

        let _ = std::fs::remove_dir_all(tmp);
    }
}
