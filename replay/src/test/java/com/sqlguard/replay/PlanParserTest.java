package com.sqlguard.replay;

import com.sqlguard.replay.plan.PlanAnalyzer;
import com.sqlguard.replay.plan.PlanFinding;
import com.sqlguard.replay.plan.PlanNode;
import com.sqlguard.replay.plan.PlanParser;
import org.junit.jupiter.api.Test;

import java.util.List;

import static org.junit.jupiter.api.Assertions.assertEquals;
import static org.junit.jupiter.api.Assertions.assertNotNull;
import static org.junit.jupiter.api.Assertions.assertTrue;

/**
 * 不依赖数据库的冒烟测试：解析硬编码的 EXPLAIN FORMAT JSON，
 * 并验证 PlanAnalyzer 在大表 Seq Scan 上命中 DP001。
 */
class PlanParserTest {

    /**
     * 一个 Seq Scan 节点，估算行数 50000，超过默认阈值 10000，应命中 DP001。
     */
    private static final String EXPLAIN_JSON = "[\n" +
            "  {\n" +
            "    \"Plan\": {\n" +
            "      \"Node Type\": \"Seq Scan\",\n" +
            "      \"Relation Name\": \"orders\",\n" +
            "      \"Alias\": \"orders\",\n" +
            "      \"Startup Cost\": 0.00,\n" +
            "      \"Total Cost\": 1542.50,\n" +
            "      \"Plan Rows\": 50000,\n" +
            "      \"Plan Width\": 8,\n" +
            "      \"Actual Startup Time\": 0.012,\n" +
            "      \"Actual Total Time\": 12.345,\n" +
            "      \"Actual Rows\": 50000,\n" +
            "      \"Actual Loops\": 1\n" +
            "    },\n" +
            "    \"Planning Time\": 0.05,\n" +
            "    \"Execution Time\": 12.40,\n" +
            "    \"Triggers\": []\n" +
            "  }\n" +
            "]";

    @Test
    void parsesSeqScanRootNode() throws Exception {
        PlanNode root = PlanParser.parse(EXPLAIN_JSON);
        assertNotNull(root, "root plan node should be parsed");
        assertEquals("Seq Scan", root.getNodeType());
        assertEquals("orders", root.getRelationName());
        assertEquals(50000L, root.getPlanRows());
        assertEquals(1542.50, root.getTotalCost(), 0.001);
        assertTrue(root.isHasActual(), "ANALYZE plan should have actual rows");
        assertEquals(50000L, root.getActualRows());
        assertEquals(1L, root.getActualLoops());
    }

    @Test
    void analyzerEmitsDp001ForLargeSeqScan() throws Exception {
        PlanNode root = PlanParser.parse(EXPLAIN_JSON);
        assertNotNull(root);
        List<PlanFinding> findings = PlanAnalyzer.analyze(root, 10000L);
        boolean hasDp001 = false;
        for (PlanFinding f : findings) {
            if ("DP001".equals(f.getRuleId())) {
                hasDp001 = true;
                assertEquals(PlanFinding.Severity.warning, f.getSeverity());
                assertEquals("Seq Scan", f.getNodeName());
                assertEquals("orders", f.getRelationName());
                assertTrue(f.getMessage().contains("50000"), "message should contain row count");
            }
        }
        assertTrue(hasDp001, "DP001 should be emitted for Seq Scan with 50000 rows");
    }
}
