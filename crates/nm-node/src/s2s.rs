use crate::grams::s2s_gram;
use crate::NodeError;
use nm_proto::{Any, BlobData, BlobRef, Command, CommandResult, Entity, FedSyncResp, Gram, GramKind};
use nm_transport::{read_gram, write_gram, NodeEndpoint};
use prost::Message;

/// Pull entities homed on a peer via `fed.sync`.
pub(crate) async fn fed_pull(ep: &NodeEndpoint, addr: nm_transport::Addr) -> Result<Vec<Entity>, NodeError> {
    let cmd = Command {
        method: "fed.sync".to_string(),
        params: None,
        correlation_id: 1,
        timeout_ms: 10_000,
        grant: None,
    };
    let gram = s2s_gram(
        ep.id_bytes(),
        GramKind::Command,
        Some(Any {
            type_url: "nmspace.v1.Command".to_string(),
            value: cmd.encode_to_vec(),
        }),
    );
    let resp = s2s_request(ep, addr, &gram).await?;
    let payload = resp.payload.ok_or_else(|| NodeError::Other("fed.sync no payload".into()))?;
    let cr = CommandResult::decode(payload.value.as_slice())
        .map_err(|e| NodeError::Other(e.to_string()))?;
    let body = cr.result.ok_or_else(|| NodeError::Other("fed.sync no result".into()))?;
    let list = FedSyncResp::decode(body.value.as_slice())
        .map_err(|e| NodeError::Other(e.to_string()))?;
    Ok(list.entities)
}

/// Wrap an inner gram in a Relay envelope and send it to the target home node.
pub(crate) async fn fed_relay(ep: &NodeEndpoint, addr: nm_transport::Addr, inner: &Gram) -> Result<(), NodeError> {
    let env = s2s_gram(
        ep.id_bytes(),
        GramKind::Relay,
        Some(Any {
            type_url: "nmspace.v1.Gram".to_string(),
            value: inner.encode_to_vec(),
        }),
    );
    s2s_request(ep, addr, &env).await.map(|_| ())
}

/// Fetch a blob from its home node. Empty `home_node` prevents chain forwarding.
pub(crate) async fn s2s_blob_get(
    ep: &NodeEndpoint,
    addr: nm_transport::Addr,
    hash: &[u8],
) -> Result<BlobData, NodeError> {
    let cmd = Command {
        method: "blob.get".to_string(),
        params: Some(Any {
            type_url: "nmspace.v1.BlobRef".to_string(),
            value: BlobRef { hash: hash.to_vec(), home_node: Vec::new() }.encode_to_vec(),
        }),
        correlation_id: 1,
        timeout_ms: 10_000,
        grant: None,
    };
    let gram = s2s_gram(
        ep.id_bytes(),
        GramKind::Command,
        Some(Any {
            type_url: "nmspace.v1.Command".to_string(),
            value: cmd.encode_to_vec(),
        }),
    );
    let resp = s2s_request(ep, addr, &gram).await?;
    let payload = resp.payload.ok_or_else(|| NodeError::Other("blob.get no payload".into()))?;
    let cr = CommandResult::decode(payload.value.as_slice())
        .map_err(|e| NodeError::Other(e.to_string()))?;
    if !cr.ok {
        return Err(NodeError::Other(if cr.error.is_empty() {
            "blob.get failed".into()
        } else {
            cr.error
        }));
    }
    let body = cr.result.ok_or_else(|| NodeError::Other("blob.get no result".into()))?;
    BlobData::decode(body.value.as_slice()).map_err(|e| NodeError::Other(e.to_string()))
}

async fn s2s_request(ep: &NodeEndpoint, addr: nm_transport::Addr, gram: &Gram) -> Result<Gram, NodeError> {
    let fut = async {
        let conn = ep.connect(addr).await.map_err(|e| NodeError::Other(e.to_string()))?;
        let (mut send, mut recv) = conn.open_bi().await.map_err(|e| NodeError::Other(e.to_string()))?;
        write_gram(&mut send, gram).await.map_err(|e| NodeError::Other(e.to_string()))?;
        let _ = send.finish();
        read_gram(&mut recv).await.map_err(|e| NodeError::Other(e.to_string()))
    };
    match tokio::time::timeout(std::time::Duration::from_secs(10), fut).await {
        Ok(result) => result,
        Err(_) => Err(NodeError::Other("s2s request timed out".into())),
    }
}
