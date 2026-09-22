//! 极简 HTTP 路由：把 `POST /api/<cmd>` 的 JSON body 派发到 core，`GET /api/*` 为只读。
//!
//! 手写而不是引 axum：本服务只跑在本机回环上、请求形态固定，多一层框架就把整个 tokio
//! 依赖树拖进来了。这里零新增依赖，路由本身可脱离 socket 单测。
//!
//! 破坏性端点必须带 `?confirm=switch` —— 少这一步，任何本机进程或浏览器里的一个跨站
//! 表单都能把你的账号切掉。

use std::path::Path;

use serde::Serialize;
use serde_json::{json, Value};

use qs_switch_core::modules::config::PathRoots;
use qs_switch_core::modules::variant::{QoderTarget, QoderVariant};
use qs_switch_core::modules::{bundle, export_import, rotate, snapshot, switch};

#[derive(Debug, PartialEq, Eq)]
pub struct Response {
    pub status: u16,
    pub content_type: &'static str,
    pub body: String,
    /// 二进制资源（woff2 / png / ico）走这里。
    ///
    /// 不能塞进 `body: String`：静态服务早先用 `from_utf8_lossy` 输出字体，
    /// 把字节按 UTF-8 重编码直接毁掉文件，浏览器报 "OTS parsing error"、界面白屏。
    pub bytes: Option<Vec<u8>>,
}

impl Response {
    fn json(status: u16, v: Value) -> Self {
        Self {
            status,
            content_type: "application/json",
            body: v.to_string(),
            bytes: None,
        }
    }

    pub fn binary(status: u16, content_type: &'static str, bytes: Vec<u8>) -> Self {
        Self {
            status,
            content_type,
            body: String::new(),
            bytes: Some(bytes),
        }
    }
    fn err(e: String) -> Self {
        Self::json(400, json!({ "ok": false, "error": e, "message": e }))
    }
    fn ok<T: Serialize>(v: T) -> Self {
        match serde_json::to_value(&v) {
            Ok(data) => Self::json(200, json!({ "ok": true, "data": data })),
            Err(e) => Self::err(format!("结果序列化失败: {e}")),
        }
    }
    /// 裸契约对象，不套 `{ok,data}`。
    ///
    /// 参考前端的 `httpCall` 结尾是 `return data as T`，它把 HTTP body 原样当作 Tauri
    /// `invoke` 的返回值。多包一层，`const [status, {accounts}] = ...` 就解出 `undefined`，
    /// 界面在 `accounts.filter` 上抛错、整页空白 —— 而网络面板全是 200，很难看出问题。
    fn bare(v: Value) -> Self {
        Self::json(200, v)
    }
}

fn parse_variant(v: Option<&Value>) -> Result<QoderVariant, String> {
    match v.and_then(|x| x.as_str()) {
        Some("cn") => Ok(QoderVariant::Cn),
        Some("global") => Ok(QoderVariant::Global),
        Some(other) => Err(format!("未知版本: {other}")),
        None => Ok(QoderVariant::Cn),
    }
}

fn parse_target(v: Option<&Value>) -> Result<QoderTarget, String> {
    match v.and_then(|x| x.as_str()) {
        Some("desktop") => Ok(QoderTarget::Desktop),
        Some("cli") => Ok(QoderTarget::Cli),
        Some("work") => Ok(QoderTarget::Work),
        Some(other) => Err(format!("未知目标: {other}")),
        None => Err("缺 target".into()),
    }
}

/// `cmd` 是路径 `/api/` 之后的部分，`query` 已按 `&` 切好。
pub fn dispatch(cmd: &str, query: &str, body: &str) -> Response {
    let roots = PathRoots::real();
    let store = qs_switch_core::modules::config::switch_root();
    dispatch_in(&roots, &store, cmd, query, body)
}

