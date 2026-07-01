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
    fs::create_dir_all(vault.join("Work/Projects")).unwrap();
    fs::create_dir_all(vault.join("Personal")).unwrap();

    fs::write(
        vault.join("Work/Projects/alpha.md"),
r#"---
area: work
reviewed: true
sample_list:
  - Alpha
  - Beta
---
# Alpha
## Plan
- [ ] #task Ship alpha 📅 2026-06-20 ⏳ 2026-06-15 🛫 2026-06-10 ➕ 2026-06-01 🔁 every week 🏁 keep 🔺 🆔 alpha
- [/] #task Review alpha [due:: 2026-06-18] [priority:: high] ⛔ alpha
- [x] #task Archive alpha ✅ 2026-06-19
- [-] #task Cancel alpha ❌ 2026-06-17
  - [ ] #task Nested alpha 📅 2026-06-19 🔼
- [ ] #task Bad alpha 📅 2026-13-40
"#,
    )
    .unwrap();

    fs::write(
        vault.join("Personal/beta.md"),
        r#"# Beta
- [ ] #home Buy milk 📅 2026-06-21 🔽
- [ ] No metadata
1. [ ] Numbered task 📅 2026-06-22
"#,
    )
    .unwrap();

    let mut database = Database::open(&directory.path().join("index.sqlite3")).unwrap();
    database.rebuild(&vault).unwrap();
    (directory, database)
}

fn context<'a>(
    database: &'a Database,
    vault: &'a std::path::Path,
    current_file: Option<std::path::PathBuf>,
) -> QueryContext<'a> {
    QueryContext {
        database,
        vault,
        current_file,
    }
}

fn run_tasks(context: &QueryContext<'_>, source: &str) -> mdq::core::RecordSet {
    CompatibilityEngine::standard()
        .execute("tasks", context, source)
        .unwrap()
}

fn run_tasks_with_statuses(
    context: &QueryContext<'_>,
    source: &str,
    statuses: &[&str],
) -> mdq::core::RecordSet {
    let statuses = statuses
        .iter()
        .map(|status| (*status).to_owned())
        .collect::<Vec<_>>();
    CompatibilityEngine::standard_with_tasks_statuses(&statuses)
        .unwrap()
        .execute("tasks", context, source)
        .unwrap()
}

fn run_tasks_with_settings(
    context: &QueryContext<'_>,
    source: &str,
    statuses: &[&str],
    global_filter: Option<&str>,
    global_query: Option<&str>,
) -> mdq::core::RecordSet {
    let statuses = statuses
        .iter()
        .map(|status| (*status).to_owned())
        .collect::<Vec<_>>();
    CompatibilityEngine::standard_with_tasks_settings(
        &statuses,
        global_filter.map(str::to_owned),
        global_query.map(str::to_owned),
    )
    .unwrap()
    .execute("tasks", context, source)
    .unwrap()
}

fn descriptions(rows: &[mdq::core::Row]) -> Vec<String> {
    rows.iter()
        .map(|row| row["description"].as_str().unwrap().to_owned())
        .collect()
}

// ---------------------------------------------------------------------------
// Obsidian Tasks docs: task extraction and task properties
// ---------------------------------------------------------------------------

#[test]
fn tasks_extracts_checkbox_markers_emoji_fields_inline_fields_and_file_properties() {
    let (dir, db) = fixture();
    let vault = dir.path().join("vault");
    let ctx = context(&db, &vault, None);

    let result = run_tasks(&ctx, "description includes ship alpha");
    assert_eq!(result.rows.len(), 1);
    let row = &result.rows[0];
    assert_eq!(row["due"], "2026-06-20");
    assert_eq!(row["scheduled"], "2026-06-15");
    assert_eq!(row["start"], "2026-06-10");
    assert_eq!(row["created"], "2026-06-01");
    assert_eq!(row["happens"], "2026-06-10");
    assert_eq!(row["recurrence"], "every week");
    assert_eq!(row["recurrenceRule"], "every week");
    assert_eq!(row["onCompletion"], "keep");
    assert_eq!(row["isRecurring"], Value::Bool(true));
    assert_eq!(row["priority"], "highest");
    assert_eq!(row["priorityName"], "highest");
    assert_eq!(row["priorityNumber"].as_i64().unwrap(), 0);
    assert_eq!(row["id"], "alpha");
    assert_eq!(row["heading"], "Plan");
    assert_eq!(row["line"], 10);
    assert_eq!(row["lineNumber"], 9);
    assert_eq!(row["file"]["path"], "Work/Projects/alpha.md");
    assert_eq!(row["file"]["folder"], "Work/Projects/");
    assert_eq!(row["file"]["root"], "Work/");
    assert_eq!(row["file"]["filename"], "alpha.md");
}

