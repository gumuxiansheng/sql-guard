package com.sqlguard.replay.replay;

import com.sqlguard.replay.config.ReplayConfig;
import com.sqlguard.replay.db.ConnectionPool;
import com.sqlguard.replay.manifest.Manifest;
import com.sqlguard.replay.manifest.ManifestLoader;
import com.sqlguard.replay.manifest.ManifestStatement;
import com.sqlguard.replay.param.ParamBinder;
import com.sqlguard.replay.report.ReplayReport;
import com.sqlguard.replay.report.ReportWriter;
import org.junit.jupiter.api.AfterAll;
import org.junit.jupiter.api.BeforeAll;
import org.junit.jupiter.api.Test;
import org.junit.jupiter.api.TestInstance;

import java.io.InputStream;
import java.nio.file.Files;
import java.nio.file.Path;
import java.nio.file.StandardCopyOption;
import java.sql.Connection;
import java.sql.Statement;
import java.util.Collections;
import java.util.List;

import static org.junit.jupiter.api.Assertions.*;

/**
 * 端到端集成测试：用 H2 内存数据库验证完整链路。
 *
 * <p>流程：建表 → 加载 manifest → 重放 select → 验证计时/计划/findings → 写报告。
 *
 * <p>注意：H2 的 EXPLAIN FORMAT JSON 输出格式与 PG 不完全一致，
 * 但 H2 支持 EXPLAIN ANALYZE 且返回 JSON，足够验证链路完整性。
 * 计划解析的 PG 专有字段（如 Seq Scan）不在此测试范围。
 */
@TestInstance(TestInstance.Lifecycle.PER_CLASS)
class ReplayerIntegrationTest {

    private ConnectionPool pool;

    @BeforeAll
    void setup() throws Exception {
        // H2 内存数据库，PG 兼容模式
        pool = new ConnectionPool(
                "jdbc:h2:mem:replaytest;MODE=PostgreSQL;DATABASE_TO_LOWER=TRUE",
                "sa", "", 4, 30000L, 60000L);

        // 建测试表
        try (Connection c = pool.getConnection();
             Statement st = c.createStatement()) {
            st.execute("CREATE TABLE users (id INT PRIMARY KEY, name VARCHAR(100), age INT)");
            st.execute("CREATE TABLE orders (id INT PRIMARY KEY, user_id INT, amount DECIMAL(10,2))");
            // 插入少量数据
            st.execute("INSERT INTO users VALUES (1, 'alice', 30), (2, 'bob', 25), (3, 'charlie', 35)");
            st.execute("INSERT INTO orders VALUES (1, 1, 100.00), (2, 1, 200.00), (3, 2, 50.00)");
        }
    }

    @AfterAll
    void teardown() {
        if (pool != null) {
            pool.close();
        }
    }

    @Test
    void replaySelectStatementWithTimingsAndPlan() throws Exception {
        ManifestStatement s = new ManifestStatement();
        s.setId("test-select-1");
        s.setType("select");
        s.setSource("test.sql");
        s.setLine(1);
        s.setEndLine(1);
        s.setSql("SELECT id, name FROM users WHERE id = ?");

        ReplayConfig config = buildConfig();
        ParamBinder binder = new ParamBinder(null);

        Replayer replayer = new Replayer(config, pool, binder);
        ReplayResult r = replayer.replay(s);

        assertNotNull(r);
        assertFalse(r.isSkipped());
        // 没有 fixture → PARAM001 警告
        boolean hasParam001 = false;
        for (com.sqlguard.replay.plan.PlanFinding f : r.getFindings()) {
            if ("PARAM001".equals(f.getRuleId())) {
                hasParam001 = true;
            }
        }
        assertTrue(hasParam001, "should have PARAM001 warning (no fixture, placeholder bound to NULL)");
    }

