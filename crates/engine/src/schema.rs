//! How the registry's JSON schemas are made: JSON Schema draft 2020-12 (the dialect MCP tool definitions default to),
//! with every subschema inlined, so that a client that can't follow `$ref` (some MCP clients can't) reads each one
//! whole. None of the command types is recursive, so inlining always ends.

use schemars::JsonSchema;
use schemars::generate::SchemaSettings;
use serde_json::Value;

/// The schema of `T`, as the registry lists it.
pub fn schema_for<T: JsonSchema>() -> Value {
    let generator = SchemaSettings::draft2020_12().with(|s| s.inline_subschemas = true).into_generator();
    Value::from(generator.into_root_schema_for::<T>())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn schemas_are_draft_2020_12_and_inlined() {
        let schema = schema_for::<florastack_raw::DevelopSettings>();
        assert_eq!(schema["$schema"], "https://json-schema.org/draft/2020-12/schema");
        assert!(schema.get("$defs").is_none(), "{schema}");
        assert_eq!(schema["properties"]["white_balance"]["default"], "as-shot");
        assert_eq!(schema["additionalProperties"], false);
    }
}
