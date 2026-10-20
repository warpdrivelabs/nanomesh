use super::*;
use prost::Message;

impl Session {
    /// P1-6：把某会话（`conversation` = 对端 EntityId / group_id，32 字节）的已读位推进到 `read_up_to_ms`。
    /// 服务端单调写入（只进不退）并扇到本账号其它**在线**设备；离线设备下次登录调 [`Session::read_list`] 追平。
    pub async fn read_mark(&self, conversation: [u8; 32], read_up_to_ms: u64) -> Result<(), ClientError> {
        let marker = nm_proto::ReadMarker { conversation: conversation.to_vec(), read_up_to_ms };
        let params = Any {
            type_url: "nmspace.v1.ReadMarker".to_string(),
            value: marker.encode_to_vec(),
        };
        let res = rpc_over(&self.conn, self.my_id, "read.mark", Some(params)).await?;
        if res.ok {
            Ok(())
        } else {
            Err(ClientError::Other(res.error))
        }
    }

    /// P1-6：拉取本账号全部会话的已读位 `(conversation, read_up_to_ms)`。
    /// 登录后调用以追平离线期间别的设备产生的已读变化（在线变化则经 StateSync gram 实时收到）。
    pub async fn read_list(&self) -> Result<Vec<([u8; 32], u64)>, ClientError> {
        let res = rpc_over(&self.conn, self.my_id, "read.list", None).await?;
        if !res.ok {
            return Err(ClientError::Other(res.error));
        }
        let raw = res.result.map(|p| p.value).unwrap_or_default();
        let list = nm_proto::ReadMarkerList::decode(raw.as_slice())
            .map_err(|e| ClientError::Other(e.to_string()))?;
        let mut out = Vec::with_capacity(list.markers.len());
        for m in list.markers {
            if let Ok(conv) = <[u8; 32]>::try_from(m.conversation.as_slice()) {
                out.push((conv, m.read_up_to_ms));
            }
        }
        Ok(out)
    }
}
