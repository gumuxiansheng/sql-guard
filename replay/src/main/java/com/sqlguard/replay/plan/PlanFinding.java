package com.sqlguard.replay.plan;

/**
 * 计划分析启发式规则的命中结果。
 */
public final class PlanFinding {

    public enum Severity {
        warning,
        error
    }

    private final String ruleId;        // 如 "DP001"
    private final Severity severity;
    private final String message;
    private final String nodeName;      // 可为 null
    private final String relationName;  // 可为 null

    public PlanFinding(String ruleId, Severity severity, String message,
                       String nodeName, String relationName) {
        this.ruleId = ruleId;
        this.severity = severity;
        this.message = message;
        this.nodeName = nodeName;
        this.relationName = relationName;
    }

    public String getRuleId() {
        return ruleId;
    }

    public Severity getSeverity() {
        return severity;
    }

    public String getMessage() {
        return message;
    }

    public String getNodeName() {
        return nodeName;
    }

    public String getRelationName() {
        return relationName;
    }
}
