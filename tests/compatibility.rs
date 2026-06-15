use std::fs;

use mdq::compat::CompatibilityEngine;
use mdq::core::QueryContext;
use mdq::db::Database;

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
}
