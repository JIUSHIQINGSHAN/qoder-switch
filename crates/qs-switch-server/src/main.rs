//! Qoder Switch 的 HTTP 宿主：`qs-switch-server` 监听 127.0.0.1，把同一套 core 能力
//! 以 JSON 暴露出来，并顺带托管已构建好的前端 `dist/`。
//!
//! 与桌面端共用 `qs-switch-core`，所以安全门（托管判定、备份回滚、半换号拒绝）在这里
//! 同样生效 —— 少一层都不算同一套逻辑。
//!
//! 默认端口刻意与参考实现的 57890 错开：本机可能装着 workbuddy-switch 的 webui。

mod router;

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};

use router::Response;

const DEFAULT_PORT: u16 = 57891;

/// 所有 /api 请求必须携带的自定义头。
///
/// 这道门专防**浏览器携带型攻击**：跨站表单（含 enctype=text/plain 的经典 JSON
/// 绕过）与 `<img>` 发不出自定义头；fetch 带自定义头会触发 CORS 预检，而本服务
/// 从不回 ACAO，预检必挂。对本地进程无效（curl -H 人人会写）——但本地进程本来
/// 就能直接删文件，API 没有给它任何新增能力。
const CLIENT_HEADER: &str = "x-qoder-switch";

fn main() {
    let mut port = DEFAULT_PORT;
    let mut dist: Option<PathBuf> = None;
    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        match a.as_str() {
            "--port" => match args.next().map(|p| p.parse::<u16>()) {
                Some(Ok(p)) => port = p,
                _ => {
                    eprintln!("--port 需要一个 0-65535 的数字");
                    std::process::exit(2);
                }
            },
            "--dist" => dist = args.next().map(PathBuf::from),
            "-h" | "--help" => {
                println!(
                    "用法: qs-switch-server [--port <{DEFAULT_PORT}>] [--dist <前端产物目录>]\n\
                     只监听 127.0.0.1。所有 /api 请求必须带头 {CLIENT_HEADER}: 1；\n\
                     破坏性端点另需知情标记 ?confirm=switch（query）或 body 里的 confirm:\"switch\"。",
                );
                return;
            }
            _ => {}
        }
    }
    let dist = std::sync::Arc::new(dist.unwrap_or_else(default_dist));

    let addr = format!("127.0.0.1:{port}");
    let listener = match TcpListener::bind(&addr) {
        Ok(l) => l,
        Err(e) => {
            eprintln!("监听 {addr} 失败: {e}");
            std::process::exit(1);
        }
    };
    println!("Qoder Switch webui: http://{addr}");
    println!("  前端产物: {}", dist.display());
    println!("  账号库:   {}", qs_switch_core::modules::config::switch_root().display());
    println!("  Ctrl+C 结束（只影响本进程，不会动 Qoder）");

    // 与桌面端共用同一条自动签到调度：webui 宿主开着时也会按配置补签，行为不分叉。
    qs_switch_core::modules::ledger::spawn_scheduler();

    for stream in listener.incoming() {
        match stream {
            Ok(s) => {
                let dist = std::sync::Arc::clone(&dist);
                // 线程耗尽时 spawn 会失败——发生在 accept 循环里就是整个服务器退出。
                // 改用 Builder 并在失败时限速重试，accept 循环必须活着。
                let spawned = std::thread::Builder::new()
                    .name("qs-conn".into())
                    .spawn(move || {
                        if let Err(e) = handle(s, &dist) {
                            eprintln!("连接处理结束: {e}");
                        }
                    });
                if spawned.is_err() {
                    eprintln!("建线程失败，稍候重试");
                    std::thread::sleep(std::time::Duration::from_millis(50));
                }
            }
            Err(e) => eprintln!("accept 失败: {e}"),
        }
    }
}

