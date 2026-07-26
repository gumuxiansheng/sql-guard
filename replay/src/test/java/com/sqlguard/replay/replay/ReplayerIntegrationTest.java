package com.sqlguard.replay.replay;

import com.sqlguard.replay.config.ReplayConfig;
import com.sqlguard.replay.db.ConnectionPool;
import com.sqlguard.replay.manifest.ManifestStatement;
import com.sqlguard.replay.param.ParamBinder;
import com.sqlguard.replay.plan.PlanFinding;
import com.sqlguard.replay.plan.PlanNode;
import com.sqlguard.replay.report.ReplayReport;
import com.sqlguard.replay.report.ReportWriter;
import org.junit.jupiter.api.AfterAll;
import org.junit.jupiter.api.BeforeAll;
import org.junit.jupiter.api.Test;
import org.junit.jupiter.api.TestInstance;

import java.nio.file.Files;
import java.nio.file.Path;
import java.sql.Connection;
import java.sql.Statement;
import java.util.Collections;
import java.util.List;

import static org.junit.jupiter.api.Assertions.*;

/**
 * 端到端集成测试：用 H2 内存数据库验证完整链路。
 *
 * <p>PG 模式用 H2 PostgreSQL 兼容模式，MySQL 模式用 H2 MySQL 兼容模式。
 * 注意：H2 的 EXPLAIN 语法与真实 PG/MySQL 不完全一致，plan 采集可能失败，
 * 核心验证的是计时链路、事务回滚、报告生成、并发安全、auto-param 等不依赖 EXPLAIN 的能力。
 */
@TestInstance(TestInstance.Lifecycle.PER_CLASS)
class ReplayerIntegrationTest {

    private ConnectionPool pgPool;

    @BeforeAll
    void setup() throws Exception {
        // H2 PostgreSQL 兼容模式
        pgPool = new ConnectionPool(
                "jdbc:h2:mem:pgtest;MODE=PostgreSQL;DATABASE_TO_LOWER=TRUE",
                "sa", "", 4, 30000L, 60000L, DbDialect.POSTGRESQL);

        try (Connection c = pgPool.getConnection();
             Statement st = c.createStatement()) {
            st.execute("CREATE TABLE users (id INT PRIMARY KEY, name VARCHAR(100), age INT)");
            st.execute("CREATE TABLE orders (id INT PRIMARY KEY, user_id INT, amount DECIMAL(10,2))");
            st.execute("INSERT INTO users VALUES (1, 'alice', 30), (2, 'bob', 25), (3, 'charlie', 35)");
            st.execute("INSERT INTO orders VALUES (1, 1, 100.00), (2, 1, 200.00), (3, 2, 50.00)");
        }
    }

