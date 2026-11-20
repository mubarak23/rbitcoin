use bitcoin::block::Header;
use bitcoin::hashes::Hash;
use bitcoin::{BlockHash, CompactTarget, TxMerkleNode};
use rbitcoin_ibd::ibd::peer_chain::PeerChain;
use std::collections::HashMap;

fn chain(n: u32, salt: u8) -> Vec<Header> {
    extend(BlockHash::from_byte_array([0u8; 32]), n, salt)
}

fn extend(from: BlockHash, n: u32, salt: u8) -> Vec<Header> {
    let mut prev = from;
    (1..=n)
        .map(|i| {
            let mut h = Header {
                version: bitcoin::block::Version::from_consensus(4),
                prev_blockhash: prev,
                merkle_root: TxMerkleNode::from_byte_array([salt; 32]),
                time: 1_600_000_000 + i,
                bits: CompactTarget::from_consensus(0x207fffff),
                nonce: i,
            };
            rbitcoin_consensus::grind_regtest_pow(&mut h);
            prev = h.block_hash();
            h
        })
        .collect()
}

/// Two full `headers` replies and then a short one: the peer holds every
/// block of its newest reply, and none past that reply's top.
#[test]
fn an_announced_chain_keeps_the_newest_reply() {
    let headers = chain(4_010, 0x5c);
    let path: HashMap<u32, BlockHash> = (1..).zip(headers.iter().map(|h| h.block_hash())).collect();
    let mut peer = PeerChain::default();
    peer.note_headers(&path, |_| None, 0, 1, &headers[..2_000]);
    peer.note_headers(&path, |_| None, 0, 2_001, &headers[2_000..4_000]);
    peer.note_headers(&path, |_| None, 0, 4_001, &headers[4_000..]);
    assert!(
        !peer.left_path_by(&path, 4_005),
        "the newest reply is what the peer has"
    );
    assert!(!peer.left_path_by(&path, 3_000));
    let mut beyond = path.clone();
    beyond.insert(4_011, BlockHash::from_byte_array([7u8; 32]));
    assert!(
        peer.left_path_by(&beyond, 4_011),
        "a short reply is the end of that peer's chain"
    );
}

/// A full reply whose split height has since been confirmed and left the
/// path. This node's confirmed block there settles it: a different block
/// means the peer left this chain, the same block means this node took it.
#[test]
fn a_split_is_settled_by_the_confirmed_chain() {
    let ours = chain(2_001, 0x5c);
    let theirs = chain(2_000, 0x77);
    let mut path: HashMap<u32, BlockHash> =
        (1..).zip(ours.iter().map(|h| h.block_hash())).collect();
    let mut left = PeerChain::default();
    left.note_headers(&path, |_| None, 0, 1, &theirs);
    let mut taken = PeerChain::default();
    let mut before = path.clone();
    before.insert(1, theirs[0].block_hash());
    taken.note_headers(&before, |_| None, 0, 1, &ours[..2_000]);
    for h in 1..=2_000 {
        path.remove(&h);
    }
    let confirmed = |h: u32| (h <= 2_000).then(|| ours[h as usize - 1].block_hash());
    left.settle_split(&path, confirmed);
    taken.settle_split(&path, confirmed);
    assert!(
        left.left_path_by(&path, 2_001),
        "a peer that split where this node confirmed another block has left"
    );
    assert!(
        !taken.left_path_by(&path, 2_001),
        "a peer whose chain this node confirmed has not"
    );
}

/// A short reply that ends at or below the confirmed tip is all that peer
/// has, so it is not asked above the tip. That holds for a first reply and
/// for one that follows a short run this node has since confirmed.
#[test]
fn a_reply_at_or_below_the_tip_ends_the_chain_there() {
    let headers = chain(20, 0x5c);
    let above = |tip: u32| -> HashMap<u32, BlockHash> {
        (tip + 1..=20)
            .map(|h| (h, headers[h as usize - 1].block_hash()))
            .collect()
    };
    let confirmed = |h: u32| Some(headers[h as usize - 1].block_hash());
    let mut behind = PeerChain::default();
    behind.note_headers(&above(10), confirmed, 10, 5, &headers[4..8]);
    assert!(
        behind.left_path_by(&above(10), 11),
        "a first reply that ends below the tip"
    );
    let mut caught = PeerChain::default();
    caught.note_headers(&above(10), confirmed, 10, 11, &headers[10..15]);
    assert!(caught.left_path_by(&above(10), 16));
    caught.note_headers(&above(15), confirmed, 15, 13, &headers[12..15]);
    assert!(
        caught.left_path_by(&above(15), 16),
        "a reply that ends at the tip after the kept run was confirmed"
    );
}