/// dist 默认在可执行文件同级的仓库里找：`crates/qs-switch-server/../../../../dist`
/// 对 `cargo run` 有效；发布版建议显式 `--dist`。
fn default_dist() -> PathBuf {
    let candidates = [
        PathBuf::from("dist"),
        std::env::current_exe()
            .ok()
            .and_then(|p| p.parent().map(|d| d.join("../../../dist")))
            .unwrap_or_else(|| PathBuf::from("dist")),
    ];
    candidates
        .into_iter()
        .find(|p| p.join("index.html").is_file())
        .unwrap_or_else(|| PathBuf::from("dist"))
}

/// 读一行（含换行），行内容超过 `cap` 字节返回 Err。手写解析没有框架的兜底：
/// 一个不发换行的连接就能让无界 read_line 涨到 OOM。
fn read_line_capped<R: BufRead>(reader: &mut R, cap: usize) -> std::io::Result<std::io::Result<String>> {
    let mut buf = Vec::new();
    let mut limited = (&mut *reader).take(cap as u64 + 1);
    let n = limited.read_until(b'\n', &mut buf)?;
    drop(limited);
    if n == 0 {
        return Ok(Err(std::io::Error::new(
            std::io::ErrorKind::UnexpectedEof,
            "eof",
        )));
    }
    if n == cap + 1 && *buf.last().unwrap_or(&0) != b'\n' {
        return Ok(Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "line too long",
        )));
    }
    Ok(Ok(String::from_utf8_lossy(&buf).into_owned()))
}

