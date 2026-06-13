use std::cmp::Ordering;

use anyhow::{Context, Result, bail};
use chrono::{DateTime, NaiveDate};
use pest::Parser;
use pest::iterators::Pair;
use pest_derive::Parser;
use regex::Regex;
use serde_json::Value;

/// A parsed, application-neutral filter over arbitrary structured metadata.
pub trait MetadataFilter: Send + Sync {
    fn matches(&self, metadata: &Value) -> bool;
}

/// Extension point for Tasks/Base/Dataview-compatible query syntaxes.
pub trait QueryLanguage: Send + Sync {
    fn name(&self) -> &'static str;
    fn parse(&self, source: &str) -> Result<Box<dyn MetadataFilter>>;
}

pub struct NativeQueryLanguage;

impl QueryLanguage for NativeQueryLanguage {
    fn name(&self) -> &'static str {
        "native"
    }

    fn parse(&self, source: &str) -> Result<Box<dyn MetadataFilter>> {
        Ok(Box::new(Expression::parse(source)?))
    }
}

#[derive(Parser)]
#[grammar = "filter.pest"]
struct FilterParser;

#[derive(Debug)]
pub enum Expression {
    And(Box<Expression>, Box<Expression>),
    Or(Box<Expression>, Box<Expression>),
    Not(Box<Expression>),
    Predicate(Predicate),
}

#[derive(Debug)]
pub struct Predicate {
    path: String,
    operator: Operator,
    expected: Option<Value>,
}

#[derive(Debug, Clone, Copy)]
enum Operator {
    Equal,
    NotEqual,
    Greater,
    GreaterEqual,
    Less,
    LessEqual,
    Contains,
    ContainsAll,
    Overlaps,
    In,
    StartsWith,
    EndsWith,
    Matches,
    Exists,
    Missing,
}

impl Expression {
    pub fn parse(input: &str) -> Result<Self> {
        let mut parsed = FilterParser::parse(Rule::expression, input)
            .with_context(|| format!("invalid filter expression: {input}"))?;
        build_expression(parsed.next().unwrap())
    }

    pub fn matches(&self, root: &Value) -> bool {
        MetadataFilter::matches(self, root)
    }
}

impl MetadataFilter for Expression {
    fn matches(&self, root: &Value) -> bool {
        match self {
            Self::And(left, right) => left.matches(root) && right.matches(root),
            Self::Or(left, right) => left.matches(root) || right.matches(root),
            Self::Not(expression) => !expression.matches(root),
            Self::Predicate(predicate) => predicate.matches(root),
        }
    }
}

impl Predicate {
    fn matches(&self, root: &Value) -> bool {
        let actual = value_at_path(root, &self.path);
        match self.operator {
            Operator::Exists => actual.is_some(),
            Operator::Missing => actual.is_none(),
            Operator::Equal => actual == self.expected.as_ref(),
            Operator::NotEqual => actual != self.expected.as_ref(),
            Operator::Contains => pair(actual, self.expected.as_ref(), contains),
            Operator::ContainsAll => pair(actual, self.expected.as_ref(), contains_all),
            Operator::Overlaps => pair(actual, self.expected.as_ref(), overlaps),
            Operator::In => pair(actual, self.expected.as_ref(), |actual, expected| {
                expected
                    .as_array()
                    .is_some_and(|values| values.contains(actual))
            }),
            Operator::StartsWith => string_pair(actual, self.expected.as_ref(), |value, prefix| {
                value.starts_with(prefix)
            }),
            Operator::EndsWith => string_pair(actual, self.expected.as_ref(), |value, suffix| {
                value.ends_with(suffix)
            }),
            Operator::Matches => pair(actual, self.expected.as_ref(), |actual, expected| {
                actual
                    .as_str()
                    .zip(expected.as_str())
                    .and_then(|(actual, pattern)| {
                        Regex::new(pattern)
                            .ok()
                            .map(|pattern| pattern.is_match(actual))
                    })
                    .unwrap_or(false)
            }),
            Operator::Greater | Operator::GreaterEqual | Operator::Less | Operator::LessEqual => {
                actual
                    .zip(self.expected.as_ref())
                    .and_then(|(actual, expected)| compare(actual, expected))
                    .is_some_and(|ordering| match self.operator {
                        Operator::Greater => ordering.is_gt(),
                        Operator::GreaterEqual => ordering.is_ge(),
                        Operator::Less => ordering.is_lt(),
                        Operator::LessEqual => ordering.is_le(),
                        _ => false,
                    })
            }
        }
    }
}