#[test]
fn tasks_exposes_tasksdate_and_file_property_api_to_functions() {
    let (dir, db) = fixture();
    let vault = dir.path().join("vault");
    let ctx = context(&db, &vault, None);

    assert_eq!(
        descriptions(
            &run_tasks(
                &ctx,
                "filter by function task.due.format('dddd') === 'Saturday'"
            )
            .rows
        ),
        vec!["#task Ship alpha"]
    );
    assert_eq!(
        descriptions(
            &run_tasks(
                &ctx,
                "filter by function task.due.formatAsDate() === '2026-06-20'"
            )
            .rows
        ),
        vec!["#task Ship alpha"]
    );
    assert_eq!(
        descriptions(
            &run_tasks(
                &ctx,
                "filter by function task.due.moment?.isSame(moment('2026-06-20'), 'day') || false"
            )
            .rows
        ),
        vec!["#task Ship alpha"]
    );
    assert_eq!(
        descriptions(
            &run_tasks(
                &ctx,
                "filter by function task.file.hasProperty('reviewed') && task.file.property('sample_list').includes('Alpha')"
            )
            .rows
        )
        .len(),
        6
    );
    assert_eq!(
        descriptions(
            &run_tasks(
                &ctx,
                "filter by function task.recurrenceRule.includes('week') && task.onCompletion === 'keep'"
            )
            .rows
        ),
        vec!["#task Ship alpha"]
    );
}

#[test]
fn tasks_exposes_link_api_to_functions_and_rows() {
    let directory = tempfile::tempdir().unwrap();
    let vault = directory.path().join("vault");
    fs::create_dir_all(vault.join("Work")).unwrap();
    fs::create_dir_all(vault.join("Refs")).unwrap();
    fs::write(
        vault.join("Work/links.md"),
        r#"---
related: "[[Refs/Yaml]]"
---
Body link [[Refs/Body]]
- [ ] Task link [[Refs/Task]]
"#,
    )
    .unwrap();
    fs::write(vault.join("Refs/Yaml.md"), "# Yaml\n").unwrap();
    fs::write(vault.join("Refs/Body.md"), "# Body\n").unwrap();
    fs::write(vault.join("Refs/Task.md"), "# Task\n").unwrap();
    let mut database = Database::open(&directory.path().join("index.sqlite3")).unwrap();
    database.rebuild(&vault).unwrap();
    let ctx = context(&database, &vault, None);

    let result = run_tasks(
        &ctx,
        "filter by function task.outlinks.map(link => link.destinationPath).includes('Refs/Task.md') && task.file.outlinksInBody.map(link => link.destinationPath).includes('Refs/Body.md') && task.file.outlinksInProperties.map(link => link.destinationPath).includes('Refs/Yaml.md')",
    );
    assert_eq!(descriptions(&result.rows), vec!["Task link [[Refs/Task]]"]);
    let row = &result.rows[0];
    assert_eq!(row["outlinks"][0]["destinationPath"], "Refs/Task.md");
    assert_eq!(
        row["file"]["outlinks"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|link| link["destinationPath"].as_str())
            .collect::<Vec<_>>(),
        vec!["Refs/Yaml.md", "Refs/Body.md", "Refs/Task.md"]
    );
}

#[test]
fn tasks_extracts_frontmatter_links_from_nested_string_values_only() {
    let directory = tempfile::tempdir().unwrap();
    let vault = directory.path().join("vault");
    fs::create_dir_all(vault.join("Work")).unwrap();
    fs::write(
        vault.join("Work/links.md"),
        r#"---
related:
  primary: "[[Refs/Yaml Link]]"
  list:
    - "[[Refs/List Link]]"
plain: Not a link
---
- [ ] Task with frontmatter links
"#,
    )
    .unwrap();
    let mut database = Database::open(&directory.path().join("index.sqlite3")).unwrap();
    database.rebuild(&vault).unwrap();
    let ctx = context(&database, &vault, None);

    let result = run_tasks(
        &ctx,
        "filter by function task.file.outlinksInProperties.map(link => link.destinationPath).includes('Refs/Yaml Link.md') && task.file.outlinksInProperties.map(link => link.destinationPath).includes('Refs/List Link.md')",
    );
    assert_eq!(
        descriptions(&result.rows),
        vec!["Task with frontmatter links"]
    );
}

#[test]
fn tasks_extracts_numbered_list_tasks_and_source_markdown_properties() {
    let (dir, db) = fixture();
    let vault = dir.path().join("vault");
    let ctx = context(&db, &vault, None);

    let result = run_tasks(&ctx, "description includes numbered task");
    assert_eq!(result.rows.len(), 1);
    let row = &result.rows[0];
    assert_eq!(row["listMarker"], "1.");
    assert!(
        row["originalMarkdown"]
            .as_str()
            .unwrap()
            .starts_with("1. [ ]")
    );
    assert_eq!(row["due"], "2026-06-22");
}

// ---------------------------------------------------------------------------
// Obsidian Tasks docs: filters for task statuses
// ---------------------------------------------------------------------------

#[test]
fn tasks_filters_done_and_not_done_by_status_type() {
    let (dir, db) = fixture();
    let vault = dir.path().join("vault");
    let ctx = context(&db, &vault, None);

    let done = run_tasks(&ctx, "done");
    assert_eq!(
        descriptions(&done.rows),
        vec!["#task Archive alpha", "#task Cancel alpha"]
    );

    let not_done = run_tasks(&ctx, "not done");
    assert_eq!(not_done.rows.len(), 7);
    assert!(descriptions(&not_done.rows).contains(&"#task Review alpha".to_owned()));
    assert!(!descriptions(&not_done.rows).contains(&"#task Cancel alpha".to_owned()));
}

