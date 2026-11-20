use bitcoin::hashes::Hash;
use bitcoin::{Block, BlockHash, CompactTarget, Target};
use rbitcoin_ibd::ibd::reorg::*;
use rbitcoin_net::chain::ChainHub;
use rbitcoin_net::error::NetError;
use rbitcoin_primitives::Height;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

/// An unproved walk may rewind through its lead, which is deeper than 1,024.
/// A proved walk still stops at [`REWIND_MAX_DEPTH`].
///
/// One chain of 1,025. The refused rewinds do not disconnect, so the unproved
/// apply is the only call that moves the tip.
#[test]
fn unproved_lead_rewinds_past_1024_and_a_proved_walk_does_not() {
    let (dir, mut hub) = tmp_hub();
    let (kept, side) = confirm_past_1024(&hub, 1_500_080_000);
    let path = [side];
    assert!(
        prepare_header_rewind(&hub, &path, REWIND_MAX_DEPTH)
            .unwrap()
            .is_none(),
        "1,025 blocks is past the proved cap, and this call must not move the tip"
    );
    assert_eq!(hub.tip_height(), Some(1_025));
    let mut proved =
        rbitcoin_ibd::ibd::state::IbdWorkState::new(Vec::new(), hub.tip_hash(), hub.tip_height());
    proved.header_walk.origin = true;
    proved.header_walk.proven = true;
    assert_eq!(
        rbitcoin_ibd::ibd::header_walk::rewind_depth_cap(&proved, &hub),
        REWIND_MAX_DEPTH
    );
    assert!(!rewind_under_own_cap(&mut proved, &hub, &path).unwrap());
    assert_eq!(hub.tip_height(), Some(1_025));
    let mut unproved =
        rbitcoin_ibd::ibd::state::IbdWorkState::new(Vec::new(), hub.tip_hash(), Some(1_025));
    unproved.header_walk.origin = true;
    unproved.header_walk.proven = false;
    hub.milestone = floor_just_above(hub.chain_work().unwrap());
    assert_eq!(
        rbitcoin_ibd::ibd::header_walk::rewind_depth_cap(&unproved, &hub),
        rbitcoin_ibd::ibd::header_walk::UNPROVEN_LEAD_BLOCKS
    );
    assert!(rewind_under_own_cap(&mut unproved, &hub, &path).unwrap());
    assert_eq!(hub.tip_height(), Some(0));
    assert!(
        hub.query
            .get_header_by_hash(kept.as_byte_array())
            .unwrap()
            .is_some(),
        "the abandoned header row stays"
    );
    let _ = dir;
}

/// Unreadable confirmed work keeps an unproved origin walk on the wide cap.
/// Deleting `!` in that fallback uses 1,024 and would leave a 1,025 tip in
/// place. A proved walk stays on 1,024 even when the read fails.
#[test]
fn unreadable_chain_work_keeps_the_unproved_lead() {
    let (dir, hub) = tmp_hub();
    let (_kept, _side) = confirm_past_1024(&hub, 1_500_120_000);
    hub.test_clear_chain_work_prefix();
    hub.query
        .store()
        .confirmed
        .set(
            &rbitcoin_store::ClassCWrite::for_test(),
            rbitcoin_primitives::Height(1),
            rbitcoin_primitives::Fk(9_000),
        )
        .unwrap();
    assert!(hub.chain_work().is_err());
    let mut st =
        rbitcoin_ibd::ibd::state::IbdWorkState::new(Vec::new(), hub.tip_hash(), Some(1_025));
    st.header_walk.origin = true;
    st.header_walk.proven = false;
    assert_eq!(
        rbitcoin_ibd::ibd::header_walk::rewind_depth_cap(&st, &hub),
        rbitcoin_ibd::ibd::header_walk::UNPROVEN_LEAD_BLOCKS
    );
    st.header_walk.proven = true;
    assert_eq!(
        rbitcoin_ibd::ibd::header_walk::rewind_depth_cap(&st, &hub),
        REWIND_MAX_DEPTH
    );
    let _ = dir;
}

/// `publish_work` marks the heavier headers proved before the confirmed tip
/// leaves. That tip is still the unproved lead, so the rewind covers it.
/// Once the confirmed tip meets the floor, 1,025 blocks is refused.
#[test]
fn proved_headers_still_rewind_a_tip_under_the_floor() {
    let (dir, mut hub) = tmp_hub();
    let (kept, side) = confirm_past_1024(&hub, 1_500_110_000);
    let work = hub.chain_work().unwrap();
    // The refused rewind does not disconnect. It runs first on this chain.
    hub.milestone = floor_at(work);
    let mut proved =
        rbitcoin_ibd::ibd::state::IbdWorkState::new(Vec::new(), hub.tip_hash(), hub.tip_height());
    proved.header_walk.origin = true;
    proved.header_walk.tip.work = work;
    rbitcoin_ibd::ibd::header_walk::publish_work(&mut proved, &hub);
    assert!(proved.header_walk.proven);
    assert_eq!(REWIND_MAX_DEPTH, 1_024);
    assert!(
        !rewind_under_own_cap(&mut proved, &hub, &[side]).unwrap(),
        "once the confirmed tip meets the floor, 1,025 is past the cap"
    );
    assert_eq!(hub.tip_height(), Some(1_025));

    hub.milestone = floor_just_above(work);
    let mut st =
        rbitcoin_ibd::ibd::state::IbdWorkState::new(Vec::new(), hub.tip_hash(), hub.tip_height());
    st.header_walk.origin = true;
    // The new headers already cleared the floor. The confirmed chain has not.
    st.header_walk.tip.work = bitcoin::Work::from_be_bytes([0xff; 32]);
    rbitcoin_ibd::ibd::header_walk::publish_work(&mut st, &hub);
    assert!(
        st.header_walk.proven,
        "the heavier headers are proved before the tip moves"
    );
    assert!(
        rewind_under_own_cap(&mut st, &hub, &[side]).unwrap(),
        "a proved header fork still moves a confirmed tip that is under the floor"
    );
    assert_eq!(hub.tip_height(), Some(0));
    assert!(
        hub.query
            .get_header_by_hash(kept.as_byte_array())
            .unwrap()
            .is_some(),
        "the abandoned header row stays"
    );
    let _ = dir;
}

fn confirm_past_1024(hub: &ChainHub, time0: u32) -> (BlockHash, BlockHash) {
    hub.ensure_genesis().unwrap();
    let gen = hub.tip_hash().unwrap();
    hub.confirm_empty_regtest(1_025).unwrap();
    let prev = hub.tip_hash().unwrap();
    let side = distinct_sib(mine(gen, time0.saturating_add(100), 1), prev);
    hub.ensure_header(&side.header).unwrap();
    (prev, side.block_hash())
}

fn floor_at(work: bitcoin::Work) -> rbitcoin_consensus::Milestone {
    rbitcoin_consensus::Milestone {
        height: 1_000_000,
        anchor: Some(rbitcoin_consensus::MilestoneAnchor {
            hash: BlockHash::from_byte_array([0xab; 32]),
            min_work_be: work.to_be_bytes(),
        }),
    }
}

fn floor_just_above(work: bitcoin::Work) -> rbitcoin_consensus::Milestone {
    let mut min_work_be = work.to_be_bytes();
    for byte in min_work_be.iter_mut().rev() {
        let (next, wrapped) = byte.overflowing_add(1);
        *byte = next;
        if !wrapped {
            break;
        }
    }
    rbitcoin_consensus::Milestone {
        height: 1_000_000,
        anchor: Some(rbitcoin_consensus::MilestoneAnchor {
            hash: BlockHash::from_byte_array([0xab; 32]),
            min_work_be,
        }),
    }
}

