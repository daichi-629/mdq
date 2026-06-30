use std::cmp::Ordering;
use std::collections::BTreeMap;

use anyhow::{Context, Result};
use serde_json::{Map, Value, json};

use crate::core::{QueryAdapter, QueryContext, RecordSet, Row};

use super::expr::{Expr, as_datetime, value_order};
use super::{LinkIndex, page_value};

pub struct BaseAdapter;

impl QueryAdapter for BaseAdapter {
    fn name(&self) -> &'static str {
        "base"
    }

    fn execute(&self, context: &QueryContext<'_>, source: &str) -> Result<RecordSet> {
        let yaml: serde_yaml::Value =
            serde_yaml::from_str(source).context("invalid Obsidian Base YAML")?;
        if !matches!(
            yaml,
            serde_yaml::Value::Mapping(_) | serde_yaml::Value::Null
        ) {
            anyhow::bail!("invalid Obsidian Base YAML: expected a mapping document, got a scalar");
        }
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
        let custom_summaries = document
            .get("summaries")
            .and_then(Value::as_object)
            .cloned()
            .unwrap_or_default();

        let mut values = Vec::new();
        let this_file = context
            .current_file
            .as_ref()
            .and_then(|path| path.strip_prefix(context.vault).ok())
            .map(|path| path.to_string_lossy().replace('\\', "/"));
        let links = LinkIndex::build(context.database)?;
        let pages = context.database.all_pages()?;
        let this_value = this_file
            .as_ref()
            .and_then(|this_file| pages.iter().find(|page| page.path == *this_file))
            .map(|page| page_value(page, &links));
        for page in &pages {
            let mut value = page_value(page, &links);
            if let Some(this_value) = &this_value {
                value
                    .as_object_mut()
                    .unwrap()
                    .insert("this".to_owned(), this_value.clone());
            }
            if !matches_base_filter(global_filter, &value)?
                || !matches_base_filter(view_filter, &value)?
            {
                continue;
            }
            // Run multiple passes so formulas can reference each other.
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
                    let ordering = value_order(left, right).unwrap_or(Ordering::Equal);
                    if descending {
                        ordering.reverse()
                    } else {
                        ordering
                    }
                });
            }
        }

        if let Some(limit) = view.and_then(|v| v.get("limit")).and_then(Value::as_u64) {
            values.truncate(limit as usize);
        }

        let view_summaries = view
            .and_then(|v| v.get("summaries"))
            .and_then(Value::as_object)
            .cloned()
            .unwrap_or_default();
        let mut computed_summaries: Row = Row::new();
        for (property, summary_name) in &view_summaries {
            let col_values: Vec<&Value> = values.iter().map(|row| field(row, property)).collect();
            let result = compute_summary(
                summary_name.as_str().unwrap_or(""),
                &col_values,
                &custom_summaries,
            );
            computed_summaries.insert(property.clone(), result);
        }

        let group_by = view
            .and_then(|v| v.get("groupBy"))
            .and_then(Value::as_object);

        let rows: Vec<Row> = if let Some(group_by) = group_by {
            let property = group_by
                .get("property")
                .and_then(Value::as_str)
                .unwrap_or("");
            let descending = group_by
                .get("direction")
                .and_then(Value::as_str)
                .is_some_and(|d| d.eq_ignore_ascii_case("desc"));
            let mut groups: BTreeMap<String, Vec<Value>> = BTreeMap::new();
            for value in &values {
                let raw = field(value, property);
                let key = raw
                    .as_str()
                    .map(str::to_owned)
                    .unwrap_or_else(|| raw.to_string());
                groups.entry(key).or_default().push(value.clone());
            }
            // BTreeMap iterates in ascending key order; reverse if descending.
            let mut group_entries: Vec<(String, Vec<Value>)> = groups.into_iter().collect();
            if descending {
                group_entries.reverse();
            }
            group_entries
                .into_iter()
                .map(|(key, group_rows)| {
                    let projected: Vec<Value> = group_rows
                        .into_iter()
                        .map(|v| Value::Object(project(v, &order).into_iter().collect()))
                        .collect();
                    let mut row = Row::new();
                    row.insert("key".to_owned(), Value::String(key));
                    row.insert("rows".to_owned(), Value::Array(projected));
                    row
                })
                .collect()
        } else {
            values
                .into_iter()
                .map(|value| project(value, &order))
                .collect()
        };

        let mut result = RecordSet::new("base", rows);
        result.summaries = computed_summaries;
        Ok(result)
    }
}

