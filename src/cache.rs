//! 文件级 mtime/size 缓存（P2-8）。
//!
//! 对未修改的文件复用上次检查的 violations，跳过解析与规则执行，
//! 大仓库重复 `sqlguard check` 时显著提速。
//!
//! ## 缓存 key
//! 文件相对 `target_dir` 的路径 + `(mtime_sec, size)`。
//!
//! ## 缓存失效（整体清空）
//! `run_signature` 不匹配时，缓存文件整体丢弃重建。签名聚合：
//! - SqlGuard 版本（`CARGO_PKG_VERSION`）
//! - SQL 方言（`[dialect]` 或 CLI `--dialect`）——含 GaussDB 重写规则集
//!   （当前阶段 1 规则集固定全开，`dialect=gaussdb` 唯一确定重写行为；
//!   未来引入配置开关时需把规则集 ID 纳入签名）
//! - 方言回退链第二候选（`dialect_fallback`）
//! - 规则筛选器（CLI `--rules/--groups/--exclude-*`）
//! - 主配置文件 `(mtime, size)`
//! - 规则配置文件 `(mtime, size)`（如有 `rules_file`）
//! - 规则脚本目录下所有 `.rhai` 文件 `(path, mtime, size)` 递归聚合
//!
//! ## 默认关闭
//! `[cache].enabled = false`，CLI `--cache` 可强制开启。

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

use serde::{Deserialize, Serialize};

use crate::config::{CheckDialect, Config};
use crate::error::Violation;
use crate::rule::engine::ast::RuleFilter;

const CACHE_VERSION: u32 = 1;

/// 缓存文件根结构，序列化为 `.sqlguard-cache.json`。
#[derive(Serialize, Deserialize)]
pub struct CacheFile {
    pub version: u32,
    pub sqlguard_version: String,
    /// 运行签名，不匹配则整体丢弃。
    pub run_signature: String,
    /// key = 文件相对 target_dir 的路径（正斜杠）。
    pub entries: HashMap<String, CacheEntry>,
}

/// 单个文件的缓存条目。
#[derive(Serialize, Deserialize)]
pub struct CacheEntry {
    pub mtime_sec: u64,
    pub size: u64,
    pub violations: Vec<Violation>,
}

/// 文件级缓存句柄。
///
/// `inner = None` 表示禁用（所有 get 返回 None，insert 是 no-op）。
pub struct FileCache {
    inner: Option<CacheFile>,
    cache_path: PathBuf,
    target_dir: PathBuf,
    run_signature: String,
    dirty: bool,
}

impl FileCache {
    /// 构造一个禁用的缓存句柄（所有操作 no-op）。
    pub fn disabled() -> Self {
        Self {
            inner: None,
            cache_path: PathBuf::new(),
            target_dir: PathBuf::new(),
            run_signature: String::new(),
            dirty: false,
        }
    }

    /// 加载缓存文件。版本或签名不匹配时返回空缓存（不报错，静默重建）。
    ///
    /// 注意：启用场景下 `inner` 始终为 `Some`（即使是首次运行的空缓存），
    /// `None` 仅由 [`FileCache::disabled`] 用于表示完全禁用。
    pub fn load(cache_path: PathBuf, target_dir: PathBuf, run_signature: String) -> Self {
        let inner = match fs::read_to_string(&cache_path) {
            Ok(content) => match serde_json::from_str::<CacheFile>(&content) {
                Ok(c) if c.version == CACHE_VERSION && c.run_signature == run_signature => c,
                _ => empty_cache_file(&run_signature),
            },
            Err(_) => empty_cache_file(&run_signature),
        };
        Self {
            inner: Some(inner),
            cache_path,
            target_dir,
            run_signature,
            dirty: false,
        }
    }

    pub fn run_signature(&self) -> &str {
        &self.run_signature
    }

