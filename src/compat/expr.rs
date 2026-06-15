use std::cmp::Ordering;

use anyhow::{Context, Result, bail};
use chrono::{Local, NaiveDate};
use pest::Parser;
use pest::iterators::Pair;
use pest_derive::Parser;
use serde_json::{Number, Value};

#[derive(Parser)]
#[grammar = "compat/expr.pest"]
struct ExprParser;

#[derive(Clone, Debug)]
pub enum Expr {
    Literal(Value),
    Field(String),
    List(Vec<Expr>),
    Call(String, Vec<Expr>),
    Method(Box<Expr>, String, Vec<Expr>),
    Not(Box<Expr>),
    Binary(Box<Expr>, Op, Box<Expr>),
}

#[derive(Clone, Copy, Debug)]
pub enum Op {
    Or,
    And,
    Eq,
    Ne,
    Gt,
    Ge,
    Lt,
    Le,
    Add,
    Sub,
}

impl Expr {
    pub fn parse(source: &str) -> Result<Self> {
        let pair = ExprParser::parse(Rule::expression, source)
            .with_context(|| format!("invalid compatibility expression: {source}"))?
            .next()
            .context("empty expression")?;
        build(pair)
    }

    pub fn eval(&self, row: &Value) -> Value {
        match self {
            Self::Literal(value) => value.clone(),
            Self::Field(path) => value_at_path(row, path).cloned().unwrap_or(Value::Null),
            Self::List(items) => Value::Array(items.iter().map(|item| item.eval(row)).collect()),
            Self::Not(expr) => Value::Bool(!truthy(&expr.eval(row))),
            Self::Binary(left, Op::Or, right) => {
                Value::Bool(truthy(&left.eval(row)) || truthy(&right.eval(row)))
            }
            Self::Binary(left, Op::And, right) => {
                Value::Bool(truthy(&left.eval(row)) && truthy(&right.eval(row)))
            }
            Self::Binary(left, Op::Add, right) => add(left.eval(row), right.eval(row)),
            Self::Binary(left, Op::Sub, right) => subtract(left.eval(row), right.eval(row)),
            Self::Binary(left, op, right) => {
                Value::Bool(compare(&left.eval(row), &right.eval(row), *op))
            }
            Self::Call(name, args) => call(name, args, row),
            Self::Method(target, name, args) => method(target.eval(row), name, args, row),
        }
    }

    pub fn test(&self, row: &Value) -> bool {
        truthy(&self.eval(row))
    }
}

fn build(pair: Pair<'_, Rule>) -> Result<Expr> {
    match pair.as_rule() {
        Rule::expression | Rule::primary => build(pair.into_inner().next().context("empty node")?),
        Rule::or_expr => fold(pair, Op::Or),
        Rule::and_expr => fold(pair, Op::And),
        Rule::unary_expr => {
            let mut negated = false;
            let mut expression = None;
            for inner in pair.into_inner() {
                if inner.as_rule() == Rule::NOT {
                    negated = !negated;
                } else {
                    expression = Some(build(inner)?);
                }
            }
            let expression = expression.context("not without expression")?;
            Ok(if negated {
                Expr::Not(Box::new(expression))
            } else {
                expression
            })
        }
        Rule::comparison => {
            let mut inner = pair.into_inner();
            let left = build(inner.next().context("missing comparison operand")?)?;
            let Some(operator) = inner.next() else {
                return Ok(left);
            };
            let right = build(inner.next().context("missing right operand")?)?;
            let op = match operator.as_str() {
                "=" | "==" => Op::Eq,
                "!=" => Op::Ne,
                ">" => Op::Gt,
                ">=" => Op::Ge,
                "<" => Op::Lt,
                "<=" => Op::Le,
                value => bail!("unsupported operator: {value}"),
            };
            Ok(Expr::Binary(Box::new(left), op, Box::new(right)))
        }
        Rule::sum => {
            let mut inner = pair.into_inner();
            let mut expression = build(inner.next().context("empty sum")?)?;
            while let Some(operator) = inner.next() {
                let right = build(inner.next().context("missing arithmetic operand")?)?;
                let op = if operator.as_str() == "+" {
                    Op::Add
                } else {
                    Op::Sub
                };
                expression = Expr::Binary(Box::new(expression), op, Box::new(right));
            }
            Ok(expression)
        }
        Rule::postfix => {
            let mut inner = pair.into_inner();
            let mut expression = build(inner.next().context("empty postfix")?)?;
            for call in inner {
                let mut parts = call.into_inner();
                let name = parts
                    .next()
                    .context("method without name")?
                    .as_str()
                    .to_owned();
                let args = parts
                    .next()
                    .map(build_arguments)
                    .transpose()?
                    .unwrap_or_default();
                expression = Expr::Method(Box::new(expression), name, args);
            }
            Ok(expression)
        }
        Rule::function_call => {
            let mut inner = pair.into_inner();
            let name = inner
                .next()
                .context("function without name")?
                .as_str()
                .to_owned();
            let args = inner
                .next()
                .map(build_arguments)
                .transpose()?
                .unwrap_or_default();
            if let Some((target, method)) = name.rsplit_once('.') {
                Ok(Expr::Method(
                    Box::new(Expr::Field(target.to_owned())),
                    method.to_owned(),
                    args,
                ))
            } else {
                Ok(Expr::Call(name, args))
            }
        }
        Rule::arguments => bail!("arguments must be handled by a call"),
        Rule::list => Ok(Expr::List(
            pair.into_inner().map(build).collect::<Result<Vec<_>>>()?,
        )),
        Rule::identifier => Ok(Expr::Field(pair.as_str().to_owned())),
        Rule::string => Ok(Expr::Literal(Value::String(unquote(pair.as_str())))),
        Rule::number => {
            let number = pair.as_str().parse::<f64>()?;
            Ok(Expr::Literal(Value::Number(
                Number::from_f64(number).context("invalid number")?,
            )))
        }
        Rule::date_literal => Ok(Expr::Literal(Value::String(pair.as_str().to_owned()))),
        Rule::boolean => Ok(Expr::Literal(Value::Bool(
            pair.as_str().eq_ignore_ascii_case("true"),
        ))),
        Rule::null => Ok(Expr::Literal(Value::Null)),
        rule => bail!("unexpected expression rule: {rule:?}"),
    }
}

