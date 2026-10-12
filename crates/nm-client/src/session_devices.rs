use super::*;

impl Session {
    pub async fn device_list(&self) -> Result<Vec<DeviceInfo>, ClientError> {
        let res = rpc_over(&self.conn, self.my_id, "device.list", None).await?;
        if !res.ok {
            return Err(ClientError::Other(res.error));
        }
        let p = res
            .result
            .ok_or_else(|| ClientError::Other("no payload".into()))?;
        Ok(DeviceList::decode(p.value.as_slice()).map_err(err)?.devices)
    }

    pub async fn device_rename(&self, device: [u8; 32], label: &str) -> Result<(), ClientError> {
        let hex: String = device.iter().map(|b| format!("{b:02x}")).collect();
        let body = serde_json::json!({ "device": hex, "label": label }).to_string();
        self.name_account("device.rename", &body).await.map(|_| ())
    }

    /// 提交账号私钥签好的吊销记录。
    pub async fn device_revoke(&self, revoke: &DeviceRevoke) -> Result<(), ClientError> {
        let params = Any {
            type_url: "nmspace.v1.DeviceRevoke".into(),
            value: revoke.encode_to_vec(),
        };
        let res = rpc_over(&self.conn, self.my_id, "device.revoke", Some(params)).await?;
        if res.ok {
            Ok(())
        } else {
            Err(ClientError::Other(res.error))
        }
    }

    /// 紧急冻结：凭账号口令请家节点代签吊销。`device` 为 None 时冻结该账号全部设备；返回冻结台数。
    pub async fn device_freeze(
        &self,
        local: &str,
        domain: &str,
        password: &str,
        device: Option<[u8; 32]>,
    ) -> Result<usize, ClientError> {
        let hex: String = device
            .map(|d| d.iter().map(|b| format!("{b:02x}")).collect())
            .unwrap_or_default();
        let body = serde_json::json!({ "local": local, "domain": domain, "password": password, "device": hex }).to_string();
        let n = self.name_account("device.freeze", &body).await?;
        Ok(n.trim().parse().unwrap_or(0))
    }
}