/// 支持注入 `roots` 与 `store` 的可测试派发入口（保证单测 hermeticity，绝不污染用户真实环境）。
pub fn dispatch_in(roots: &PathRoots, store: &Path, cmd: &str, query: &str, body: &str) -> Response {
    if let Some(r) = compat::dispatch_in(roots, store, cmd, query, body) {
        return r;
    }
    let input: Value = if body.trim().is_empty() {
        json!({})
    } else {
        match serde_json::from_str(body) {
            Ok(v) => v,
            Err(e) => return Response::err(format!("请求体不是合法 JSON: {e}")),
        }
    };

    match cmd {
        "status" => Response::ok(json!({
            "store": store.display().to_string(),
            "accounts": bundle::list_all(store),
            "unfinished": switch::unfinished(store).unwrap_or_default(),
        })),
        "rotation" => match parse_variant(input.get("variant")) {
            Err(e) => Response::err(e),
            Ok(variant) => {
                match rotate::suggest(roots, store, variant, &rotate::RotateConfig::default()) {
                    Ok(s) => Response::ok(s),
                    Err(e) => Response::err(e),
                }
            }
        },
        "snapshot" => match snapshot::Snapshot::latest().map_err(|e| e.to_string()) {
            Err(e) => Response::err(e),
            Ok(previous) => {
                let now = snapshot::Snapshot::take();
                let changes = previous.map(|p| p.diff(&now)).unwrap_or_default();
                Response::ok(json!({ "taken_at": now.taken_at, "changes": changes }))
            }
        },
        "capture" => {
            let Some(id) = input.get("account_id").and_then(|x| x.as_str()) else {
                return Response::err("缺 account_id".into());
            };
            match (parse_variant(input.get("variant")), parse_target(input.get("target"))) {
                (Ok(variant), Ok(target)) => match bundle::capture(roots, store, id, variant, target) {
                    Ok(b) => {
                        if b.is_empty() {
                            Response::err(format!("该目标在本机不落盘凭据，没有可认领的文件"))
                        } else {
                            Response::ok(b)
                        }
                    }
                    Err(e) => Response::err(e),
                },
                (Err(e), _) | (_, Err(e)) => Response::err(e),
            }
        }
        "export" => {
            let Some(id) = input.get("account_id").and_then(|x| x.as_str()) else {
                return Response::err("缺 account_id".into());
            };
            match export_import::export_account(&store, id).and_then(|e| export_import::to_bytes(&e)) {
                Ok(bytes) => match String::from_utf8(bytes) {
                    Ok(text) => Response::ok(json!({ "text": text })),
                    Err(e) => Response::err(e.to_string()),
                },
                Err(e) => Response::err(e),
            }
        }
        "import" => {
            let Some(text) = input.get("text").and_then(|x| x.as_str()) else {
                return Response::err("缺 text".into());
            };
            let overwrite = input.get("overwrite").and_then(|x| x.as_bool()).unwrap_or(false);
            match export_import::import(&store, text.as_bytes(), overwrite) {
                Ok(r) => Response::ok(json!({
                    "written": r.written.into_iter().map(|(id,v,t,n)| format!("{id} {v:?} {t:?} {n}")).collect::<Vec<_>>(),
                    "skipped": r.skipped,
                })),
                Err(e) => Response::err(e),
            }
        }
        "preview" => {
            let Some(id) = input.get("account_id").and_then(|x| x.as_str()) else {
                return Response::err("缺 account_id".into());
            };
            match (parse_variant(input.get("variant")), parse_target(input.get("target"))) {
                (Ok(variant), Ok(target)) => {
                    let req = switch::Request { account_id: id.into(), variant, target, restart: true };
                    match switch::preview(&roots, &store, &req) {
                        Ok(p) => Response::ok(p),
                        Err(e) => Response::err(e),
                    }
                }
                (Err(e), _) | (_, Err(e)) => Response::err(e),
            }
        }
        "switch" => {
            // 破坏性端点：必须显式带 confirm=switch。
            if !query.split('&').any(|p| p == "confirm=switch") {
                return Response::err(
                    "切换账号会终止并重开目标客户端，必须带 ?confirm=switch 表示知情".into(),
                );
            }
            let Some(id) = input.get("account_id").and_then(|x| x.as_str()) else {
                return Response::err("缺 account_id".into());
            };
            match (parse_variant(input.get("variant")), parse_target(input.get("target"))) {
                (Ok(variant), Ok(target)) => {
                    let req = switch::Request {
                        account_id: id.into(),
                        variant,
                        target,
                        restart: input.get("restart").and_then(|x| x.as_bool()).unwrap_or(true),
                    };
                    let actor = if input.get("forced").and_then(|x| x.as_bool()).unwrap_or(false) {
                        switch::Actor::RealForced
                    } else {
                        switch::Actor::Real
                    };
                    match switch::execute(&roots, &store, &req, actor, &mut |_| {}) {
                        Ok(j) => Response::ok(j),
                        Err(e) => Response::err(e),
                    }
                }
                (Err(e), _) | (_, Err(e)) => Response::err(e),
            }
        }
        "recover" => match serde_json::from_value::<switch::Journal>(input.clone()) {
            Err(e) => Response::err(format!("journal 形状不对: {e}")),
            Ok(j) => match switch::recover(&store, &j) {
                Ok(p) => Response::ok(json!({ "phase": format!("{p:?}") })),
                Err(e) => Response::err(e),
            },
        },
        other => {
            let msg = format!("未知端点: {other}");
            Response::json(404, json!({ "ok": false, "error": msg, "message": msg }))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct TempDir(std::path::PathBuf);

    impl TempDir {
        fn new(name: &str) -> Self {
            let dir = std::env::temp_dir().join(format!(
                "qs_switch_router_test_{}_{name}",
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
    fn unknown_endpoint_is_404_not_panic() {
        let dir = TempDir::new("unknown");
        let r = dispatch_in(&PathRoots::real(), &dir.0, "nope", "", "{}");
        assert_eq!(r.status, 404);
        assert!(r.body.contains("未知端点"));
        let v: Value = serde_json::from_str(&r.body).unwrap();
        assert!(v.get("message").is_some());
        assert!(v.get("error").is_some());
    }

    #[test]
    fn malformed_json_is_rejected() {
        let dir = TempDir::new("malformed");
        let r = dispatch_in(&PathRoots::real(), &dir.0, "capture", "", "{不是 JSON");
        assert_eq!(r.status, 400);
        assert!(r.body.contains("合法 JSON"), "{}", r.body);
    }

    /// 破坏性端点在没有知情标记时必须被挡下，且不产生任何副作用。
    #[test]
    fn switch_requires_explicit_confirmation() {
        let dir = TempDir::new("switch_confirm");
        let r = dispatch_in(
            &PathRoots::real(),
            &dir.0,
            "switch",
            "",
            r#"{"account_id":"main-cn","variant":"cn","target":"desktop"}"#,
        );
        assert_eq!(r.status, 400);
        assert!(r.body.contains("confirm=switch"), "{}", r.body);
    }

    /// 知情标记必须能从 POST body 进来：前端 `httpCall` 对 POST 不拼 query，body 是
    /// 唯一通道。此前只认 query，webui 的切换永远 400 —— 核心功能在浏览器宿主从未跑通。
    /// 门通过后的下一站是"账号不存在"，据此区分门被拒与门已过。
    #[test]
    fn switch_accepts_confirmation_from_post_body() {
        let dir = TempDir::new("switch_body");
        let r = dispatch_in(
            &PathRoots::real(),
            &dir.0,
            "switch",
            "",
            r#"{"account_id":"no-such-account-xyz","confirm":"switch","target":"desktop"}"#,
        );
        assert_eq!(r.status, 400);
        assert!(
            !r.body.contains("confirm") && !r.body.contains("知情"),
            "body 知情标记应放行门禁，实得: {}",
            r.body
        );
        // query 通道保留：脚本/curl 仍可 ?confirm=switch。
        let r = dispatch_in(
            &PathRoots::real(),
            &dir.0,
            "switch",
            "confirm=switch",
            r#"{"account_id":"no-such-account-xyz","target":"desktop"}"#,
        );
        assert!(!r.body.contains("知情"), "query 知情标记应放行门禁: {}", r.body);
    }

    /// indexes 解析失败必须报错：此前 `.ok()` 把坏值吞成 None（=全量导入），
    /// 用户只勾一个、实际全部写盘。
    #[test]
    fn import_rejects_malformed_indexes_instead_of_importing_all() {
        let dir = TempDir::new("import_idx");
        let r = dispatch_in(&PathRoots::real(), &dir.0, "import", "", r#"{"fileText":"[]","indexes":[null]}"#);
        assert_eq!(r.status, 400);
        assert!(r.body.contains("indexes"), "{}", r.body);
    }

    /// 通知端点的返回必须对齐前端契约（{recorded}/{cleared}），不能是裸 null：
    /// 未来任何读 `.recorded` 的调用方都会在 null 上炸。
    #[test]
    fn notification_endpoints_return_contract_shapes() {
        let dir = TempDir::new("notify_shape");
        let r = dispatch_in(
            &PathRoots::real(),
            &dir.0,
            "notifications/record",
            "",
            r#"{"level":"info","title":"测试"}"#,
        );
        assert_eq!(r.status, 200, "{}", r.body);
        let v: Value = serde_json::from_str(&r.body).unwrap();
        assert_eq!(v["recorded"], true, "{v}");
        let r = dispatch_in(&PathRoots::real(), &dir.0, "notifications/clear", "", "");
        assert_eq!(r.status, 200, "{}", r.body);
        let v: Value = serde_json::from_str(&r.body).unwrap();
        assert_eq!(v["cleared"], true, "{v}");
    }

    #[test]
    fn bad_variant_and_missing_target_are_reported() {
        let dir = TempDir::new("bad_variant");
        let r = dispatch_in(&PathRoots::real(), &dir.0, "capture", "", r#"{"account_id":"x","variant":"eu","target":"desktop"}"#);
        assert!(r.body.contains("未知版本"), "{}", r.body);
        let r = dispatch_in(&PathRoots::real(), &dir.0, "capture", "", r#"{"account_id":"x","variant":"cn"}"#);
        assert!(r.body.contains("缺 target"), "{}", r.body);
        let r = dispatch_in(&PathRoots::real(), &dir.0, "capture", "", "{}");
        assert!(r.body.contains("account_id"), "{}", r.body);
    }

    #[test]
    fn read_only_endpoints_answer() {
        let dir = TempDir::new("read_only");
        let r = dispatch_in(&PathRoots::real(), &dir.0, "status", "", "");
        assert_eq!(r.status, 200, "{}", r.body);
        // compat 路由已把 /api/status 换成前端契约形状（AppStatus）。
        assert!(r.body.contains("authFile"), "应是 AppStatus: {}", r.body);
        let r = dispatch_in(&PathRoots::real(), &dir.0, "rotation", "", r#"{"variant":"cn"}"#);
        assert_eq!(r.status, 200, "{}", r.body);
        assert!(r.body.contains("decision"));
    }

    #[test]
    fn default_variant_is_cn_and_port_is_not_hardcoded_here() {
        assert_eq!(parse_variant(None).unwrap(), QoderVariant::Cn);
        assert_eq!(parse_variant(Some(&json!("global"))).unwrap(), QoderVariant::Global);
        assert_eq!(parse_target(Some(&json!("work"))).unwrap(), QoderTarget::Work);
    }

    /// 复刻前端的 `httpCall` 结尾是 `return data as T`：HTTP body 就是命令的返回值。
    /// 一旦套上 `{ok,data}` 信封，`{accounts}` 就解成 undefined，界面在 `accounts.filter`
    /// 上抛错、整页空白，而网络面板里全是 200 —— 所以这条是白屏回归的门禁。
    #[test]
    fn contract_routes_return_bare_objects_not_an_envelope() {
        let dir = TempDir::new("contract_bare");
        let cases: &[(&str, &str)] = &[
            ("accounts", "accounts"),
            ("status", "authFile"),
            ("capabilities", "unavailable"),
            ("rotate/logs", "logs"),
            ("rotate/status", "cliConfigured"),
        ];
        for (cmd, key) in cases {
            let r = dispatch_in(&PathRoots::real(), &dir.0, cmd, "", "");
            assert_eq!(r.status, 200, "{cmd}: {}", r.body);
            let v: Value = serde_json::from_str(&r.body).expect("合法 JSON");
            assert!(v.get(*key).is_some(), "{cmd} 应在顶层给出 {key}，实得 {v}");
            assert!(v.get("data").is_none(), "{cmd} 不得被包进 data 信封：{v}");
        }
    }

    /// 保存轮换配置时前端发的是 `{"config":{…}}`；只认顶层键的话会把配置静默写回默认值。
    /// 测试沙箱必须与用户盘隔离，绝不写真实 ~/.qs-switch/auto_rotate_config.json。
    #[test]
    fn rotate_config_save_reads_the_nested_config_key() {
        let dir = TempDir::new("rotate_cfg");
        let r = dispatch_in(
            &PathRoots::real(),
            &dir.0,
            "rotate/config",
            "",
            r#"{"config":{"enabled":false,"check_interval_minutes":60,"cooldown_minutes":120,"min_gap_hours":48,"min_urgency_hours":72,"active_guard_minutes":0,"min_remaining_credits":0}}"#,
        );
        assert_eq!(r.status, 200, "{}", r.body);
        let v: Value = serde_json::from_str(&r.body).unwrap();
        assert_eq!(v["enabled"], false);
        assert_eq!(v["min_urgency_hours"], 72);
        // 紧接着的读取必须看到刚落盘的值。
        let back: Value = serde_json::from_str(&dispatch_in(&PathRoots::real(), &dir.0, "rotate/config", "", "").body).unwrap();
        assert_eq!(back["min_urgency_hours"], 72, "保存后读取不一致");
        assert_eq!(back["enabled"], false);
    }

    /// 切换对话框会**持续轮询**进度：这个路径拼错一次就是控制台里刷不完的 404。
    #[test]
    fn switch_progress_path_matches_the_frontend_route() {
        let dir = TempDir::new("switch_prog");
        let r = dispatch_in(&PathRoots::real(), &dir.0, "switch/progress", "", "");
        assert_eq!(r.status, 200, "{}", r.body);
        let v: Value = serde_json::from_str(&r.body).unwrap();
        assert_eq!(v["running"], false);
        assert!(v.get("progress").is_some());
    }

    #[test]
    fn variant_comes_from_the_query_but_confirm_does_not_masquerade_as_it() {
        assert_eq!(compat::query_param("variant=ai", "variant"), Some("ai"));
        assert_eq!(compat::query_param("confirm=switch", "variant"), None);
        assert_eq!(compat::query_param("x=1&variant=ai", "variant"), Some("ai"));
        assert_eq!(compat::query_param("variant=", "variant"), None, "空值不算带了档位");
    }
}

// ---------------------------------------------------------------------------
// 前端契约路由：路径与返回形状对齐 workbuddy-switch 的 webui，
// 这样那份原样副本 UI 在浏览器里也能拿到真数据，而不只是能渲染外壳。
// ---------------------------------------------------------------------------
mod compat {
    use std::path::Path;
    use serde_json::{json, Value};

    use qs_switch_core::modules::config::{switch_root, PathRoots};
    use qs_switch_core::modules::notifications;
    use qs_switch_core::modules::variant::{QoderTarget, QoderVariant};
    use qs_switch_core::modules::{bundle, switch, view};
    use qs_switch_core::Result;

    use super::Response;

    fn target_of(v: Option<&Value>) -> QoderTarget {
        match v.and_then(|x| x.as_str()).unwrap_or("desktop") {
            "cli" => QoderTarget::Cli,
            "work" => QoderTarget::Work,
            _ => QoderTarget::Desktop,
        }
    }

    fn account_id(input: &Value) -> Result<String> {
        input
            .get("accountId")
            .or_else(|| input.get("account_id"))
            .and_then(|x| x.as_str())
            .map(|s| s.to_string())
            .ok_or_else(|| "缺 accountId".to_string())
    }

    /// `import-local` 的 id 可选：前端只送档位，包名由档位推出（见 `view::local_account_id`）。
    fn account_id_or_local(input: &Value, v: QoderVariant) -> String {
        input
            .get("accountId")
            .or_else(|| input.get("account_id"))
            .and_then(|x| x.as_str())
            .map(|s| s.to_string())
            .unwrap_or_else(|| view::local_account_id(v))
    }

    /// 前端契约里出现的、且本宿主实现了的路径。
    const OWNED: &[&str] = &[
        "status",
        "accounts",
        "capabilities",
        "import-local",
        "delete",
        "set-proxy",
        "export-accounts",
        "import/preview",
        "import",
        "switch",
        "switch/progress",
        "rotate/config",
        "rotate/status",
        "rotate/run",
        "rotate/logs",
        "notifications",
        "notifications/record",
        "notifications/clear",
        "credits",
        "credits/stats",
        "checkin/status",
        "checkin/logs",
        "checkin/config",
        "checkin",
        "checkin/all",
        "oauth/start",
        "oauth/status",
    ];

    /// 命中则处理并返回 Some，未命中返回 None 交给自有端点。
    pub fn dispatch(cmd: &str, query: &str, body: &str) -> Option<Response> {
        let roots = PathRoots::real();
        let store = switch_root();
        dispatch_in(&roots, &store, cmd, query, body)
    }

    pub fn dispatch_in(
        roots: &PathRoots,
        store: &Path,
        cmd: &str,
        query: &str,
        body: &str,
    ) -> Option<Response> {
        if !OWNED.contains(&cmd) {
            return None;
        }
        let input: Value = if body.trim().is_empty() {
            json!({})
        } else {
            match serde_json::from_str(body) {
                Ok(v) => v,
                Err(e) => {
                    return Some(Response::err(format!("请求体不是合法 JSON: {e}")));
                }
            }
        };
        Some(match handle(roots, store, cmd, query, &input) {
            Ok(v) => Response::bare(v),
            Err(e) => Response::err(e),
        })
    }

    /// 取查询参数。不能"随便取第一个 `=` 的右边"：切换请求带 `?confirm=switch`，
    /// 那样会把 `switch` 当成档位解析。
    pub(super) fn query_param<'a>(query: &'a str, key: &str) -> Option<&'a str> {
        query.split('&').find_map(|pair| {
            let (k, val) = pair.split_once('=')?;
            (k == key && !val.is_empty()).then_some(val)
        })
    }

    fn handle(
        roots: &PathRoots,
        store: &Path,
        cmd: &str,
        query: &str,
        input: &Value,
    ) -> std::result::Result<Value, String> {
        // body 优先，其次 query：GET 类命令只能靠 query 带档位。
        let variant = input
            .get("variant")
            .and_then(|x| x.as_str())
            .or_else(|| query_param(query, "variant"));
        let v = view::variant_from_key(variant);

        let r: Result<Value> = match cmd {
            "status" => Ok(view::app_status(roots, v)),
            "accounts" => Ok(view::accounts_in(roots, store)),
            "capabilities" => Ok(view::capabilities()),
            "import-local" => {
                let id = account_id_or_local(&input, v);
                let t = target_of(input.get("target"));
                let b = bundle::capture(roots, store, &id, v, t)?;
                if b.is_empty() {
                    return Err("该目标在本机不落盘凭据，没有可认领的文件".into());
                }
                Ok(json!({ "ok": true, "account": view::account_meta(&b) }))
            }
            "delete" => {
                let id = account_id(&input)?;
                // 与桌面端同一条防线：id 是 store 路径组成部分，delete 是 remove_dir_all。
                bundle::validate_account_id(&id)?;
                let dir = qs_switch_core::modules::bundle::accounts_root_in(store).join(&id);
                let meta = std::fs::symlink_metadata(&dir)
                    .map_err(|e| format!("账号目录不存在: {e}"))?;
                if meta.is_symlink() || !meta.is_dir() {
                    return Err(format!("账号目录不存在或不是真实目录: {}", dir.display()));
                }
                std::fs::remove_dir_all(&dir)
                    .map_err(|e| format!("删除失败: {e}"))?;
                Ok(json!({ "ok": true }))
            }
            "set-proxy" => {
                let id = account_id(&input)?;
                bundle::validate_account_id(&id)?;
                let proxy = input.get("proxy").and_then(|x| x.as_str()).map(String::from);
                let b = bundle::set_proxy(store, &id, v, QoderTarget::Desktop, proxy)?;
                Ok(json!({ "ok": true, "account": view::account_meta(&b) }))
            }
            "export-accounts" => {
                let ids: Vec<String> = input
                    .get("accountIds")
                    .or_else(|| input.get("account_ids"))
                    .and_then(|x| x.as_array())
                    .map(|a| {
                        a.iter()
                            .filter_map(|x| x.as_str().map(String::from))
                            .collect()
                    })
                    .unwrap_or_default();
                view::export_records(store, &ids)
            }
            "import/preview" => {
                let text = input
                    .get("fileText")
                    .and_then(|x| x.as_str())
                    .ok_or_else(|| "缺 fileText".to_string())?;
                view::preview_import(text)
            }
            "import" => {
                let text = input
                    .get("fileText")
                    .and_then(|x| x.as_str())
                    .ok_or_else(|| "缺 fileText".to_string())?;
                // 解析失败必须报错而不是吞成"缺省=全部"：用户只勾了一个，静默全量导入
                // 等于把数据面悄悄放大。此前 `.ok()` 正是这么干的。
                let idx: Option<Vec<usize>> = match input.get("indexes") {
                    Some(x) => Some(serde_json::from_value(x.clone())
                        .map_err(|e| format!("indexes 不是合法的下标数组: {e}"))?),
                    None => None,
                };
                view::import_records(store, text, idx.as_deref())
            }
            "switch" => {
                // 这道门不能因为换了宿主就消失：compat 路由接管 switch 后同样要求知情标记。
                // 知情标记收 query（`?confirm=switch`，脚本/curl 用）**或** POST body 里的
                // `confirm:"switch"`（前端 httpCall 对 POST 不拼 query，body 是唯一通道；
                // 跨站表单发不出 application/json body，防护理由不变）。
                let confirmed_by_body =
                    input.get("confirm").and_then(|x| x.as_str()) == Some("switch");
                if !confirmed_by_body && !query.split('&').any(|q| q == "confirm=switch") {
                    return Err(
                        "切换账号会终止并重开目标客户端，必须带 ?confirm=switch（query）或 {\"confirm\":\"switch\"}（body）表示知情".into(),
                    );
                }
                let id = account_id(&input)?;
                let t = target_of(input.get("target"));
                let req = switch::Request {
                    account_id: id,
                    variant: v,
                    target: t,
                    restart: input.get("restart").and_then(|x| x.as_bool()).unwrap_or(true),
                };
                // 与桌面端对齐：forced 走 RealForced（跳过软失败项）。
                // 之前固定 Real，同一操作两个宿主结果会分叉。
                let actor = if input.get("forced").and_then(|x| x.as_bool()).unwrap_or(false) {
                    switch::Actor::RealForced
                } else {
                    switch::Actor::Real
                };
                let j = switch::execute(roots, store, &req, actor, &mut |_| {})?;
                Ok(view::switch_result(&j, req.restart, false))
            }
            // webui 的切换是同步的，走到这里一定是空闲；契约要求这个端点存在，
            // 前端的切换对话框在轮询它（路径是 /api/switch/progress，不是连字符）。
            "switch/progress" => Ok(json!({ "running": false, "progress": null })),
            // 前端读取时是空 body，保存时 POST 的是 `{"config":{…}}` —— 按有无 config 键区分，
            // 不能靠"body 空不空"猜方法：dispatch 拿不到 HTTP method。
            "rotate/config" => match input.get("config") {
                Some(patch) => {
                    let merged = view::merge_ui_config(store, patch)?;
                    Ok(serde_json::to_value(merged).map_err(|e| e.to_string())?)
                }
                None => Ok(serde_json::to_value(view::read_ui_config(store))
                    .map_err(|e| e.to_string())?),
            },
            "rotate/status" => Ok(view::rotate_status(roots, store, v)),
            "rotate/run" => Ok(view::run_rotate(roots, store, v)),
            "rotate/logs" => Ok(view::rotate_logs(store)),
            "notifications" => Ok(view::notifications(notifications::list_at(store)?)),
            "notifications/record" => {
                // POST body：{level,title,description?}；与桌面端同一条 core 路径。
                notifications::record_at(
                    store,
                    input.get("level").and_then(|x| x.as_str()).unwrap_or("info"),
                    input.get("title").and_then(|x| x.as_str()).unwrap_or(""),
                    input.get("description").and_then(|x| x.as_str()),
                )?;
                // 契约声明返回 {recorded:true}；裸 null 会让未来任何读 `.recorded` 的
                // 调用方在 null 上炸掉。
                Ok(json!({ "recorded": true }))
            }
            "notifications/clear" => {
                notifications::clear_at(store)?;
                Ok(json!({ "cleared": true }))
            }
            "credits" => {
                let id = account_id(&input)?;
                Ok(qs_switch_core::modules::quota::fetch_credit_expiry_sync(roots, store, &id, v))
            }
            "credits/stats" => {
                let refresh = input
                    .get("refresh")
                    .and_then(|x| x.as_bool())
                    .or_else(|| query_param(query, "refresh").map(|s| s == "true"))
                    .unwrap_or(false);
                Ok(qs_switch_core::modules::ledger::credit_statistics(roots, store, refresh))
            }
            "checkin/status" => {
                // 带 accountId → 单账号；不带 → 批量。批量只在**显式**带 variant 时筛档，
                // 缺省覆盖两档：webui 的按账号查询会过滤这个返回，缺省锁死 CN 会把
                // 国际版账号漏成"未找到账号"。
                match input.get("accountId").or_else(|| input.get("account_id")).and_then(|x| x.as_str()) {
                    Some(id) => Ok(qs_switch_core::modules::quota::get_checkin_status_sync(roots, store, id, v)),
                    None => {
                        let only = input
                            .get("variant")
                            .and_then(|x| x.as_str())
                            .or_else(|| query_param(query, "variant"))
                            .map(|s| view::variant_from_key(Some(s)));
                        Ok(qs_switch_core::modules::quota::get_checkin_status_all_sync(roots, store, only))
                    }
                }
            }
            "checkin/logs" => Ok(qs_switch_core::modules::ledger::read_checkin_logs(store)),
            // 读时无 config 键、写时带 `{config:{…}}`，与 rotate/config 同一区分法。
            "checkin/config" => match input.get("config") {
                Some(patch) => qs_switch_core::modules::ledger::write_checkin_config(store, patch),
                None => Ok(qs_switch_core::modules::ledger::read_checkin_config(store)),
            },
            "checkin" => {
                let id = account_id(&input)?;
                Ok(qs_switch_core::modules::quota::checkin_sync(roots, store, &id, v))
            }
            "checkin/all" => {
                // 带 variant 只处理该档；不带覆盖全部档位。
                let only = input
                    .get("variant")
                    .and_then(|x| x.as_str())
                    .or_else(|| query_param(query, "variant"))
                    .map(|s| view::variant_from_key(Some(s)));
                Ok(qs_switch_core::modules::quota::checkin_all_sync(roots, store, only))
            }
            "oauth/start" => {
                Ok(qs_switch_core::modules::oauth::oauth_start(v))
            }
            "oauth/status" => {
                let id = input.get("loginId").or_else(|| input.get("login_id")).and_then(|x| x.as_str()).unwrap_or("");
                Ok(qs_switch_core::modules::oauth::oauth_status_sync(id, roots, store))
            }
            _ => Err(format!("契约路由漏了 {cmd}")),
        };
        r
    }
}