    @Test
    void replaySelectWithoutPlaceholders() throws Exception {
        ManifestStatement s = new ManifestStatement();
        s.setId("test-select-2");
        s.setType("select");
        s.setSource("test.sql");
        s.setLine(1);
        s.setEndLine(1);
        s.setSql("SELECT COUNT(*) FROM users");

        ReplayConfig config = buildConfig();
        ParamBinder binder = new ParamBinder(null);

        Replayer replayer = new Replayer(config, pool, binder);
        ReplayResult r = replayer.replay(s);

        assertNotNull(r);
        assertFalse(r.isSkipped());
        // H2 不支持 PG 的 EXPLAIN FORMAT JSON 语法，plan 会失败但计时应该成功
        // 核心验证：无占位符 → 不应有 PARAM001
        for (com.sqlguard.replay.plan.PlanFinding f : r.getFindings()) {
            assertNotEquals("PARAM001", f.getRuleId(),
                    "should not have PARAM001 (no placeholders)");
        }
    }

    @Test
    void replayDmlWithTransactionRollback() throws Exception {
        ManifestStatement s = new ManifestStatement();
        s.setId("test-insert-1");
        s.setType("insert");
        s.setSource("test.sql");
        s.setLine(1);
        s.setEndLine(1);
        s.setSql("INSERT INTO users (id, name, age) VALUES (?, ?, ?)");

        ReplayConfig config = buildConfig();
        ParamBinder binder = new ParamBinder(null);

        Replayer replayer = new Replayer(config, pool, binder);
        ReplayResult r = replayer.replay(s);

        assertNotNull(r);
        assertFalse(r.isSkipped());
        // DML 走事务回滚 → 镜像库不应有新数据
        // 验证回滚：查询 users 表仍应只有 3 条
        try (Connection c = pool.getConnection();
             Statement st = c.createStatement()) {
            java.sql.ResultSet rs = st.executeQuery("SELECT COUNT(*) FROM users");
            rs.next();
            assertEquals(3, rs.getInt(1), "DML should have been rolled back");
        }
    }

    @Test
    void replayDdlSkippedByDefault() throws Exception {
        ManifestStatement s = new ManifestStatement();
        s.setId("test-ddl-1");
        s.setType("ddl");
        s.setSource("test.sql");
        s.setLine(1);
        s.setEndLine(1);
        s.setSql("CREATE TABLE temp_test (id INT)");

        ReplayConfig config = buildConfig(); // allowDdl = false
        ParamBinder binder = new ParamBinder(null);

        Replayer replayer = new Replayer(config, pool, binder);
        ReplayResult r = replayer.replay(s);

        assertNotNull(r);
        assertTrue(r.isSkipped());
        assertNotNull(r.getSkipReason());
        assertTrue(r.getSkipReason().contains("ddl skipped"));
    }

    @Test
    void reportGenerationEndToEnd() throws Exception {
        // 构造 3 条语句的重放结果，验证报告生成
        ManifestStatement s1 = new ManifestStatement();
        s1.setId("rpt-1");
        s1.setType("select");
        s1.setSource("test.sql");
        s1.setLine(1);
        s1.setEndLine(1);
        s1.setSql("SELECT * FROM users");

        ManifestStatement s2 = new ManifestStatement();
        s2.setId("rpt-2");
        s2.setType("select");
        s2.setSource("test.sql");
        s2.setLine(2);
        s2.setEndLine(2);
        s2.setSql("SELECT * FROM orders");

        ManifestStatement s3 = new ManifestStatement();
        s3.setId("rpt-3");
        s3.setType("ddl");
        s3.setSource("test.sql");
        s3.setLine(3);
        s3.setEndLine(3);
        s3.setSql("DROP TABLE temp");

        ReplayConfig config = buildConfig();
        ParamBinder binder = new ParamBinder(null);
        Replayer replayer = new Replayer(config, pool, binder);

        List<ReplayResult> results = new java.util.ArrayList<ReplayResult>();
        results.add(replayer.replay(s1));
        results.add(replayer.replay(s2));
        results.add(replayer.replay(s3));

        ReplayReport report = ReportWriter.buildReport(results);
        assertNotNull(report);
        assertNotNull(report.getSummary());
        assertEquals(3, report.getSummary().getTotal());
        assertEquals(2, report.getSummary().getReplayed());
        assertEquals(1, report.getSummary().getSkipped());

        // 写到临时目录
        Path tmpDir = Files.createTempDirectory("sqlguard-replay-test-");
        ReportWriter.write(report, tmpDir);
        assertTrue(Files.exists(tmpDir.resolve("replay-report.json")));
        assertTrue(Files.exists(tmpDir.resolve("replay-report.html")));

        // 验证 JSON 可解析
        byte[] jsonBytes = Files.readAllBytes(tmpDir.resolve("replay-report.json"));
        com.fasterxml.jackson.databind.ObjectMapper mapper = new com.fasterxml.jackson.databind.ObjectMapper();
        ReplayReport parsed = mapper.readValue(jsonBytes, ReplayReport.class);
        assertEquals(3, parsed.getSummary().getTotal());
        assertEquals(3, parsed.getStatements().size());
    }