    @AfterAll
    void teardown() {
        if (pgPool != null) {
            pgPool.close();
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

        ReplayConfig config = buildPgConfig();
        ParamBinder binder = new ParamBinder(null);

        Replayer replayer = new Replayer(config, pgPool, binder);
        ReplayResult r = replayer.replay(s);

        assertNotNull(r);
        assertFalse(r.isSkipped());
        boolean hasParam001 = false;
        for (PlanFinding f : r.getFindings()) {
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

        ReplayConfig config = buildPgConfig();
        ParamBinder binder = new ParamBinder(null);

        Replayer replayer = new Replayer(config, pgPool, binder);
        ReplayResult r = replayer.replay(s);

        assertNotNull(r);
        assertFalse(r.isSkipped());
        for (PlanFinding f : r.getFindings()) {
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

        ReplayConfig config = buildPgConfig();
        ParamBinder binder = new ParamBinder(null);

        Replayer replayer = new Replayer(config, pgPool, binder);
        ReplayResult r = replayer.replay(s);

        assertNotNull(r);
        assertFalse(r.isSkipped());
        try (Connection c = pgPool.getConnection();
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

        ReplayConfig config = buildPgConfig();
        ParamBinder binder = new ParamBinder(null);

        Replayer replayer = new Replayer(config, pgPool, binder);
        ReplayResult r = replayer.replay(s);

        assertNotNull(r);
        assertTrue(r.isSkipped());
        assertNotNull(r.getSkipReason());
        assertTrue(r.getSkipReason().contains("ddl skipped"));
    }

    @Test
    void reportGenerationEndToEnd() throws Exception {
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

        ReplayConfig config = buildPgConfig();
        ParamBinder binder = new ParamBinder(null);
        Replayer replayer = new Replayer(config, pgPool, binder);

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

        Path tmpDir = Files.createTempDirectory("sqlguard-replay-test-");
        ReportWriter.write(report, tmpDir);
        assertTrue(Files.exists(tmpDir.resolve("replay-report.json")));
        assertTrue(Files.exists(tmpDir.resolve("replay-report.html")));

        byte[] jsonBytes = Files.readAllBytes(tmpDir.resolve("replay-report.json"));
        com.fasterxml.jackson.databind.ObjectMapper mapper = new com.fasterxml.jackson.databind.ObjectMapper();
        ReplayReport parsed = mapper.readValue(jsonBytes, ReplayReport.class);
        assertEquals(3, parsed.getSummary().getTotal());
        assertEquals(3, parsed.getStatements().size());
    }

    @Test
    void parallelReplayProducesSameResults() throws Exception {
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

        ReplayConfig config = buildPgConfig();
        ParamBinder binder = new ParamBinder(null);
        Replayer replayer = new Replayer(config, pgPool, binder);

        ReplayResult r1Serial = replayer.replay(s1);
        ReplayResult r2Serial = replayer.replay(s2);

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

        ReplayConfig config = buildPgConfig();
        ParamBinder binder = new ParamBinder(
                java.util.Collections.<String, java.util.List<String>>emptyMap(),
                new com.sqlguard.replay.param.FixedValueStrategy("1"));

        Replayer replayer = new Replayer(config, pgPool, binder);
        ReplayResult r = replayer.replay(s);

        assertNotNull(r);
        assertFalse(r.isSkipped());
        for (PlanFinding f : r.getFindings()) {
            assertNotEquals("PARAM001", f.getRuleId(),
                    "should not have PARAM001 in auto-param mode");
        }
    }

    // ============================ MySQL 方言测试 ============================

    @Test
    void mySqlDialectFromJdbcUrl() {
        assertEquals(DbDialect.MYSQL, DbDialect.fromJdbcUrl("jdbc:mysql://localhost:3306/test"));
        assertEquals(DbDialect.MYSQL, DbDialect.fromJdbcUrl("jdbc:mariadb://localhost:3306/test"));
        assertEquals(DbDialect.POSTGRESQL, DbDialect.fromJdbcUrl("jdbc:postgresql://localhost:5432/test"));
        assertEquals(DbDialect.POSTGRESQL, DbDialect.fromJdbcUrl("jdbc:opengauss://localhost:5432/test"));
    }

    @Test
    void mySqlDialectParse() {
        assertEquals(DbDialect.MYSQL, DbDialect.parse("mysql"));
        assertEquals(DbDialect.MYSQL, DbDialect.parse("MySQL"));
        assertEquals(DbDialect.MYSQL, DbDialect.parse("mariadb"));
        assertEquals(DbDialect.POSTGRESQL, DbDialect.parse("pg"));
        assertEquals(DbDialect.POSTGRESQL, DbDialect.parse("postgresql"));
        assertEquals(DbDialect.POSTGRESQL, DbDialect.parse(null));
        assertEquals(DbDialect.POSTGRESQL, DbDialect.parse(""));
    }

    @Test
    void mySqlReplaySelectWithH2() throws Exception {
        // H2 MySQL 兼容模式
        try (ConnectionPool mysqlPool = new ConnectionPool(
                "jdbc:h2:mem:mysqltest;MODE=MySQL;DATABASE_TO_LOWER=TRUE",
                "sa", "", 2, 30000L, 0L, DbDialect.MYSQL)) {

            try (Connection c = mysqlPool.getConnection();
                 Statement st = c.createStatement()) {
                st.execute("CREATE TABLE products (id INT PRIMARY KEY, name VARCHAR(100), price DECIMAL(10,2))");
                st.execute("INSERT INTO products VALUES (1, 'widget', 9.99), (2, 'gadget', 19.99), (3, 'gizmo', 4.99)");
            }

            ManifestStatement s = new ManifestStatement();
            s.setId("mysql-select-1");
            s.setType("select");
            s.setSource("test.sql");
            s.setLine(1);
            s.setEndLine(1);
            s.setSql("SELECT id, name FROM products WHERE id = ?");

            ReplayConfig config = buildMySqlConfig();
            ParamBinder binder = new ParamBinder(
                    java.util.Collections.<String, java.util.List<String>>emptyMap(),
                    new com.sqlguard.replay.param.FixedValueStrategy("1"));

            Replayer replayer = new Replayer(config, mysqlPool, binder);
            ReplayResult r = replayer.replay(s);

            assertNotNull(r);
            assertFalse(r.isSkipped());
            // auto-param 模式下不应有 PARAM001
            for (PlanFinding f : r.getFindings()) {
                assertNotEquals("PARAM001", f.getRuleId());
            }
        }
    }

    @Test
    void mySqlReplayDmlRollback() throws Exception {
        try (ConnectionPool mysqlPool = new ConnectionPool(
                "jdbc:h2:mem:mysqltest2;MODE=MySQL;DATABASE_TO_LOWER=TRUE",
                "sa", "", 2, 30000L, 0L, DbDialect.MYSQL)) {

            try (Connection c = mysqlPool.getConnection();
                 Statement st = c.createStatement()) {
                st.execute("CREATE TABLE items (id INT PRIMARY KEY, name VARCHAR(50))");
                st.execute("INSERT INTO items VALUES (1, 'existing')");
            }

            ManifestStatement s = new ManifestStatement();
            s.setId("mysql-insert-1");
            s.setType("insert");
            s.setSource("test.sql");
            s.setLine(1);
            s.setEndLine(1);
            s.setSql("INSERT INTO items (id, name) VALUES (?, ?)");

            ReplayConfig config = buildMySqlConfig();
            ParamBinder binder = new ParamBinder(null);

            Replayer replayer = new Replayer(config, mysqlPool, binder);
            ReplayResult r = replayer.replay(s);

            assertNotNull(r);
            assertFalse(r.isSkipped());
            // DML 应回滚
            try (Connection c = mysqlPool.getConnection();
                 Statement st = c.createStatement()) {
                java.sql.ResultSet rs = st.executeQuery("SELECT COUNT(*) FROM items");
                rs.next();
                assertEquals(1, rs.getInt(1), "DML should have been rolled back");
            }
        }
    }

    // ----------------------- 配置工厂 -----------------------

    private ReplayConfig buildPgConfig() {
        return new ReplayConfig(
                null, null, ExplainMode.AUTO,
                2, 1, false, false, "1", Collections.<String>emptySet(),
                100L, 1000L, null,
                null, null, null, null, "org.h2.Driver",
                10000L, 1000, 4, 60000L, DbDialect.POSTGRESQL);
    }

    private ReplayConfig buildMySqlConfig() {
        return new ReplayConfig(
                null, null, ExplainMode.AUTO,
                2, 1, false, false, "1", Collections.<String>emptySet(),
                100L, 1000L, null,
                null, null, null, null, "org.h2.Driver",
                10000L, 1000, 2, 60000L, DbDialect.MYSQL);
    }
}
