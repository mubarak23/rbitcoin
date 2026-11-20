use rbitcoin_primitives::heap_bytes::{hash_table_bytes, vec_bytes};
use std::collections::HashMap;

/// A map's allocation from its capacity: hashbrown's buckets (the next power
/// of two over 8/7 of the capacity, or capacity + 1 below 8), one entry and
/// one control byte each, plus one control group.
#[test]
fn hash_table_bytes_reads_the_bucket_count_back_from_capacity() {
    let m: HashMap<u64, u64> = HashMap::with_capacity(1000);
    assert_eq!(m.capacity(), 1792);
    assert_eq!(hash_table_bytes::<u64, u64>(m.capacity()), 2048 * 17 + 16);

    let small: HashMap<[u8; 32], u64> = HashMap::with_capacity(3);
    assert_eq!(small.capacity(), 3);
    assert_eq!(
        hash_table_bytes::<[u8; 32], u64>(small.capacity()),
        4 * 41 + 16
    );

    assert_eq!(hash_table_bytes::<u64, u64>(0), 0);
}

/// A vec's allocation is its capacity, not its length.
#[test]
fn vec_bytes_counts_capacity() {
    let mut v: Vec<u64> = Vec::with_capacity(100);
    v.push(1);
    assert_eq!(vec_bytes(&v), 800);
}