    /// 查询缓存。命中且 `(mtime, size)` 一致时返回 violations（file_path 重写为当前路径）。
    pub fn get(&self, file_path: &Path) -> Option<Vec<Violation>> {
        let cache = self.inner.as_ref()?;
        let rel = self.rel_key(file_path)?;
        let entry = cache.entries.get(&rel)?;
        let meta = fs::metadata(file_path).ok()?;
        let mtime = file_mtime_sec(&meta)?;
        let size = meta.len();
        if entry.mtime_sec == mtime && entry.size == size {
            let mut vs = entry.violations.clone();
            for v in &mut vs {
                v.file_path = file_path.to_path_buf();
            }
            Some(vs)
        } else {
            None
        }
    }

    /// 写入缓存条目。禁用时 no-op。
    pub fn insert(&mut self, file_path: &Path, violations: Vec<Violation>) {
        if self.inner.is_none() {
            return;
        }
        // 先计算 rel key 与 metadata，避免与 inner 的可变借用冲突
        let rel = match self.rel_key(file_path) {
            Some(r) => r,
            None => return,
        };
        let meta = match fs::metadata(file_path) {
            Ok(m) => m,
            Err(_) => return,
        };
        let mtime = file_mtime_sec(&meta).unwrap_or(0);
        let size = meta.len();
        if let Some(cache) = self.inner.as_mut() {
            cache.entries.insert(
                rel,
                CacheEntry {
                    mtime_sec: mtime,
                    size,
                    violations,
                },
            );
            self.dirty = true;
        }
    }

    /// 写回磁盘（仅在 dirty 且启用时）。失败静默忽略——缓存不可用不应阻断检查。
    pub fn flush(&self) {
        if !self.dirty {
            return;
        }
        let cache = match self.inner.as_ref() {
            Some(c) => c,
            None => return,
        };
        if let Some(parent) = self.cache_path.parent() {
            let _ = fs::create_dir_all(parent);
        }
        if let Ok(json) = serde_json::to_string_pretty(cache) {
            let _ = fs::write(&self.cache_path, json);
        }
    }

    /// 计算文件相对 target_dir 的 key（正斜杠）。无法 strip_prefix 时回退到全路径。
    fn rel_key(&self, file_path: &Path) -> Option<String> {
        let rel = file_path
            .strip_prefix(&self.target_dir)
            .map(|p| p.to_string_lossy().replace('\\', "/"))
            .unwrap_or_else(|_| file_path.to_string_lossy().replace('\\', "/"));
        Some(rel)
    }
}

fn empty_cache_file(run_signature: &str) -> CacheFile {
    CacheFile {
        version: CACHE_VERSION,
        sqlguard_version: env!("CARGO_PKG_VERSION").to_string(),
        run_signature: run_signature.to_string(),
        entries: HashMap::new(),
    }
}

/// 计算运行签名。任一输入变化时签名改变，触发缓存整体失效。
pub fn compute_run_signature(
    config_path: &Path,
    config: &Config,
    config_dir: &Path,
    dialect: CheckDialect,
    dialect_fallback: Option<CheckDialect>,
    filter: &RuleFilter,
) -> String {
    let mut parts: Vec<String> = Vec::new();

    parts.push(format!("v={}", env!("CARGO_PKG_VERSION")));
    parts.push(format!("dialect={}", dialect.as_str()));
    parts.push(format!(
        "dialect_fb={}",
        dialect_fallback.map(|d| d.as_str()).unwrap_or("none")
    ));
    parts.push(format!("ir={}", filter.include_rules.join(",")));
    parts.push(format!("ig={}", filter.include_groups.join(",")));
    parts.push(format!("er={}", filter.exclude_rules.join(",")));
    parts.push(format!("eg={}", filter.exclude_groups.join(",")));

    if let Some(s) = file_signature(config_path) {
        parts.push(format!("cfg={}", s));
    }

    if let Some(rf) = &config.rules_file {
        let rf_path = if rf.is_absolute() {
            rf.clone()
        } else {
            config_dir.join(rf)
        };
        if let Some(s) = file_signature(&rf_path) {
            parts.push(format!("rf={}", s));
        }
    }

    let rules_dir = if config.rules_dir.as_os_str().is_empty() {
        config_dir.to_path_buf()
    } else {
        config.rules_dir.clone()
    };
    if let Some(s) = dir_signature_recursive(&rules_dir, "rhai") {
        parts.push(format!("rhai={}", s));
    }

    parts.join("|")
}

