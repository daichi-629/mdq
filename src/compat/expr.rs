use std::cmp::Ordering;

use anyhow::{Context, Result, bail};
use chrono::{Datelike, Local, NaiveDate, NaiveDateTime, Timelike};
use pest::Parser;
use pest::iterators::Pair;
use pest_derive::Parser;
use regex::Regex;
use serde_json::{Map, Number, Value, json};

#[derive(Parser)]
#[grammar = "compat/expr.pest"]
struct ExprParser;

pub const KIND_KEY: &str = "__kind";

#[derive(Clone, Debug)]
pub enum Expr {
    Literal(Value),
    Field(String),
    FieldAccess(Box<Expr>, String),
    Index(Box<Expr>, Box<Expr>),
    List(Vec<Expr>),
    Object(Vec<(String, Expr)>),
    Regex(String, String),
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
    Mul,
    Div,
    Mod,
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
            Self::Field(name) => row.get(name).cloned().unwrap_or(Value::Null),
            Self::FieldAccess(target, name) => field_of(&target.eval(row), name),
            Self::Index(target, index) => index_into(&target.eval(row), &index.eval(row)),
            Self::List(items) => Value::Array(items.iter().map(|item| item.eval(row)).collect()),
            Self::Object(entries) => Value::Object(
                entries
                    .iter()
                    .map(|(key, value)| (key.clone(), value.eval(row)))
                    .collect(),
            ),
            Self::Regex(pattern, flags) => regex_value(pattern, flags),
            Self::Not(expr) => Value::Bool(!truthy(&expr.eval(row))),
            Self::Binary(left, Op::Or, right) => {
                Value::Bool(truthy(&left.eval(row)) || truthy(&right.eval(row)))
            }
            Self::Binary(left, Op::And, right) => {
                Value::Bool(truthy(&left.eval(row)) && truthy(&right.eval(row)))
            }
            Self::Binary(left, Op::Add, right) => add(left.eval(row), right.eval(row)),
            Self::Binary(left, Op::Sub, right) => subtract(left.eval(row), right.eval(row)),
            Self::Binary(left, Op::Mul, right) => multiply(left.eval(row), right.eval(row)),
            Self::Binary(left, Op::Div, right) => divide(left.eval(row), right.eval(row)),
            Self::Binary(left, Op::Mod, right) => modulo(left.eval(row), right.eval(row)),
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
        Rule::product => {
            let mut inner = pair.into_inner();
            let mut expression = build(inner.next().context("empty product")?)?;
            while let Some(operator) = inner.next() {
                let right = build(inner.next().context("missing arithmetic operand")?)?;
                let op = match operator.as_str() {
                    "*" => Op::Mul,
                    "/" => Op::Div,
                    _ => Op::Mod,
                };
                expression = Expr::Binary(Box::new(expression), op, Box::new(right));
            }
            Ok(expression)
        }
        Rule::postfix => {
            let mut inner = pair.into_inner();
            let mut expression = build(inner.next().context("empty postfix")?)?;
            for call in inner {
                match call.as_rule() {
                    Rule::method_call => {
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
                    Rule::field_call => {
                        let name = call
                            .into_inner()
                            .next()
                            .context("field access without name")?
                            .as_str()
                            .to_owned();
                        expression = Expr::FieldAccess(Box::new(expression), name);
                    }
                    Rule::index_call => {
                        let index = build(call.into_inner().next().context("empty index")?)?;
                        expression = Expr::Index(Box::new(expression), Box::new(index));
                    }
                    rule => bail!("unexpected postfix rule: {rule:?}"),
                }
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
        Rule::object => {
            let mut entries = Vec::new();
            for entry in pair.into_inner() {
                let mut inner = entry.into_inner();
                let key_pair = inner.next().context("missing object key")?;
                let key_inner = key_pair.into_inner().next().context("empty object key")?;
                let key = match key_inner.as_rule() {
                    Rule::string => unquote(key_inner.as_str()),
                    Rule::identifier => key_inner.as_str().to_owned(),
                    rule => bail!("unexpected object key rule: {rule:?}"),
                };
                let value = build(inner.next().context("missing object value")?)?;
                entries.push((key, value));
            }
            Ok(Expr::Object(entries))
        }
        Rule::regex => {
            let (pattern, flags) = split_regex_literal(pair.as_str());
            Ok(Expr::Regex(pattern, flags))
        }
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

fn split_regex_literal(raw: &str) -> (String, String) {
    let without_lead = &raw[1..];
    let mut split = without_lead.len();
    while split > 0
        && without_lead
            .as_bytes()
            .get(split - 1)
            .is_some_and(u8::is_ascii_alphabetic)
    {
        split -= 1;
    }
    let flags = without_lead[split..].to_owned();
    let body = without_lead[..split.saturating_sub(1)].replace("\\/", "/");
    (body, flags)
}

// ---------------------------------------------------------------------------
// Value model helpers: tagged Date/Duration/Link/File/Regex/Html/Icon/Image
// values are represented as JSON objects carrying a `__kind` discriminator.
// ---------------------------------------------------------------------------

pub fn kind_of(value: &Value) -> &str {
    match value {
        Value::Null => "null",
        Value::Bool(_) => "boolean",
        Value::Number(_) => "number",
        Value::String(_) => "string",
        Value::Array(_) => "list",
        Value::Object(map) => map
            .get(KIND_KEY)
            .and_then(Value::as_str)
            .unwrap_or("object"),
    }
}

fn tagged(kind: &str, fields: Vec<(&str, Value)>) -> Value {
    let mut map = Map::new();
    map.insert(KIND_KEY.to_owned(), Value::String(kind.to_owned()));
    for (key, value) in fields {
        map.insert(key.to_owned(), value);
    }
    Value::Object(map)
}

const DATE_FORMAT: &str = "%Y-%m-%dT%H:%M:%S%.3f";

fn date_value(datetime: NaiveDateTime) -> Value {
    tagged(
        "date",
        vec![(
            "value",
            Value::String(datetime.format(DATE_FORMAT).to_string()),
        )],
    )
}

fn link_value(path: Value, display: Value) -> Value {
    tagged("link", vec![("path", path), ("display", display)])
}

fn regex_value(pattern: &str, flags: &str) -> Value {
    tagged(
        "regexp",
        vec![
            ("pattern", Value::String(pattern.to_owned())),
            ("flags", Value::String(flags.to_owned())),
        ],
    )
}

/// Parses a date string in any of the formats Bases/Dataview commonly emit:
/// RFC 3339, `YYYY-MM-DD HH:mm:ss[.fff]`, `YYYY-MM-DDTHH:mm:ss`, or a bare
/// `YYYY-MM-DD` date (midnight is assumed).
fn parse_date_flexible(source: &str) -> Option<NaiveDateTime> {
    if let Ok(parsed) = chrono::DateTime::parse_from_rfc3339(source) {
        return Some(parsed.naive_local());
    }
    for pattern in [
        "%Y-%m-%dT%H:%M:%S%.f",
        "%Y-%m-%dT%H:%M:%S",
        "%Y-%m-%dT%H:%M",
        "%Y-%m-%d %H:%M:%S%.f",
        "%Y-%m-%d %H:%M:%S",
        "%Y-%m-%d %H:%M",
    ] {
        if let Ok(parsed) = NaiveDateTime::parse_from_str(source, pattern) {
            return Some(parsed);
        }
    }
    NaiveDate::parse_from_str(source.get(..10)?, "%Y-%m-%d")
        .ok()
        .map(|date| date.and_hms_opt(0, 0, 0).unwrap())
}

pub(crate) fn as_datetime(value: &Value) -> Option<NaiveDateTime> {
    match value {
        Value::Object(map) if map.get(KIND_KEY).and_then(Value::as_str) == Some("date") => map
            .get("value")
            .and_then(Value::as_str)
            .and_then(parse_date_flexible),
        Value::String(value) => parse_date_flexible(value),
        _ => None,
    }
}

#[derive(Clone, Copy, Debug, Default)]
struct DurationSpec {
    years: f64,
    months: f64,
    weeks: f64,
    days: f64,
    hours: f64,
    minutes: f64,
    seconds: f64,
    milliseconds: f64,
}

fn parse_duration_string(source: &str) -> Option<DurationSpec> {
    static PATTERN: std::sync::OnceLock<Regex> = std::sync::OnceLock::new();
    let pattern =
        PATTERN.get_or_init(|| Regex::new(r"^\s*(-?\d+(?:\.\d+)?)\s*([A-Za-z]+)\s*$").unwrap());
    let captures = pattern.captures(source)?;
    let amount: f64 = captures[1].parse().ok()?;
    let unit = &captures[2];
    let mut spec = DurationSpec::default();
    match unit.as_ref() {
        "y" => spec.years = amount,
        "M" => spec.months = amount,
        "d" => spec.days = amount,
        "w" => spec.weeks = amount,
        "h" => spec.hours = amount,
        "m" => spec.minutes = amount,
        "s" => spec.seconds = amount,
        "ms" => spec.milliseconds = amount,
        other => match other.to_ascii_lowercase().as_str() {
            "year" | "years" => spec.years = amount,
            "month" | "months" => spec.months = amount,
            "day" | "days" => spec.days = amount,
            "week" | "weeks" => spec.weeks = amount,
            "hour" | "hours" => spec.hours = amount,
            "minute" | "minutes" => spec.minutes = amount,
            "second" | "seconds" => spec.seconds = amount,
            "millisecond" | "milliseconds" => spec.milliseconds = amount,
            _ => return None,
        },
    }
    Some(spec)
}

fn duration_fields(spec: DurationSpec) -> Value {
    json!({
        "years": spec.years,
        "months": spec.months,
        "weeks": spec.weeks,
        "days": spec.days,
        "hours": spec.hours,
        "minutes": spec.minutes,
        "seconds": spec.seconds,
        "milliseconds": spec.milliseconds,
    })
}

fn duration_value(spec: DurationSpec) -> Value {
    let mut map = Map::new();
    map.insert(KIND_KEY.to_owned(), Value::String("duration".to_owned()));
    if let Value::Object(fields) = duration_fields(spec) {
        map.extend(fields);
    }
    Value::Object(map)
}

fn as_duration_spec(value: &Value) -> Option<DurationSpec> {
    match value {
        Value::Object(map) if map.get(KIND_KEY).and_then(Value::as_str) == Some("duration") => {
            Some(DurationSpec {
                years: map.get("years").and_then(Value::as_f64).unwrap_or(0.0),
                months: map.get("months").and_then(Value::as_f64).unwrap_or(0.0),
                weeks: map.get("weeks").and_then(Value::as_f64).unwrap_or(0.0),
                days: map.get("days").and_then(Value::as_f64).unwrap_or(0.0),
                hours: map.get("hours").and_then(Value::as_f64).unwrap_or(0.0),
                minutes: map.get("minutes").and_then(Value::as_f64).unwrap_or(0.0),
                seconds: map.get("seconds").and_then(Value::as_f64).unwrap_or(0.0),
                milliseconds: map
                    .get("milliseconds")
                    .and_then(Value::as_f64)
                    .unwrap_or(0.0),
            })
        }
        Value::String(value) => parse_duration_string(value),
        _ => None,
    }
}

fn scale_duration(spec: DurationSpec, factor: f64) -> DurationSpec {
    DurationSpec {
        years: spec.years * factor,
        months: spec.months * factor,
        weeks: spec.weeks * factor,
        days: spec.days * factor,
        hours: spec.hours * factor,
        minutes: spec.minutes * factor,
        seconds: spec.seconds * factor,
        milliseconds: spec.milliseconds * factor,
    }
}

fn combine_duration(left: DurationSpec, right: DurationSpec, sign: f64) -> DurationSpec {
    DurationSpec {
        years: left.years + sign * right.years,
        months: left.months + sign * right.months,
        weeks: left.weeks + sign * right.weeks,
        days: left.days + sign * right.days,
        hours: left.hours + sign * right.hours,
        minutes: left.minutes + sign * right.minutes,
        seconds: left.seconds + sign * right.seconds,
        milliseconds: left.milliseconds + sign * right.milliseconds,
    }
}

fn apply_duration(
    datetime: NaiveDateTime,
    spec: &DurationSpec,
    sign: f64,
) -> Option<NaiveDateTime> {
    let mut result = datetime;
    let months = ((spec.years * 12.0 + spec.months) * sign).round() as i64;
    if months != 0 {
        result = if months > 0 {
            result.checked_add_months(chrono::Months::new(months as u32))?
        } else {
            result.checked_sub_months(chrono::Months::new((-months) as u32))?
        };
    }
    let millis = sign
        * (spec.weeks * 604_800_000.0
            + spec.days * 86_400_000.0
            + spec.hours * 3_600_000.0
            + spec.minutes * 60_000.0
            + spec.seconds * 1_000.0
            + spec.milliseconds);
    result.checked_add_signed(chrono::Duration::milliseconds(millis.round() as i64))
}

fn epoch_millis(datetime: NaiveDateTime) -> f64 {
    datetime.and_utc().timestamp_millis() as f64
}

pub fn truthy(value: &Value) -> bool {
    match value {
        Value::Null => false,
        Value::Bool(value) => *value,
        Value::Number(value) => value.as_f64().is_some_and(|value| value != 0.0),
        Value::String(value) => !value.is_empty(),
        Value::Array(_) | Value::Object(_) => true,
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
        Op::Or | Op::And | Op::Add | Op::Sub | Op::Mul | Op::Div | Op::Mod => false,
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
        (Value::Bool(left), Value::Bool(right)) => Some(left.cmp(right)),
        _ => {
            if let (Some(left), Some(right)) = (as_datetime(left), as_datetime(right)) {
                return Some(left.cmp(&right));
            }
            match (left, right) {
                (Value::String(left), Value::String(right)) => Some(left.cmp(right)),
                _ => None,
            }
        }
    }
}

fn num_value(value: f64) -> Value {
    Number::from_f64(value)
        .map(Value::Number)
        .unwrap_or(Value::Null)
}

fn add(left: Value, right: Value) -> Value {
    if kind_of(&left) == "duration" && kind_of(&right) == "duration" {
        if let (Some(left), Some(right)) = (as_duration_spec(&left), as_duration_spec(&right)) {
            return duration_value(combine_duration(left, right, 1.0));
        }
    }
    if let (Some(left), Some(right)) = (left.as_f64(), right.as_f64()) {
        return num_value(left + right);
    }
    if let Some(datetime) = as_datetime(&left) {
        if let Some(spec) = as_duration_spec(&right) {
            if let Some(result) = apply_duration(datetime, &spec, 1.0) {
                return date_value(result);
            }
        }
    }
    Value::String(format!("{}{}", display_value(&left), display_value(&right)))
}

fn subtract(left: Value, right: Value) -> Value {
    if let (Some(left), Some(right)) = (left.as_f64(), right.as_f64()) {
        return num_value(left - right);
    }
    if let Some(left_dt) = as_datetime(&left) {
        if let Some(right_dt) = as_datetime(&right) {
            return num_value(left_dt.signed_duration_since(right_dt).num_milliseconds() as f64);
        }
        if let Some(spec) = as_duration_spec(&right) {
            if let Some(result) = apply_duration(left_dt, &spec, -1.0) {
                return date_value(result);
            }
        }
        return Value::Null;
    }
    if kind_of(&left) == "duration" && kind_of(&right) == "duration" {
        if let (Some(left), Some(right)) = (as_duration_spec(&left), as_duration_spec(&right)) {
            return duration_value(combine_duration(left, right, -1.0));
        }
    }
    Value::Null
}

fn multiply(left: Value, right: Value) -> Value {
    if kind_of(&left) == "duration" {
        if let (Some(spec), Some(factor)) = (as_duration_spec(&left), right.as_f64()) {
            return duration_value(scale_duration(spec, factor));
        }
    }
    if let (Some(left), Some(right)) = (left.as_f64(), right.as_f64()) {
        return num_value(left * right);
    }
    Value::Null
}

fn divide(left: Value, right: Value) -> Value {
    if let (Some(left), Some(right)) = (left.as_f64(), right.as_f64()) {
        if right == 0.0 {
            return Value::Null;
        }
        return num_value(left / right);
    }
    Value::Null
}

fn modulo(left: Value, right: Value) -> Value {
    if let (Some(left), Some(right)) = (left.as_f64(), right.as_f64()) {
        if right == 0.0 {
            return Value::Null;
        }
        return num_value(left % right);
    }
    Value::Null
}

fn display_value(value: &Value) -> String {
    match value {
        Value::String(value) => value.clone(),
        Value::Object(map) if map.get(KIND_KEY).and_then(Value::as_str) == Some("date") => map
            .get("value")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned(),
        Value::Object(map) if map.get(KIND_KEY).and_then(Value::as_str) == Some("link") => map
            .get("display")
            .and_then(Value::as_str)
            .map(str::to_owned)
            .unwrap_or_else(|| display_value(map.get("path").unwrap_or(&Value::Null))),
        Value::Null => String::new(),
        other => other.to_string(),
    }
}

fn field_of(value: &Value, name: &str) -> Value {
    match value {
        Value::Object(map) => {
            if map.get(KIND_KEY).and_then(Value::as_str) == Some("date") {
                if let Some(datetime) = as_datetime(value) {
                    let date_result = match name {
                        "year" => Some(num_value(datetime.year() as f64)),
                        "month" => Some(num_value(datetime.month() as f64)),
                        "day" => Some(num_value(datetime.day() as f64)),
                        "hour" => Some(num_value(datetime.hour() as f64)),
                        "minute" => Some(num_value(datetime.minute() as f64)),
                        "second" => Some(num_value(datetime.second() as f64)),
                        "millisecond" => Some(num_value(
                            datetime.and_utc().timestamp_subsec_millis() as f64,
                        )),
                        _ => None,
                    };
                    if let Some(result) = date_result {
                        return result;
                    }
                }
            }
            map.get(name).cloned().unwrap_or(Value::Null)
        }
        Value::String(value) if name == "length" => num_value(value.chars().count() as f64),
        Value::Array(items) if name == "length" => num_value(items.len() as f64),
        _ => Value::Null,
    }
}

fn index_into(target: &Value, index: &Value) -> Value {
    match (target, index) {
        (Value::Array(items), Value::Number(number)) => {
            let index = number.as_f64().unwrap_or(-1.0);
            if index < 0.0 {
                return Value::Null;
            }
            items.get(index as usize).cloned().unwrap_or(Value::Null)
        }
        (Value::Object(map), Value::String(key)) => map.get(key).cloned().unwrap_or(Value::Null),
        _ => Value::Null,
    }
}

fn values_equal(left: &Value, right: &Value) -> bool {
    semantic_equal(left, right) || value_order(left, right) == Some(Ordering::Equal)
}

fn file_path_of(value: &Value) -> Option<String> {
    match value {
        Value::Object(map) => map.get("path").and_then(Value::as_str).map(str::to_owned),
        Value::String(value) => Some(value.clone()),
        _ => None,
    }
}

fn paths_match(left: &str, right: &str) -> bool {
    left == right || normalize_link_path(left) == normalize_link_path(right)
}

fn parse_wikilink_path(value: &Value) -> Value {
    if let Value::String(value) = value {
        if let Some(inner) = value
            .trim()
            .strip_prefix("[[")
            .and_then(|v| v.strip_suffix("]]"))
        {
            let path = inner.split('|').next().unwrap_or(inner).trim();
            return Value::String(path.to_owned());
        }
    }
    value.clone()
}

fn file_stub_for_path(path: &str, row: &Value) -> Value {
    if let Some(current) = row.get("file") {
        if current.get("path").and_then(Value::as_str) == Some(path) {
            return current.clone();
        }
    }
    let name = path.rsplit('/').next().unwrap_or(path);
    tagged(
        "file",
        vec![
            ("path", Value::String(path.to_owned())),
            ("name", Value::String(name.to_owned())),
            (
                "basename",
                Value::String(name.trim_end_matches(".md").to_owned()),
            ),
        ],
    )
}

fn escape_html(source: &str) -> String {
    source
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#39;")
}

fn numeric_variadic(args: &[Expr], row: &Value, combine: fn(f64, f64) -> f64, init: f64) -> Value {
    let mut result = init;
    let mut any = false;
    for arg in args {
        if let Some(number) = arg.eval(row).as_f64() {
            result = combine(result, number);
            any = true;
        }
    }
    if any { num_value(result) } else { Value::Null }
}

fn to_number(value: &Value) -> Value {
    match value {
        Value::Number(_) => value.clone(),
        Value::Bool(value) => num_value(if *value { 1.0 } else { 0.0 }),
        Value::String(value) => value
            .trim()
            .parse::<f64>()
            .ok()
            .map(num_value)
            .unwrap_or(Value::Null),
        other => as_datetime(other)
            .map(epoch_millis)
            .map(num_value)
            .unwrap_or(Value::Null),
    }
}

fn length_of(value: &Value) -> Value {
    match value {
        Value::String(value) => num_value(value.chars().count() as f64),
        Value::Array(items) => num_value(items.len() as f64),
        Value::Object(map) => {
            num_value(map.iter().filter(|(key, _)| *key != KIND_KEY).count() as f64)
        }
        _ => num_value(0.0),
    }
}

fn contains_value(value: &Value, expected: &Value) -> bool {
    match value {
        Value::String(value) => expected.as_str().is_some_and(|item| value.contains(item)),
        Value::Array(values) => values.iter().any(|item| values_equal(item, expected)),
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

fn random_unit() -> f64 {
    use std::cell::Cell;
    use std::time::{SystemTime, UNIX_EPOCH};
    thread_local! {
        static COUNTER: Cell<u64> = Cell::new(0);
    }
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos() as u64)
        .unwrap_or_default();
    let count = COUNTER.with(|counter| {
        let next = counter.get().wrapping_add(1);
        counter.set(next);
        next
    });
    let mut seed = nanos
        .wrapping_mul(6_364_136_223_846_793_005)
        .wrapping_add(count.wrapping_mul(1_442_695_040_888_963_407))
        ^ 0x9E3779B97F4A7C15;
    seed ^= seed << 13;
    seed ^= seed >> 7;
    seed ^= seed << 17;
    (seed as f64) / (u64::MAX as f64)
}

fn call(name: &str, args: &[Expr], row: &Value) -> Value {
    let lower = name.to_ascii_lowercase();
    match lower.as_str() {
        "escapehtml" => {
            let source = args
                .first()
                .map(|arg| arg.eval(row))
                .and_then(|value| value.as_str().map(str::to_owned))
                .unwrap_or_default();
            Value::String(escape_html(&source))
        }
        "date" => args
            .first()
            .map(|arg| arg.eval(row))
            .and_then(|value| as_datetime(&value))
            .map(date_value)
            .unwrap_or(Value::Null),
        "duration" => args
            .first()
            .map(|arg| arg.eval(row))
            .and_then(|value| value.as_str().map(str::to_owned))
            .and_then(|source| parse_duration_string(&source))
            .map(duration_value)
            .unwrap_or(Value::Null),
        "file" => {
            let value = args.first().map(|arg| arg.eval(row)).unwrap_or(Value::Null);
            file_path_of(&value)
                .map(|path| file_stub_for_path(&path, row))
                .unwrap_or(Value::Null)
        }
        "html" => {
            let source = args
                .first()
                .map(|arg| display_value(&arg.eval(row)))
                .unwrap_or_default();
            tagged("html", vec![("value", Value::String(source))])
        }
        "if" => {
            if args.first().is_some_and(|arg| truthy(&arg.eval(row))) {
                args.get(1).map(|arg| arg.eval(row)).unwrap_or(Value::Null)
            } else {
                args.get(2).map(|arg| arg.eval(row)).unwrap_or(Value::Null)
            }
        }
        "image" => {
            let value = args.first().map(|arg| arg.eval(row)).unwrap_or(Value::Null);
            let path = file_path_of(&value).map(Value::String).unwrap_or(value);
            tagged("image", vec![("path", path)])
        }
        "icon" => {
            let name = args
                .first()
                .map(|arg| arg.eval(row))
                .and_then(|value| value.as_str().map(str::to_owned))
                .unwrap_or_default();
            tagged("icon", vec![("name", Value::String(name))])
        }
        "link" => {
            let value = args.first().map(|arg| arg.eval(row)).unwrap_or(Value::Null);
            let path =
                parse_wikilink_path(&file_path_of(&value).map(Value::String).unwrap_or(value));
            let display = args
                .get(1)
                .map(|arg| arg.eval(row))
                .unwrap_or_else(|| path.clone());
            link_value(path, display)
        }
        "list" => {
            let value = args.first().map(|arg| arg.eval(row)).unwrap_or(Value::Null);
            if value.is_array() {
                value
            } else {
                Value::Array(vec![value])
            }
        }
        "max" => numeric_variadic(args, row, f64::max, f64::NEG_INFINITY),
        "min" => numeric_variadic(args, row, f64::min, f64::INFINITY),
        "now" => date_value(Local::now().naive_local()),
        "number" => to_number(&args.first().map(|arg| arg.eval(row)).unwrap_or(Value::Null)),
        "today" => date_value(Local::now().date_naive().and_hms_opt(0, 0, 0).unwrap()),
        "random" => num_value(random_unit()),
        "length" => args
            .first()
            .map(|arg| length_of(&arg.eval(row)))
            .unwrap_or(Value::Null),
        "contains" => binary_args(args, row, |value, expected| contains_value(value, expected)),
        "icontains" => binary_args(args, row, |value, expected| {
            contains_value(&lowercase_value(value), &lowercase_value(expected))
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
        _ => Value::Null,
    }
}

/// Evaluates method arguments that accept either multiple scalar args or a single list arg.
/// `f(a, b)` and `f([a, b])` both produce `[a, b]`.
fn eval_varargs(args: &[Expr], row: &Value) -> Vec<Value> {
    if args.len() == 1 {
        let v = args[0].eval(row);
        if let Value::Array(items) = v {
            return items;
        } else {
            return vec![v];
        }
    }
    args.iter().map(|arg| arg.eval(row)).collect()
}

fn with_bindings(row: &Value, bindings: &[(&str, Value)]) -> Value {
    let mut map = row.as_object().cloned().unwrap_or_default();
    for (key, value) in bindings {
        map.insert((*key).to_owned(), value.clone());
    }
    Value::Object(map)
}

fn list_filter(items: &[Value], expr: &Expr, row: &Value) -> Value {
    Value::Array(
        items
            .iter()
            .enumerate()
            .filter(|(index, value)| {
                let scope = with_bindings(
                    row,
                    &[
                        ("value", (*value).clone()),
                        ("index", num_value(*index as f64)),
                    ],
                );
                truthy(&expr.eval(&scope))
            })
            .map(|(_, value)| value.clone())
            .collect(),
    )
}

fn list_map(items: &[Value], expr: &Expr, row: &Value) -> Value {
    Value::Array(
        items
            .iter()
            .enumerate()
            .map(|(index, value)| {
                let scope = with_bindings(
                    row,
                    &[("value", value.clone()), ("index", num_value(index as f64))],
                );
                expr.eval(&scope)
            })
            .collect(),
    )
}

fn list_reduce(items: &[Value], expr: &Expr, initial: Option<&Expr>, row: &Value) -> Value {
    let mut accumulator = initial.map(|expr| expr.eval(row)).unwrap_or(Value::Null);
    for (index, value) in items.iter().enumerate() {
        let scope = with_bindings(
            row,
            &[
                ("value", value.clone()),
                ("index", num_value(index as f64)),
                ("acc", accumulator.clone()),
            ],
        );
        accumulator = expr.eval(&scope);
    }
    accumulator
}

fn flatten_once(items: &[Value]) -> Vec<Value> {
    items
        .iter()
        .flat_map(|item| match item {
            Value::Array(inner) => inner.clone(),
            other => vec![other.clone()],
        })
        .collect()
}

fn unique_values(items: &[Value]) -> Vec<Value> {
    let mut output: Vec<Value> = Vec::new();
    for item in items {
        if !output.iter().any(|existing| values_equal(existing, item)) {
            output.push(item.clone());
        }
    }
    output
}

fn clamp_index(value: f64, length: usize) -> usize {
    let value = if value < 0.0 {
        (length as f64 + value).max(0.0)
    } else {
        value
    };
    (value as usize).min(length)
}

fn slice_bounds(length: usize, args: &[Expr], row: &Value) -> (usize, usize) {
    let start = args
        .first()
        .map(|arg| arg.eval(row))
        .and_then(|value| value.as_f64())
        .unwrap_or(0.0);
    let end = args
        .get(1)
        .map(|arg| arg.eval(row))
        .and_then(|value| value.as_f64());
    let start = clamp_index(start, length);
    let end = end.map(|end| clamp_index(end, length)).unwrap_or(length);
    (start, end.max(start))
}

fn slice_list(items: &[Value], args: &[Expr], row: &Value) -> Vec<Value> {
    let (start, end) = slice_bounds(items.len(), args, row);
    items[start..end].to_vec()
}

fn slice_string(source: &str, args: &[Expr], row: &Value) -> String {
    let characters: Vec<char> = source.chars().collect();
    let (start, end) = slice_bounds(characters.len(), args, row);
    characters[start..end].iter().collect()
}

fn title_case(source: &str) -> String {
    source
        .split(' ')
        .map(|word| {
            let mut characters = word.chars();
            match characters.next() {
                Some(first) => first.to_uppercase().collect::<String>() + characters.as_str(),
                None => String::new(),
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

fn compile_regex(pattern: &str, flags: &str) -> std::result::Result<Regex, regex::Error> {
    regex::RegexBuilder::new(pattern)
        .case_insensitive(flags.contains('i'))
        .multi_line(flags.contains('m'))
        .dot_matches_new_line(flags.contains('s'))
        .build()
}

fn string_replace(source: &str, args: &[Expr], row: &Value) -> Value {
    let Some(pattern_value) = args.first().map(|arg| arg.eval(row)) else {
        return Value::String(source.to_owned());
    };
    let replacement = args
        .get(1)
        .map(|arg| arg.eval(row))
        .and_then(|value| value.as_str().map(str::to_owned))
        .unwrap_or_default();
    if kind_of(&pattern_value) == "regexp" {
        let pattern = pattern_value
            .get("pattern")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let flags = pattern_value
            .get("flags")
            .and_then(Value::as_str)
            .unwrap_or_default();
        return match compile_regex(pattern, flags) {
            Ok(regex) if flags.contains('g') => {
                Value::String(regex.replace_all(source, replacement.as_str()).into_owned())
            }
            Ok(regex) => Value::String(regex.replace(source, replacement.as_str()).into_owned()),
            Err(_) => Value::String(source.to_owned()),
        };
    }
    match pattern_value.as_str() {
        Some(needle) => Value::String(source.replace(needle, &replacement)),
        None => Value::String(source.to_owned()),
    }
}

fn string_split(source: &str, args: &[Expr], row: &Value) -> Value {
    let Some(separator_value) = args.first().map(|arg| arg.eval(row)) else {
        return Value::Array(vec![Value::String(source.to_owned())]);
    };
    let limit = args
        .get(1)
        .map(|arg| arg.eval(row))
        .and_then(|value| value.as_f64())
        .map(|value| value as usize);
    let parts: Vec<String> = if kind_of(&separator_value) == "regexp" {
        let pattern = separator_value
            .get("pattern")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let flags = separator_value
            .get("flags")
            .and_then(Value::as_str)
            .unwrap_or_default();
        match compile_regex(pattern, flags) {
            Ok(regex) => regex.split(source).map(str::to_owned).collect(),
            Err(_) => vec![source.to_owned()],
        }
    } else {
        let separator = separator_value.as_str().unwrap_or(",");
        source.split(separator).map(str::to_owned).collect()
    };
    let parts = match limit {
        Some(limit) => parts.into_iter().take(limit).collect(),
        None => parts,
    };
    Value::Array(parts.into_iter().map(Value::String).collect())
}

fn moment_to_chrono(pattern: &str) -> String {
    pattern
        .replace("YYYY", "%Y")
        .replace("MM", "%m")
        .replace("DD", "%d")
        .replace("HH", "%H")
        .replace("mm", "%M")
        .replace("ss", "%S")
}

fn format_date_value(datetime: NaiveDateTime, pattern: &str) -> Value {
    Value::String(datetime.format(&moment_to_chrono(pattern)).to_string())
}

fn relative_time(datetime: NaiveDateTime) -> String {
    let now = Local::now().naive_local();
    let delta = now.signed_duration_since(datetime);
    let seconds = delta.num_seconds();
    let (amount, unit) = match seconds.abs() {
        value if value < 60 => (value, "second"),
        value if value < 3_600 => (value / 60, "minute"),
        value if value < 86_400 => (value / 3_600, "hour"),
        value => (value / 86_400, "day"),
    };
    let plural = if amount == 1 { "" } else { "s" };
    if seconds >= 0 {
        format!("{amount} {unit}{plural} ago")
    } else {
        format!("in {amount} {unit}{plural}")
    }
}

fn tag_matches(tag: &str, query: &str) -> bool {
    let tag = tag.trim_start_matches('#');
    let query = query.trim_start_matches('#');
    tag == query || tag.starts_with(&format!("{query}/"))
}

fn any_method(target: &Value, name: &str, args: &[Expr], row: &Value) -> Option<Value> {
    match name {
        "istruthy" => Some(Value::Bool(truthy(target))),
        "istype" => {
            let expected = args
                .first()
                .map(|arg| arg.eval(row))
                .and_then(|value| value.as_str().map(str::to_owned))?;
            Some(Value::Bool(kind_of(target) == expected))
        }
        "tostring" => Some(Value::String(display_value(target))),
        _ => None,
    }
}

fn date_method(target: &Value, name: &str, args: &[Expr], row: &Value) -> Option<Value> {
    let datetime = as_datetime(target)?;
    match name {
        "date" => Some(date_value(datetime.date().and_hms_opt(0, 0, 0)?)),
        "format" => {
            let pattern = args
                .first()
                .map(|arg| arg.eval(row))
                .and_then(|value| value.as_str().map(str::to_owned))
                .unwrap_or_default();
            Some(format_date_value(datetime, &pattern))
        }
        "time" => Some(Value::String(datetime.format("%H:%M:%S").to_string())),
        "relative" => Some(Value::String(relative_time(datetime))),
        "isempty" => Some(Value::Bool(false)),
        _ => None,
    }
}

fn string_method(target: &Value, name: &str, args: &[Expr], row: &Value) -> Option<Value> {
    let source = target.as_str()?;
    let arg_str = |index: usize| {
        args.get(index)
            .map(|arg| arg.eval(row))
            .and_then(|value| value.as_str().map(str::to_owned))
    };
    match name {
        "contains" => Some(Value::Bool(
            arg_str(0).is_some_and(|needle| source.contains(&needle)),
        )),
        "containsall" => {
            let needles = eval_varargs(args, row);
            Some(Value::Bool(
                needles
                    .iter()
                    .all(|v| v.as_str().is_some_and(|n| source.contains(n))),
            ))
        }
        "containsany" => {
            let needles = eval_varargs(args, row);
            Some(Value::Bool(
                needles
                    .iter()
                    .any(|v| v.as_str().is_some_and(|n| source.contains(n))),
            ))
        }
        "endswith" => Some(Value::Bool(
            arg_str(0).is_some_and(|needle| source.ends_with(&needle)),
        )),
        "isempty" => Some(Value::Bool(source.is_empty())),
        "lower" => Some(Value::String(source.to_lowercase())),
        "upper" => Some(Value::String(source.to_uppercase())),
        "replace" => Some(string_replace(source, args, row)),
        "repeat" => {
            let count = args
                .first()
                .map(|arg| arg.eval(row))
                .and_then(|value| value.as_f64())
                .unwrap_or(0.0)
                .max(0.0) as usize;
            Some(Value::String(source.repeat(count)))
        }
        "reverse" => Some(Value::String(source.chars().rev().collect())),
        "slice" => Some(Value::String(slice_string(source, args, row))),
        "split" => Some(string_split(source, args, row)),
        "startswith" => Some(Value::Bool(
            arg_str(0).is_some_and(|needle| source.starts_with(&needle)),
        )),
        "title" => Some(Value::String(title_case(source))),
        "trim" => Some(Value::String(source.trim().to_owned())),
        "date" => parse_date_flexible(source).map(date_value),
        "format" => {
            let pattern = arg_str(0).unwrap_or_default();
            parse_date_flexible(source).map(|datetime| format_date_value(datetime, &pattern))
        }
        _ => None,
    }
}

fn number_method(target: &Value, name: &str, args: &[Expr], row: &Value) -> Option<Value> {
    let number = target.as_f64()?;
    match name {
        "abs" => Some(num_value(number.abs())),
        "ceil" => Some(num_value(number.ceil())),
        "floor" => Some(num_value(number.floor())),
        "isempty" => Some(Value::Bool(false)),
        "round" => {
            let digits = args
                .first()
                .map(|arg| arg.eval(row))
                .and_then(|value| value.as_f64())
                .unwrap_or(0.0);
            let factor = 10f64.powi(digits as i32);
            Some(num_value((number * factor).round() / factor))
        }
        "tofixed" => {
            let precision = args
                .first()
                .map(|arg| arg.eval(row))
                .and_then(|value| value.as_f64())
                .unwrap_or(0.0)
                .max(0.0) as usize;
            Some(Value::String(format!("{number:.precision$}")))
        }
        _ => None,
    }
}

fn list_method(target: &Value, name: &str, args: &[Expr], row: &Value) -> Option<Value> {
    let items = target.as_array()?;
    match name {
        "contains" => {
            let expected = args.first().map(|arg| arg.eval(row)).unwrap_or(Value::Null);
            Some(Value::Bool(
                items.iter().any(|item| values_equal(item, &expected)),
            ))
        }
        "containsall" => {
            let expected = eval_varargs(args, row);
            Some(Value::Bool(
                expected
                    .iter()
                    .all(|e| items.iter().any(|item| values_equal(item, e))),
            ))
        }
        "containsany" => {
            let expected = eval_varargs(args, row);
            Some(Value::Bool(
                expected
                    .iter()
                    .any(|e| items.iter().any(|item| values_equal(item, e))),
            ))
        }
        "filter" => Some(list_filter(items, args.first()?, row)),
        "flat" => Some(Value::Array(flatten_once(items))),
        "isempty" => Some(Value::Bool(items.is_empty())),
        "join" => {
            let separator = args
                .first()
                .map(|arg| arg.eval(row))
                .and_then(|value| value.as_str().map(str::to_owned))
                .unwrap_or_else(|| ",".to_owned());
            Some(Value::String(
                items
                    .iter()
                    .map(display_value)
                    .collect::<Vec<_>>()
                    .join(&separator),
            ))
        }
        "map" => Some(list_map(items, args.first()?, row)),
        "reduce" => Some(list_reduce(items, args.first()?, args.get(1), row)),
        "reverse" => {
            let mut reversed = items.clone();
            reversed.reverse();
            Some(Value::Array(reversed))
        }
        "slice" => Some(Value::Array(slice_list(items, args, row))),
        "sort" => {
            let mut sorted = items.clone();
            sorted.sort_by(|left, right| value_order(left, right).unwrap_or(Ordering::Equal));
            Some(Value::Array(sorted))
        }
        "unique" => Some(Value::Array(unique_values(items))),
        _ => None,
    }
}

fn link_method(target: &Value, name: &str, args: &[Expr], row: &Value) -> Option<Value> {
    match name {
        "asfile" => {
            let path = target.get("path").and_then(Value::as_str)?;
            Some(file_stub_for_path(path, row))
        }
        "linksto" => {
            let target_path = args
                .first()
                .map(|arg| arg.eval(row))
                .and_then(|value| file_path_of(&value))?;
            let own_links = row
                .get("file")
                .and_then(|file| file.get("links"))
                .and_then(Value::as_array);
            Some(Value::Bool(own_links.is_some_and(|links| {
                links.iter().any(|link| {
                    file_path_of(link).is_some_and(|path| paths_match(&path, &target_path))
                })
            })))
        }
        _ => None,
    }
}

fn file_method(target: &Value, name: &str, args: &[Expr], row: &Value) -> Option<Value> {
    match name {
        "aslink" => {
            let path = target.get("path").cloned().unwrap_or(Value::Null);
            let display = args
                .first()
                .map(|arg| arg.eval(row))
                .unwrap_or_else(|| target.get("name").cloned().unwrap_or(Value::Null));
            Some(link_value(path, display))
        }
        "haslink" => {
            let other_path = args
                .first()
                .map(|arg| arg.eval(row))
                .and_then(|value| file_path_of(&value));
            let links = target.get("links").and_then(Value::as_array);
            Some(Value::Bool(other_path.zip(links).is_some_and(
                |(path, links)| {
                    links.iter().any(|link| {
                        file_path_of(link).is_some_and(|other| paths_match(&other, &path))
                    })
                },
            )))
        }
        "hasproperty" => {
            let key = args
                .first()
                .map(|arg| arg.eval(row))
                .and_then(|value| value.as_str().map(str::to_owned));
            let properties = target
                .get("properties")
                .or_else(|| target.get("frontmatter"))
                .and_then(Value::as_object);
            Some(Value::Bool(key.zip(properties).is_some_and(
                |(key, properties)| properties.contains_key(&key),
            )))
        }
        "hastag" => {
            let tags = target.get("tags").and_then(Value::as_array);
            Some(Value::Bool(tags.is_some_and(|tags| {
                args.iter().any(|arg| {
                    arg.eval(row).as_str().is_some_and(|query| {
                        tags.iter()
                            .any(|tag| tag.as_str().is_some_and(|tag| tag_matches(tag, query)))
                    })
                })
            })))
        }
        "infolder" => {
            let folder = args
                .first()
                .map(|arg| arg.eval(row))
                .and_then(|value| value.as_str().map(str::to_owned));
            let path = target.get("path").and_then(Value::as_str);
            Some(Value::Bool(path.zip(folder).is_some_and(
                |(path, folder)| path == folder || path.starts_with(&format!("{folder}/")),
            )))
        }
        _ => None,
    }
}

fn object_method(target: &Value, name: &str, _args: &[Expr], _row: &Value) -> Option<Value> {
    let map = target.as_object()?;
    match name {
        "isempty" => Some(Value::Bool(map.iter().all(|(key, _)| key == KIND_KEY))),
        "keys" => Some(Value::Array(
            map.keys()
                .filter(|key| *key != KIND_KEY)
                .map(|key| Value::String(key.clone()))
                .collect(),
        )),
        "values" => Some(Value::Array(
            map.iter()
                .filter(|(key, _)| *key != KIND_KEY)
                .map(|(_, value)| value.clone())
                .collect(),
        )),
        _ => None,
    }
}

fn regexp_method(target: &Value, name: &str, args: &[Expr], row: &Value) -> Option<Value> {
    let pattern = target.get("pattern").and_then(Value::as_str)?;
    let flags = target.get("flags").and_then(Value::as_str).unwrap_or("");
    match name {
        "matches" => {
            let candidate = args
                .first()
                .map(|arg| arg.eval(row))
                .and_then(|value| value.as_str().map(str::to_owned))?;
            Some(Value::Bool(
                compile_regex(pattern, flags).is_ok_and(|regex| regex.is_match(&candidate)),
            ))
        }
        _ => None,
    }
}

fn method(target: Value, name: &str, args: &[Expr], row: &Value) -> Value {
    let lower = name.to_ascii_lowercase();
    if let Some(value) = any_method(&target, &lower, args, row) {
        return value;
    }
    let result = match kind_of(&target) {
        "date" => date_method(&target, &lower, args, row),
        "string" => string_method(&target, &lower, args, row),
        "number" => number_method(&target, &lower, args, row),
        "list" => list_method(&target, &lower, args, row),
        "link" => link_method(&target, &lower, args, row),
        "file" => file_method(&target, &lower, args, row),
        "object" => object_method(&target, &lower, args, row),
        "regexp" => regexp_method(&target, &lower, args, row),
        _ => None,
    };
    result.unwrap_or(Value::Null)
}