fn build_expression(pair: Pair<'_, Rule>) -> Result<Expression> {
    match pair.as_rule() {
        Rule::expression | Rule::unary_expr => {
            let mut inner = pair.into_inner();
            let first = inner.next().context("empty expression")?;
            if first.as_rule() == Rule::NOT {
                Ok(Expression::Not(Box::new(build_expression(
                    inner.next().context("missing expression after not")?,
                )?)))
            } else {
                build_expression(first)
            }
        }
        Rule::or_expr => fold_binary(pair, Expression::Or),
        Rule::and_expr => fold_binary(pair, Expression::And),
        Rule::predicate => build_predicate(pair),
        _ => bail!("unexpected grammar rule: {:?}", pair.as_rule()),
    }
}

fn fold_binary(
    pair: Pair<'_, Rule>,
    constructor: fn(Box<Expression>, Box<Expression>) -> Expression,
) -> Result<Expression> {
    let mut inner = pair.into_inner();
    let mut expression = build_expression(inner.next().context("empty boolean expression")?)?;
    for next in inner {
        expression = constructor(Box::new(expression), Box::new(build_expression(next)?));
    }
    Ok(expression)
}

fn build_predicate(pair: Pair<'_, Rule>) -> Result<Expression> {
    let mut inner = pair.into_inner();
    let path = inner
        .next()
        .context("predicate has no path")?
        .as_str()
        .to_owned();
    let operator_pair = inner.next().context("predicate has no operator")?;
    let operator = match operator_pair.as_rule() {
        Rule::EXISTS => Operator::Exists,
        Rule::MISSING => Operator::Missing,
        Rule::operator => parse_operator(operator_pair.as_str())?,
        _ => bail!("unexpected predicate operator"),
    };
    let expected = inner.next().map(|value| parse_value(value.as_str()));
    Ok(Expression::Predicate(Predicate {
        path,
        operator,
        expected,
    }))
}

fn parse_operator(operator: &str) -> Result<Operator> {
    Ok(match operator.to_ascii_lowercase().as_str() {
        "=" | "==" => Operator::Equal,
        "!=" => Operator::NotEqual,
        ">" => Operator::Greater,
        ">=" => Operator::GreaterEqual,
        "<" => Operator::Less,
        "<=" => Operator::LessEqual,
        "contains" => Operator::Contains,
        "contains_all" => Operator::ContainsAll,
        "overlaps" => Operator::Overlaps,
        "in" => Operator::In,
        "starts_with" => Operator::StartsWith,
        "ends_with" => Operator::EndsWith,
        "matches" => Operator::Matches,
        _ => bail!("unsupported operator: {operator}"),
    })
}

fn parse_value(input: &str) -> Value {
    serde_yaml::from_str::<serde_yaml::Value>(input)
        .ok()
        .and_then(|value| serde_json::to_value(value).ok())
        .unwrap_or_else(|| Value::String(input.to_owned()))
}

fn value_at_path<'a>(root: &'a Value, path: &str) -> Option<&'a Value> {
    let mut current = root;
    for component in parse_path(path) {
        current = match component {
            PathComponent::Key(key) => current.as_object()?.get(&key)?,
            PathComponent::Index(index) => current.as_array()?.get(index)?,
        };
    }
    Some(current)
}

enum PathComponent {
    Key(String),
    Index(usize),
}

fn parse_path(path: &str) -> Vec<PathComponent> {
    let mut components = Vec::new();
    for part in path.split('.') {
        let mut rest = part;
        if let Some(bracket) = rest.find('[') {
            if bracket > 0 {
                components.push(PathComponent::Key(rest[..bracket].to_owned()));
            }
            rest = &rest[bracket..];
            while let Some(after_open) = rest.strip_prefix('[') {
                let Some(end) = after_open.find(']') else {
                    break;
                };
                if let Ok(index) = after_open[..end].parse() {
                    components.push(PathComponent::Index(index));
                }
                rest = &after_open[end + 1..];
            }
        } else if !rest.is_empty() {
            components.push(PathComponent::Key(rest.to_owned()));
        }
    }
    components
}

