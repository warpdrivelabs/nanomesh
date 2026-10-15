use nm_proto::now_ms;

use crate::records::pkarr_rec;
use crate::{Node, LIVE_TTL_MS};

impl Node {
    /// F2：设置本节点 pkarr relay 端点（nmd 据 infra 透传；`None`=不发布/解析群发现记录）。
    pub fn set_pkarr(&self, url: Option<String>) {
        *self.pkarr_url.write().unwrap() = url;
    }

    pub(crate) fn pkarr_endpoint(&self) -> Option<String> {
        self.pkarr_url.read().unwrap().clone()
    }

    /// F2：把某「带密钥群」的 seed 节点集合（本节点=home + 该群主题上存活的成员 home）签名后 PUT 到
    /// pkarr relay，使无锚节点可据群公钥解析入网 seed。无 pkarr / 无群私钥 / 非 home 则跳过。
    /// pub：供 sweep 周期调用，也便于运维/测试直接触发。
    pub async fn pkarr_publish_group(&self, gid: &[u8]) {
        let Some(url) = self.pkarr_endpoint() else { return };
        let Some(store) = &self.store else { return };
        let Ok(Some(secret_bytes)) = store.group_secret(gid) else { return };
        let Ok(sk_arr) = <[u8; 32]>::try_from(secret_bytes.as_slice()) else { return };
        let sk = nm_transport::SecretKey::from_bytes(&sk_arr);
        let mut seeds: Vec<[u8; 32]> = vec![self.ep.id_bytes()]; // 本节点(home) 为首 seed
        if let Some(gl) = self.per_group_live.get(gid) {
            let now = now_ms();
            for e in gl.iter() {
                if now.saturating_sub(*e.value()) < LIVE_TTL_MS {
                    if let Ok(id) = <[u8; 32]>::try_from(e.key().as_slice()) {
                        if !seeds.contains(&id) {
                            seeds.push(id);
                        }
                    }
                }
            }
        }
        let Ok(payload) = pkarr_rec::build(&sk, &seeds) else { return };
        let endpoint = format!("{}/{}", url.trim_end_matches('/'), sk.public().to_z32());
        match self.http.put(&endpoint).body(payload).send().await {
            Ok(r) if r.status().is_success() => {
                tracing::debug!(seeds = seeds.len(), "pkarr group record published")
            }
            Ok(r) => tracing::debug!(status = %r.status(), "pkarr publish non-success"),
            Err(e) => tracing::debug!("pkarr publish failed: {e}"),
        }
    }

    /// F2：据群公钥从 pkarr relay 解析 seed 节点集合（GET + 验签）。无 pkarr / 解析失败则空。
    /// pub：供 sweep 周期调用，也便于运维/测试直接触发。
    pub async fn pkarr_resolve_group(&self, gid: &[u8]) -> Vec<[u8; 32]> {
        let Some(url) = self.pkarr_endpoint() else { return Vec::new() };
        let Ok(gid_arr) = <[u8; 32]>::try_from(gid) else { return Vec::new() };
        let Ok(pk) = nm_transport::Id::from_bytes(&gid_arr) else { return Vec::new() };
        let endpoint = format!("{}/{}", url.trim_end_matches('/'), pk.to_z32());
        let payload = match self.http.get(&endpoint).send().await {
            Ok(r) if r.status().is_success() => match r.bytes().await {
                Ok(b) => b.to_vec(),
                Err(_) => return Vec::new(),
            },
            _ => return Vec::new(),
        };
        pkarr_rec::parse(&gid_arr, &payload).unwrap_or_default()
    }

    /// P4：频道发现——home 把本节点（该频道定序器）签名为该频道的 seed，PUT 到 pkarr relay。
    /// 复用 `pkarr_rec` 编解码（按频道公钥签名；channel_id 即公钥、私钥在 CHANNEL_SECRETS）。
    /// 无 pkarr / 非 home（无频道私钥）则跳过。晚加入者据频道 id 解析到 seed 即可入网回填历史。
    pub async fn pkarr_publish_channel(&self, channel_id: &[u8]) {
        let Some(url) = self.pkarr_endpoint() else { return };
        let Some(store) = &self.store else { return };
        let Ok(Some(secret_bytes)) = store.channel_secret(channel_id) else { return };
        let Ok(sk_arr) = <[u8; 32]>::try_from(secret_bytes.as_slice()) else { return };
        let sk = nm_transport::SecretKey::from_bytes(&sk_arr);
        let seeds: Vec<[u8; 32]> = vec![self.ep.id_bytes()]; // home 节点为 seed（定序器=权威源）
        let Ok(payload) = pkarr_rec::build(&sk, &seeds) else { return };
        let endpoint = format!("{}/{}", url.trim_end_matches('/'), sk.public().to_z32());
        match self.http.put(&endpoint).body(payload).send().await {
            Ok(r) if r.status().is_success() => tracing::debug!("pkarr channel record published"),
            Ok(r) => tracing::debug!(status = %r.status(), "pkarr channel publish non-success"),
            Err(e) => tracing::debug!("pkarr channel publish failed: {e}"),
        }
    }

    /// P4：据频道公钥从 pkarr relay 解析 seed 节点集合（GET + 验签）。无 pkarr / 解析失败则空。
    pub async fn pkarr_resolve_channel(&self, channel_id: &[u8]) -> Vec<[u8; 32]> {
        self.pkarr_resolve_group(channel_id).await // 同一 relay/编解码；公钥命名空间天然隔离群与频道
    }
}
