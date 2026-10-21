//! P2 邀请链接：节点为客户端签发**自描述的签名入网凭证**。
//! 客户端提供目标联邦、selfhost relay/pkarr、可选群/频道、有效期；节点盖上权威字段
//! （`seed_node`=本节点、`nonce`、`issuer`+`sig`）后返回。新端验签后据此**零配置**接入本联邦
//! （选用同一 relay/pkarr + 该节点作种子），可选自动加入群/订阅频道。
//! 节点只对自己作种子的事实签名背书；联邦/relay/pkarr 由客户端(其自身可用配置)提供并一并纳入签名防篡改。

use nm_proto::{now_ms, Any, Command, Invite};
use prost::Message;

use crate::Ctx;

/// `invite.create`：客户端填入网信息 → 节点盖 `seed_node`/`nonce`/`issuer`/`sig` 后返回签名 Invite。
pub(crate) fn invite_create(ctx: &Ctx, cmd: &Command) -> (bool, Option<Any>, String) {
    let Some(mut inv) = cmd.params.as_ref().and_then(|p| Invite::decode(p.value.as_slice()).ok()) else {
        return (false, None, "bad Invite params".to_string());
    };
    // 节点权威字段：种子=本节点；nonce 唯一（now_ms ++ 节点前缀）；issuer+sig 由本节点私钥签发。
    inv.seed_node = ctx.node_id.to_vec();
    let mut nonce = now_ms().to_be_bytes().to_vec();
    nonce.extend_from_slice(&ctx.node_id[..8]);
    inv.nonce = nonce;
    nm_crypto::sign_invite(ctx.ep.secret_key(), &mut inv);
    (true, Some(Any { type_url: "nmspace.v1.Invite".to_string(), value: inv.encode_to_vec() }), String::new())
}
