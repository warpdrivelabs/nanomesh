use super::*;

impl Session {
    /// 家节点账号口令命令。`name.register` / `name.login` 的参数是 JSON 文本。
    /// 成功时 `name.login` 的结果正文是该名字登记的客户端公钥（hex）。
    pub async fn name_account(&self, method: &str, body: &str) -> Result<String, ClientError> {
        let params = Any {
            type_url: "text/plain".to_string(),
            value: body.as_bytes().to_vec(),
        };
        let res = rpc_over(&self.conn, self.my_id, method, Some(params)).await?;
        if !res.ok {
            return Err(ClientError::Other(res.error));
        }
        Ok(res
            .result
            .map(|p| String::from_utf8_lossy(&p.value).into_owned())
            .unwrap_or_default())
    }

    /// 在 home node 认领本地名 local-part（→ 本客户端公钥）；返回签发的 NameRecord。
    pub async fn name_claim(&self, local_part: &str) -> Result<NameRecord, ClientError> {
        let params = Any {
            type_url: "nmspace.v1.NameOp".to_string(),
            value: NameOp {
                local_part: local_part.to_string(),
            }
            .encode_to_vec(),
        };
        let res = rpc_over(&self.conn, self.my_id, "name.claim", Some(params)).await?;
        if !res.ok {
            return Err(ClientError::Other(res.error));
        }
        let p = res
            .result
            .ok_or_else(|| ClientError::Other("no payload".into()))?;
        NameRecord::decode(p.value.as_slice()).map_err(err)
    }

    /// 解析 name（local@domain 或域名）→ NameRecord（含目标公钥）；无则 None。
    pub async fn name_resolve(&self, name: &str) -> Result<Option<NameRecord>, ClientError> {
        let params = Any {
            type_url: "nmspace.v1.NameQuery".to_string(),
            value: NameQuery {
                name: name.to_string(),
                pubkey: Vec::new(),
            }
            .encode_to_vec(),
        };
        let res = rpc_over(&self.conn, self.my_id, "name.resolve", Some(params)).await?;
        if !res.ok {
            return Err(ClientError::Other(res.error));
        }
        let p = res
            .result
            .ok_or_else(|| ClientError::Other("no payload".into()))?;
        Ok(NameList::decode(p.value.as_slice())
            .map_err(err)?
            .records
            .into_iter()
            .next())
    }

    /// 反向解析 公钥 → NameRecord（规范名）；无则 None。
    pub async fn name_reverse(&self, pubkey: [u8; 32]) -> Result<Option<NameRecord>, ClientError> {
        let params = Any {
            type_url: "nmspace.v1.NameQuery".to_string(),
            value: NameQuery {
                name: String::new(),
                pubkey: pubkey.to_vec(),
            }
            .encode_to_vec(),
        };
        let res = rpc_over(&self.conn, self.my_id, "name.reverse", Some(params)).await?;
        if !res.ok {
            return Err(ClientError::Other(res.error));
        }
        let p = res
            .result
            .ok_or_else(|| ClientError::Other("no payload".into()))?;
        Ok(NameList::decode(p.value.as_slice())
            .map_err(err)?
            .records
            .into_iter()
            .next())
    }
}
