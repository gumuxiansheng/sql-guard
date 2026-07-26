package com.sqlguard.replay.replay;

import com.sqlguard.replay.config.ReplayConfig;
import com.sqlguard.replay.db.ConnectionPool;
import com.sqlguard.replay.manifest.ManifestStatement;
import com.sqlguard.replay.param.ParamBinder;
import com.sqlguard.replay.plan.ExplainAdapter;
import com.sqlguard.replay.plan.MySqlExplainAdapter;
import com.sqlguard.replay.plan.MySqlPlanAnalyzer;
import com.sqlguard.replay.plan.MySqlPlanParser;
import com.sqlguard.replay.plan.PgExplainAdapter;
import com.sqlguard.replay.plan.PlanAnalyzer;
import com.sqlguard.replay.plan.PlanFinding;
import com.sqlguard.replay.plan.PlanNode;
import com.sqlguard.replay.plan.PlanParser;
import com.sqlguard.replay.slow.SlowDetector;

import java.sql.Connection;
import java.sql.PreparedStatement;
import java.sql.ResultSet;
import java.sql.SQLException;
import java.util.ArrayList;
import java.util.List;

/**
 * 核心重放器：按 SAFETY MODEL 对每条语句执行计时、采集计划、识别问题。
 *
 * <p>线程安全：单实例对应一个 ConnectionPool，自身无状态。
 *
 * <p>按 {@link DbDialect} 分发 ExplainAdapter 和 PlanParser/Analyzer：
 * <ul>
 *   <li>PostgreSQL/openGauss：PgExplainAdapter + PlanParser + PlanAnalyzer</li>
 *   <li>MySQL/MariaDB：MySqlExplainAdapter + MySqlPlanParser + MySqlPlanAnalyzer</li>
 * </ul>
 */
public final class Replayer {

    private final ReplayConfig config;
    private final ConnectionPool pool;
    private final ParamBinder binder;
    private final DbDialect dialect;
    private final ExplainAdapter explainAdapter;

    public Replayer(ReplayConfig config, ConnectionPool pool, ParamBinder binder) {
        this.config = config;
        this.pool = pool;
        this.binder = binder;
        this.dialect = config.getDialect();
        this.explainAdapter = (dialect == DbDialect.MYSQL)
                ? new MySqlExplainAdapter()
                : new PgExplainAdapter();
    }

    /**
     * 对单条语句执行重放。
     *
     * <p>任何 SQLException 都被吞掉并记录到结果中，绝不向上抛。
     */
    public ReplayResult replay(ManifestStatement s) {
        String type = s.getType() == null ? "other" : s.getType().toLowerCase();

        // 1. 解析错误 / other 类型：跳过
        if (s.getParseError() != null) {
            return skipped(s, "parse_error: " + s.getParseError());
        }
        if ("other".equals(type)) {
            return skipped(s, "type=other, not replayable");
        }

        // 2. DDL：默认跳过；若允许则只采集 EXPLAIN 计划，不计时执行
        if ("ddl".equals(type)) {
            if (!config.isAllowDdl()) {
                return skipped(s, "ddl skipped (require --allow-ddl)");
            }
            return replayDdlPlanOnly(s);
        }

        // 3. select：只读，无需事务
        if ("select".equals(type)) {
            return replayReadonly(s, type);
        }

        // 4. insert / update / delete / merge：DML，事务包裹后回滚
        if ("insert".equals(type) || "update".equals(type)
                || "delete".equals(type) || "merge".equals(type)) {
            return replayDml(s, type);
        }

        // 兜底
        return skipped(s, "unsupported type: " + type);
    }

    // ----------------------- 分支实现 -----------------------

    private ReplayResult replayDdlPlanOnly(ManifestStatement s) {
        // 检测 CTAS（CREATE TABLE ... AS SELECT / CREATE VIEW ... AS SELECT）
        // 用 token 扫描器避免子串匹配误判注释/字符串字面量中的 "AS SELECT"
        if (!isCreateAsSelect(s.getSql())) {
            return skipped(s, "ddl: EXPLAIN not supported for non-CTAS DDL");
        }
        try (Connection c = pool.getConnection()) {
            String planJson = explainAdapter.explainOnly(c, s.getSql(), binder, s);
            return buildResultWithPlan(s, null, null, planJson, false, null);
        } catch (SQLException e) {
            return buildResultWithPlan(s, null, describe("plan", e), null, false, null);
        }
    }