/// [`apply_header_rewind`] under the cap this state puts in force.
fn rewind_under_own_cap(
    st: &mut rbitcoin_ibd::ibd::state::IbdWorkState,
    hub: &ChainHub,
    path: &[BlockHash],
) -> Result<bool, NetError> {
    let cap = rbitcoin_ibd::ibd::header_walk::rewind_depth_cap(st, hub);
    apply_header_rewind(st, hub, path, cap)
}

fn tmp_hub() -> (rbitcoin_query::testutil::TempDir, ChainHub) {
    rbitcoin_net::chain::tiny_regtest_hub_labeled("ibd-reorg")
}

fn mine(prev: BlockHash, time: u32, height: u32) -> Block {
    rbitcoin_consensus::mine_empty_regtest(prev, time, height)
}

#[test]
fn header_hashes_to_best_ancestor_walks_mid_path() {
    let (dir, hub) = tmp_hub();
    hub.ensure_genesis().unwrap();
    let gen = hub.tip_hash().unwrap();
    let l1 = mine(gen, 1_500_060_100, 1);
    hub.accept_block(l1.clone()).unwrap();
    // Side path not confirmed: w1 → w2
    let mut w1 = mine(gen, 1_500_060_101, 1);
    if w1.block_hash() == l1.block_hash() {
        let target = Target::from_compact(w1.header.bits);
        for nonce in 0..u32::MAX {
            w1.header.nonce = nonce;
            if w1.header.validate_pow(target).is_ok() && w1.block_hash() != l1.block_hash() {
                break;
            }
        }
    }
    hub.ensure_header(&w1.header).unwrap();
    let w2 = mine(w1.block_hash(), 1_500_060_200, 2);
    hub.ensure_header(&w2.header).unwrap();
    let path = header_hashes_to_best_ancestor(&hub, w2.block_hash()).unwrap();
    assert_eq!(
        path,
        vec![w1.block_hash(), w2.block_hash()],
        "oldest-first mid path to LCA"
    );
    // Already on best chain → empty.
    assert!(header_hashes_to_best_ancestor(&hub, l1.block_hash())
        .unwrap()
        .is_empty());
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn reorg_state_held_awaiting_need_getdata() {
    let mut st = IbdReorgState::new();
    assert!(st.need_getdata().is_empty());
    let gen = BlockHash::from_byte_array([0x11; 32]);
    let bits = CompactTarget::from_consensus(0x207f_ffff);
    let held = mine(gen, 1_300_000_000, 1);
    let mut need_h = mine(gen, 1_300_000_001, 1);
    if need_h.block_hash() == held.block_hash() {
        let target = Target::from_compact(bits);
        for nonce in 0..u32::MAX {
            need_h.header.nonce = nonce;
            if need_h.header.validate_pow(target).is_ok()
                && need_h.block_hash() != held.block_hash()
            {
                break;
            }
        }
    }
    let need = need_h.block_hash();
    st.register_explore([need], None);
    assert_eq!(st.need_getdata(), vec![need]);
    st.hold_body(need_h);
    assert!(st.need_getdata().is_empty(), "held satisfies need");
    let explore = mine(gen, 1_300_000_050, 1);
    let eh = explore.block_hash();
    st.register_explore([eh], Some(eh));
    assert_eq!(st.need_getdata(), vec![eh]);
    st.hold_body(explore);
    assert!(st.need_getdata().is_empty());
    let mut st_cap = IbdReorgState::new();
    let mut held_keys = Vec::new();
    let mut prev = gen;
    for i in 0u32..(IbdReorgState::HELD_CAP as u32 + 4) {
        let b = mine(prev, 1_300_001_000 + i, 1);
        prev = b.block_hash();
        held_keys.push(b.block_hash());
        st_cap.hold_body(b);
    }
    for k in &held_keys {
        st_cap.register_explore([*k], None);
    }
    let still = held_keys
        .iter()
        .filter(|k| !st_cap.need_getdata().contains(k))
        .count();
    assert_eq!(
        still,
        IbdReorgState::HELD_CAP,
        "held map must stay exactly HELD_CAP after overflow inserts"
    );
    let _ = held;
}

fn distinct_sib(mut b: Block, avoid: BlockHash) -> Block {
    if b.block_hash() == avoid {
        let target = Target::from_compact(b.header.bits);
        for nonce in 0..u32::MAX {
            b.header.nonce = nonce;
            if b.header.validate_pow(target).is_ok() && b.block_hash() != avoid {
                break;
            }
        }
    }
    b
}

/// Tip on a 2-block loser; heavier winner headers do not connect at
/// tip+1. Name the connecting hashes (not a dead-tip child).
#[test]
fn heavier_disconnected_path_names_connecting_hashes() {
    let (dir, hub) = tmp_hub();
    hub.ensure_genesis().unwrap();
    let gen = hub.tip_hash().unwrap();
    let l1 = mine(gen, 1_500_060_100, 1);
    hub.accept_block(l1.clone()).unwrap();
    let l2 = mine(l1.block_hash(), 1_500_060_200, 2);
    hub.accept_block(l2.clone()).unwrap();
    assert_eq!(hub.tip_hash().unwrap(), l2.block_hash());

    let w1 = distinct_sib(mine(gen, 1_500_060_101, 1), l1.block_hash());
    hub.ensure_header(&w1.header).unwrap();
    let w2 = mine(w1.block_hash(), 1_500_060_201, 2);
    hub.ensure_header(&w2.header).unwrap();
    let w3 = mine(w2.block_hash(), 1_500_060_301, 3);
    hub.ensure_header(&w3.header).unwrap();
    let w4 = mine(w3.block_hash(), 1_500_060_401, 4);
    hub.ensure_header(&w4.header).unwrap();

    // Connected loser child is not a disconnected heavier path.
    let l3 = mine(l2.block_hash(), 1_500_060_300, 3);
    hub.ensure_header(&l3.header).unwrap();
    assert!(
        connecting_hashes_heavier_disconnected_n(&hub, l3.block_hash(), ANCESTOR_WALK_MAX)
            .unwrap()
            .is_none(),
        "child of current tip is a normal extension, not a connecting search"
    );

    let path = connecting_hashes_heavier_disconnected_n(&hub, w4.block_hash(), ANCESTOR_WALK_MAX)
        .unwrap()
        .expect("heavier winner that does not connect at tip must name a path");
    assert_eq!(
        path,
        vec![
            w1.block_hash(),
            w2.block_hash(),
            w3.block_hash(),
            w4.block_hash()
        ],
        "path must be winner mids from LCA, not the loser tip+1"
    );
    assert!(!path.contains(&l3.block_hash()));
    let _ = std::fs::remove_dir_all(dir);
}

/// Work-path tip+1 / far header (no resume seed) still registers the
/// connecting prefix — the live IBD hook, not BadPrev.
#[test]
fn consider_disconnected_from_work_path_without_resume_seed() {
    let (dir, hub) = tmp_hub();
    hub.ensure_genesis().unwrap();
    let gen = hub.tip_hash().unwrap();
    let l1 = mine(gen, 1_500_062_100, 1);
    hub.accept_block(l1.clone()).unwrap();
    let l2 = mine(l1.block_hash(), 1_500_062_200, 2);
    hub.accept_block(l2.clone()).unwrap();
    let w1 = distinct_sib(mine(gen, 1_500_062_101, 1), l1.block_hash());
    hub.ensure_header(&w1.header).unwrap();
    let w2 = mine(w1.block_hash(), 1_500_062_201, 2);
    hub.ensure_header(&w2.header).unwrap();
    let w3 = mine(w2.block_hash(), 1_500_062_301, 3);
    hub.ensure_header(&w3.header).unwrap();
    let w4 = mine(w3.block_hash(), 1_500_062_401, 4);
    hub.ensure_header(&w4.header).unwrap();

    let mut st =
        rbitcoin_ibd::ibd::state::IbdWorkState::new(Vec::new(), hub.tip_hash(), hub.tip_height());
    st.record_height(w3.block_hash(), 3);
    st.note_path_prev(w3.block_hash(), w2.block_hash());
    st.record_height(w4.block_hash(), 4);
    st.max_ordered_height = 4;
    let shared = std::sync::Mutex::new(st);
    assert!(rewind_off_coord_lock(&shared, &hub).unwrap());
    let st = shared.into_inner().unwrap_or_else(|err| err.into_inner());
    assert_eq!(
        hub.tip_height(),
        Some(0),
        "competing tip+1 must rewind to the LCA"
    );
    assert_eq!(st.height_to_hash.get(&1), Some(&w1.block_hash()));
    assert_eq!(st.height_to_hash.get(&2), Some(&w2.block_hash()));
    assert!(
        st.header_fks.contains_key(&w1.block_hash()),
        "a rewound header keeps the fk the body path needs"
    );
    assert!(
        st.reorg.need_getdata().is_empty(),
        "winner is a linear extension after rewind; need={:?}",
        st.reorg.need_getdata()
    );
    let _ = std::fs::remove_dir_all(dir);
}

/// The walk's candidate forks below the confirmed tip, as after a retire
/// walks back and a heavier chain arrives that forked under it. None of it
/// is planted, so the path's next height names no fork. Its stored tip is
/// where the rewind walks back from. A walk tip that is not stored is not,
/// and once a path is planted the walk's tip is not read from the store.
#[test]
fn a_walk_candidate_forked_below_the_tip_is_rewound_to() {
    let (dir, hub) = tmp_hub();
    hub.ensure_genesis().unwrap();
    let gen = hub.tip_hash().unwrap();
    let l1 = mine(gen, 1_500_063_100, 1);
    hub.accept_block(l1.clone()).unwrap();
    let l2 = mine(l1.block_hash(), 1_500_063_200, 2);
    hub.accept_block(l2.clone()).unwrap();
    let w1 = distinct_sib(mine(gen, 1_500_063_101, 1), l1.block_hash());
    let w2 = mine(w1.block_hash(), 1_500_063_201, 2);
    let w3 = mine(w2.block_hash(), 1_500_063_301, 3);
    for w in [&w1, &w2, &w3] {
        hub.ensure_header(&w.header).unwrap();
    }
    let w4 = mine(w3.block_hash(), 1_500_063_401, 4);

    let st =
        rbitcoin_ibd::ibd::state::IbdWorkState::new(Vec::new(), hub.tip_hash(), hub.tip_height());
    let shared = Mutex::new(st);
    {
        let mut st = shared.lock().unwrap_or_else(|err| err.into_inner());
        st.header_walk.tip.hash = Some(w4.block_hash());
        st.header_walk.tip.height = 4;
    }
    assert!(!consider_disconnected_heavier(&shared, &hub).unwrap());
    assert_eq!(
        hub.tip_height(),
        Some(2),
        "an unstored walk tip is not a rewind"
    );

    {
        let mut st = shared.lock().unwrap_or_else(|err| err.into_inner());
        st.header_walk.tip.hash = Some(w3.block_hash());
        st.header_walk.tip.height = 3;
    }
    assert!(consider_disconnected_heavier(&shared, &hub).unwrap());
    assert_eq!(hub.tip_height(), Some(0), "rewound to the fork");
    {
        let st = shared.lock().unwrap_or_else(|err| err.into_inner());
        assert_eq!(st.height_to_hash.get(&1), Some(&w1.block_hash()));
        assert_eq!(st.height_to_hash.get(&3), Some(&w3.block_hash()));
    }
    reset_parent_hash_of_count();
    assert!(!consider_disconnected_heavier(&shared, &hub).unwrap());
    assert_eq!(
        parent_hash_of_count(),
        0,
        "a planted path is not a store walk"
    );
    let _ = std::fs::remove_dir_all(dir);
}

struct ClearRewindHook;
impl Drop for ClearRewindHook {
    fn drop(&mut self) {
        arm_after_header_rewind_prepare_for_test(None);
        *HOOK_HUB.lock().unwrap_or_else(|err| err.into_inner()) = None;
        *HOOK_SHARED.lock().unwrap_or_else(|err| err.into_inner()) = None;
        HOOK_G1.lock().unwrap_or_else(|err| err.into_inner()).take();
    }
}

static HOOK_HUB: Mutex<Option<Arc<ChainHub>>> = Mutex::new(None);
static HOOK_SHARED: Mutex<Option<Arc<Mutex<rbitcoin_ibd::ibd::state::IbdWorkState>>>> =
    Mutex::new(None);
static HOOK_G1: Mutex<Option<BlockHash>> = Mutex::new(None);
static HOOK_RAN: AtomicBool = AtomicBool::new(false);

/// Stands in for the header thread committing a deeper fork after this
/// prepare already disconnected, and before its commit.
fn commit_deeper_fork_after_prepare() {
    let hub = HOOK_HUB
        .lock()
        .unwrap_or_else(|err| err.into_inner())
        .as_ref()
        .expect("hub armed")
        .clone();
    let shared = HOOK_SHARED
        .lock()
        .unwrap_or_else(|err| err.into_inner())
        .as_ref()
        .expect("shared armed")
        .clone();
    let g1 = HOOK_G1
        .lock()
        .unwrap_or_else(|err| err.into_inner())
        .expect("side hash armed");
    let prep = prepare_header_rewind(&hub, &[g1], REWIND_MAX_DEPTH)
        .unwrap()
        .expect("deeper fork disconnects to genesis");
    let mut st = shared.lock().unwrap_or_else(|err| err.into_inner());
    commit_header_rewind(&mut st, &hub, &[g1], &prep);
    HOOK_RAN.store(true, Ordering::Release);
}

/// The coordinator prepared a rewind to L1. The header thread then rewound
/// to genesis and committed that fork. The prepared L1 commit must not run.
#[test]
fn stale_heavier_rewind_does_not_commit_after_the_tip_moves() {
    let _clear = ClearRewindHook;
    HOOK_RAN.store(false, Ordering::Release);
    let (dir, hub) = tmp_hub();
    hub.ensure_genesis().unwrap();
    let gen = hub.tip_hash().unwrap();
    let l1 = mine(gen, 1_500_064_100, 1);
    hub.accept_block(l1.clone()).unwrap();
    let l2 = mine(l1.block_hash(), 1_500_064_200, 2);
    hub.accept_block(l2.clone()).unwrap();
    let w2 = distinct_sib(mine(l1.block_hash(), 1_500_064_201, 2), l2.block_hash());
    let w3 = mine(w2.block_hash(), 1_500_064_301, 3);
    let w4 = mine(w3.block_hash(), 1_500_064_401, 4);
    let w5 = mine(w4.block_hash(), 1_500_064_501, 5);
    for w in [&w2, &w3, &w4, &w5] {
        hub.ensure_header(&w.header).unwrap();
    }
    let g1 = distinct_sib(mine(gen, 1_500_064_111, 1), l1.block_hash());
    hub.ensure_header(&g1.header).unwrap();

    let mut st =
        rbitcoin_ibd::ibd::state::IbdWorkState::new(Vec::new(), hub.tip_hash(), hub.tip_height());
    st.header_walk.tip.hash = Some(w5.block_hash());
    st.header_walk.tip.height = 5;
    let shared = Arc::new(Mutex::new(st));
    let hub = Arc::new(hub);
    *HOOK_HUB.lock().unwrap_or_else(|err| err.into_inner()) = Some(Arc::clone(&hub));
    *HOOK_SHARED.lock().unwrap_or_else(|err| err.into_inner()) = Some(Arc::clone(&shared));
    *HOOK_G1.lock().unwrap_or_else(|err| err.into_inner()) = Some(g1.block_hash());
    arm_after_header_rewind_prepare_for_test(Some(commit_deeper_fork_after_prepare));
    rbitcoin_ibd::ibd::settle_disconnected_heavier_for_test(&shared, &hub);

    assert!(
        HOOK_RAN.load(Ordering::Acquire),
        "the header-thread commit ran while the coordinator rewind was prepared"
    );
    assert_eq!(
        hub.tip_height(),
        Some(0),
        "the later rewind to genesis stays the tip"
    );
    assert_eq!(
        hub.query.lookup_taken_hi(),
        Some(0),
        "the prepared L1 commit must not move lookup_taken_hi back up"
    );
    let st = shared.lock().unwrap_or_else(|err| err.into_inner());
    assert!(
        !st.ordered.contains(&w2.block_hash()),
        "losing hashes from the prepared path stay off ordered"
    );
    assert_eq!(st.height_to_hash.get(&1), Some(&g1.block_hash()));
    let _ = std::fs::remove_dir_all(dir);
}

/// A real block whose prev is not a stored header fails the live confirm
/// load as BadPrev. That reject is soft reget: the tip stays, the slot is
/// evicted, and the hash is not blacklisted or fetched again.
#[test]
fn unknown_prev_header_is_soft_reget_not_a_rewind() {
    use rbitcoin_ibd::ibd::confirm::ConfirmRejectClass;
    use rbitcoin_ibd::ibd::events::apply_confirm_reject;

    let (dir, hub) = tmp_hub();
    hub.ensure_genesis().unwrap();
    let gen = hub.tip_hash().unwrap();
    let tip = mine(gen, 1_500_080_100, 1);
    hub.accept_block(tip).unwrap();
    assert_eq!(hub.tip_height(), Some(1));

    let unknown = BlockHash::from_byte_array([0x9e; 32]);
    assert!(
        hub.query
            .get_header_by_hash(&unknown.to_byte_array())
            .unwrap()
            .is_none(),
        "prev must not be a stored header"
    );
    let orphan = mine(unknown, 1_500_080_200, 2);
    let err = match hub.confirm_wire_load_phase(&[(Height(2), orphan.clone())]) {
        Err(e) => e,
        Ok(_) => panic!("unknown prev must fail confirm load"),
    };
    assert!(
        matches!(err, NetError::BadPrev),
        "live load maps ConsensusError::BadPrev, got {err}"
    );
    let msg = err.to_string();
    let class = ConfirmRejectClass::from_net(&err);
    assert_eq!(class, ConfirmRejectClass::SoftWire, "{msg}");
    assert!(is_bad_prev_err(&msg), "{msg}");

    let mut st =
        rbitcoin_ibd::ibd::state::IbdWorkState::new(Vec::new(), hub.tip_hash(), hub.tip_height());
    st.headers_done = true;
    st.record_height(orphan.block_hash(), 2);
    apply_confirm_reject(
        &mut st,
        2,
        orphan.block_hash(),
        class,
        &msg,
        Some(hub.query.as_ref()),
        Some(&hub),
        1,
        None,
        None,
        rbitcoin_ibd::ibd::confirm::RejectParent::default(),
    );
    assert_eq!(hub.tip_height(), Some(1), "unknown prev must not rewind");
    assert!(!st.headers_done, "soft reget clears the header latch");
    assert!(
        !st.height_to_hash.contains_key(&2),
        "the rejected slot is evicted"
    );
    assert!(
        !st.reorg
            .invalid
            .contains(orphan.block_hash().to_byte_array()),
        "soft reget does not blacklist"
    );
    assert!(
        !st.reorg.need_getdata().contains(&orphan.block_hash()),
        "BadPrev does not re-get the same hash"
    );
    let _ = std::fs::remove_dir_all(dir);
}

/// Linear headers ahead of tip are a normal extension, not a fork search.
/// (CI: two_node IBD hung at tip=1 after consider treated h=8 as disconnected.)
#[test]
fn linear_ahead_headers_are_not_a_disconnected_fork() {
    let (dir, hub) = tmp_hub();
    hub.ensure_genesis().unwrap();
    let gen = hub.tip_hash().unwrap();
    let b1 = mine(gen, 1_500_070_100, 1);
    hub.accept_block(b1.clone()).unwrap();
    let mut prev = b1.block_hash();
    let mut time = 1_500_070_200u32;
    let mut ahead = Vec::new();
    for h in 2..=8u32 {
        let b = mine(prev, time, h);
        hub.ensure_header(&b.header).unwrap();
        prev = b.block_hash();
        time += 100;
        ahead.push((h, prev));
    }
    let last = ahead.last().unwrap().1;
    assert!(
        connecting_hashes_heavier_disconnected_n(&hub, last, ANCESTOR_WALK_MAX)
            .unwrap()
            .is_none(),
        "far header on the same chain as tip is not a disconnected fork"
    );
    let mut st =
        rbitcoin_ibd::ibd::state::IbdWorkState::new(Vec::new(), hub.tip_hash(), hub.tip_height());
    for (h, hash) in &ahead {
        st.record_height(*hash, *h);
    }
    st.max_ordered_height = 8;
    let shared = Mutex::new(st);
    let rewound = consider_disconnected_heavier(&shared, &hub).unwrap();
    let st = shared.into_inner().unwrap_or_else(|err| err.into_inner());
    assert!(
        !rewound,
        "linear work-path must not register connecting search; need={:?}",
        st.reorg.need_getdata()
    );
    assert!(st.reorg.need_getdata().is_empty());
    let _ = std::fs::remove_dir_all(dir);
}

/// Early IBD: headers sit far above a low tip. The ancestor walk hits its
/// cap on a header-only mid, so join is not a confirmed LCA. That is still
/// a linear extension, not a connecting search.
///
/// Production walk is [`ancestor_walk_max`]; a smaller cap plus ~20 headers hits the same
/// branch without mining a mainnet-scale horizon.
#[test]
fn capped_walk_on_linear_headers_is_not_a_disconnected_fork() {
    let (dir, hub) = tmp_hub();
    hub.ensure_genesis().unwrap();
    let gen = hub.tip_hash().unwrap();
    let b1 = mine(gen, 1_500_080_100, 1);
    hub.accept_block(b1.clone()).unwrap();
    let mut prev = b1.block_hash();
    let mut time = 1_500_080_200u32;
    let mut last = prev;
    for h in 2..=20u32 {
        let b = mine(prev, time, h);
        hub.ensure_header(&b.header).unwrap();
        prev = b.block_hash();
        last = prev;
        time += 100;
    }
    let path = connecting_hashes_heavier_disconnected_n(&hub, last, 8).unwrap();
    assert!(
        path.is_none(),
        "capped walk whose join is header-only must not start connecting search; got {path:?}"
    );
    let _ = std::fs::remove_dir_all(dir);
}

/// Tip on a 2-block loser; a 3-header heavier fork from genesis is stored as
/// headers only. Its tip w3 sits at tip+1.
fn fork_three_under_a_two_block_tip(hub: &ChainHub, time0: u32) -> [BlockHash; 3] {
    hub.ensure_genesis().unwrap();
    let gen = hub.tip_hash().unwrap();
    let l1 = mine(gen, time0, 1);
    hub.accept_block(l1.clone()).unwrap();
    let l2 = mine(l1.block_hash(), time0 + 100, 2);
    hub.accept_block(l2).unwrap();
    let w1 = distinct_sib(mine(gen, time0 + 1, 1), l1.block_hash());
    let w2 = mine(w1.block_hash(), time0 + 101, 2);
    let w3 = mine(w2.block_hash(), time0 + 201, 3);
    for w in [&w1, &w2, &w3] {
        hub.ensure_header(&w.header).unwrap();
    }
    [w1.block_hash(), w2.block_hash(), w3.block_hash()]
}

/// A tip+1 candidate whose LCA is `d` blocks under the tip needs a walk of
/// `d + 1`. One header short, the fork is not selected.
#[test]
fn selection_walk_reaches_an_lca_one_past_its_depth() {
    let (dir, hub) = tmp_hub();
    let fork = fork_three_under_a_two_block_tip(&hub, 1_500_093_100);
    let none = std::collections::HashSet::new();
    let mut cap = || REWIND_MAX_DEPTH;
    assert_eq!(
        select_heavier_path(&hub, &[fork[2]], &none, 2, &mut cap, Pick::First).unwrap(),
        None,
        "a walk of two ends on a header-only row"
    );
    assert_eq!(
        select_heavier_path(&hub, &[fork[2]], &none, 3, &mut cap, Pick::First).unwrap(),
        Some((fork.to_vec(), REWIND_MAX_DEPTH))
    );
    let _ = std::fs::remove_dir_all(dir);
}

/// Each rewind cap in force has a selection walk that reaches its deepest
/// LCA from tip+1. A 10,000 walk under the 65,536 unproved cap dropped a
/// heavier fork the rewind would take. The proved walk is no wider than
/// [`ANCESTOR_WALK_MAX`], as before.
#[test]
fn selection_walk_covers_every_rewind_cap() {
    for cap in [
        REWIND_MAX_DEPTH,
        rbitcoin_ibd::ibd::header_walk::UNPROVEN_LEAD_BLOCKS,
    ] {
        assert!(
            ancestor_walk_max(cap) > cap as usize,
            "cap {cap} walk {}",
            ancestor_walk_max(cap)
        );
    }
    assert_eq!(ancestor_walk_max(REWIND_MAX_DEPTH), ANCESTOR_WALK_MAX);
}

/// A heavier fork deeper than the rewind cap is passed over, not returned.
/// The later candidate the rewind takes is selected, and the rewind under
/// that same cap disconnects to it.
#[test]
fn a_fork_deeper_than_the_cap_does_not_end_selection() {
    let (dir, hub) = tmp_hub();
    hub.ensure_genesis().unwrap();
    let gen = hub.tip_hash().unwrap();
    let t = 1_500_095_100;
    let l1 = mine(gen, t, 1);
    hub.accept_block(l1.clone()).unwrap();
    let l2 = mine(l1.block_hash(), t + 100, 2);
    hub.accept_block(l2.clone()).unwrap();
    hub.accept_block(mine(l2.block_hash(), t + 200, 3)).unwrap();
    let side = |from: BlockHash, avoid: BlockHash, h0: u32, n: u32, dt: u32| {
        let mut out = Vec::new();
        let mut prev = from;
        for h in h0..h0 + n {
            let b = distinct_sib(mine(prev, t + dt + 100 * (h - 1), h), avoid);
            hub.ensure_header(&b.header).unwrap();
            prev = b.block_hash();
            out.push(prev);
        }
        out
    };
    let deep = side(gen, l1.block_hash(), 1, 4, 1);
    let shallow = side(l1.block_hash(), l2.block_hash(), 2, 3, 2);
    let none = std::collections::HashSet::new();
    assert_eq!(
        select_heavier_path(
            &hub,
            &[deep[3], shallow[2]],
            &none,
            10,
            &mut || 2,
            Pick::First
        )
        .unwrap(),
        Some((shallow.clone(), 2)),
        "the fork 3 under the tip is past a cap of 2"
    );
    let prep = prepare_header_rewind(&hub, &shallow, 2)
        .unwrap()
        .expect("the rewind takes the path selection returned");
    assert_eq!(prep.lca_h, 1);
    assert_eq!(hub.tip_height(), Some(1));
    let _ = std::fs::remove_dir_all(dir);
}

/// A selection under `shared` stops at its walk. A heavier fork past that
/// walk is deferred, and the coordinator's off-lock turn selects it after
/// the explore tip that named it is cleared, as a missed exploration does.
#[test]
fn a_fork_past_the_under_lock_walk_is_selected_off_lock() {
    let (dir, hub) = tmp_hub();
    let fork = fork_three_under_a_two_block_tip(&hub, 1_500_096_100);
    let mut st =
        rbitcoin_ibd::ibd::state::IbdWorkState::new(Vec::new(), hub.tip_hash(), hub.tip_height());
    st.reorg
        .register_explore(std::iter::empty::<BlockHash>(), Some(fork[2]));
    assert!(!maybe_rewind_to_best_work_n(&mut st, &hub, 2).unwrap());
    assert_eq!(
        hub.tip_height(),
        Some(2),
        "a walk of two does not reach the LCA"
    );
    st.reorg.clear_explore();
    let shared = Mutex::new(st);
    rbitcoin_ibd::ibd::settle_disconnected_heavier_for_test(&shared, &hub);
    assert_eq!(
        hub.tip_height(),
        Some(0),
        "the off-lock turn rewinds to the fork"
    );
    let st = shared.into_inner().unwrap_or_else(|err| err.into_inner());
    assert_eq!(st.height_to_hash.get(&3), Some(&fork[2]));
    assert!(
        st.reorg.deferred.is_empty(),
        "a deferred candidate is retried once"
    );
    let _ = std::fs::remove_dir_all(dir);
}

/// A confirmed chain of `n` blocks above genesis, oldest first from genesis.
fn confirmed_chain(hub: &ChainHub, n: u32, t: u32) -> Vec<BlockHash> {
    hub.ensure_genesis().unwrap();
    let mut chain = vec![hub.tip_hash().unwrap()];
    for h in 1..=n {
        let b = mine(chain[h as usize - 1], t + 100 * (h - 1), h);
        hub.accept_block(b.clone()).unwrap();
        chain.push(b.block_hash());
    }
    chain
}

/// `n` stored headers forking from `chain[from]`, oldest first.
fn side_headers(hub: &ChainHub, chain: &[BlockHash], from: u32, n: u32, t: u32) -> Vec<BlockHash> {
    let mut out = Vec::new();
    let mut prev = chain[from as usize];
    for h in from + 1..=from + n {
        let mut b = mine(prev, t + 100 * (h - 1), h);
        if let Some(sib) = chain.get(h as usize) {
            b = distinct_sib(b, *sib);
        }
        hub.ensure_header(&b.header).unwrap();
        prev = b.block_hash();
        out.push(prev);
    }
    out
}

/// One pass under `shared` with a walk of 3 sees the 2-header `shallow`
/// fork but not `deep`, then a missed exploration clears both tips. The
/// coordinator's next off-lock turn must end on the fork with the most
/// work against the confirmed chain the pass saw, and leave nothing to
/// retry.
fn hand_off_both(
    hub: &ChainHub,
    shallow: BlockHash,
    deep: BlockHash,
) -> rbitcoin_ibd::ibd::state::IbdWorkState {
    let mut st =
        rbitcoin_ibd::ibd::state::IbdWorkState::new(Vec::new(), hub.tip_hash(), hub.tip_height());
    for tip in [shallow, deep] {
        st.reorg
            .register_explore(std::iter::empty::<BlockHash>(), Some(tip));
    }
    let _ = maybe_rewind_to_best_work_n(&mut st, hub, 3).unwrap();
    st.reorg.clear_explore();
    let shared = Mutex::new(st);
    rbitcoin_ibd::ibd::settle_disconnected_heavier_for_test(&shared, hub);
    let st = shared.into_inner().unwrap_or_else(|err| err.into_inner());
    assert!(st.reorg.deferred.is_empty(), "nothing is left to retry");
    st
}

/// The deeper fork has the most work and an older ancestor.
#[test]
fn a_heavier_fork_past_the_under_lock_walk_wins_the_off_lock_pass() {
    let (dir, hub) = tmp_hub();
    let t = 1_500_099_100;
    let chain = confirmed_chain(&hub, 2, t);
    let shallow = side_headers(&hub, &chain, 1, 2, t + 2);
    let deep = side_headers(&hub, &chain, 0, 5, t + 1);
    let st = hand_off_both(&hub, shallow[1], deep[4]);
    assert_eq!(hub.tip_height(), Some(0));
    assert_eq!(st.height_to_hash.get(&5), Some(&deep[4]));
    let _ = std::fs::remove_dir_all(dir);
}

/// Both forks leave the same confirmed block. Committing the shallow one
/// first would make that block the tip, and the deeper fork would then read
/// as a linear extension of it.
#[test]
fn forks_from_one_ancestor_rewind_to_the_heavier() {
    let (dir, hub) = tmp_hub();
    let t = 1_500_099_300;
    let chain = confirmed_chain(&hub, 2, t);
    let shallow = side_headers(&hub, &chain, 1, 2, t + 2);
    let deep = side_headers(&hub, &chain, 1, 4, t + 1);
    let st = hand_off_both(&hub, shallow[1], deep[3]);
    assert_eq!(hub.tip_height(), Some(1));
    assert_eq!(st.height_to_hash.get(&5), Some(&deep[3]));
    let _ = std::fs::remove_dir_all(dir);
}

/// The deeper fork only ties the confirmed chain, so it is no rewind. It
/// would beat the tip a committed shallow rewind leaves behind, and must not
/// replace the shallow fork that way.
#[test]
fn a_fork_lighter_than_the_confirmed_chain_never_replaces_a_heavier_one() {
    let (dir, hub) = tmp_hub();
    let t = 1_500_099_500;
    let chain = confirmed_chain(&hub, 4, t);
    let shallow = side_headers(&hub, &chain, 3, 2, t + 2);
    let deep = side_headers(&hub, &chain, 0, 4, t + 1);
    let st = hand_off_both(&hub, shallow[1], deep[3]);
    assert_eq!(hub.tip_height(), Some(3));
    assert_eq!(st.height_to_hash.get(&5), Some(&shallow[1]));
    let _ = std::fs::remove_dir_all(dir);
}

/// A header batch names a third fork while two deferred forks wait for the
/// coordinator. The header thread's pass ranks the deferred forks with it,
/// so it rewinds to the heaviest, not to the one it alone can see.
#[test]
fn the_header_thread_ranks_deferred_forks_too() {
    let (dir, hub) = tmp_hub();
    let t = 1_500_099_700;
    let chain = confirmed_chain(&hub, 2, t);
    let named = side_headers(&hub, &chain, 1, 2, t + 2);
    let deferred = side_headers(&hub, &chain, 1, 4, t + 1);
    let mut st =
        rbitcoin_ibd::ibd::state::IbdWorkState::new(Vec::new(), hub.tip_hash(), hub.tip_height());
    st.reorg.defer(&[deferred[3]]);
    st.reorg
        .register_explore(std::iter::empty::<BlockHash>(), Some(named[1]));
    let shared = Mutex::new(st);
    assert!(rewind_off_coord_lock(&shared, &hub).unwrap());
    assert_eq!(hub.tip_height(), Some(1));
    let st = shared.into_inner().unwrap_or_else(|err| err.into_inner());
    assert_eq!(st.height_to_hash.get(&5), Some(&deferred[3]));
    assert!(st.reorg.deferred.is_empty());
    let _ = std::fs::remove_dir_all(dir);
}

/// A selected fork whose rewind cannot run while confirm holds the tip
/// commit is deferred again, by the pass under `shared` and by the
/// coordinator's off-lock turn. Once the commit is free the next turn
/// rewinds to it.
#[test]
fn a_fork_refused_by_a_busy_tip_commit_is_retried() {
    let (dir, hub) = tmp_hub();
    let fork = fork_three_under_a_two_block_tip(&hub, 1_500_098_100);
    let mut st =
        rbitcoin_ibd::ibd::state::IbdWorkState::new(Vec::new(), hub.tip_hash(), hub.tip_height());
    st.reorg
        .register_explore(std::iter::empty::<BlockHash>(), Some(fork[2]));
    let shared = {
        let _commit = hub.query.lock_tip_commit().unwrap();
        assert!(!maybe_rewind_to_best_work(&mut st, &hub).unwrap());
        st.reorg.clear_explore();
        assert_eq!(st.reorg.deferred, vec![fork[2]]);
        let shared = Mutex::new(st);
        rbitcoin_ibd::ibd::settle_disconnected_heavier_for_test(&shared, &hub);
        assert_eq!(hub.tip_height(), Some(2), "confirm still holds the tip");
        shared
    };
    rbitcoin_ibd::ibd::settle_disconnected_heavier_for_test(&shared, &hub);
    assert_eq!(
        hub.tip_height(),
        Some(0),
        "the next turn rewinds to the fork"
    );
    let _ = std::fs::remove_dir_all(dir);
}

/// One pass defers a full explore list and the competing tip+1 without
/// evicting its own first candidate.
#[test]
fn deferral_keeps_a_whole_pass() {
    let mut st = IbdReorgState::new();
    let cands: Vec<BlockHash> = (0..=IbdReorgState::EXPLORE_CAP as u32)
        .map(|i| {
            let mut raw = [0u8; 32];
            raw[0..4].copy_from_slice(&i.to_le_bytes());
            BlockHash::from_byte_array(raw)
        })
        .collect();
    st.defer(&cands);
    assert_eq!(st.deferred, cands);
}

/// A hash deferred again is the newest entry, so a full list evicts a hash
/// no later pass offered, not the fork the last pass selected.
#[test]
fn deferring_a_queued_hash_again_keeps_it() {
    let hash = |i: u32| {
        let mut raw = [0u8; 32];
        raw[0..4].copy_from_slice(&i.to_le_bytes());
        BlockHash::from_byte_array(raw)
    };
    let mut st = IbdReorgState::new();
    let full: Vec<BlockHash> = (0..IbdReorgState::DEFER_CAP as u32).map(hash).collect();
    st.defer(&full);
    let selected = full[0];
    let late = hash(IbdReorgState::DEFER_CAP as u32);
    st.defer(&[selected, late]);
    assert_eq!(st.deferred.len(), IbdReorgState::DEFER_CAP);
    assert!(
        st.deferred.contains(&selected),
        "the re-deferred fork stays"
    );
    assert!(st.deferred.contains(&late));
    assert!(
        !st.deferred.contains(&full[1]),
        "the oldest untouched hash goes"
    );
}

/// A header-thread pass with no candidates takes no gate and reads no
/// chain work, so a cold chain-work prefix stays cold.
#[test]
fn an_empty_off_lock_pass_reads_no_chain_work() {
    let (dir, hub) = tmp_hub();
    hub.ensure_genesis().unwrap();
    hub.confirm_empty_regtest(3).unwrap();
    hub.test_clear_chain_work_prefix();
    let mut st =
        rbitcoin_ibd::ibd::state::IbdWorkState::new(Vec::new(), hub.tip_hash(), hub.tip_height());
    st.header_walk.origin = true;
    let shared = Mutex::new(st);
    assert!(!rewind_off_coord_lock(&shared, &hub).unwrap());
    assert_eq!(hub.test_chain_work_prefix_len(), 0);
    let _ = std::fs::remove_dir_all(dir);
}

/// Under the floor the rewind cap is 65,536, but a selection under `shared`
/// walks no further than [`ANCESTOR_WALK_MAX`]. The coordinator's off-lock
/// turn is the one that walks as far as the cap.
#[test]
fn the_under_lock_walk_stays_at_ancestor_walk_max() {
    use rbitcoin_ibd::ibd::header_walk::{rewind_depth_cap, UNPROVEN_LEAD_BLOCKS};
    let (dir, mut hub) = tmp_hub();
    let fork = fork_three_under_a_two_block_tip(&hub, 1_500_097_100);
    hub.milestone = floor_just_above(hub.chain_work().unwrap());
    let mut st =
        rbitcoin_ibd::ibd::state::IbdWorkState::new(Vec::new(), hub.tip_hash(), hub.tip_height());
    st.header_walk.origin = true;
    assert_eq!(rewind_depth_cap(&st, &hub), UNPROVEN_LEAD_BLOCKS);
    st.reorg
        .register_explore(std::iter::empty::<BlockHash>(), Some(fork[2]));
    reset_longest_walk();
    assert!(maybe_rewind_to_best_work(&mut st, &hub).unwrap());
    assert_eq!(longest_walk(), ANCESTOR_WALK_MAX);
    st.reorg.defer(&[fork[2]]);
    let shared = Mutex::new(st);
    reset_longest_walk();
    rbitcoin_ibd::ibd::settle_disconnected_heavier_for_test(&shared, &hub);
    assert_eq!(longest_walk(), ancestor_walk_max(UNPROVEN_LEAD_BLOCKS));
    let _ = std::fs::remove_dir_all(dir);
}

/// An unplanted walk tip as far above the confirmed tip as the widest walk
/// is not walked: that walk ends before any connected header. One height
/// lower it is. The recorded height alone decides, so a short fork stands in
/// for a far one.
#[test]
fn a_walk_tip_out_of_walk_reach_is_not_a_candidate() {
    let (dir, hub) = tmp_hub();
    let fork = fork_three_under_a_two_block_tip(&hub, 1_500_094_100);
    let mut st =
        rbitcoin_ibd::ibd::state::IbdWorkState::new(Vec::new(), hub.tip_hash(), hub.tip_height());
    let widest = ancestor_walk_max(rbitcoin_ibd::ibd::header_walk::UNPROVEN_LEAD_BLOCKS);
    st.header_walk.tip.hash = Some(fork[2]);
    st.header_walk.tip.height = 2 + widest as u32;
    let shared = Mutex::new(st);
    assert!(!consider_disconnected_heavier(&shared, &hub).unwrap());
    assert_eq!(hub.tip_height(), Some(2));
    shared
        .lock()
        .unwrap_or_else(|err| err.into_inner())
        .header_walk
        .tip
        .height -= 1;
    assert!(consider_disconnected_heavier(&shared, &hub).unwrap());
    assert_eq!(hub.tip_height(), Some(0), "rewound to the fork");
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn connecting_search_skips_linear_tip_plus_one() {
    let (dir, hub) = tmp_hub();
    hub.ensure_genesis().unwrap();
    let gen = hub.tip_hash().unwrap();
    let mut prev = gen;
    let mut time = 1_500_090_100u32;
    let b1 = mine(prev, time, 1);
    hub.accept_block(b1.clone()).unwrap();
    prev = b1.block_hash();
    time += 100;
    let b2 = mine(prev, time, 2);
    hub.accept_block(b2.clone()).unwrap();
    prev = b2.block_hash();
    time += 100;
    let mut st =
        rbitcoin_ibd::ibd::state::IbdWorkState::new(Vec::new(), hub.tip_hash(), hub.tip_height());
    let mut last = prev;
    for h in 3..=20u32 {
        let b = mine(prev, time, h);
        hub.ensure_header(&b.header).unwrap();
        prev = b.block_hash();
        last = prev;
        time += 100;
        st.record_height(prev, h);
    }
    st.max_ordered_height = 20;
    assert!(
        connecting_search_candidates(&st, &hub).unwrap().is_empty(),
        "linear tip+1 must not be a connecting-search candidate (avoids a long max_ordered walk)"
    );
    let shared = Mutex::new(st);
    let rewound = consider_disconnected_heavier(&shared, &hub).unwrap();
    let st = shared.into_inner().unwrap_or_else(|err| err.into_inner());
    assert!(
        !rewound,
        "linear tip+1 + far headers must not register; last={last} need={:?}",
        st.reorg.need_getdata()
    );
    assert!(st.reorg.need_getdata().is_empty());
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn connecting_search_uses_recorded_prev_not_the_header_store() {
    let (dir, hub) = tmp_hub();
    hub.ensure_genesis().unwrap();
    let mut st =
        rbitcoin_ibd::ibd::state::IbdWorkState::new(Vec::new(), hub.tip_hash(), hub.tip_height());
    let child = BlockHash::from_byte_array([0xab; 32]);
    st.record_height(child, 1);
    st.note_path_prev(child, BlockHash::from_byte_array([0xcd; 32]));
    assert!(
        hub.query
            .get_header_by_hash(&child.to_byte_array())
            .unwrap()
            .is_none(),
        "this hash was never stored"
    );
    assert_eq!(
        connecting_search_candidates(&st, &hub).unwrap(),
        vec![child],
        "a recorded parent that is not the tip is a competing child"
    );
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn connecting_search_candidates_is_tip_plus_one_when_competing() {
    let (dir, hub) = tmp_hub();
    hub.ensure_genesis().unwrap();
    let gen = hub.tip_hash().unwrap();
    let l1 = mine(gen, 1_500_091_100, 1);
    hub.accept_block(l1.clone()).unwrap();
    let l2 = mine(l1.block_hash(), 1_500_091_200, 2);
    hub.accept_block(l2.clone()).unwrap();
    let w1 = distinct_sib(mine(gen, 1_500_091_101, 1), l1.block_hash());
    hub.ensure_header(&w1.header).unwrap();
    let w2 = mine(w1.block_hash(), 1_500_091_201, 2);
    hub.ensure_header(&w2.header).unwrap();
    let w3 = mine(w2.block_hash(), 1_500_091_301, 3);
    hub.ensure_header(&w3.header).unwrap();
    let w4 = mine(w3.block_hash(), 1_500_091_401, 4);
    hub.ensure_header(&w4.header).unwrap();
    let mut st =
        rbitcoin_ibd::ibd::state::IbdWorkState::new(Vec::new(), hub.tip_hash(), hub.tip_height());
    st.record_height(w3.block_hash(), 3);
    st.note_path_prev(w3.block_hash(), w2.block_hash());
    st.record_height(w4.block_hash(), 4);
    st.max_ordered_height = 4;
    assert_eq!(
        connecting_search_candidates(&st, &hub).unwrap(),
        vec![w3.block_hash()],
        "competing tip+1 is the only candidate; far header is the same fork"
    );
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn connecting_search_skips_when_tip_plus_one_is_own_child() {
    let (dir, hub) = tmp_hub();
    hub.ensure_genesis().unwrap();
    let gen = hub.tip_hash().unwrap();
    let l1 = mine(gen, 1_500_092_100, 1);
    hub.accept_block(l1.clone()).unwrap();
    let l2 = mine(l1.block_hash(), 1_500_092_200, 2);
    hub.accept_block(l2.clone()).unwrap();
    let l3 = mine(l2.block_hash(), 1_500_092_300, 3);
    hub.ensure_header(&l3.header).unwrap();
    let w1 = distinct_sib(mine(gen, 1_500_092_101, 1), l1.block_hash());
    hub.ensure_header(&w1.header).unwrap();
    let w2 = mine(w1.block_hash(), 1_500_092_201, 2);
    hub.ensure_header(&w2.header).unwrap();
    let w3 = mine(w2.block_hash(), 1_500_092_301, 3);
    hub.ensure_header(&w3.header).unwrap();
    let w4 = mine(w3.block_hash(), 1_500_092_401, 4);
    hub.ensure_header(&w4.header).unwrap();
    let mut st =
        rbitcoin_ibd::ibd::state::IbdWorkState::new(Vec::new(), hub.tip_hash(), hub.tip_height());
    st.record_height(l3.block_hash(), 3);
    st.record_height(w4.block_hash(), 4);
    st.max_ordered_height = 4;
    assert!(
        connecting_search_candidates(&st, &hub).unwrap().is_empty(),
        "own-child tip+1 is linear; heavier far header is not a consider candidate"
    );
    let shared = Mutex::new(st);
    assert!(
        !consider_disconnected_heavier(&shared, &hub).unwrap(),
        "accepted miss: heavier fork only at max_ordered while tip+1 is our child"
    );
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn explore_need_and_tips_stay_capped() {
    let mut st = IbdReorgState::new();
    let mut last = BlockHash::from_byte_array([1u8; 32]);
    for i in 0..80u32 {
        let mut raw = [0u8; 32];
        raw[0..4].copy_from_slice(&i.to_le_bytes());
        last = BlockHash::from_byte_array(raw);
        st.register_explore([last], Some(last));
    }
    assert!(
        st.explore_need_hashes().len() <= 64,
        "explore_need grew to {}",
        st.explore_need_hashes().len()
    );
    assert!(
        st.explore_tips().len() <= 64,
        "explore_tips grew to {}",
        st.explore_tips().len()
    );
    assert!(st.explore_need_hashes().contains(&last));
    assert!(st.explore_tips().contains(&last));
}

fn pin_hash(n: u8) -> BlockHash {
    let mut raw = [0u8; 32];
    raw[0] = n;
    BlockHash::from_byte_array(raw)
}

/// Equal tip and LCA is a plant. `>` to `>=` marks it rewound.
#[test]
fn reorg_pin_a_plant_at_the_tip_is_not_a_rewind() {
    let (dir, hub) = tmp_hub();
    hub.ensure_genesis().unwrap();
    let gen = hub.tip_hash().unwrap();
    let side = mine(gen, hub.tip_header().unwrap().time.saturating_add(1), 1);
    hub.ensure_header(&side.header).unwrap();
    let prep = prepare_header_rewind(&hub, &[side.block_hash()], 1)
        .unwrap()
        .expect("a child of the tip is a plant");
    assert!(!prep.rewound);
    assert_eq!(hub.tip_height(), Some(0));
    let _ = dir;
}

/// Depth equal to the cap still disconnects. `>` to `>=` refuses it.
#[test]
fn reorg_pin_a_rewind_at_the_depth_cap_still_disconnects() {
    let (dir, hub) = tmp_hub();
    hub.ensure_genesis().unwrap();
    let gen = hub.tip_hash().unwrap();
    hub.confirm_empty_regtest(1).unwrap();
    let tip = hub.tip_hash().unwrap();
    let side = distinct_sib(
        mine(gen, hub.tip_header().unwrap().time.saturating_add(1), 1),
        tip,
    );
    hub.ensure_header(&side.header).unwrap();
    let prep = prepare_header_rewind(&hub, &[side.block_hash()], 1)
        .unwrap()
        .expect("depth equal to the cap still rewinds");
    assert!(prep.rewound);
    assert_eq!(hub.tip_height(), Some(0));
    let _ = dir;
}

/// The off-lock rewind takes one mutex. A leaked mutex each call would not.
#[test]
fn reorg_pin_the_off_lock_rewind_gate_is_one_mutex() {
    let first = rewind_exclusion();
    let second = rewind_exclusion();
    assert!(std::ptr::eq(first, second));
}

/// Invalid, not rejected. `||` to `&&` keeps the row as a competing tip.
#[test]
fn reorg_pin_an_invalid_row_is_not_a_competing_tip() {
    let (dir, hub) = tmp_hub();
    hub.ensure_genesis().unwrap();
    let tip = hub.tip_hash().unwrap();
    let mut st = rbitcoin_ibd::ibd::state::IbdWorkState::new(Vec::new(), Some(tip), Some(0));
    let bad = pin_hash(0xab);
    st.hash_height.insert(bad, 0);
    st.reorg.invalid.mark(bad.to_byte_array());
    let tips = competing_valid_header_tips(&st, &hub);
    assert!(tips.is_empty(), "{tips:?}");
    let _ = dir;
}

/// The active tip is not a start. `==` to `!=` keeps the tip and drops the sibling.
/// Delete `!` on the duplicate check pushes nothing.
#[test]
fn reorg_pin_the_active_tip_is_not_a_competing_start() {
    let (dir, hub) = tmp_hub();
    hub.ensure_genesis().unwrap();
    let tip = hub.tip_hash().unwrap();
    let mut st = rbitcoin_ibd::ibd::state::IbdWorkState::new(Vec::new(), Some(tip), Some(0));
    let sib = pin_hash(0x11);
    st.hash_height.insert(sib, 0);
    st.reorg.invalid.mark(pin_hash(0xee).to_byte_array());
    let tips = competing_valid_header_tips(&st, &hub);
    assert_eq!(tips, vec![sib]);
    let _ = dir;
}

/// One hash at a height is not a fork. Two are. `>` to `>=`, `==`, or `<`
/// changes that, and delete `!` on the tip list pushes nothing.
#[test]
fn reorg_pin_only_a_real_fork_is_a_competing_start() {
    let (dir, hub) = tmp_hub();
    hub.ensure_genesis().unwrap();
    let tip = hub.tip_hash().unwrap();
    let mut st = rbitcoin_ibd::ibd::state::IbdWorkState::new(Vec::new(), Some(tip), Some(0));
    let a = pin_hash(1);
    let b = pin_hash(2);
    let c = pin_hash(3);
    st.hash_height.insert(a, 2);
    st.hash_height.insert(b, 2);
    st.hash_height.insert(c, 5);
    st.reorg.invalid.mark(pin_hash(9).to_byte_array());
    let tips = competing_valid_header_tips(&st, &hub);
    assert!(tips.contains(&a) && tips.contains(&b), "{tips:?}");
    assert!(!tips.contains(&c), "{tips:?}");
    let _ = dir;
}

/// An invalid child is not the extension. `||` to `&&` follows it.
#[test]
fn reorg_pin_an_invalid_child_does_not_extend_the_tip() {
    let (dir, hub) = tmp_hub();
    hub.ensure_genesis().unwrap();
    let gen = hub.tip_hash().unwrap();
    let child = mine(gen, hub.tip_header().unwrap().time.saturating_add(1), 1);
    hub.ensure_header(&child.header).unwrap();
    let mut st = rbitcoin_ibd::ibd::state::IbdWorkState::new(Vec::new(), Some(gen), Some(0));
    st.reorg.invalid.mark(child.block_hash().to_byte_array());
    let mut by_h = std::collections::HashMap::new();
    by_h.insert(1u32, vec![child.block_hash()]);
    let got = extend_valid_header_tip(&st, &hub, &by_h, gen, 0);
    assert_eq!(got, gen);
    let _ = dir;
}

/// A non-empty download queue hides the walk tip. `||` to `&&` requires both
/// a queue and a tip+1 slot.
#[test]
fn reorg_pin_a_planted_queue_hides_the_walk_tip() {
    let (dir, hub) = tmp_hub();
    hub.ensure_genesis().unwrap();
    let mut st =
        rbitcoin_ibd::ibd::state::IbdWorkState::new(Vec::new(), hub.tip_hash(), hub.tip_height());
    st.ordered.push_back(pin_hash(4));
    st.header_walk.tip.hash = Some(pin_hash(7));
    st.header_walk.tip.height = 5;
    let (cands, _) = heavier_fork_candidates(&mut st, &hub).unwrap();
    assert!(cands.is_empty(), "{cands:?}");
    let _ = dir;
}
