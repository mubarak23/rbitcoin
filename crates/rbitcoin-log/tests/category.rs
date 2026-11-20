use crate::root::lock_log_init as lock_categories;
use rbitcoin_log::*;

fn enabled_names() -> Vec<&'static str> {
    category_map()
        .into_iter()
        .filter(|(_, on)| *on)
        .map(|(c, _)| c.as_str())
        .collect()
}

#[test]
fn category_names_are_sorted_unique_and_parse_back() {
    let names: Vec<_> = Category::ALL.iter().map(|c| c.as_str()).collect();
    let mut sorted = names.clone();
    sorted.sort_unstable();
    sorted.dedup();
    assert_eq!(names, sorted, "ALL must be sorted by name with no repeats");
    assert_eq!(
        names,
        [
            "api", "electrum", "i2p", "ibd", "mempool", "node", "p2p", "query", "store", "sv2",
            "tip", "tor", "txrelay",
        ]
    );
    for c in Category::ALL {
        assert_eq!(Category::parse(c.as_str()), Some(c));
    }
    assert_eq!(Category::parse("all"), None);
    assert_eq!(Category::parse("MEMPOOL"), None);
    assert_eq!(Category::parse("net"), None);
}

#[test]
fn init_level_sets_the_starting_categories() {
    let _g = lock_categories();
    init(Level::Info);
    assert!(enabled_names().is_empty(), "{:?}", enabled_names());
    init(Level::Debug);
    assert_eq!(enabled_names().len(), Category::ALL.len());
    init(Level::Trace);
    assert_eq!(enabled_names().len(), Category::ALL.len());
    init_off();
    assert!(enabled_names().is_empty());
    init(Level::Info);
}

#[test]
fn update_applies_include_then_exclude() {
    let _g = lock_categories();
    init(Level::Info);
    let map = update_categories(&[Category::Mempool, Category::P2p], &[]);
    let on: Vec<_> = map
        .iter()
        .filter(|(_, b)| *b)
        .map(|(c, _)| c.as_str())
        .collect();
    assert_eq!(on, ["mempool", "p2p"]);
    assert_eq!(map, category_map());
    // A name in both lists ends up off.
    update_categories(&[Category::Ibd], &[Category::Ibd, Category::P2p]);
    assert_eq!(enabled_names(), ["mempool"]);
    // Empty lists change nothing.
    let before = category_map();
    assert_eq!(update_categories(&[], &[]), before);
    init(Level::Info);
}

#[test]
fn concurrent_updates_do_not_lose_a_category() {
    let _g = lock_categories();
    init(Level::Info);
    std::thread::scope(|s| {
        for c in Category::ALL {
            s.spawn(move || {
                for _ in 0..2_000 {
                    update_categories(&[c], &[]);
                    update_categories(&[], &[c]);
                }
                update_categories(&[c], &[]);
            });
        }
    });
    assert_eq!(enabled_names().len(), Category::ALL.len());
    init(Level::Info);
}

#[test]
fn debug_line_follows_its_category_not_the_level() {
    let _g = lock_categories();
    init(Level::Info);
    assert!(!log_enabled(Level::Debug, Category::Mempool));
    update_categories(&[Category::Mempool], &[]);
    assert!(log_enabled(Level::Debug, Category::Mempool));
    assert!(!log_enabled(Level::Debug, Category::P2p));
    // Trace also needs the global level at trace.
    assert!(!log_enabled(Level::Trace, Category::Mempool));
    init(Level::Trace);
    update_categories(&[], &[Category::Store]);
    assert!(log_enabled(Level::Trace, Category::Mempool));
    assert!(!log_enabled(Level::Debug, Category::Store));
    assert!(!log_enabled(Level::Trace, Category::Store));
    // Info and above are never category gated.
    init(Level::Info);
    assert!(log_enabled(Level::Info, Category::Store));
    init(Level::Warn);
    assert!(!log_enabled(Level::Info, Category::Store));
    init(Level::Info);
}

/// `api_call` is an existing log site (`api:` at TRACE). Its line reaches
/// the debug log only while `api` is on.
#[test]
fn api_call_line_follows_the_api_category() {
    let _g = lock_categories();
    let path = std::env::temp_dir().join(format!("rbtc-log-category-{}", std::process::id()));
    let _ = std::fs::remove_file(&path);
    reset_debug_log();
    std::env::set_var("RBITCOIN_DEBUG_LOG", &path);
    init(Level::Trace);
    update_categories(&[], &[Category::Api]);
    api_call("rpc", "local", "marker_off", "[]", 1, None);
    update_categories(&[Category::Api], &[]);
    api_call("rpc", "local", "marker_on", "[]", 1, None);
    update_categories(&[], &[Category::Api]);
    api_call("rpc", "local", "marker_off_again", "[]", 1, None);
    let text = std::fs::read_to_string(&path).unwrap_or_default();
    reset_debug_log();
    std::env::remove_var("RBITCOIN_DEBUG_LOG");
    let _ = std::fs::remove_file(&path);
    init(Level::Info);
    assert!(text.contains("marker_on"), "{text:?}");
    assert!(!text.contains("marker_off"), "{text:?}");
}

#[test]
fn categorized_macros_write_only_when_enabled() {
    let _g = lock_categories();
    let path = std::env::temp_dir().join(format!("rbtc-log-macro-{}", std::process::id()));
    let _ = std::fs::remove_file(&path);
    reset_debug_log();
    std::env::set_var("RBITCOIN_DEBUG_LOG", &path);
    init(Level::Info);
    debug!(Tor, "marker hidden");
    update_categories(&[Category::Tor], &[]);
    debug!(Tor, "marker shown {}", 7);
    trace!(Tor, "trace hidden");
    let text = std::fs::read_to_string(&path).unwrap_or_default();
    reset_debug_log();
    std::env::remove_var("RBITCOIN_DEBUG_LOG");
    let _ = std::fs::remove_file(&path);
    init(Level::Info);
    // The category is the prefix; the call site does not repeat it.
    assert!(text.contains("DEBUG tor: marker shown 7"), "{text:?}");
    assert!(!text.contains("hidden"), "{text:?}");
}

#[test]
fn captured_categorized_line_carries_the_category_prefix() {
    let _g = lock_categories();
    init(Level::Info);
    capture_logs(true);
    debug!(Mempool, "removed {} confirmed tx(s)", 3);
    let logs = take_logs();
    capture_logs(false);
    assert_eq!(
        logs,
        [(
            Level::Debug,
            "mempool: removed 3 confirmed tx(s)".to_string()
        )]
    );
}