fn pair(
    actual: Option<&Value>,
    expected: Option<&Value>,
    operation: impl FnOnce(&Value, &Value) -> bool,
) -> bool {
    actual
        .zip(expected)
        .is_some_and(|(actual, expected)| operation(actual, expected))
}

fn string_pair(
    actual: Option<&Value>,
    expected: Option<&Value>,
    operation: impl FnOnce(&str, &str) -> bool,
) -> bool {
    actual
        .and_then(Value::as_str)
        .zip(expected.and_then(Value::as_str))
        .is_some_and(|(actual, expected)| operation(actual, expected))
}

fn contains(actual: &Value, expected: &Value) -> bool {
    match actual {
        Value::Array(values) => values.contains(expected),
        Value::Object(values) => expected
            .as_str()
            .is_some_and(|key| values.contains_key(key)),
        Value::String(value) => expected
            .as_str()
            .is_some_and(|expected| value.contains(expected)),
        _ => false,
    }
}

fn contains_all(actual: &Value, expected: &Value) -> bool {
    actual
        .as_array()
        .zip(expected.as_array())
        .is_some_and(|(actual, expected)| expected.iter().all(|item| actual.contains(item)))
}

fn overlaps(actual: &Value, expected: &Value) -> bool {
    actual
        .as_array()
        .zip(expected.as_array())
        .is_some_and(|(actual, expected)| expected.iter().any(|item| actual.contains(item)))
}

fn compare(left: &Value, right: &Value) -> Option<Ordering> {
    match (left, right) {
        (Value::Number(left), Value::Number(right)) => left.as_f64()?.partial_cmp(&right.as_f64()?),
        (Value::String(left), Value::String(right)) => {
            compare_dates(left, right).or_else(|| Some(left.cmp(right)))
        }
        _ => None,
    }
}

fn compare_dates(left: &str, right: &str) -> Option<Ordering> {
    if let (Ok(left), Ok(right)) = (
        DateTime::parse_from_rfc3339(left),
        DateTime::parse_from_rfc3339(right),
    ) {
        return Some(left.cmp(&right));
    }
    if let (Ok(left), Ok(right)) = (
        NaiveDate::parse_from_str(left, "%Y-%m-%d"),
        NaiveDate::parse_from_str(right, "%Y-%m-%d"),
    ) {
        return Some(left.cmp(&right));
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn supports_boolean_and_collection_expressions() {
        let value = json!({
            "任意": {"状態": "active"},
            "items": ["a", "b", "c"],
            "score": 3
        });
        assert!(
            Expression::parse("任意.状態 = active and (score >= 2 or items contains z)")
                .unwrap()
                .matches(&value)
        );
        assert!(
            Expression::parse("items contains_all [a,b]")
                .unwrap()
                .matches(&value)
        );
        assert!(
            Expression::parse("items overlaps [z,c]")
                .unwrap()
                .matches(&value)
        );
        assert!(
            Expression::parse("not (score < 3 or 任意.状態 = paused)")
                .unwrap()
                .matches(&value)
        );
    }

    #[test]
    fn compares_dates_numbers_and_strings() {
        let value = json!({
            "date": "2026-06-14",
            "datetime": "2026-06-14T10:00:00+09:00",
            "score": 3.5,
            "name": "research-note"
        });
        assert!(
            Expression::parse("date >= 2026-06-01")
                .unwrap()
                .matches(&value)
        );
        assert!(
            Expression::parse("datetime < 2026-06-15T00:00:00+09:00")
                .unwrap()
                .matches(&value)
        );
        assert!(Expression::parse("score > 3").unwrap().matches(&value));
        assert!(
            Expression::parse("name starts_with research")
                .unwrap()
                .matches(&value)
        );
        assert!(
            Expression::parse("name matches \"^research-\"")
                .unwrap()
                .matches(&value)
        );
    }
}
