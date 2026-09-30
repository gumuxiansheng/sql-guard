//! 规则包锁文件 `sqlguard.lock`（M3）。
//!
//! 目的：让"同一份配置 + 锁文件"在任意机器/CI 上得到**完全相同的规则集**，
//! 使检查结论可复现、可审计。
//!
//! - `sqlguard rules lock`：按当前解析结果生成 / 更新锁文件。
//! - `sqlguard rules verify` / `check --locked`：校验锁与实际情况一致。
//!
//! 校验强度分两档（见设计稿 §6.3 与 §7 的 IO 折中）：
//! - 默认（Auto）：只比对 `name` + `version`（零文件读取）；
//! - `--locked`（Strict）：额外比对 **checksum**（读取包内全部文件），
//!   并要求锁文件存在、且不含多余条目。
//!
//! `source` 语法本期只实现 `path:`；`git+` / `registry:` 为预留（后续里程碑）。

use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::config::Config;
use crate::error::SqlGuardError;

/// 锁文件名（位于配置文件同目录）。
pub const LOCK_FILE_NAME: &str = "sqlguard.lock";

/// 锁文件格式版本。
const LOCK_FORMAT_VERSION: u32 = 1;

/// 锁文件内容。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LockFile {
    /// 格式版本（未来结构变更时递增）。
    #[serde(default = "default_format_version")]
    pub version: u32,
    /// 被锁定的规则包（`[[pack]]`）。
    #[serde(default, rename = "pack")]
    pub packs: Vec<LockedPack>,
}

/// 单个被锁定的规则包。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LockedPack {
    pub name: String,
    pub version: String,
    /// 来源：`path:<相对配置文件目录的路径>`（本期唯一实现）。
    pub source: String,
    /// `sha256:<hex>`，覆盖包内按路径排序后的全部文件。
    pub checksum: String,
    /// 该包声明的规则 API 版本（冗余记录，便于审计）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub api_version: Option<u32>,
}

fn default_format_version() -> u32 {
    LOCK_FORMAT_VERSION
}

/// 锁校验强度。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LockMode {
    /// 不校验（`--no-lock`）。
    Off,
    /// 默认：比对 name + version。
    Auto,
    /// `--locked`：额外比对 checksum，并要求锁完整。
    Strict,
}

fn lock_err(msg: impl Into<String>) -> SqlGuardError {
    SqlGuardError::ConfigError(msg.into())
}

/// 锁文件路径（配置文件所在目录）。
pub fn lock_path(config_dir: &Path) -> PathBuf {
    config_dir.join(LOCK_FILE_NAME)
}

/// 按当前解析结果构建锁内容。
pub fn build_lock(config: &Config, config_dir: &Path) -> Result<LockFile, SqlGuardError> {
    let cfg_dir = canonical_or_self(config_dir);
    let mut packs = Vec::with_capacity(config.resolved_packs.len());
    for pack in &config.resolved_packs {
        packs.push(LockedPack {
            name: pack.name.clone(),
            version: pack.version.clone(),
            source: source_of(&pack.root, &cfg_dir),
            checksum: pack_checksum(&pack.root)?,
            api_version: Some(pack.api_version),
        });
    }
    Ok(LockFile {
        version: LOCK_FORMAT_VERSION,
        packs,
    })
}

/// 写出锁文件（带说明头注释），返回写入路径。
pub fn write_lock(lock: &LockFile, config_dir: &Path) -> Result<PathBuf, SqlGuardError> {
    let body = toml::to_string_pretty(lock)
        .map_err(|e| lock_err(format!("failed to serialize {LOCK_FILE_NAME}: {e}")))?;
    let header = format!(
        "# {LOCK_FILE_NAME} — 规则包锁文件（由 `sqlguard rules lock` 生成，请勿手工编辑）\n\
         # 提交到版本库以保证规则集可复现。\n\n"
    );
    let path = lock_path(config_dir);
    fs::write(&path, format!("{header}{body}"))
        .map_err(|e| lock_err(format!("failed to write '{}': {e}", path.display())))?;
    Ok(path)
}

/// 读取锁文件；不存在时返回 `Ok(None)`。
pub fn read_lock(config_dir: &Path) -> Result<Option<LockFile>, SqlGuardError> {
    let path = lock_path(config_dir);
    if !path.exists() {
        return Ok(None);
    }
    let content = fs::read_to_string(&path)
        .map_err(|e| lock_err(format!("failed to read '{}': {e}", path.display())))?;
    let lock: LockFile = toml::from_str(&content)
        .map_err(|e| lock_err(format!("failed to parse '{}': {e}", path.display())))?;
    Ok(Some(lock))
}

