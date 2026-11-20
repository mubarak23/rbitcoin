//! Mutable IBD work-path state (peers, ordered queue, body cache, inflight).
//!
//! ## Ordered path memory
//!
//! - `ordered` + `ordered_set` track headers after the local tip for getdata.
//! - Middle completions leave **ghost** deque entries (see [`super::assign_plan::remove_from_ordered`]);
//!   `Self::hygiene` compacts when the deque bloats.
//! - `hash_height` / `header_fks` / `path_prev` / `known_headers` hold header
//!   rows for the work path. `Self::deep_hygiene` drops rows that are not
//!   live, in flight, or the occupant of their height, and only when a map
//!   is past that keep set. `Self::hygiene` runs every second and does not
//!   visit the indexed header walk above the queue tail.

use super::assign_plan::compact_ordered;
use super::body::{BodyPresence, BodyPresenceSizes};
use super::reorg::IbdReorgState;
use bitcoin::BlockHash;
use rbitcoin_net::NetAddr;
use rbitcoin_primitives::Fk;
use std::collections::{HashMap, HashSet, VecDeque};
use std::time::Instant;

use super::peer_io::PeerSlot;

/// O(1) occupancy of [`IbdWorkState`] retain structures (for `ibd: sizes`).
#[derive(Clone, Copy, Debug, Default, serde::Serialize)]
pub struct WorkStructureSizes {
    pub ordered: usize,
    pub ordered_set: usize,
    pub hash_height: usize,
    pub height_to_hash: usize,
    pub header_fks: usize,
    pub known_headers: usize,
    pub inflight: usize,
    /// Sum of per-peer `in_flight` sets (may exceed unique `inflight` on tip races).
    pub peer_inflight: usize,
    pub addr_cooldown: usize,
    pub body: BodyPresenceSizes,
    /// Heap of the work maps, deques and body presence, from their
    /// `capacity()`. A lower bound: a hash map that removed keeps buckets
    /// `capacity()` no longer counts.
    pub map_bytes: u64,
}

/// One peer that has been sent getdata for this hash.
#[derive(Debug, Clone)]
struct Holder {
    peer: usize,
    asked_at: Instant,
    retired: bool,
}

/// Outstanding getdata for one block hash (one or more peers).
///
/// Near/far densify use a single peer. Tip-hole hashes race up to
/// [`super::TIP_HOLE_MAX_PEERS`] once the body queue is a quarter of the
/// confirm window or the configured assign-stop, or holds
/// [`rbitcoin_query::TIP_HOLE_MIN_AHEAD_BLOCKS`] bodies. Below that, the gap
/// is the frontier.
///
/// Getdata cannot be cancelled. A peer dropped from the race moves to
/// `retired`: it no longer counts as a racer, but it still holds the request,
/// so the hash stays requested. Except for the racerless re-ask after
/// [`super::assign::TIP_HOLE_RX_STALE`], a retired peer is not asked for it again.
/// Retired peers count toward the ask cap once a racer is live. A retired-only
/// set is not coverage: cover still places a racer when holders are already
/// at the cap. The holders share one list.
#[derive(Debug, Clone)]
pub struct InflightReq {
    /// First-ask order. A peer is in the list once.
    holders: Vec<Holder>,
    /// When this hash first entered global inflight (stale tip-hole re-get).
    pub started_at: Instant,
}

impl Default for InflightReq {
    fn default() -> Self {
        Self {
            holders: Vec::new(),
            started_at: Instant::now(),
        }
    }
}

impl InflightReq {
    pub fn new(peer: usize) -> Self {
        let mut r = Self::default();
        r.add_peer(peer);
        r
    }

    fn slot(&self, peer: usize) -> Option<&Holder> {
        self.holders.iter().find(|h| h.peer == peer)
    }

    fn slot_mut(&mut self, peer: usize) -> Option<&mut Holder> {
        self.holders.iter_mut().find(|h| h.peer == peer)
    }

    /// Racing owner.
    pub fn is_racing(&self, peer: usize) -> bool {
        self.slot(peer).is_some_and(|h| !h.retired)
    }

    /// Dropped from the race; still holding the getdata.
    pub fn is_retired(&self, peer: usize) -> bool {
        self.slot(peer).is_some_and(|h| h.retired)
    }

