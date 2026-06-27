use std::fs;

use mdq::compat::CompatibilityEngine;
use mdq::core::QueryContext;
use mdq::db::Database;
use serde_json::Value;

// ---------------------------------------------------------------------------
// Fixture helpers
// ---------------------------------------------------------------------------

fn fixture() -> (tempfile::TempDir, Database) {
    let directory = tempfile::tempdir().unwrap();
    let vault = directory.path().join("vault");
    fs::create_dir_all(vault.join("Work")).unwrap();
    fs::create_dir_all(vault.join("Personal")).unwrap();

    fs::write(
        vault.join("Work/alpha.md"),
        r#"---
score: 8
rating: 4.5
active: true
tags: [work, project/alpha, featured]
title: "Alpha Note"
word_count: 120
color: red
due: 2026-01-01
related: "[[Personal/gamma]]"
---
# Alpha
[[Personal/delta]] and more text. #body/alpha
"#,
    )
    .unwrap();

    fs::write(
        vault.join("Work/beta.md"),
        r#"---
score: 3
rating: 2.0
active: false
tags: [work, project/beta]
title: "Beta Note"
word_count: 45
color: blue
due: 2026-01-03
---
# Beta
Some content.
"#,
    )
    .unwrap();

    fs::write(
        vault.join("Personal/gamma.md"),
        r#"---
score: 5
rating: 3.5
active: true
tags: [personal, archive]
title: "Gamma Note"
word_count: 80
color: red
due: 2026-01-05
---
# Gamma
"#,
    )
    .unwrap();

    fs::write(
        vault.join("Personal/delta.md"),
        r#"---
score: 1
rating: 1.0
active: false
tags: [personal]
title: "Delta Note"
word_count: 10
color: blue
due: 2026-01-07
---
# Delta
"#,
    )
    .unwrap();

    let mut database = Database::open(&directory.path().join("index.sqlite3")).unwrap();
    database.rebuild(&vault).unwrap();
    (directory, database)
}

fn engine() -> CompatibilityEngine {
    CompatibilityEngine::standard()
}

fn run_base<'a>(
    engine: &'a CompatibilityEngine,
    context: &QueryContext<'_>,
    yaml: &str,
) -> mdq::core::RecordSet {
    engine.execute("base", context, yaml).unwrap()
}

fn scores(rows: &[mdq::core::Row]) -> Vec<i64> {
    let mut v: Vec<i64> = rows
        .iter()
        .filter_map(|r| r.get("score").and_then(Value::as_i64))
        .collect();
    v.sort_unstable();
    v
}

// ---------------------------------------------------------------------------
// Obsidian docs: Bases syntax > Filters
// ---------------------------------------------------------------------------

#[test]
fn base_filter_simple_comparison() {
    let (dir, db) = fixture();
    let vault = dir.path().join("vault");
    let ctx = QueryContext {
        database: &db,
        vault: &vault,
        current_file: None,
    };
    let eng = engine();
    let result = run_base(&eng, &ctx, "filters: \"score > 5\"");
    let mut s = scores(&result.rows);
    s.sort_unstable();
    assert_eq!(s, vec![8]);
}

#[test]
fn base_filter_boolean_property() {
    let (dir, db) = fixture();
    let vault = dir.path().join("vault");
    let ctx = QueryContext {
        database: &db,
        vault: &vault,
        current_file: None,
    };
    let eng = engine();
    let result = run_base(&eng, &ctx, "filters: \"active = true\"");
    assert_eq!(result.rows.len(), 2);
    assert!(result.rows.iter().all(|r| r["active"] == Value::Bool(true)));
}

#[test]
fn base_filter_and_or_combination() {
    let (dir, db) = fixture();
    let vault = dir.path().join("vault");
    let ctx = QueryContext {
        database: &db,
        vault: &vault,
        current_file: None,
    };
    let eng = engine();
    let result = run_base(
        &eng,
        &ctx,
        r#"
filters:
  and:
    - "score >= 3"
    - or:
        - "active = true"
        - "rating >= 4"
"#,
    );
    // alpha (score=8, active=true), beta (score=3, rating=4+? no rating=2, active=false) — just alpha and gamma
    let mut s = scores(&result.rows);
    s.sort_unstable();
    assert_eq!(s, vec![5, 8]);
}

#[test]
fn base_filter_not() {
    let (dir, db) = fixture();
    let vault = dir.path().join("vault");
    let ctx = QueryContext {
        database: &db,
        vault: &vault,
        current_file: None,
    };
    let eng = engine();
    let result = run_base(
        &eng,
        &ctx,
        r#"
filters:
  not: "active = true"
"#,
    );
    assert!(
        result
            .rows
            .iter()
            .all(|r| r["active"] == Value::Bool(false))
    );
}

#[test]
fn base_filter_not_list_means_none_are_true() {
    let (dir, db) = fixture();
    let vault = dir.path().join("vault");
    let ctx = QueryContext {
        database: &db,
        vault: &vault,
        current_file: None,
    };
    let eng = engine();
    let result = run_base(
        &eng,
        &ctx,
        r#"
filters:
  not:
    - file.hasTag("work")
    - file.inFolder("Personal")
"#,
    );
    assert!(result.rows.is_empty());
}

