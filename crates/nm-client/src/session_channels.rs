use super::*;

impl Session {
    async fn channel_op(&self, method: &str, op: ChannelOp) -> Result<(), ClientError> {
        let params = Any {
            type_url: "nmspace.v1.ChannelOp".to_string(),
            value: op.encode_to_vec(),
        };
        let res = rpc_over(&self.conn, self.my_id, method, Some(params)).await?;
        if res.ok {
            Ok(())
        } else {
            Err(ClientError::Other(res.error))
        }
    }

    pub async fn channel_create(
        &self,
        channel_id: [u8; 32],
        name: &str,
        topic: &str,
        avatar_url: &str,
        secret: &[u8],
    ) -> Result<(), ClientError> {
        self.channel_op(
            "channel.create",
            ChannelOp {
                channel_id: channel_id.to_vec(),
                name: name.to_string(),
                topic: topic.to_string(),
                avatar_url: avatar_url.to_string(),
                secret: secret.to_vec(),
            },
        )
        .await
    }

    /// owner 设置频道信息（名称/简介/头像）→ gossip 广播给订阅者。
    pub async fn channel_set_meta(
        &self,
        channel_id: [u8; 32],
        name: &str,
        topic: &str,
        avatar_url: &str,
    ) -> Result<(), ClientError> {
        self.channel_op(
            "channel.set_meta",
            ChannelOp {
                channel_id: channel_id.to_vec(),
                name: name.to_string(),
                topic: topic.to_string(),
                avatar_url: avatar_url.to_string(),
                ..Default::default()
            },
        )
        .await
    }

    pub async fn channel_sub(&self, channel_id: [u8; 32], name: &str) -> Result<(), ClientError> {
        self.channel_op(
            "channel.sub",
            ChannelOp {
                channel_id: channel_id.to_vec(),
                name: name.to_string(),
                ..Default::default()
            },
        )
        .await
    }

    pub async fn channel_unsub(&self, channel_id: [u8; 32]) -> Result<(), ClientError> {
        self.channel_op(
            "channel.unsub",
            ChannelOp {
                channel_id: channel_id.to_vec(),
                ..Default::default()
            },
        )
        .await
    }

    pub async fn channel_publish(
        &self,
        channel_id: [u8; 32],
        body: &str,
    ) -> Result<(), ClientError> {
        let params = Any {
            type_url: "nmspace.v1.ChannelPub".to_string(),
            value: ChannelPub {
                channel_id: channel_id.to_vec(),
                body: body.to_string(),
            }
            .encode_to_vec(),
        };
        let res = rpc_over(&self.conn, self.my_id, "channel.publish", Some(params)).await?;
        if res.ok {
            Ok(())
        } else {
            Err(ClientError::Other(res.error))
        }
    }

    pub async fn channel_list(&self) -> Result<Vec<Channel>, ClientError> {
        let res = rpc_over(&self.conn, self.my_id, "channel.list", None).await?;
        if !res.ok {
            return Err(ClientError::Other(res.error));
        }
        let p = res
            .result
            .ok_or_else(|| ClientError::Other("no payload".into()))?;
        Ok(ChannelList::decode(p.value.as_slice())
            .map_err(err)?
            .channels)
    }

    pub async fn channel_backfill(
        &self,
        channel_id: [u8; 32],
        since_seq: u64,
    ) -> Result<Vec<ChannelMsg>, ClientError> {
        let params = Any {
            type_url: "nmspace.v1.ChannelBackfillReq".to_string(),
            value: ChannelBackfillReq {
                channel_id: channel_id.to_vec(),
                since_seq,
                limit: 0, // 0 = 服务端默认页大小；Phase 3 前端可传实际值
            }
            .encode_to_vec(),
        };
        let res = rpc_over(&self.conn, self.my_id, "channel.backfill", Some(params)).await?;
        if !res.ok {
            return Err(ClientError::Other(res.error));
        }
        let p = res
            .result
            .ok_or_else(|| ClientError::Other("no payload".into()))?;
        Ok(ChannelLog::decode(p.value.as_slice()).map_err(err)?.msgs)
    }
}
