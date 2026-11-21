//! A command's parameters as a form ([`Form`]), made from its JSON schema (draft 2020-12, inlined, as the registry
//! lists it): a checkbox for a boolean, a number field within the schema's range, a list for a fixed choice of values
//! (`oneOf` constants or `enum`), a text field for a string, and the fields of a nested object, indented. A field that
//! may be left out (not required) has a checkbox that puts it in or takes it out, starting from its default. Anything
//! else (lists, choices between objects, maps) is a small JSON box of its own. The form edits the parameters' JSON
//! value in place, and changes nothing that isn't touched.

use std::collections::HashMap;

use florastack_engine::Value;
use serde_json::Map;

/// What a schema is drawn as.
#[derive(Debug, PartialEq)]
enum Kind {
    Bool,
    Integer {
        min: f64,
        max: f64,
    },
    Number {
        min: f64,
        max: f64,
    },
    Text,
    /// The values allowed, with their descriptions.
    Choice(Vec<(Value, String)>),
    Object,
    /// Anything else, as JSON.
    Json,
}

/// The schema without its `null`: `type: [T, "null"]` as `type: T`, and `anyOf: [X, {type: null}]` as `X`.
fn non_null(schema: &Value) -> Value {
    if let Some(any) = schema["anyOf"].as_array() {
        let rest: Vec<&Value> = any.iter().filter(|s| s["type"] != "null").collect();
        if let [only] = rest[..] {
            let mut merged = only.clone();
            if let (Some(merged), Some(description)) = (merged.as_object_mut(), schema.get("description")) {
                merged.entry("description").or_insert_with(|| description.clone());
            }
            return merged;
        }
    }
    let mut schema = schema.clone();
    if let Some(types) = schema["type"].as_array() {
        let rest: Vec<Value> = types.iter().filter(|t| *t != "null").cloned().collect();
        if let [only] = &rest[..] {
            schema["type"] = only.clone();
        }
    }
    schema
}

fn kind(schema: &Value) -> Kind {
    let bound = |key: &str, fallback: f64| schema[key].as_f64().unwrap_or(fallback);
    match schema["type"].as_str() {
        Some("boolean") => return Kind::Bool,
        Some("integer") => return Kind::Integer { min: bound("minimum", -1e15), max: bound("maximum", 1e15) },
        Some("number") => return Kind::Number { min: bound("minimum", f64::MIN), max: bound("maximum", f64::MAX) },
        Some("object") if schema["properties"].is_object() => return Kind::Object,
        _ => {}
    }
    let described = |s: &Value| s["description"].as_str().map(|d| d.lines().next().unwrap_or_default().to_owned()).unwrap_or_default();
    if let Some(values) = schema["enum"].as_array() {
        return Kind::Choice(values.iter().map(|v| (v.clone(), String::new())).collect());
    }
    if let Some(options) = schema["oneOf"].as_array() {
        let mut choices = Vec::new();
        for option in options {
            if let Some(value) = option.get("const") {
                choices.push((value.clone(), described(option)));
            } else if let Some(values) = option["enum"].as_array() {
                choices.extend(values.iter().map(|v| (v.clone(), described(option))));
            } else {
                return Kind::Json;
            }
        }
        return Kind::Choice(choices);
    }
    if schema["type"] == "string" {
        return Kind::Text;
    }
    Kind::Json
}

/// A value for `schema` when a field is put in: its default, or the simplest value of its kind.
fn default_for(schema: &Value) -> Value {
    if let Some(default) = schema.get("default") {
        return default.clone();
    }
    let schema = non_null(schema);
    if let Some(default) = schema.get("default") {
        return default.clone();
    }
    match kind(&schema) {
        Kind::Bool => Value::Bool(false),
        #[allow(clippy::cast_possible_truncation, reason = "a schema's minimum, which is an integer's")]
        Kind::Integer { min, .. } => Value::from(min.max(0.0) as i64),
        Kind::Number { min, .. } => serde_json::Number::from_f64(min.max(0.0)).map_or(Value::Null, Value::Number),
        Kind::Text => Value::String(String::new()),
        Kind::Choice(choices) => choices.first().map_or(Value::Null, |(v, _)| v.clone()),
        Kind::Object => {
            let mut object = Map::new();
            for name in required(&schema) {
                object.insert(name.to_owned(), default_for(&schema["properties"][name]));
            }
            Value::Object(object)
        }
        Kind::Json => match schema["type"].as_str() {
            Some("array") => Value::Array(Vec::new()),
            Some("object") => Value::Object(Map::new()),
            _ => Value::Null,
        },
    }
}

