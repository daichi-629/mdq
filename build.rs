use std::env;
use std::fs;
use std::path::PathBuf;

fn main() {
    println!("cargo:rerun-if-changed=src/compat/tasks.pest");
    println!("cargo:rerun-if-changed=src/compat/base.pest");
    println!("cargo:rerun-if-changed=src/compat/dataview.pest");
    println!("cargo:rerun-if-changed=src/filter.pest");

    let output = format!(
        "{}{}{}{}",
        generated_manual("GENERATED_TASKS_MANUAL", "src/compat/tasks.pest"),
        generated_manual("GENERATED_BASE_EXPR_MANUAL", "src/compat/base.pest"),
        generated_manual("GENERATED_DATAVIEW_EXPR_MANUAL", "src/compat/dataview.pest"),
        generated_manual("GENERATED_NATIVE_MANUAL", "src/filter.pest"),
    );
    let out_dir = PathBuf::from(env::var("OUT_DIR").expect("OUT_DIR is not set"));
    fs::write(out_dir.join("generated_manual.rs"), output)
        .expect("failed to write generated manual");
}

fn generated_manual(const_name: &str, path: &str) -> String {
    let grammar =
        fs::read_to_string(path).unwrap_or_else(|error| panic!("failed to read {path}: {error}"));
    let manual = grammar
        .lines()
        .filter_map(|line| line.trim_start().strip_prefix("// mdq-doc:"))
        .map(|line| line.trim_start())
        .collect::<Vec<_>>()
        .join("\n");

    assert!(
        !manual.trim().is_empty(),
        "{path} must contain // mdq-doc: manual lines"
    );

    format!("const {const_name}: &str = {manual:?};\n")
}