    /// Racing owners, in first-ask order.
    pub fn racing(&self) -> impl Iterator<Item = usize> + '_ {
        self.holders.iter().filter(|h| !h.retired).map(|h| h.peer)
    }

    /// Racing owners as a set. Tests compare membership; assign does not.
    #[doc(hidden)]
    pub fn racing_set(&self) -> HashSet<usize> {
        self.racing().collect()
    }

    /// Retired holders, in first-ask order.
    pub fn retired_ids(&self) -> impl Iterator<Item = usize> + '_ {
        self.holders.iter().filter(|h| h.retired).map(|h| h.peer)
    }

    /// Every peer already sent this getdata.
    pub fn holder_ids(&self) -> impl Iterator<Item = usize> + '_ {
        self.holders.iter().map(|h| h.peer)
    }

    pub fn retired_is_empty(&self) -> bool {
        self.holders.iter().all(|h| !h.retired)
    }

    /// Racing or retired: `peer` has been sent this getdata and not answered.
    pub fn holds(&self, peer: usize) -> bool {
        self.slot(peer).is_some()
    }

    /// Racing owners only.
    pub fn len(&self) -> usize {
        self.holders.iter().filter(|h| !h.retired).count()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Racing and retired: every peer already sent this getdata.
    pub fn holders(&self) -> usize {
        self.holders.len()
    }

    /// Returns true if `peer` was newly added as a racer.
    pub fn add_peer(&mut self, peer: usize) -> bool {
        if let Some(slot) = self.slot_mut(peer) {
            if slot.retired {
                slot.retired = false;
                slot.asked_at = Instant::now();
                return true;
            }
            return false;
        }
        self.holders.push(Holder {
            peer,
            asked_at: Instant::now(),
            retired: false,
        });
        true
    }

    /// When racing or retired owner `peer` was asked.
    pub fn owner_asked_at(&self, peer: usize) -> Option<Instant> {
        self.slot(peer).map(|h| h.asked_at)
    }

    /// Ask time of a peer that already holds this getdata.
    pub(crate) fn holder_asked_at(&self, peer: usize) -> Instant {
        self.slot(peer)
            .map(|h| h.asked_at)
            .expect("peer holds this getdata")
    }

    /// Test seam: age an existing holder's ask.
    #[doc(hidden)]
    pub fn set_asked_at(&mut self, peer: usize, at: Instant) {
        if let Some(slot) = self.slot_mut(peer) {
            slot.asked_at = at;
        }
    }

    /// Stop counting `peer` as a racer. It keeps the request and its ask time,
    /// so a racerless hash can re-ask it once the hold has aged out.
    pub fn retire_peer(&mut self, peer: usize) {
        if let Some(slot) = self.slot_mut(peer) {
            slot.retired = true;
        }
    }

    /// `peer` no longer holds the request (answered, notfound, or gone).
    /// Returns true if no holder remains (caller should drop the hash).
    pub fn remove_peer(&mut self, peer: usize) -> bool {
        if let Some(i) = self.holders.iter().position(|h| h.peer == peer) {
            self.holders.remove(i);
        }
        self.holders.is_empty()
    }
}

/// Confirmed-chain `getheaders` tail for one published tip.
///
/// Absent until the first catch-up ask. The next ask reuses `entries` while
/// both `height` and `hash` still match `published_tip`. The work-path
/// prefix is not stored here.
pub struct ConfirmedLocatorTail {
    pub height: Option<u32>,
    pub hash: Option<[u8; 32]>,
    /// Each locator hash with its height.
    pub entries: Vec<(u32, BlockHash)>,
}

/// Why the next confirm drain drops in-channel plans.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ConfirmQuiesce {
    #[default]
    /// Confirm is keeping up with the tip.
    Live,
    /// The header walk rewound. The confirmed tip did not move, so the lead
    /// stays reserved.
    Plan,
    /// The confirmed tip moved. Reservations above it go back to the lead.
    Refund,
}

/// Core mutable state for the IBD event loop.
pub struct IbdWorkState {
    pub slots: Vec<PeerSlot>,
    /// Unique hashes with outstanding getdata (1 peer normally; tip-hole races ≤4).
    pub inflight: HashMap<BlockHash, InflightReq>,
    /// Chain-order download path after local tip (front ≈ next to confirm).
    pub ordered: VecDeque<BlockHash>,
    pub ordered_set: HashSet<BlockHash>,
    pub hash_height: HashMap<BlockHash, u32>,
    /// Inverse of [`Self::hash_height`] for O(1) tip+1‥ confirm offers.
    ///
    /// Dense from the lowest occupant to the highest. Per-second hygiene
    /// drops slots at or below the queue tail without visiting the indexed
    /// header walk above that tail.
    pub height_to_hash: HeightIndex,
    /// Parent hash recorded when the header was admitted. The orchestration
    /// loop uses this instead of reading the header store.
    pub path_prev: HashMap<BlockHash, BlockHash>,
    pub known_headers: HashSet<BlockHash>,
    pub body: BodyPresence,
    /// header hash → Class A header fk (from getheaders; Block path skips store).
    pub header_fks: HashMap<BlockHash, Fk>,
    /// Best peer-advertised tip (version.start_height + learned header heights).
    pub max_peer_height: u32,
    /// Highest claim-ready / offered body height on the work path (body queue
    /// densify + confirm offer bookkeeping; not a dual-track Class A HWM).
    pub max_ready_height: u32,
    /// Highest header height currently on the ordered work path.
    pub max_ordered_height: u32,
    pub headers_done: bool,
    pub empty_header_streak: u32,
    pub header_req_seq: u32,
    /// Rotates which peer is offered work first.
    pub assign_rot: usize,
    /// Stall-disconnect cooldowns (addr → until).
    pub addr_cooldown: HashMap<NetAddr, Instant>,
    /// Per-peer addr token buckets (peer id → bucket), as on tip-follow
    /// sessions. Dropped when the peer dies.
    pub addr_buckets: HashMap<usize, rbitcoin_net::peer::AddrTokenBucket>,
    /// Process-local kick counts for addresses in addrman or still cooling.
    pub addr_strikes: HashMap<NetAddr, u8>,
    /// Relative-slow hysteresis: peer id + first Gate B fail ms.
    pub relative_slow_suspect: Option<(usize, u64)>,
    /// Mono ms of last relative-slow disconnect (`0` = never).
    pub relative_slow_last_kick_ms: u64,
    /// Process-local invalid apply marks for most-work reorg (IBD run only).
    pub reorg: IbdReorgState,
    /// First height ≥ path_lo densify should walk (filled prefix skip).
    pub densify_scan_lo: u32,
    /// Last Full-assign `path_lo`; drop watermark when the tip shrinks.
    pub assign_path_lo: u32,
    /// Hashes that already produced one [`super::confirm::ConfirmRejectClass::EngineFault`].
    pub engine_fault_seen: HashSet<BlockHash>,
    /// Set when a second engine-fault hits the same hash — IBD must halt.
    pub halt: Option<String>,
    /// First time confirm rejected without tip progress (download gate timer).
    pub confirm_stuck_since: Option<Instant>,
    /// Set by header-work rewind/plant so the main loop can drop in-channel plans.
    /// [`ConfirmQuiesce::Plan`] keeps the lead reserved. [`ConfirmQuiesce::Refund`]
    /// is a confirmed rewind.
    pub confirm_quiesce: ConfirmQuiesce,
    /// Repeated Cascade at the same tip for the same hash → escalate to halt.
    pub cascade_at: Option<(BlockHash, [u8; 32], u8)>,
    /// Tip-child BadPrev retries. Another block's soft reject does not clear this.
    pub tip_child_bad_prev: Option<(BlockHash, [u8; 32], u8)>,
    /// Assign-stop byte budget, snapshotted when assign runs (`u64::MAX` = off).
    pub intake_stop: u64,
    /// Body-queue bytes snapshotted with [`Self::intake_stop`].
    pub intake_queued: u64,
    /// Recent block payload lengths, oldest first. Per-peer byte cap only.
    pub block_lens: VecDeque<u32>,
    /// Checkpoints for headers past the download queue.
    pub header_walk: super::header_walk::HeaderWalk,
    /// `-whitelist` / operator noban grants. `None`: no peer is noban.
    pub perms: Option<std::sync::Arc<rbitcoin_net::peers::PeerHub>>,
    /// Confirm engine feed: assign does not demote a body load still holds,
    /// and does not getdata a lone consensus-invalid body until its verdict
    /// is applied.
    pub confirm_feed: Option<std::sync::Arc<super::confirm::ConfirmFeed>>,
    /// Set while a header thread owns store IO. Peer reads only enqueue.
    pub header_cmd: Option<HeaderCmdTx>,
    /// Heights below this are already claim-ready. Rewind starts here.
    pub densify_ready_through: u32,
    /// Confirmed-chain `getheaders` tail for one published tip.
    pub confirmed_locator_tail: Option<ConfirmedLocatorTail>,
}