    /**
     * 判断 DDL 是否为 CREATE TABLE/VIEW ... AS SELECT 形态（CTAS）。
     *
     * <p>用 token 扫描器跳过字符串字面量、行注释、块注释，再匹配 {@code AS} 后紧接
     * {@code SELECT} 关键字（大小写不敏感）。避免原 {@code sql.contains(" AS SELECT")}
     * 误匹配 {@code -- AS SELECT} 注释或 {@code 'AS SELECT'} 字符串字面量的问题。
     */
    static boolean isCreateAsSelect(String sql) {
        if (sql == null || sql.isEmpty()) {
            return false;
        }
        int len = sql.length();
        int i = 0;
        while (i < len) {
            char ch = sql.charAt(i);

            // 跳过单引号字符串
            if (ch == '\'') {
                i++;
                while (i < len) {
                    if (sql.charAt(i) == '\'') {
                        if (i + 1 < len && sql.charAt(i + 1) == '\'') {
                            i += 2;
                        } else {
                            i++;
                            break;
                        }
                    } else {
                        i++;
                    }
                }
                continue;
            }

            // 跳过行注释 -- ... \n
            if (ch == '-' && i + 1 < len && sql.charAt(i + 1) == '-') {
                i += 2;
                while (i < len && sql.charAt(i) != '\n') {
                    i++;
                }
                continue;
            }

            // 跳过块注释 /* ... */（支持嵌套）
            if (ch == '/' && i + 1 < len && sql.charAt(i + 1) == '*') {
                i += 2;
                int depth = 1;
                while (i < len && depth > 0) {
                    if (sql.charAt(i) == '/' && i + 1 < len && sql.charAt(i + 1) == '*') {
                        depth++;
                        i += 2;
                    } else if (sql.charAt(i) == '*' && i + 1 < len && sql.charAt(i + 1) == '/') {
                        depth--;
                        i += 2;
                    } else {
                        i++;
                    }
                }
                continue;
            }

            // 跳过双引号标识符（PG 引用标识符）
            if (ch == '"') {
                i++;
                while (i < len) {
                    if (sql.charAt(i) == '"') {
                        if (i + 1 < len && sql.charAt(i + 1) == '"') {
                            i += 2;
                        } else {
                            i++;
                            break;
                        }
                    } else {
                        i++;
                    }
                }
                continue;
            }

            // 匹配 AS 关键字（前后需为非字母数字/下划线边界）
            if (matchesKeyword(sql, i, "AS")) {
                int afterAsPos = i + 2;
                // 跳过 AS 后的空白
                while (afterAsPos < len && Character.isWhitespace(sql.charAt(afterAsPos))) {
                    afterAsPos++;
                }
                if (matchesKeyword(sql, afterAsPos, "SELECT")) {
                    return true;
                }
                // AS 后非 SELECT（如 AS alias），继续扫描
                i = afterAsPos;
                continue;
            }

            i++;
        }
        return false;
    }

    /**
     * 在 sql 位置 i 处匹配关键字 keyword（大小写不敏感），
     * 要求关键字前后均为非字母数字/下划线边界（避免 INDEX 误匹配 IN、BETWEEN 误匹配 AS）。
     */
    private static boolean matchesKeyword(String sql, int i, String keyword) {
        int len = sql.length();
        if (i + keyword.length() > len) {
            return false;
        }
        // 前边界
        if (i > 0) {
            char prev = sql.charAt(i - 1);
            if (Character.isLetterOrDigit(prev) || prev == '_') {
                return false;
            }
        }
        for (int k = 0; k < keyword.length(); k++) {
            if (Character.toUpperCase(sql.charAt(i + k)) != keyword.charAt(k)) {
                return false;
            }
        }
        // 后边界
        int after = i + keyword.length();
        if (after < len) {
            char next = sql.charAt(after);
            if (Character.isLetterOrDigit(next) || next == '_') {
                return false;
            }
        }
        return true;
    }

    private ReplayResult replayReadonly(ManifestStatement s, String type) {
        StatementTimings timings = null;
        String error = null;
        String planJson = null;

        try {
            timings = timeReadonly(s);
        } catch (SQLException e) {
            error = describe("timing", e);
        }

        try (Connection c = pool.getConnection()) {
            boolean useAnalyze = config.getExplainMode() == ExplainMode.ANALYZE
                    || config.getExplainMode() == ExplainMode.AUTO;
            planJson = useAnalyze
                    ? explainAdapter.explainAnalyze(c, s.getSql(), binder, s, true)
                    : explainAdapter.explainOnly(c, s.getSql(), binder, s);
        } catch (SQLException e) {
            if (error == null) {
                error = describe("plan", e);
            }
        }

        return buildResultWithPlan(s, timings, error, planJson, false, null);
    }