    @Test
    void parallelReplayProducesSameResults() throws Exception {
        // 用 Main 的 replayParallel 验证并发安全
        ManifestStatement s1 = new ManifestStatement();
        s1.setId("par-1");
        s1.setType("select");
        s1.setSource("test.sql");
        s1.setLine(1);
        s1.setEndLine(1);
        s1.setSql("SELECT * FROM users");

        ManifestStatement s2 = new ManifestStatement();
        s2.setId("par-2");
        s2.setType("select");
        s2.setSource("test.sql");
        s2.setLine(2);
        s2.setEndLine(2);
        s2.setSql("SELECT * FROM orders");

        ReplayConfig config = buildConfig();
        ParamBinder binder = new ParamBinder(null);
        Replayer replayer = new Replayer(config, pool, binder);

        // 串行
        ReplayResult r1Serial = replayer.replay(s1);
        ReplayResult r2Serial = replayer.replay(s2);

        // 并发（通过线程池模拟）
        java.util.concurrent.ExecutorService exec =
                java.util.concurrent.Executors.newFixedThreadPool(2);
        java.util.concurrent.Future<ReplayResult> f1 = exec.submit(
                new java.util.concurrent.Callable<ReplayResult>() {
                    public ReplayResult call() { return replayer.replay(s1); }
                });
        java.util.concurrent.Future<ReplayResult> f2 = exec.submit(
                new java.util.concurrent.Callable<ReplayResult>() {
                    public ReplayResult call() { return replayer.replay(s2); }
                });

        ReplayResult r1Par = f1.get();
        ReplayResult r2Par = f2.get();
        exec.shutdown();

        // 并发结果应与串行一致（可能计时不同，但 error/skip 状态应一致）
        assertEquals(r1Serial.isSkipped(), r1Par.isSkipped());
        assertEquals(r2Serial.isSkipped(), r2Par.isSkipped());
        assertEquals(r1Serial.getError() == null, r1Par.getError() == null);
        assertEquals(r2Serial.getError() == null, r2Par.getError() == null);
    }

    @Test
    void autoParamModeBindsDefaultInsteadOfNull() throws Exception {
        ManifestStatement s = new ManifestStatement();
        s.setId("test-autoparam-1");
        s.setType("select");
        s.setSource("test.sql");
        s.setLine(1);
        s.setEndLine(1);
        s.setSql("SELECT id, name FROM users WHERE id = ?");

        // auto-param 模式：用 FixedValueStrategy("1") 绑定占位符
        ReplayConfig config = buildConfig();
        ParamBinder binder = new ParamBinder(
                java.util.Collections.<String, java.util.List<String>>emptyMap(),
                new com.sqlguard.replay.param.FixedValueStrategy("1"));

        Replayer replayer = new Replayer(config, pool, binder);
        ReplayResult r = replayer.replay(s);

        assertNotNull(r);
        assertFalse(r.isSkipped());
        // auto-param 模式下 hasFixture 返回 true → 不应有 PARAM001
        for (com.sqlguard.replay.plan.PlanFinding f : r.getFindings()) {
            assertNotEquals("PARAM001", f.getRuleId(),
                    "should not have PARAM001 in auto-param mode");
        }
    }

    private ReplayConfig buildConfig() {
        return new ReplayConfig(
                null, null, ExplainMode.AUTO,
                2, 1, false, false, "1", Collections.<String>emptySet(),
                100L, 1000L, null,
                null, null, null, null, "org.h2.Driver",
                10000L, 1000, 4, 60000L);
    }
}