pub enum HeaderCmd {
    Batch {
        peer: usize,
        headers: Vec<bitcoin::block::Header>,
    },
    Ask,
    Reseed,
    Report,
}

/// Header batches waiting on the header thread. Past this, the coordination
/// loop stops pulling header events so the queue cannot grow without bound.
pub const HEADER_CMD_BOUND: usize = 64;

pub type HeaderCmdTx = tokio::sync::mpsc::Sender<HeaderCmd>;

/// Send `cmd` to the header thread. A full channel leaves the event on the
/// peer queue; the coordination loop stops pulling header events until there
/// is room.
pub fn enqueue_header_cmd(st: &mut IbdWorkState, cmd: HeaderCmd) {
    let Some(tx) = st.header_cmd.clone() else {
        return;
    };
    let _ = tx.try_send(cmd);
}

impl IbdWorkState {
    /// Header walk rewound. A confirmed rewind already recorded keeps its refund.
    pub fn quiesce_plan(&mut self) {
        if self.confirm_quiesce != ConfirmQuiesce::Refund {
            self.confirm_quiesce = ConfirmQuiesce::Plan;
        }
    }

    /// Confirmed tip moved. The next drain returns reservations above that tip.
    pub fn quiesce_refund(&mut self) {
        self.confirm_quiesce = ConfirmQuiesce::Refund;
    }

    pub fn new(slots: Vec<PeerSlot>, tip_hash: Option<BlockHash>, tip_height: Option<u32>) -> Self {
        let mut known_headers = HashSet::new();
        let mut hash_height = HashMap::new();
        let mut max_peer_height = tip_height.unwrap_or(0);
        if let Some(h) = tip_hash {
            known_headers.insert(h);
            if let Some(th) = tip_height {
                hash_height.insert(h, th);
            }
        }
        for s in &slots {
            max_peer_height = max_peer_height.max(s.peer_height);
        }
        let start_tip = tip_height.unwrap_or(0);
        Self {
            slots,
            inflight: HashMap::new(),
            ordered: VecDeque::new(),
            ordered_set: HashSet::new(),
            hash_height,
            path_prev: HashMap::new(),
            height_to_hash: {
                let mut m = HeightIndex::default();
                if let (Some(h), Some(th)) = (tip_hash, tip_height) {
                    m.insert(th, h);
                }
                m
            },
            known_headers,
            body: BodyPresence::new(),
            header_fks: HashMap::new(),
            max_peer_height,
            max_ready_height: start_tip,
            max_ordered_height: start_tip,
            headers_done: false,
            empty_header_streak: 0,
            header_req_seq: 0,
            assign_rot: 0,
            addr_cooldown: HashMap::new(),
            addr_buckets: HashMap::new(),
            addr_strikes: HashMap::new(),
            relative_slow_suspect: None,
            relative_slow_last_kick_ms: 0,
            reorg: IbdReorgState::new(),
            densify_scan_lo: 0,
            assign_path_lo: 0,
            engine_fault_seen: HashSet::new(),
            halt: None,
            confirm_stuck_since: None,
            confirm_quiesce: ConfirmQuiesce::Live,
            cascade_at: None,
            tip_child_bad_prev: None,
            intake_stop: u64::MAX,
            intake_queued: 0,
            block_lens: VecDeque::new(),
            header_walk: super::header_walk::HeaderWalk::default(),
            perms: None,
            confirm_feed: None,
            header_cmd: None,
            densify_ready_through: 0,
            confirmed_locator_tail: None,
        }
    }

