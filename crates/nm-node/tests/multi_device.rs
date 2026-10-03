//! 多设备子密钥：同账号多台设备同时在线收发、按设备离线信箱、已发同步、管理权限、吊销与紧急冻结。

use std::time::Duration;

use nm_crypto::{sign_device_cert, sign_device_revoke, SecretKey, DEVICE_ROLE_ADMIN, DEVICE_ROLE_MEMBER};
use nm_proto::Gram;

const T: Duration = Duration::from_secs(10);

fn text_of(g: &Gram) -> String {
    String::from_utf8(g.payload.clone().expect("payload").value).unwrap()
}

/// 跳过节点推来的设备事件，取下一条聊天消息。
async fn next_chat(s: &mut nm_client::Session) -> Gram {
    loop {
        let g = tokio::time::timeout(T, s.recv()).await.expect("recv timed out").expect("inbox closed");
        if g.payload.as_ref().is_some_and(|p| p.type_url == "nmspace.v1/device.event") {
            continue;
        }
        return g;
    }
}

async fn next_event(s: &mut nm_client::Session) -> serde_json::Value {
    loop {
        let g = tokio::time::timeout(T, s.recv()).await.expect("event timed out").expect("inbox closed");
        if let Some(p) = g.payload.as_ref().filter(|p| p.type_url == "nmspace.v1/device.event") {
            return serde_json::from_slice(&p.value).unwrap();
        }
    }
}

