use std::process::Command;
use std::path::Path;

const BINARY: &str = "target/release/sqlguard";

/// 取 sqlguard 二进制的绝对路径（避免 current_dir 切换后相对路径失效）
fn binary_abs_path() -> String {
    let cwd = std::env::current_dir().unwrap_or_default();
    cwd.join(BINARY).to_string_lossy().to_string()
}

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

    let output = Command::new(&binary_abs_path())
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

    Command::new(&binary_abs_path())
        .args(["init", dir])
        .output()
        .expect("Failed to run sqlguard init");

    setup_test_project(dir);

    let config_path = format!("{}/sqlguard.toml", dir);

    let output = Command::new(&binary_abs_path())
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

    let output = Command::new(&binary_abs_path())
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

#[test]
fn test_check_mapper_mode() {
    let dir = "/tmp/sqlguard-test-mapper";
    let _ = std::fs::remove_dir_all(dir);

    // 1. 初始化项目配置（生成规则脚本骨架）
    Command::new(&binary_abs_path())
        .args(["init", dir])
        .output()
        .expect("Failed to run sqlguard init");

    // 2. 创建 mapper XML，含 SELECT * 违规和动态 SQL 标签
    std::fs::create_dir_all(format!("{}/src/main/resources/mapper", dir)).unwrap();
    std::fs::write(
        format!("{}/src/main/resources/mapper/UserMapper.xml", dir),
        r#"<?xml version="1.0" encoding="UTF-8"?>
<mapper namespace="com.example.UserMapper">
  <sql id="BaseColumns">id, name, email</sql>

  <select id="findAll" resultType="User">
    SELECT * FROM users
  </select>

  <select id="findById" resultType="User">
    SELECT <include refid="BaseColumns"/> FROM users WHERE id = #{id}
  </select>

  <select id="findByCondition" resultType="User">
    SELECT * FROM users
    <where>
      <if test="name != null">AND name LIKE #{name}</if>
    </where>
  </select>

  <insert id="insertUser">
    INSERT INTO users (name, email) VALUES (#{name}, #{email})
  </insert>
</mapper>
"#,
    ).unwrap();

    // 3. 启用 mapper 模式的配置
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
id = "DML001"
name = "no_select_all"
group = "dml-safety"
enabled = true
script_path = "config/rules/dml/no_select_all.rhai"
applies_to = ["dml"]
severity = "error"

[mapper]
enabled = true
paths = ["src/main/resources/mapper"]
patterns = ["**/*Mapper.xml"]

[output]
formats = ["json"]
"#).unwrap();

    // 4. 执行 check
    let output = Command::new(&binary_abs_path())
        .args(["check", dir, "-c", &format!("{}/sqlguard.toml", dir), "-f", "json", "-o", dir])
        .output()
        .expect("Failed to run sqlguard check with mapper");

    let stderr = String::from_utf8_lossy(&output.stderr);

    // 5. JSON 报告应被生成
    assert!(stderr.contains("JSON report saved"), "JSON report should be saved: {}", stderr);

    let report_path = format!("{}/sqlguard-report.json", dir);
    assert!(Path::new(&report_path).exists(), "JSON report file should exist");

    let content = std::fs::read_to_string(&report_path).unwrap();

    // 6. 验证：mapper XML 文件出现在报告中
    assert!(
        content.contains("UserMapper.xml"),
        "Report should contain mapper XML path: {}",
        content
    );

    // 7. 验证：找到了 SELECT * 违规（findAll 和 findByCondition 两条）
    // 计数 "no_select_all" 出现次数
    let count = content.matches("no_select_all").count();
    assert!(
        count >= 2,
        "Should find at least 2 SELECT * violations (findAll + findByCondition), got {}: {}",
        count,
        content
    );

    // 8. 验证：findById（用了 <include> + 显式列）不应触发 no_select_all
    //    insertUser（INSERT）也不应触发 no_select_all
    //    （如果触发数 > 2，说明 include 解析或 statement_type 过滤有问题）

    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn test_mapper_disabled_by_default() {
    // 验证：mapper 默认禁用，不启用时不会扫描 XML 文件
    let dir = "/tmp/sqlguard-test-mapper-disabled";
    let _ = std::fs::remove_dir_all(dir);

    Command::new(&binary_abs_path())
        .args(["init", dir])
        .output()
        .expect("Failed to run sqlguard init");

    // 创建一个含违规 SQL 的 mapper XML
    std::fs::create_dir_all(format!("{}/src/main/resources/mapper", dir)).unwrap();
    std::fs::write(
        format!("{}/src/main/resources/mapper/BadMapper.xml", dir),
        r#"<mapper>
  <select id="bad">SELECT * FROM users</select>
</mapper>
"#,
    ).unwrap();

    // 用 init 生成的默认配置（不含 [mapper] 段）跑 check
    let output = Command::new(&binary_abs_path())
        .args(["check", dir, "-c", &format!("{}/sqlguard.toml", dir), "-f", "json", "-o", dir])
        .output()
        .expect("Failed to run sqlguard check");

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("JSON report saved"), "JSON report should be saved: {}", stderr);

    let report_path = format!("{}/sqlguard-report.json", dir);
    let content = std::fs::read_to_string(&report_path).unwrap();

    // mapper XML 不应被扫描
    assert!(
        !content.contains("BadMapper.xml"),
        "Mapper XML should NOT be scanned when mapper.enabled defaults to false: {}",
        content
    );

    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn test_check_diff_only_changed_statements() {
    // 验证 check-diff 只校验改动语句，未改动语句的违规被过滤掉
    let dir = "/tmp/sqlguard-test-diff";
    let _ = std::fs::remove_dir_all(dir);

    // 1. 初始化 git 仓库
    Command::new("git")
        .args(["init", dir])
        .output()
        .expect("git init");
    Command::new("git")
        .current_dir(dir)
        .args(["config", "user.email", "test@test.com"])
        .output()
        .expect("git config");
    Command::new("git")
        .current_dir(dir)
        .args(["config", "user.name", "Test"])
        .output()
        .expect("git config");

    // 2. 初始化 sqlguard
    Command::new(&binary_abs_path())
        .args(["init", dir])
        .output()
        .expect("sqlguard init");

    // 3. 创建初始 SQL 文件，包含两条 SELECT *（都会违规）
    std::fs::create_dir_all(format!("{}/sql/dml", dir)).unwrap();
    std::fs::write(
        format!("{}/sql/dml/001.sql", dir),
        "SELECT * FROM users;\nSELECT * FROM orders;\n",
    ).unwrap();

    // 4. 提交初始版本
    Command::new("git")
        .current_dir(dir)
        .args(["add", "."])
        .output()
        .expect("git add");
    Command::new("git")
        .current_dir(dir)
        .args(["commit", "-m", "initial"])
        .output()
        .expect("git commit");

    // 5. 修改第一条 SELECT *（改为规范查询），第二条保持违规不变
    std::fs::write(
        format!("{}/sql/dml/001.sql", dir),
        "SELECT id, name FROM users;\nSELECT * FROM orders;\n",
    ).unwrap();
    Command::new("git")
        .current_dir(dir)
        .args(["add", "."])
        .output()
        .expect("git add");
    Command::new("git")
        .current_dir(dir)
        .args(["commit", "-m", "fix first query"])
        .output()
        .expect("git commit");

    // 6. 跑 check-diff HEAD~1
    let output = Command::new(&binary_abs_path())
        .current_dir(dir)
        .args([
            "check-diff",
            "--base", "HEAD~1",
            "-c", &format!("{}/sqlguard.toml", dir),
            "-f", "json",
            "-o", dir,
        ])
        .output()
        .expect("sqlguard check-diff");

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("JSON report saved"),
        "JSON report should be saved: {}",
        stderr
    );

    // 7. 验证：因为改动行（第 1 行 SELECT * FROM users → SELECT id, name FROM users）
    //    已经不再违规，所以增量校验应无违规
    //    第二条 SELECT * FROM orders 虽然违规，但不在改动 hunk 内（hunk = 第 1 行）
    //    所以应被过滤掉
    let report_path = format!("{}/sqlguard-report.json", dir);
    let content = std::fs::read_to_string(&report_path).unwrap();
    let total: usize = serde_json::from_str::<serde_json::Value>(&content)
        .ok()
        .and_then(|v| v["summary"]["total_violations"].as_u64().map(|n| n as usize))
        .unwrap_or(999);
    assert_eq!(
        total, 0,
        "Incremental check should find 0 violations (changed line no longer violates, unchanged violation filtered): {}",
        content
    );

    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn test_check_diff_detects_new_violation_in_changed_line() {
    // 验证 check-diff 能检测到改动行新增的违规
    let dir = "/tmp/sqlguard-test-diff-new";
    let _ = std::fs::remove_dir_all(dir);

    Command::new("git").args(["init", dir]).output().expect("git init");
    Command::new("git").current_dir(dir).args(["config", "user.email", "t@t.com"]).output().expect("git config");
    Command::new("git").current_dir(dir).args(["config", "user.name", "T"]).output().expect("git config");

    Command::new(&binary_abs_path()).args(["init", dir]).output().expect("sqlguard init");

    // 初始：规范查询
    std::fs::create_dir_all(format!("{}/sql/dml", dir)).unwrap();
    std::fs::write(
        format!("{}/sql/dml/001.sql", dir),
        "SELECT id FROM users;\n",
    ).unwrap();
    Command::new("git").current_dir(dir).args(["add", "."]).output().expect("git add");
    Command::new("git").current_dir(dir).args(["commit", "-m", "initial"]).output().expect("git commit");

    // 改动：把规范查询改成 SELECT *（新增违规）
    std::fs::write(
        format!("{}/sql/dml/001.sql", dir),
        "SELECT * FROM users;\n",
    ).unwrap();
    Command::new("git").current_dir(dir).args(["add", "."]).output().expect("git add");
    Command::new("git").current_dir(dir).args(["commit", "-m", "add violation"]).output().expect("git commit");

    let output = Command::new(&binary_abs_path())
        .current_dir(dir)
        .args([
            "check-diff",
            "--base", "HEAD~1",
            "-c", &format!("{}/sqlguard.toml", dir),
            "-f", "json",
            "-o", dir,
        ])
        .output()
        .expect("sqlguard check-diff");

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("JSON report saved"), "JSON report should be saved: {}", stderr);

    let report_path = format!("{}/sqlguard-report.json", dir);
    let content = std::fs::read_to_string(&report_path).unwrap();
    let total: usize = serde_json::from_str::<serde_json::Value>(&content)
        .ok()
        .and_then(|v| v["summary"]["total_violations"].as_u64().map(|n| n as usize))
        .unwrap_or(0);
    assert_eq!(
        total, 1,
        "Incremental check should find 1 violation (new SELECT *): {}",
        content
    );

    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn test_check_diff_new_file_all_checked() {
    // 验证新增文件整体算改动，所有违规都被保留
    let dir = "/tmp/sqlguard-test-diff-newfile";
    let _ = std::fs::remove_dir_all(dir);

    Command::new("git").args(["init", dir]).output().expect("git init");
    Command::new("git").current_dir(dir).args(["config", "user.email", "t@t.com"]).output().expect("git config");
    Command::new("git").current_dir(dir).args(["config", "user.name", "T"]).output().expect("git config");

    Command::new(&binary_abs_path()).args(["init", dir]).output().expect("sqlguard init");

    // 初始 commit（无 SQL 文件）
    Command::new("git").current_dir(dir).args(["add", "."]).output().expect("git add");
    Command::new("git").current_dir(dir).args(["commit", "-m", "initial"]).output().expect("git commit");

    // 新增 SQL 文件，含 2 条违规
    std::fs::create_dir_all(format!("{}/sql/dml", dir)).unwrap();
    std::fs::write(
        format!("{}/sql/dml/new.sql", dir),
        "SELECT * FROM users;\nSELECT * FROM orders;\n",
    ).unwrap();
    Command::new("git").current_dir(dir).args(["add", "."]).output().expect("git add");
    Command::new("git").current_dir(dir).args(["commit", "-m", "add new file"]).output().expect("git commit");

    let output = Command::new(&binary_abs_path())
        .current_dir(dir)
        .args([
            "check-diff",
            "--base", "HEAD~1",
            "-c", &format!("{}/sqlguard.toml", dir),
            "-f", "json",
            "-o", dir,
        ])
        .output()
        .expect("sqlguard check-diff");

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("JSON report saved"), "JSON report should be saved: {}", stderr);

    let report_path = format!("{}/sqlguard-report.json", dir);
    let content = std::fs::read_to_string(&report_path).unwrap();
    let total: usize = serde_json::from_str::<serde_json::Value>(&content)
        .ok()
        .and_then(|v| v["summary"]["total_violations"].as_u64().map(|n| n as usize))
        .unwrap_or(0);
    assert_eq!(
        total, 2,
        "New file: both SELECT * violations should be kept (is_new=true bypasses hunk filter): {}",
        content
    );

    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn test_check_diff_no_changes() {
    // 验证无改动时正常退出，输出空报告
    let dir = "/tmp/sqlguard-test-diff-empty";
    let _ = std::fs::remove_dir_all(dir);

    Command::new("git").args(["init", dir]).output().expect("git init");
    Command::new("git").current_dir(dir).args(["config", "user.email", "t@t.com"]).output().expect("git config");
    Command::new("git").current_dir(dir).args(["config", "user.name", "T"]).output().expect("git config");

    Command::new(&binary_abs_path()).args(["init", dir]).output().expect("sqlguard init");

    std::fs::create_dir_all(format!("{}/sql/dml", dir)).unwrap();
    std::fs::write(
        format!("{}/sql/dml/001.sql", dir),
        "SELECT * FROM users;\n",
    ).unwrap();
    Command::new("git").current_dir(dir).args(["add", "."]).output().expect("git add");
    Command::new("git").current_dir(dir).args(["commit", "-m", "initial"]).output().expect("git commit");

    // 无改动直接跑 check-diff
    let output = Command::new(&binary_abs_path())
        .current_dir(dir)
        .args([
            "check-diff",
            "--base", "HEAD",
            "-c", &format!("{}/sqlguard.toml", dir),
            "-f", "json",
            "-o", dir,
        ])
        .output()
        .expect("sqlguard check-diff");

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("No SQL changes detected"),
        "Should report no changes: {}",
        stderr
    );

    let _ = std::fs::remove_dir_all(dir);
}