    /// Record `hash` at chain height `ht` (keeps inverse map in sync).
    ///
    /// Tests / reorg gather may plant a slot. Header intake uses
    /// [`Self::try_set_path_slot`] (first-wins, prev-anchored).
    /// Hashes that left `inflight` without arriving. Densify skips forward past
    /// requested heights and never walks below `densify_scan_lo`, so move the
    /// cursor back to the lowest of them or densify will not request them again.
    pub fn reopen_for_densify(&mut self, freed: &[BlockHash]) {
        let lowest = freed
            .iter()
            .filter_map(|h| self.hash_height.get(h).copied())
            .min();
        if let Some(lo) = lowest {
            self.densify_scan_lo = self.densify_scan_lo.min(lo);
            self.densify_ready_through = self.densify_ready_through.min(lo);
        }
    }

    pub fn record_height(&mut self, hash: BlockHash, ht: u32) {
        if let Some(old) = self.hash_height.insert(hash, ht) {
            if old != ht && self.height_to_hash.get(&old) == Some(&hash) {
                self.height_to_hash.remove(&old);
            }
        }
        self.height_to_hash.insert(ht, hash);
    }

    /// Admit `hash` to the work-path slot `ht` only if it chains onto the
    /// occupant of `ht-1` (or onto the store tip for tip+1). First-wins:
    /// a competing header never displaces a connected occupant.
    ///
    /// Always records `hash_height`. Returns whether the path slot was set
    /// to `hash`.
    pub fn note_path_prev(&mut self, hash: BlockHash, prev: BlockHash) {
        self.path_prev.insert(hash, prev);
    }

    pub fn try_set_path_slot(
        &mut self,
        hash: BlockHash,
        ht: u32,
        prev: BlockHash,
        tip: Option<(u32, BlockHash)>,
    ) -> bool {
        self.path_prev.insert(hash, prev);
        if let Some(old) = self.hash_height.insert(hash, ht) {
            if old != ht && self.height_to_hash.get(&old) == Some(&hash) {
                self.height_to_hash.remove(&old);
            }
        }
        let anchored = match tip {
            Some((tip_h, tip_hash)) if ht == tip_h.saturating_add(1) => prev == tip_hash,
            _ => self.height_to_hash.get(&ht.wrapping_sub(1)) == Some(&prev),
        };
        if !anchored {
            return false;
        }
        if let Some(cur) = self.height_to_hash.get(&ht) {
            return *cur == hash;
        }
        self.height_to_hash.insert(ht, hash);
        true
    }

    pub fn is_on_path(&self, hash: &BlockHash, ht: u32) -> bool {
        self.height_to_hash.get(&ht) == Some(hash)
    }

    /// Drop work-path slots strictly above `ht` (reorg apply suffix).
    pub fn clear_path_above(&mut self, ht: u32) {
        let drop: Vec<BlockHash> = self
            .height_to_hash
            .iter()
            .filter(|(h, _)| *h > ht)
            .map(|(_, hash)| *hash)
            .collect();
        self.height_to_hash.retain(|h, _| *h <= ht);
        for hash in drop {
            self.ordered_set.remove(&hash);
        }
        while let Some(front) = self.ordered.front().copied() {
            if self.ordered_set.contains(&front) {
                break;
            }
            self.ordered.pop_front();
        }
    }

    /// Cheap occupancy of work-path maps/deques (for `ibd: sizes`; all O(1) lens).
    pub fn structure_sizes(&self) -> WorkStructureSizes {
        let peer_inflight: usize = self.slots.iter().map(|s| s.in_flight.len()).sum();
        WorkStructureSizes {
            ordered: self.ordered.len(),
            ordered_set: self.ordered_set.len(),
            hash_height: self.hash_height.len(),
            height_to_hash: self.height_to_hash.len(),
            header_fks: self.header_fks.len(),
            known_headers: self.known_headers.len(),
            inflight: self.inflight.len(),
            peer_inflight,
            addr_cooldown: self.addr_cooldown.len(),
            body: self.body.size_snapshot(),
            map_bytes: self.map_bytes(),
        }
    }

    /// Heap the work-path maps hold at their `capacity()`.
    fn map_bytes(&self) -> u64 {
        use rbitcoin_primitives::heap_bytes::hash_table_bytes;
        let peers: u64 = self
            .slots
            .iter()
            .map(|s| hash_table_bytes::<BlockHash, ()>(s.in_flight.capacity()))
            .sum();
        peers
            + hash_table_bytes::<BlockHash, InflightReq>(self.inflight.capacity())
            + (self.ordered.capacity() * std::mem::size_of::<BlockHash>()) as u64
            + hash_table_bytes::<BlockHash, ()>(self.ordered_set.capacity())
            + hash_table_bytes::<BlockHash, u32>(self.hash_height.capacity())
            + self.height_to_hash.heap_bytes()
            + hash_table_bytes::<BlockHash, BlockHash>(self.path_prev.capacity())
            + hash_table_bytes::<BlockHash, ()>(self.known_headers.capacity())
            + self.body.heap_bytes()
            + hash_table_bytes::<BlockHash, Fk>(self.header_fks.capacity())
            + hash_table_bytes::<NetAddr, Instant>(self.addr_cooldown.capacity())
    }

