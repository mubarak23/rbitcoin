//! Heap a container holds, from its capacity, for the IBD heap meters.
//!
//! A meter reads capacity, not length: a map that grew to a high-water mark
//! and was emptied still holds its buckets.

/// Bytes a std (hashbrown) `HashMap<K, V>` with this `capacity()` holds:
/// its buckets, one `(K, V)` and one control byte each, plus one control
/// group. hashbrown keeps 7/8 of a power-of-two bucket count usable, or
/// one less than the bucket count below 8 buckets.
///
/// `capacity()` is only exact for a map that has not removed: a removed
/// entry leaves a tombstone that `capacity()` stops counting while the
/// buckets stay. A map that removes is metered from the highest
/// `capacity()` it showed after an insert.
pub fn hash_table_bytes<K, V>(capacity: usize) -> u64 {
    if capacity == 0 {
        return 0;
    }
    let buckets = if capacity < 8 {
        (capacity + 1).next_power_of_two()
    } else {
        (capacity * 8 / 7).next_power_of_two()
    };
    (buckets * (std::mem::size_of::<(K, V)>() + 1) + 16) as u64
}

/// Bytes a `Vec<T>` holds: its capacity, not its length.
pub fn vec_bytes<T>(v: &Vec<T>) -> u64 {
    (v.capacity() * std::mem::size_of::<T>()) as u64
}