#[test]
fn tasks_filters_status_name_and_status_type() {
    let (dir, db) = fixture();
    let vault = dir.path().join("vault");
    let ctx = context(&db, &vault, None);

    let in_progress = run_tasks(&ctx, "status.type is IN_PROGRESS");
    assert_eq!(descriptions(&in_progress.rows), vec!["#task Review alpha"]);

    let not_cancelled = run_tasks(&ctx, "status.type is not CANCELLED");
    assert_eq!(not_cancelled.rows.len(), 8);

    let named = run_tasks(&ctx, "status.name includes progress");
    assert_eq!(descriptions(&named.rows), vec!["#task Review alpha"]);
}

#[test]
fn tasks_preserves_arbitrary_unknown_status_symbols_without_vault_settings() {
    let directory = tempfile::tempdir().unwrap();
    let vault = directory.path().join("vault");
    fs::create_dir_all(&vault).unwrap();
    fs::write(
        vault.join("custom.md"),
        "- [?] Needs triage\n- [P] Waiting for person\n- [x] Finished\n",
    )
    .unwrap();
    let mut database = Database::open(&directory.path().join("index.sqlite3")).unwrap();
    database.rebuild(&vault).unwrap();
    let ctx = context(&database, &vault, None);

    let unknown = run_tasks(&ctx, "status.name is Unknown\nsort by description");
    assert_eq!(
        descriptions(&unknown.rows),
        vec!["Needs triage", "Waiting for person"]
    );
    assert_eq!(unknown.rows[0]["status"]["symbol"], "?");
    assert_eq!(unknown.rows[1]["status"]["symbol"], "P");
    assert_eq!(unknown.rows[0]["status"]["type"], "TODO");
    assert_eq!(unknown.rows[0]["status"]["nextSymbol"], "x");

    assert_eq!(descriptions(&run_tasks(&ctx, "not done").rows).len(), 2);
    assert_eq!(
        descriptions(&run_tasks(&ctx, "done").rows),
        vec!["Finished"]
    );
}

#[test]
fn tasks_accepts_cli_supplied_status_names_types_and_next_symbols() {
    let directory = tempfile::tempdir().unwrap();
    let vault = directory.path().join("vault");
    fs::create_dir_all(&vault).unwrap();
    fs::write(
        vault.join("custom.md"),
        "- [?] Needs triage\n- [P] Waiting for person\n- [>] Delegated\n",
    )
    .unwrap();
    let mut database = Database::open(&directory.path().join("index.sqlite3")).unwrap();
    database.rebuild(&vault).unwrap();
    let ctx = context(&database, &vault, None);

    let triage = run_tasks_with_statuses(
        &ctx,
        "status.name is Needs Triage",
        &["?=Needs Triage:ON_HOLD"],
    );
    assert_eq!(descriptions(&triage.rows), vec!["Needs triage"]);
    assert_eq!(triage.rows[0]["status"]["type"], "ON_HOLD");

    let delegated = run_tasks_with_statuses(
        &ctx,
        "status.name includes delegated",
        &[">=Delegated:NON_TASK:space"],
    );
    assert_eq!(descriptions(&delegated.rows), vec!["Delegated"]);
    assert_eq!(delegated.rows[0]["status"]["nextSymbol"], " ");

    let done_alias = run_tasks_with_statuses(&ctx, "done", &["P=DONE"]);
    assert_eq!(descriptions(&done_alias.rows), vec!["Waiting for person"]);
}

#[test]
fn tasks_accepts_cli_supplied_global_filter_and_global_query() {
    let directory = tempfile::tempdir().unwrap();
    let vault = directory.path().join("vault");
    fs::create_dir_all(&vault).unwrap();
    fs::write(
        vault.join("tasks.md"),
        "- [ ] #task Work item 📅 2026-06-20\n- [ ] Personal item 📅 2026-06-19\n- [x] #task Done item\n",
    )
    .unwrap();
    let mut database = Database::open(&directory.path().join("index.sqlite3")).unwrap();
    database.rebuild(&vault).unwrap();
    let ctx = context(&database, &vault, None);

    let global_filter = run_tasks_with_settings(&ctx, "", &[], Some("#task"), None);
    assert_eq!(
        descriptions(&global_filter.rows),
        vec!["Work item", "Done item"]
    );
    assert_eq!(global_filter.rows[0]["descriptionWithoutTags"], "Work item");
    assert_eq!(global_filter.rows[0]["tags"].as_array().unwrap().len(), 0);

    let global_query = run_tasks_with_settings(&ctx, "not done", &[], None, Some("has due date"));
    assert_eq!(
        descriptions(&global_query.rows),
        vec!["Personal item", "#task Work item"]
    );

    let ignored = run_tasks_with_settings(
        &ctx,
        "not done\nignore global query",
        &[],
        None,
        Some("description includes impossible"),
    );
    assert_eq!(
        descriptions(&ignored.rows),
        vec!["Personal item", "#task Work item"]
    );

    let global_query_comment =
        run_tasks_with_settings(&ctx, "not done", &[], None, Some("{{! global comment }}"));
    assert_eq!(
        descriptions(&global_query_comment.rows),
        vec!["Personal item", "#task Work item"]
    );
}

