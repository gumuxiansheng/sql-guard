//! 扫描文件编码支持。
//!
//! 默认只支持 UTF-8（与 `std::fs::read_to_string` 语义一致）。部分场景
//! （Windows 历史项目 / 老系统导出的脚本）SQL 与 Mapper XML 可能是 GBK、
//! GB18030、UTF-16 等编码，此时按 UTF-8 读取会直接失败。
//!
//! 本模块提供按用户指定编码读取文件的能力：
//! - 配置：`[scan] encoding = "gbk"`（或 CLI `--encoding gbk`）
//! - 支持的编码标签（WHATWG labels，经 `encoding_rs` 解析）：
//!   `utf-8` / `gbk` / `gb2312` / `gb18030` / `big5` / `shift_jis`（含
//!   `sjis` / `cp932`）/ `euc-jp` / `euc-kr` / `utf-16le` / `utf-16be` /
//!   `windows-1252`（含 `latin1` / `iso-8859-1`）/ `ascii` 等
//! - 额外支持 UTF-32 LE/BE（`encoding_rs` 不支持 UTF-32，此处手动解码）
//! - **BOM 优先于配置**：文件带 BOM 时按 BOM 判定编码并剥离 BOM
//!   （BOM 是编码的权威信号，能正确处理「配置了 GBK 但个别文件带 UTF-8 BOM」）
//! - 解码失败（编码选错）报错并提示可用的编码标签
//!
//! 注意：与 `[file_check]` 的 FILE001「必须 UTF-8 无 BOM」策略检查是两回事——
//! 本模块负责**读取**，FILE001 负责**策略**。配置非 UTF-8 编码时 FILE001
//! 自动跳过（见 `checker::encoding`），避免「用户声明 GBK 却被 FILE001 全量报错」。

use std::path::Path;

/// 默认编码（向后兼容：与 `std::fs::read_to_string` 行为一致）。
pub const DEFAULT_ENCODING: &str = "utf-8";

/// 标签归一化：去空白 + 小写。
fn normalize(label: &str) -> String {
    label.trim().to_ascii_lowercase()
}

/// 指定标签是否表示 UTF-8（用于 FILE001 策略检查的开关判断）。
pub fn is_utf8(label: &str) -> bool {
    matches!(
        normalize(label).as_str(),
        "utf-8" | "utf8" | "unicode-1-1-utf-8"
    )
}

/// 校验编码标签是否受支持。不区分大小写，忽略首尾空白。
///
/// 返回 `Err` 时附带支持的编码清单，便于用户修正配置。
pub fn validate(label: &str) -> Result<(), String> {
    let l = normalize(label);
    if l.is_empty() {
        return Err("encoding must not be empty".to_string());
    }
    if is_utf8(&l) || l == "utf-32le" || l == "utf-32be" {
        return Ok(());
    }
    if encoding_rs::Encoding::for_label(l.as_bytes()).is_some() {
        return Ok(());
    }
    Err(format!(
        "Unsupported encoding '{}'. Supported labels: utf-8, gbk, gb2312, gb18030, \
         big5, shift_jis (sjis, cp932), euc-jp, euc-kr, utf-16le, utf-16be, \
         utf-32le, utf-32be, windows-1252 (latin1, iso-8859-1), ascii, \
         iso-8859-5/8/15 等 WHATWG 编码标签。",
        label
    ))
}

/// 按指定编码解码字节内容（严格模式：非法字节序列报错，不静默替换）。
///
/// BOM 检测优先级：UTF-32 LE/BE（手动）→ UTF-8/UTF-16（`encoding_rs`）。
/// 带 BOM 时剥离 BOM；无 BOM 时按 `label` 指定编码解码。
pub fn decode(bytes: &[u8], label: &str) -> Result<String, String> {
    // 1. UTF-32 BOM：FF FE 00 00（LE）/ 00 00 FE FF（BE），须先于 UTF-16 LE（FF FE）判断
    if bytes.starts_with(&[0xFF, 0xFE, 0x00, 0x00]) {
        return decode_utf32(&bytes[4..], true);
    }
    if bytes.starts_with(&[0x00, 0x00, 0xFE, 0xFF]) {
        return decode_utf32(&bytes[4..], false);
    }
    // 2. UTF-8 / UTF-16 BOM：encoding_rs 的 BOM 检测（UTF-16 LE/BE 亦在此）
    if let Some((enc, len)) = encoding_rs::Encoding::for_bom(bytes) {
        return decode_strict(enc, &bytes[len..]);
    }
    // 3. 用户指定编码（无 BOM）
    let l = normalize(label);
    if l.is_empty() {
        return Err("encoding must not be empty".to_string());
    }
    if l == "utf-32le" {
        return decode_utf32(bytes, true);
    }
    if l == "utf-32be" {
        return decode_utf32(bytes, false);
    }
    let enc = encoding_rs::Encoding::for_label(l.as_bytes())
        .ok_or_else(|| format!("Unsupported encoding '{}'", label))?;
    decode_strict(enc, bytes)
}

