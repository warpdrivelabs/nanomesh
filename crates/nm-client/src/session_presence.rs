use super::*;

impl Session {
    /// 设置本人在线状态（P2）：online / away / busy / dnd（空/online = 清除意图）。
    pub async fn presence_set(&self, status: &str) -> Result<(), ClientError> {
        let params = Any {
            type_url: "nmspace.v1/presence.status".to_string(),
            value: status.as_bytes().to_vec(),
        };
        let res = rpc_over(&self.conn, self.my_id, "presence.set", Some(params)).await?;
        if res.ok {
            Ok(())
        } else {
            Err(ClientError::Other(res.error))
        }
    }

    /// A：按 id 批量查询在线状态（含跨节点好友）。返回 {hex: "online|away|busy|dnd|offline"}。
    pub async fn presence_query(
        &self,
        ids: &[[u8; 32]],
    ) -> Result<std::collections::HashMap<String, String>, ClientError> {
        let hexed: Vec<String> = ids
            .iter()
            .map(|b| b.iter().map(|x| format!("{x:02x}")).collect())
            .collect();
        let body = serde_json::json!({ "ids": hexed }).to_string();
        let params = Any {
            type_url: "text/plain".to_string(),
            value: body.into_bytes(),
        };
        let res = rpc_over(&self.conn, self.my_id, "presence.query", Some(params)).await?;
        if !res.ok {
            return Err(ClientError::Other(res.error));
        }
        let raw = res.result.map(|p| p.value).unwrap_or_default();
        Ok(serde_json::from_slice(&raw).unwrap_or_default())
    }
}