    private ReplayResult replayDml(ManifestStatement s, String type) {
        StatementTimings timings = null;
        String error = null;
        String planJson = null;

        try {
            timings = timeDml(s);
        } catch (SQLException e) {
            error = describe("timing", e);
        }

        try (Connection c = pool.getConnection()) {
            if (config.getExplainMode() == ExplainMode.ANALYZE) {
                planJson = explainAdapter.explainAnalyzeDml(c, s.getSql(), binder, s);
            } else {
                planJson = explainAdapter.explainOnly(c, s.getSql(), binder, s);
            }
        } catch (SQLException e) {
            if (error == null) {
                error = describe("plan", e);
            }
        }

        return buildResultWithPlan(s, timings, error, planJson, false, null);
    }

    // ----------------------- 计时实现 -----------------------

    private StatementTimings timeReadonly(ManifestStatement s) throws SQLException {
        int warmup = config.getWarmup();
        int iter = config.getIterations();
        long[] samples = new long[Math.max(0, iter)];
        try (Connection c = pool.getConnection()) {
            for (int i = 0; i < warmup; i++) {
                runOnceReadonly(c, s);
            }
            for (int i = 0; i < iter; i++) {
                long t0 = System.nanoTime();
                runOnceReadonly(c, s);
                samples[i] = System.nanoTime() - t0;
            }
        }
        return new StatementTimings(samples);
    }

    private void runOnceReadonly(Connection c, ManifestStatement s) throws SQLException {
        long maxRows = config.getMaxRows();
        try (PreparedStatement ps = c.prepareStatement(s.getSql())) {
            binder.bind(ps, s);
            try (ResultSet rs = ps.executeQuery()) {
                if (maxRows > 0) {
                    long counted = 0;
                    while (rs.next() && counted < maxRows) {
                        counted++;
                    }
                } else {
                    // 无限制：保持原有行为，全量物化结果集
                    while (rs.next()) {
                        rs.getObject(1);
                    }
                }
            }
        }
    }

    private StatementTimings timeDml(ManifestStatement s) throws SQLException {
        int warmup = config.getWarmup();
        int iter = config.getIterations();
        long[] samples = new long[Math.max(0, iter)];
        try (Connection c = pool.getConnection()) {
            c.setAutoCommit(false);
            try {
                for (int i = 0; i < warmup; i++) {
                    runOnceDml(c, s);
                    c.rollback();
                }
                for (int i = 0; i < iter; i++) {
                    long t0 = System.nanoTime();
                    runOnceDml(c, s);
                    samples[i] = System.nanoTime() - t0;
                    c.rollback();
                }
            } finally {
                try { c.rollback(); } catch (SQLException ignored) { }
                try { c.setAutoCommit(true); } catch (SQLException ignored) { }
            }
        }
        return new StatementTimings(samples);
    }

    private void runOnceDml(Connection c, ManifestStatement s) throws SQLException {
        try (PreparedStatement ps = c.prepareStatement(s.getSql())) {
            binder.bind(ps, s);
            ps.executeUpdate();
        }
    }

    // ----------------------- 结果组装 -----------------------