#[test]
fn tasks_does_not_warn_for_missing_default_sort_fields() {
    let (dir, db) = fixture();
    let vault = dir.path().join("vault");
    let ctx = context(&db, &vault, None);

    let result = run_tasks(&ctx, "description includes cancel alpha");

    assert_eq!(descriptions(&result.rows), vec!["#task Cancel alpha"]);
    assert!(result.diagnostics.is_empty());
}

#[test]
fn tasks_keeps_successful_function_filter_results_clean_when_some_rows_error() {
    let (dir, db) = fixture();
    let vault = dir.path().join("vault");
    let ctx = context(&db, &vault, None);

    let result = run_tasks(
        &ctx,
        "filter by function task.file.property('sample_list').includes('Alpha')",
    );

    assert_eq!(result.rows.len(), 6);
    assert!(result.diagnostics.is_empty());
}

// ---------------------------------------------------------------------------
// Obsidian Tasks docs: filters for dates in tasks
// ---------------------------------------------------------------------------

#[test]
fn tasks_filters_date_presence_and_comparison_for_all_supported_date_fields() {
    let (dir, db) = fixture();
    let vault = dir.path().join("vault");
    let ctx = context(&db, &vault, None);

    assert_eq!(run_tasks(&ctx, "has due date").rows.len(), 6);
    assert_eq!(run_tasks(&ctx, "no due date").rows.len(), 3);
    assert_eq!(
        descriptions(&run_tasks(&ctx, "scheduled before 2026-06-16").rows),
        vec!["#task Ship alpha"]
    );
    assert_eq!(
        descriptions(&run_tasks(&ctx, "starts on or after 2026-06-10").rows),
        vec!["#task Ship alpha"]
    );
    assert_eq!(
        descriptions(&run_tasks(&ctx, "done on 2026-06-19").rows),
        vec!["#task Archive alpha"]
    );
    assert_eq!(
        descriptions(&run_tasks(&ctx, "cancelled on 2026-06-17").rows),
        vec!["#task Cancel alpha"]
    );
    assert_eq!(
        descriptions(&run_tasks(&ctx, "created 2026-06-01").rows),
        vec!["#task Ship alpha"]
    );
}

#[test]
fn tasks_filters_happens_by_earliest_start_scheduled_or_due_date() {
    let (dir, db) = fixture();
    let vault = dir.path().join("vault");
    let ctx = context(&db, &vault, None);

    let result = run_tasks(&ctx, "happens before 2026-06-11");
    assert_eq!(descriptions(&result.rows), vec!["#task Ship alpha"]);
}

#[test]
fn tasks_filters_flexible_english_dates_and_start_undated_tasks() {
    let (dir, db) = fixture();
    let vault = dir.path().join("vault");
    let ctx = context(&db, &vault, None);

    assert_eq!(
        descriptions(&run_tasks(&ctx, "due on June 20, 2026").rows),
        vec!["#task Ship alpha"]
    );
    assert_eq!(
        descriptions(&run_tasks(&ctx, "due on 20th June 2026").rows),
        vec!["#task Ship alpha"]
    );

    let no_start_or_before = run_tasks(&ctx, "starts before 2000-01-01");
    assert_eq!(no_start_or_before.rows.len(), 8);
    assert!(!descriptions(&no_start_or_before.rows).contains(&"#task Ship alpha".to_owned()));
    assert!(descriptions(&no_start_or_before.rows).contains(&"#task Review alpha".to_owned()));

    assert!(
        run_tasks(&ctx, "(starts before 2000-01-01) AND (has start date)")
            .rows
            .is_empty()
    );
}

// ---------------------------------------------------------------------------
// Obsidian Tasks docs: text, dependency, and file-property filters
// ---------------------------------------------------------------------------

#[test]
fn tasks_filters_text_tags_priority_id_and_dependencies() {
    let (dir, db) = fixture();
    let vault = dir.path().join("vault");
    let ctx = context(&db, &vault, None);

    assert_eq!(run_tasks(&ctx, "tag includes #home").rows.len(), 1);
    assert_eq!(run_tasks(&ctx, "tags do not include #task").rows.len(), 3);
    assert_eq!(
        descriptions(&run_tasks(&ctx, "description includes buy milk").rows),
        vec!["#home Buy milk"]
    );
    assert_eq!(run_tasks(&ctx, "is recurring").rows.len(), 1);
    assert_eq!(run_tasks(&ctx, "is not recurring").rows.len(), 8);
    assert_eq!(run_tasks(&ctx, "has id").rows.len(), 1);
    assert_eq!(run_tasks(&ctx, "has depends on").rows.len(), 1);
    assert_eq!(
        descriptions(
            &run_tasks(
                &ctx,
                "filter by function task.dependsOn && task.dependsOn.includes('alpha')"
            )
            .rows
        ),
        vec!["#task Review alpha"]
    );
    assert_eq!(
        descriptions(&run_tasks(&ctx, "id includes alpha").rows),
        vec!["#task Ship alpha"]
    );
    assert_eq!(
        descriptions(&run_tasks(&ctx, "priority is high").rows),
        vec!["#task Review alpha"]
    );
    assert_eq!(
        descriptions(&run_tasks(&ctx, "descriptionWithoutTags includes ship alpha").rows),
        vec!["#task Ship alpha"]
    );
    assert_eq!(
        descriptions(&run_tasks(&ctx, "onCompletion includes keep").rows),
        vec!["#task Ship alpha"]
    );
    let review = run_tasks(&ctx, "description includes review alpha");
    let row = &review.rows[0];
    assert_eq!(row["priority"], "high");
    assert_eq!(row["priorityName"], "high");
    assert_eq!(row["priorityNumber"].as_i64().unwrap(), 1);
    assert_eq!(row["priorityGroup"], "High priority");
}