/// A rewind keeps a split the peer itself showed at or below the fork. A
/// peer on a third chain stays off both chains when this node rewinds away
/// from its chain and back.
#[test]
fn a_third_chain_split_survives_two_rewinds() {
    let ours = chain(3, 0x5c);
    let won = chain(3, 0x77);
    let third = chain(3, 0x91);
    let path = |c: &[Header]| -> HashMap<u32, BlockHash> {
        (1..).zip(c.iter().map(|h| h.block_hash())).collect()
    };
    let confirmed = |h: u32| Some(ours[h as usize - 1].block_hash());
    let mut peer = PeerChain::default();
    peer.note_headers(&HashMap::new(), confirmed, 2, 1, &third);
    peer.note_rewind(&path(&won), 0);
    assert!(peer.left_path_by(&path(&won), 1));
    peer.note_rewind(&path(&ours), 0);
    assert!(
        peer.left_path_by(&path(&ours), 1),
        "a peer on a third chain is not taken back onto this one"
    );
}

/// A rewind leaves a record that took the rewound blocks as the peer's chain
/// stale from the fork up, also when the peer's own split is above the fork,
/// across further rewinds, until the peer answers again. It is probed again.
#[test]
fn a_rewind_leaves_a_record_stale_until_the_peer_answers() {
    let ours = chain(2_003, 0x5c);
    let won = extend(ours[1_996].block_hash(), 3, 0x77);
    let third = extend(ours[1_997].block_hash(), 3, 0x91);
    let at = |c: &[Header], from: u32| -> HashMap<u32, BlockHash> {
        (from..).zip(c.iter().map(|h| h.block_hash())).collect()
    };
    let confirmed = |h: u32| (h <= 2_001).then(|| ours[h as usize - 1].block_hash());
    // A full reply that leaves this chain at 1999, above the fork at 1998.
    let mut reply = ours[1..1_998].to_vec();
    reply.extend_from_slice(&third);
    let mut peer = PeerChain::default();
    peer.note_headers(&HashMap::new(), confirmed, 2_001, 2, &reply);
    let on_won = at(&won, 1_998);
    peer.note_rewind(&on_won, 1_997);
    assert!(peer.left_path_by(&on_won, 1_998));
    assert!(!peer.probed(), "the peer is probed again");
    let on_ours = at(&ours[1_997..], 1_998);
    peer.note_rewind(&on_ours, 1_997);
    assert!(
        peer.left_path_by(&on_ours, 2_003),
        "it left this chain above the fork"
    );
    // A deeper rewind keeps it off from the lower fork.
    let deeper = at(&extend(ours[1_988].block_hash(), 2, 0x33), 1_990);
    peer.note_rewind(&deeper, 1_989);
    assert!(peer.left_path_by(&deeper, 1_990));
    peer.note_headers(&on_ours, |_| None, 1_997, 1_998, &ours[1_997..2_000]);
    assert!(!peer.left_path_by(&on_ours, 1_999));
    assert!(
        peer.left_path_by(&on_ours, 2_001),
        "the reply ended at 2000"
    );

    // A full reply that ends below the fork says nothing there: the record
    // stays stale and the peer is probed again.
    let mut far = PeerChain::default();
    let confirmed_far = |h: u32| (h <= 2_002).then(|| ours[h as usize - 1].block_hash());
    far.note_headers(&HashMap::new(), confirmed_far, 2_002, 2_003, &ours[2_002..]);
    let side = at(&extend(ours[2_000].block_hash(), 2, 0x44), 2_002);
    far.note_rewind(&side, 2_001);
    far.note_headers(&side, confirmed_far, 2_001, 1, &ours[..2_000]);
    assert!(far.left_path_by(&side, 2_002));
    assert!(!far.probed());
    // A full reply that reaches the fork on the new chain answers it.
    let mut onto = ours[3..2_001].to_vec();
    onto.extend(extend(ours[2_000].block_hash(), 2, 0x44));
    far.note_headers(&side, confirmed_far, 2_001, 4, &onto);
    assert!(!far.left_path_by(&side, 2_002));
}

