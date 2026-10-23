//! P1-8 操作员凭据:redb OPERATORS 表 + Argon2 PHC + 作用域 + 启禁用/删除 round-trip。
//! 单节点、确定性、不依赖网络。

use std::sync::Arc;

#[tokio::test]
async fn operator_credentials_roundtrip() {
    let dir = tempfile::tempdir().unwrap();
    let node = Arc::new(
        nm_node::Node::bind_local_persistent([51u8; 32], dir.path().join("n.redb")).await.unwrap(),
    );

    // 新建:read 作用域。
    node.operator_set("ops", "pw-123456", "read").expect("set ops");
    assert_eq!(node.operator_verify("ops", "pw-123456").as_deref(), Some("read"));
    assert_eq!(node.operator_verify("ops", "wrong-pass"), None, "错误密码必须拒");
    assert_eq!(node.operator_verify("ghost", "pw-123456"), None, "不存在必须拒");

    // 改密 + 升 control(upsert)。旧密码随即失效。
    node.operator_set("ops", "pw-654321", "control").expect("upsert ops");
    assert_eq!(node.operator_verify("ops", "pw-654321").as_deref(), Some("control"));
    assert_eq!(node.operator_verify("ops", "pw-123456"), None, "改密后旧密码必须失效");

    // 禁用 → 拒;启用 → 恢复。
    assert!(node.operator_set_disabled("ops", true).unwrap());
    assert_eq!(node.operator_verify("ops", "pw-654321"), None, "禁用必须拒");
    assert!(node.operator_set_disabled("ops", false).unwrap());
    assert_eq!(node.operator_verify("ops", "pw-654321").as_deref(), Some("control"));

    // 列表不泄露 PHC(结构仅 name/scope/disabled)。
    let rows = node.operators_list();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0], ("ops".to_string(), "control".to_string(), false));

    // 入参校验:坏名字 / 坏作用域 / 短密码。
    assert!(node.operator_set("bad name!", "pw-123456", "read").is_err(), "名字须 header 安全");
    assert!(node.operator_set("x", "pw-123456", "root").is_err(), "作用域仅 read|control");
    assert!(node.operator_set("x", "123", "read").is_err(), "密码至少 6 位");

    // 删除 → 拒;幂等。
    assert!(node.operator_del("ops").unwrap());
    assert_eq!(node.operator_verify("ops", "pw-654321"), None);
    assert!(!node.operator_del("ops").unwrap(), "再删返回不存在");
}

#[tokio::test]
async fn token_eq_constant_time_compare() {
    assert!(nm_node::Node::token_eq("secret-token", "secret-token"));
    assert!(!nm_node::Node::token_eq("secret-token", "secret-tokeN"));
    assert!(!nm_node::Node::token_eq("", "x"));
    assert!(nm_node::Node::token_eq("", ""));
}