fn compute_summary(
    name: &str,
    values: &[&Value],
    custom_summaries: &serde_json::Map<String, Value>,
) -> Value {
    match name {
        "Count" => json!(values.len()),
        "Sum" => {
            let sum: f64 = values.iter().filter_map(|v| v.as_f64()).sum();
            json!(sum)
        }
        "Average" => {
            let nums: Vec<f64> = values.iter().filter_map(|v| v.as_f64()).collect();
            if nums.is_empty() {
                Value::Null
            } else {
                json!(nums.iter().sum::<f64>() / nums.len() as f64)
            }
        }
        "Min" => values
            .iter()
            .copied()
            .filter(|v| !v.is_null())
            .min_by(|a, b| value_order(a, b).unwrap_or(Ordering::Equal))
            .cloned()
            .unwrap_or(Value::Null),
        "Max" => values
            .iter()
            .copied()
            .filter(|v| !v.is_null())
            .max_by(|a, b| value_order(a, b).unwrap_or(Ordering::Equal))
            .cloned()
            .unwrap_or(Value::Null),
        "Range" => {
            let mut non_null: Vec<&Value> =
                values.iter().copied().filter(|v| !v.is_null()).collect();
            non_null.sort_by(|a, b| value_order(a, b).unwrap_or(Ordering::Equal));
            if let (Some(min), Some(max)) = (
                non_null.first().and_then(|v| as_datetime(v)),
                non_null.last().and_then(|v| as_datetime(v)),
            ) {
                return json!(max.signed_duration_since(min).num_milliseconds() as f64);
            }
            match (
                non_null.first().and_then(|v| v.as_f64()),
                non_null.last().and_then(|v| v.as_f64()),
            ) {
                (Some(min), Some(max)) => json!(max - min),
                _ => Value::Null,
            }
        }
        "Median" => {
            let mut nums: Vec<f64> = values.iter().filter_map(|v| v.as_f64()).collect();
            if nums.is_empty() {
                return Value::Null;
            }
            nums.sort_by(|a, b| a.partial_cmp(b).unwrap_or(Ordering::Equal));
            let mid = nums.len() / 2;
            if nums.len() % 2 == 0 {
                json!((nums[mid - 1] + nums[mid]) / 2.0)
            } else {
                json!(nums[mid])
            }
        }
        "Stddev" => {
            let nums: Vec<f64> = values.iter().filter_map(|v| v.as_f64()).collect();
            if nums.is_empty() {
                return Value::Null;
            }
            let mean = nums.iter().sum::<f64>() / nums.len() as f64;
            let variance = nums.iter().map(|x| (x - mean).powi(2)).sum::<f64>() / nums.len() as f64;
            json!(variance.sqrt())
        }
        "Earliest" => values
            .iter()
            .copied()
            .filter(|v| as_datetime(v).is_some())
            .min_by(|a, b| value_order(a, b).unwrap_or(Ordering::Equal))
            .cloned()
            .unwrap_or(Value::Null),
        "Latest" => values
            .iter()
            .copied()
            .filter(|v| as_datetime(v).is_some())
            .max_by(|a, b| value_order(a, b).unwrap_or(Ordering::Equal))
            .cloned()
            .unwrap_or(Value::Null),
        "Checked" => json!(values.iter().filter(|v| v.as_bool() == Some(true)).count()),
        "Unchecked" => json!(values.iter().filter(|v| v.as_bool() == Some(false)).count()),
        "Empty" => json!(
            values
                .iter()
                .filter(|v| v.is_null() || v.as_str() == Some(""))
                .count()
        ),
        "Filled" => json!(
            values
                .iter()
                .filter(|v| !v.is_null() && v.as_str() != Some(""))
                .count()
        ),
        "Unique" => {
            use std::collections::HashSet;
            let unique: HashSet<String> = values.iter().map(|v| v.to_string()).collect();
            json!(unique.len())
        }
        custom_name => {
            if let Some(formula) = custom_summaries.get(custom_name).and_then(Value::as_str) {
                let values_arr = Value::Array(values.iter().map(|v| (*v).clone()).collect());
                let ctx = json!({"values": values_arr});
                Expr::parse(formula)
                    .map(|expr| expr.eval(&ctx))
                    .unwrap_or(Value::Null)
            } else {
                Value::Null
            }
        }
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
        Value::Object(object) if object.contains_key("not") => match object.get("not") {
            Some(Value::Array(filters)) => Ok(!any_filters(filters, row)?),
            filter => Ok(!matches_base_filter(filter, row)?),
        },
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

    #[test]
    fn compute_summary_count() {
        let vals = vec![json!(1), json!(2), json!(3)];
        let refs: Vec<&Value> = vals.iter().collect();
        let result = compute_summary("Count", &refs, &Default::default());
        assert_eq!(result, json!(3));
    }

    #[test]
    fn compute_summary_sum_average() {
        let vals = vec![json!(10.0), json!(20.0), json!(30.0)];
        let refs: Vec<&Value> = vals.iter().collect();
        assert_eq!(
            compute_summary("Sum", &refs, &Default::default()),
            json!(60.0)
        );
        assert_eq!(
            compute_summary("Average", &refs, &Default::default()),
            json!(20.0)
        );
    }

    #[test]
    fn compute_summary_min_max() {
        let vals = vec![json!(5), json!(1), json!(9)];
        let refs: Vec<&Value> = vals.iter().collect();
        assert_eq!(compute_summary("Min", &refs, &Default::default()), json!(1));
        assert_eq!(compute_summary("Max", &refs, &Default::default()), json!(9));
    }

    #[test]
    fn compute_summary_median_even() {
        let vals = vec![json!(1.0), json!(3.0), json!(5.0), json!(7.0)];
        let refs: Vec<&Value> = vals.iter().collect();
        assert_eq!(
            compute_summary("Median", &refs, &Default::default()),
            json!(4.0)
        );
    }

    #[test]
    fn compute_summary_unique_filled_empty() {
        let vals = vec![json!("a"), json!("b"), json!("a"), Value::Null];
        let refs: Vec<&Value> = vals.iter().collect();
        assert_eq!(
            compute_summary("Unique", &refs, &Default::default()),
            json!(3)
        );
        assert_eq!(
            compute_summary("Filled", &refs, &Default::default()),
            json!(3)
        );
        assert_eq!(
            compute_summary("Empty", &refs, &Default::default()),
            json!(1)
        );
    }

    #[test]
    fn compute_summary_checked_unchecked() {
        let vals = vec![json!(true), json!(false), json!(true), Value::Null];
        let refs: Vec<&Value> = vals.iter().collect();
        assert_eq!(
            compute_summary("Checked", &refs, &Default::default()),
            json!(2)
        );
        assert_eq!(
            compute_summary("Unchecked", &refs, &Default::default()),
            json!(1)
        );
    }

    #[test]
    fn compute_summary_range() {
        let vals = vec![json!(2.0), json!(8.0), json!(5.0)];
        let refs: Vec<&Value> = vals.iter().collect();
        assert_eq!(
            compute_summary("Range", &refs, &Default::default()),
            json!(6.0)
        );
    }

    #[test]
    fn compute_summary_stddev() {
        let vals = vec![
            json!(2.0),
            json!(4.0),
            json!(4.0),
            json!(4.0),
            json!(5.0),
            json!(5.0),
            json!(7.0),
            json!(9.0),
        ];
        let refs: Vec<&Value> = vals.iter().collect();
        let result = compute_summary("Stddev", &refs, &Default::default());
        let stddev = result.as_f64().unwrap();
        assert!(
            (stddev - 2.0).abs() < 1e-10,
            "expected stddev ~2.0, got {stddev}"
        );
    }

    #[test]
    fn scalar_base_yaml_is_rejected() {
        use crate::core::QueryContext;
        use crate::db::Database;
        use std::path::PathBuf;
        let dir = tempfile::tempdir().unwrap();
        let db = Database::open(&dir.path().join("i.sqlite3")).unwrap();
        let vault = PathBuf::from("/tmp");
        let context = QueryContext {
            database: &db,
            vault: &vault,
            current_file: None,
        };
        let result = BaseAdapter.execute(&context, "INVALID QUERY SYNTAX");
        assert!(result.is_err(), "scalar YAML must be rejected");
        let msg = result.unwrap_err().to_string();
        assert!(
            msg.contains("mapping"),
            "error must mention expected type: {msg}"
        );
    }
}
