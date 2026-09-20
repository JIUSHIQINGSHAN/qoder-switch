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

fn main() {
    let mut port = DEFAULT_PORT;
    let mut dist: Option<PathBuf> = None;
    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        match a.as_str() {
            "--port" => {
                port = args
                    .next()
                    .and_then(|p| p.parse().ok())
                    .unwrap_or(DEFAULT_PORT)
            }
            "--dist" => dist = args.next().map(PathBuf::from),
            "-h" | "--help" => {
                println!(
                    "用法: qs-switch-server [--port <{}>] [--dist <前端产物目录>]\n\
                     只监听 127.0.0.1。破坏性端点需带 ?confirm=switch。",
                    DEFAULT_PORT
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

    for stream in listener.incoming() {
        match stream {
            Ok(s) => {
                let dist = std::sync::Arc::clone(&dist);
                std::thread::spawn(move || {
                    if let Err(e) = handle(s, &dist) {
                        eprintln!("连接处理结束: {e}");
                    }
                });
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

fn handle(mut stream: TcpStream, dist: &Path) -> std::io::Result<()> {
    // 本机工具，但慢客户端不该把线程一直挂着。
    let _ = stream.set_read_timeout(Some(std::time::Duration::from_secs(30)));
    let peer = stream.peer_addr()?;
    if !peer.ip().is_loopback() {
        // 绑定已经挡了外部地址；这里再兜一层，防止将来有人改成 0.0.0.0。
        return write(
            &mut stream.try_clone()?,
            &Response {
                status: 403,
                content_type: "application/json",
                body: r#"{"ok":false,"error":"只允许回环地址访问"}"#.into(),
                bytes: None,
            },
        );
    }
    let mut reader = BufReader::new(stream.try_clone()?);
    let mut line = String::new();
    reader.read_line(&mut line)?;
    let mut headers = Vec::new();
    loop {
        let mut h = String::new();
        if reader.read_line(&mut h)? == 0 || h.trim().is_empty() {
            break;
        }
        headers.push(h);
    }
    let len = headers
        .iter()
        .find_map(|h| {
            let (k, v) = h.split_once(':')?;
            (k.trim().eq_ignore_ascii_case("content-length"))
                .then(|| v.trim().parse::<usize>().ok())
                .flatten()
        })
        .unwrap_or(0);
    if len > 8 * 1024 * 1024 {
        return write(
            &mut stream,
            &Response {
                status: 413,
                content_type: "application/json",
                body: r#"{"ok":false,"error":"请求体过大"}"#.into(),
                bytes: None,
            },
        );
    }
    let mut body = vec![0u8; len];
    if len > 0 {
        reader.read_exact(&mut body)?;
    }

    let mut parts = line.split_whitespace();
    let method = parts.next().unwrap_or("GET");
    let target = parts.next().unwrap_or("/");
    let (path, query) = target.split_once('?').map(|(p, q)| (p, q)).unwrap_or((target, ""));

    let resp = match (method, path.strip_prefix("/api/")) {
        ("GET", Some(cmd)) => router::dispatch(cmd, query, ""),
        ("POST", Some(cmd)) => {
            router::dispatch(cmd, query, &String::from_utf8_lossy(&body))
        }
        ("OPTIONS", Some(_)) => Response {
            status: 204,
            content_type: "text/plain",
            body: String::new(),
            bytes: None,
        },
        _ => serve_static(dist, path),
    };
    let status_line = resp.status;
    write(&mut stream, &resp)?;
    println!("{method} {target} -> {status_line}");
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
        413 => "Payload Too Large",
        500 => "Internal Server Error",
        _ => "Unknown",
    }
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
    let ok_ext = ["html", "js", "css", "svg", "png", "ico", "json", "woff2"];
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
        "woff2" => "font/woff2",
        _ => "application/octet-stream",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
}