/// A peer that showed a stale sibling and then builds on this node's block
/// at or above that height has joined this chain: the split is gone.
#[test]
fn a_reply_on_this_chain_above_a_split_clears_it() {
    let ours = chain(3, 0x5c);
    let sibling = chain(1, 0x77);
    let path: HashMap<u32, BlockHash> = (1..).zip(ours.iter().map(|h| h.block_hash())).collect();
    let mut peer = PeerChain::default();
    peer.note_headers(&path, |_| None, 0, 1, &sibling);
    assert!(peer.left_path_by(&path, 2));
    peer.note_headers(&path, |_| None, 0, 2, &ours[1..]);
    assert!(!peer.left_path_by(&path, 2));
    assert!(!peer.left_path_by(&path, 3));
    assert!(
        peer.shows(&path, 3),
        "the peer is asked for this chain again"
    );

    // A reply on this node's block that leaves it at the next height: the
    // parent clears the old split and the new one is where it left.
    let mut moved = PeerChain::default();
    moved.note_headers(&path, |_| None, 0, 1, &sibling);
    let other = chain(2, 0x91);
    let off = Header {
        prev_blockhash: ours[0].block_hash(),
        ..other[1]
    };
    moved.note_headers(&path, |_| None, 0, 2, &[off]);
    assert!(
        !moved.left_path_by(&path, 1),
        "it has this node's block below"
    );
    assert!(moved.left_path_by(&path, 2));
}

/// A peer whose record does not decide a height is probed again, but not
/// within 10 s of its last probe.
#[test]
fn a_record_that_stops_short_is_refreshed() {
    use std::time::{Duration, Instant};
    let headers = chain(20, 0x5c);
    let path: HashMap<u32, BlockHash> = (1..).zip(headers.iter().map(|h| h.block_hash())).collect();
    let Some(sent) = Instant::now().checked_sub(Duration::from_secs(30)) else {
        return;
    };
    let mut ended = PeerChain::default();
    ended.note_probe_sent(sent, true);
    ended.note_headers(&path, |_| None, 0, 1, &headers[..10]);
    assert!(
        !ended.needs_refresh(&path, 5, Instant::now()),
        "a height the record shows is decided"
    );
    assert!(
        !ended.needs_refresh(&path, 15, Instant::now()),
        "so is one above where a short reply ended"
    );
    let mut reached = PeerChain::default();
    reached.note_probe_sent(sent, false);
    reached.note_reaches(10, headers[9].block_hash());
    assert!(!reached.needs_refresh(&path, 5, sent + Duration::from_secs(10)));
    assert!(reached.needs_refresh(&path, 15, sent + Duration::from_secs(10)));
    assert!(
        !reached.needs_refresh(&path, 15, sent + Duration::from_millis(9_999)),
        "not within 10 s of the last probe"
    );
}

/// Header walk reads the split after `note_headers`. A reply that does not
/// connect leaves a lower split where it was.
#[test]
fn a_stray_reply_above_keeps_a_lower_split() {
    let ours = chain(6, 0x5c);
    let side = chain(2, 0x77);
    let path: HashMap<u32, BlockHash> = (1..).zip(ours.iter().map(|h| h.block_hash())).collect();
    let mut peer = PeerChain::default();
    peer.note_headers(&path, |_| None, 0, 1, &side[..1]);
    let stray = Header {
        prev_blockhash: BlockHash::from_byte_array([3u8; 32]),
        ..side[1]
    };
    peer.note_headers(&path, |_| None, 0, 5, &[stray]);
    assert!(
        peer.left_path_by(&path, 1),
        "a reply that does not connect leaves the lower split"
    );
}

/// Header walk's off-path split stays when a later reply starts at that
/// height and the block is no longer one this node has.
#[test]
fn a_reply_at_the_split_keeps_it_once_the_block_is_gone() {
    let ours = chain(4, 0x5c);
    let side = chain(1, 0x77);
    let path: HashMap<u32, BlockHash> = HashMap::new();
    let hashes: Vec<BlockHash> = ours.iter().map(|h| h.block_hash()).collect();
    let stray = Header {
        prev_blockhash: ours[1].block_hash(),
        ..side[0]
    };
    let mut peer = PeerChain::default();
    peer.note_headers(
        &path,
        |h| (h <= 3).then(|| hashes[h as usize - 1]),
        3,
        3,
        &[stray],
    );
    peer.note_headers(
        &path,
        |h| (h <= 2).then(|| hashes[h as usize - 1]),
        2,
        3,
        &[ours[2]],
    );
    assert!(
        peer.left_path_by(&path, 3),
        "the split is still where the peer left, after that block is gone"
    );
}

