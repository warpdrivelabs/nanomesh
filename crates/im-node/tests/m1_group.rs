//! M0/M1 群组：混合实体群（人 + Agent + 离线设备）。群消息扇出——
//! 在线成员实时收到、离线成员上线后补投；非成员发送被拒。

use std::collections::HashMap;
use std::time::Duration;

use im_entity::kinds;
use im_proto::pb::{AgentProfile, DeviceProfile, PersonProfile};
use im_proto::GramKind;

async fn online(seed: [u8; 32], addr: &im_transport::Addr) -> (im_client::Client, im_client::Session) {
    let c = im_client::Client::bind_local(seed).await.unwrap();
    let s = tokio::time::timeout(Duration::from_secs(20), c.online(addr.clone()))
        .await
        .expect("online timed out")
        .expect("online failed");
    (c, s)
}

#[tokio::test]
async fn mixed_group_fanout_with_offline() {
    let dir = tempfile::tempdir().unwrap();
    let node = im_node::Node::bind_local_persistent([61u8; 32], dir.path().join("n.redb"))
        .await
        .expect("bind node");
    let addr = node.addr();
    tokio::spawn(async move {
        let _ = node.serve().await;
    });

    // 人(owner) 上线注册。
    let (_pc, person) = online([62u8; 32], &addr).await;
    person
        .register_as::<kinds::Person>(&PersonProfile::default(), "群主", HashMap::new())
        .await
        .unwrap();

    // Agent 上线注册。
    let (_ac, mut agent) = online([63u8; 32], &addr).await;
    agent
        .register_as::<kinds::AgentAssistant>(
            &AgentProfile { backend: "claude".into(), ..Default::default() },
            "助手",
            HashMap::new(),
        )
        .await
        .unwrap();
    let agent_id = agent.id_bytes();

    // 设备的身份（种子固定），暂不上线（离线成员）。
    let device_seed = [64u8; 32];
    let device_id = im_client::Client::bind_local(device_seed).await.unwrap().id_bytes();

    // 人创建群，拉入 Agent 和（离线的）设备。
    let gid = [70u8; 32];
    person.group_create(gid, "混合群").await.unwrap();
    // 成员加入：Agent 自己 join；设备由 owner 侧无法直接加——这里让 Agent join，
    // 设备通过“自己 join”模拟：先上线 join 再下线，验证离线补投。
    agent.group_join(gid).await.unwrap();

    // 设备上线、join、随即显式关闭连接下线。
    {
        let (_dc, device) = online(device_seed, &addr).await;
        device
            .register_as::<kinds::IotDevice>(&DeviceProfile::default(), "传感器", HashMap::new())
            .await
            .unwrap();
        device.group_join(gid).await.unwrap();
        device.close().await; // 显式下线 → 节点推送将失败并转离线队列
    }

    tokio::time::sleep(Duration::from_millis(200)).await;

    // 人向群发消息。
    let ack = person.send_group(gid, "大家好").await.unwrap();
    assert_eq!(ack.kind(), GramKind::Receipt);

    // 在线的 Agent 应实时收到群消息。
    let got = tokio::time::timeout(Duration::from_secs(10), agent.recv())
        .await
        .expect("agent recv timed out")
        .expect("agent no msg");
    assert_eq!(got.kind(), GramKind::GroupMessage);
    assert_eq!(String::from_utf8(got.payload.clone().unwrap().value).unwrap(), "大家好");
    let _ = agent_id;

    // 设备重新上线 → 应收到补投的群消息（离线入库）。
    let (_dc2, mut device2) = online(device_seed, &addr).await;
    let pushed = tokio::time::timeout(Duration::from_secs(10), device2.recv())
        .await
        .expect("device recv timed out")
        .expect("device no offline msg");
    assert_eq!(pushed.kind(), GramKind::GroupMessage);
    assert_eq!(String::from_utf8(pushed.payload.unwrap().value).unwrap(), "大家好");
    let _ = device_id;
}
