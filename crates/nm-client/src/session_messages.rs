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

    /// 收到节点推送的私聊消息后，回送达回执给节点（经 bi，fire-and-forget）。
    /// 调用方应 spawn，勿阻塞收流。节点据此清离线副本 + 转回回执给原发送方（双勾）。
    pub async fn send_receipt(&self, orig: &Gram) -> Result<(), ClientError> {
        let n = self.next_corr.fetch_add(1, Ordering::SeqCst); // 单调 gram_id，防被节点 SeenGrams 误去重
        let mut r = build_gram(GramKind::Receipt, self.my_id, orig.sender.clone(), None);
        r.gram_id = n;
        r.ref_gram_id = Some(orig.gram_id); // 关联原消息（UI 据此匹配本人气泡升双勾）
        let _ = route_send(&self.conn, &r).await; // 吞节点回的 Reply / 老节点 error，不打扰上层
        Ok(())
    }

    /// 应用层心跳：向节点发 Ping，收到任何响应（含老节点 error Reply）即视为连接存活；
    /// 仅超时 / 连接关闭才 Err（上层据此触发重连）。
    pub async fn ping(&self) -> Result<(), ClientError> {
        let n = self.next_corr.fetch_add(1, Ordering::SeqCst);
        let mut p = build_gram(GramKind::Ping, self.my_id, Vec::new(), None); // receiver 空 = 面向节点
        p.gram_id = n;
        let _ = route_send(&self.conn, &p).await?;
        Ok(())
    }
}