    private ReplayResult buildResultWithPlan(ManifestStatement s,
                                             StatementTimings timings,
                                             String error,
                                             String planJson,
                                             boolean skipped,
                                             String skipReason) {
        // 解析计划树（按方言分发）
        PlanNode topNode = null;
        List<PlanNode> planNodes = null;
        if (planJson != null) {
            try {
                if (dialect == DbDialect.MYSQL) {
                    planNodes = MySqlPlanParser.parseList(planJson);
                    topNode = planNodes.isEmpty() ? null : planNodes.get(0);
                } else {
                    topNode = PlanParser.parse(planJson);
                    planNodes = new ArrayList<PlanNode>();
                    if (topNode != null) {
                        planNodes.add(topNode);
                    }
                }
            } catch (Exception e) {
                if (error == null) {
                    error = "plan parse failed: " + e.getMessage();
                }
            }
        }

        String planTopNode = topNode == null ? null : topNode.getNodeType();
        double planTotalCost = topNode == null ? 0.0 : topNode.getTotalCost();

        // 启发式分析（按方言分发）
        List<PlanFinding> findings = new ArrayList<PlanFinding>();
        if (planNodes != null && !planNodes.isEmpty()) {
            if (dialect == DbDialect.MYSQL) {
                findings = MySqlPlanAnalyzer.analyze(planNodes, config.getSeqScanRows());
            } else {
                findings = PlanAnalyzer.analyze(topNode, config.getSeqScanRows());
            }
        }

        if (!skipped) {
            // PARAM001
            int phCount = ParamBinder.countPlaceholders(s.getSql());
            if (phCount > 0 && !binder.hasFixture(s.getId())) {
                findings.add(new PlanFinding(
                        "PARAM001",
                        PlanFinding.Severity.warning,
                        "SQL 含 " + phCount + " 个占位符但无 fixture，全绑 NULL → "
                                + "计时/慢SQL 检测结果不可信（WHERE col = NULL 恒为假）",
                        null, null));
            }

            // DYN003
            int maxInParams = countMaxInClauseParams(s.getSql());
            if (maxInParams > config.getMaxInClauseParams()) {
                findings.add(new PlanFinding(
                        "DYN003",
                        PlanFinding.Severity.warning,
                        "IN 子句包含 " + maxInParams + " 个参数（阈值 "
                                + config.getMaxInClauseParams() + "），"
                                + "可能导致查询计划退化或超出数据库 IN 限制",
                        null, null));
            }
        }

        ReplayResult.SlowLevel level = ReplayResult.SlowLevel.none;
        if (timings != null && error == null) {
            level = SlowDetector.detect(timings, config.getSlowWarnMs(), config.getSlowErrorMs());
        }

        return new ReplayResult(
                s.getId(),
                s.getType(),
                s.getSource(),
                s.getLine(),
                s.getSql(),
                timings,
                level,
                error,
                planJson,
                planTopNode,
                planTotalCost,
                findings,
                skipped,
                skipReason);
    }

    private ReplayResult skipped(ManifestStatement s, String reason) {
        return new ReplayResult(
                s.getId(),
                s.getType(),
                s.getSource(),
                s.getLine(),
                s.getSql(),
                null,
                ReplayResult.SlowLevel.none,
                null,
                null,
                null,
                0.0,
                java.util.Collections.<PlanFinding>emptyList(),
                true,
                reason);
    }

    private static String describe(String phase, SQLException e) {
        return phase + " failed: " + e.getClass().getSimpleName() + ": " + e.getMessage();
    }

    /**
     * 统计 SQL 中最大的 IN 子句占位符数量。
     */
    static int countMaxInClauseParams(String sql) {
        if (sql == null || sql.isEmpty()) {
            return 0;
        }
        int maxCount = 0;
        int len = sql.length();
        int i = 0;
        while (i < len) {
            if (isInKeyword(sql, i)) {
                int j = i + 2;
                while (j < len && Character.isWhitespace(sql.charAt(j))) {
                    j++;
                }
                if (j < len && sql.charAt(j) == '(') {
                    j++;
                    int count = 0;
                    while (j < len && sql.charAt(j) != ')') {
                        if (sql.charAt(j) == '?') {
                            count++;
                        }
                        if (sql.charAt(j) == '\'') {
                            j++;
                            while (j < len) {
                                if (sql.charAt(j) == '\'') {
                                    if (j + 1 < len && sql.charAt(j + 1) == '\'') {
                                        j += 2;
                                    } else {
                                        j++;
                                        break;
                                    }
                                } else {
                                    j++;
                                }
                            }
                            continue;
                        }
                        j++;
                    }
                    if (count > maxCount) {
                        maxCount = count;
                    }
                    i = j;
                    continue;
                }
            }
            i++;
        }
        return maxCount;
    }

    private static boolean isInKeyword(String sql, int i) {
        if (i + 2 > sql.length()) {
            return false;
        }
        char c1 = sql.charAt(i);
        char c2 = sql.charAt(i + 1);
        if (!(Character.toUpperCase(c1) == 'I' && Character.toUpperCase(c2) == 'N')) {
            return false;
        }
        if (i + 2 < sql.length() && Character.isLetter(sql.charAt(i + 2))) {
            return false;
        }
        if (i > 0) {
            char prev = sql.charAt(i - 1);
            if (!Character.isWhitespace(prev) && prev != '(') {
                return false;
            }
        }
        return true;
    }
}