fn build_arguments(pair: Pair<'_, Rule>) -> Result<Vec<Expr>> {
    pair.into_inner().map(build).collect()
}

fn fold(pair: Pair<'_, Rule>, op: Op) -> Result<Expr> {
    let mut inner = pair.into_inner();
    let mut expression = build(inner.next().context("empty boolean expression")?)?;
    for item in inner {
        expression = Expr::Binary(Box::new(expression), op, Box::new(build(item)?));
    }
    Ok(expression)
}

fn unquote(value: &str) -> String {
    value[1..value.len() - 1]
        .replace("\\\"", "\"")
        .replace("\\'", "'")
}

fn value_at_path<'a>(root: &'a Value, path: &str) -> Option<&'a Value> {
    let mut current = root;
    for part in path.split('.') {
        current = current.as_object()?.get(part)?;
    }
    Some(current)
}

pub fn truthy(value: &Value) -> bool {
    match value {
        Value::Null => false,
        Value::Bool(value) => *value,
        Value::Number(value) => value.as_f64().is_some_and(|value| value != 0.0),
        Value::String(value) => !value.is_empty(),
        Value::Array(value) => !value.is_empty(),
        Value::Object(value) => !value.is_empty(),
    }
}

fn compare(left: &Value, right: &Value, op: Op) -> bool {
    let ordering = value_order(left, right);
    match op {
        Op::Eq => semantic_equal(left, right) || ordering == Some(Ordering::Equal),
        Op::Ne => !semantic_equal(left, right) && ordering != Some(Ordering::Equal),
        Op::Gt => ordering == Some(Ordering::Greater),
        Op::Ge => matches!(ordering, Some(Ordering::Greater | Ordering::Equal)),
        Op::Lt => ordering == Some(Ordering::Less),
        Op::Le => matches!(ordering, Some(Ordering::Less | Ordering::Equal)),
        Op::Or | Op::And | Op::Add | Op::Sub => false,
    }
}

fn semantic_equal(left: &Value, right: &Value) -> bool {
    if left == right {
        return true;
    }
    semantic_link_path(left)
        .zip(semantic_link_path(right))
        .is_some_and(|(left, right)| normalize_link_path(left) == normalize_link_path(right))
}

fn semantic_link_path(value: &Value) -> Option<&str> {
    value.get("path").and_then(Value::as_str)
}

fn normalize_link_path(path: &str) -> &str {
    path.trim_end_matches(".md")
        .rsplit('/')
        .next()
        .unwrap_or(path)
}

pub fn value_order(left: &Value, right: &Value) -> Option<Ordering> {
    match (left, right) {
        (Value::Number(left), Value::Number(right)) => left.as_f64()?.partial_cmp(&right.as_f64()?),
        (Value::String(left), Value::String(right)) => {
            let dates = NaiveDate::parse_from_str(left, "%Y-%m-%d")
                .ok()
                .zip(NaiveDate::parse_from_str(right, "%Y-%m-%d").ok());
            dates
                .map(|(left, right)| left.cmp(&right))
                .or_else(|| Some(left.cmp(right)))
        }
        (Value::Bool(left), Value::Bool(right)) => Some(left.cmp(right)),
        _ => None,
    }
}