fn required(schema: &Value) -> Vec<&str> {
    schema["required"].as_array().into_iter().flatten().filter_map(Value::as_str).collect()
}

/// A JSON box's text, and why it doesn't parse.
struct Text {
    text: String,
    error: Option<String>,
}

/// See the module documentation.
pub struct Form {
    pub params: Value,
    texts: HashMap<String, Text>,
}

impl Form {
    pub fn new(params: Value) -> Form {
        Form { params, texts: HashMap::new() }
    }

    /// Whether every JSON box parses.
    pub fn is_valid(&self) -> bool {
        self.texts.values().all(|t| t.error.is_none())
    }

    /// Draws the form for `schema`, the command's parameters.
    pub fn ui(&mut self, ui: &mut egui::Ui, schema: &Value) {
        if !self.params.is_object() {
            self.params = Value::Object(Map::new());
        }
        let Form { params, texts } = self;
        if let Some(object) = params.as_object_mut() {
            fields(ui, schema, object, "", texts);
        }
    }
}

/// The fields of an object schema, editing `object`.
fn fields(ui: &mut egui::Ui, schema: &Value, object: &mut Map<String, Value>, path: &str, texts: &mut HashMap<String, Text>) {
    let Some(properties) = schema["properties"].as_object() else { return };
    let required = required(schema);
    egui::Grid::new(("palette-form", path)).num_columns(2).spacing([12.0, 6.0]).show(ui, |ui| {
        for (name, property) in properties {
            let path = format!("{path}/{name}");
            let description = non_null(property)["description"].as_str().unwrap_or_default().to_owned();
            let present = object.contains_key(name);
            // The row's label names its widget for screen readers.
            let label = if required.contains(&name.as_str()) {
                let label = ui.label(name.as_str());
                if !description.is_empty() {
                    label.clone().on_hover_text(&description);
                }
                label.id
            } else {
                let mut on = present;
                let check = ui.checkbox(&mut on, name.as_str());
                let check = if description.is_empty() { check } else { check.on_hover_text(&description) };
                if check.changed() {
                    if on {
                        object.insert(name.clone(), default_for(property));
                    } else {
                        object.remove(name);
                        texts.retain(|k, _| !k.starts_with(&path));
                    }
                }
                check.id
            };
            match object.get_mut(name) {
                Some(value) => widget(ui, property, value, (&path, label), texts),
                None if required.contains(&name.as_str()) => {
                    let mut value = default_for(property);
                    widget(ui, property, &mut value, (&path, label), texts);
                    object.insert(name.clone(), value);
                }
                None => {
                    ui.weak("—");
                }
            }
            ui.end_row();
        }
    });
}