#[test]
fn tasks_extracts_unicode_tags_like_obsidian() {
    let directory = tempfile::tempdir().unwrap();
    let vault = directory.path().join("vault");
    fs::create_dir_all(&vault).unwrap();
    fs::write(
        vault.join("unicode.md"),
        "- [ ] #タスク 日本語のタスク 📅 2026-07-01\n- [ ] #task Latin task\n",
    )
    .unwrap();
    let mut database = Database::open(&directory.path().join("index.sqlite3")).unwrap();
    database.rebuild(&vault).unwrap();
    let ctx = context(&database, &vault, None);

    let result = run_tasks(&ctx, "tags include #タスク");
    assert_eq!(descriptions(&result.rows), vec!["#タスク 日本語のタスク"]);
    assert_eq!(result.rows[0]["tags"], serde_json::json!(["#タスク"]));
    assert_eq!(result.rows[0]["descriptionWithoutTags"], "日本語のタスク");
}

#[test]
fn tasks_filters_file_path_root_folder_filename_and_heading() {
    let (dir, db) = fixture();
    let vault = dir.path().join("vault");
    let ctx = context(&db, &vault, None);

    assert_eq!(run_tasks(&ctx, "path includes Work/Projects").rows.len(), 6);
    assert_eq!(run_tasks(&ctx, "root includes Work").rows.len(), 6);
    assert_eq!(
        run_tasks(&ctx, "folder includes Work/Projects").rows.len(),
        6
    );
    assert_eq!(run_tasks(&ctx, "filename includes alpha").rows.len(), 6);
    assert_eq!(run_tasks(&ctx, "heading includes plan").rows.len(), 6);
}

#[test]
fn tasks_expands_query_file_placeholders() {
    let (dir, db) = fixture();
    let vault = dir.path().join("vault");
    let current = vault.join("Work/Projects/alpha.md");
    let ctx = context(&db, &vault, Some(current));

    assert_eq!(
        run_tasks(&ctx, "path includes {{query.file.pathWithoutExtension}}")
            .rows
            .len(),
        6
    );
    assert_eq!(
        run_tasks(&ctx, "folder includes {{query.file.folder}}")
            .rows
            .len(),
        6
    );
    assert_eq!(
        run_tasks(&ctx, "root includes {{query.file.root}}")
            .rows
            .len(),
        6
    );
    assert_eq!(
        run_tasks(
            &ctx,
            "filename includes {{query.file.filenameWithoutExtension}}"
        )
        .rows
        .len(),
        6
    );
}

// ---------------------------------------------------------------------------
// Obsidian Tasks docs: boolean filters, sorting, grouping, limits, functions
// ---------------------------------------------------------------------------

#[test]
fn tasks_combines_boolean_filters() {
    let (dir, db) = fixture();
    let vault = dir.path().join("vault");
    let ctx = context(&db, &vault, None);

    let any = run_tasks(
        &ctx,
        "(description includes buy milk) OR (status.type is IN_PROGRESS)",
    );
    assert_eq!(any.rows.len(), 2);

    let all = run_tasks(&ctx, "(tag includes #task) AND (has due date)");
    assert_eq!(all.rows.len(), 4);
}

