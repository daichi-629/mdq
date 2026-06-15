use std::collections::BTreeMap;

use anyhow::{Context, Result};
use serde_json::{Map, Value};

use crate::core::{QueryAdapter, QueryContext, RecordSet, Row};

use super::expr::{Expr, value_order};
use super::page_value;

pub struct BaseAdapter;

impl QueryAdapter for BaseAdapter {
    fn name(&self) -> &'static str {
        "base"
    }

    fn execute(&self, context: &QueryContext<'_>, source: &str) -> Result<RecordSet> {
        let yaml: serde_yaml::Value =
            serde_yaml::from_str(source).context("invalid Obsidian Base YAML")?;
        let document = serde_json::to_value(yaml)?;
        let global_filter = document.get("filters");
        let view = document
            .get("views")
            .and_then(Value::as_array)
            .and_then(|views| views.first());
        let view_filter = view.and_then(|view| view.get("filters"));
        let formulas = document
            .get("formulas")
            .and_then(Value::as_object)
            .cloned()
            .unwrap_or_default();
        let order = view
            .and_then(|view| view.get("order"))
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();

        let mut values = Vec::new();
        let this_file = context
            .current_file
            .as_ref()
            .and_then(|path| path.strip_prefix(context.vault).ok())
            .map(|path| path.to_string_lossy().replace('\\', "/"));
        for page in context.database.all_pages()? {
            let mut value = page_value(&page);
            if let Some(this_file) = &this_file {
                let name = this_file
                    .rsplit('/')
                    .next()
                    .unwrap_or(this_file)
                    .trim_end_matches(".md");
                value.as_object_mut().unwrap().insert(
                    "this".to_owned(),
                    serde_json::json!({
                        "file": {
                            "path": this_file,
                            "name": name,
                            "link": this_file
                        }
                    }),
                );
            }
            if !matches_base_filter(global_filter, &value)?
                || !matches_base_filter(view_filter, &value)?
            {
                continue;
            }
            for _ in 0..formulas.len().max(1) {
                for (name, source) in &formulas {
                    let Some(source) = source.as_str() else {
                        continue;
                    };
                    let evaluated = Expr::parse(source)
                        .map(|expr| expr.eval(&value))
                        .unwrap_or(Value::Null);
                    value
                        .as_object_mut()
                        .unwrap()
                        .entry("formula")
                        .or_insert_with(|| Value::Object(Map::new()))
                        .as_object_mut()
                        .unwrap()
                        .insert(name.clone(), evaluated);
                }
            }
            values.push(value);
        }

        if let Some(sort) = view
            .and_then(|view| view.get("sort"))
            .and_then(Value::as_array)
        {
            for sort in sort.iter().rev() {
                let property = sort
                    .get("property")
                    .or_else(|| sort.get("column"))
                    .and_then(Value::as_str)
                    .unwrap_or("");
                let descending = sort
                    .get("direction")
                    .and_then(Value::as_str)
                    .is_some_and(|direction| direction.eq_ignore_ascii_case("desc"));
                values.sort_by(|left, right| {
                    let left = field(left, property);
                    let right = field(right, property);
                    let ordering = value_order(left, right).unwrap_or(std::cmp::Ordering::Equal);
                    if descending {
                        ordering.reverse()
                    } else {
                        ordering
                    }
                });
            }
        }

        let rows = values
            .into_iter()
            .map(|value| project(value, &order))
            .collect();
        Ok(RecordSet::new("base", rows))
    }
}

fn matches_base_filter(filter: Option<&Value>, row: &Value) -> Result<bool> {
    let Some(filter) = filter else {
        return Ok(true);
    };
    match filter {
        Value::String(source) => Ok(Expr::parse(source)?.test(row)),
        Value::Array(filters) => all_filters(filters, row),
        Value::Object(object) if object.contains_key("and") => all_filters(
            object["and"]
                .as_array()
                .context("Base and filter must be an array")?,
            row,
        ),
        Value::Object(object) if object.contains_key("or") => any_filters(
            object["or"]
                .as_array()
                .context("Base or filter must be an array")?,
            row,
        ),
        Value::Object(object) if object.contains_key("not") => {
            Ok(!matches_base_filter(object.get("not"), row)?)
        }
        _ => Ok(true),
    }
}

fn all_filters(filters: &[Value], row: &Value) -> Result<bool> {
    for filter in filters {
        if !matches_base_filter(Some(filter), row)? {
            return Ok(false);
        }
    }
    Ok(true)
}

fn any_filters(filters: &[Value], row: &Value) -> Result<bool> {
    for filter in filters {
        if matches_base_filter(Some(filter), row)? {
            return Ok(true);
        }
    }
    Ok(false)
}

fn field<'a>(value: &'a Value, path: &str) -> &'a Value {
    let mut current = value;
    for part in path.split('.') {
        let Some(next) = current.get(part) else {
            return &Value::Null;
        };
        current = next;
    }
    current
}

fn project(value: Value, order: &[Value]) -> Row {
    if order.is_empty() {
        return value.as_object().unwrap().clone().into_iter().collect();
    }
    let mut row = BTreeMap::new();
    for property in order.iter().filter_map(Value::as_str) {
        row.insert(property.to_owned(), field(&value, property).clone());
    }
    row
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn supports_nested_boolean_base_filters() {
        let filter = json!({"and": ["file.ext = 'md'", {"or": ["score >= 3", "active = true"]}]});
        assert!(
            matches_base_filter(
                Some(&filter),
                &json!({
                    "file": {"ext": "md"},
                    "score": 3
                })
            )
            .unwrap()
        );
    }
}
