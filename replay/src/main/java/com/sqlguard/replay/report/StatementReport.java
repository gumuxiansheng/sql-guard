package com.sqlguard.replay.report;

import com.fasterxml.jackson.annotation.JsonInclude;
import com.fasterxml.jackson.annotation.JsonProperty;

import java.util.List;

/**
 * 单条语句的报告项（replay-report.json 中 statements 数组元素）。
 */
@JsonInclude(JsonInclude.Include.NON_NULL)
public class StatementReport {

    @JsonProperty("id")
    private String id;

    @JsonProperty("type")
    private String type;

    @JsonProperty("source")
    private String source;

    @JsonProperty("line")
    private long line;

    @JsonProperty("sql")
    private String sql;

    @JsonProperty("timings")
    private Timings timings;

    @JsonProperty("slowLevel")
    private String slowLevel;

    @JsonProperty("planTopNode")
    private String planTopNode;

    @JsonProperty("planTotalCost")
    private double planTotalCost;

    /**
     * 原始 EXPLAIN JSON 字符串，便于深度调试与离线分析。
     * 无计划时省略（{@link JsonInclude.Include#NON_NULL}）。
     */
    @JsonInclude(JsonInclude.Include.NON_NULL)
    @JsonProperty("planJson")
    private String planJson;

    @JsonProperty("findings")
    private List<Finding> findings;

    @JsonProperty("error")
    private String error;

    @JsonProperty("skipped")
    private boolean skipped;

    @JsonProperty("skipReason")
    private String skipReason;

    public static class Timings {
        @JsonProperty("iterations")
        private int iterations;
        @JsonProperty("minMs")
        private double minMs;
        @JsonProperty("p50Ms")
        private double p50Ms;
        @JsonProperty("p99Ms")
        private double p99Ms;
        @JsonProperty("avgMs")
        private double avgMs;

        public Timings() {
        }

        public Timings(int iterations, double minMs, double p50Ms, double p99Ms, double avgMs) {
            this.iterations = iterations;
            this.minMs = minMs;
            this.p50Ms = p50Ms;
            this.p99Ms = p99Ms;
            this.avgMs = avgMs;
        }

        public int getIterations() { return iterations; }
        public void setIterations(int iterations) { this.iterations = iterations; }
        public double getMinMs() { return minMs; }
        public void setMinMs(double minMs) { this.minMs = minMs; }
        public double getP50Ms() { return p50Ms; }
        public void setP50Ms(double p50Ms) { this.p50Ms = p50Ms; }
        public double getP99Ms() { return p99Ms; }
        public void setP99Ms(double p99Ms) { this.p99Ms = p99Ms; }
        public double getAvgMs() { return avgMs; }
        public void setAvgMs(double avgMs) { this.avgMs = avgMs; }
    }

    public static class Finding {
        @JsonProperty("ruleId")
        private String ruleId;
        @JsonProperty("severity")
        private String severity;
        @JsonProperty("message")
        private String message;
        @JsonProperty("nodeName")
        private String nodeName;
        @JsonProperty("relationName")
        private String relationName;

        public Finding() {
        }

        public Finding(String ruleId, String severity, String message,
                       String nodeName, String relationName) {
            this.ruleId = ruleId;
            this.severity = severity;
            this.message = message;
            this.nodeName = nodeName;
            this.relationName = relationName;
        }

        public String getRuleId() { return ruleId; }
        public void setRuleId(String ruleId) { this.ruleId = ruleId; }
        public String getSeverity() { return severity; }
        public void setSeverity(String severity) { this.severity = severity; }
        public String getMessage() { return message; }
        public void setMessage(String message) { this.message = message; }
        public String getNodeName() { return nodeName; }
        public void setNodeName(String nodeName) { this.nodeName = nodeName; }
        public String getRelationName() { return relationName; }
        public void setRelationName(String relationName) { this.relationName = relationName; }
    }

    // getters / setters

    public String getId() { return id; }
    public void setId(String id) { this.id = id; }
    public String getType() { return type; }
    public void setType(String type) { this.type = type; }
    public String getSource() { return source; }
    public void setSource(String source) { this.source = source; }
    public long getLine() { return line; }
    public void setLine(long line) { this.line = line; }
    public String getSql() { return sql; }
    public void setSql(String sql) { this.sql = sql; }
    public Timings getTimings() { return timings; }
    public void setTimings(Timings timings) { this.timings = timings; }
    public String getSlowLevel() { return slowLevel; }
    public void setSlowLevel(String slowLevel) { this.slowLevel = slowLevel; }
    public String getPlanTopNode() { return planTopNode; }
    public void setPlanTopNode(String planTopNode) { this.planTopNode = planTopNode; }
    public double getPlanTotalCost() { return planTotalCost; }
    public void setPlanTotalCost(double planTotalCost) { this.planTotalCost = planTotalCost; }
    public String getPlanJson() { return planJson; }
    public void setPlanJson(String planJson) { this.planJson = planJson; }
    public List<Finding> getFindings() { return findings; }
    public void setFindings(List<Finding> findings) { this.findings = findings; }
    public String getError() { return error; }
    public void setError(String error) { this.error = error; }
    public boolean isSkipped() { return skipped; }
    public void setSkipped(boolean skipped) { this.skipped = skipped; }
    public String getSkipReason() { return skipReason; }
    public void setSkipReason(String skipReason) { this.skipReason = skipReason; }
}
