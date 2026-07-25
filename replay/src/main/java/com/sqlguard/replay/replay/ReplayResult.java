package com.sqlguard.replay.replay;

import com.sqlguard.replay.plan.PlanFinding;

import java.util.Collections;
import java.util.List;

/**
 * 单条语句的重放结果。
 *
 * <p>{@code timings} 在跳过或出错时可为 null；{@code planJson}/{@code planTopNode} 可为 null。
 */
public final class ReplayResult {

    private final String id;
    private final String type;
    private final String source;
    private final long line;
    private final String sql;

    private final StatementTimings timings;
    private final SlowLevel slowLevel;
    private final String error;                 // 可为 null
    private final String planJson;              // 原始 JSON 字符串，可为 null
    private final String planTopNode;           // 顶层节点类型，可为 null
    private final double planTotalCost;         // 顶层节点 totalCost，无计划为 0
    private final List<PlanFinding> findings;

    private final boolean skipped;
    private final String skipReason;            // 可为 null

    public enum SlowLevel {
        none,
        warn,
        error
    }

    public ReplayResult(String id,
                        String type,
                        String source,
                        long line,
                        String sql,
                        StatementTimings timings,
                        SlowLevel slowLevel,
                        String error,
                        String planJson,
                        String planTopNode,
                        double planTotalCost,
                        List<PlanFinding> findings,
                        boolean skipped,
                        String skipReason) {
        this.id = id;
        this.type = type;
        this.source = source;
        this.line = line;
        this.sql = sql;
        this.timings = timings;
        this.slowLevel = slowLevel == null ? SlowLevel.none : slowLevel;
        this.error = error;
        this.planJson = planJson;
        this.planTopNode = planTopNode;
        this.planTotalCost = planTotalCost;
        this.findings = findings == null
                ? Collections.<PlanFinding>emptyList()
                : Collections.unmodifiableList(findings);
        this.skipped = skipped;
        this.skipReason = skipReason;
    }

    public String getId() {
        return id;
    }

    public String getType() {
        return type;
    }

    public String getSource() {
        return source;
    }

    public long getLine() {
        return line;
    }

    public String getSql() {
        return sql;
    }

    public StatementTimings getTimings() {
        return timings;
    }

    public SlowLevel getSlowLevel() {
        return slowLevel;
    }

    public String getError() {
        return error;
    }

    public String getPlanJson() {
        return planJson;
    }

    public String getPlanTopNode() {
        return planTopNode;
    }

    public double getPlanTotalCost() {
        return planTotalCost;
    }

    public List<PlanFinding> getFindings() {
        return findings;
    }

    public boolean isSkipped() {
        return skipped;
    }

    public String getSkipReason() {
        return skipReason;
    }
}
