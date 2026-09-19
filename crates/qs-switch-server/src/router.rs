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
}

impl Response {
    fn json(status: u16, v: Value) -> Self {
        Self { status, content_type: "application/json", body: v.to_string() }
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
        assert!(r.body.contains("\"store\""));
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
}