/// Agreeing at the recorded split clears it. Leaving on the next block
/// records that height, so the peer still has the block below.
#[test]
fn agreeing_at_the_split_then_leaving_records_the_new_one() {
    let ours = chain(4, 0x5c);
    let side = chain(2, 0x77);
    let path: HashMap<u32, BlockHash> = (1..).zip(ours.iter().map(|h| h.block_hash())).collect();
    let mut peer = PeerChain::default();
    peer.note_headers(&path, |_| None, 0, 2, &side[..1]);
    let leaving = Header {
        prev_blockhash: ours[1].block_hash(),
        ..side[1]
    };
    peer.note_headers(&path, |_| None, 0, 2, &[ours[1], leaving]);
    assert!(
        !peer.left_path_by(&path, 2),
        "it has this node's block at the old split"
    );
    assert!(peer.left_path_by(&path, 3));
}

/// A disagreement below a higher split moves the split down. Block fetch
/// then skips that peer from the lower height.
#[test]
fn a_lower_disagreement_moves_the_split_down() {
    let ours = chain(6, 0x5c);
    let side = chain(6, 0x77);
    let path: HashMap<u32, BlockHash> = (1..).zip(ours.iter().map(|h| h.block_hash())).collect();
    let mut first = ours[..4].to_vec();
    first.push(side[4]);
    let mut peer = PeerChain::default();
    peer.note_headers(&path, |_| None, 0, 1, &first);
    let lower = Header {
        prev_blockhash: BlockHash::from_byte_array([1u8; 32]),
        ..side[1]
    };
    peer.note_headers(&path, |_| None, 6, 2, &[lower]);
    assert!(
        peer.left_path_by(&path, 2),
        "the split moved down to the disagreement"
    );
}

/// Block fetch asks a peer that `shows` a height. A short reply at or below
/// the tip drops a kept run that sat above the tip.
#[test]
fn a_short_reply_below_the_tip_drops_the_kept_run() {
    let headers = chain(12, 0x5c);
    let path: HashMap<u32, BlockHash> = (1..).zip(headers.iter().map(|h| h.block_hash())).collect();
    let mut peer = PeerChain::default();
    peer.note_headers(&path, |_| None, 0, 10, &headers[9..10]);
    assert!(peer.shows(&path, 10));
    peer.note_headers(&path, |_| None, 5, 3, &headers[2..3]);
    assert!(
        !peer.shows(&path, 3),
        "the short reply dropped the run above the tip"
    );
}

/// Block fetch treats `left_path_by` as the peer missing that block. A record
/// whose first hash is not the path block there has left, including when the
/// path gained that block after the reply.
#[test]
fn a_different_hash_at_the_record_start_has_left() {
    let headers = chain(1, 0x5c);
    let mut path = HashMap::new();
    let mut peer = PeerChain::default();
    peer.note_headers(&path, |_| None, 0, 10, &headers);
    path.insert(10, BlockHash::from_byte_array([9u8; 32]));
    assert!(
        peer.left_path_by(&path, 10),
        "the announced hash is not this node's block"
    );
}

/// Block fetch skips a peer that lacks the hash. A full table of 1024 drops
/// an expired row before it takes a new one, and it does not grow past that.
#[test]
fn a_full_notfound_table_drops_expired_and_refuses_a_fresh_extra() {
    let mut peer = PeerChain::default();
    let old = BlockHash::from_byte_array([0x01; 32]);
    peer.note_notfound(old, 0);
    let mut recent = Vec::new();
    for i in 0..1023u32 {
        let mut bytes = [0x02u8; 32];
        bytes[..4].copy_from_slice(&i.to_le_bytes());
        let hash = BlockHash::from_byte_array(bytes);
        peer.note_notfound(hash, 20_000);
        recent.push(hash);
    }
    let fresh = BlockHash::from_byte_array([0x03; 32]);
    peer.note_notfound(fresh, 30_000);
    assert!(
        peer.lacks(&fresh, 30_000),
        "an expired row gives up its seat"
    );
    assert!(peer.lacks(&recent[0], 30_000));
    assert!(!peer.lacks(&old, 30_000));
    let extra = BlockHash::from_byte_array([0x04; 32]);
    peer.note_notfound(extra, 30_000);
    assert!(!peer.lacks(&extra, 30_000), "1024 live rows is the cap");
}

/// A `notfound` keeps a peer off that block for one stall window, then the
/// peer may be asked again.
#[test]
fn a_notfound_lasts_one_stall_window() {
    let mut peer = PeerChain::default();
    let hash = BlockHash::from_byte_array([3u8; 32]);
    peer.note_notfound(hash, 1_000);
    assert!(peer.lacks(&hash, 1_000 + 29_999));
    assert!(
        !peer.lacks(&hash, 1_000 + 30_000),
        "past the stall window the peer is asked again"
    );
}