fn reject(stream: &mut TcpStream, status: u16, msg: &str) -> std::io::Result<()> {
    write(
        stream,
        &Response {
            status,
            content_type: "application/json",
            body: format!(r#"{{"ok":false,"error":"{msg}"}}"#),
            bytes: None,
        },
    )
}

fn handle(mut stream: TcpStream, dist: &Path) -> std::io::Result<()> {
    // 本机工具，但慢客户端不该把线程一直挂着（读和写都要有界）。
    let _ = stream.set_read_timeout(Some(std::time::Duration::from_secs(30)));
    let _ = stream.set_write_timeout(Some(std::time::Duration::from_secs(30)));
    let peer = stream.peer_addr()?;
    if !peer.ip().is_loopback() {
        // 绑定已经挡了外部地址；这里再兜一层，防止将来有人改成 0.0.0.0。
        return reject(&mut stream, 403, "只允许回环地址访问");
    }
    let mut reader = BufReader::new(stream.try_clone()?);

    // 请求行 ≤ 8KB。
    let line = match read_line_capped(&mut reader, 8 * 1024)? {
        Ok(l) if !l.trim().is_empty() => l,
        Ok(_) => return Ok(()), // 空请求行：连接开着没发东西，直接断
        Err(_) => return reject(&mut stream, 400, "请求行过长"),
    };

    // 头 ≤ 100 行、总量 ≤ 64KB。
    let mut headers = Vec::new();
    let mut headers_total = 0usize;
    loop {
        let h = match read_line_capped(&mut reader, 8 * 1024)? {
            Ok(h) => h,
            Err(_) => return reject(&mut stream, 431, "请求头过长"),
        };
        if h.trim().is_empty() || headers.len() >= 100 || headers_total > 64 * 1024 {
            if headers.len() >= 100 || headers_total > 64 * 1024 {
                return reject(&mut stream, 431, "请求头过多");
            }
            break;
        }
        headers_total += h.len();
        headers.push(h);
    }

    // 一个 body 只能有一种长度；chunked 是另一套编码，本服务不实现（明确拒绝，
    // 不静默当空 body 处理）。
    if headers.iter().any(|h| {
        h.split_once(':')
            .map_or(false, |(k, _)| k.trim().eq_ignore_ascii_case("transfer-encoding"))
    }) {
        return reject(&mut stream, 501, "不支持 Transfer-Encoding，请用 Content-Length");
    }
    let mut len: Option<usize> = None;
    for h in &headers {
        let Some((k, v)) = h.split_once(':') else { continue };
        if k.trim().eq_ignore_ascii_case("content-length") {
            let parsed = v.trim().parse::<usize>();
            match (parsed, len) {
                (Ok(n), None) => len = Some(n),
                (Ok(n), Some(prev)) if n == prev => {}
                _ => return reject(&mut stream, 400, "Content-Length 冲突或非法"),
            }
        }
    }
    let len = len.unwrap_or(0);
    if len > 8 * 1024 * 1024 {
        return reject(&mut stream, 413, "请求体过大");
    }
    let mut body = vec![0u8; len];
    if len > 0 {
        reader.read_exact(&mut body)?;
    }

    let mut parts = line.split_whitespace();
    let method = parts.next().unwrap_or("GET");
    let target = parts.next().unwrap_or("/");
    let (path, query) = target.split_once('?').map(|(p, q)| (p, q)).unwrap_or((target, ""));

    // 终端日志注入：target 是外部输入，控制字符（含 ANSI ESC、\r）打印前过滤。
    let safe_target: String = target.chars().map(|c| if c.is_control() { '?' } else { c }).collect();

    if path.starts_with("/api/") && !client_header_present(&headers) {
        reject(
            &mut stream,
            403,
            "缺少 x-qoder-switch: 1 请求头（防跨站携带型攻击；curl 请加 -H \"x-qoder-switch: 1\"）",
        )?;
        println!("{method} {safe_target} -> 403");
        return Ok(());
    }

    let resp = match (method, path.strip_prefix("/api/")) {
        ("GET" | "HEAD", Some(cmd)) => router::dispatch(cmd, query, ""),
        ("POST", Some(cmd)) => {
            router::dispatch(cmd, query, &String::from_utf8_lossy(&body))
        }
        ("OPTIONS", Some(_)) => Response {
            status: 204,
            content_type: "text/plain",
            body: String::new(),
            bytes: None,
        },
        // /api 上的其它方法（DELETE/PUT…）明说 405，不再静默落进静态路由
        // 伪装成 index.html。
        (_, Some(_)) => {
            reject(&mut stream, 405, "方法不支持（/api 只收 GET/POST/OPTIONS/HEAD）")?;
            println!("{method} {safe_target} -> 405");
            return Ok(());
        }
        _ => serve_static(dist, path),
    };
    let status_line = resp.status;
    if method == "HEAD" {
        // HEAD 只回头不回体。
        let head_only = Response {
            status: resp.status,
            content_type: resp.content_type,
            body: String::new(),
            bytes: None,
        };
        write(&mut stream, &head_only)?;
    } else {
        write(&mut stream, &resp)?;
    }
    println!("{method} {safe_target} -> {status_line}");
    Ok(())
}

/// 构造响应。二进制资源走 `bytes`，文本走 `body` —— 两者不能混用同一字段，
/// 因为把 woff2/png 塞进 String 会按 UTF-8 重编码并毁掉文件。
fn write(stream: &mut TcpStream, r: &Response) -> std::io::Result<()> {
    let payload: &[u8] = r.bytes.as_deref().unwrap_or(r.body.as_bytes());
    let head = format!(
        "HTTP/1.1 {code} {why}\r\n\
         Content-Type: {ct}\r\n\
         Content-Length: {len}\r\n\
         Cache-Control: no-store\r\n\
         X-Content-Type-Options: nosniff\r\n\
         Connection: close\r\n\r\n",
        code = r.status,
        why = reason(r.status),
        ct = r.content_type,
        len = payload.len(),
    );
    stream.write_all(head.as_bytes())?;
    stream.write_all(payload)?;
    stream.flush()
}

fn reason(status: u16) -> &'static str {
    match status {
        200 => "OK",
        204 => "No Content",
        400 => "Bad Request",
        403 => "Forbidden",
        404 => "Not Found",
        405 => "Method Not Allowed",
        413 => "Payload Too Large",
        431 => "Request Header Fields Too Large",
        500 => "Internal Server Error",
        501 => "Not Implemented",
        _ => "Unknown",
    }
}

