//! 极简 HTTP 路由：把 `POST /api/<cmd>` 的 JSON body 派发到 core，`GET /api/*` 为只读。
//!
//! 手写而不是引 axum：本服务只跑在本机回环上、请求形态固定，多一层框架就把整个 tokio
//! 依赖树拖进来了。这里零新增依赖，路由本身可脱离 socket 单测。
//!
//! 破坏性端点必须带 `?confirm=switch` —— 少这一步，任何本机进程或浏览器里的一个跨站
//! 表单都能把你的账号切掉。

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
        Self::json(400, json!({ "ok": false, "error": e }))
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
    if let Some(r) = compat::dispatch(cmd, query, body) {
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
    let roots = PathRoots::real();
    let store = qs_switch_core::modules::config::switch_root();

    match cmd {
        "status" => Response::ok(json!({
            "store": store.display().to_string(),
            "accounts": bundle::list_all(&store),
            "unfinished": switch::unfinished(&store).unwrap_or_default(),
        })),
        "rotation" => match parse_variant(input.get("variant")) {
            Err(e) => Response::err(e),
            Ok(variant) => {
                match rotate::suggest(&roots, &store, variant, &rotate::RotateConfig::default()) {
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
                (Ok(variant), Ok(target)) => match bundle::capture(&roots, &store, id, variant, target) {
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
        other => Response::json(404, json!({ "ok": false, "error": format!("未知端点: {other}") })),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unknown_endpoint_is_404_not_panic() {
        let r = dispatch("nope", "", "{}");
        assert_eq!(r.status, 404);
        assert!(r.body.contains("未知端点"));
    }

    #[test]
    fn malformed_json_is_rejected() {
        let r = dispatch("capture", "", "{不是 JSON");
        assert_eq!(r.status, 400);
        assert!(r.body.contains("合法 JSON"), "{}", r.body);
    }

    /// 破坏性端点在没有知情标记时必须被挡下，且不产生任何副作用。
    #[test]
    fn switch_requires_explicit_confirmation() {
        let r = dispatch(
            "switch",
            "",
            r#"{"account_id":"main-cn","variant":"cn","target":"desktop"}"#,
        );
        assert_eq!(r.status, 400);
        assert!(r.body.contains("confirm=switch"), "{}", r.body);
    }

    #[test]
    fn bad_variant_and_missing_target_are_reported() {
        let r = dispatch("capture", "", r#"{"account_id":"x","variant":"eu","target":"desktop"}"#);
        assert!(r.body.contains("未知版本"), "{}", r.body);
        let r = dispatch("capture", "", r#"{"account_id":"x","variant":"cn"}"#);
        assert!(r.body.contains("缺 target"), "{}", r.body);
        let r = dispatch("capture", "", "{}");
        assert!(r.body.contains("account_id"), "{}", r.body);
    }

    #[test]
    fn read_only_endpoints_answer() {
        let r = dispatch("status", "", "");
        assert_eq!(r.status, 200, "{}", r.body);
        // compat 路由已把 /api/status 换成前端契约形状（AppStatus）。
        assert!(r.body.contains("authFile"), "应是 AppStatus: {}", r.body);
        let r = dispatch("rotation", "", r#"{"variant":"cn"}"#);
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
        let cases: &[(&str, &str)] = &[
            ("accounts", "accounts"),
            ("status", "authFile"),
            ("capabilities", "unavailable"),
            ("rotate/logs", "logs"),
            ("rotate/status", "cliConfigured"),
        ];
        for (cmd, key) in cases {
            let r = dispatch(cmd, "", "");
            assert_eq!(r.status, 200, "{cmd}: {}", r.body);
            let v: Value = serde_json::from_str(&r.body).expect("合法 JSON");
            assert!(v.get(*key).is_some(), "{cmd} 应在顶层给出 {key}，实得 {v}");
            assert!(v.get("data").is_none(), "{cmd} 不得被包进 data 信封：{v}");
        }
    }

    /// 保存轮换配置时前端发的是 `{"config":{…}}`；只认顶层键的话会把配置静默写回默认值。
    #[test]
    fn rotate_config_save_reads_the_nested_config_key() {
        let r = dispatch(
            "rotate/config",
            "",
            r#"{"config":{"enabled":false,"check_interval_minutes":60,"cooldown_minutes":120,"min_gap_hours":48,"min_urgency_hours":72,"active_guard_minutes":0,"min_remaining_credits":0}}"#,
        );
        assert_eq!(r.status, 200, "{}", r.body);
        let v: Value = serde_json::from_str(&r.body).unwrap();
        assert_eq!(v["enabled"], false);
        assert_eq!(v["min_urgency_hours"], 72);
        // 紧接着的读取必须看到刚落盘的值。
        let back: Value = serde_json::from_str(&dispatch("rotate/config", "", "").body).unwrap();
        assert_eq!(back["min_urgency_hours"], 72, "保存后读取不一致");
        assert_eq!(back["enabled"], false);
    }

    /// 切换对话框会**持续轮询**进度：这个路径拼错一次就是控制台里刷不完的 404。
    #[test]
    fn switch_progress_path_matches_the_frontend_route() {
        let r = dispatch("switch/progress", "", "");
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
    use serde_json::{json, Value};

    use qs_switch_core::modules::config::{switch_root, PathRoots};
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
    ];

    /// 命中则处理并返回 Some，未命中返回 None 交给自有端点。
    pub fn dispatch(cmd: &str, query: &str, body: &str) -> Option<Response> {
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
        Some(match handle(cmd, query, &input) {
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
        cmd: &str,
        query: &str,
        input: &Value,
    ) -> std::result::Result<Value, String> {
        let roots = PathRoots::real();
        let store = switch_root();
        // body 优先，其次 query：GET 类命令只能靠 query 带档位。
        let variant = input
            .get("variant")
            .and_then(|x| x.as_str())
            .or_else(|| query_param(query, "variant"));
        let v = view::variant_from_key(variant);

        let r: Result<Value> = match cmd {
            "status" => Ok(view::app_status(&roots, v)),
            "accounts" => Ok(view::accounts(&roots)),
            "capabilities" => Ok(view::capabilities()),
            "import-local" => {
                let id = account_id_or_local(&input, v);
                let t = target_of(input.get("target"));
                let b = bundle::capture(&roots, &store, &id, v, t)?;
                if b.is_empty() {
                    return Err("该目标在本机不落盘凭据，没有可认领的文件".into());
                }
                Ok(json!({ "ok": true, "account": view::account_meta(&b) }))
            }
            "delete" => {
                let id = account_id(&input)?;
                let dir = qs_switch_core::modules::bundle::accounts_root_in(&store).join(&id);
                if !dir.is_dir() {
                    return Err(format!("账号目录不存在: {}", dir.display()));
                }
                std::fs::remove_dir_all(&dir)
                    .map_err(|e| format!("删除失败: {e}"))?;
                Ok(json!({ "ok": true }))
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
                view::export_records(&store, &ids)
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
                let idx: Option<Vec<usize>> =
                    input.get("indexes").and_then(|x| serde_json::from_value(x.clone()).ok());
                view::import_records(&store, text, idx.as_deref())
            }
            "switch" => {
                // 这道门不能因为换了宿主就消失：compat 路由接管 switch 后同样要求知情标记。
                if !query.split('&').any(|q| q == "confirm=switch") {
                    return Err(
                        "切换账号会终止并重开目标客户端，必须带 ?confirm=switch 表示知情".into(),
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
                let j = switch::execute(&roots, &store, &req, switch::Actor::Real, &mut |_| {})?;
                Ok(view::switch_result(&j, req.restart, false))
            }
            // webui 的切换是同步的，走到这里一定是空闲；契约要求这个端点存在，
            // 前端的切换对话框在轮询它（路径是 /api/switch/progress，不是连字符）。
            "switch/progress" => Ok(json!({ "running": false, "progress": null })),
            // 前端读取时是空 body，保存时 POST 的是 `{"config":{…}}` —— 按有无 config 键区分，
            // 不能靠"body 空不空"猜方法：dispatch 拿不到 HTTP method。
            "rotate/config" => match input.get("config") {
                Some(patch) => {
                    let merged = view::merge_ui_config(&store, patch)?;
                    Ok(serde_json::to_value(merged).map_err(|e| e.to_string())?)
                }
                None => Ok(serde_json::to_value(view::read_ui_config(&store))
                    .map_err(|e| e.to_string())?),
            },
            "rotate/status" => Ok(view::rotate_status(&roots, &store, v)),
            "rotate/run" => Ok(view::run_rotate(&roots, &store, v)),
            "rotate/logs" => Ok(view::rotate_logs(&store)),
            // 应用内通知中心：本宿主不落盘，返回空集合即可让界面正常渲染。
            "notifications" | "notifications/record" | "notifications/clear" => {
                Ok(json!({ "items": [], "cleared": true, "recorded": true }))
            }
            _ => Err(format!("契约路由漏了 {cmd}")),
        };
        r
    }
}
