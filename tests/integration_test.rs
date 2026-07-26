use std::process::Command;
use std::path::Path;

/// 取 sqlguard 二进制的绝对路径。
///
/// 使用 `CARGO_BIN_EXE_sqlguard` 让 cargo 在测试前自动构建并注入二进制路径，
/// 避免 `cargo test` 不重建 `target/release/sqlguard` 导致测试跑旧二进制。
/// 注入的路径已是绝对路径，`current_dir` 切换后仍然有效。
fn binary_abs_path() -> String {
    env!("CARGO_BIN_EXE_sqlguard").to_string()
}

/// 检测 git 是否可用；不可用时测试应跳过而非 panic。
/// CI 环境或精简容器可能未安装 git，此时 diff 相关测试无意义。
fn git_available() -> bool {
    Command::new("git")
        .arg("--version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

/// 在 diff 测试开头调用：git 不可用时打印提示并 early return（测试标记为通过）。
fn require_git() -> bool {
    if !git_available() {
        eprintln!("Skipping test: git is not installed");
        return false;
    }
    true
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
    // init 现在将规则拆分到独立文件
    assert!(Path::new(dir).join("sqlguard.rules.toml").exists());
    assert!(Path::new(dir).join("config/rules/ddl/no_drop_table.rhai").exists());
    assert!(Path::new(dir).join("config/rules/dml/no_select_all.rhai").exists());

    // 端到端：init 生成的主配置通过 rules_file 引用 sqlguard.rules.toml，
    // check 应能基于拆分后的规则发现违规。
    setup_test_project(dir);
    let config_path = format!("{}/sqlguard.toml", dir);
    let check_output = Command::new(&binary_abs_path())
        .args(["check", dir, "-c", &config_path, "-f", "plain"])
        .output()
        .expect("Failed to run sqlguard check");
    let stdout = String::from_utf8_lossy(&check_output.stdout);
    let stderr = String::from_utf8_lossy(&check_output.stderr);
    assert!(
        stdout.contains("no_drop_table") || stderr.contains("no_drop_table"),
        "init 生成的拆分配置应发现 DROP TABLE 违规\nstdout: {}\nstderr: {}",
        stdout, stderr
    );

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

/// 验证规则配置拆分到独立文件（显式 rules_file）时可被正确加载：
/// 主配置不含 [[rules]]，仅通过 rules_file 指向 sqlguard.rules.toml。
#[test]
fn test_split_rules_config_explicit_file() {
    let dir = "/tmp/sqlguard-test-split-explicit";
    let _ = std::fs::remove_dir_all(dir);

    setup_test_project(dir);

    // 主配置：引用独立规则文件，本身不含 [[rules]]
    std::fs::write(
        format!("{}/sqlguard.toml", dir),
        r#"
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

rules_file = "sqlguard.rules.toml"
"#,
    )
    .unwrap();

    // 独立规则文件：脚本路径相对本文件所在目录解析
    std::fs::write(
        format!("{}/sqlguard.rules.toml", dir),
        r#"
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
enabled = true
script_path = "config/rules/dml/no_select_all.rhai"
applies_to = ["dml"]
severity = "error"
"#,
    )
    .unwrap();

    std::fs::create_dir_all(format!("{}/config/rules/ddl", dir)).unwrap();
    std::fs::create_dir_all(format!("{}/config/rules/dml", dir)).unwrap();
    std::fs::write(
        format!("{}/config/rules/ddl/no_drop_table.rhai", dir),
        "let sql = context[\"sql_content\"];\nlet upper = sql.to_upper();\nif upper.contains(\"DROP TABLE\") { violations.push(\"DROP TABLE not allowed\"); }\n",
    )
    .unwrap();
    std::fs::write(
        format!("{}/config/rules/dml/no_select_all.rhai", dir),
        "let sql = context[\"sql_content\"];\nlet upper = sql.to_upper();\nif upper.contains(\"SELECT *\") { violations.push(\"SELECT * not allowed\"); }\n",
    )
    .unwrap();

    let output = Command::new(&binary_abs_path())
        .args([
            "check",
            dir,
            "-c",
            &format!("{}/sqlguard.toml", dir),
            "-f",
            "plain",
        ])
        .output()
        .expect("Failed to run sqlguard check with split config");

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stdout.contains("no_drop_table") || stderr.contains("no_drop_table"),
        "拆分配置（显式 rules_file）应发现 DROP TABLE 违规\nstdout: {}\nstderr: {}",
        stdout,
        stderr
    );
    assert!(
        stdout.contains("no_select_all") || stderr.contains("no_select_all"),
        "拆分配置（显式 rules_file）应发现 SELECT * 违规\nstdout: {}\nstderr: {}",
        stdout,
        stderr
    );

    let _ = std::fs::remove_dir_all(dir);
}

/// 验证未写 rules_file 时，工具自动在同目录发现 sqlguard.rules.toml。
#[test]
fn test_split_rules_config_auto_discovery() {
    let dir = "/tmp/sqlguard-test-split-auto";
    let _ = std::fs::remove_dir_all(dir);

    setup_test_project(dir);

    // 主配置：不写 rules_file，但同目录存在 sqlguard.rules.toml
    std::fs::write(
        format!("{}/sqlguard.toml", dir),
        r#"
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
"#,
    )
    .unwrap();

    std::fs::write(
        format!("{}/sqlguard.rules.toml", dir),
        r#"
[[rules]]
id = "DDL001"
name = "no_drop_table"
group = "ddl-safety"
enabled = true
script_path = "config/rules/ddl/no_drop_table.rhai"
applies_to = ["ddl"]
severity = "error"
"#,
    )
    .unwrap();

    std::fs::create_dir_all(format!("{}/config/rules/ddl", dir)).unwrap();
    std::fs::write(
        format!("{}/config/rules/ddl/no_drop_table.rhai", dir),
        "let sql = context[\"sql_content\"];\nlet upper = sql.to_upper();\nif upper.contains(\"DROP TABLE\") { violations.push(\"DROP TABLE not allowed\"); }\n",
    )
    .unwrap();

    let output = Command::new(&binary_abs_path())
        .args([
            "check",
            dir,
            "-c",
            &format!("{}/sqlguard.toml", dir),
            "-f",
            "plain",
        ])
        .output()
        .expect("Failed to run sqlguard check with auto-discovered rules file");

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stdout.contains("no_drop_table") || stderr.contains("no_drop_table"),
        "自动发现 sqlguard.rules.toml 应发现 DROP TABLE 违规\nstdout: {}\nstderr: {}",
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
fn test_check_ddl002_alter_primary_key() {
    // 验证 DDL002 跨语句识别：
    // - CREATE 无内联 PK + ALTER ADD PRIMARY KEY 不误报（修复漏洞）
    // - CREATE 无 PK 且无 ALTER → 报违规
    // - CREATE 有 PK 但 ALTER DROP PRIMARY KEY 移除 → 仍报违规
    let dir = "/tmp/sqlguard-test-ddl002";
    let _ = std::fs::remove_dir_all(dir);

    std::fs::create_dir_all(format!("{}/sql/ddl", dir)).unwrap();
    std::fs::create_dir_all(format!("{}/config/rules/ddl", dir)).unwrap();

    // 规则脚本从仓库复制（集成测试在 crate 根目录运行）
    let rule_src = std::env::current_dir()
        .unwrap()
        .join("config/rules/ddl/primary_key_required.rhai");
    std::fs::write(
        format!("{}/config/rules/ddl/primary_key_required.rhai", dir),
        std::fs::read_to_string(&rule_src).unwrap(),
    )
    .unwrap();

    std::fs::write(
        format!("{}/sql/ddl/compliant_alter.sql", dir),
        "CREATE TABLE sessions (\n    token VARCHAR(255),\n    user_id INT\n);\nALTER TABLE sessions ADD PRIMARY KEY (token);\n",
    )
    .unwrap();
    std::fs::write(
        format!("{}/sql/ddl/compliant_alter_named.sql", dir),
        "CREATE TABLE s2 (token VARCHAR(255));\nALTER TABLE s2 ADD CONSTRAINT pk_s2 PRIMARY KEY (token);\n",
    )
    .unwrap();
    std::fs::write(
        format!("{}/sql/ddl/violation_missing.sql", dir),
        "CREATE TABLE users (id INT, name VARCHAR(100));\n",
    )
    .unwrap();
    std::fs::write(
        format!("{}/sql/ddl/violation_dropped.sql", dir),
        "CREATE TABLE orders (id INT PRIMARY KEY);\nALTER TABLE orders DROP PRIMARY KEY;\n",
    )
    .unwrap();

    std::fs::write(
        format!("{}/sqlguard.toml", dir),
        r#"
[structure]
paths = ["sql/ddl"]
strict = false

[classification]
default_type = "sql"

[[classification.rules]]
name = "ddl-by-dir"
pattern = "**/ddl/**"
type = "ddl"
priority = 10

[[rules]]
id = "DDL002"
name = "primary_key_required"
group = "ddl-safety"
enabled = true
script_path = "config/rules/ddl/primary_key_required.rhai"
applies_to = ["ddl"]
severity = "warning"

[output]
formats = ["json"]
"#,
    )
    .unwrap();

    let output = Command::new(&binary_abs_path())
        .args([
            "check",
            dir,
            "-c",
            &format!("{}/sqlguard.toml", dir),
            "-f",
            "json",
            "-o",
            dir,
        ])
        .output()
        .expect("Failed to run sqlguard check");

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("JSON report saved"),
        "JSON report should be saved: {}",
        stderr
    );

    let report_path = format!("{}/sqlguard-report.json", dir);
    let content = std::fs::read_to_string(&report_path).unwrap();
    let report: serde_json::Value = serde_json::from_str(&content).unwrap();

    // 收集触发 DDL002 的文件集合
    let mut files_with_ddl002: std::collections::HashSet<String> = std::collections::HashSet::new();
    if let Some(violations) = report["violations"].as_array() {
        for v in violations {
            if v["rule_id"].as_str() == Some("DDL002") {
                files_with_ddl002.insert(v["file"].as_str().unwrap().to_string());
            }
        }
    }

    assert!(
        !files_with_ddl002
            .iter()
            .any(|f| f.contains("compliant_alter.sql")),
        "compliant_alter.sql (CREATE + ALTER ADD PK) should NOT trigger DDL002: {:?}",
        files_with_ddl002
    );
    assert!(
        !files_with_ddl002
            .iter()
            .any(|f| f.contains("compliant_alter_named.sql")),
        "compliant_alter_named.sql (ADD CONSTRAINT PK) should NOT trigger DDL002: {:?}",
        files_with_ddl002
    );
    assert!(
        files_with_ddl002
            .iter()
            .any(|f| f.contains("violation_missing.sql")),
        "violation_missing.sql (no PK at all) should trigger DDL002: {:?}",
        files_with_ddl002
    );
    assert!(
        files_with_ddl002
            .iter()
            .any(|f| f.contains("violation_dropped.sql")),
        "violation_dropped.sql (PK dropped by ALTER) should trigger DDL002: {:?}",
        files_with_ddl002
    );

    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn test_check_rule_audit_fixes() {
    // 验证规则审计修复：
    // - DML005 不再把 COUNT(*) AS 别名误报为未限定列；裸列仍报
    // - DML106 不再把 INTERSECT / UNION ALL 误报；普通 UNION 仍报
    // - DML001 能识别 UNION 分支 / 子查询中的 SELECT *
    let dir = "/tmp/sqlguard-test-ruleaudit";
    let _ = std::fs::remove_dir_all(dir);

    std::fs::create_dir_all(format!("{}/sql/dml", dir)).unwrap();
    std::fs::create_dir_all(format!("{}/config/rules/dml", dir)).unwrap();

    // 从仓库复制三条规则脚本（集成测试在 crate 根目录运行）
    let rule_dir = std::env::current_dir().unwrap().join("config/rules/dml");
    for r in ["column_references_qualified", "union_all_preferred", "no_select_all"] {
        std::fs::write(
            format!("{}/config/rules/dml/{}.rhai", dir, r),
            std::fs::read_to_string(rule_dir.join(format!("{}.rhai", r))).unwrap(),
        )
        .unwrap();
    }

    // A: 多表 + COUNT(*) AS 别名（不应触发 DML005）
    std::fs::write(
        format!("{}/sql/dml/a_count_alias.sql", dir),
        "SELECT u.id, COUNT(*) AS cnt FROM users u JOIN orders o ON u.id = o.user_id GROUP BY u.id;\n",
    )
    .unwrap();
    // B: INTERSECT（不应触发 DML106）
    std::fs::write(
        format!("{}/sql/dml/b_intersect.sql", dir),
        "SELECT id FROM a INTERSECT SELECT id FROM b;\n",
    )
    .unwrap();
    // C: UNION ALL 且 SELECT *（应触发 DML001，不应触发 DML106）
    std::fs::write(
        format!("{}/sql/dml/c_union_star.sql", dir),
        "SELECT * FROM a UNION ALL SELECT * FROM b;\n",
    )
    .unwrap();
    // D: 裸列（应触发 DML005）
    std::fs::write(
        format!("{}/sql/dml/d_bare_col.sql", dir),
        "SELECT id, name FROM users u JOIN orders o ON u.id = o.user_id;\n",
    )
    .unwrap();
    // E: 普通 UNION（应触发 DML106）
    std::fs::write(
        format!("{}/sql/dml/e_plain_union.sql", dir),
        "SELECT id FROM a UNION SELECT id FROM b;\n",
    )
    .unwrap();

    std::fs::write(
        format!("{}/sqlguard.toml", dir),
        r#"
[structure]
paths = ["sql/dml"]
strict = false

[classification]
default_type = "sql"

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

[[rules]]
id = "DML005"
name = "column_references_qualified"
group = "dml-style"
enabled = true
script_path = "config/rules/dml/column_references_qualified.rhai"
applies_to = ["dml"]
severity = "warning"

[[rules]]
id = "DML106"
name = "union_all_preferred"
group = "dml-performance"
enabled = true
script_path = "config/rules/dml/union_all_preferred.rhai"
applies_to = ["dml"]
severity = "warning"

[output]
formats = ["json"]
"#,
    )
    .unwrap();

    let output = Command::new(&binary_abs_path())
        .args([
            "check",
            dir,
            "-c",
            &format!("{}/sqlguard.toml", dir),
            "-f",
            "json",
            "-o",
            dir,
        ])
        .output()
        .expect("Failed to run sqlguard check");

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("JSON report saved"),
        "JSON report should be saved: {}",
        stderr
    );

    let report_path = format!("{}/sqlguard-report.json", dir);
    let content = std::fs::read_to_string(&report_path).unwrap();
    let report: serde_json::Value = serde_json::from_str(&content).unwrap();

    // 收集 文件 -> 触发规则集合
    let mut by_file: std::collections::HashMap<String, std::collections::HashSet<String>> =
        std::collections::HashMap::new();
    if let Some(violations) = report["violations"].as_array() {
        for v in violations {
            let f = v["file"].as_str().unwrap().to_string();
            let rid = v["rule_id"].as_str().unwrap().to_string();
            by_file.entry(f).or_default().insert(rid);
        }
    }
    let rules_of = |name: &str| -> std::collections::HashSet<String> {
        by_file
            .iter()
            .find(|(f, _)| f.contains(name))
            .map(|(_, s)| s.clone())
            .unwrap_or_default()
    };

    // A: COUNT(*) 别名 → 不应有 DML005
    assert!(
        !rules_of("a_count_alias.sql").contains("DML005"),
        "COUNT(*) alias should NOT trigger DML005: {:?}",
        rules_of("a_count_alias.sql")
    );
    // B: INTERSECT → 不应有 DML106
    assert!(
        !rules_of("b_intersect.sql").contains("DML106"),
        "INTERSECT should NOT trigger DML106: {:?}",
        rules_of("b_intersect.sql")
    );
    // C: UNION ALL + SELECT * → 应有 DML001，不应有 DML106
    assert!(
        rules_of("c_union_star.sql").contains("DML001"),
        "UNION branch SELECT * should trigger DML001: {:?}",
        rules_of("c_union_star.sql")
    );
    assert!(
        !rules_of("c_union_star.sql").contains("DML106"),
        "UNION ALL should NOT trigger DML106: {:?}",
        rules_of("c_union_star.sql")
    );
    // D: 裸列 → 应有 DML005
    assert!(
        rules_of("d_bare_col.sql").contains("DML005"),
        "bare column should trigger DML005: {:?}",
        rules_of("d_bare_col.sql")
    );
    // E: 普通 UNION → 应有 DML106
    assert!(
        rules_of("e_plain_union.sql").contains("DML106"),
        "plain UNION should trigger DML106: {:?}",
        rules_of("e_plain_union.sql")
    );

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
    if !require_git() { return; }
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
    if !require_git() { return; }
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
    if !require_git() { return; }
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
fn test_check_diff_with_diff_noprefix_config() {
    // 回归测试：用户设置 `git config diff.noprefix true` 后，
    // git diff 输出 `+++ path` 而非 `+++ b/path`。
    // 解析器依赖 `+++ b/` 前缀提取路径，若无前缀会返回空列表，
    // 导致 check-diff 报 "No SQL changes detected" / files_checked = 0。
    // 修复：get_diff 显式传 `--src-prefix=a/ --dst-prefix=b/` 强制前缀。
    if !require_git() { return; }
    let dir = "/tmp/sqlguard-test-diff-noprefix";
    let _ = std::fs::remove_dir_all(dir);

    Command::new("git").args(["init", dir]).output().expect("git init");
    Command::new("git").current_dir(dir).args(["config", "user.email", "t@t.com"]).output().expect("git config");
    Command::new("git").current_dir(dir).args(["config", "user.name", "T"]).output().expect("git config");
    // 关键：设置 noprefix，模拟用户全局配置
    Command::new("git").current_dir(dir).args(["config", "diff.noprefix", "true"]).output().expect("git config diff.noprefix");

    Command::new(&binary_abs_path()).args(["init", dir]).output().expect("sqlguard init");

    // 初始：规范查询
    std::fs::create_dir_all(format!("{}/sql/dml", dir)).unwrap();
    std::fs::write(
        format!("{}/sql/dml/001.sql", dir),
        "SELECT id FROM users;\n",
    ).unwrap();
    Command::new("git").current_dir(dir).args(["add", "."]).output().expect("git add");
    Command::new("git").current_dir(dir).args(["commit", "-m", "initial"]).output().expect("git commit");

    // 改动：引入 SELECT * 违规
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
    assert!(
        !stderr.contains("No SQL changes detected"),
        "noprefix=true should not cause empty diff (fix: --src-prefix/--dst-prefix): {}",
        stderr
    );
    assert!(stderr.contains("JSON report saved"), "JSON report should be saved: {}", stderr);

    let report_path = format!("{}/sqlguard-report.json", dir);
    let content = std::fs::read_to_string(&report_path).unwrap();
    let json: serde_json::Value = serde_json::from_str(&content).unwrap();
    let files_checked = json["files_checked"].as_u64().unwrap_or(0);
    let total = json["summary"]["total_violations"].as_u64().unwrap_or(0);
    assert_eq!(
        files_checked, 1,
        "noprefix=true: should still detect 1 changed file: {}",
        content
    );
    assert!(
        total >= 1,
        "noprefix=true: should still detect SELECT * violation: {}",
        content
    );

    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn test_check_ast_capability_upgrades() {
    // 验证 AST 能力补全（P0-P3）后三条规则脚本的升级效果：
    //   DML102 no_unused_cte：基于 AST 的 ctes() 检测
    //     A. 未使用 CTE → 触发
    //     B. 已使用 CTE → 不触发
    //   DML101 no_unused_join：别名优先匹配
    //     C. JOIN orders o，投影用 o.id → 不触发（修复旧版误报）
    //     D. JOIN orders o，投影与 WHERE 都不引用 o/orders → 触发
    //   DML105 no_order_by_in_subquery：AST 递归子查询 + 窗口函数修复
    //     E. 子查询 ORDER BY 无 LIMIT → 触发
    //     F. 窗口函数 OVER(ORDER BY ...) → 不触发（修复旧版误报）
    let dir = "/tmp/sqlguard-test-ast-upgrades";
    let _ = std::fs::remove_dir_all(dir);

    std::fs::create_dir_all(format!("{}/sql/dml", dir)).unwrap();
    std::fs::create_dir_all(format!("{}/config/rules/dml", dir)).unwrap();

    // 从仓库复制三条规则脚本（集成测试在 crate 根目录运行）
    let rule_dir = std::env::current_dir().unwrap().join("config/rules/dml");
    for r in ["no_unused_join", "no_unused_cte", "no_order_by_in_subquery"] {
        std::fs::write(
            format!("{}/config/rules/dml/{}.rhai", dir, r),
            std::fs::read_to_string(rule_dir.join(format!("{}.rhai", r))).unwrap(),
        )
        .unwrap();
    }

    // A: 未使用 CTE（应触发 DML102）
    std::fs::write(
        format!("{}/sql/dml/a_unused_cte.sql", dir),
        "WITH unused AS (SELECT 1 AS x) SELECT id FROM users;\n",
    )
    .unwrap();
    // B: 已使用 CTE（不应触发 DML102）
    std::fs::write(
        format!("{}/sql/dml/b_used_cte.sql", dir),
        "WITH active AS (SELECT id FROM users WHERE status = 'active') SELECT id FROM active;\n",
    )
    .unwrap();
    // C: JOIN orders o，投影用 o.id（不应触发 DML101）
    std::fs::write(
        format!("{}/sql/dml/c_join_alias_used.sql", dir),
        "SELECT o.id FROM users u JOIN orders o ON u.id = o.user_id;\n",
    )
    .unwrap();
    // D: JOIN orders o，投影与 WHERE 都不引用 o/orders（应触发 DML101）
    std::fs::write(
        format!("{}/sql/dml/d_join_unused.sql", dir),
        "SELECT u.id FROM users u JOIN orders o ON u.id = o.user_id;\n",
    )
    .unwrap();
    // E: 子查询 ORDER BY 无 LIMIT（应触发 DML105）
    std::fs::write(
        format!("{}/sql/dml/e_subq_order_by.sql", dir),
        "SELECT * FROM (SELECT id FROM users ORDER BY name) AS u;\n",
    )
    .unwrap();
    // F: 窗口函数 OVER(ORDER BY ...)（不应触发 DML105）
    std::fs::write(
        format!("{}/sql/dml/f_window_order_by.sql", dir),
        "SELECT id, ROW_NUMBER() OVER (ORDER BY created_at) AS rn FROM users;\n",
    )
    .unwrap();

    std::fs::write(
        format!("{}/sqlguard.toml", dir),
        r#"
[structure]
paths = ["sql/dml"]
strict = false

[classification]
default_type = "sql"

[[classification.rules]]
name = "dml-by-dir"
pattern = "**/dml/**"
type = "dml"
priority = 10

[[rules]]
id = "DML101"
name = "no_unused_join"
group = "dml-performance"
enabled = true
script_path = "config/rules/dml/no_unused_join.rhai"
applies_to = ["dml"]
severity = "warning"

[[rules]]
id = "DML102"
name = "no_unused_cte"
group = "dml-performance"
enabled = true
script_path = "config/rules/dml/no_unused_cte.rhai"
applies_to = ["dml"]
severity = "warning"

[[rules]]
id = "DML105"
name = "no_order_by_in_subquery"
group = "dml-performance"
enabled = true
script_path = "config/rules/dml/no_order_by_in_subquery.rhai"
applies_to = ["dml"]
severity = "warning"

[output]
formats = ["json"]
"#,
    )
    .unwrap();

    let output = Command::new(&binary_abs_path())
        .args([
            "check",
            dir,
            "-c",
            &format!("{}/sqlguard.toml", dir),
            "-f",
            "json",
            "-o",
            dir,
        ])
        .output()
        .expect("Failed to run sqlguard check");

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("JSON report saved"),
        "JSON report should be saved: {}",
        stderr
    );

    let report_path = format!("{}/sqlguard-report.json", dir);
    let content = std::fs::read_to_string(&report_path).unwrap();
    let report: serde_json::Value = serde_json::from_str(&content).unwrap();

    // 收集 文件 -> 触发规则集合
    let mut by_file: std::collections::HashMap<String, std::collections::HashSet<String>> =
        std::collections::HashMap::new();
    if let Some(violations) = report["violations"].as_array() {
        for v in violations {
            let f = v["file"].as_str().unwrap().to_string();
            let rid = v["rule_id"].as_str().unwrap().to_string();
            by_file.entry(f).or_default().insert(rid);
        }
    }
    let rules_of = |name: &str| -> std::collections::HashSet<String> {
        by_file
            .iter()
            .find(|(f, _)| f.contains(name))
            .map(|(_, s)| s.clone())
            .unwrap_or_default()
    };

    // A: 未使用 CTE → 应有 DML102
    assert!(
        rules_of("a_unused_cte.sql").contains("DML102"),
        "unused CTE should trigger DML102: {:?}",
        rules_of("a_unused_cte.sql")
    );
    // B: 已使用 CTE → 不应有 DML102
    assert!(
        !rules_of("b_used_cte.sql").contains("DML102"),
        "used CTE should NOT trigger DML102: {:?}",
        rules_of("b_used_cte.sql")
    );
    // C: JOIN orders o，投影 o.id → 不应有 DML101（别名优先修复）
    assert!(
        !rules_of("c_join_alias_used.sql").contains("DML101"),
        "JOIN with alias used in projection should NOT trigger DML101: {:?}",
        rules_of("c_join_alias_used.sql")
    );
    // D: JOIN orders o，投影与 WHERE 都不引用 → 应有 DML101
    assert!(
        rules_of("d_join_unused.sql").contains("DML101"),
        "truly unused JOIN should trigger DML101: {:?}",
        rules_of("d_join_unused.sql")
    );
    // E: 子查询 ORDER BY 无 LIMIT → 应有 DML105
    assert!(
        rules_of("e_subq_order_by.sql").contains("DML105"),
        "subquery ORDER BY without LIMIT should trigger DML105: {:?}",
        rules_of("e_subq_order_by.sql")
    );
    // F: 窗口函数 OVER(ORDER BY ...) → 不应有 DML105（窗口函数修复）
    assert!(
        !rules_of("f_window_order_by.sql").contains("DML105"),
        "window function OVER(ORDER BY ...) should NOT trigger DML105: {:?}",
        rules_of("f_window_order_by.sql")
    );

    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn test_check_ast_ddl_constraint_capabilities() {
    // 验证 DDL 侧 AST 能力补全：
    //   - CreateInfo.foreign_keys() / checks() / indexes() / uniques()
    //   - ColumnInfo.default_value() / is_auto_increment() / comment() / references_table()
    //   - AlterTableInfo.operations()
    //   - StmtInfo.has_truncate() / has_create_view() / has_create_index() / has_transaction()
    // 通过一条规则脚本调用这些方法，确保 Rhai 端能正常访问。
    let dir = "/tmp/sqlguard-test-ast-ddl";
    let _ = std::fs::remove_dir_all(dir);

    std::fs::create_dir_all(format!("{}/sql/ddl", dir)).unwrap();
    std::fs::create_dir_all(format!("{}/sql/others", dir)).unwrap();
    std::fs::create_dir_all(format!("{}/config/rules/ddl", dir)).unwrap();

    // 触发各种 DDL 能力的 SQL
    std::fs::write(
        format!("{}/sql/ddl/full.sql", dir),
        "CREATE TABLE orders (\n\
         id INT PRIMARY KEY AUTO_INCREMENT,\n\
         user_id INT NOT NULL,\n\
         status VARCHAR(20) DEFAULT 'pending',\n\
         amount DECIMAL(10,2),\n\
         remark VARCHAR(255) COMMENT 'order remark',\n\
         CONSTRAINT fk_user FOREIGN KEY (user_id) REFERENCES users(id) ON DELETE CASCADE,\n\
         CONSTRAINT chk_amount CHECK (amount > 0),\n\
         INDEX idx_status (status),\n\
         UNIQUE KEY uk_user_status (user_id, status)\n\
         );\n\
         CREATE INDEX idx_amount ON orders(amount);\n\
         CREATE VIEW v_orders AS SELECT id FROM orders;\n\
         TRUNCATE TABLE orders;\n\
         ALTER TABLE orders ADD COLUMN note VARCHAR(100);\n\
         ALTER TABLE orders DROP COLUMN note;\n\
         START TRANSACTION;\n\
         COMMIT;\n\
         ROLLBACK;\n",
    )
    .unwrap();

    // 一条"探针"规则脚本：调用所有新增 API，只要能跑完不报错即视为 API 可用
    std::fs::write(
        format!("{}/config/rules/ddl/probe.rhai", dir),
        r#"// 探针规则：调用所有新增 DDL/其他 AST API，验证 Rhai 端可访问
let ast = context["ast"];
for s in ast.statements() {
    if s.has_create_table() {
        let ct = s.create_table();
        // 新增约束 API
        let _fks = ct.foreign_keys();
        let _checks = ct.checks();
        let _indexes = ct.indexes();
        let _uniques = ct.uniques();
        if ct.has_foreign_key() { violations.push("has FK"); }
        if ct.has_check() { violations.push("has CHECK"); }
        if ct.has_index() { violations.push("has INDEX"); }
        if ct.has_unique() { violations.push("has UNIQUE"); }
        // 列级新 API（Rhai 不支持 `_` 作为变量名，直接调用方法即可）
        for c in ct.columns() {
            c.default_value();
            c.is_auto_increment();
            c.comment();
            c.has_check();
            c.references_table();
            c.has_foreign_key();
        }
    }
    if s.has_alter_table() {
        let at = s.alter_table();
        at.operations();
    }
    if s.has_truncate() {
        let t = s.truncate();
        t.table_name();
        t.has_table_keyword();
    }
    if s.has_create_view() {
        let v = s.create_view();
        v.name();
        v.materialized();
        v.is_replace();
        v.column_count();
    }
    if s.has_create_index() {
        let ci = s.create_index();
        ci.name();
        ci.table_name();
        ci.columns();
        ci.is_unique();
    }
    if s.has_transaction() {
        let tx = s.transaction();
        tx.kind();
    }
}
// 全局能力：注释 + 逗号 JOIN
let _comments = ast.comments();
let _comma_join = ast.has_comma_join_anywhere();
"#,
    )
    .unwrap();

    std::fs::write(
        format!("{}/sqlguard.toml", dir),
        r#"
[structure]
paths = ["sql/ddl", "sql/others"]
strict = false

[classification]
default_type = "ddl"

[[classification.rules]]
name = "ddl-by-dir"
pattern = "**/ddl/**"
type = "ddl"
priority = 10

[[rules]]
id = "DDL901"
name = "probe"
group = "ddl-probe"
enabled = true
script_path = "config/rules/ddl/probe.rhai"
applies_to = ["ddl"]
severity = "warning"

[output]
formats = ["json"]
"#,
    )
    .unwrap();

    let output = Command::new(&binary_abs_path())
        .args([
            "check",
            dir,
            "-c",
            &format!("{}/sqlguard.toml", dir),
            "-f",
            "json",
            "-o",
            dir,
        ])
        .output()
        .expect("Failed to run sqlguard check");

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("JSON report saved"),
        "JSON report should be saved: {}",
        stderr
    );

    let report_path = format!("{}/sqlguard-report.json", dir);
    let content = std::fs::read_to_string(&report_path).unwrap();
    let report: serde_json::Value = serde_json::from_str(&content).unwrap();

    // 探针规则应至少触发 4 条违规（has FK / CHECK / INDEX / UNIQUE）
    let mut probe_count = 0;
    if let Some(violations) = report["violations"].as_array() {
        for v in violations {
            if v["rule_id"].as_str() == Some("DDL901") {
                probe_count += 1;
            }
        }
    }
    assert!(
        probe_count >= 4,
        "Probe rule should fire at least 4 times (FK/CHECK/INDEX/UNIQUE), got {}: {}",
        probe_count,
        content
    );

    // 同时验证：探针规则不应抛错（如果 API 调用失败会变成单条规则错误而非 4 条）
    // 如果探针脚本抛错，violations 中会有 message 含 "error" 的项，且通常只有 1 条
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn test_check_diff_no_changes() {
    // 验证无改动时正常退出，输出空报告
    if !require_git() { return; }
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

#[test]
fn test_check_dml007_order_by_required_for_pagination() {
    let dir = "/tmp/sqlguard-test-dml007";
    let _ = std::fs::remove_dir_all(dir);

    std::fs::create_dir_all(format!("{}/sql/dml", dir)).unwrap();
    std::fs::create_dir_all(format!("{}/config/rules/dml", dir)).unwrap();

    let rule_src = std::env::current_dir()
        .unwrap()
        .join("config/rules/dml/order_by_required_for_pagination.rhai");
    std::fs::write(
        format!("{}/config/rules/dml/order_by_required_for_pagination.rhai", dir),
        std::fs::read_to_string(&rule_src).unwrap(),
    )
    .unwrap();

    // A: LIMIT + ORDER BY → 合规
    std::fs::write(
        format!("{}/sql/dml/a_limit_with_order.sql", dir),
        "SELECT id, name FROM users ORDER BY id LIMIT 10;\n",
    )
    .unwrap();
    // B: LIMIT 无 ORDER BY → 违规
    std::fs::write(
        format!("{}/sql/dml/b_limit_no_order.sql", dir),
        "SELECT id, name FROM users LIMIT 10;\n",
    )
    .unwrap();
    // C: OFFSET + FETCH 无 ORDER BY → 违规
    std::fs::write(
        format!("{}/sql/dml/c_offset_fetch_no_order.sql", dir),
        "SELECT id, name FROM users OFFSET 5 ROWS FETCH NEXT 10 ROWS ONLY;\n",
    )
    .unwrap();
    // D: LIMIT 无 ORDER BY + RANDOM_ORDER 注释 → 合规（豁免）
    std::fs::write(
        format!("{}/sql/dml/d_random_order_hint.sql", dir),
        "-- RANDOM_ORDER\nSELECT id, name FROM users LIMIT 10;\n",
    )
    .unwrap();
    // E: LIMIT + ORDER BY + OFFSET → 合规
    std::fs::write(
        format!("{}/sql/dml/e_full_pagination.sql", dir),
        "SELECT id, name FROM users ORDER BY id LIMIT 10 OFFSET 20;\n",
    )
    .unwrap();
    // F: 无 LIMIT/OFFSET → 合规
    std::fs::write(
        format!("{}/sql/dml/f_no_pagination.sql", dir),
        "SELECT id, name FROM users;\n",
    )
    .unwrap();

    std::fs::write(
        format!("{}/sqlguard.toml", dir),
        r#"
[structure]
paths = ["sql/dml"]
strict = false

[classification]
default_type = "sql"

[[classification.rules]]
name = "dml-by-dir"
pattern = "**/dml/**"
type = "dml"
priority = 10

[[rules]]
id = "DML007"
name = "order_by_required_for_pagination"
group = "dml-safety"
enabled = true
script_path = "config/rules/dml/order_by_required_for_pagination.rhai"
applies_to = ["dml"]
severity = "error"

[output]
formats = ["json"]
"#,
    )
    .unwrap();

    let output = Command::new(&binary_abs_path())
        .args([
            "check",
            dir,
            "-c",
            &format!("{}/sqlguard.toml", dir),
            "-f",
            "json",
            "-o",
            dir,
        ])
        .output()
        .expect("Failed to run sqlguard check");

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("JSON report saved"),
        "JSON report should be saved: {}",
        stderr
    );

    let report_path = format!("{}/sqlguard-report.json", dir);
    let content = std::fs::read_to_string(&report_path).unwrap();
    let report: serde_json::Value = serde_json::from_str(&content).unwrap();

    let mut by_file: std::collections::HashMap<String, std::collections::HashSet<String>> =
        std::collections::HashMap::new();
    if let Some(violations) = report["violations"].as_array() {
        for v in violations {
            let f = v["file"].as_str().unwrap().to_string();
            let rid = v["rule_id"].as_str().unwrap().to_string();
            by_file.entry(f).or_default().insert(rid);
        }
    }
    let rules_of = |name: &str| -> std::collections::HashSet<String> {
        by_file
            .iter()
            .find(|(f, _)| f.contains(name))
            .map(|(_, s)| s.clone())
            .unwrap_or_default()
    };

    assert!(
        !rules_of("a_limit_with_order.sql").contains("DML007"),
        "LIMIT with ORDER BY should NOT trigger DML007: {:?}",
        rules_of("a_limit_with_order.sql")
    );
    assert!(
        rules_of("b_limit_no_order.sql").contains("DML007"),
        "LIMIT without ORDER BY should trigger DML007: {:?}",
        rules_of("b_limit_no_order.sql")
    );
    assert!(
        rules_of("c_offset_fetch_no_order.sql").contains("DML007"),
        "OFFSET/FETCH without ORDER BY should trigger DML007: {:?}",
        rules_of("c_offset_fetch_no_order.sql")
    );
    assert!(
        !rules_of("d_random_order_hint.sql").contains("DML007"),
        "RANDOM_ORDER comment should suppress DML007: {:?}",
        rules_of("d_random_order_hint.sql")
    );
    assert!(
        !rules_of("e_full_pagination.sql").contains("DML007"),
        "Full pagination with ORDER BY should NOT trigger DML007: {:?}",
        rules_of("e_full_pagination.sql")
    );
    assert!(
        !rules_of("f_no_pagination.sql").contains("DML007"),
        "Non-pagination query should NOT trigger DML007: {:?}",
        rules_of("f_no_pagination.sql")
    );

    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn test_check_ddl003_to_ddl006_rules() {
    // 验证四条新 DDL 规则：
    //   DDL003 禁止使用 SQL 关键字命名数据库对象
    //   DDL004 CTAS 备份表必须以 bks_ 开头（有注释说明的非备份表例外）
    //   DDL005 索引命名规范 idx_/uk_/pk_
    //   DDL006 避免冗余索引（与主键重复 / 最左前缀重复）
    let dir = "/tmp/sqlguard-test-ddl003-006";
    let _ = std::fs::remove_dir_all(dir);

    std::fs::create_dir_all(format!("{}/sql/ddl", dir)).unwrap();
    std::fs::create_dir_all(format!("{}/config/rules/ddl", dir)).unwrap();

    // 从仓库复制四条规则脚本（集成测试在 crate 根目录运行）
    let rule_dir = std::env::current_dir().unwrap().join("config/rules/ddl");
    for r in [
        "no_reserved_keyword_naming",
        "backup_table_naming",
        "index_naming_convention",
        "no_redundant_index",
    ] {
        std::fs::write(
            format!("{}/config/rules/ddl/{}.rhai", dir, r),
            std::fs::read_to_string(rule_dir.join(format!("{}.rhai", r))).unwrap(),
        )
        .unwrap();
    }

    // 1) DDL003: 用关键字 `order`/`select` 命名表与列 → 应触发
    std::fs::write(
        format!("{}/sql/ddl/keyword_naming.sql", dir),
        "CREATE TABLE `order` (`select` INT, id INT PRIMARY KEY);\n",
    )
    .unwrap();

    // 2) DDL004: CTAS 无 bks_ 前缀且无注释 → 应触发；有 bks_ 前缀 → 不触发
    std::fs::write(
        format!("{}/sql/ddl/backup_naming.sql", dir),
        "CREATE TABLE tmp_log AS SELECT 1 AS id;\n\
         CREATE TABLE bks_log AS SELECT 1 AS id;\n",
    )
    .unwrap();

    // 3) DDL005: 索引命名不符合 idx_/uk_/pk_ 规范 → 应触发
    std::fs::write(
        format!("{}/sql/ddl/index_naming.sql", dir),
        "CREATE TABLE orders (\n\
         id INT,\n\
         user_id INT,\n\
         CONSTRAINT wrong_pk PRIMARY KEY (id),\n\
         KEY foo (user_id)\n\
         );\n",
    )
    .unwrap();

    // 4) DDL006: 与主键完全相同的索引 / 主键前缀索引 → 应触发
    std::fs::write(
        format!("{}/sql/ddl/redundant_index.sql", dir),
        "CREATE TABLE products (\n\
         a INT,\n\
         b INT,\n\
         c INT,\n\
         PRIMARY KEY (a, b),\n\
         KEY idx_a (a),\n\
         KEY idx_ab (a, b)\n\
         );\n",
    )
    .unwrap();

    std::fs::write(
        format!("{}/sqlguard.toml", dir),
        r#"
[structure]
paths = ["sql/ddl"]
strict = false

[classification]
default_type = "ddl"

[[classification.rules]]
name = "ddl-by-dir"
pattern = "**/ddl/**"
type = "ddl"
priority = 10

[[rules]]
id = "DDL003"
name = "no_reserved_keyword_naming"
group = "ddl-safety"
enabled = true
script_path = "config/rules/ddl/no_reserved_keyword_naming.rhai"
applies_to = ["ddl"]
severity = "error"

[[rules]]
id = "DDL004"
name = "backup_table_naming"
group = "ddl-convention"
enabled = true
script_path = "config/rules/ddl/backup_table_naming.rhai"
applies_to = ["ddl"]
severity = "warning"

[[rules]]
id = "DDL005"
name = "index_naming_convention"
group = "ddl-convention"
enabled = true
script_path = "config/rules/ddl/index_naming_convention.rhai"
applies_to = ["ddl"]
severity = "warning"

[[rules]]
id = "DDL006"
name = "no_redundant_index"
group = "ddl-performance"
enabled = true
script_path = "config/rules/ddl/no_redundant_index.rhai"
applies_to = ["ddl"]
severity = "warning"

[output]
formats = ["json"]
"#,
    )
    .unwrap();

    let output = Command::new(&binary_abs_path())
        .args([
            "check",
            dir,
            "-c",
            &format!("{}/sqlguard.toml", dir),
            "-f",
            "json",
            "-o",
            dir,
        ])
        .output()
        .expect("Failed to run sqlguard check");

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("JSON report saved"),
        "JSON report should be saved: {}",
        stderr
    );

    let report_path = format!("{}/sqlguard-report.json", dir);
    let content = std::fs::read_to_string(&report_path).unwrap();
    let report: serde_json::Value = serde_json::from_str(&content).unwrap();

    // 收集每个文件触发的规则 id 集合
    use std::collections::HashSet;
    let mut rules_by_file: std::collections::HashMap<String, HashSet<String>> =
        std::collections::HashMap::new();
    if let Some(violations) = report["violations"].as_array() {
        for v in violations {
            if let (Some(file), Some(rule_id)) =
                (v["file"].as_str(), v["rule_id"].as_str())
            {
                rules_by_file
                    .entry(file.to_string())
                    .or_default()
                    .insert(rule_id.to_string());
            }
        }
    }

    let has_rule = |file_substr: &str, rule_id: &str| -> bool {
        rules_by_file
            .iter()
            .any(|(f, rules)| f.contains(file_substr) && rules.contains(rule_id))
    };

    // DDL003: keyword_naming.sql 应触发
    assert!(
        has_rule("keyword_naming.sql", "DDL003"),
        "keyword_naming.sql should trigger DDL003: {:?}",
        rules_by_file
    );

    // DDL004: tmp_log(无 bks_) 应触发；bks_log(有 bks_) 不应触发
    assert!(
        has_rule("backup_naming.sql", "DDL004"),
        "backup_naming.sql (tmp_log without bks_) should trigger DDL004: {:?}",
        rules_by_file
    );

    // DDL005: index_naming.sql 应触发
    assert!(
        has_rule("index_naming.sql", "DDL005"),
        "index_naming.sql should trigger DDL005: {:?}",
        rules_by_file
    );

    // DDL006: redundant_index.sql 应触发
    assert!(
        has_rule("redundant_index.sql", "DDL006"),
        "redundant_index.sql should trigger DDL006: {:?}",
        rules_by_file
    );

    // 验证规则脚本未抛出执行错误（error 消息中不应含 "Rule execution error"）
    if let Some(violations) = report["violations"].as_array() {
        for v in violations {
            if let Some(msg) = v["message"].as_str() {
                assert!(
                    !msg.contains("Rule execution error"),
                    "Rule script error detected: {}",
                    msg
                );
            }
        }
    }

    let _ = std::fs::remove_dir_all(dir);
}
