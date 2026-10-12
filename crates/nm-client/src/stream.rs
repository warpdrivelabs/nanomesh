use nm_proto::{CommandResult, Gram};
use prost::Message;
use tokio::sync::mpsc;

/// 流式命令的接收端：逐帧取 [`CommandResult`]（`done` 帧后通道关闭 → `next` 返回 `None`）。
pub struct InferStream {
    pub(crate) rx: mpsc::UnboundedReceiver<Gram>,
}

impl InferStream {
    /// 取下一帧的 `CommandResult`；流结束（done 帧已消费或连接断）返回 `None`。
    pub async fn next(&mut self) -> Option<CommandResult> {
        let gram = self.rx.recv().await?;
        gram.payload
            .and_then(|p| CommandResult::decode(p.value.as_slice()).ok())
    }
}
