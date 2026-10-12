use super::*;

impl Session {
    /// C2：发现网络里的模型 provider（kind=model.*）。给了 `model` 名则按 attributes["model"]/["models"] 过滤。
    pub async fn find_model_providers(
        &self,
        model: Option<&str>,
    ) -> Result<Vec<Entity>, ClientError> {
        let mut list = self
            .directory_query(DirectoryQuery {
                kind_prefix: "model.".into(),
                ..Default::default()
            })
            .await?;
        if let Some(m) = model {
            list.retain(|e| {
                e.attributes.get("model").map(|v| v == m).unwrap_or(false)
                    || e.attributes
                        .get("models")
                        .map(|v| v.split(',').any(|x| x.trim() == m))
                        .unwrap_or(false)
            });
        }
        Ok(list)
    }

    /// C2：向某 provider 发一次 OpenAI 兼容推理（method=model.infer）。
    /// 入参 `openai_request_json` 为 OpenAI `/chat/completions` 请求体字节；返回 completion JSON 字节。
    pub async fn infer(
        &self,
        provider: [u8; 32],
        openai_request_json: &[u8],
    ) -> Result<Vec<u8>, ClientError> {
        self.infer_with_grant(provider, openai_request_json, None)
            .await
    }

    /// C2：带 Grant 的推理（provider 要求授权时用）。
    pub async fn infer_with_grant(
        &self,
        provider: [u8; 32],
        openai_request_json: &[u8],
        grant: Option<nm_proto::Grant>,
    ) -> Result<Vec<u8>, ClientError> {
        let params = Any {
            type_url: "openai.chat.v1".to_string(),
            value: openai_request_json.to_vec(),
        };
        let res = self
            .call_with_grant(provider, "model.infer", Some(params), grant)
            .await?;
        if !res.ok {
            return Err(ClientError::Other(res.error));
        }
        Ok(res.result.map(|a| a.value).unwrap_or_default())
    }

    /// 路由命令 RPC：向目标实体发命令，等待其 CommandResult（超时 10s）。
    pub async fn call(
        &self,
        target: [u8; 32],
        method: &str,
        params: Option<Any>,
    ) -> Result<CommandResult, ClientError> {
        self.call_with_grant(target, method, params, None).await
    }

    /// 带能力授权(Grant)的路由命令 RPC（目标要求鉴权时需提供）。
    pub async fn call_with_grant(
        &self,
        target: [u8; 32],
        method: &str,
        params: Option<Any>,
        grant: Option<nm_proto::Grant>,
    ) -> Result<CommandResult, ClientError> {
        let n = self.next_corr.fetch_add(1, Ordering::SeqCst);
        let cmd = Command {
            method: method.to_string(),
            params,
            correlation_id: n,
            timeout_ms: 10_000,
            grant,
        };
        let gram = {
            let mut g = build_gram(
                GramKind::Command,
                self.my_id,
                target.to_vec(),
                Some(Any {
                    type_url: "nmspace.v1.Command".to_string(),
                    value: cmd.encode_to_vec(),
                }),
            );
            g.gram_id = n; // 关联键
            g
        };
        let (tx, rx) = oneshot::channel();
        self.pending.insert(n, tx);
        // 发出命令（节点回 ack；结果稍后作为推送经 pending 唤醒）。
        if let Err(e) = route_send(&self.conn, &gram).await {
            self.pending.remove(&n);
            return Err(e);
        }
        let result_gram = match tokio::time::timeout(Duration::from_secs(10), rx).await {
            Ok(Ok(g)) => g,
            Ok(Err(_)) => return Err(ClientError::Other("call channel closed".into())),
            Err(_) => {
                self.pending.remove(&n);
                return Err(ClientError::Timeout);
            }
        };
        let payload = result_gram
            .payload
            .ok_or_else(|| ClientError::Other("command result has no payload".into()))?;
        CommandResult::decode(payload.value.as_slice()).map_err(err)
    }

    /// 流式路由命令 RPC：发出命令后立即返回一个 [`InferStream`]；多帧 `CommandResult` 陆续到达（done 帧止）。
    /// 对端须按流式回（`reply_frame` 多帧）；否则只会收到单个终帧。帧乱序由上层按 `seq` 处理。
    pub async fn call_stream(
        &self,
        target: [u8; 32],
        method: &str,
        params: Option<Any>,
    ) -> Result<InferStream, ClientError> {
        let n = self.next_corr.fetch_add(1, Ordering::SeqCst);
        let cmd = Command {
            method: method.to_string(),
            params,
            correlation_id: n,
            timeout_ms: 0,
            grant: None,
        };
        let gram = {
            let mut g = build_gram(
                GramKind::Command,
                self.my_id,
                target.to_vec(),
                Some(Any {
                    type_url: "nmspace.v1.Command".to_string(),
                    value: cmd.encode_to_vec(),
                }),
            );
            g.gram_id = n; // 关联键
            g
        };
        let (tx, rx) = mpsc::unbounded_channel();
        self.pending_streams.insert(n, tx);
        if let Err(e) = route_send(&self.conn, &gram).await {
            self.pending_streams.remove(&n);
            return Err(e);
        }
        Ok(InferStream { rx })
    }

    /// 流式推理（C2）：OpenAI 兼容请求（应带 `"stream": true`），返回逐帧 [`InferStream`]。
    pub async fn infer_stream(
        &self,
        provider: [u8; 32],
        openai_request_json: &[u8],
    ) -> Result<InferStream, ClientError> {
        let params = Any {
            type_url: "openai.chat.v1".to_string(),
            value: openai_request_json.to_vec(),
        };
        self.call_stream(provider, "model.infer", Some(params))
            .await
    }
}
