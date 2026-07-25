package com.sqlguard.replay.manifest;

import com.fasterxml.jackson.annotation.JsonIgnoreProperties;
import com.fasterxml.jackson.annotation.JsonProperty;

import java.util.List;

/**
 * sql-manifest.json 顶层结构。
 */
@JsonIgnoreProperties(ignoreUnknown = true)
public class Manifest {

    @JsonProperty("version")
    private int version;

    @JsonProperty("generator")
    private String generator;

    @JsonProperty("generated_at")
    private String generatedAt;

    @JsonProperty("statement_count")
    private int statementCount;

    @JsonProperty("statements")
    private List<ManifestStatement> statements;

    public int getVersion() {
        return version;
    }

    public String getGenerator() {
        return generator;
    }

    public String getGeneratedAt() {
        return generatedAt;
    }

    public int getStatementCount() {
        return statementCount;
    }

    public List<ManifestStatement> getStatements() {
        return statements;
    }

    public void setVersion(int version) {
        this.version = version;
    }

    public void setGenerator(String generator) {
        this.generator = generator;
    }

    public void setGeneratedAt(String generatedAt) {
        this.generatedAt = generatedAt;
    }

    public void setStatementCount(int statementCount) {
        this.statementCount = statementCount;
    }

    public void setStatements(List<ManifestStatement> statements) {
        this.statements = statements;
    }
}