fn call(name: &str, args: &[Expr], row: &Value) -> Value {
    match name.to_ascii_lowercase().as_str() {
        "date" => args.first().map(|arg| arg.eval(row)).unwrap_or(Value::Null),
        "today" => Value::String(Local::now().date_naive().to_string()),
        "length" => args
            .first()
            .map(|arg| length(&arg.eval(row)))
            .unwrap_or(Value::Null),
        "contains" => binary_args(args, row, contains),
        "icontains" => binary_args(args, row, |value, expected| {
            contains(&lowercase_value(value), &lowercase_value(expected))
        }),
        "startswith" => binary_args(args, row, |value, expected| {
            value
                .as_str()
                .zip(expected.as_str())
                .is_some_and(|(value, expected)| value.starts_with(expected))
        }),
        "endswith" => binary_args(args, row, |value, expected| {
            value
                .as_str()
                .zip(expected.as_str())
                .is_some_and(|(value, expected)| value.ends_with(expected))
        }),
        "if" => {
            if args.first().is_some_and(|arg| truthy(&arg.eval(row))) {
                args.get(1).map(|arg| arg.eval(row)).unwrap_or(Value::Null)
            } else {
                args.get(2).map(|arg| arg.eval(row)).unwrap_or(Value::Null)
            }
        }
        "list" => Value::Array(
            args.iter()
                .flat_map(|arg| match arg.eval(row) {
                    Value::Null => Vec::new(),
                    Value::Array(values) => values,
                    value => vec![value],
                })
                .collect(),
        ),
        "join" => {
            let separator = args
                .get(1)
                .and_then(|arg| arg.eval(row).as_str().map(str::to_owned))
                .unwrap_or_else(|| ", ".to_owned());
            args.first()
                .map(|arg| arg.eval(row))
                .and_then(|value| value.as_array().cloned())
                .map(|items| {
                    Value::String(
                        items
                            .iter()
                            .map(display_value)
                            .collect::<Vec<_>>()
                            .join(&separator),
                    )
                })
                .unwrap_or(Value::Null)
        }
        "link" => {
            let path = args.first().map(|arg| arg.eval(row)).unwrap_or(Value::Null);
            let path = path.get("path").cloned().unwrap_or(path);
            let display = args
                .get(1)
                .map(|arg| arg.eval(row))
                .unwrap_or_else(|| path.clone());
            serde_json::json!({"path": path, "display": display})
        }
        _ => Value::Null,
    }
}

fn method(target: Value, name: &str, args: &[Expr], row: &Value) -> Value {
    match name.to_ascii_lowercase().as_str() {
        "contains" => {
            let expected = args.first().map(|arg| arg.eval(row)).unwrap_or(Value::Null);
            Value::Bool(contains(&target, &expected))
        }
        "infolder" => {
            let expected = args
                .first()
                .and_then(|arg| arg.eval(row).as_str().map(str::to_owned));
            Value::Bool(
                target
                    .get("path")
                    .and_then(Value::as_str)
                    .zip(expected)
                    .is_some_and(|(path, folder)| {
                        path == folder || path.starts_with(&format!("{folder}/"))
                    }),
            )
        }
        "hasproperty" => {
            let expected = args
                .first()
                .and_then(|arg| arg.eval(row).as_str().map(str::to_owned));
            Value::Bool(
                target
                    .get("frontmatter")
                    .and_then(Value::as_object)
                    .zip(expected)
                    .is_some_and(|(metadata, key)| metadata.contains_key(&key)),
            )
        }
        "hastag" => {
            let expected = args
                .first()
                .and_then(|arg| arg.eval(row).as_str().map(str::to_owned));
            Value::Bool(
                target
                    .get("tags")
                    .and_then(Value::as_array)
                    .zip(expected)
                    .is_some_and(|(tags, expected)| {
                        tags.iter().any(|tag| {
                            tag.as_str()
                                .is_some_and(|tag| tag == expected || tag == format!("#{expected}"))
                        })
                    }),
            )
        }
        "startswith" => Value::Bool(
            target
                .as_str()
                .zip(
                    args.first()
                        .and_then(|arg| arg.eval(row).as_str().map(str::to_owned)),
                )
                .is_some_and(|(value, expected)| value.starts_with(&expected)),
        ),
        "endswith" => Value::Bool(
            target
                .as_str()
                .zip(
                    args.first()
                        .and_then(|arg| arg.eval(row).as_str().map(str::to_owned)),
                )
                .is_some_and(|(value, expected)| value.ends_with(&expected)),
        ),
        "date" => target
            .as_str()
            .and_then(|value| value.get(..10))
            .map(|value| Value::String(value.to_owned()))
            .unwrap_or(Value::Null),
        "format" => {
            let pattern = args
                .first()
                .and_then(|arg| arg.eval(row).as_str().map(str::to_owned))
                .unwrap_or_default();
            format_date(&target, &pattern).unwrap_or(target)
        }
        "aslink" => target
            .get("link")
            .or_else(|| target.get("path"))
            .cloned()
            .unwrap_or(target),
        _ => Value::Null,
    }
}

