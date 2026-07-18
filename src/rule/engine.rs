use std::path::Path;
use std::fs;

use rhai::{Engine, Scope, Dynamic, Array, Map};

use crate::config::{Config, RuleConfig};
use crate::error::{SqlGuardError, Violation};

#[derive(Debug, Clone)]
pub struct RuleContext {
    pub sql_content: String,
    pub file_path: String,
    pub file_name: String,
    pub script_type: String,
    pub line_count: usize,
}

pub fn run_rules_for_file(
    file_path: &Path,
    sql_content: &str,
    script_type: &str,
    config: &Config,
    config_dir: &Path,
) -> Result<Vec<Violation>, SqlGuardError> {
    let mut violations = Vec::new();

    let context = RuleContext {
        sql_content: sql_content.to_string(),
        file_path: file_path.to_string_lossy().to_string(),
        file_name: file_path
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_default(),
        script_type: script_type.to_string(),
        line_count: sql_content.lines().count(),
    };

    for rule_config in &config.rules {
        if !rule_config.enabled {
            continue;
        }
        if !rule_config.applies_to.contains(&script_type.to_string()) {
            continue;
        }

        let script_path = config.resolve_script_path(&rule_config.script_path, config_dir);
        if !script_path.exists() {
            violations.push(Violation {
                rule_name: rule_config.name.clone(),
                severity: rule_config.severity.clone(),
                message: format!("Rule script not found: {}", script_path.display()),
                file_path: file_path.to_path_buf(),
                script_type: script_type.to_string(),
            });
            continue;
        }

        match run_single_rule(&context, rule_config, &script_path) {
            Ok(rule_violations) => violations.extend(rule_violations),
            Err(e) => {
                violations.push(Violation {
                    rule_name: rule_config.name.clone(),
                    severity: rule_config.severity.clone(),
                    message: format!("Rule execution error: {}", e),
                    file_path: file_path.to_path_buf(),
                    script_type: script_type.to_string(),
                });
            }
        }
    }

    Ok(violations)
}

fn run_single_rule(
    context: &RuleContext,
    rule_config: &RuleConfig,
    script_path: &Path,
) -> Result<Vec<Violation>, SqlGuardError> {
    let script = fs::read_to_string(script_path)
        .map_err(|e| SqlGuardError::ScriptError(format!("Failed to read script '{}': {}", script_path.display(), e)))?;

    let engine = Engine::new();

    let mut scope = Scope::new();

    let mut ctx_map = Map::new();
    ctx_map.insert("sql_content".into(), Dynamic::from(context.sql_content.clone()));
    ctx_map.insert("file_path".into(), Dynamic::from(context.file_path.clone()));
    ctx_map.insert("file_name".into(), Dynamic::from(context.file_name.clone()));
    ctx_map.insert("script_type".into(), Dynamic::from(context.script_type.clone()));
    ctx_map.insert("line_count".into(), Dynamic::from_int(context.line_count as i64));
    scope.push("context", Dynamic::from(ctx_map));

    let violations_array: Array = Array::new();
    scope.push("violations", Dynamic::from(violations_array));

    let result = engine.eval_with_scope::<Dynamic>(&mut scope, &script);

    match result {
        Ok(_) => {
            let final_violations = scope
                .get_value::<Array>("violations")
                .unwrap_or_default();

            let violations: Vec<Violation> = final_violations
                .iter()
                .map(|v| {
                    let msg = if v.is_string() {
                        v.clone_cast::<String>()
                    } else if v.is_map() {
                        let m = v.clone_cast::<Map>();
                        m.get("message")
                            .and_then(|d| d.clone().try_cast::<String>())
                            .unwrap_or_else(|| "Unknown violation".to_string())
                    } else {
                        v.to_string()
                    };
                    Violation {
                        rule_name: rule_config.name.clone(),
                        severity: rule_config.severity.clone(),
                        message: msg,
                        file_path: Path::new(&context.file_path).to_path_buf(),
                        script_type: context.script_type.clone(),
                    }
                })
                .collect();

            Ok(violations)
        }
        Err(e) => {
            let err_msg = e.to_string();
            if err_msg.to_lowercase().contains("runtime error") || err_msg.contains("throw")
            {
                Ok(vec![Violation {
                    rule_name: rule_config.name.clone(),
                    severity: rule_config.severity.clone(),
                    message: err_msg,
                    file_path: Path::new(&context.file_path).to_path_buf(),
                    script_type: context.script_type.clone(),
                }])
            } else {
                Err(SqlGuardError::ScriptError(err_msg))
            }
        }
    }
}
