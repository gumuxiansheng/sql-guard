package com.sqlguard.replay.plan;

import java.util.Collections;
import java.util.List;

/**
 * 解析后的执行计划节点（树形结构，对应 EXPLAIN FORMAT JSON 的 "Plan" 对象）。
 *
 * <p>成本/行数在 JSON 中可能是 double，统一以 double/long 存储。
 * {@code hasActual} 表示是否存在 "Actual Rows" 字段（即是否执行过 ANALYZE）。
 */
public final class PlanNode {

    private final String nodeType;
    private final String relationName;
    private final String indexName;
    private final double startupCost;
    private final double totalCost;
    private final long planRows;
    private final long actualRows;
    private final long actualLoops;
    private final boolean hasActual;
    private final String filter;
    private final String indexCond;
    private final String sortKey;
    private final List<PlanNode> children;

    public PlanNode(String nodeType,
                    String relationName,
                    String indexName,
                    double startupCost,
                    double totalCost,
                    long planRows,
                    long actualRows,
                    long actualLoops,
                    boolean hasActual,
                    String filter,
                    String indexCond,
                    String sortKey,
                    List<PlanNode> children) {
        this.nodeType = nodeType;
        this.relationName = relationName;
        this.indexName = indexName;
        this.startupCost = startupCost;
        this.totalCost = totalCost;
        this.planRows = planRows;
        this.actualRows = actualRows;
        this.actualLoops = actualLoops;
        this.hasActual = hasActual;
        this.filter = filter;
        this.indexCond = indexCond;
        this.sortKey = sortKey;
        this.children = children == null
                ? Collections.<PlanNode>emptyList()
                : Collections.unmodifiableList(children);
    }

    public String getNodeType() {
        return nodeType;
    }

    public String getRelationName() {
        return relationName;
    }

    public String getIndexName() {
        return indexName;
    }

    public double getStartupCost() {
        return startupCost;
    }

    public double getTotalCost() {
        return totalCost;
    }

    public long getPlanRows() {
        return planRows;
    }

    public long getActualRows() {
        return actualRows;
    }

    public long getActualLoops() {
        return actualLoops;
    }

    public boolean isHasActual() {
        return hasActual;
    }

    public String getFilter() {
        return filter;
    }

    public String getIndexCond() {
        return indexCond;
    }

    public String getSortKey() {
        return sortKey;
    }

    public List<PlanNode> getChildren() {
        return children;
    }
}
