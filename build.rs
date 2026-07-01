use std::env;
use std::fs;
use std::path::PathBuf;

fn main() {
    println!("cargo:rerun-if-changed=src/compat/tasks.pest");

    let grammar =
        fs::read_to_string("src/compat/tasks.pest").expect("failed to read src/compat/tasks.pest");
    let manual = grammar
        .lines()
        .filter_map(|line| line.trim_start().strip_prefix("// mdq-doc:"))
        .map(|line| line.trim_start())
        .collect::<Vec<_>>()
        .join("\n");

    assert!(
        !manual.trim().is_empty(),
        "src/compat/tasks.pest must contain // mdq-doc: manual lines"
    );

    let output = format!("const GENERATED_TASKS_MANUAL: &str = {manual:?};\n");
    let out_dir = PathBuf::from(env::var("OUT_DIR").expect("OUT_DIR is not set"));
    fs::write(out_dir.join("tasks_manual.rs"), output)
        .expect("failed to write generated Tasks manual");
}
