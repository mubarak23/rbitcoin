//! `logging (include) (exclude)`: read or change the debug categories.

use super::{json_type_name, rpc_error, RpcParams, ERR_INVALID_PARAMETER, ERR_TYPE_ERROR};
use rbitcoin_log::Category;
use serde_json::{Map, Value};

/// Core `logging`. Every name in both lists is checked before anything
/// changes, so an unknown name leaves the set as it was (Core applies
/// `include` before it reads `exclude`). Then `include` goes on and
/// `exclude` goes off in one step; a name in both ends up off.
pub fn logging(params: &RpcParams) -> Result<Value, Value> {
    params.reject_unknown(&["include", "exclude"])?;
    let include = categories(params.get(0, "include"))?;
    let exclude = categories(params.get(1, "exclude"))?;
    let map = rbitcoin_log::update_categories(&include, &exclude);
    let obj: Map<String, Value> = map
        .into_iter()
        .map(|(c, on)| (c.as_str().to_string(), Value::Bool(on)))
        .collect();
    Ok(Value::Object(obj))
}

/// `""`, `1`, and `all` are every category, as in Core `GetLogCategory`.
/// Core has no `none` / `0` selector here (`-debug=none` is a startup flag),
/// so those are unknown names.
fn categories(v: Option<&Value>) -> Result<Vec<Category>, Value> {
    let items = match v {
        None | Some(Value::Null) => return Ok(Vec::new()),
        Some(Value::Array(a)) => a,
        Some(other) => return Err(type_error(other, "array")),
    };
    let mut out = Vec::new();
    for item in items {
        let name = item.as_str().ok_or_else(|| type_error(item, "string"))?;
        match name {
            "" | "1" | "all" => out.extend(Category::ALL),
            _ => out.push(Category::parse(name).ok_or_else(|| {
                rpc_error(
                    ERR_INVALID_PARAMETER,
                    format!("unknown logging category {name}"),
                )
            })?),
        }
    }
    Ok(out)
}

fn type_error(v: &Value, want: &str) -> Value {
    rpc_error(
        ERR_TYPE_ERROR,
        format!(
            "JSON value of type {} is not of expected type {want}",
            json_type_name(v)
        ),
    )
}
