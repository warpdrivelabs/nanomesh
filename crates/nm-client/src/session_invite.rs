use super::*;
use prost::Message;

impl Session {
    /// P2：请节点签发一张邀请凭证。客户端提供目标联邦 / selfhost relay+pkarr / 可选群频道 / 有效期（0=永不）；
    /// 节点盖上种子(=该节点)+签名后返回。调用方把返回的 `Invite` 编码进链接分享；新端 `nm_crypto::verify_invite`
    /// 验签后据此零配置接入。
    #[allow(clippy::too_many_arguments)]
    pub async fn invite_create(
        &self,
        federation: &str,
        relay_urls: Vec<String>,
        pkarr_url: &str,
        group_id: Option<[u8; 32]>,
        channel_id: Option<[u8; 32]>,
        expires_ms: u64,
    ) -> Result<nm_proto::Invite, ClientError> {
        let inv = nm_proto::Invite {
            federation: federation.to_string(),
            seed_node: Vec::new(),
            relay_urls,
            pkarr_url: pkarr_url.to_string(),
            group_id: group_id.map(|g| g.to_vec()).unwrap_or_default(),
            channel_id: channel_id.map(|c| c.to_vec()).unwrap_or_default(),
            expires_ms,
            nonce: Vec::new(),
            issuer: Vec::new(),
            sig: Vec::new(),
        };
        let params = Any { type_url: "nmspace.v1.Invite".to_string(), value: inv.encode_to_vec() };
        let res = rpc_over(&self.conn, self.my_id, "invite.create", Some(params)).await?;
        if !res.ok {
            return Err(ClientError::Other(res.error));
        }
        let raw = res.result.map(|p| p.value).unwrap_or_default();
        nm_proto::Invite::decode(raw.as_slice()).map_err(|e| ClientError::Other(e.to_string()))
    }
}