#[test]
fn tasks_sorts_groups_and_limits() {
    let (dir, db) = fixture();
    let vault = dir.path().join("vault");
    let ctx = context(&db, &vault, None);

    let sorted = run_tasks(&ctx, "has due date\nsort by due\nlimit 2");
    assert_eq!(
        descriptions(&sorted.rows),
        vec!["#task Bad alpha", "#task Review alpha"]
    );
    assert_eq!(
        descriptions(&run_tasks(&ctx, "has due date\nsort by due\nlimit to 2 tasks").rows),
        vec!["#task Bad alpha", "#task Review alpha"]
    );

    let grouped = run_tasks(&ctx, "group by status.type");
    assert_eq!(grouped.rows.len(), 4);
    assert_eq!(
        grouped
            .rows
            .iter()
            .map(|row| row["key"].as_str().unwrap())
            .collect::<Vec<_>>(),
        vec!["IN_PROGRESS", "TODO", "DONE", "CANCELLED"]
    );

    let sorted_status_type = run_tasks(&ctx, "sort by status.type");
    assert_eq!(
        descriptions(&sorted_status_type.rows).first().unwrap(),
        "#task Review alpha"
    );

    let grouped_status = run_tasks(&ctx, "group by status");
    assert!(grouped_status.rows.iter().any(|row| row["key"] == "Done"));
    assert!(grouped_status.rows.iter().any(|row| row["key"] == "Todo"));

    let grouped_recurring = run_tasks(&ctx, "group by recurring");
    assert!(
        grouped_recurring
            .rows
            .iter()
            .any(|row| row["key"] == "Recurring")
    );

    let grouped_tags = run_tasks(&ctx, "group by tags");
    assert!(grouped_tags.rows.iter().any(|row| row["key"] == "#task"));
    assert!(grouped_tags.rows.iter().any(|row| row["key"] == "#home"));
    assert!(
        grouped_tags
            .rows
            .iter()
            .any(|row| row["key"] == "(No tags)")
    );

    let grouped_file = run_tasks(&ctx, "group by filename\ngroup by backlink");
    assert!(grouped_file.rows.iter().any(|row| row["key"] == "alpha"));

    let grouped_priority = run_tasks(&ctx, "group by priority");
    assert!(
        grouped_priority
            .rows
            .iter()
            .any(|row| row["key"] == "Highest priority")
    );

    let grouped_recurrence = run_tasks(&ctx, "group by recurrence");
    assert!(
        grouped_recurrence
            .rows
            .iter()
            .any(|row| row["key"] == "every week")
    );

    let limited_groups = run_tasks(&ctx, "group by status.type\nlimit groups to 1 tasks");
    assert!(
        limited_groups
            .rows
            .iter()
            .all(|row| { row["rows"].as_array().is_some_and(|tasks| tasks.len() <= 1) })
    );

    let grouped_due = run_tasks(&ctx, "group by due");
    assert_eq!(grouped_due.rows[0]["key"], "Invalid due date");
    assert!(
        grouped_due
            .rows
            .iter()
            .any(|row| row["key"] == "No due date")
    );

    let no_group = run_tasks(&ctx, "limit groups to 1 tasks");
    assert_eq!(
        no_group.diagnostics,
        vec!["limit groups has no effect without a group by instruction"]
    );
}

#[test]
fn tasks_multiple_sorts_do_not_panic_on_missing_values() {
    let (dir, db) = fixture();
    let vault = dir.path().join("vault");
    let ctx = context(&db, &vault, None);

    let result = run_tasks(&ctx, "not done\nsort by due reverse\nsort by priority");

    assert_eq!(result.rows.len(), 7);
}

#[test]
fn tasks_supports_function_filters_sorts_and_groups() {
    let (dir, db) = fixture();
    let vault = dir.path().join("vault");
    let ctx = context(&db, &vault, None);

    let filtered = run_tasks(
        &ctx,
        "filter by function task.file.folder === 'Work/Projects/' && task.tags.includes('#task')",
    );
    assert_eq!(filtered.rows.len(), 6);

    let sorted = run_tasks(
        &ctx,
        "filter by function task.due.moment && task.due.moment.isValid()\nsort by function task.due.formatAsDate()",
    );
    assert_eq!(
        descriptions(&sorted.rows),
        vec![
            "#task Review alpha",
            "#task Nested alpha",
            "#task Ship alpha",
            "#home Buy milk",
            "Numbered task"
        ]
    );

    let grouped = run_tasks(&ctx, "group by function task.file.root");
    assert_eq!(grouped.rows.len(), 2);
}

#[test]
fn tasks_supports_find_closest_parent_task_in_functions() {
    let directory = tempfile::tempdir().unwrap();
    let vault = directory.path().join("vault");
    fs::create_dir_all(&vault).unwrap();
    fs::write(
        vault.join("parents.md"),
        r#"- [ ] Parent #context/home
  - [ ] Child without own tag
- [ ] Orphan
"#,
    )
    .unwrap();
    let mut database = Database::open(&directory.path().join("index.sqlite3")).unwrap();
    database.rebuild(&vault).unwrap();
    let ctx = context(&database, &vault, None);

    let result = run_tasks(
        &ctx,
        "description includes child\ngroup by function task.tags.length > 0 ? task.tags : task.findClosestParentTask()?.tags ?? []",
    );
    assert_eq!(result.rows.len(), 1);
    assert_eq!(result.rows[0]["key"], "#context/home");
    assert_eq!(
        result.rows[0]["rows"][0]["description"],
        "Child without own tag"
    );

    assert_eq!(
        descriptions(
            &run_tasks(
                &ctx,
                "filter by function task.findClosestParentTask()?.description === 'Parent #context/home'"
            )
            .rows
        ),
        vec!["Child without own tag"]
    );
}

#[test]
fn tasks_reports_unsupported_query_instructions_as_diagnostics() {
    let (dir, db) = fixture();
    let vault = dir.path().join("vault");
    let ctx = context(&db, &vault, None);

    let result = run_tasks(&ctx, "unknown instruction");
    assert!(result.rows.is_empty());
    assert_eq!(
        result.diagnostics,
        vec!["unsupported Tasks instruction: unknown instruction"]
    );
}

