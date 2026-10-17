use soroban_sdk::{Address, Env, Vec};

use crate::constants::TOP_PAYERS_CAPACITY;
use crate::invoice::{StorageKey, TopPayerEntry};

/// Load the top-payers min-heap from persistent storage.
pub fn get_top_payers_heap(env: &Env) -> Vec<TopPayerEntry> {
    env.storage()
        .persistent()
        .get(&StorageKey::TopPayersHeap)
        .unwrap_or_else(|| Vec::new(env))
}

fn save_top_payers_heap(env: &Env, heap: &Vec<TopPayerEntry>) {
    let key = StorageKey::TopPayersHeap;
    if heap.is_empty() {
        if env.storage().persistent().has(&key) {
            env.storage().persistent().remove(&key);
        }
        return;
    }
    env.storage().persistent().set(&key, heap);
    env.storage()
        .persistent()
        .extend_ttl(&key, 1_000_000, 2_000_000);
}

fn parent_index(index: u32) -> u32 {
    (index.saturating_sub(1)).saturating_div(2)
}

fn left_child(index: u32) -> u32 {
    index.saturating_mul(2).saturating_add(1)
}

fn right_child(index: u32) -> u32 {
    index.saturating_mul(2).saturating_add(2)
}

fn entry_score(entry: &TopPayerEntry) -> u32 {
    entry.score
}

fn sift_up(heap: &mut Vec<TopPayerEntry>, mut index: u32) {
    while index > 0 {
        let parent = parent_index(index);
        let current = heap.get(index).unwrap();
        let parent_entry = heap.get(parent).unwrap();
        if entry_score(&current) >= entry_score(&parent_entry) {
            break;
        }
        heap.set(parent, current);
        heap.set(index, parent_entry);
        index = parent;
    }
}

fn sift_down(heap: &mut Vec<TopPayerEntry>, mut index: u32) {
    let len = heap.len();
    loop {
        let left = left_child(index);
        let right = right_child(index);
        let mut smallest = index;

        if left < len {
            let left_entry = heap.get(left).unwrap();
            let smallest_entry = heap.get(smallest).unwrap();
            if entry_score(&left_entry) < entry_score(&smallest_entry) {
                smallest = left;
            }
        }

        if right < len {
            let right_entry = heap.get(right).unwrap();
            let smallest_entry = heap.get(smallest).unwrap();
            if entry_score(&right_entry) < entry_score(&smallest_entry) {
                smallest = right;
            }
        }

        if smallest == index {
            break;
        }

        let current = heap.get(index).unwrap();
        let swap_entry = heap.get(smallest).unwrap();
        heap.set(index, swap_entry);
        heap.set(smallest, current);
        index = smallest;
    }
}

fn remove_payer_from_heap(heap: &mut Vec<TopPayerEntry>, payer: &Address) {
    let len = heap.len();
    let mut remove_index: Option<u32> = None;
    for i in 0..len {
        let entry = heap.get(i).unwrap();
        if entry.address == *payer {
            remove_index = Some(i);
            break;
        }
    }

    let Some(index) = remove_index else {
        return;
    };

    let last_index = len.saturating_sub(1);
    if index != last_index {
        let last = heap.get(last_index).unwrap();
        heap.set(index, last);
    }
    heap.pop_back();

    if index < heap.len() {
        let current = heap.get(index).unwrap();
        if index > 0 {
            let parent = parent_index(index);
            let parent_entry = heap.get(parent).unwrap();
            if entry_score(&current) < entry_score(&parent_entry) {
                sift_up(heap, index);
                return;
            }
        }
        sift_down(heap, index);
    }
}

fn insert_into_heap(heap: &mut Vec<TopPayerEntry>, payer: Address, score: u32) {
    if heap.len() < TOP_PAYERS_CAPACITY {
        heap.push_back(TopPayerEntry {
            address: payer,
            score,
        });
        sift_up(heap, heap.len().saturating_sub(1));
        return;
    }

    let min_entry = heap.get(0).unwrap();
    if score <= min_entry.score {
        return;
    }

    heap.set(
        0,
        TopPayerEntry {
            address: payer,
            score,
        },
    );
    sift_down(heap, 0);
}

/// Maintain the fixed-size top-payers heap after a score change.
pub fn update_top_payers_on_score_change(env: &Env, payer: &Address, new_score: u32) {
    let mut heap = get_top_payers_heap(env);
    remove_payer_from_heap(&mut heap, payer);
    insert_into_heap(&mut heap, payer.clone(), new_score);
    save_top_payers_heap(env, &heap);
}

/// Return up to `limit` top payers from the maintained heap (descending score).
pub fn get_top_payers(env: &Env, limit: u32) -> Vec<TopPayerEntry> {
    let heap = get_top_payers_heap(env);
    let heap_len = heap.len();
    if heap_len == 0 || limit == 0 {
        return Vec::new(env);
    }

    let take = limit.min(heap_len);
    let mut selected = Vec::new(env);
    let mut used = Vec::new(env);

    for _ in 0..take {
        let mut best_index: Option<u32> = None;
        for i in 0..heap_len {
            if used.contains(i) {
                continue;
            }
            let candidate = heap.get(i).unwrap();
            match best_index {
                None => best_index = Some(i),
                Some(best) => {
                    let best_entry = heap.get(best).unwrap();
                    if candidate.score > best_entry.score {
                        best_index = Some(i);
                    }
                }
            }
        }

        let Some(index) = best_index else {
            break;
        };
        selected.push_back(heap.get(index).unwrap());
        used.push_back(index);
    }

    selected
}
