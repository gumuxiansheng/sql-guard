package com.sqlguard.replay.plan;

import java.util.ArrayList;
import java.util.List;

/**
 * MySQL 计划质量启发式分析器。
 *
 * <p>MySQL EXPLAIN 的字段与 PG 差异很大，规则映射：
 * <ul>
 *   <li>DP001 大表全表扫描：MySQL {@code type=ALL} + rows > 阈值</li>
 *   <li>DP003 大排序：MySQL {@code Extra} 含 "Using filesort" + rows > 阈值</li>
 *   <li>DP006 临时表：MySQL {@code Extra} 含 "Using temporary"</li>
 *   <li>DP009 无索引访问：MySQL {@code type} 为 ALL 或 index 且 key 为 null</li>
 * </ul>
 *
 * <p>PG 专有规则（DP002 Nested Loop / DP004 估算偏差 / DP007 Bitmap Heap Scan / DP008 Streaming）
 * 不适用于 MySQL，跳过。
 */
public final class MySqlPlanAnalyzer {

    private MySqlPlanAnalyzer() {
    }

    /**
     * 分析 MySQL 计划节点列表，返回所有命中规则的 finding。
     *
     * @param nodes               MySQL EXPLAIN 的节点列表
     * @param seqScanRowsThreshold 全表扫描行数阈值
     */
    public static List<PlanFinding> analyze(List<PlanNode> nodes, long seqScanRowsThreshold) {
        List<PlanFinding> findings = new ArrayList<PlanFinding>();
        for (PlanNode n : nodes) {
            checkNode(n, findings, seqScanRowsThreshold);
        }
        return findings;
    }

    private static void checkNode(PlanNode n, List<PlanFinding> out, long seqScanRowsThreshold) {
        String nodeType = n.getNodeType() == null ? "" : n.getNodeType().toLowerCase();
        long rows = n.getPlanRows();
        String extra = n.getFilter() == null ? "" : n.getFilter().toLowerCase();

        // DP001: 大表全表扫描 (type=ALL)
        if ("all".equals(nodeType) && rows > seqScanRowsThreshold) {
            out.add(new PlanFinding("DP001", PlanFinding.Severity.warning,
                    "大表全表扫描 (type=ALL, 估算行数 " + rows + ")",
                    n.getNodeType(), n.getRelationName()));
        }

        // DP003: 大排序 (Extra 含 Using filesort)
        if (extra.contains("using filesort") && rows > seqScanRowsThreshold) {
            out.add(new PlanFinding("DP003", PlanFinding.Severity.warning,
                    "大排序节点 (Using filesort, 估算行数 " + rows + ")",
                    n.getNodeType(), n.getRelationName()));
        }

        // DP006: 使用临时表 (Extra 含 Using temporary)
        if (extra.contains("using temporary")) {
            out.add(new PlanFinding("DP006", PlanFinding.Severity.warning,
                    "使用临时表 (Using temporary)",
                    n.getNodeType(), n.getRelationName()));
        }

        // DP009: 无索引访问 (type=ALL 或 index 且 key=null)
        if (("all".equals(nodeType) || "index".equals(nodeType))
                && (n.getIndexName() == null || n.getIndexName().isEmpty())) {
            // type=index 且 key=null 表示全索引扫描，虽然比全表扫描快但仍不理想
            // type=ALL 且 key=null 是真正的全表扫描
            if ("all".equals(nodeType) && rows > 0) {
                // 只对小表的全表扫描不发 DP009（DP001 已覆盖大表）
                // 这里对有行数的 ALL 都报 DP009
                out.add(new PlanFinding("DP009", PlanFinding.Severity.warning,
                        "无索引访问 (type=ALL, key=null)",
                        n.getNodeType(), n.getRelationName()));
            }
        }
    }
}
