//! okcode-embed — port of src/identity.js and src/embedders.js.
pub mod embedders;
pub mod identity;

use serde_json::Value;

pub fn golden_call(name: &str, args: &[Value]) -> Result<Value, String> {
    let a = |i: usize| args.get(i).cloned().unwrap_or(Value::Null);
    match name.strip_prefix("identity.").unwrap_or(name) {
        "endpointOf" => Ok(Value::String(identity::endpoint_of(&a(0)))),
        "partsOf" => Ok(identity::parts_of(&a(0)).to_json()),
        "identityOf" => Ok(identity::identity_of(&identity::Parts::from_json(&a(0)), &a(1)).map_or(Value::Null, Value::String)),
        "shortHash" => Ok(Value::String(identity::short_hash(&identity::js_string(&a(0))))),
        "pipelineName" => identity::pipeline_name(&identity::Parts::from_json(&a(0)), &a(1)).map(Value::String),
        "isOurs" => Ok(Value::Bool(identity::is_ours(&a(0)))),
        "findPipeline" => {
            let names: Vec<String> = a(0).as_array().map(|v| v.iter().map(identity::js_string).collect()).unwrap_or_default();
            Ok(identity::find_pipeline(&names, &identity::Parts::from_json(&a(1)), &a(2))
                .map_or(Value::Null, |(p, d)| serde_json::json!({"pipeline": p, "dims": d})))
        }
        other => Err(format!("unknown fn {other}")),
    }
}
