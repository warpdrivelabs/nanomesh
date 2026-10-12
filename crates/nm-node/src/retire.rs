use dashmap::{DashMap, DashSet};
use nm_proto::{Entity, Group};

/// F5 live ledger: group/account id -> (node id -> last seen timestamp in ms).
pub(crate) type GroupLive = DashMap<Vec<u8>, DashMap<Vec<u8>, u64>>;

/// F1 readiness predicate: all remote member homes have advertised per-topic support.
pub(crate) fn group_fully_per_topic(
    node_id: &[u8],
    entities: &DashMap<Vec<u8>, Entity>,
    per_topic_nodes: &DashSet<Vec<u8>>,
    group: &Group,
) -> bool {
    for member in &group.members {
        let Some(entity) = entities.get(member) else {
            return false;
        };
        if entity.home_node.is_empty() {
            return false;
        }
        if entity.home_node.as_slice() == node_id {
            continue;
        }
        if !per_topic_nodes.contains(&entity.home_node) {
            return false;
        }
    }
    true
}

/// F5 predicate: all remote member homes are fresh on this group's per-topic overlay.
pub(crate) fn group_topic_retireable(
    node_id: &[u8],
    entities: &DashMap<Vec<u8>, Entity>,
    live: &GroupLive,
    group: &Group,
    now_ms: u64,
    ttl_ms: u64,
) -> bool {
    let group_live = live.get(&group.group_id);
    for member in &group.members {
        let Some(entity) = entities.get(member) else {
            return false;
        };
        if entity.home_node.is_empty() {
            return false;
        }
        if entity.home_node.as_slice() == node_id {
            continue;
        }
        let fresh = group_live
            .as_ref()
            .and_then(|nodes| {
                nodes
                    .get(&entity.home_node)
                    .map(|ts| now_ms.saturating_sub(*ts) < ttl_ms)
            })
            .unwrap_or(false);
        if !fresh {
            return false;
        }
    }
    true
}

/// F3 predicate: a remote recipient's home is fresh on its inbox overlay.
pub(crate) fn inbox_retireable(
    account: &[u8],
    node_id: &[u8],
    entities: &DashMap<Vec<u8>, Entity>,
    live: &GroupLive,
    now_ms: u64,
    ttl_ms: u64,
) -> bool {
    let Some(entity) = entities.get(account) else {
        return false;
    };
    if entity.home_node.is_empty() || entity.home_node.as_slice() == node_id {
        return false;
    }
    live
        .get(account)
        .and_then(|nodes| {
            nodes
                .get(&entity.home_node)
                .map(|ts| now_ms.saturating_sub(*ts) < ttl_ms)
        })
        .unwrap_or(false)
}
