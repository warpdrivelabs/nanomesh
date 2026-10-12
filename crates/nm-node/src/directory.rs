use dashmap::DashMap;
use nm_core::Directory;
use nm_proto::{DirectoryQuery, Entity};

/// In-memory entity directory: `entity_id -> Entity`, with kind/capability/attribute filters.
#[derive(Default)]
pub struct MemDirectory {
    pub(crate) entities: DashMap<Vec<u8>, Entity>,
}

impl MemDirectory {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn len(&self) -> usize {
        self.entities.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entities.is_empty()
    }

    /// Last-writer-wins merge. Accepts records whose `updated_at` is newer or equal.
    pub fn merge_lww(&self, entity: Entity) -> bool {
        use dashmap::mapref::entry::Entry;
        match self.entities.entry(entity.entity_id.clone()) {
            Entry::Occupied(mut o) => {
                if entity.updated_at >= o.get().updated_at {
                    o.insert(entity);
                    true
                } else {
                    false
                }
            }
            Entry::Vacant(v) => {
                v.insert(entity);
                true
            }
        }
    }
}

impl Directory for MemDirectory {
    async fn upsert(&self, entity: Entity) -> nm_core::Result<()> {
        self.merge_lww(entity);
        Ok(())
    }

    async fn get(&self, entity_id: &[u8]) -> nm_core::Result<Option<Entity>> {
        Ok(self.entities.get(entity_id).map(|e| e.clone()))
    }

    async fn query(&self, q: &DirectoryQuery) -> nm_core::Result<Vec<Entity>> {
        let out = self
            .entities
            .iter()
            .filter(|e| entity_matches(e.value(), q))
            .map(|e| e.value().clone())
            .collect();
        Ok(out)
    }
}

fn entity_matches(e: &Entity, q: &DirectoryQuery) -> bool {
    if !q.kind_prefix.is_empty() && !e.kind.starts_with(&q.kind_prefix) {
        return false;
    }
    for cap in &q.require_capabilities {
        if !e.capabilities.contains(cap) {
            return false;
        }
    }
    for (k, v) in &q.match_attributes {
        if e.attributes.get(k).map(String::as_str) != Some(v.as_str()) {
            return false;
        }
    }
    true
}
