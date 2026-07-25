package com.sqlguard.replay.manifest;

import com.fasterxml.jackson.annotation.JsonIgnoreProperties;
import com.fasterxml.jackson.annotation.JsonInclude;
import com.fasterxml.jackson.annotation.JsonProperty;

/**
 * 清单中的单条 SQL 语句。
 *
 * <p>{@code statementId} 与 {@code parseError} 为可选字段，null 时序列化省略。
 */
@JsonIgnoreProperties(ignoreUnknown = true)
public class ManifestStatement {

    @JsonProperty("id")
    private String id;

    @JsonProperty("sql")
    private String sql;

    @JsonProperty("type")
    private String type;

    @JsonProperty("source")
    private String source;

    @JsonProperty("source_type")
    private String sourceType;

    @JsonProperty("line")
    private long line;

    @JsonProperty("end_line")
    private long endLine;

    @JsonInclude(JsonInclude.Include.NON_NULL)
    @JsonProperty("statement_id")
    private String statementId;

    @JsonInclude(JsonInclude.Include.NON_NULL)
    @JsonProperty("parse_error")
    private String parseError;

    /** 动态分支变体：指向原 mapper statement 的 id（如 mapper/User.xml#selectById）。
     *  仅 mapper 动态分支展开的变体有此字段；SQL 脚本与无动态分支的 mapper 为 null。 */
    @JsonInclude(JsonInclude.Include.NON_NULL)
    @JsonProperty("variant_of")
    private String variantOf;

    /** 动态分支组合描述（如 if:name!=null=true,foreach:1elem）。
     *  仅 mapper 动态分支展开的变体有此字段。 */
    @JsonInclude(JsonInclude.Include.NON_NULL)
    @JsonProperty("variant_label")
    private String variantLabel;

    public String getId() {
        return id;
    }

    public String getSql() {
        return sql;
    }

    public String getType() {
        return type;
    }

    public String getSource() {
        return source;
    }

    public String getSourceType() {
        return sourceType;
    }

    public long getLine() {
        return line;
    }

    public long getEndLine() {
        return endLine;
    }

    public String getStatementId() {
        return statementId;
    }

    public String getParseError() {
        return parseError;
    }

    public String getVariantOf() {
        return variantOf;
    }

    public String getVariantLabel() {
        return variantLabel;
    }

    public void setId(String id) {
        this.id = id;
    }

    public void setSql(String sql) {
        this.sql = sql;
    }

    public void setType(String type) {
        this.type = type;
    }

    public void setSource(String source) {
        this.source = source;
    }

    public void setSourceType(String sourceType) {
        this.sourceType = sourceType;
    }

    public void setLine(long line) {
        this.line = line;
    }

    public void setEndLine(long endLine) {
        this.endLine = endLine;
    }

    public void setStatementId(String statementId) {
        this.statementId = statementId;
    }

    public void setParseError(String parseError) {
        this.parseError = parseError;
    }

    public void setVariantOf(String variantOf) {
        this.variantOf = variantOf;
    }

    public void setVariantLabel(String variantLabel) {
        this.variantLabel = variantLabel;
    }
}
