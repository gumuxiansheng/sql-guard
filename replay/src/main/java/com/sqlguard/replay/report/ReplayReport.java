package com.sqlguard.replay.report;

import com.fasterxml.jackson.annotation.JsonProperty;

import java.util.List;

/**
 * replay-report.json 顶层结构：summary + statements。
 */
public class ReplayReport {

    @JsonProperty("summary")
    private Summary summary;

    @JsonProperty("statements")
    private List<StatementReport> statements;

    public static class Summary {
        @JsonProperty("total")
        private int total;
        @JsonProperty("replayed")
        private int replayed;
        @JsonProperty("skipped")
        private int skipped;
        @JsonProperty("slowWarn")
        private int slowWarn;
        @JsonProperty("slowError")
        private int slowError;
        @JsonProperty("errors")
        private int errors;
        @JsonProperty("planFindings")
        private int planFindings;

        public Summary() {
        }

        public Summary(int total, int replayed, int skipped, int slowWarn,
                       int slowError, int errors, int planFindings) {
            this.total = total;
            this.replayed = replayed;
            this.skipped = skipped;
            this.slowWarn = slowWarn;
            this.slowError = slowError;
            this.errors = errors;
            this.planFindings = planFindings;
        }

        public int getTotal() { return total; }
        public void setTotal(int total) { this.total = total; }
        public int getReplayed() { return replayed; }
        public void setReplayed(int replayed) { this.replayed = replayed; }
        public int getSkipped() { return skipped; }
        public void setSkipped(int skipped) { this.skipped = skipped; }
        public int getSlowWarn() { return slowWarn; }
        public void setSlowWarn(int slowWarn) { this.slowWarn = slowWarn; }
        public int getSlowError() { return slowError; }
        public void setSlowError(int slowError) { this.slowError = slowError; }
        public int getErrors() { return errors; }
        public void setErrors(int errors) { this.errors = errors; }
        public int getPlanFindings() { return planFindings; }
        public void setPlanFindings(int planFindings) { this.planFindings = planFindings; }
    }

    public Summary getSummary() { return summary; }
    public void setSummary(Summary summary) { this.summary = summary; }
    public List<StatementReport> getStatements() { return statements; }
    public void setStatements(List<StatementReport> statements) { this.statements = statements; }
}