fn file_mtime_sec(meta: &fs::Metadata) -> Option<u64> {
    meta.modified()
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map(|d| d.as_secs())
}

fn file_signature(path: &Path) -> Option<String> {
    let meta = fs::metadata(path).ok()?;
    let mtime = file_mtime_sec(&meta).unwrap_or(0);
    Some(format!("{}:{}", mtime, meta.len()))
}

/// 递归扫描目录下所有指定扩展名的文件，聚合 `(path, mtime, size)` 签名。
/// 结果按路径排序，保证不同扫描顺序产出相同签名。
fn dir_signature_recursive(dir: &Path, ext: &str) -> Option<String> {
    let mut sigs: Vec<(String, String)> = Vec::new();
    collect_signatures(dir, ext, &mut sigs);
    if sigs.is_empty() {
        return None;
    }
    sigs.sort_by(|a, b| a.0.cmp(&b.0));
    let combined = sigs
        .iter()
        .map(|(n, s)| format!("{}={}", n, s))
        .collect::<Vec<_>>()
        .join(",");
    Some(combined)
}

fn collect_signatures(dir: &Path, ext: &str, out: &mut Vec<(String, String)>) {
    let entries = match fs::read_dir(dir) {
        Ok(e) => e,
        Err(_) => return,
    };
    for entry in entries.flatten() {
        let p = entry.path();
        if p.is_dir() {
            collect_signatures(&p, ext, out);
        } else if p.extension().map(|e| e == ext).unwrap_or(false) {
            if let Some(s) = file_signature(&p) {
                let name = p.to_string_lossy().replace('\\', "/");
                out.push((name, s));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::Violation;
    use std::io::Write;

    fn write_file(path: &Path, content: &str) {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        let mut f = fs::File::create(path).unwrap();
        f.write_all(content.as_bytes()).unwrap();
    }

    fn make_violation(rule_id: &str, file_path: &Path) -> Violation {
        Violation {
            rule_id: rule_id.to_string(),
            rule_name: "test".to_string(),
            rule_group: None,
            severity: "error".to_string(),
            message: "msg".to_string(),
            file_path: file_path.to_path_buf(),
            script_type: "dml".to_string(),
            line: Some(1),
            end_line: Some(1),
            column: Some(1),
        }
    }

    #[test]
    fn disabled_cache_is_noop() {
        let mut cache = FileCache::disabled();
        let tmp = std::env::temp_dir().join("sqlguard_cache_disabled_test.sql");
        write_file(&tmp, "SELECT 1;");
        assert!(cache.get(&tmp).is_none());
        cache.insert(&tmp, vec![make_violation("DML001", &tmp)]);
        assert!(cache.get(&tmp).is_none());
        cache.flush(); // 不应 panic
    }

    #[test]
    fn hit_returns_violations_with_rewritten_path() {
        let dir = std::env::temp_dir().join("sqlguard_cache_hit_test");
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let sql = dir.join("a.sql");
        write_file(&sql, "SELECT 1;");

        let cache_path = dir.join(".sqlguard-cache.json");
        let mut cache = FileCache::load(cache_path, dir.clone(), "sig1".to_string());
        cache.insert(&sql, vec![make_violation("DML001", &sql)]);

        let got = cache.get(&sql).expect("should hit");
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].rule_id, "DML001");
        // file_path 应被重写为当前路径
        assert_eq!(got[0].file_path, sql);
    }

    #[test]
    fn miss_after_mtime_change() {
        let dir = std::env::temp_dir().join("sqlguard_cache_mtime_test");
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let sql = dir.join("a.sql");
        write_file(&sql, "SELECT 1;");

        let cache_path = dir.join(".sqlguard-cache.json");
        let mut cache = FileCache::load(cache_path, dir.clone(), "sig1".to_string());
        cache.insert(&sql, vec![make_violation("DML001", &sql)]);
        assert!(cache.get(&sql).is_some());

        // 修改文件内容（mtime + size 变化）
        std::thread::sleep(std::time::Duration::from_secs(2));
        write_file(&sql, "SELECT 1, 2, 3, 4 FROM x;");
        assert!(cache.get(&sql).is_none(), "mtime/size 变化后应 miss");
    }

    #[test]
    fn signature_mismatch_invalidates_all() {
        let dir = std::env::temp_dir().join("sqlguard_cache_sig_test");
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let sql = dir.join("a.sql");
        write_file(&sql, "SELECT 1;");

        let cache_path = dir.join(".sqlguard-cache.json");

        // 第一次：签名 sig1，写入条目
        let mut cache = FileCache::load(cache_path.clone(), dir.clone(), "sig1".to_string());
        cache.insert(&sql, vec![make_violation("DML001", &sql)]);
        cache.flush();

        // 第二次：签名 sig2，应整体丢弃
        let cache2 = FileCache::load(cache_path, dir.clone(), "sig2".to_string());
        assert!(cache2.get(&sql).is_none(), "签名不匹配时应整体失效");
    }

    #[test]
    fn flush_persists_to_disk() {
        let dir = std::env::temp_dir().join("sqlguard_cache_flush_test");
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let sql = dir.join("a.sql");
        write_file(&sql, "SELECT 1;");

        let cache_path = dir.join(".sqlguard-cache.json");

        let mut cache = FileCache::load(cache_path.clone(), dir.clone(), "sig1".to_string());
        cache.insert(&sql, vec![make_violation("DML001", &sql)]);
        cache.flush();

        // 磁盘文件应存在
        assert!(cache_path.exists(), "flush 后缓存文件应存在");
        let content = fs::read_to_string(&cache_path).unwrap();
        assert!(content.contains("DML001"), "缓存内容应包含 violation");
        assert!(content.contains("\"run_signature\": \"sig1\""));
    }

    #[test]
    fn reload_from_disk_hits() {
        let dir = std::env::temp_dir().join("sqlguard_cache_reload_test");
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let sql = dir.join("a.sql");
        write_file(&sql, "SELECT 1;");

        let cache_path = dir.join(".sqlguard-cache.json");

        // 第一次运行：写入并 flush
        let mut cache = FileCache::load(cache_path.clone(), dir.clone(), "sig1".to_string());
        cache.insert(&sql, vec![make_violation("DML001", &sql)]);
        cache.flush();

        // 第二次运行（模拟新进程）：重新 load，应命中
        let cache2 = FileCache::load(cache_path, dir.clone(), "sig1".to_string());
        let got = cache2.get(&sql).expect("reload 后应命中");
        assert_eq!(got[0].rule_id, "DML001");
    }

    #[test]
    fn rel_key_uses_forward_slash() {
        let dir = std::env::temp_dir().join("sqlguard_cache_relkey_test");
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let sub = dir.join("sub").join("a.sql");
        write_file(&sub, "SELECT 1;");

        let cache_path = dir.join(".cache.json");
        let mut cache = FileCache::load(cache_path, dir.clone(), "sig1".to_string());
        cache.insert(&sub, vec![make_violation("DML001", &sub)]);
        cache.flush();

        let content = fs::read_to_string(
            &std::env::temp_dir()
                .join("sqlguard_cache_relkey_test")
                .join(".cache.json"),
        )
        .unwrap();
        assert!(
            content.contains("sub/a.sql"),
            "相对路径应使用正斜杠: {}",
            content
        );
    }
}
