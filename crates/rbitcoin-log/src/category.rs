//! Debug categories: which subsystems write DEBUG and TRACE lines.
//!
//! ERROR, WARN, and INFO follow the global level only. A DEBUG line is
//! written when its category is on. A TRACE line also needs the global
//! level at TRACE. [`crate::init`] turns every category on at DEBUG or
//! TRACE and every category off below that, so startup behaves as the
//! level alone did. The `logging` RPC changes the set at runtime.
//!
//! The set is one atomic word, so concurrent updates never tear it.

use crate::Level;
use std::sync::atomic::{AtomicU32, Ordering};

/// One subsystem. [`crate::log_cat`] writes its name as the line prefix.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[repr(u8)]
pub enum Category {
    /// Per-call `api:` lines for RPC, Esplora, and Electrum (TRACE).
    Api,
    Electrum,
    I2p,
    Ibd,
    Mempool,
    Node,
    /// Peer sessions, handshakes, dials, addr and compact-block relay.
    P2p,
    Query,
    Store,
    Sv2,
    Tip,
    Tor,
    /// Transaction announce, getdata, orphan parking, isolated broadcast.
    Txrelay,
}

impl Category {
    /// Every category, sorted by name.
    pub const ALL: [Category; 13] = [
        Category::Api,
        Category::Electrum,
        Category::I2p,
        Category::Ibd,
        Category::Mempool,
        Category::Node,
        Category::P2p,
        Category::Query,
        Category::Store,
        Category::Sv2,
        Category::Tip,
        Category::Tor,
        Category::Txrelay,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Category::Api => "api",
            Category::Electrum => "electrum",
            Category::I2p => "i2p",
            Category::Ibd => "ibd",
            Category::Mempool => "mempool",
            Category::Node => "node",
            Category::P2p => "p2p",
            Category::Query => "query",
            Category::Store => "store",
            Category::Sv2 => "sv2",
            Category::Tip => "tip",
            Category::Tor => "tor",
            Category::Txrelay => "txrelay",
        }
    }

    /// Exact lowercase name. `all` and `none` are selectors, not
    /// categories; the RPC layer expands them.
    pub fn parse(s: &str) -> Option<Category> {
        Category::ALL.into_iter().find(|c| c.as_str() == s)
    }

    fn bit(self) -> u32 {
        1 << (self as u8)
    }
}

const ALL_MASK: u32 = (1 << Category::ALL.len()) - 1;

static CATEGORIES: AtomicU32 = AtomicU32::new(0);

/// Every category on (`true`) or off. Called by [`crate::init`].
pub(crate) fn set_all(on: bool) {
    CATEGORIES.store(if on { ALL_MASK } else { 0 }, Ordering::Relaxed);
}

pub fn category_enabled(cat: Category) -> bool {
    CATEGORIES.load(Ordering::Relaxed) & cat.bit() != 0
}

/// Whether a line at `level` in `cat` is written.
pub fn log_enabled(level: Level, cat: Category) -> bool {
    match level {
        Level::Error | Level::Warn | Level::Info => crate::enabled(level),
        Level::Debug => category_enabled(cat),
        Level::Trace => category_enabled(cat) && crate::enabled(Level::Trace),
    }
}

fn map_of(mask: u32) -> Vec<(Category, bool)> {
    Category::ALL
        .into_iter()
        .map(|c| (c, mask & c.bit() != 0))
        .collect()
}

/// Every category with its state, sorted by name.
pub fn category_map() -> Vec<(Category, bool)> {
    map_of(CATEGORIES.load(Ordering::Relaxed))
}

/// Turn `include` on, then `exclude` off, in one atomic step. A category in
/// both ends up off. Returns the set this update produced.
pub fn update_categories(include: &[Category], exclude: &[Category]) -> Vec<(Category, bool)> {
    let on = include.iter().fold(0, |m, c| m | c.bit());
    let off = exclude.iter().fold(0, |m, c| m | c.bit());
    let next = |mask: u32| (mask | on) & !off;
    let prev = CATEGORIES
        .try_update(Ordering::Relaxed, Ordering::Relaxed, |m| Some(next(m)))
        .unwrap_or_else(|m| m);
    map_of(next(prev))
}