#[test]
fn base_filter_file_in_folder() {
    let (dir, db) = fixture();
    let vault = dir.path().join("vault");
    let ctx = QueryContext {
        database: &db,
        vault: &vault,
        current_file: None,
    };
    let eng = engine();
    let result = run_base(&eng, &ctx, r#"filters: "file.inFolder(\"Work\")""#);
    assert_eq!(result.rows.len(), 2);
}

#[test]
fn base_filter_file_ext() {
    let (dir, db) = fixture();
    let vault = dir.path().join("vault");
    let ctx = QueryContext {
        database: &db,
        vault: &vault,
        current_file: None,
    };
    let eng = engine();
    let result = run_base(&eng, &ctx, r#"filters: "file.ext = 'md'""#);
    assert_eq!(result.rows.len(), 4);
}

#[test]
fn base_filter_has_tag() {
    let (dir, db) = fixture();
    let vault = dir.path().join("vault");
    let ctx = QueryContext {
        database: &db,
        vault: &vault,
        current_file: None,
    };
    let eng = engine();
    let result = run_base(&eng, &ctx, r#"filters: "file.hasTag(\"work\")""#);
    assert_eq!(result.rows.len(), 2);
}

#[test]
fn base_filter_has_tag_nested() {
    // Tags like "project/alpha" should match hasTag("project")
    let (dir, db) = fixture();
    let vault = dir.path().join("vault");
    let ctx = QueryContext {
        database: &db,
        vault: &vault,
        current_file: None,
    };
    let eng = engine();
    let result = run_base(&eng, &ctx, r#"filters: "file.hasTag(\"project\")""#);
    // alpha has project/alpha, beta has project/beta
    assert_eq!(result.rows.len(), 2);
}

#[test]
fn base_filter_has_tag_includes_body_tags() {
    let (dir, db) = fixture();
    let vault = dir.path().join("vault");
    let ctx = QueryContext {
        database: &db,
        vault: &vault,
        current_file: None,
    };
    let eng = engine();
    let result = run_base(&eng, &ctx, r#"filters: "file.hasTag(\"body\")""#);
    assert_eq!(result.rows.len(), 1);
    assert_eq!(result.rows[0]["title"], "Alpha Note");
}

#[test]
fn base_filter_has_property() {
    let (dir, db) = fixture();
    let vault = dir.path().join("vault");
    let ctx = QueryContext {
        database: &db,
        vault: &vault,
        current_file: None,
    };
    let eng = engine();
    // All notes have "score" property
    let result = run_base(&eng, &ctx, r#"filters: "file.hasProperty(\"score\")""#);
    assert_eq!(result.rows.len(), 4);
}

#[test]
fn base_file_links_include_frontmatter_links() {
    let (dir, db) = fixture();
    let vault = dir.path().join("vault");
    let ctx = QueryContext {
        database: &db,
        vault: &vault,
        current_file: None,
    };
    let eng = engine();
    let result = run_base(
        &eng,
        &ctx,
        r#"
filters: "file.hasLink(\"Personal/gamma\")"
"#,
    );
    assert_eq!(result.rows.len(), 1);
    assert_eq!(result.rows[0]["title"], "Alpha Note");
}

#[test]
fn base_filter_string_contains() {
    let (dir, db) = fixture();
    let vault = dir.path().join("vault");
    let ctx = QueryContext {
        database: &db,
        vault: &vault,
        current_file: None,
    };
    let eng = engine();
    let result = run_base(&eng, &ctx, r#"filters: "title.contains(\"Alpha\")""#);
    assert_eq!(result.rows.len(), 1);
    assert_eq!(result.rows[0]["title"], "Alpha Note");
}

#[test]
fn base_filter_string_starts_with() {
    let (dir, db) = fixture();
    let vault = dir.path().join("vault");
    let ctx = QueryContext {
        database: &db,
        vault: &vault,
        current_file: None,
    };
    let eng = engine();
    let result = run_base(&eng, &ctx, r#"filters: "title.startsWith(\"Alpha\")""#);
    assert_eq!(result.rows.len(), 1);
}

#[test]
fn base_filter_string_ends_with() {
    let (dir, db) = fixture();
    let vault = dir.path().join("vault");
    let ctx = QueryContext {
        database: &db,
        vault: &vault,
        current_file: None,
    };
    let eng = engine();
    let result = run_base(&eng, &ctx, r#"filters: "color.endsWith(\"ue\")""#);
    // beta and delta have "blue"
    assert_eq!(result.rows.len(), 2);
}

#[test]
fn base_filter_regex() {
    let (dir, db) = fixture();
    let vault = dir.path().join("vault");
    let ctx = QueryContext {
        database: &db,
        vault: &vault,
        current_file: None,
    };
    let eng = engine();
    let result = run_base(&eng, &ctx, r#"filters: "/^Alpha/i.matches(title)""#);
    assert_eq!(result.rows.len(), 1);
    assert_eq!(result.rows[0]["title"], "Alpha Note");
}

#[test]
fn base_filter_arithmetic_comparison() {
    let (dir, db) = fixture();
    let vault = dir.path().join("vault");
    let ctx = QueryContext {
        database: &db,
        vault: &vault,
        current_file: None,
    };
    let eng = engine();
    let result = run_base(&eng, &ctx, r#"filters: "score * 2 >= 10""#);
    // score >= 5: gamma (5) and alpha (8)
    let mut s = scores(&result.rows);
    s.sort_unstable();
    assert_eq!(s, vec![5, 8]);
}

// ---------------------------------------------------------------------------
// Obsidian docs: Bases syntax > Operators
// ---------------------------------------------------------------------------

#[test]
fn base_formula_arithmetic_operators() {
    let (dir, db) = fixture();
    let vault = dir.path().join("vault");
    let ctx = QueryContext {
        database: &db,
        vault: &vault,
        current_file: None,
    };
    let eng = engine();
    let result = run_base(
        &eng,
        &ctx,
        r#"
filters: "score = 8"
formulas:
  plus: score + 2
  minus: score - 3
  mul: score * 2
  div: score / 4
  mod: score % 3
"#,
    );
    assert_eq!(result.rows.len(), 1);
    let row = &result.rows[0];
    let f = |r: &mdq::core::Row, k: &str| r["formula"][k].as_f64().unwrap();
    assert_eq!(f(row, "plus"), 10.0); // 8+2
    assert_eq!(f(row, "minus"), 5.0); // 8-3
    assert_eq!(f(row, "mul"), 16.0); // 8*2
    assert_eq!(f(row, "div"), 2.0); // 8/4
    assert_eq!(f(row, "mod"), 2.0); // 8%3
}

#[test]
fn base_formula_string_concatenation() {
    let (dir, db) = fixture();
    let vault = dir.path().join("vault");
    let ctx = QueryContext {
        database: &db,
        vault: &vault,
        current_file: None,
    };
    let eng = engine();
    let result = run_base(
        &eng,
        &ctx,
        r#"
filters: "score = 8"
formulas:
  label: title + " (" + file.name + ")"
"#,
    );
    assert_eq!(result.rows.len(), 1);
    assert_eq!(result.rows[0]["formula"]["label"], "Alpha Note (alpha)");
}

// ---------------------------------------------------------------------------
// Obsidian docs: Functions > Global
// ---------------------------------------------------------------------------

#[test]
fn base_formula_if_conditional() {
    let (dir, db) = fixture();
    let vault = dir.path().join("vault");
    let ctx = QueryContext {
        database: &db,
        vault: &vault,
        current_file: None,
    };
    let eng = engine();
    // No views.order so formula stays nested under row["formula"]
    let result = run_base(
        &eng,
        &ctx,
        r#"
formulas:
  label: 'if(active, "yes", "no")'
"#,
    );
    assert_eq!(result.rows.len(), 4);
    let active_rows: Vec<_> = result
        .rows
        .iter()
        .filter(|r| r["active"] == Value::Bool(true))
        .collect();
    assert_eq!(active_rows.len(), 2);
    assert!(active_rows.iter().all(|r| r["formula"]["label"] == "yes"));
    let inactive_rows: Vec<_> = result
        .rows
        .iter()
        .filter(|r| r["active"] == Value::Bool(false))
        .collect();
    assert_eq!(inactive_rows.len(), 2);
    assert!(inactive_rows.iter().all(|r| r["formula"]["label"] == "no"));
}

#[test]
fn base_formula_number_coerce() {
    let (dir, db) = fixture();
    let vault = dir.path().join("vault");
    let ctx = QueryContext {
        database: &db,
        vault: &vault,
        current_file: None,
    };
    let eng = engine();
    let result = run_base(
        &eng,
        &ctx,
        r#"
filters: "score = 8"
formulas:
  n: number("42")
  max_val: max(score, 10)
  min_val: min(score, 10)
"#,
    );
    assert_eq!(result.rows.len(), 1);
    let formula = &result.rows[0]["formula"];
    assert_eq!(formula["n"].as_f64().unwrap(), 42.0);
    assert_eq!(formula["max_val"].as_f64().unwrap(), 10.0); // max(8, 10) = 10
    assert_eq!(formula["min_val"].as_f64().unwrap(), 8.0); // min(8, 10) = 8
}

#[test]
fn base_formula_date_global() {
    let (dir, db) = fixture();
    let vault = dir.path().join("vault");
    let ctx = QueryContext {
        database: &db,
        vault: &vault,
        current_file: None,
    };
    let eng = engine();
    let result = run_base(
        &eng,
        &ctx,
        r#"
filters: "score = 8"
formulas:
  d: date("2026-03-15")
  yr: date("2026-03-15").year
  mo: date("2026-03-15").month
  dy: date("2026-03-15").day
"#,
    );
    assert_eq!(result.rows.len(), 1);
    let formula = &result.rows[0]["formula"];
    // d should be a tagged date
    assert_eq!(formula["d"]["__kind"], "date");
    assert_eq!(formula["yr"].as_f64().unwrap(), 2026.0);
    assert_eq!(formula["mo"].as_f64().unwrap(), 3.0);
    assert_eq!(formula["dy"].as_f64().unwrap(), 15.0);
}

#[test]
fn base_formula_date_arithmetic_add() {
    let (dir, db) = fixture();
    let vault = dir.path().join("vault");
    let ctx = QueryContext {
        database: &db,
        vault: &vault,
        current_file: None,
    };
    let eng = engine();
    let result = run_base(
        &eng,
        &ctx,
        r#"
filters: "score = 8"
formulas:
  plus_days: date("2026-01-01") + duration("30d")
  plus_months: date("2026-01-15") + duration("2M")
  minus_week: date("2026-03-15") - duration("1w")
"#,
    );
    assert_eq!(result.rows.len(), 1);
    let formula = &result.rows[0]["formula"];
    // 2026-01-01 + 30d = 2026-01-31
    let plus_days_val = formula["plus_days"]["value"].as_str().unwrap();
    assert!(
        plus_days_val.starts_with("2026-01-31"),
        "got: {plus_days_val}"
    );
    // 2026-01-15 + 2M = 2026-03-15
    let plus_months_val = formula["plus_months"]["value"].as_str().unwrap();
    assert!(
        plus_months_val.starts_with("2026-03-15"),
        "got: {plus_months_val}"
    );
    // 2026-03-15 - 1w = 2026-03-08
    let minus_week_val = formula["minus_week"]["value"].as_str().unwrap();
    assert!(
        minus_week_val.starts_with("2026-03-08"),
        "got: {minus_week_val}"
    );
}

#[test]
fn base_formula_duration_units_all() {
    let (dir, db) = fixture();
    let vault = dir.path().join("vault");
    let ctx = QueryContext {
        database: &db,
        vault: &vault,
        current_file: None,
    };
    let eng = engine();
    let result = run_base(
        &eng,
        &ctx,
        r#"
filters: "score = 8"
formulas:
  plus_year: date("2025-06-15") + duration("1y")
  plus_hour: date("2026-01-01") + duration("2h")
  plus_minute: date("2026-01-01") + duration("90m")
  plus_second: date("2026-01-01") + duration("120s")
"#,
    );
    assert_eq!(result.rows.len(), 1);
    let formula = &result.rows[0]["formula"];
    let year_val = formula["plus_year"]["value"].as_str().unwrap();
    assert!(year_val.starts_with("2026-06-15"), "got: {year_val}");
    let hour_val = formula["plus_hour"]["value"].as_str().unwrap();
    assert!(hour_val.contains("T02:00:00"), "got: {hour_val}");
    let min_val = formula["plus_minute"]["value"].as_str().unwrap();
    assert!(min_val.contains("T01:30:00"), "got: {min_val}");
    let sec_val = formula["plus_second"]["value"].as_str().unwrap();
    assert!(sec_val.contains("T00:02:00"), "got: {sec_val}");
}

// ---------------------------------------------------------------------------
// Formula tests — string methods
// ---------------------------------------------------------------------------

#[test]
fn base_formula_string_methods() {
    let (dir, db) = fixture();
    let vault = dir.path().join("vault");
    let ctx = QueryContext {
        database: &db,
        vault: &vault,
        current_file: None,
    };
    let eng = engine();
    let result = run_base(
        &eng,
        &ctx,
        r#"
filters: "score = 8"
formulas:
  lower: title.lower()
  upper: title.upper()
  trim: '"  hello  ".trim()'
  reversed: '"abcd".reverse()'
  sliced: title.slice(0, 5)
  repeated: '"ab".repeat(3)'
  replaced: 'title.replace("Alpha", "Omega")'
  split_result: '"a,b,c".split(",").length'
  is_empty: title.isEmpty()
  title_case: '"hello world".title()'
"#,
    );
    assert_eq!(result.rows.len(), 1);
    let f = &result.rows[0]["formula"];
    assert_eq!(f["lower"], "alpha note");
    assert_eq!(f["upper"], "ALPHA NOTE");
    assert_eq!(f["trim"], "hello");
    assert_eq!(f["reversed"], "dcba");
    assert_eq!(f["sliced"], "Alpha");
    assert_eq!(f["repeated"], "ababab");
    assert_eq!(f["replaced"], "Omega Note");
    assert_eq!(f["split_result"].as_f64().unwrap(), 3.0);
    assert_eq!(f["is_empty"], Value::Bool(false));
    assert_eq!(f["title_case"], "Hello World");
}

#[test]
fn base_formula_string_contains_methods() {
    let (dir, db) = fixture();
    let vault = dir.path().join("vault");
    let ctx = QueryContext {
        database: &db,
        vault: &vault,
        current_file: None,
    };
    let eng = engine();
    let result = run_base(
        &eng,
        &ctx,
        r#"
filters: "score = 8"
formulas:
  has_alpha: 'title.contains("Alpha")'
  has_both: 'title.containsAll("Alpha", "Note")'
  has_any: 'title.containsAny("Zeta", "Note")'
  starts: 'title.startsWith("Alpha")'
  ends: 'title.endsWith("Note")'
"#,
    );
    assert_eq!(result.rows.len(), 1);
    let f = &result.rows[0]["formula"];
    assert_eq!(f["has_alpha"], Value::Bool(true));
    assert_eq!(f["has_both"], Value::Bool(true));
    assert_eq!(f["has_any"], Value::Bool(true));
    assert_eq!(f["starts"], Value::Bool(true));
    assert_eq!(f["ends"], Value::Bool(true));
}

// ---------------------------------------------------------------------------
// Formula tests — number methods
// ---------------------------------------------------------------------------

#[test]
fn base_formula_number_methods() {
    let (dir, db) = fixture();
    let vault = dir.path().join("vault");
    let ctx = QueryContext {
        database: &db,
        vault: &vault,
        current_file: None,
    };
    let eng = engine();
    let result = run_base(
        &eng,
        &ctx,
        r#"
filters: "score = 8"
formulas:
  abs_neg: (-3.7).abs()
  ceil_val: rating.ceil()
  floor_val: rating.floor()
  round_val: rating.round()
  fixed: rating.toFixed(1)
  is_empty: score.isEmpty()
"#,
    );
    assert_eq!(result.rows.len(), 1);
    let f = &result.rows[0]["formula"];
    assert_eq!(f["abs_neg"].as_f64().unwrap(), 3.7);
    assert_eq!(f["ceil_val"].as_f64().unwrap(), 5.0);
    assert_eq!(f["floor_val"].as_f64().unwrap(), 4.0);
    assert_eq!(f["round_val"].as_f64().unwrap(), 5.0); // 4.5 rounds to 5
    assert_eq!(f["fixed"], "4.5");
    assert_eq!(f["is_empty"], Value::Bool(false));
}

// ---------------------------------------------------------------------------
// Formula tests — list methods
// ---------------------------------------------------------------------------

#[test]
fn base_formula_list_global_and_methods() {
    let (dir, db) = fixture();
    let vault = dir.path().join("vault");
    let ctx = QueryContext {
        database: &db,
        vault: &vault,
        current_file: None,
    };
    let eng = engine();
    let result = run_base(
        &eng,
        &ctx,
        r#"
filters: "score = 8"
formulas:
  built: '[3, 1, 2]'
  len: '[3, 1, 2].length'
  sorted: '[3, 1, 2].sort()'
  reversed_list: '[1, 2, 3].reverse()'
  joined: '["a", "b", "c"].join(", ")'
  unique_list: '[1, 2, 1, 3, 2].unique().length'
  flat_list: '[[1, 2], [3, 4]].flat().length'
  sliced_list: '[1, 2, 3, 4, 5].slice(1, 3).length'
  tag_len: file.tags.length
"#,
    );
    assert_eq!(result.rows.len(), 1);
    let f = &result.rows[0]["formula"];
    // Use f64 comparisons because the evaluator produces float Number values
    let as_f64_vec = |v: &Value| -> Vec<f64> {
        v.as_array()
            .unwrap()
            .iter()
            .map(|x| x.as_f64().unwrap())
            .collect()
    };
    assert_eq!(as_f64_vec(&f["built"]), vec![3.0, 1.0, 2.0]);
    assert_eq!(f["len"].as_f64().unwrap(), 3.0);
    assert_eq!(as_f64_vec(&f["sorted"]), vec![1.0, 2.0, 3.0]);
    assert_eq!(as_f64_vec(&f["reversed_list"]), vec![3.0, 2.0, 1.0]);
    assert_eq!(f["joined"], "a, b, c");
    assert_eq!(f["unique_list"].as_f64().unwrap(), 3.0);
    assert_eq!(f["flat_list"].as_f64().unwrap(), 4.0);
    assert_eq!(f["sliced_list"].as_f64().unwrap(), 2.0);
    // alpha has 3 frontmatter tags plus one inline body tag.
    assert_eq!(f["tag_len"].as_f64().unwrap(), 4.0);
}

#[test]
fn base_formula_list_global_wraps_scalars_but_preserves_lists() {
    let (dir, db) = fixture();
    let vault = dir.path().join("vault");
    let ctx = QueryContext {
        database: &db,
        vault: &vault,
        current_file: None,
    };
    let eng = engine();
    let result = run_base(
        &eng,
        &ctx,
        r#"
filters: "score = 8"
formulas:
  scalar_len: list("value").length
  existing_len: list(file.tags).length
"#,
    );
    let formula = &result.rows[0]["formula"];
    assert_eq!(formula["scalar_len"].as_f64().unwrap(), 1.0);
    assert_eq!(formula["existing_len"].as_f64().unwrap(), 4.0);
}

#[test]
fn base_formula_list_contains_methods() {
    let (dir, db) = fixture();
    let vault = dir.path().join("vault");
    let ctx = QueryContext {
        database: &db,
        vault: &vault,
        current_file: None,
    };
    let eng = engine();
    let result = run_base(
        &eng,
        &ctx,
        r#"
filters: "score = 8"
formulas:
  has_work: 'file.tags.contains("work")'
  has_all: 'file.tags.containsAll("work", "featured")'
  has_any: 'file.tags.containsAny("archive", "featured")'
  is_empty_list: '[].isEmpty()'
  is_not_empty: file.tags.isEmpty()
"#,
    );
    assert_eq!(result.rows.len(), 1);
    let f = &result.rows[0]["formula"];
    assert_eq!(f["has_work"], Value::Bool(true));
    assert_eq!(f["has_all"], Value::Bool(true));
    assert_eq!(f["has_any"], Value::Bool(true));
    assert_eq!(f["is_empty_list"], Value::Bool(true));
    assert_eq!(f["is_not_empty"], Value::Bool(false));
}

// ---------------------------------------------------------------------------
// Formula tests — map / filter / reduce with implicit bindings
// ---------------------------------------------------------------------------

#[test]
fn base_formula_list_map_implicit_value() {
    let (dir, db) = fixture();
    let vault = dir.path().join("vault");
    let ctx = QueryContext {
        database: &db,
        vault: &vault,
        current_file: None,
    };
    let eng = engine();
    let result = run_base(
        &eng,
        &ctx,
        r#"
filters: "score = 8"
formulas:
  doubled: '[1, 2, 3].map(value * 2)'
  with_index: '[10, 20, 30].map(value + index)'
"#,
    );
    assert_eq!(result.rows.len(), 1);
    let f = &result.rows[0]["formula"];
    let nums = |v: &Value| -> Vec<f64> {
        v.as_array()
            .unwrap()
            .iter()
            .map(|x| x.as_f64().unwrap())
            .collect()
    };
    assert_eq!(nums(&f["doubled"]), vec![2.0, 4.0, 6.0]);
    // value+index: 10+0=10, 20+1=21, 30+2=32
    assert_eq!(nums(&f["with_index"]), vec![10.0, 21.0, 32.0]);
}

#[test]
fn base_formula_list_filter_implicit_value() {
    let (dir, db) = fixture();
    let vault = dir.path().join("vault");
    let ctx = QueryContext {
        database: &db,
        vault: &vault,
        current_file: None,
    };
    let eng = engine();
    let result = run_base(
        &eng,
        &ctx,
        r#"
filters: "score = 8"
formulas:
  evens: '[1, 2, 3, 4, 5].filter(value % 2 = 0)'
  big: '[1, 5, 10, 15].filter(value > 5)'
"#,
    );
    assert_eq!(result.rows.len(), 1);
    let f = &result.rows[0]["formula"];
    let nums = |v: &Value| -> Vec<f64> {
        v.as_array()
            .unwrap()
            .iter()
            .map(|x| x.as_f64().unwrap())
            .collect()
    };
    assert_eq!(nums(&f["evens"]), vec![2.0, 4.0]);
    assert_eq!(nums(&f["big"]), vec![10.0, 15.0]);
}

#[test]
fn base_formula_list_reduce_implicit_acc() {
    let (dir, db) = fixture();
    let vault = dir.path().join("vault");
    let ctx = QueryContext {
        database: &db,
        vault: &vault,
        current_file: None,
    };
    let eng = engine();
    let result = run_base(
        &eng,
        &ctx,
        r#"
filters: "score = 8"
formulas:
  total: '[1, 2, 3, 4, 5].reduce(acc + value, 0)'
  product: '[1, 2, 3, 4].reduce(acc * value, 1)'
"#,
    );
    assert_eq!(result.rows.len(), 1);
    let f = &result.rows[0]["formula"];
    assert_eq!(f["total"].as_f64().unwrap(), 15.0);
    assert_eq!(f["product"].as_f64().unwrap(), 24.0);
}

// ---------------------------------------------------------------------------
// Formula tests — object literals, index access
// ---------------------------------------------------------------------------

#[test]
fn base_formula_object_literal() {
    let (dir, db) = fixture();
    let vault = dir.path().join("vault");
    let ctx = QueryContext {
        database: &db,
        vault: &vault,
        current_file: None,
    };
    let eng = engine();
    let result = run_base(
        &eng,
        &ctx,
        r#"
filters: "score = 8"
formulas:
  obj: '{name: title, value: score}'
  nested: '{a: {b: 42}}'
"#,
    );
    assert_eq!(result.rows.len(), 1);
    let f = &result.rows[0]["formula"];
    assert_eq!(f["obj"]["name"], "Alpha Note");
    assert_eq!(f["obj"]["value"].as_f64().unwrap(), 8.0);
    assert_eq!(f["nested"]["a"]["b"].as_f64().unwrap(), 42.0);
}

#[test]
fn base_formula_index_access() {
    let (dir, db) = fixture();
    let vault = dir.path().join("vault");
    let ctx = QueryContext {
        database: &db,
        vault: &vault,
        current_file: None,
    };
    let eng = engine();
    let result = run_base(
        &eng,
        &ctx,
        r#"
filters: "score = 8"
formulas:
  first_tag: file.tags[0]
  second_tag: file.tags[1]
"#,
    );
    assert_eq!(result.rows.len(), 1);
    let f = &result.rows[0]["formula"];
    // alpha has tags: [work, project/alpha, featured]
    assert_eq!(f["first_tag"], "work");
    assert_eq!(f["second_tag"], "project/alpha");
}

// ---------------------------------------------------------------------------
// Formula tests — note.xxx prefix
// ---------------------------------------------------------------------------

#[test]
fn base_formula_note_prefix() {
    // note.xxx is an alias for frontmatter properties
    let (dir, db) = fixture();
    let vault = dir.path().join("vault");
    let ctx = QueryContext {
        database: &db,
        vault: &vault,
        current_file: None,
    };
    let eng = engine();
    let result = run_base(
        &eng,
        &ctx,
        r#"
filters: "score = 8"
formulas:
  via_note: note.title
  direct: title
  note_score: note.score
"#,
    );
    assert_eq!(result.rows.len(), 1);
    let f = &result.rows[0]["formula"];
    // note.xxx is an alias for frontmatter — same value as the direct property
    assert_eq!(f["via_note"], f["direct"]);
    assert_eq!(f["via_note"], "Alpha Note");
    assert_eq!(f["note_score"].as_f64().unwrap(), 8.0);
}

// ---------------------------------------------------------------------------
// Obsidian docs: Bases syntax > File properties
// ---------------------------------------------------------------------------

#[test]
fn base_formula_file_fields() {
    let (dir, db) = fixture();
    let vault = dir.path().join("vault");
    let ctx = QueryContext {
        database: &db,
        vault: &vault,
        current_file: None,
    };
    let eng = engine();
    let result = run_base(
        &eng,
        &ctx,
        r#"
filters: "score = 8"
formulas:
  fname: file.name
  fbasename: file.basename
  fext: file.ext
  ffolder: file.folder
  fsize: file.size
  mtime_kind: file.mtime.__kind
  mtime_year: file.mtime.year
"#,
    );
    assert_eq!(result.rows.len(), 1);
    let f = &result.rows[0]["formula"];
    assert_eq!(f["fname"], "alpha");
    assert_eq!(f["fbasename"], "alpha");
    assert_eq!(f["fext"], "md");
    assert_eq!(f["ffolder"], "Work");
    assert!(f["fsize"].as_f64().unwrap() > 0.0);
    assert_eq!(f["mtime_kind"], Value::String("date".to_owned()));
    // mtime_year should be a current-ish year number
    assert!(f["mtime_year"].as_f64().unwrap() >= 2024.0);
}

// ---------------------------------------------------------------------------
// View — sort
// ---------------------------------------------------------------------------

#[test]
fn base_view_sort_ascending() {
    let (dir, db) = fixture();
    let vault = dir.path().join("vault");
    let ctx = QueryContext {
        database: &db,
        vault: &vault,
        current_file: None,
    };
    let eng = engine();
    let result = run_base(
        &eng,
        &ctx,
        r#"
views:
  - type: table
    sort:
      - property: score
        direction: asc
"#,
    );
    let scores_asc: Vec<i64> = result
        .rows
        .iter()
        .filter_map(|r| r["score"].as_i64())
        .collect();
    assert_eq!(scores_asc, vec![1, 3, 5, 8]);
}

#[test]
fn base_view_sort_descending() {
    let (dir, db) = fixture();
    let vault = dir.path().join("vault");
    let ctx = QueryContext {
        database: &db,
        vault: &vault,
        current_file: None,
    };
    let eng = engine();
    let result = run_base(
        &eng,
        &ctx,
        r#"
views:
  - type: table
    sort:
      - property: score
        direction: desc
"#,
    );
    let scores_desc: Vec<i64> = result
        .rows
        .iter()
        .filter_map(|r| r["score"].as_i64())
        .collect();
    assert_eq!(scores_desc, vec![8, 5, 3, 1]);
}

// ---------------------------------------------------------------------------
// View — limit
// ---------------------------------------------------------------------------

#[test]
fn base_view_limit() {
    let (dir, db) = fixture();
    let vault = dir.path().join("vault");
    let ctx = QueryContext {
        database: &db,
        vault: &vault,
        current_file: None,
    };
    let eng = engine();
    let result = run_base(
        &eng,
        &ctx,
        r#"
views:
  - type: table
    sort:
      - property: score
        direction: desc
    limit: 2
"#,
    );
    assert_eq!(result.rows.len(), 2);
    assert_eq!(result.rows[0]["score"].as_i64().unwrap(), 8);
    assert_eq!(result.rows[1]["score"].as_i64().unwrap(), 5);
}

// ---------------------------------------------------------------------------
// View — groupBy
// ---------------------------------------------------------------------------

#[test]
fn base_view_group_by_asc() {
    let (dir, db) = fixture();
    let vault = dir.path().join("vault");
    let ctx = QueryContext {
        database: &db,
        vault: &vault,
        current_file: None,
    };
    let eng = engine();
    let result = run_base(
        &eng,
        &ctx,
        r#"
views:
  - type: table
    groupBy:
      property: color
      direction: asc
"#,
    );
    // Two groups: "blue" and "red"
    assert_eq!(result.rows.len(), 2);
    assert_eq!(
        result.rows[0].get("key").and_then(Value::as_str),
        Some("blue")
    );
    assert_eq!(
        result.rows[1].get("key").and_then(Value::as_str),
        Some("red")
    );
    // Each group has inner rows
    let blue_rows = result.rows[0]["rows"].as_array().unwrap();
    assert_eq!(blue_rows.len(), 2); // beta and delta
    let red_rows = result.rows[1]["rows"].as_array().unwrap();
    assert_eq!(red_rows.len(), 2); // alpha and gamma
}

#[test]
fn base_view_group_by_desc() {
    let (dir, db) = fixture();
    let vault = dir.path().join("vault");
    let ctx = QueryContext {
        database: &db,
        vault: &vault,
        current_file: None,
    };
    let eng = engine();
    let result = run_base(
        &eng,
        &ctx,
        r#"
views:
  - type: table
    groupBy:
      property: color
      direction: desc
"#,
    );
    assert_eq!(result.rows.len(), 2);
    assert_eq!(
        result.rows[0].get("key").and_then(Value::as_str),
        Some("red")
    );
    assert_eq!(
        result.rows[1].get("key").and_then(Value::as_str),
        Some("blue")
    );
}

// ---------------------------------------------------------------------------
// Obsidian docs: Bases syntax > Summaries
// ---------------------------------------------------------------------------

#[test]
fn base_view_summaries_count_sum_average() {
    let (dir, db) = fixture();
    let vault = dir.path().join("vault");
    let ctx = QueryContext {
        database: &db,
        vault: &vault,
        current_file: None,
    };
    let eng = engine();
    let result = run_base(
        &eng,
        &ctx,
        r#"
views:
  - type: table
    summaries:
      score: Count
      rating: Sum
      word_count: Average
"#,
    );
    // 4 notes total
    assert_eq!(result.summaries["score"].as_f64().unwrap(), 4.0);
    // sum of ratings: 4.5 + 2.0 + 3.5 + 1.0 = 11.0
    assert_eq!(result.summaries["rating"].as_f64().unwrap(), 11.0);
    // avg word_count: (120 + 45 + 80 + 10) / 4 = 255 / 4 = 63.75
    assert_eq!(result.summaries["word_count"].as_f64().unwrap(), 63.75);
}

#[test]
fn base_view_summaries_min_max_range() {
    let (dir, db) = fixture();
    let vault = dir.path().join("vault");
    let ctx = QueryContext {
        database: &db,
        vault: &vault,
        current_file: None,
    };
    let eng = engine();
    let result = run_base(
        &eng,
        &ctx,
        r#"
views:
  - type: table
    summaries:
      score: Min
      rating: Max
      word_count: Range
"#,
    );
    assert_eq!(result.summaries["score"].as_f64().unwrap(), 1.0);
    assert_eq!(result.summaries["rating"].as_f64().unwrap(), 4.5);
    // range: 120 - 10 = 110
    assert_eq!(result.summaries["word_count"].as_f64().unwrap(), 110.0);
}

#[test]
fn base_view_summaries_date_range() {
    let (dir, db) = fixture();
    let vault = dir.path().join("vault");
    let ctx = QueryContext {
        database: &db,
        vault: &vault,
        current_file: None,
    };
    let eng = engine();
    let result = run_base(
        &eng,
        &ctx,
        r#"
views:
  - type: table
    summaries:
      due: Range
"#,
    );
    let six_days_ms = 6.0 * 24.0 * 60.0 * 60.0 * 1000.0;
    assert_eq!(result.summaries["due"].as_f64().unwrap(), six_days_ms);
}

#[test]
fn base_view_summaries_median_stddev() {
    let (dir, db) = fixture();
    let vault = dir.path().join("vault");
    let ctx = QueryContext {
        database: &db,
        vault: &vault,
        current_file: None,
    };
    let eng = engine();
    let result = run_base(
        &eng,
        &ctx,
        r#"
views:
  - type: table
    sort:
      - property: score
        direction: asc
    summaries:
      score: Median
      rating: Stddev
"#,
    );
    // scores sorted: 1, 3, 5, 8 → median = (3+5)/2 = 4.0
    assert_eq!(result.summaries["score"].as_f64().unwrap(), 4.0);
    // stddev of ratings: 1.0, 2.0, 3.5, 4.5 → mean=2.75
    // variance = ((1.5625 + 0.5625 + 0.5625 + 3.0625) / 4) = 7.25/4 = 1.8125
    let stddev = result.summaries["rating"].as_f64().unwrap();
    assert!(
        (stddev - 1.8125_f64.sqrt()).abs() < 1e-8,
        "stddev: {stddev}"
    );
}

#[test]
fn base_view_summaries_unique_filled_empty() {
    let (dir, db) = fixture();
    let vault = dir.path().join("vault");
    let ctx = QueryContext {
        database: &db,
        vault: &vault,
        current_file: None,
    };
    let eng = engine();
    let result = run_base(
        &eng,
        &ctx,
        r#"
views:
  - type: table
    summaries:
      color: Unique
      score: Filled
      title: Empty
"#,
    );
    // colors: red, blue, red, blue → 2 unique
    assert_eq!(result.summaries["color"].as_f64().unwrap(), 2.0);
    // all 4 have a score → filled = 4
    assert_eq!(result.summaries["score"].as_f64().unwrap(), 4.0);
    // all 4 have a title → empty = 0
    assert_eq!(result.summaries["title"].as_f64().unwrap(), 0.0);
}

#[test]
fn base_view_summaries_checked_unchecked() {
    let (dir, db) = fixture();
    let vault = dir.path().join("vault");
    let ctx = QueryContext {
        database: &db,
        vault: &vault,
        current_file: None,
    };
    let eng = engine();
    let result = run_base(
        &eng,
        &ctx,
        r#"
views:
  - type: table
    summaries:
      active: Checked
      word_count: Unchecked
"#,
    );
    // alpha and gamma are active=true → Checked = 2
    assert_eq!(result.summaries["active"].as_f64().unwrap(), 2.0);
    // word_count values are all numbers, not booleans → Unchecked = 0
    assert_eq!(result.summaries["word_count"].as_f64().unwrap(), 0.0);
}

// ---------------------------------------------------------------------------
// Document-level custom summaries
// ---------------------------------------------------------------------------

#[test]
fn base_document_custom_summary() {
    let (dir, db) = fixture();
    let vault = dir.path().join("vault");
    let ctx = QueryContext {
        database: &db,
        vault: &vault,
        current_file: None,
    };
    let eng = engine();
    let result = run_base(
        &eng,
        &ctx,
        r#"
summaries:
  DoubleSum: values.reduce(acc + value, 0) * 2
views:
  - type: table
    summaries:
      score: DoubleSum
"#,
    );
    // sum of scores (1+3+5+8=17) * 2 = 34
    assert_eq!(result.summaries["score"].as_f64().unwrap(), 34.0);
}

// ---------------------------------------------------------------------------
// Global filter + view filter both applied
// ---------------------------------------------------------------------------

#[test]
fn base_global_and_view_filters_combined() {
    let (dir, db) = fixture();
    let vault = dir.path().join("vault");
    let ctx = QueryContext {
        database: &db,
        vault: &vault,
        current_file: None,
    };
    let eng = engine();
    let result = run_base(
        &eng,
        &ctx,
        r#"
filters: "active = true"
views:
  - type: table
    filters: "score >= 5"
"#,
    );
    // active=true: alpha(8), gamma(5); score>=5: alpha(8), gamma(5) → both
    let mut s = scores(&result.rows);
    s.sort_unstable();
    assert_eq!(s, vec![5, 8]);
}

// ---------------------------------------------------------------------------
// Formulas referencing each other (multi-pass)
// ---------------------------------------------------------------------------

#[test]
fn base_formula_inter_formula_reference() {
    let (dir, db) = fixture();
    let vault = dir.path().join("vault");
    let ctx = QueryContext {
        database: &db,
        vault: &vault,
        current_file: None,
    };
    let eng = engine();
    let result = run_base(
        &eng,
        &ctx,
        r#"
filters: "score = 8"
formulas:
  base_val: score * 2
  doubled_again: formula.base_val * 2
"#,
    );
    assert_eq!(result.rows.len(), 1);
    let f = &result.rows[0]["formula"];
    assert_eq!(f["base_val"].as_f64().unwrap(), 16.0); // score * 2 = 8 * 2
    assert_eq!(f["doubled_again"].as_f64().unwrap(), 32.0); // base_val * 2 = 16 * 2
}

// ---------------------------------------------------------------------------
// this context
// ---------------------------------------------------------------------------

#[test]
fn base_this_context_is_current_file() {
    let (dir, db) = fixture();
    let vault = dir.path().join("vault");
    let current = vault.join("Work/alpha.md");
    let ctx = QueryContext {
        database: &db,
        vault: &vault,
        current_file: Some(current),
    };
    let eng = engine();
    let result = run_base(
        &eng,
        &ctx,
        r#"
formulas:
  this_name: this.file.name
  this_score: this.score
"#,
    );
    // Every row should see this = alpha's data
    assert!(!result.rows.is_empty());
    for row in &result.rows {
        assert_eq!(row["formula"]["this_name"], "alpha");
        assert_eq!(row["formula"]["this_score"].as_f64().unwrap(), 8.0);
    }
}

// ---------------------------------------------------------------------------
// View — order (column projection)
// ---------------------------------------------------------------------------

#[test]
fn base_view_order_projects_columns() {
    let (dir, db) = fixture();
    let vault = dir.path().join("vault");
    let ctx = QueryContext {
        database: &db,
        vault: &vault,
        current_file: None,
    };
    let eng = engine();
    let result = run_base(
        &eng,
        &ctx,
        r#"
filters: "score = 8"
views:
  - type: table
    order: [title, score]
"#,
    );
    assert_eq!(result.rows.len(), 1);
    let row = &result.rows[0];
    // Only title and score should be present
    assert!(row.contains_key("title"));
    assert!(row.contains_key("score"));
    // Other frontmatter should NOT be projected
    assert!(!row.contains_key("rating"));
    assert!(!row.contains_key("active"));
}