    /// True when `ordered` has piled up ghost entries (completed hashes left
    /// in the deque). Caller cadences retain; bloat skips the wait.
    pub fn ordered_bloated(&self) -> bool {
        self.ordered.len() > self.ordered_set.len().saturating_mul(4).max(128)
    }

    /// Per-second work-path hygiene.
    ///
    /// Compacts `ordered` ghosts, drops `height_to_hash` slots at or below
    /// the queue tail that are no longer live or in flight, and bounds the
    /// body cache to live work. The indexed header walk above the queue tail
    /// stays without a visit. Chain-sized follower maps are
    /// [`Self::deep_hygiene`]; sweeping them here cost ~0.8 s of every second
    /// at a mainnet tip.
    pub fn hygiene(&mut self) {
        compact_ordered(&mut self.ordered, &self.ordered_set);
        let live = &self.ordered_set;
        let inflight = &self.inflight;
        let queue_tail = self
            .ordered
            .iter()
            .rev()
            .find(|hash| live.contains(*hash))
            .and_then(|hash| self.hash_height.get(hash).copied())
            .unwrap_or(0);
        let indexed = self.header_walk.indexed_band_end();
        // Slot writers set `hash_height` in the same call, so the indexed
        // band does not need that check on this pass.
        self.height_to_hash
            .retain_outside(queue_tail, indexed, |ht, hash| {
                self.hash_height.get(hash) == Some(&ht)
                    && (live.contains(hash) || inflight.contains_key(hash))
            });
        // Bound body presence cache to live work (rejected
        // never hygiene-pruned — see BodyPresence::hygiene_retain).
        self.body
            .hygiene_retain(|h| live.contains(h) || inflight.contains_key(h));
    }

    /// Chain-sized prune of the work-path maps.
    ///
    /// Keeps live headers, in-flight headers, and the current occupant of
    /// each height. `header_fks` / `hash_height` / `path_prev` keep those
    /// plus any row still in `known_headers`. A map is walked only when it
    /// is more than [`DEEP_HYGIENE_SLACK`] past that ceiling: during a header
    /// walk `known_headers` is chain-sized because the heights are occupied,
    /// and a sweep that cannot drop rows still holds the coordination lock.
    /// Caller owns cadence: main loop every `cadence::DEEP_HYGIENE_PERIOD`,
    /// never per turn.
    pub fn deep_hygiene(&mut self) {
        let live_n = self.ordered_set.len();
        let inflight_n = self.inflight.len();
        let occupant_ceiling = live_n
            .saturating_add(inflight_n)
            .saturating_add(self.height_to_hash.len())
            .saturating_add(DEEP_HYGIENE_SLACK);
        if self.known_headers.len() > occupant_ceiling {
            let live = &self.ordered_set;
            let inflight = &self.inflight;
            self.known_headers.retain(|h| {
                live.contains(h)
                    || inflight.contains_key(h)
                    || self
                        .hash_height
                        .get(h)
                        .is_some_and(|ht| self.height_to_hash.get(ht) == Some(h))
            });
        }
        // `forget_queue_hashes` removes known and height rows and leaves the
        // fk and parent rows, so those maps can exceed `known_headers`.
        let follower_ceiling = self
            .known_headers
            .len()
            .saturating_add(live_n)
            .saturating_add(inflight_n)
            .saturating_add(DEEP_HYGIENE_SLACK);
        let followers = self
            .header_fks
            .len()
            .max(self.path_prev.len())
            .max(self.hash_height.len());
        if followers > follower_ceiling {
            let live = &self.ordered_set;
            let inflight = &self.inflight;
            let known = &self.known_headers;
            // Body-queue rehydrate records an occupant without `known_headers`.
            // Per-second hygiene does not revisit that band, so this pass must
            // leave the occupant's `hash_height` in place.
            self.hash_height.retain(|h, ht| {
                live.contains(h)
                    || inflight.contains_key(h)
                    || known.contains(h)
                    || self.height_to_hash.get(ht) == Some(h)
            });
            let keep = |h: &BlockHash| {
                live.contains(h)
                    || inflight.contains_key(h)
                    || known.contains(h)
                    || self.hash_height.contains_key(h)
            };
            self.header_fks.retain(|h, _| keep(h));
            self.path_prev.retain(|h, _| keep(h));
        }
    }
}

/// Extra rows tolerated before [`IbdWorkState::deep_hygiene`] walks a map.
pub const DEEP_HYGIENE_SLACK: usize = 4096;

/// One block hash per height, from the lowest occupant through the highest.
///
/// A hole is an empty slot. Per-second hygiene uses [`Self::retain_outside`]
/// so the indexed header walk above the download queue is not visited.
#[derive(Clone, Debug, Default)]
pub struct HeightIndex {
    base: u32,
    slots: Vec<Option<BlockHash>>,
    occupied: usize,
}

/// Read one height's occupant. [`HeightIndex`] is the work path; tests still
/// build a [`HashMap`].
pub trait HeightOccupants {
    fn occupant(&self, height: u32) -> Option<BlockHash>;
}

