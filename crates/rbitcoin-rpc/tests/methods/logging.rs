use super::ctx_empty;
use rbitcoin_log::{Category, Level};
use rbitcoin_rpc::methods::*;
use serde_json::{json, Value};
use std::sync::{Mutex, MutexGuard, OnceLock};

/// The category set is process-wide; serialize the tests that change it.
fn lock_categories() -> MutexGuard<'static, ()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    let g = LOCK
        .get_or_init(|| Mutex::new(()))
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    rbitcoin_log::init(Level::Info);
    g
}

fn all_names() -> Vec<&'static str> {
    Category::ALL.iter().map(|c| c.as_str()).collect()
}

fn on_names(map: &Value) -> Vec<String> {
    map.as_object()
        .unwrap()
        .iter()
        .filter(|(_, v)| v.as_bool().unwrap())
        .map(|(k, _)| k.clone())
        .collect()
}

#[test]
fn logging_no_args_returns_every_category_as_bool() {
    let _g = lock_categories();
    let (ctx, _dir) = ctx_empty();
    let map = dispatch(&ctx, "logging", vec![]).unwrap();
    let obj = map.as_object().unwrap();
    let keys: Vec<_> = obj.keys().map(String::as_str).collect();
    assert_eq!(keys, all_names());
    assert!(obj.values().all(|v| v == &json!(false)), "{map}");
    rbitcoin_log::init(Level::Debug);
    let map = dispatch(&ctx, "logging", vec![]).unwrap();
    assert_eq!(on_names(&map), all_names());
    rbitcoin_log::init(Level::Info);
}

#[test]
fn logging_all_then_exclude_all_is_visible_in_a_later_call() {
    let _g = lock_categories();
    let (ctx, _dir) = ctx_empty();
    let map = dispatch(&ctx, "logging", vec![json!(["all"])]).unwrap();
    assert_eq!(on_names(&map), all_names());
    assert_eq!(dispatch(&ctx, "logging", vec![]).unwrap(), map);
    let map = dispatch(&ctx, "logging", vec![json!([]), json!(["1"])]).unwrap();
    assert!(on_names(&map).is_empty(), "{map}");
    assert_eq!(dispatch(&ctx, "logging", vec![]).unwrap(), map);
    // An empty name is every category, as in Core GetLogCategory.
    let map = dispatch(&ctx, "logging", vec![json!([""])]).unwrap();
    assert_eq!(on_names(&map), all_names());
    let map = dispatch(&ctx, "logging", vec![json!([]), json!([""])]).unwrap();
    assert!(on_names(&map).is_empty(), "{map}");
    // The RPC still answers with every category off.
    assert!(dispatch(&ctx, "getrpcinfo", vec![]).is_ok());
}

#[test]
fn logging_include_exclude_by_name_and_named_params() {
    let _g = lock_categories();
    let (ctx, _dir) = ctx_empty();
    let map = dispatch(&ctx, "logging", vec![json!(["mempool", "p2p"])]).unwrap();
    assert_eq!(on_names(&map), ["mempool", "p2p"]);
    assert!(rbitcoin_log::category_enabled(Category::Mempool));
    // A name in both lists: include, then exclude, so it ends up off.
    let map = dispatch(&ctx, "logging", vec![json!(["ibd"]), json!(["ibd", "p2p"])]).unwrap();
    assert_eq!(on_names(&map), ["mempool"]);
    // Empty arrays and nulls change nothing.
    let before = dispatch(&ctx, "logging", vec![]).unwrap();
    assert_eq!(
        dispatch(&ctx, "logging", vec![json!([]), json!([])]).unwrap(),
        before
    );
    assert_eq!(
        dispatch(&ctx, "logging", vec![Value::Null, Value::Null]).unwrap(),
        before
    );
    let named = RpcParams::named(
        json!({"exclude": ["mempool"], "include": ["tor"]})
            .as_object()
            .unwrap()
            .clone(),
    );
    let map = dispatch(&ctx, "logging", named).unwrap();
    assert_eq!(on_names(&map), ["tor"]);
    rbitcoin_log::init(Level::Info);
}

#[test]
fn logging_unknown_category_is_rejected_and_changes_nothing() {
    let _g = lock_categories();
    let (ctx, _dir) = ctx_empty();
    dispatch(&ctx, "logging", vec![json!(["store"])]).unwrap();
    let before = dispatch(&ctx, "logging", vec![]).unwrap();
    // The include half is valid; the unknown exclude name must not let it land.
    let err = dispatch(&ctx, "logging", vec![json!(["mempool"]), json!(["bogus"])]).unwrap_err();
    assert_eq!(err["code"], json!(ERR_INVALID_PARAMETER));
    assert_eq!(err["message"], json!("unknown logging category bogus"));
    let err = dispatch(&ctx, "logging", vec![json!(["Mempool"])]).unwrap_err();
    assert_eq!(err["message"], json!("unknown logging category Mempool"));
    // Core GetLogCategory has no `none` / `0` selector; `-debug=none` is a
    // startup flag only.
    for name in ["none", "0"] {
        let err = dispatch(&ctx, "logging", vec![json!([name])]).unwrap_err();
        assert_eq!(err["code"], json!(ERR_INVALID_PARAMETER));
        assert_eq!(
            err["message"],
            json!(format!("unknown logging category {name}"))
        );
        let err = dispatch(&ctx, "logging", vec![json!([]), json!([name])]).unwrap_err();
        assert_eq!(err["code"], json!(ERR_INVALID_PARAMETER));
    }
    assert_eq!(dispatch(&ctx, "logging", vec![]).unwrap(), before);
    rbitcoin_log::init(Level::Info);
}

#[test]
fn logging_rejects_wrong_types_and_unknown_named_params() {
    let _g = lock_categories();
    let (ctx, _dir) = ctx_empty();
    let err = dispatch(&ctx, "logging", vec![json!("mempool")]).unwrap_err();
    assert_eq!(err["code"], json!(ERR_TYPE_ERROR));
    assert_eq!(
        err["message"],
        json!("JSON value of type string is not of expected type array")
    );
    let err = dispatch(&ctx, "logging", vec![json!([1])]).unwrap_err();
    assert_eq!(err["code"], json!(ERR_TYPE_ERROR));
    assert_eq!(
        err["message"],
        json!("JSON value of type number is not of expected type string")
    );
    let named = RpcParams::named(json!({"verbose": true}).as_object().unwrap().clone());
    let err = dispatch(&ctx, "logging", named).unwrap_err();
    assert_eq!(err["code"], json!(ERR_INVALID_PARAMETER));
    assert!(on_names(&dispatch(&ctx, "logging", vec![]).unwrap()).is_empty());
}

#[test]
fn logging_is_listed_and_help_names_every_category() {
    assert!(METHOD_LIST.contains(&"logging"));
    assert!(!rpc_call_reads_store("logging"));
    let help = method_help("logging");
    for name in all_names() {
        assert!(
            help.contains(&format!("\"{name}\"")),
            "{name} missing: {help}"
        );
    }
}
