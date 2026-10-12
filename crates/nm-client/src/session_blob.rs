use super::*;

impl Session {
    /// 上传一个内容寻址 blob（P1 头像等）；返回 (hash, home_node)。hash 由节点按 blake3 计算。
    pub async fn blob_put(
        &self,
        data: Vec<u8>,
        mime: &str,
    ) -> Result<(Vec<u8>, Vec<u8>), ClientError> {
        let params = Any {
            type_url: "nmspace.v1.BlobPut".to_string(),
            value: BlobPut {
                data,
                mime: mime.to_string(),
            }
            .encode_to_vec(),
        };
        let res = rpc_over(&self.conn, self.my_id, "blob.put", Some(params)).await?;
        if !res.ok {
            return Err(ClientError::Other(res.error));
        }
        let payload = res
            .result
            .ok_or_else(|| ClientError::Other("no payload".into()))?;
        let br = BlobRef::decode(payload.value.as_slice()).map_err(err)?;
        Ok((br.hash, br.home_node))
    }

    /// 按 hash 取一个 blob（本节点未命中时，节点据 home_node 回源）；返回 (data, mime)。
    pub async fn blob_get(
        &self,
        hash: Vec<u8>,
        home_node: Vec<u8>,
    ) -> Result<(Vec<u8>, String), ClientError> {
        let params = Any {
            type_url: "nmspace.v1.BlobRef".to_string(),
            value: BlobRef { hash, home_node }.encode_to_vec(),
        };
        let res = rpc_over(&self.conn, self.my_id, "blob.get", Some(params)).await?;
        if !res.ok {
            return Err(ClientError::Other(res.error));
        }
        let payload = res
            .result
            .ok_or_else(|| ClientError::Other("no payload".into()))?;
        let bd = BlobData::decode(payload.value.as_slice()).map_err(err)?;
        Ok((bd.data, bd.mime))
    }
}