impl HeightIndex {
    /// Heap the slot window holds at its capacity.
    pub fn heap_bytes(&self) -> u64 {
        rbitcoin_primitives::heap_bytes::vec_bytes(&self.slots)
    }
}

impl HeightOccupants for HeightIndex {
    fn occupant(&self, height: u32) -> Option<BlockHash> {
        self.get(&height).copied()
    }
}

impl HeightOccupants for HashMap<u32, BlockHash> {
    fn occupant(&self, height: u32) -> Option<BlockHash> {
        self.get(&height).copied()
    }
}

impl HeightIndex {
    pub fn len(&self) -> usize {
        self.occupied
    }

    pub fn is_empty(&self) -> bool {
        self.occupied == 0
    }

    pub fn get(&self, ht: &u32) -> Option<&BlockHash> {
        let i = ht.checked_sub(self.base)? as usize;
        self.slots.get(i)?.as_ref()
    }

    pub fn contains_key(&self, ht: &u32) -> bool {
        self.get(ht).is_some()
    }

    pub fn insert(&mut self, ht: u32, hash: BlockHash) -> Option<BlockHash> {
        if self.occupied == 0 {
            self.base = ht;
            self.slots.clear();
            self.slots.push(Some(hash));
            self.occupied = 1;
            return None;
        }
        if ht < self.base {
            let gap = (self.base - ht) as usize;
            self.slots.splice(0..0, std::iter::repeat_n(None, gap));
            self.base = ht;
            self.slots[0] = Some(hash);
            self.occupied += 1;
            return None;
        }
        let i = (ht - self.base) as usize;
        if i >= self.slots.len() {
            self.slots.resize(i + 1, None);
        }
        let prev = self.slots[i].replace(hash);
        if prev.is_none() {
            self.occupied += 1;
        }
        prev
    }

    pub fn remove(&mut self, ht: &u32) -> Option<BlockHash> {
        let i = ht.checked_sub(self.base)? as usize;
        let prev = self.slots.get_mut(i)?.take()?;
        self.occupied -= 1;
        self.shrink_ends();
        Some(prev)
    }

    pub fn clear(&mut self) {
        self.base = 0;
        self.slots.clear();
        self.occupied = 0;
    }

    pub fn iter(&self) -> impl Iterator<Item = (u32, &BlockHash)> {
        let base = self.base;
        self.slots
            .iter()
            .enumerate()
            .filter_map(move |(i, slot)| slot.as_ref().map(|hash| (base + i as u32, hash)))
    }

    pub fn values(&self) -> impl Iterator<Item = &BlockHash> {
        self.slots.iter().filter_map(|slot| slot.as_ref())
    }

    /// Highest occupied height.
    pub fn highest(&self) -> Option<u32> {
        if self.occupied == 0 {
            None
        } else {
            Some(self.base + self.slots.len() as u32 - 1)
        }
    }

    pub fn any_above(&self, tip: u32) -> bool {
        self.highest().is_some_and(|h| h > tip)
    }

    pub fn retain(&mut self, mut f: impl FnMut(&u32, &mut BlockHash) -> bool) {
        for i in 0..self.slots.len() {
            let ht = self.base.saturating_add(i as u32);
            let drop = match self.slots[i].as_mut() {
                Some(hash) => !f(&ht, hash),
                None => false,
            };
            if drop {
                self.slots[i] = None;
                self.occupied -= 1;
            }
        }
        self.shrink_ends();
    }

    /// Keep or drop slots outside `(queue_tail, indexed_through]`.
    ///
    /// `indexed_through` is the header-walk tip when that walk indexes every
    /// height up to it. Those slots are not visited. Every other occupied
    /// slot is dropped unless `keep` returns true.
    pub fn retain_outside(
        &mut self,
        queue_tail: u32,
        indexed_through: Option<u32>,
        mut keep: impl FnMut(u32, &BlockHash) -> bool,
    ) {
        if self.slots.is_empty() {
            return;
        }
        let skip_hi = indexed_through.filter(|tip| queue_tail < *tip);
        if self.base <= queue_tail {
            let last = ((queue_tail - self.base) as usize).min(self.slots.len() - 1);
            for i in 0..=last {
                self.consider(i, &mut keep);
            }
        }
        let suffix_from = if let Some(tip) = skip_hi {
            if self.base > tip {
                0
            } else {
                ((tip - self.base) as usize).saturating_add(1)
            }
        } else if queue_tail < self.base {
            0
        } else {
            ((queue_tail - self.base) as usize).saturating_add(1)
        };
        for i in suffix_from..self.slots.len() {
            self.consider(i, &mut keep);
        }
        self.shrink_ends();
    }

    fn consider(&mut self, i: usize, keep: &mut impl FnMut(u32, &BlockHash) -> bool) {
        let ht = self.base.saturating_add(i as u32);
        let drop = match self.slots[i].as_ref() {
            Some(hash) => !keep(ht, hash),
            None => false,
        };
        if drop {
            self.slots[i] = None;
            self.occupied -= 1;
        }
    }

    fn shrink_ends(&mut self) {
        if self.occupied == 0 {
            self.slots.clear();
            self.base = 0;
            return;
        }
        if let Some(skip) = self.slots.iter().position(|slot| slot.is_some()) {
            self.slots.drain(0..skip);
            self.base = self.base.saturating_add(skip as u32);
        }
        while self.slots.last().is_some_and(|slot| slot.is_none()) {
            self.slots.pop();
        }
    }
}

