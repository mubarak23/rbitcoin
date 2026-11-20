use bitcoin::block::{Header, Version};
use bitcoin::hashes::Hash;
use bitcoin::{Block, BlockHash, CompactTarget, Transaction, TxMerkleNode};

use rbitcoin_net::cache::*;

#[test]
fn default_body_depth_is_compact_serve_window() {
    assert_eq!(DEFAULT_BODY_DEPTH, 16);
}

fn block_at(prev: BlockHash, nonce: u32) -> Block {
    let header = Header {
        version: Version::from_consensus(4),
        prev_blockhash: prev,
        merkle_root: TxMerkleNode::from_byte_array([0; 32]),
        time: 1,
        bits: CompactTarget::from_consensus(0x207f_ffff),
        nonce,
    };
    Block {
        header,
        txdata: vec![Transaction {
            version: bitcoin::transaction::Version::TWO,
            lock_time: bitcoin::absolute::LockTime::ZERO,
            input: vec![],
            output: vec![],
        }],
    }
}

#[test]
fn replacing_the_tip_drops_the_block_already_at_that_height() {
    let cache = BlockCache::with_body_depth(16);
    let genesis = block_at(BlockHash::from_byte_array([0; 32]), 1);
    cache.push_best(0, genesis.clone());
    let first = block_at(genesis.block_hash(), 2);
    cache.push_best(1, first);
    let replacement = block_at(genesis.block_hash(), 3);
    cache.push_best(1, replacement.clone());
    assert_eq!(cache.hash_at_height(0), Some(genesis.block_hash()));
    assert_eq!(cache.hash_at_height(1), Some(replacement.block_hash()));
    assert_eq!(cache.hash_at_height(2), None);
}
