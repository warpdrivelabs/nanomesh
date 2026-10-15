/// Recent delivered `(sender, gram_id, crc[..8])` triples, bounded by FIFO eviction.
///
/// Firehose and per-topic delivery (and reliable retransmits) can duplicate the same
/// gram; this cache keeps delivery idempotent across those paths. The `crc` prefix is
/// part of the key so a client that restarts and reuses a low `gram_id` (next_corr resets
/// to 1) is not mistaken for a duplicate of an older message — see the persistent
/// `dedup_*` tables for the authoritative, restart-surviving version.
#[derive(Default)]
pub(crate) struct SeenGrams {
    set: std::collections::HashSet<(Vec<u8>, u64, [u8; 8])>,
    order: std::collections::VecDeque<(Vec<u8>, u64, [u8; 8])>,
}

impl SeenGrams {
    const CAP: usize = 8192;

    /// Take up to 8 bytes of `crc` as the dedup discriminator (0-padded if shorter).
    fn crc8(crc: &[u8]) -> [u8; 8] {
        let mut out = [0u8; 8];
        let n = crc.len().min(8);
        out[..n].copy_from_slice(&crc[..n]);
        out
    }

    /// `true` means first seen and should be delivered; `false` means duplicate.
    pub(crate) fn first_seen(&mut self, sender: &[u8], gram_id: u64, crc: &[u8]) -> bool {
        let key = (sender.to_vec(), gram_id, Self::crc8(crc));
        if self.set.contains(&key) {
            return false;
        }
        self.set.insert(key.clone());
        self.order.push_back(key);
        if self.order.len() > Self::CAP
            && let Some(old) = self.order.pop_front()
        {
            self.set.remove(&old);
        }
        true
    }
}
