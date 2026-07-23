use std::fs;

use mdq::compat::CompatibilityEngine;
use mdq::core::QueryContext;
use mdq::db::Database;
use serde_json::Value;

fn fixture() -> (tempfile::TempDir, Database) {
    let directory = tempfile::tempdir().unwrap();
    let vault = directory.path().join("vault");
    fs::create_dir_all(vault.join("Daily")).unwrap();
    fs::write(
        vault.join("Daily/2026-06-14.md"),
        r#"---
created: 2026-06-14
score: 4
tags: [daily, sample]
---
# Daily
- [ ] #task open [due:: 2026-06-14] [priority:: high]
- [x] #task closed [completion:: 2026-06-14]
"#,
    )
    .unwrap();
    fs::write(
        vault.join("Project.md"),
        r#"---
created: 2026-06-13
score: 2
tags: [project]
---
# Project
"#,
    )
    .unwrap();
    let mut database = Database::open(&directory.path().join("index.sqlite3")).unwrap();
    database.rebuild(&vault).unwrap();
    (directory, database)
}

#[test]
fn executes_all_compatibility_languages() {
    let (directory, database) = fixture();
    let vault = directory.path().join("vault");
    let context = QueryContext {
        database: &database,
        vault: &vault,
        current_file: Some(vault.join("Daily/2026-06-14.md")),
    };
    let engine = CompatibilityEngine::standard();

    let tasks = engine
        .execute(
            "tasks",
            &context,
            "not done\ndue on 2026-06-14\ngroup by status.type",
        )
        .unwrap();
    assert_eq!(tasks.rows.len(), 1);
    assert_eq!(tasks.rows[0]["key"], "TODO");

    let base = engine
        .execute(
            "base",
            &context,
            r#"
filters:
  and:
    - file.inFolder("Daily")
    - score >= 3
formulas:
  label: file.name + '!'
views:
  - type: table
    order: [file.name, formula.label]
"#,
        )
        .unwrap();
    assert_eq!(base.rows.len(), 1);
    assert_eq!(base.rows[0]["file.name"], "2026-06-14");

    let dql = engine
        .execute(
            "dataview",
            &context,
            "TABLE file.name AS Name, score FROM \"Daily\" WHERE score >= 3 SORT score DESC",
        )
        .unwrap();
    assert_eq!(dql.rows.len(), 1);
    assert_eq!(dql.rows[0]["Name"], "2026-06-14");

    let dataviewjs = engine
        .execute(
            "dataviewjs",
            &context,
            "dv.taskList(dv.pages('\"Daily\"').flatMap(p => p.file.tasks).where(t => !t.completed));",
        )
        .unwrap();
    assert_eq!(dataviewjs.rows.len(), 1);
    assert_eq!(dataviewjs.rows[0]["render"], "task");

    let dataviewjs_list = engine
        .execute("dataviewjs", &context, "dv.list([1, 2, 3]);")
        .unwrap();
    assert_eq!(dataviewjs_list.rows.len(), 3);
    assert_eq!(dataviewjs_list.rows[0]["value"], 1);
    assert_eq!(dataviewjs_list.rows[1]["value"], 2);
    assert_eq!(dataviewjs_list.rows[2]["value"], 3);

    let dataviewjs_current = engine
        .execute(
            "dataviewjs",
            &context,
            "dv.list([dv.current().file.frontmatter.score, dv.current().note.score]);",
        )
        .unwrap();
    assert_eq!(dataviewjs_current.rows.len(), 2);
    assert_eq!(dataviewjs_current.rows[0]["value"], 4);
    assert_eq!(dataviewjs_current.rows[1]["value"], 4);

    let dataviewjs_keys = engine
        .execute(
            "dataviewjs",
            &context,
            "dv.list(Object.keys(dv.current().file.frontmatter));",
        )
        .unwrap();
    assert!(
        dataviewjs_keys
            .rows
            .iter()
            .any(|row| row["value"] == "score")
    );

    let dataviewjs_tag = engine
        .execute(
            "dataviewjs",
            &context,
            "dv.list(dv.pages('#daily').map(p => p.file.path));",
        )
        .unwrap();
    assert_eq!(dataviewjs_tag.rows.len(), 1);
    assert_eq!(dataviewjs_tag.rows[0]["value"], "Daily/2026-06-14.md");
}

