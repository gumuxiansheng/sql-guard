package com.sqlguard.replay.plan;

import com.fasterxml.jackson.databind.JsonNode;
import com.fasterxml.jackson.databind.ObjectMapper;

import java.util.ArrayList;
import java.util.List;

/**
 * 解析 EXPLAIN FORMAT JSON 的输出字符串为 PlanNode 树。
 *
 * <p>openGauss/GaussDB 的 EXPLAIN FORMAT JSON 返回一个 JSON 数组字符串，
 * 第一个元素的 "Plan" 字段是计划树根节点，"Plans" 是子节点数组。
 */
public final class PlanParser {

    private static final ObjectMapper MAPPER = new ObjectMapper();

    private PlanParser() {
    }

    /**
     * 解析计划 JSON 字符串，返回 PlanNode 树（可能为 null）。
     */
    public static PlanNode parse(String planJson) throws Exception {
        if (planJson == null || planJson.trim().isEmpty()) {
            return null;
        }
        JsonNode root = MAPPER.readTree(planJson);
        if (!root.isArray() || root.size() == 0) {
            return null;
        }
        JsonNode first = root.get(0);
        JsonNode plan = first.get("Plan");
        if (plan == null || plan.isNull()) {
            return null;
        }
        return buildNode(plan);
    }

    private static PlanNode buildNode(JsonNode node) {
        String nodeType = textOrNull(node, "Node Type");
        String relationName = textOrNull(node, "Relation Name");
        String indexName = textOrNull(node, "Index Name");
        double startupCost = doubleOr(node, "Startup Cost", 0.0);
        double totalCost = doubleOr(node, "Total Cost", 0.0);
        long planRows = longOr(node, "Plan Rows", 0L);
        long actualRows = longOr(node, "Actual Rows", 0L);
        long actualLoops = longOr(node, "Actual Loops", 0L);
        boolean hasActual = node.has("Actual Rows") && !node.get("Actual Rows").isNull();
        String filter = textOrNull(node, "Filter");
        String indexCond = textOrNull(node, "Index Cond");
        String sortKey = textOrNull(node, "Sort Key");

        List<PlanNode> children = new ArrayList<PlanNode>();
        JsonNode plans = node.get("Plans");
        if (plans != null && plans.isArray()) {
            for (JsonNode child : plans) {
                children.add(buildNode(child));
            }
        }
        return new PlanNode(nodeType, relationName, indexName, startupCost, totalCost,
                planRows, actualRows, actualLoops, hasActual, filter, indexCond, sortKey, children);
    }

    private static String textOrNull(JsonNode node, String field) {
        JsonNode v = node.get(field);
        if (v == null || v.isNull()) {
            return null;
        }
        if (v.isArray()) {
            // Sort Key 可能是数组
            StringBuilder sb = new StringBuilder();
            for (int i = 0; i < v.size(); i++) {
                if (i > 0) {
                    sb.append(", ");
                }
                sb.append(v.get(i).asText());
            }
            return sb.toString();
        }
        return v.asText();
    }

    private static double doubleOr(JsonNode node, String field, double def) {
        JsonNode v = node.get(field);
        if (v == null || v.isNull()) {
            return def;
        }
        return v.asDouble(def);
    }

    private static long longOr(JsonNode node, String field, long def) {
        JsonNode v = node.get(field);
        if (v == null || v.isNull()) {
            return def;
        }
        return v.asLong(def);
    }
}
