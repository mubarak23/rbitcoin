use rbitcoin_query::resolved_wire::*;
use rbitcoin_query::TxPrecompute;
use std::sync::Arc;

#[test]
fn resolved_wire_stamps_n_inputs() {
    let genesis = bitcoin::blockdata::constants::genesis_block(bitcoin::Network::Regtest);
    let n = genesis
        .txdata
        .iter()
        .map(|tx| tx.input.len() as u32)
        .fold(0u32, u32::saturating_add);
    let pres: Arc<[TxPrecompute]> = genesis
        .txdata
        .iter()
        .map(TxPrecompute::from_tx)
        .collect::<Vec<_>>()
        .into();
    let wire = ResolvedWire::new(Arc::new(genesis), pres);
    assert_eq!(wire.n_inputs, n);
    assert_eq!(n, 1);
    let stamped = ResolvedWire {
        block: Arc::clone(&wire.block),
        pres: Arc::clone(&wire.pres),
        n_inputs: 9,
        header_fk: 7,
        spend_keys: Arc::from([[0x11u8; 32]]),
        sender: None,
        heap: Arc::clone(&wire.heap),
    };
    assert_eq!(stamped.n_inputs, 9);
    assert_eq!(stamped.header_fk, 7);
    assert_eq!(wire.header_fk, 0);
    assert_eq!(stamped.spend_keys.as_ref(), &[[0x11u8; 32]]);
}

/// A decoded block holds at least its serialized bytes on the heap: every
/// script and witness is its own allocation, beside a struct per
/// transaction, input and output, and a precompute row per transaction.
#[test]
fn decoded_heap_bytes_cover_the_block_and_its_rows() {
    let block = bitcoin::blockdata::constants::genesis_block(bitcoin::Network::Bitcoin);
    let pres: Arc<[TxPrecompute]> = block
        .txdata
        .iter()
        .map(TxPrecompute::from_tx)
        .collect::<Vec<_>>()
        .into();
    let wire = bitcoin::consensus::serialize(&block).len() as u64;
    let rows = (pres.len() * std::mem::size_of::<TxPrecompute>()) as u64;
    let bytes = decoded_heap_bytes(&block, &pres);
    assert!(bytes >= wire + rows, "{bytes} < {wire} + {rows}");
}

/// A charge counts while the value carrying it lives: a clone counts again,
/// and dropping both gives every byte back.
#[test]
fn a_heap_charge_follows_the_value_that_carries_it() {
    use rbitcoin_query::process_mem_stats::{HeapCharge, HeapGauge};
    static GAUGE: HeapGauge = HeapGauge::new();
    let a = HeapCharge::new(&GAUGE, 100);
    assert_eq!(GAUGE.load(), (1, 100));
    let b = a.clone();
    assert_eq!(GAUGE.load(), (2, 200));
    drop(a);
    assert_eq!(GAUGE.load(), (1, 100));
    drop(b);
    assert_eq!(GAUGE.load(), (0, 0));
}
