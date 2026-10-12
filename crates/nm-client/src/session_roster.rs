use super::*;

impl Session {
    /// 返回调用方账号的全部联系人。
    pub async fn roster_list(
        &self,
    ) -> Result<Vec<std::collections::HashMap<String, serde_json::Value>>, ClientError> {
        let params = Any {
            type_url: "text/plain".to_string(),
            value: b"{}".to_vec(),
        };
        let res = rpc_over(&self.conn, self.my_id, "roster.list", Some(params)).await?;
        if !res.ok {
            return Err(ClientError::Other(res.error));
        }
        let raw = res.result.map(|p| p.value).unwrap_or_default();
        Ok(serde_json::from_slice(&raw).unwrap_or_default())
    }

    /// 添加或更新一条联系人记录；返回服务端写入后的完整条目。
    pub async fn roster_add(
        &self,
        id: [u8; 32],
        kind: &str,
        name: &str,
        handle: &str,
        remark: &str,
    ) -> Result<serde_json::Value, ClientError> {
        let id_hex: String = id.iter().map(|x| format!("{x:02x}")).collect();
        let body = serde_json::json!({ "id": id_hex, "kind": kind, "name": name, "handle": handle, "remark": remark }).to_string();
        let params = Any {
            type_url: "text/plain".to_string(),
            value: body.into_bytes(),
        };
        let res = rpc_over(&self.conn, self.my_id, "roster.add", Some(params)).await?;
        if !res.ok {
            return Err(ClientError::Other(res.error));
        }
        let raw = res.result.map(|p| p.value).unwrap_or_default();
        Ok(serde_json::from_slice(&raw).unwrap_or(serde_json::Value::Null))
    }

    /// 删除一条联系人记录（幂等）。
    pub async fn roster_remove(&self, id: [u8; 32]) -> Result<(), ClientError> {
        let id_hex: String = id.iter().map(|x| format!("{x:02x}")).collect();
        let body = serde_json::json!({ "id": id_hex }).to_string();
        let params = Any {
            type_url: "text/plain".to_string(),
            value: body.into_bytes(),
        };
        let res = rpc_over(&self.conn, self.my_id, "roster.remove", Some(params)).await?;
        if !res.ok {
            return Err(ClientError::Other(res.error));
        }
        Ok(())
    }

    /// 更新联系人的 name、handle 或 remark（空字符串 = 不覆盖现有值；remark 可清空）。
    pub async fn roster_update(
        &self,
        id: [u8; 32],
        name: Option<&str>,
        handle: Option<&str>,
        remark: Option<&str>,
    ) -> Result<serde_json::Value, ClientError> {
        let id_hex: String = id.iter().map(|x| format!("{x:02x}")).collect();
        let mut m = serde_json::json!({ "id": id_hex });
        if let Some(n) = name {
            m["name"] = serde_json::Value::String(n.to_string());
        }
        if let Some(h) = handle {
            m["handle"] = serde_json::Value::String(h.to_string());
        }
        if let Some(r) = remark {
            m["remark"] = serde_json::Value::String(r.to_string());
        }
        let params = Any {
            type_url: "text/plain".to_string(),
            value: m.to_string().into_bytes(),
        };
        let res = rpc_over(&self.conn, self.my_id, "roster.update", Some(params)).await?;
        if !res.ok {
            return Err(ClientError::Other(res.error));
        }
        let raw = res.result.map(|p| p.value).unwrap_or_default();
        Ok(serde_json::from_slice(&raw).unwrap_or(serde_json::Value::Null))
    }
}