#[test]
fn dataview_where_and_chain_filters_booleans_correctly() {
    let directory = tempfile::tempdir().unwrap();
    let vault = directory.path().join("vault");
    fs::create_dir_all(vault.join("Projects")).unwrap();
    for (name, owner, active, score) in [
        ("Atlas-011", "Alice-Old", true, 8),
        ("Atlas-155", "Alice-Old", true, 1),
        ("Beacon-062", "Alice-Old", false, 8),
        ("Echo-016", "Alice-Old", true, 9),
        ("Other-001", "Alice", true, 10),
    ] {
        fs::write(
            vault.join(format!("Projects/{name}.md")),
            format!(
                r#"---
owner: {owner}
active: {active}
score: {score}
type: project
---
# {name}
"#
            ),
        )
        .unwrap();
    }
    let mut database = Database::open(&directory.path().join("index.sqlite3")).unwrap();
    database.rebuild(&vault).unwrap();
    let context = QueryContext {
        database: &database,
        vault: &vault,
        current_file: None,
    };
    let engine = CompatibilityEngine::standard();

    let dql = engine
        .execute(
            "dataview",
            &context,
            r#"TABLE file.path FROM "Projects" WHERE owner = "Alice-Old" AND active = true AND score >= 8 AND type = "project""#,
        )
        .unwrap();
    let paths: Vec<_> = dql
        .rows
        .iter()
        .map(|row| row["file.path"].as_str().unwrap())
        .collect();

    assert_eq!(paths, ["Projects/Atlas-011.md", "Projects/Echo-016.md"]);
}

#[test]
fn dataview_exposes_inlinks_and_outlinks() {
    let directory = tempfile::tempdir().unwrap();
    let vault = directory.path().join("vault");
    fs::create_dir_all(&vault).unwrap();
    fs::write(vault.join("alpha.md"), "# Alpha\n[[beta]]\n").unwrap();
    fs::write(vault.join("beta.md"), "# Beta\n[[alpha]]\n").unwrap();
    fs::write(vault.join("gamma.md"), "# Gamma\n").unwrap();

    let mut database = Database::open(&directory.path().join("index.sqlite3")).unwrap();
    database.rebuild(&vault).unwrap();
    let context = QueryContext {
        database: &database,
        vault: &vault,
        current_file: None,
    };
    let engine = CompatibilityEngine::standard();

    let dql = engine
        .execute(
            "dataview",
            &context,
            r#"TABLE file.path, length(file.inlinks) AS inlinks, length(file.outlinks) AS outlinks FROM "" SORT file.path"#,
        )
        .unwrap();
    let alpha = dql
        .rows
        .iter()
        .find(|row| row["file.path"] == Value::String("alpha.md".to_owned()))
        .unwrap();
    assert_eq!(alpha["inlinks"].as_f64(), Some(1.0));
    assert_eq!(alpha["outlinks"].as_f64(), Some(1.0));

    let dataviewjs = engine
        .execute(
            "dataviewjs",
            &context,
            r#"dv.table(["path", "inlinks", "outlinks"], dv.pages().sort(p => p.file.path).map(p => [p.file.path, p.file.inlinks.length, p.file.outlinks.length]));"#,
        )
        .unwrap();
    let alpha = dataviewjs
        .rows
        .iter()
        .find(|row| row["value"][0] == Value::String("alpha.md".to_owned()))
        .unwrap();
    assert_eq!(alpha["value"][1].as_f64(), Some(1.0));
    assert_eq!(alpha["value"][2].as_f64(), Some(1.0));
}

#[test]
fn dataview_resolves_this_from_current_file() {
    let (directory, database) = fixture();
    let vault = directory.path().join("vault");
    let context = QueryContext {
        database: &database,
        vault: &vault,
        current_file: Some(vault.join("Daily/2026-06-14.md")),
    };
    let engine = CompatibilityEngine::standard();

    let result = engine
        .execute(
            "dataview",
            &context,
            r#"LIST
WHERE file.path != this.file.path
SORT file.name ASC"#,
        )
        .unwrap();

    let paths: Vec<_> = result
        .rows
        .iter()
        .map(|row| row["file"]["path"].as_str().unwrap())
        .collect();
    assert_eq!(paths, ["Project.md"]);
}