#[test]
fn tasks_reports_invalid_status_type_as_diagnostic() {
    let (dir, db) = fixture();
    let vault = dir.path().join("vault");
    let ctx = context(&db, &vault, None);

    let result = run_tasks(&ctx, "status.type is HACKED");

    assert!(result.rows.is_empty());
    assert_eq!(
        result.diagnostics,
        vec!["unsupported Tasks instruction: status.type is HACKED"]
    );
}

#[test]
fn tasks_reports_function_filter_errors_as_diagnostics() {
    let (dir, db) = fixture();
    let vault = dir.path().join("vault");
    let ctx = context(&db, &vault, None);

    let result = run_tasks(
        &ctx,
        "filter by function (() => { throw new Error('boom') })()",
    );

    assert!(result.rows.is_empty());
    assert!(
        result
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.contains("Tasks function filter failed"))
    );
}

#[test]
fn tasks_sorts_by_tag_index_and_relative_date_ranges() {
    let (dir, db) = fixture();
    let vault = dir.path().join("vault");
    let ctx = context(&db, &vault, None);

    let sorted = run_tasks(&ctx, "has tags\nsort by tag");
    assert_eq!(
        descriptions(&sorted.rows).first().unwrap(),
        "#home Buy milk"
    );

    let this_year = run_tasks(&ctx, "due in this year");
    assert!(this_year.diagnostics.is_empty());
}

#[test]
fn tasks_sorts_by_urgency_random_and_reverse_function() {
    let (dir, db) = fixture();
    let vault = dir.path().join("vault");
    let ctx = context(&db, &vault, None);

    let urgent = run_tasks(&ctx, "not done\nsort by urgency");
    assert_eq!(
        descriptions(&urgent.rows).first().unwrap(),
        "#task Ship alpha"
    );
    assert!(urgent.rows[0]["urgency"].as_f64().unwrap() > 0.0);

    let random = run_tasks(&ctx, "not done\nsort by random");
    assert_eq!(random.rows.len(), 7);
    assert!(
        random
            .rows
            .iter()
            .all(|row| row["random"].as_i64().is_some())
    );

    let reversed_function = run_tasks(
        &ctx,
        "filter by function task.due.moment && task.due.moment.isValid()\nsort by function reverse task.due.formatAsDate()\nlimit 1",
    );
    assert_eq!(descriptions(&reversed_function.rows), vec!["Numbered task"]);
}

#[test]
fn tasks_dependency_state_ignores_completed_or_missing_dependencies() {
    let directory = tempfile::tempdir().unwrap();
    let vault = directory.path().join("vault");
    fs::create_dir_all(&vault).unwrap();
    fs::write(
        vault.join("deps.md"),
        r#"- [ ] Active blocker 🆔 active
- [ ] Blocked active ⛔ active
- [x] Done dependency target 🆔 done ✅ 2026-06-01
- [ ] Depends on done ⛔ done
- [x] Completed blocked ⛔ active ✅ 2026-06-02
- [ ] Missing dependency ⛔ missing
"#,
    )
    .unwrap();
    let mut database = Database::open(&directory.path().join("index.sqlite3")).unwrap();
    database.rebuild(&vault).unwrap();
    let ctx = context(&database, &vault, None);

    assert_eq!(
        descriptions(&run_tasks(&ctx, "is blocked").rows),
        vec!["Blocked active"]
    );
    assert_eq!(
        descriptions(&run_tasks(&ctx, "is blocking").rows),
        vec!["Active blocker"]
    );
}

#[test]
fn tasks_filters_date_ranges_relative_ranges_and_invalid_dates() {
    let (dir, db) = fixture();
    let vault = dir.path().join("vault");
    let ctx = context(&db, &vault, None);

    assert_eq!(
        descriptions(&run_tasks(&ctx, "due 2026-06-18 2026-06-20").rows),
        vec![
            "#task Review alpha",
            "#task Ship alpha",
            "#task Nested alpha"
        ]
    );
    assert_eq!(
        descriptions(&run_tasks(&ctx, "due in 2026-W25").rows),
        vec![
            "#task Review alpha",
            "#task Ship alpha",
            "#task Nested alpha",
            "#home Buy milk"
        ]
    );
    assert_eq!(
        descriptions(&run_tasks(&ctx, "due in 2026-06").rows),
        vec![
            "#task Review alpha",
            "#task Ship alpha",
            "#task Nested alpha",
            "Numbered task",
            "#home Buy milk"
        ]
    );
    assert_eq!(
        descriptions(&run_tasks(&ctx, "due date is invalid").rows),
        vec!["#task Bad alpha"]
    );
    assert_eq!(
        descriptions(&run_tasks(&ctx, "due 2026-06-18 2026-99-99").rows),
        vec!["#task Review alpha"]
    );
    assert_eq!(
        descriptions(&run_tasks(&ctx, "due 2026-99-99 2026-06-20").rows),
        vec!["#task Ship alpha"]
    );
}

