package com.sqlguard.replay.replay;

/**
 * 执行计划采集模式。
 *
 * <p>{@code AUTO} 让 Replayer 按语句类型自动决定：select/merge 走 ANALYZE，
 * DML 走纯 EXPLAIN（避免实际写库）。
 */
public enum ExplainMode {
    EXPLAIN,
    ANALYZE,
    AUTO
}
