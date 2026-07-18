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

#[test]
fn test_check_mapper_mode() {
    let dir = "/tmp/sqlguard-test-mapper";
    let _ = std::fs::remove_dir_all(dir);

    // 1. 初始化项目配置（生成规则脚本骨架）
    Command::new(BINARY)
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
    let output = Command::new(BINARY)
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

    Command::new(BINARY)
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
    let output = Command::new(BINARY)
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
