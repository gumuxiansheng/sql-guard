use std::process::Command;
use std::path::Path;

const BINARY: &str = "target/release/sqlguard";

fn setup_test_project(dir: &str) {
    std::fs::create_dir_all(format!("{}/sql/ddl", dir)).unwrap();
    std::fs::create_dir_all(format!("{}/sql/dml", dir)).unwrap();
    std::fs::create_dir_all(format!("{}/sql/others", dir)).unwrap();

    std::fs::write(
        format!("{}/sql/ddl/create_users.sql", dir),
        "CREATE TABLE users (id INT, name VARCHAR(100));",
    ).unwrap();

    std::fs::write(
        format!("{}/sql/ddl/drop_table.sql", dir),
        "DROP TABLE users;",
    ).unwrap();

    std::fs::write(
        format!("{}/sql/dml/select_all.sql", dir),
        "SELECT * FROM orders;",
    ).unwrap();

    std::fs::write(
        format!("{}/sql/dml/select_good.sql", dir),
        "SELECT id, name FROM orders;",
    ).unwrap();
}

#[test]
fn test_init_creates_config() {
    let dir = "/tmp/sqlguard-test-init";
    let _ = std::fs::remove_dir_all(dir);

    let output = Command::new(BINARY)
        .args(["init", dir])
        .output()
        .expect("Failed to run sqlguard init");

    assert!(output.status.success());
    assert!(Path::new(dir).join("sqlguard.toml").exists());
    assert!(Path::new(dir).join("config/rules/ddl/no_drop_table.rhai").exists());
    assert!(Path::new(dir).join("config/rules/dml/no_select_all.rhai").exists());

    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn test_check_finds_violations() {
    let dir = "/tmp/sqlguard-test-check";
    let _ = std::fs::remove_dir_all(dir);

    Command::new(BINARY)
        .args(["init", dir])
        .output()
        .expect("Failed to run sqlguard init");

    setup_test_project(dir);

    let config_path = format!("{}/sqlguard.toml", dir);

    let output = Command::new(BINARY)
        .args(["check", dir, "-c", &config_path, "-f", "plain"])
        .output()
        .expect("Failed to run sqlguard check");

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);

    assert!(
        stdout.contains("no_drop_table") || stderr.contains("no_drop_table"),
        "Should find DROP TABLE violation\nstdout: {}\nstderr: {}",
        stdout,
        stderr
    );
    assert!(
        stdout.contains("no_select_all") || stderr.contains("no_select_all"),
        "Should find SELECT * violation\nstdout: {}\nstderr: {}",
        stdout,
        stderr
    );

    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn test_check_json_output() {
    let dir = "/tmp/sqlguard-test-json";
    let _ = std::fs::remove_dir_all(dir);

    setup_test_project(dir);
    std::fs::write(format!("{}/sqlguard.toml", dir), r#"
[structure]
paths = ["sql/ddl", "sql/dml"]
strict = false

[classification]
default_type = "sql"

[[classification.rules]]
name = "ddl-by-dir"
pattern = "**/ddl/**"
type = "ddl"
priority = 10

[[classification.rules]]
name = "dml-by-dir"
pattern = "**/dml/**"
type = "dml"
priority = 10

[[rules]]
id = "DDL001"
name = "no_drop_table"
group = "ddl-safety"
enabled = true
script_path = "config/rules/ddl/no_drop_table.rhai"
applies_to = ["ddl"]
severity = "error"

[[rules]]
id = "DML001"
name = "no_select_all"
group = "dml-safety"
enabled = false
script_path = "config/rules/dml/no_select_all.rhai"
applies_to = ["dml"]
severity = "error"

[output]
formats = ["json"]
"#).unwrap();

    std::fs::create_dir_all(format!("{}/config/rules/ddl", dir)).unwrap();
    std::fs::create_dir_all(format!("{}/config/rules/dml", dir)).unwrap();
    std::fs::write(format!("{}/config/rules/ddl/no_drop_table.rhai", dir),
        "let sql = context[\"sql_content\"];\nlet upper = sql.to_upper();\nif upper.contains(\"DROP TABLE\") { violations.push(\"DROP TABLE not allowed\"); }\n"
    ).unwrap();
    std::fs::write(format!("{}/config/rules/dml/no_select_all.rhai", dir),
        "let sql = context[\"sql_content\"];\nlet upper = sql.to_upper();\nif upper.contains(\"SELECT *\") { violations.push(\"SELECT * not allowed\"); }\n"
    ).unwrap();

    let output = Command::new(BINARY)
        .args(["check", dir, "-c", &format!("{}/sqlguard.toml", dir), "-f", "json", "-o", dir])
        .output()
        .expect("Failed to run sqlguard check with JSON");

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("JSON report saved"), "JSON report should be saved: {}", stderr);

    let report_path = format!("{}/sqlguard-report.json", dir);
    assert!(Path::new(&report_path).exists(), "JSON report file should exist");

    let content = std::fs::read_to_string(&report_path).unwrap();
    assert!(content.contains("no_drop_table"), "JSON should contain violation: {}", content);

    let _ = std::fs::remove_dir_all(dir);
}