fn add(left: Value, right: Value) -> Value {
    if let (Some(left), Some(right)) = (left.as_f64(), right.as_f64()) {
        return Number::from_f64(left + right)
            .map(Value::Number)
            .unwrap_or(Value::Null);
    }
    if let (Some(date), Some(days)) = (left.as_str().and_then(parse_date), duration_days(&right)) {
        return Value::String((date + chrono::Duration::days(days)).to_string());
    }
    Value::String(format!("{}{}", display_value(&left), display_value(&right)))
}

fn subtract(left: Value, right: Value) -> Value {
    if let (Some(left), Some(right)) = (left.as_f64(), right.as_f64()) {
        return Number::from_f64(left - right)
            .map(Value::Number)
            .unwrap_or(Value::Null);
    }
    if let (Some(date), Some(days)) = (left.as_str().and_then(parse_date), duration_days(&right)) {
        return Value::String((date - chrono::Duration::days(days)).to_string());
    }
    Value::Null
}

fn parse_date(value: &str) -> Option<NaiveDate> {
    NaiveDate::parse_from_str(value.get(..10)?, "%Y-%m-%d").ok()
}

fn duration_days(value: &Value) -> Option<i64> {
    value.as_str()?.strip_suffix('d')?.parse::<i64>().ok()
}

fn display_value(value: &Value) -> String {
    value
        .as_str()
        .map(str::to_owned)
        .unwrap_or_else(|| value.to_string())
}

fn format_date(value: &Value, pattern: &str) -> Option<Value> {
    let source = value.as_str()?;
    let chrono_pattern = pattern
        .replace("YYYY", "%Y")
        .replace("MM", "%m")
        .replace("DD", "%d")
        .replace("HH", "%H")
        .replace("mm", "%M");
    if let Ok(value) = chrono::DateTime::parse_from_rfc3339(source) {
        return Some(Value::String(value.format(&chrono_pattern).to_string()));
    }
    for source_pattern in [
        "%Y-%m-%dT%H:%M:%S",
        "%Y-%m-%dT%H:%M",
        "%Y-%m-%d %H:%M:%S",
        "%Y-%m-%d %H:%M",
    ] {
        if let Ok(value) = chrono::NaiveDateTime::parse_from_str(source, source_pattern) {
            return Some(Value::String(value.format(&chrono_pattern).to_string()));
        }
    }
    if chrono_pattern.contains("%H") || chrono_pattern.contains("%M") {
        return None;
    }
    NaiveDate::parse_from_str(source.get(..10)?, "%Y-%m-%d")
        .ok()
        .map(|value| Value::String(value.format(&chrono_pattern).to_string()))
}

fn binary_args(
    args: &[Expr],
    row: &Value,
    predicate: impl FnOnce(&Value, &Value) -> bool,
) -> Value {
    let Some((left, right)) = args.first().zip(args.get(1)) else {
        return Value::Bool(false);
    };
    Value::Bool(predicate(&left.eval(row), &right.eval(row)))
}

fn contains(value: &Value, expected: &Value) -> bool {
    match value {
        Value::String(value) => expected.as_str().is_some_and(|item| value.contains(item)),
        Value::Array(values) => values.iter().any(|value| value == expected),
        Value::Object(values) => expected
            .as_str()
            .is_some_and(|key| values.contains_key(key)),
        _ => false,
    }
}

fn lowercase_value(value: &Value) -> Value {
    value
        .as_str()
        .map(|value| Value::String(value.to_lowercase()))
        .unwrap_or_else(|| value.clone())
}

fn length(value: &Value) -> Value {
    let length = match value {
        Value::String(value) => value.chars().count(),
        Value::Array(value) => value.len(),
        Value::Object(value) => value.len(),
        _ => 0,
    };
    Value::Number(length.into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn evaluates_dataview_style_expressions() {
        let row = json!({"file": {"path": "Daily/2026-06-14.md", "tags": ["daily"]}, "score": 3});
        assert!(
            Expr::parse("file.path.startsWith('Daily/') and score >= 2")
                .unwrap()
                .test(&row)
        );
        assert!(
            Expr::parse("file.tags.contains('daily')")
                .unwrap()
                .test(&row)
        );
    }
}
