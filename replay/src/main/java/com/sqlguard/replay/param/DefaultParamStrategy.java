package com.sqlguard.replay.param;

import com.sqlguard.replay.manifest.ManifestStatement;

/**
 * 自动参数生成策略：当语句无 fixture 时，为占位符生成默认值。
 *
 * <p>避免全绑 NULL 导致 WHERE col = NULL 恒为假、EXPLAIN 计划失真的问题。
 */
public interface DefaultParamStrategy {

    /**
     * 为指定语句的第 index 个占位符生成默认值。
     *
     * @param stmt  当前语句
     * @param index 占位符序号（从 1 开始，与 JDBC 一致）
     * @return 默认值（String 类型，由 JDBC 驱动做类型转换），或 null 表示绑 NULL
     */
    String generate(ManifestStatement stmt, int index);
}
