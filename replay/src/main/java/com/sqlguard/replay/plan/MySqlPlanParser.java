package com.sqlguard.replay.plan;

import com.fasterxml.jackson.databind.JsonNode;
import com.fasterxml.jackson.databind.ObjectMapper;

import java.util.ArrayList;
import java.util.List;

/**
 * MySQL EXPLAIN FORMAT=JSON 输出解析器。
 *
 * <p>MySQL 的 EXPLAIN FORMAT=JSON 返回一个 JSON 数组（通过 ResultSet.getString(1) 获取），
 * 结构形如：
 * <pre>
 * [
 *   {
 *     "id": 1,
 *     "select_type": "SIMPLE",
 *     "table": "users",
 *     "type": "ALL",          // 访问类型：ALL=全表扫描, index, range, ref, eq_ref, const...
 *     "possible_keys": null,
 *     "key": null,             // 实际使用的索引
 *     "key_len": null,
 *     "ref": null,
 *     "rows": 3,              // 估算扫描行数
 *     "Extra": null            // 额外信息
 *   }
 * ]
 * </pre>
 *
 * <p>复杂查询（含子查询/JOIN）会有多行，每行一个节点。
 * 解析为扁平的 PlanNode 列表（无父子关系），用 children 空列表表示。
 * 第一个节点作为根节点。
 *
 * <p>字段映射：
 * <ul>
 *   <li>nodeType ← type（ALL/index/range/ref/eq_ref/const...）</li>
 *   <li>relationName ← table</li>
 *   <li>indexName ← key</li>
 *   <li>planRows ← rows</li>
 *   <li>filter ← Extra</li>
 *   <li>其他字段（cost/actualRows/loops）MySQL 不提供，设为 0/null/false</li>
 * </ul>
 */
public final class MySqlPlanParser {

    private static final ObjectMapper MAPPER = new ObjectMapper();

    private MySqlPlanParser() {
    }

    /**
     * 解析 MySQL EXPLAIN FORMAT=JSON 输出，返回 PlanNode 列表。
     *
     * <p>与 PlanParser.parse 不同，返回列表而非单根节点树。
     * Replayer 中的调用方需适配：对 MySQL 取列表第一个作为 topNode，
     * 并对每个节点跑 PlanAnalyzer。
     */
    public static List<PlanNode> parseList(String planJson) throws Exception {
        if (planJson == null || planJson.trim().isEmpty()) {
            return new ArrayList<PlanNode>();
        }
        JsonNode root = MAPPER.readTree(planJson);
        if (!root.isArray()) {
            return new ArrayList<PlanNode>();
        }
        List<PlanNode> nodes = new ArrayList<PlanNode>(root.size());
        for (JsonNode item : root) {
            nodes.add(buildNode(item));
        }
        return nodes;
    }

    /**
     * 取列表第一个节点作为根节点（兼容 PlanParser.parse 的返回类型）。
     * 如果列表为空返回 null。
     */
    public static PlanNode parse(String planJson) throws Exception {
        List<PlanNode> nodes = parseList(planJson);
        return nodes.isEmpty() ? null : nodes.get(0);
    }

    private static PlanNode buildNode(JsonNode node) {
        String nodeType = textOrNull(node, "type");        // ALL, index, range, ref...
        String relationName = textOrNull(node, "table");
        String indexName = textOrNull(node, "key");
        long planRows = longOr(node, "rows", 0L);
        String filter = textOrNull(node, "Extra");

        return new PlanNode(
                nodeType,
                relationName,
                indexName,
                0.0,        // startupCost — MySQL 不提供
                0.0,        // totalCost — MySQL 不提供
                planRows,
                0L,         // actualRows
                0L,         // actualLoops
                false,      // hasActual
                filter,
                null,       // indexCond
                null,       // sortKey
                java.util.Collections.<PlanNode>emptyList()
        );
    }

    private static String textOrNull(JsonNode node, String field) {
        JsonNode v = node.get(field);
        if (v == null || v.isNull()) {
            return null;
        }
        return v.asText();
    }

    private static long longOr(JsonNode node, String field, long def) {
        JsonNode v = node.get(field);
        if (v == null || v.isNull()) {
            return def;
        }
        return v.asLong(def);
    }
}
