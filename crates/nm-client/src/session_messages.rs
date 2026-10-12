use super::*;

impl Session {
    /// 向目标实体发送一条文本消息（节点路由到其在线会话），返回节点 ack。
    pub async fn send_to(&self, target: [u8; 32], text: &str) -> Result<Gram, ClientError> {
        let n = self.next_corr.fetch_add(1, Ordering::SeqCst);
        let msg = text_message(self.my_id, target, text, n);
        route_send(&self.conn, &msg).await
    }

    /// 向群发消息（节点扇出到各成员，在线路由/离线入库）。返回节点 ack。
    pub async fn send_group(&self, group_id: [u8; 32], text: &str) -> Result<Gram, ClientError> {
        let n = self.next_corr.fetch_add(1, Ordering::SeqCst);
        let mut msg = text_message(self.my_id, group_id, text, n);
        msg.kind = GramKind::GroupMessage as i32;
        route_send(&self.conn, &msg).await
    }

    /// 私聊或群的富消息。`json` 为 `nmspace.v1/chat` 载荷，媒体本体在 blob 里。
    pub async fn send_rich(
        &self,
        target: [u8; 32],
        json: &str,
        group: bool,
    ) -> Result<Gram, ClientError> {
        let n = self.next_corr.fetch_add(1, Ordering::SeqCst);
        let mut msg = build_gram(
            if group {
                GramKind::GroupMessage
            } else {
                GramKind::Message
            },
            self.my_id,
            target.to_vec(),
            Some(Any {
                type_url: "nmspace.v1/chat".to_string(),
                value: json.as_bytes().to_vec(),
            }),
        );
        msg.gram_id = n;
        msg.crc = nm_crypto::content_hash(json.as_bytes()).to_vec();
        route_send(&self.conn, &msg).await
    }

    /// 私聊通道上的自定义载荷（如设备迁移握手），接收方按 `type_url` 分派。
    pub async fn send_typed(
        &self,
        target: [u8; 32],
        type_url: &str,
        body: &[u8],
    ) -> Result<Gram, ClientError> {
        let n = self.next_corr.fetch_add(1, Ordering::SeqCst);
        let mut msg = build_gram(
            GramKind::Message,
            self.my_id,
            target.to_vec(),
            Some(Any {
                type_url: type_url.to_string(),
                value: body.to_vec(),
            }),
        );
        msg.gram_id = n;
        msg.crc = nm_crypto::content_hash(body).to_vec();
        route_send(&self.conn, &msg).await
    }
}