/// A `notfound` lasts its full stall window even when the record is full:
/// a new hash is refused while every kept one is fresh, and takes the place
/// of one that expired.
#[test]
fn a_full_notfound_record_keeps_its_fresh_lacks() {
    let mut peer = PeerChain::default();
    let hash = |b: u8| BlockHash::from_byte_array([b; 32]);
    assert!(peer.note_notfound_capped(hash(1), 0, 2));
    assert!(peer.note_notfound_capped(hash(2), 1, 2));
    assert!(
        !peer.note_notfound_capped(hash(3), 2, 2),
        "a full record says it refused"
    );
    assert!(
        peer.lacks(&hash(1), 29_999),
        "the first stays until its TTL"
    );
    assert!(peer.lacks(&hash(2), 29_999));
    assert!(
        !peer.lacks(&hash(3), 2),
        "a full record refuses the new hash"
    );
    peer.note_notfound_capped(hash(3), 30_000, 2);
    assert!(peer.lacks(&hash(3), 30_000), "it takes the expired place");
    assert!(peer.lacks(&hash(2), 30_000));
    assert!(!peer.lacks(&hash(1), 30_000));
}

/// A repeat `notfound` lasts one stall window from the repeat, and expires
/// after the hashes answered before it.
#[test]
fn a_repeated_notfound_holds_from_its_last_answer() {
    let mut peer = PeerChain::default();
    let hash = |b: u8| BlockHash::from_byte_array([b; 32]);
    peer.note_notfound_capped(hash(1), 0, 3);
    peer.note_notfound_capped(hash(2), 1, 3);
    peer.note_notfound_capped(hash(1), 20_000, 3);
    peer.note_notfound_capped(hash(3), 30_001, 3);
    peer.note_notfound_capped(hash(4), 30_002, 3);
    peer.note_notfound_capped(hash(5), 30_003, 3);
    assert!(
        peer.lacks(&hash(1), 30_003),
        "the repeat holds until 50 s and kept its place over a newer hash"
    );
    assert!(!peer.lacks(&hash(2), 30_003), "the hash before it expired");
    assert!(peer.lacks(&hash(3), 30_003) && peer.lacks(&hash(4), 30_003));
    assert!(
        !peer.lacks(&hash(5), 30_003),
        "the record is full of fresh lacks"
    );
}

/// Only unexpired hashes take places in the record: an earlier answer to a
/// hash that was answered again does not.
#[test]
fn a_repeat_takes_one_place() {
    let mut peer = PeerChain::default();
    let hash = |b: u8| BlockHash::from_byte_array([b; 32]);
    assert!(peer.note_notfound_capped(hash(1), 0, 3));
    assert!(peer.note_notfound_capped(hash(2), 1, 3));
    assert!(peer.note_notfound_capped(hash(2), 2, 3));
    assert!(
        peer.note_notfound_capped(hash(3), 3, 3),
        "two hashes are kept, so a third fits"
    );
    assert!(peer.lacks(&hash(1), 3) && peer.lacks(&hash(2), 3) && peer.lacks(&hash(3), 3));
    assert!(!peer.note_notfound_capped(hash(4), 4, 3));
}

/// An announcement lifts the lacks for the blocks it lists and frees their
/// places. The others stay.
#[test]
fn an_announcement_lifts_only_the_lacks_it_lists() {
    let mut peer = PeerChain::default();
    let hash = |b: u8| BlockHash::from_byte_array([b; 32]);
    assert!(peer.note_notfound_capped(hash(1), 0, 2));
    assert!(peer.note_notfound_capped(hash(2), 1, 2));
    peer.note_announcement([hash(1), hash(9)]);
    assert!(!peer.lacks(&hash(1), 2), "the listed block is lifted");
    assert!(peer.lacks(&hash(2), 2), "the other stays");
    assert!(
        peer.note_notfound_capped(hash(3), 2, 2),
        "the lifted block's place is free"
    );
    assert!(peer.lacks(&hash(2), 2) && peer.lacks(&hash(3), 2));
}

/// Repeats inside one millisecond behind an unexpired front still leave
/// one answer per hash once the record drops its skipped answers, so it
/// stays within twice the cap.
#[test]
fn same_ms_repeats_stay_within_the_bound() {
    let mut peer = PeerChain::default();
    let hash = |b: u8| BlockHash::from_byte_array([b; 32]);
    assert!(peer.note_notfound_capped(hash(1), 0, 2));
    for _ in 0..10 {
        assert!(peer.note_notfound_capped(hash(2), 0, 2));
    }
    assert!(
        peer.lacks_answers() <= 4,
        "{} answers",
        peer.lacks_answers()
    );
    assert!(peer.lacks(&hash(1), 0) && peer.lacks(&hash(2), 0));
}
