package com.sqlguard.replay.plan;

import java.util.ArrayList;
import java.util.List;

/**
 * 计划质量启发式分析器：深度优先遍历 PlanNode 树，命中规则则发射 PlanFinding。
 *
 * <p>已实现规则：DP001 / DP002 / DP003 / DP004 / DP007 / DP008，所有规则 severity=warning。
 * DP005 / DP006 暂未实现（预留）。
 */
public final class PlanAnalyzer {

    private PlanAnalyzer() {
    }

    /**
     * 分析计划树，返回所有命中规则的 finding 列表（按 DFS 顺序）。
     *
     * @param root              计划树根节点
     * @param seqScanRowsThreshold DP001/DP003/DP007 的行数阈值（来自 --seq-scan-rows）
     */
    public static List<PlanFinding> analyze(PlanNode root, long seqScanRowsThreshold) {
        List<PlanFinding> findings = new ArrayList<PlanFinding>();
        walk(root, findings, seqScanRowsThreshold);
        return findings;
    }

    private static void walk(PlanNode node, List<PlanFinding> out, long seqScanRowsThreshold) {
        if (node == null) {
            return;
        }
        checkNode(node, out, seqScanRowsThreshold);
        for (PlanNode child : node.getChildren()) {
            walk(child, out, seqScanRowsThreshold);
        }
    }

    private static void checkNode(PlanNode n, List<PlanFinding> out, long seqScanRowsThreshold) {
        String nodeType = n.getNodeType() == null ? "" : n.getNodeType();
        long rows = n.getPlanRows();

        // DP001: 大表全表扫描
        if (nodeType.contains("Seq Scan") && rows > seqScanRowsThreshold) {
            out.add(new PlanFinding("DP001", PlanFinding.Severity.warning,
                    "大表全表扫描 (Seq Scan, 估算行数 " + rows + ")",
                    nodeType, n.getRelationName()));
        }

        // DP002: Nested Loop 外层估算行数过大
        if ("Nested Loop".equals(nodeType) && !n.getChildren().isEmpty()) {
            PlanNode first = n.getChildren().get(0);
            if (first.getPlanRows() > 1000) {
                out.add(new PlanFinding("DP002", PlanFinding.Severity.warning,
                        "Nested Loop 外层估算行数过大 (" + first.getPlanRows() + ")",
                        nodeType, n.getRelationName()));
            }
        }

        // DP003: 大排序节点
        if ("Sort".equals(nodeType) && rows > seqScanRowsThreshold) {
            out.add(new PlanFinding("DP003", PlanFinding.Severity.warning,
                    "大排序节点 (Sort, 估算行数 " + rows + ")",
                    nodeType, n.getRelationName()));
        }

        // DP004: 估算行数与实际行数偏差过大（仅 ANALYZE 才有实际值）
        if (n.isHasActual() && rows > 0) {
            double ratio = Math.abs(n.getActualRows() - rows) / (double) rows;
            if (ratio > 10.0) {
                out.add(new PlanFinding("DP004", PlanFinding.Severity.warning,
                        "估算行数与实际行数偏差过大 (估算 " + rows + ", 实际 "
                                + n.getActualRows() + "), 统计信息可能陈旧",
                        nodeType, n.getRelationName()));
            }
        }

        // DP007: Bitmap Heap Scan 回表量大
        if ("Bitmap Heap Scan".equals(nodeType) && rows > seqScanRowsThreshold) {
            out.add(new PlanFinding("DP007", PlanFinding.Severity.warning,
                    "Bitmap Heap Scan 回表量大 (估算行数 " + rows + ")",
                    nodeType, n.getRelationName()));
        }

        // DP008: 分布式 Streaming 重分布行数过大（GaussDB 分布式）
        if (nodeType.contains("Streaming") && rows > 100000) {
            out.add(new PlanFinding("DP008", PlanFinding.Severity.warning,
                    "分布式 Streaming 重分布行数过大 (" + rows + ")",
                    nodeType, n.getRelationName()));
        }
    }
}
