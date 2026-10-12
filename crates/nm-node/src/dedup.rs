/// Recent delivered `(sender, gram_id)` pairs, bounded by FIFO eviction.
///
/// Firehose and per-topic delivery can duplicate the same gram; this cache keeps
/// delivery idempotent across those paths.
#[derive(Default)]
pub(crate) struct SeenGrams {
    set: std::collections::HashSet<(Vec<u8>, u64)>,
    order: std::collections::VecDeque<(Vec<u8>, u64)>,
}

impl SeenGrams {
    const CAP: usize = 8192;

    /// `true` means first seen and should be delivered; `false` means duplicate.
    pub(crate) fn first_seen(&mut self, sender: &[u8], gram_id: u64) -> bool {
        let key = (sender.to_vec(), gram_id);
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