/// 校验锁文件与当前解析结果是否一致。
///
/// `Ok(notes)` 为可展示的提示；不一致时返回 `Err`（调用方决定是否中止）。
pub fn verify_lock(
    config: &Config,
    config_dir: &Path,
    mode: LockMode,
) -> Result<Vec<String>, SqlGuardError> {
    if mode == LockMode::Off {
        return Ok(Vec::new());
    }
    let mut notes = Vec::new();
    let lock = read_lock(config_dir)?;

    let Some(lock) = lock else {
        if mode == LockMode::Strict {
            return Err(lock_err(format!(
                "{LOCK_FILE_NAME} not found; run `sqlguard rules lock` and commit it \
                 (CI is expected to run with --locked)."
            )));
        }
        if !config.resolved_packs.is_empty() {
            notes.push(format!(
                "no {LOCK_FILE_NAME}; run `sqlguard rules lock` to pin rule pack versions"
            ));
        }
        return Ok(notes);
    };

    let mut problems: Vec<String> = Vec::new();

    for pack in &config.resolved_packs {
        match lock.packs.iter().find(|p| p.name == pack.name) {
            None => problems.push(format!(
                "rule pack '{}' ({}) is not recorded in {LOCK_FILE_NAME}",
                pack.name, pack.version
            )),
            Some(locked) => {
                if locked.version != pack.version {
                    problems.push(format!(
                        "rule pack '{}' resolved to version {} but {LOCK_FILE_NAME} pins {}",
                        pack.name, pack.version, locked.version
                    ));
                }
                if mode == LockMode::Strict {
                    let actual = pack_checksum(&pack.root)?;
                    if locked.checksum != actual {
                        problems.push(format!(
                            "rule pack '{}' checksum mismatch (lock {}, actual {}) — the pack \
                             content changed without a version bump",
                            pack.name, locked.checksum, actual
                        ));
                    }
                }
            }
        }
    }

    if mode == LockMode::Strict {
        for locked in &lock.packs {
            if !config.resolved_packs.iter().any(|p| p.name == locked.name) {
                problems.push(format!(
                    "{LOCK_FILE_NAME} pins '{}' but the config no longer resolves it",
                    locked.name
                ));
            }
        }
    }

    if problems.is_empty() {
        Ok(notes)
    } else {
        Err(lock_err(format!(
            "{} (run `sqlguard rules lock` to update {LOCK_FILE_NAME})",
            problems.join("; ")
        )))
    }
}

/// 计算包内容校验和：`sha256:` + 覆盖包内**按路径排序**的全部文件的哈希。
///
/// 逐文件喂入 `相对路径\0长度\0内容`，路径统一用 `/` 分隔——保证跨平台、
/// 跨遍历顺序结果稳定。
pub fn pack_checksum(root: &Path) -> Result<String, SqlGuardError> {
    let mut files: Vec<(String, PathBuf)> = Vec::new();
    collect_files(root, root, &mut files)?;
    files.sort_by(|a, b| a.0.cmp(&b.0));

    let mut hasher = Sha256::new();
    for (rel, path) in files {
        let bytes = fs::read(&path)
            .map_err(|e| lock_err(format!("failed to read '{}': {e}", path.display())))?;
        hasher.update(rel.as_bytes());
        hasher.update([0u8]);
        hasher.update((bytes.len() as u64).to_le_bytes());
        hasher.update(&bytes);
    }
    Ok(format!("sha256:{}", to_hex(&hasher.finalize())))
}

/// 递归收集包内文件（跳过 `.git` 等版本控制目录）。
fn collect_files(
    root: &Path,
    dir: &Path,
    out: &mut Vec<(String, PathBuf)>,
) -> Result<(), SqlGuardError> {
    let entries = fs::read_dir(dir)
        .map_err(|e| lock_err(format!("failed to read dir '{}': {e}", dir.display())))?;
    for entry in entries {
        let entry =
            entry.map_err(|e| lock_err(format!("failed to read dir '{}': {e}", dir.display())))?;
        let path = entry.path();
        let file_type = entry
            .file_type()
            .map_err(|e| lock_err(format!("failed to stat '{}': {e}", path.display())))?;
        if file_type.is_dir() {
            let name = entry.file_name();
            if name == ".git" || name == ".svn" || name == ".hg" {
                continue;
            }
            collect_files(root, &path, out)?;
        } else if file_type.is_file() {
            if let Ok(rel) = path.strip_prefix(root) {
                out.push((rel.to_string_lossy().replace('\\', "/"), path));
            }
        }
    }
    Ok(())
}