/// 头门判定（抽出为纯函数以便单测）：名字大小写不敏感，值必须恰为 "1"。
fn client_header_present(headers: &[String]) -> bool {
    headers.iter().any(|h| {
        h.split_once(':')
            .map_or(false, |(k, v)| {
                k.trim().eq_ignore_ascii_case(CLIENT_HEADER) && v.trim() == "1"
            })
    })
}

/// 静态文件：只允许 dist 目录内的白名单后缀，路径必须解析到 dist 之下。
///
/// 带 `.` 的请求按资源处理（找不到就是 404，让控制台如实报缺文件）；
/// 不带 `.` 的是前端路由 —— 前端用 BrowserRouter，刷新 `/settings` 必须回 index.html，
/// 否则复刻来的界面一刷新就白屏。
fn serve_static(dist: &Path, path: &str) -> Response {
    let rel = match path {
        "/" | "" => "index.html",
        other => other.trim_start_matches('/'),
    };
    let ok_ext = ["html", "js", "css", "svg", "png", "ico", "json", "woff", "woff2", "ttf", "webp"];
    let is_route = !rel.contains('.');
    let ext = if is_route { "html" } else { rel.rsplit('.').next().unwrap_or("") };
    if !ok_ext.contains(&ext) || rel.contains("..\\") || rel.contains("/../") || rel.starts_with("../")
    {
        return Response {
            status: 404,
            content_type: "text/plain",
            body: "not found".into(),
            bytes: None,
        };
    }
    let joined = if is_route {
        dist.join("index.html")
    } else {
        dist.join(rel)
    };
    let canonical = match joined.canonicalize() {
        Ok(c) => c,
        Err(_) => {
            return Response {
                status: 404,
                content_type: "text/plain",
                body: "not found".into(),
                bytes: None,
            }
        }
    };
    let root = match dist.canonicalize() {
        Ok(r) => r,
        Err(_) => {
            return Response {
                status: 500,
                content_type: "text/plain",
                body: "dist 目录不存在".into(),
                bytes: None,
            }
        }
    };
    if !canonical.starts_with(&root) || !canonical.is_file() {
        return Response {
            status: 404,
            content_type: "text/plain",
            body: "not found".into(),
            bytes: None,
        };
    }
    match std::fs::read(&canonical) {
        Ok(bytes) => Response::binary(200, mime_for(ext), bytes),
        Err(_) => Response {
            status: 500,
            content_type: "text/plain",
            body: "读取失败".into(),
            bytes: None,
        },
    }
}