/// 严格解码：任何非法字节序列都视为错误（不产生 U+FFFD 替换字符）。
fn decode_strict(
    enc: &'static encoding_rs::Encoding,
    bytes: &[u8],
) -> Result<String, String> {
    let mut decoder = enc.new_decoder_without_bom_handling();
    let capacity = decoder
        .max_utf8_buffer_length(bytes.len())
        .unwrap_or(bytes.len());
    let mut out = String::with_capacity(capacity);
    let (result, read) = decoder.decode_to_string_without_replacement(bytes, &mut out, true);
    match result {
        encoding_rs::DecoderResult::InputEmpty => Ok(out),
        encoding_rs::DecoderResult::OutputFull => {
            // 容量按 max_utf8_buffer_length 预分配，理论上不可达
            Err(format!(
                "decode buffer overflow for encoding '{}'",
                enc.name()
            ))
        }
        encoding_rs::DecoderResult::Malformed(len, after) => {
            // read 是「含非法序列在内」的已消费字节数；非法序列起始 = read - len - after
            let offset = read.saturating_sub(usize::from(len) + usize::from(after));
            Err(format!(
                "invalid byte sequence for encoding '{}' at offset {}",
                enc.name(),
                offset
            ))
        }
    }
}

/// 手动解码 UTF-32（encoding_rs 不支持）。BOM 已由调用方剥离。
fn decode_utf32(bytes: &[u8], little_endian: bool) -> Result<String, String> {
    if !bytes.len().is_multiple_of(4) {
        return Err(format!(
            "invalid UTF-32 data: length {} is not a multiple of 4",
            bytes.len()
        ));
    }
    let mut out = String::with_capacity(bytes.len() / 2);
    let mut i = 0;
    while i < bytes.len() {
        let raw = if little_endian {
            u32::from_le_bytes([bytes[i], bytes[i + 1], bytes[i + 2], bytes[i + 3]])
        } else {
            u32::from_be_bytes([bytes[i], bytes[i + 1], bytes[i + 2], bytes[i + 3]])
        };
        if raw > 0x10_FFFF || (0xD800..=0xDFFF).contains(&raw) {
            return Err(format!(
                "invalid UTF-32 code point U+{:08X} at byte offset {}",
                raw,
                i
            ));
        }
        // char::from_u32 对 0..=0x10FFFF 且非代理区恒为 Some
        out.push(char::from_u32(raw).unwrap_or('\u{FFFD}'));
        i += 4;
    }
    Ok(out)
}

