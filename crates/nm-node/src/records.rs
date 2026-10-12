//! Small record codecs shared by federation discovery paths.

use iroh_dns::pkarr::SignedPacket;
use nm_transport::SecretKey;

pub(crate) fn hex_encode(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

pub(crate) fn hex_decode_n<const N: usize>(s: &str) -> Option<[u8; N]> {
    if s.len() != N * 2 {
        return None;
    }
    let mut out = [0u8; N];
    for (i, byte) in out.iter_mut().enumerate() {
        *byte = u8::from_str_radix(s.get(i * 2..i * 2 + 2)?, 16).ok()?;
    }
    Some(out)
}

/// F2 pkarr group discovery record codec.
pub(crate) mod pkarr_rec {
    use super::{hex_decode_n, hex_encode, SignedPacket, SecretKey};

    pub(crate) const SEED_NAME: &str = "_nmspace";
    pub(crate) const REC_TTL: u32 = 300;
    pub(crate) const MAX_SEEDS: usize = 8;

    /// Sign the group's seed node set as a relay payload.
    pub(crate) fn build(secret: &SecretKey, seeds: &[[u8; 32]]) -> Result<Vec<u8>, String> {
        let values: Vec<String> = seeds.iter().take(MAX_SEEDS).map(|s| hex_encode(s)).collect();
        let pkt = SignedPacket::from_txt_strings(secret, SEED_NAME, values, REC_TTL)
            .map_err(|e| e.to_string())?;
        Ok(pkt.to_relay_payload())
    }

    /// Verify and parse relay payload for a group pubkey into seed node ids.
    pub(crate) fn parse(pubkey: &[u8; 32], payload: &[u8]) -> Result<Vec<[u8; 32]>, String> {
        let pk = nm_transport::Id::from_bytes(pubkey).map_err(|e| e.to_string())?;
        let pkt = SignedPacket::from_relay_payload(&pk, payload).map_err(|e| e.to_string())?;
        Ok(pkt
            .txt_records(SEED_NAME)
            .iter()
            .filter_map(|t| hex_decode_n::<32>(t))
            .collect())
    }
}

/// F6/M1 member bootstrap index record codec.
pub(crate) mod member_rec {
    use super::{hex_decode_n, hex_encode, SignedPacket, SecretKey};

    pub(crate) const NAME: &str = "_nmmember";
    pub(crate) const REC_TTL: u32 = 300;
    pub(crate) const MAX_MEMBERS: usize = 8;

    /// Sign bootstrap member node ids as a relay payload.
    pub(crate) fn build(secret: &SecretKey, members: &[[u8; 32]]) -> Result<Vec<u8>, String> {
        let values: Vec<String> = members.iter().take(MAX_MEMBERS).map(|s| hex_encode(s)).collect();
        let pkt = SignedPacket::from_txt_strings(secret, NAME, values, REC_TTL)
            .map_err(|e| e.to_string())?;
        Ok(pkt.to_relay_payload())
    }

    /// Verify and parse relay payload for an anchor pubkey into bootstrap member ids.
    pub(crate) fn parse(anchor_pubkey: &[u8; 32], payload: &[u8]) -> Result<Vec<[u8; 32]>, String> {
        let pk = nm_transport::Id::from_bytes(anchor_pubkey).map_err(|e| e.to_string())?;
        let pkt = SignedPacket::from_relay_payload(&pk, payload).map_err(|e| e.to_string())?;
        Ok(pkt
            .txt_records(NAME)
            .iter()
            .filter_map(|t| hex_decode_n::<32>(t))
            .collect())
    }
}