#[test]
fn tasks_filters_regex_priority_recurrence_tags_and_sub_items() {
    let (dir, db) = fixture();
    let vault = dir.path().join("vault");
    let ctx = context(&db, &vault, None);

    assert_eq!(
        descriptions(&run_tasks(&ctx, "description regex matches /ship|buy/i").rows),
        vec!["#task Ship alpha", "#home Buy milk"]
    );
    assert_eq!(run_tasks(&ctx, "priority is above none").rows.len(), 3);
    assert_eq!(
        descriptions(&run_tasks(&ctx, "priority is medium").rows),
        vec!["#task Nested alpha"]
    );
    assert_eq!(
        descriptions(&run_tasks(&ctx, "recurrence includes week").rows),
        vec!["#task Ship alpha"]
    );
    assert_eq!(run_tasks(&ctx, "has tags").rows.len(), 7);
    assert_eq!(run_tasks(&ctx, "no tags").rows.len(), 2);
    assert_eq!(run_tasks(&ctx, "exclude sub-items").rows.len(), 8);

    let unsupported_flag = run_tasks(&ctx, "tags regex matches /task/g");
    assert!(unsupported_flag.rows.is_empty());
    assert_eq!(
        unsupported_flag.diagnostics,
        vec!["unsupported Tasks instruction: tags regex matches /task/g"]
    );
}

#[test]
fn tasks_filters_boolean_not_xor_and_dependencies() {
    let (dir, db) = fixture();
    let vault = dir.path().join("vault");
    let ctx = context(&db, &vault, None);

    assert_eq!(
        descriptions(&run_tasks(&ctx, "NOT (description includes alpha)").rows),
        vec!["Numbered task", "#home Buy milk", "No metadata"]
    );
    assert_eq!(
        descriptions(
            &run_tasks(
                &ctx,
                "(tag includes #task) AND NOT (description includes nested)"
            )
            .rows
        ),
        vec![
            "#task Review alpha",
            "#task Ship alpha",
            "#task Bad alpha",
            "#task Archive alpha",
            "#task Cancel alpha"
        ]
    );
    assert_eq!(
        descriptions(
            &run_tasks(
                &ctx,
                "(description includes buy) XOR (description includes numbered)"
            )
            .rows
        ),
        vec!["Numbered task", "#home Buy milk"]
    );
    assert_eq!(
        descriptions(&run_tasks(&ctx, "is blocked").rows),
        vec!["#task Review alpha"]
    );
    assert_eq!(
        descriptions(&run_tasks(&ctx, "is blocking").rows),
        vec!["#task Ship alpha"]
    );
    assert_eq!(
        descriptions(
            &run_tasks(
                &ctx,
                "filter by function task.isBlocked(query.allTasks) || task.isBlocking(query.allTasks)"
            )
            .rows
        ),
        vec!["#task Review alpha", "#task Ship alpha"]
    );
}

#[test]
fn tasks_applies_comments_layout_directives_and_nested_groups() {
    let (dir, db) = fixture();
    let vault = dir.path().join("vault");
    let ctx = context(&db, &vault, None);

    let commented = run_tasks(
        &ctx,
        "not done {{! inline comment }}\nhide tags\nshow urgency\nfull mode\nexplain",
    );
    assert_eq!(commented.rows.len(), 7);

    let comment_after_text = run_tasks(
        &ctx,
        "description includes ship alpha {{! comment after a text filter }}",
    );
    assert_eq!(
        descriptions(&comment_after_text.rows),
        vec!["#task Ship alpha"]
    );

    let ignore_global_query_with_comment = run_tasks_with_settings(
        &ctx,
        "ignore global query {{! trailing comment }}",
        &[],
        None,
        Some("description includes ship alpha"),
    );
    assert_eq!(ignore_global_query_with_comment.rows.len(), 9);

    let grouped = run_tasks(&ctx, "group by status.type\ngroup by file.root");
    let todo = grouped
        .rows
        .iter()
        .find(|row| row["key"] == "TODO")
        .expect("TODO group");
    assert!(
        todo["rows"]
            .as_array()
            .unwrap()
            .iter()
            .any(|row| row["key"] == "Work/")
    );
}

#[test]
fn tasks_extracts_blockquote_tasks_punctuation_tags_and_strips_closing_heading_markers() {
    let directory = tempfile::tempdir().unwrap();
    let vault = directory.path().join("vault");
    fs::create_dir_all(&vault).unwrap();
    fs::write(
        vault.join("quoted.md"),
        r#"# Root
## Plan ##
> - [ ] (#paren/tag) Quoted task
> > - [ ] #nested Deep quote
- [ ] (#plain/tag) Plain punctuation tag
"#,
    )
    .unwrap();
    let mut database = Database::open(&directory.path().join("index.sqlite3")).unwrap();
    database.rebuild(&vault).unwrap();
    let ctx = context(&database, &vault, None);

    assert_eq!(
        descriptions(&run_tasks(&ctx, "description includes quoted task").rows),
        vec!["(#paren/tag) Quoted task"]
    );
    assert_eq!(
        descriptions(&run_tasks(&ctx, "tag includes #paren/tag").rows),
        vec!["(#paren/tag) Quoted task"]
    );
    assert_eq!(
        descriptions(&run_tasks(&ctx, "tag includes #plain/tag").rows),
        vec!["(#plain/tag) Plain punctuation tag"]
    );
    assert_eq!(run_tasks(&ctx, "heading includes Plan").rows.len(), 3);
    assert!(run_tasks(&ctx, "heading includes Plan ##").rows.is_empty());
}
