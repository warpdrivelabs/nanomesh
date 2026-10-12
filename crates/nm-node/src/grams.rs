use nm_proto::{now_ms, Any, CommandResult, Gram, GramKind, PROTOCOL_VERSION};
use prost::Message;

pub(crate) fn s2s_gram(sender: [u8; 32], kind: GramKind, payload: Option<Any>) -> Gram {
    Gram {
        version: PROTOCOL_VERSION,
        kind: kind as i32,
        gram_id: 1,
        ref_gram_id: None,
        sender: sender.to_vec(),
        receiver: Vec::new(),
        timestamp_ms: now_ms(),
        payload,
        crc: Vec::new(),
    }
}

pub(crate) fn command_result_gram(req: &Gram, cr: &CommandResult) -> Gram {
    reply_gram(
        req,
        GramKind::CommandResult,
        Some(Any {
            type_url: "nmspace.v1.CommandResult".to_string(),
            value: cr.encode_to_vec(),
        }),
    )
}

pub(crate) fn receipt_for(msg: &Gram) -> Gram {
    reply_gram(msg, GramKind::Receipt, None)
}

pub(crate) fn login_ok(login: &Gram) -> Gram {
    reply_gram(login, GramKind::Reply, None)
}

pub(crate) fn reply_gram(req: &Gram, kind: GramKind, payload: Option<Any>) -> Gram {
    Gram {
        version: req.version,
        kind: kind as i32,
        gram_id: req.gram_id.wrapping_add(1),
        ref_gram_id: Some(req.gram_id),
        sender: req.receiver.clone(),
        receiver: req.sender.clone(),
        timestamp_ms: now_ms(),
        payload,
        crc: Vec::new(),
    }
}