/// 读取文件并按指定编码解码为字符串。
///
/// I/O 错误与解码错误都返回带路径与编码标签的描述，供调用方包装为
/// `SqlGuardError`；解码错误附带修复提示（指定正确的 `[scan] encoding`）。
pub fn read_to_string(path: &Path, label: &str) -> Result<String, String> {
    let bytes = std::fs::read(path)
        .map_err(|e| format!("Failed to read '{}': {}", path.display(), e))?;
    decode(&bytes, label).map_err(|e| {
        format!(
            "Failed to decode '{}' with encoding '{}': {}\n\
             Hint: set the correct encoding via [scan] encoding (or --encoding), \
             e.g. encoding = \"gbk\".",
            path.display(),
            label,
            e
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn gbk_bytes(s: &str) -> Vec<u8> {
        let (bytes, _, _) = encoding_rs::GBK.encode(s);
        bytes.into_owned()
    }

    #[test]
    fn ascii_is_identity() {
        assert_eq!(decode(b"SELECT 1;", "utf-8").unwrap(), "SELECT 1;");
    }

    #[test]
    fn empty_input_decodes_to_empty() {
        assert_eq!(decode(b"", "utf-8").unwrap(), "");
        assert_eq!(decode(b"", "gbk").unwrap(), "");
    }

    #[test]
    fn utf8_bom_is_stripped() {
        let mut bytes = vec![0xEF, 0xBB, 0xBF];
        bytes.extend_from_slice(b"SELECT 1;");
        assert_eq!(decode(&bytes, "utf-8").unwrap(), "SELECT 1;");
    }

    #[test]
    fn invalid_utf8_is_rejected() {
        let err = decode(&[0x53, 0x45, 0xFF, 0x4C], "utf-8").unwrap_err();
        assert!(err.contains("invalid byte sequence"), "got: {}", err);
    }

    #[test]
    fn gbk_chinese_round_trip() {
        let bytes = gbk_bytes("-- 中文注释\nSELECT * FROM t;");
        assert_eq!(
            decode(&bytes, "gbk").unwrap(),
            "-- 中文注释\nSELECT * FROM t;"
        );
    }

    #[test]
    fn gb2312_alias_accepts_gbk_bytes() {
        let bytes = gbk_bytes("中文");
        assert_eq!(decode(&bytes, "gb2312").unwrap(), "中文");
        assert_eq!(decode(&bytes, "GB2312").unwrap(), "中文");
    }

    #[test]
    fn gbk_invalid_byte_is_rejected() {
        // 0xFF 不是合法的 GBK 前导字节
        let err = decode(&[0xFF], "gbk").unwrap_err();
        assert!(err.contains("invalid byte sequence"), "got: {}", err);
    }

    #[test]
    fn utf16le_bom_decoded() {
        let mut bytes = vec![0xFF, 0xFE];
        for u in "SELECT 1;".encode_utf16() {
            bytes.extend_from_slice(&u.to_le_bytes());
        }
        assert_eq!(decode(&bytes, "gbk").unwrap(), "SELECT 1;");
    }

    #[test]
    fn utf16le_label_without_bom() {
        let mut bytes = Vec::new();
        for u in "SELECT 1;".encode_utf16() {
            bytes.extend_from_slice(&u.to_le_bytes());
        }
        assert_eq!(decode(&bytes, "utf-16le").unwrap(), "SELECT 1;");
    }

    #[test]
    fn utf16be_label_without_bom() {
        let mut bytes = Vec::new();
        for u in "SELECT 1;".encode_utf16() {
            bytes.extend_from_slice(&u.to_be_bytes());
        }
        assert_eq!(decode(&bytes, "utf-16be").unwrap(), "SELECT 1;");
    }

    #[test]
    fn utf32le_bom_decoded_before_utf16() {
        let mut bytes = vec![0xFF, 0xFE, 0x00, 0x00];
        for c in "中文".chars() {
            bytes.extend_from_slice(&(c as u32).to_le_bytes());
        }
        assert_eq!(decode(&bytes, "utf-8").unwrap(), "中文");
    }

    #[test]
    fn utf32be_label_without_bom() {
        let mut bytes = Vec::new();
        for c in "中文".chars() {
            bytes.extend_from_slice(&(c as u32).to_be_bytes());
        }
        assert_eq!(decode(&bytes, "utf-32be").unwrap(), "中文");
    }

    #[test]
    fn utf32_odd_length_is_rejected() {
        let err = decode(&[0x00, 0x00, 0x00], "utf-32be").unwrap_err();
        assert!(err.contains("multiple of 4"), "got: {}", err);
    }

    #[test]
    fn big5_round_trip() {
        let (bytes, _, _) = encoding_rs::BIG5.encode("中文");
        assert_eq!(decode(&bytes, "big5").unwrap(), "中文");
    }

    #[test]
    fn windows1252_label_accepted() {
        assert!(validate("windows-1252").is_ok());
        assert!(validate("latin1").is_ok());
        assert!(validate("iso-8859-1").is_ok());
    }

    #[test]
    fn validate_unknown_label_errors_with_list() {
        let err = validate("klingon").unwrap_err();
        assert!(err.contains("Unsupported encoding"), "got: {}", err);
        assert!(err.contains("gbk"), "err should list supported labels: {}", err);
    }

    #[test]
    fn validate_empty_label_errors() {
        assert!(validate("").is_err());
        assert!(validate("   ").is_err());
    }

    #[test]
    fn validate_accepts_utf32_and_case_insensitive() {
        assert!(validate("utf-32le").is_ok());
        assert!(validate("UTF-32BE").is_ok());
        assert!(validate("Utf-8").is_ok());
        assert!(validate("GBK").is_ok());
    }

    #[test]
    fn is_utf8_detects_aliases() {
        assert!(is_utf8("utf-8"));
        assert!(is_utf8("UTF8"));
        assert!(!is_utf8("gbk"));
        assert!(!is_utf8("utf-16le"));
    }

    #[test]
    fn read_to_string_missing_file_reports_path() {
        let err = read_to_string(Path::new("no/such/file.sql"), "utf-8").unwrap_err();
        assert!(err.contains("no/such/file.sql"), "got: {}", err);
    }

    #[test]
    fn read_to_string_gbk_file_with_hint() {
        let dir = std::env::temp_dir().join("sqlguard_encoding_read_test");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("gbk.sql");
        std::fs::write(&path, gbk_bytes("-- 中文\nSELECT 1;")).unwrap();

        assert_eq!(
            read_to_string(&path, "gbk").unwrap(),
            "-- 中文\nSELECT 1;"
        );
        // 错误提示应包含路径与修复 hint
        let err = read_to_string(&path, "utf-8").unwrap_err();
        assert!(err.contains("gbk.sql"), "got: {}", err);
        assert!(err.contains("Hint"), "got: {}", err);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
