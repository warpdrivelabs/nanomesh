//! Roster regression: per-account contact list stored on home node.
//! Single-node deterministic tests — no gossip overlay needed.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use nm_entity::kinds;
use nm_proto::pb::PersonProfile;

async fn online(c: &nm_client::Client, addr: &nm_transport::Addr) -> nm_client::Session {
    tokio::time::timeout(Duration::from_secs(20), c.online(addr.clone()))
        .await
        .expect("online timed out")
        .expect("online failed")
}

/// roster_list returns empty for a new account.
/// roster_add stores an entry and returns it.
/// roster_list then returns that entry.
/// roster_remove deletes it.
#[tokio::test]
async fn roster_crud() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("node.redb");
    let node = Arc::new(nm_node::Node::bind_local_persistent([71u8; 32], &db).await.unwrap());
    let addr = node.addr();
    { let n = node.clone(); tokio::spawn(async move { let _ = n.serve().await; }); }

    let a = nm_client::Client::bind_local([72u8; 32]).await.unwrap();
    let a_sess = online(&a, &addr).await;
    a_sess.register_as::<kinds::Person>(&PersonProfile::default(), "Alice", HashMap::new()).await.unwrap();

    // contact B — a different identity seed
    let b_id = nm_client::Client::bind_local([73u8; 32]).await.unwrap().id_bytes();
    let b_hex: String = b_id.iter().map(|x| format!("{x:02x}")).collect();

    // empty list
    let list = a_sess.roster_list().await.expect("roster_list");
    assert!(list.is_empty(), "new account should have empty roster");

    // add
    let entry = a_sess.roster_add(b_id, "person", "Bob", "bob@example.com", "colleague")
        .await.expect("roster_add");
    assert_eq!(entry["id"].as_str().unwrap(), b_hex);
    assert_eq!(entry["kind"].as_str().unwrap(), "person");
    assert_eq!(entry["name"].as_str().unwrap(), "Bob");
    assert_eq!(entry["handle"].as_str().unwrap(), "bob@example.com");
    assert_eq!(entry["remark"].as_str().unwrap(), "colleague");
    assert_eq!(entry["status"].as_str().unwrap(), "active");

    // list returns the entry
    let list = a_sess.roster_list().await.expect("roster_list after add");
    assert_eq!(list.len(), 1);
    assert_eq!(list[0]["id"].as_str().unwrap(), b_hex);

    // update name + handle
    let updated = a_sess.roster_update(b_id, Some("Bobby"), Some("bobby@example.com"), None)
        .await.expect("roster_update");
    assert_eq!(updated["name"].as_str().unwrap(), "Bobby");
    assert_eq!(updated["handle"].as_str().unwrap(), "bobby@example.com");

    // remove
    a_sess.roster_remove(b_id).await.expect("roster_remove");
    let list = a_sess.roster_list().await.expect("roster_list after remove");
    assert!(list.is_empty(), "roster should be empty after remove");
}

/// Two accounts on the same node have independent roster namespaces.
#[tokio::test]
async fn roster_per_account_isolation() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("node.redb");
    let node = Arc::new(nm_node::Node::bind_local_persistent([74u8; 32], &db).await.unwrap());
    let addr = node.addr();
    { let n = node.clone(); tokio::spawn(async move { let _ = n.serve().await; }); }

    let a = nm_client::Client::bind_local([75u8; 32]).await.unwrap();
    let a_sess = online(&a, &addr).await;
    a_sess.register_as::<kinds::Person>(&PersonProfile::default(), "Alice", HashMap::new()).await.unwrap();

    let b = nm_client::Client::bind_local([76u8; 32]).await.unwrap();
    let b_sess = online(&b, &addr).await;
    b_sess.register_as::<kinds::Person>(&PersonProfile::default(), "Bob", HashMap::new()).await.unwrap();

    let c_id = nm_client::Client::bind_local([77u8; 32]).await.unwrap().id_bytes();

    // A adds C
    a_sess.roster_add(c_id, "person", "Charlie", "", "").await.expect("A roster_add");

    // A sees it, B does not
    let a_list = a_sess.roster_list().await.expect("A roster_list");
    let b_list = b_sess.roster_list().await.expect("B roster_list");
    assert_eq!(a_list.len(), 1, "A should see 1 entry");
    assert!(b_list.is_empty(), "B should see empty roster — isolation breach");
}