/// `source` 字段：包根相对配置文件目录（不可相对时用绝对路径）。
fn source_of(root: &Path, canonical_config_dir: &Path) -> String {
    match root.strip_prefix(canonical_config_dir) {
        Ok(rel) => format!("path:{}", rel.to_string_lossy().replace('\\', "/")),
        Err(_) => format!("path:{}", root.to_string_lossy().replace('\\', "/")),
    }
}

fn canonical_or_self(p: &Path) -> PathBuf {
    p.canonicalize().unwrap_or_else(|_| p.to_path_buf())
}

fn to_hex(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        out.push(HEX[(b >> 4) as usize] as char);
        out.push(HEX[(b & 0x0f) as usize] as char);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(name);
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn checksum_is_order_independent_and_content_sensitive() {
        let dir = tmp("sqlguard_lock_ck");
        // 以不同创建顺序写入同一组文件 → 结果必须一致
        fs::write(dir.join("b.rhai"), "b").unwrap();
        fs::write(dir.join("a.rhai"), "a").unwrap();
        let first = pack_checksum(&dir).unwrap();

        let dir2 = tmp("sqlguard_lock_ck2");
        fs::write(dir2.join("a.rhai"), "a").unwrap();
        fs::write(dir2.join("b.rhai"), "b").unwrap();
        assert_eq!(first, pack_checksum(&dir2).unwrap());
        assert!(first.starts_with("sha256:"));

        // 内容变化 → 校验和变化
        fs::write(dir.join("a.rhai"), "a!").unwrap();
        assert_ne!(first, pack_checksum(&dir).unwrap());

        let _ = fs::remove_dir_all(&dir);
        let _ = fs::remove_dir_all(&dir2);
    }

    #[test]
    fn checksum_ignores_git_dir_and_includes_nested() {
        let dir = tmp("sqlguard_lock_ck3");
        fs::create_dir_all(dir.join("rules/ddl")).unwrap();
        fs::create_dir_all(dir.join(".git")).unwrap();
        fs::write(dir.join("rules/ddl/x.rhai"), "x").unwrap();
        let with_git = pack_checksum(&dir).unwrap();

        fs::write(dir.join(".git/HEAD"), "ref: refs/heads/main").unwrap();
        assert_eq!(
            with_git,
            pack_checksum(&dir).unwrap(),
            ".git must not affect the checksum"
        );

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn checksum_hex_is_lowercase_64_chars() {
        let dir = tmp("sqlguard_lock_ck4");
        fs::write(dir.join("only.rhai"), "hello").unwrap();
        let sum = pack_checksum(&dir).unwrap();
        let hex = sum.strip_prefix("sha256:").unwrap();
        assert_eq!(hex.len(), 64);
        assert!(hex
            .chars()
            .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase()));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn lock_round_trips_through_toml() {
        let dir = tmp("sqlguard_lock_rt");
        let lock = LockFile {
            version: 1,
            packs: vec![LockedPack {
                name: "rules-core".to_string(),
                version: "2.1.0".to_string(),
                source: "path:vendor/rules/rules-core".to_string(),
                checksum: "sha256:abc".to_string(),
                api_version: Some(1),
            }],
        };
        let path = write_lock(&lock, &dir).unwrap();
        let text = fs::read_to_string(&path).unwrap();
        assert!(
            text.starts_with("# sqlguard.lock"),
            "header comment expected"
        );
        let parsed = read_lock(&dir).unwrap().expect("lock present");
        assert_eq!(parsed.version, 1);
        assert_eq!(parsed.packs, lock.packs);
        assert!(text.contains("[[pack]]"), "array-of-tables form expected");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn read_lock_returns_none_when_absent() {
        let dir = tmp("sqlguard_lock_absent");
        assert!(read_lock(&dir).unwrap().is_none());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn source_prefers_relative_to_config_dir() {
        let dir = tmp("sqlguard_lock_src");
        // 目标必须真实存在，canonicalize 才能成功（Windows 下临时目录可能含短名，
        // 未规范化时无法与规范化后的 config_dir 做前缀匹配）。
        fs::create_dir_all(dir.join("vendor/rules/x")).unwrap();
        let cfg = canonical_or_self(&dir);
        let inside = canonical_or_self(&dir.join("vendor/rules/x"));
        assert_eq!(source_of(&inside, &cfg), "path:vendor/rules/x");

        // 配置目录之外（这里是其父目录）→ 退化为绝对路径
        let outside = canonical_or_self(&std::env::temp_dir());
        assert!(source_of(&outside, &cfg).starts_with("path:"));
        let _ = fs::remove_dir_all(&dir);
    }
}