#[cfg(test)]
mod height_index_pins {
    use super::HeightIndex;
    use bitcoin::hashes::Hash;
    use bitcoin::BlockHash;

    fn hash(b: u8) -> BlockHash {
        BlockHash::from_byte_array([b; 32])
    }

    fn index(pairs: &[(u32, u8)]) -> HeightIndex {
        let mut ix = HeightIndex::default();
        for &(ht, b) in pairs {
            ix.insert(ht, hash(b));
        }
        ix
    }

    fn heights(ix: &HeightIndex) -> Vec<u32> {
        ix.iter().map(|(ht, _)| ht).collect()
    }

    /// Per-second hygiene reads this map. A height below the base has to count.
    #[test]
    fn a_lower_height_widens_the_base() {
        let ix = index(&[(5, 1), (3, 2)]);
        assert!(!ix.is_empty());
        assert_eq!(ix.len(), 2);
        assert_eq!(ix.values().count(), 2);
        assert_eq!(ix.get(&3), Some(&hash(2)));
        assert_eq!(ix.get(&5), Some(&hash(1)));
    }

    #[test]
    fn remove_drops_the_occupant() {
        let mut ix = index(&[(5, 1)]);
        assert_eq!(ix.remove(&5), Some(hash(1)));
        assert!(ix.is_empty());
        assert_eq!(ix.len(), 0);
        assert_eq!(ix.values().count(), 0);
    }

    #[test]
    fn retain_drops_only_the_rejected_height() {
        let mut ix = index(&[(5, 1), (7, 2)]);
        ix.retain(|ht, _| *ht == 7);
        assert_eq!(heights(&ix), vec![7]);
        assert_eq!(ix.len(), 1);
    }

    /// Slots inside `(queue_tail, indexed_through]` stay. Hygiene uses that window.
    #[test]
    fn outside_the_indexed_window_is_the_only_band_visited() {
        let mut ix = index(&[(1, 1), (2, 2), (3, 3), (4, 4), (5, 5)]);
        ix.retain_outside(2, Some(4), |_, _| false);
        assert_eq!(heights(&ix), vec![3, 4]);
    }

    #[test]
    fn without_an_index_tip_every_height_above_the_tail_is_visited() {
        let mut ix = index(&[(1, 1), (2, 2), (3, 3)]);
        ix.retain_outside(1, None, |_, _| false);
        assert!(ix.is_empty());
    }

    #[test]
    fn a_base_on_the_index_tip_stays_in_the_window() {
        let mut ix = index(&[(5, 1), (6, 2)]);
        ix.retain_outside(1, Some(5), |_, _| false);
        assert_eq!(heights(&ix), vec![5]);
    }

    #[test]
    fn a_base_above_the_index_tip_is_all_outside() {
        let mut ix = index(&[(10, 1), (11, 2)]);
        ix.retain_outside(1, Some(5), |_, _| false);
        assert!(ix.is_empty());
    }
}

#[cfg(test)]
mod work_path_pins {
    use super::{IbdWorkState, InflightReq, DEEP_HYGIENE_SLACK};
    use bitcoin::hashes::Hash;
    use bitcoin::BlockHash;

    fn hash_n(n: u32) -> BlockHash {
        let mut raw = [0u8; 32];
        raw[..4].copy_from_slice(&n.to_le_bytes());
        BlockHash::from_byte_array(raw)
    }

    fn fresh() -> IbdWorkState {
        IbdWorkState::new(Vec::new(), None, None)
    }

    /// Assign still treats a retired holder as someone who was asked.
    #[test]
    fn retired_holder_stays_on_the_request() {
        let mut req = InflightReq::new(2);
        assert!(req.add_peer(7));
        req.retire_peer(2);
        assert_eq!(req.retired_ids().collect::<Vec<_>>(), vec![2]);
        assert_eq!(req.holder_ids().collect::<Vec<_>>(), vec![2, 7]);
    }

    /// Reorg gather moves a hash without erasing the height a sibling owns.
    #[test]
    fn moving_a_hash_leaves_the_height_a_sibling_owns() {
        let mut st = fresh();
        let a = hash_n(1);
        let b = hash_n(2);
        st.record_height(a, 1);
        st.record_height(b, 1);
        st.record_height(a, 2);
        assert_eq!(st.height_to_hash.get(&1), Some(&b));
        assert_eq!(st.height_to_hash.get(&2), Some(&a));
    }

    /// Header intake keeps the connected occupant when the same hash is re-offered.
    #[test]
    fn an_unanchored_repeat_leaves_the_slot() {
        let mut st = fresh();
        let a = hash_n(1);
        st.record_height(a, 5);
        assert!(!st.try_set_path_slot(a, 5, hash_n(9), None));
        assert_eq!(st.height_to_hash.get(&5), Some(&a));
    }

    /// Header intake chains onto the previous occupant.
    #[test]
    fn a_chained_offer_keeps_the_parent_slot() {
        let mut st = fresh();
        let a = hash_n(1);
        let b = hash_n(2);
        st.record_height(a, 1);
        st.record_height(b, 1);
        assert!(st.try_set_path_slot(a, 2, b, None));
        assert_eq!(st.height_to_hash.get(&1), Some(&b));
        assert_eq!(st.height_to_hash.get(&2), Some(&a));
    }