fn mime_for(ext: &str) -> &'static str {
    match ext {
        "html" => "text/html; charset=utf-8",
        "js" => "text/javascript; charset=utf-8",
        "css" => "text/css; charset=utf-8",
        "json" => "application/json",
        "svg" => "image/svg+xml",
        "png" => "image/png",
        "ico" => "image/x-icon",
        "woff" => "font/woff",
        "woff2" => "font/woff2",
        "ttf" => "font/ttf",
        "webp" => "image/webp",
        _ => "application/octet-stream",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 头门：名字大小写不敏感、值必须恰为 "1"、其它头不算数。
    /// 这道门是 text/plain 表单 CSRF 的解药（表单发不出自定义头）。
    #[test]
    fn client_header_gate_is_exact() {
        assert!(client_header_present(&["x-qoder-switch: 1".into()]));
        assert!(client_header_present(&["X-Qoder-Switch:1".into()]));
        assert!(client_header_present(&["accept: */*".into(), "x-qoder-switch: 1 ".into()]));
        assert!(!client_header_present(&Vec::<String>::new()));
        assert!(!client_header_present(&["x-qoder-switch: 0".into()]));
        assert!(!client_header_present(&["x-qoder-switch: yes".into()]));
        assert!(!client_header_present(&["other: 1".into()]));
    }

    /// 无界 read_line 的解药：超长行必须报错，正常行照读。
    #[test]
    fn read_line_capped_rejects_long_lines() {
        let mut ok = std::io::Cursor::new(b"GET / HTTP/1.1\r\n".to_vec());
        assert_eq!(
            read_line_capped(&mut ok, 8 * 1024).unwrap().unwrap(),
            "GET / HTTP/1.1\r\n"
        );
        let big = vec![b'a'; 9 * 1024];
        let mut bad = std::io::Cursor::new(big);
        assert!(read_line_capped(&mut bad, 8 * 1024).unwrap().is_err());
        let mut eof = std::io::Cursor::new(Vec::new());
        assert!(read_line_capped(&mut eof, 8 * 1024).unwrap().is_err());
    }

    fn tmp_dist() -> PathBuf {
        let d = std::env::temp_dir().join(format!("qs-dist-{}", uuid::Uuid::new_v4().simple()));
        std::fs::create_dir_all(d.join("assets")).unwrap();
        std::fs::write(d.join("index.html"), b"<html>ok</html>").unwrap();
        std::fs::write(d.join("assets/app.js"), b"console.log(1)").unwrap();
        d
    }

    #[test]
    fn serves_index_for_root_and_assets() {
        let d = tmp_dist();
        // 静态文件一律按原始字节返回，所以断言要看 bytes 而不是 body。
        let r = serve_static(&d, "/");
        assert_eq!(r.status, 200);
        assert_eq!(r.bytes.as_deref(), Some(&b"<html>ok</html>"[..]));
        let r = serve_static(&d, "/assets/app.js");
        assert_eq!(r.status, 200);
        assert!(r.content_type.contains("javascript"));

        std::fs::write(d.join("assets/icon.webp"), b"webp-bytes").unwrap();
        let r = serve_static(&d, "/assets/icon.webp");
        assert_eq!(r.status, 200);
        assert_eq!(r.content_type, "image/webp");

        std::fs::remove_dir_all(d).ok();
    }

    /// 路径穿越与越权后缀都必须拿不到东西。
    #[test]
    fn blocks_traversal_and_unknown_extensions() {
        let d = tmp_dist();
        let secret = d.join("secret.txt");
        std::fs::write(&secret, b"nope").unwrap();
        assert_eq!(serve_static(&d, "/secret.txt").status, 404, "后缀不在白名单");
        assert_eq!(serve_static(&d, "/../secret.txt").status, 404);
        assert_eq!(serve_static(&d, "/assets/../../etc/passwd").status, 404);
        assert_eq!(serve_static(&d, "/assets/app.js%00.png").status, 404);
        std::fs::remove_dir_all(d).ok();
    }

    /// dist 不存在时不该 panic，也不该把"目录没找到"说成服务器坏了。
    #[test]
    fn missing_dist_is_404_not_panic() {
        let r = serve_static(Path::new("/definitely/not/here"), "/");
        assert_eq!(r.status, 404);
    }

    /// BrowserRouter 的前端路由要回 index.html；但缺文件的资源请求必须继续 404，
    /// 否则 JS/CSS 404 会变成"下载了一个 HTML"，白屏且看不出原因。
    #[test]
    fn spa_routes_fall_back_to_index_but_missing_assets_do_not() {
        let d = tmp_dist();
        for route in ["/settings", "/token-stats", "/credit-stats", "/credit-stats/"] {
            let r = serve_static(&d, route);
            assert_eq!(r.status, 200, "{route} 应回 index.html");
            assert!(r.content_type.contains("html"), "{route} 的 Content-Type 错了");
            assert_eq!(r.bytes.as_deref(), Some(&b"<html>ok</html>"[..]));
        }
        assert_eq!(serve_static(&d, "/assets/nope.js").status, 404);
        assert_eq!(serve_static(&d, "/assets/nope.png").status, 404);
        std::fs::remove_dir_all(d).ok();
    }

    #[test]
    fn head_method_returns_status_without_body() {
        let resp = router::dispatch("status", "", "");
        assert_eq!(resp.status, 200);
        let head_only = Response {
            status: resp.status,
            content_type: resp.content_type,
            body: String::new(),
            bytes: None,
        };
        assert_eq!(head_only.status, 200);
        assert!(head_only.body.is_empty());
        assert!(head_only.bytes.is_none());
    }
}
