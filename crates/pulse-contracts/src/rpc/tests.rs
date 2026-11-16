use std::collections::HashSet;

use serde_json::json;

use super::*;
use crate::ids::{SubscriptionId, Ulid};

#[test]
fn every_method_parses_and_names_are_unique() {
    let mut seen = HashSet::new();
    for name in METHOD_NAMES {
        assert!(seen.insert(*name), "duplicate {name}");
        let (noun, verb) = name.split_once('.').expect("noun.verb");
        assert!(
            ["events", "rules", "agents", "sources", "runs", "projects", "system"].contains(&noun),
            "{name}"
        );
        assert!(verb.bytes().all(|b| b.is_ascii_lowercase() || b == b'_'), "{name}");
    }
    assert_eq!(Request::parse("system.status", Value::Null).unwrap().method(), "system.status");
    assert_eq!(Request::parse("system.pause", json!({})).unwrap().method(), "system.pause");
    assert_eq!(Request::parse("events.query", json!({})).unwrap().method(), "events.query");
    let err = Request::parse("rules.get", json!({})).unwrap_err();
    assert_eq!(err.kind(), Some(ErrorCode::InvalidParams));
    let err = Request::parse("system.status", json!({"extra": 1})).unwrap_err();
    assert_eq!(err.kind(), Some(ErrorCode::InvalidParams));
    let err = Request::parse("rules.explode", json!({})).unwrap_err();
    assert_eq!(err.kind(), Some(ErrorCode::MethodNotFound));
}

#[test]
fn frames() {
    let req = request::<method::RulesGet>(RequestId::Number(7), &NameParams { name: "x".into() })
        .unwrap();
    assert_eq!(
        serde_json::to_value(&req).unwrap(),
        json!({"jsonrpc": "2.0", "id": 7, "method": "rules.get", "params": {"name": "x"}})
    );
    let ok = JsonRpcResponse::ok(Some(RequestId::String("a".into())), json!({}));
    assert_eq!(
        serde_json::to_value(&ok).unwrap(),
        json!({"jsonrpc": "2.0", "id": "a", "result": {}})
    );
    let err = JsonRpcResponse::err(None, RpcError::not_found("rule", "x"));
    let v = serde_json::to_value(&err).unwrap();
    assert_eq!(v["error"]["code"], -32002);
    assert_eq!(v["id"], Value::Null);
    let back: JsonRpcResponse = serde_json::from_value(v).unwrap();
    assert_eq!(
        decode_response::<method::RulesGet>(back).unwrap_err().kind(),
        Some(ErrorCode::NotFound)
    );
    assert!(
        serde_json::from_value::<JsonRpcRequest>(json!({"jsonrpc": "1.0", "method": "x"})).is_err()
    );

    let n: JsonRpcNotification = Notification::RunsChanged(RunsChanged {
        subscription: SubscriptionId::from_ulid(Ulid::from_parts(1, 1)),
        runs: vec![],
    })
    .into();
    let v = serde_json::to_value(&n).unwrap();
    assert_eq!(v["jsonrpc"], "2.0");
    assert_eq!(v["method"], "runs.changed");
    assert!(v["params"]["runs"].is_array());
    assert_eq!(serde_json::from_value::<JsonRpcNotification>(v).unwrap(), n);
}