#[tokio::test]
async fn multi_device_send_receive_revoke() {
    let dir = tempfile::tempdir().unwrap();
    let node = nm_node::Node::bind_local_persistent([60u8; 32], &dir.path().join("node.redb")).await.unwrap();
    node.add_domain("md.test").unwrap();
    let addr = node.addr();
    tokio::spawn(async move {
        let _ = node.serve().await;
    });

    let ak = SecretKey::from_bytes(&[61u8; 32]);
    let account = *ak.public().as_bytes();
    let d1_seed = [62u8; 32];
    let d2_seed = [63u8; 32];
    let d1_pk = *SecretKey::from_bytes(&d1_seed).public().as_bytes();
    let d2_pk = *SecretKey::from_bytes(&d2_seed).public().as_bytes();
    let c1 = sign_device_cert(&ak, d1_pk, "laptop", DEVICE_ROLE_ADMIN, 1);
    let c2 = sign_device_cert(&ak, d2_pk, "desktop", DEVICE_ROLE_MEMBER, 2);

    // 账号私钥直连一次，登记名字和口令（老客户端路径）。
    let ak_client = nm_client::Client::bind_local([61u8; 32]).await.unwrap();
    let ak_sess = ak_client.online(addr.clone()).await.unwrap();
    ak_sess
        .name_account("name.register", r#"{"local":"alice","domain":"md.test","password":"password123"}"#)
        .await
        .expect("register name");
    ak_sess.close().await;

    let dev1 = nm_client::Client::bind_local(d1_seed).await.unwrap();
    let mut s1 = dev1.online_as(addr.clone(), &c1).await.expect("d1 hello");
    assert_eq!(s1.id_bytes(), account);
    assert_eq!(s1.device_id(), d1_pk);
    let dev2 = nm_client::Client::bind_local(d2_seed).await.unwrap();
    let mut s2 = dev2.online_as(addr.clone(), &c2).await.expect("d2 hello");

    // 不是自己的证书：设备公钥与连接不符。
    let stranger = nm_client::Client::bind_local([69u8; 32]).await.unwrap();
    let err = stranger.online_as(addr.clone(), &c1).await.err().expect("mismatch must fail");
    assert!(err.to_string().contains("device_mismatch"), "{err}");

    let bob = nm_client::Client::bind_local([64u8; 32]).await.unwrap();
    let mut sb = bob.online(addr.clone()).await.unwrap();
    let bob_id = sb.id_bytes();

    // 1) 发给账号 → 两台设备都收到。
    sb.send_to(account, "hi alice").await.unwrap();
    assert_eq!(text_of(&next_chat(&mut s1).await), "hi alice");
    assert_eq!(text_of(&next_chat(&mut s2).await), "hi alice");

    // 2) 设备 1 发出 → 对方看到 sender=账号；设备 2 收到已发同步。
    s1.send_to(bob_id, "from laptop").await.unwrap();
    let got = next_chat(&mut sb).await;
    assert_eq!(got.sender, account.to_vec());
    assert_eq!(text_of(&got), "from laptop");
    let synced = next_chat(&mut s2).await;
    assert_eq!(synced.sender, account.to_vec());
    assert_eq!(synced.receiver, bob_id.to_vec());

    // 3) 普通设备不能改口令；管理设备可以。
    let reset = r#"{"local":"alice","domain":"md.test","password":"newpassword1"}"#;
    let e = s2.name_account("name.reset", reset).await.err().expect("member must be denied");
    assert!(e.to_string().contains("not_admin_device"), "{e}");
    s1.name_account("name.reset", reset).await.expect("admin reset");

    // 4) 设备 2 离线时消息按设备入信箱，上线后补投。
    s2.close().await;
    sb.send_to(account, "while d2 offline").await.unwrap();
    assert_eq!(text_of(&next_chat(&mut s1).await), "while d2 offline");
    let mut s2 = dev2.online_as(addr.clone(), &c2).await.expect("d2 re-hello");
    assert_eq!(text_of(&next_chat(&mut s2).await), "while d2 offline");

    // 5) 设备列表与改名。
    s2.device_rename(d2_pk, "office pc").await.unwrap();
    let list = s1.device_list().await.unwrap();
    assert_eq!(list.len(), 2);
    let d2_info = list.iter().find(|d| d.cert.as_ref().unwrap().device == d2_pk.to_vec()).unwrap();
    assert_eq!(d2_info.label, "office pc");
    assert!(d2_info.online);

    // 6) 账号私钥签发吊销 → 设备 2 被断开、无法再上线；设备 1 收到事件。
    let r = sign_device_revoke(&ak, account, d2_pk, 3, "lost", false);
    s1.device_revoke(&r).await.expect("revoke");
    let ev = next_event(&mut s1).await;
    assert_eq!(ev["event"], "revoked");
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert!(s2.close_reason().is_some(), "revoked device session must be closed");
    let e = dev2.online_as(addr.clone(), &c2).await.err().expect("revoked device must not reconnect");
    assert!(e.to_string().contains("device_revoked"), "{e}");
    let list = s1.device_list().await.unwrap();
    assert!(list.iter().any(|d| d.revoked.is_some()));

    // 7) 紧急冻结：凭口令由家节点代签吊销设备 3。
    let d3_seed = [65u8; 32];
    let d3_pk = *SecretKey::from_bytes(&d3_seed).public().as_bytes();
    let c3 = sign_device_cert(&ak, d3_pk, "phone", DEVICE_ROLE_MEMBER, 4);
    let dev3 = nm_client::Client::bind_local(d3_seed).await.unwrap();
    let s3 = dev3.online_as(addr.clone(), &c3).await.expect("d3 hello");
    let e = sb.device_freeze("alice", "md.test", "wrong-password", Some(d3_pk)).await.err().expect("bad pw");
    assert!(e.to_string().contains("bad_password"), "{e}");
    assert_eq!(sb.device_freeze("alice", "md.test", "newpassword1", Some(d3_pk)).await.expect("freeze"), 1);
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert!(s3.close_reason().is_some(), "frozen device session must be closed");
    assert!(dev3.online_as(addr.clone(), &c3).await.is_err());

    // 吊销后消息只到设备 1。
    sb.send_to(account, "after revoke").await.unwrap();
    assert_eq!(text_of(&next_chat(&mut s1).await), "after revoke");

    // 不指定设备：冻结账号全部有效设备（此时只剩设备 1）。
    assert_eq!(sb.device_freeze("alice", "md.test", "newpassword1", None).await.expect("freeze all"), 1);
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert!(s1.close_reason().is_some(), "all devices must be frozen");
}
