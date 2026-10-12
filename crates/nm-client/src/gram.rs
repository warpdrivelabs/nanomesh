use nm_proto::{Any, Gram, GramKind, PROTOCOL_VERSION, now_ms};

pub(crate) fn text_message(sender: [u8; 32], receiver: [u8; 32], text: &str, gram_id: u64) -> Gram {
    let mut msg = build_gram(
        GramKind::Message,
        sender,
        receiver.to_vec(),
        Some(Any {
            type_url: "nmspace.v1/text".to_string(),
            value: text.as_bytes().to_vec(),
        }),
    );
    msg.gram_id = gram_id;
    msg.crc = nm_crypto::content_hash(text.as_bytes()).to_vec();
    msg
}

pub(crate) fn build_gram(
    kind: GramKind,
    sender: [u8; 32],
    receiver: Vec<u8>,
    payload: Option<Any>,
) -> Gram {
    Gram {
        version: PROTOCOL_VERSION,
        kind: kind as i32,
        gram_id: 1,
        ref_gram_id: None,
        sender: sender.to_vec(),
        receiver,
        timestamp_ms: now_ms(),
        payload,
        crc: Vec::new(),
    }
}