    /// Reorg apply drops the suffix and leaves the fork height queued.
    #[test]
    fn clear_above_keeps_the_fork_height_queued() {
        let mut st = fresh();
        let a = hash_n(1);
        let b = hash_n(2);
        st.record_height(a, 5);
        st.record_height(b, 6);
        st.ordered_set.insert(a);
        st.ordered_set.insert(b);
        st.ordered.push_back(a);
        st.ordered.push_back(b);
        st.clear_path_above(5);
        assert!(st.is_on_path(&a, 5));
        assert!(!st.is_on_path(&b, 6));
        assert!(st.ordered_set.contains(&a));
        assert!(!st.ordered_set.contains(&b));
    }

    /// Ghost entries past four times the live set, and past 128, compact.
    #[test]
    fn one_past_the_ghost_floor_is_bloated() {
        let mut st = fresh();
        for n in 0..128 {
            st.ordered.push_back(hash_n(n));
        }
        assert!(!st.ordered_bloated());
        st.ordered.push_back(hash_n(128));
        assert!(st.ordered_bloated());
    }

    /// Per-second hygiene keeps an in-flight hash that is not on the live queue.
    #[test]
    fn hygiene_keeps_an_inflight_hash_below_the_tail() {
        let mut st = fresh();
        let h = hash_n(1);
        st.height_to_hash.insert(5, h);
        st.hash_height.insert(h, 5);
        st.inflight.insert(h, InflightReq::new(1));
        st.body.mark_pending(h);
        st.hygiene();
        assert_eq!(st.height_to_hash.get(&5), Some(&h));
        assert!(st.body.is_pending_hash(&h));
    }

    /// Deep hygiene walks `known_headers` only past the slack.
    #[test]
    fn known_at_the_slack_ceiling_is_not_swept() {
        let mut st = fresh();
        let kept = hash_n(1);
        st.inflight.insert(kept, InflightReq::new(1));
        st.known_headers.insert(kept);
        let junk = DEEP_HYGIENE_SLACK as u32;
        for n in 0..junk {
            st.known_headers.insert(hash_n(100 + n));
        }
        assert_eq!(st.known_headers.len(), DEEP_HYGIENE_SLACK + 1);
        st.deep_hygiene();
        assert!(st.known_headers.contains(&hash_n(100)));
    }

    /// One past that ceiling, an in-flight hash stays and a stranger goes.
    #[test]
    fn known_sweep_keeps_inflight_only() {
        let mut st = fresh();
        let kept = hash_n(1);
        st.inflight.insert(kept, InflightReq::new(1));
        st.known_headers.insert(kept);
        for n in 0..=DEEP_HYGIENE_SLACK as u32 {
            st.known_headers.insert(hash_n(100 + n));
        }
        st.deep_hygiene();
        assert!(st.known_headers.contains(&kept));
        assert!(!st.known_headers.contains(&hash_n(100)));
        assert_eq!(st.known_headers.len(), 1);
    }

    /// Follower maps walk only past their own slack.
    #[test]
    fn followers_at_the_slack_ceiling_are_not_swept() {
        let mut st = fresh();
        st.inflight.insert(hash_n(1), InflightReq::new(1));
        let n = (DEEP_HYGIENE_SLACK + 1) as u32;
        for i in 0..n {
            st.header_fks
                .insert(hash_n(100 + i), rbitcoin_primitives::Fk(1));
        }
        st.deep_hygiene();
        assert_eq!(st.header_fks.len(), DEEP_HYGIENE_SLACK + 1);
    }

    /// Past that ceiling, in-flight, known, and occupant rows stay.
    #[test]
    fn follower_sweep_keeps_each_reason() {
        let mut st = fresh();
        let row_inflight = hash_n(1);
        let row_known = hash_n(2);
        let occupant = hash_n(3);
        let fk_inflight = hash_n(4);
        let fk_known = hash_n(5);
        st.inflight.insert(row_inflight, InflightReq::new(1));
        st.inflight.insert(fk_inflight, InflightReq::new(2));
        st.known_headers.insert(row_known);
        st.known_headers.insert(fk_known);
        st.hash_height.insert(occupant, 9);
        st.height_to_hash.insert(9, occupant);
        st.hash_height.insert(row_inflight, 8);
        st.hash_height.insert(row_known, 7);
        for i in 0..(DEEP_HYGIENE_SLACK as u32 + 8) {
            let h = hash_n(100 + i);
            st.hash_height.insert(h, 1_000 + i);
            st.header_fks.insert(h, rbitcoin_primitives::Fk(1));
            st.path_prev.insert(h, hash_n(9));
        }
        for h in [fk_inflight, fk_known, occupant] {
            st.header_fks.insert(h, rbitcoin_primitives::Fk(1));
            st.path_prev.insert(h, hash_n(6));
        }
        st.deep_hygiene();
        assert!(st.hash_height.contains_key(&row_inflight));
        assert!(st.hash_height.contains_key(&row_known));
        assert!(st.hash_height.contains_key(&occupant));
        assert!(st.header_fks.contains_key(&fk_inflight));
        assert!(st.header_fks.contains_key(&fk_known));
        assert!(st.header_fks.contains_key(&occupant));
        assert!(st.path_prev.contains_key(&fk_inflight));
        assert!(!st.hash_height.contains_key(&hash_n(100)));
        assert!(!st.header_fks.contains_key(&hash_n(100)));
    }
}