/// The widget for one value of `schema`, editing `value` when it is changed.
/// `label` names it for screen readers.
fn widget(ui: &mut egui::Ui, schema: &Value, value: &mut Value, (path, label): (&str, egui::Id), texts: &mut HashMap<String, Text>) {
    let schema = non_null(schema);
    match kind(&schema) {
        Kind::Bool => {
            let mut on = value.as_bool().unwrap_or(false);
            if ui.checkbox(&mut on, "").labelled_by(label).changed() {
                *value = Value::Bool(on);
            }
        }
        Kind::Integer { min, max } => {
            let mut v = value.as_f64().unwrap_or(min.max(0.0));
            if ui.add(egui::DragValue::new(&mut v).range(min..=max).speed(1.0).fixed_decimals(0)).labelled_by(label).changed() {
                #[allow(clippy::cast_possible_truncation, reason = "within the schema's range, which is an integer's")]
                let rounded = v.round() as i64;
                *value = Value::from(rounded);
            }
        }
        Kind::Number { min, max } => {
            let mut v = value.as_f64().unwrap_or(min.max(0.0));
            let speed = (v.abs() * 0.01).max(0.01);
            if ui.add(egui::DragValue::new(&mut v).range(min..=max).speed(speed).max_decimals(4)).labelled_by(label).changed()
                && let Some(number) = serde_json::Number::from_f64(v)
            {
                *value = Value::Number(number);
            }
        }
        Kind::Text => {
            let mut text = value.as_str().unwrap_or_default().to_owned();
            if ui.add(egui::TextEdit::singleline(&mut text).desired_width(280.0)).labelled_by(label).changed() {
                *value = Value::String(text);
            }
        }
        Kind::Choice(choices) => {
            let name = |v: &Value| v.as_str().map_or_else(|| v.to_string(), str::to_owned);
            let mut picked = None;
            let combo = egui::ComboBox::from_id_salt(("palette-choice", path)).selected_text(name(value)).show_ui(ui, |ui| {
                for (choice, description) in &choices {
                    let option = ui.selectable_label(choice == value, name(choice));
                    let option = if description.is_empty() { option } else { option.on_hover_text(description) };
                    if option.clicked() {
                        picked = Some(choice.clone());
                    }
                }
            });
            let _ = combo.response.labelled_by(label);
            if let Some(choice) = picked {
                *value = choice;
            }
        }
        Kind::Object => {
            if !value.is_object() {
                *value = Value::Object(Map::new());
            }
            if let Some(object) = value.as_object_mut() {
                ui.vertical(|ui| fields(ui, &schema, object, path, texts));
                let _ = label;
            }
        }
        Kind::Json => {
            let entry = texts.entry(path.to_owned()).or_insert_with(|| Text { text: value.to_string(), error: None });
            ui.vertical(|ui| {
                let edit = egui::TextEdit::multiline(&mut entry.text).code_editor().desired_rows(1).desired_width(280.0);
                if ui.add(edit).labelled_by(label).changed() {
                    match serde_json::from_str::<Value>(&entry.text) {
                        Ok(parsed) => {
                            *value = parsed;
                            entry.error = None;
                        }
                        Err(e) => entry.error = Some(e.to_string()),
                    }
                }
                if let Some(error) = &entry.error {
                    ui.colored_label(ui.visuals().error_fg_color, error);
                }
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use florastack_engine::json;

    #[test]
    fn schemas_are_read_as_their_widgets() {
        assert_eq!(kind(&json!({ "type": "boolean" })), Kind::Bool);
        assert_eq!(kind(&json!({ "type": "integer", "minimum": 1, "maximum": 9 })), Kind::Integer { min: 1.0, max: 9.0 });
        assert_eq!(kind(&non_null(&json!({ "type": ["number", "null"], "minimum": 0 }))), Kind::Number { min: 0.0, max: f64::MAX });
        let choice = json!({ "anyOf": [{ "oneOf": [{ "const": "a", "description": "A." }, { "enum": ["b"] }] }, { "type": "null" }] });
        assert_eq!(kind(&non_null(&choice)), Kind::Choice(vec![(json!("a"), "A.".to_owned()), (json!("b"), String::new())]));
        assert_eq!(kind(&json!({ "oneOf": [{ "type": "object", "properties": {} }] })), Kind::Json);
        assert_eq!(kind(&json!({ "type": "array", "items": { "type": "string" } })), Kind::Json);
        let object =
            json!({ "type": "object", "properties": { "a": { "type": "integer", "minimum": 3 }, "b": { "type": "string" } }, "required": ["a"] });
        assert_eq!(default_for(&object), json!({ "a": 3 }));
        assert_eq!(default_for(&json!({ "type": "boolean", "default": true })), json!(true));
    }

    #[test]
    fn ticking_a_field_puts_in_its_default_and_unticking_takes_it_out() {
        use egui_kittest::kittest::Queryable;
        let schema = json!({
            "type": "object",
            "properties": {
                "grit": { "type": "number", "minimum": 0, "maximum": 1, "default": 0.5, "description": "How much grit." },
                "name": { "type": "string" },
            },
            "required": ["name"],
        });
        let mut harness = egui_kittest::Harness::new_ui_state(|ui, form: &mut Form| form.ui(ui, &schema), Form::new(json!({ "name": "A" })));
        harness.run();
        assert_eq!(harness.state().params, json!({ "name": "A" }));
        harness.get_by_label("grit").click();
        harness.run();
        assert_eq!(harness.state().params, json!({ "name": "A", "grit": 0.5 }));
        // The number field is named by the same label now: the checkbox is the one with its role.
        harness.get_by_role_and_label(egui::accesskit::Role::CheckBox, "grit").click();
        harness.run();
        assert_eq!(harness.state().params, json!({ "name": "A" }));
    }
}
