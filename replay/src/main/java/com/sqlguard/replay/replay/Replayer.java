package com.sqlguard.replay.replay;

import com.sqlguard.replay.config.ReplayConfig;
import com.sqlguard.replay.db.ConnectionPool;
import com.sqlguard.replay.manifest.ManifestStatement;
import com.sqlguard.replay.param.ParamBinder;
import com.sqlguard.replay.plan.PlanAnalyzer;
import com.sqlguard.replay.plan.PlanCollector;
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
 */
public final class Replayer {

    private final ReplayConfig config;
    private final ConnectionPool pool;
    private final ParamBinder binder;

    public Replayer(ReplayConfig config, ConnectionPool pool, ParamBinder binder) {
        this.config = config;
        this.pool = pool;
        this.binder = binder;
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
        // MERGE 是写语句（UPSERT），必须走 DML 路径（事务+回滚），
        // 否则 EXPLAIN ANALYZE 会真正执行 MERGE 并在 autocommit 下污染镜像库。
        if ("insert".equals(type) || "update".equals(type)
                || "delete".equals(type) || "merge".equals(type)) {
            return replayDml(s, type);
        }

        // 兜底
        return skipped(s, "unsupported type: " + type);
    }

    // ----------------------- 分支实现 -----------------------

    private ReplayResult replayDdlPlanOnly(ManifestStatement s) {
        // PG/openGauss 的 EXPLAIN 只支持 SELECT/INSERT/UPDATE/DELETE/MERGE/CTAS，
        // 对普通 CREATE/ALTER/TRUNCATE/DROP/GRANT/REVOKE 是语法错误。
        // 仅对含 AS SELECT 的 CTAS / CREATE VIEW 采集计划，其余直接跳过。
        String sqlUpper = s.getSql() == null ? "" : s.getSql().trim().toUpperCase();
        boolean hasAsSelect = sqlUpper.contains(" AS SELECT");
        if (!hasAsSelect) {
            return skipped(s, "ddl: EXPLAIN not supported for non-CTAS DDL");
        }
        try (Connection c = pool.getConnection()) {
            String planJson = PlanCollector.explainOnly(c, s.getSql(), binder, s);
            return buildResultWithPlan(s, null, null, planJson, false, null);
        } catch (SQLException e) {
            return buildResultWithPlan(s, null, describe("plan", e), null, false, null);
        }
    }

    private ReplayResult replayReadonly(ManifestStatement s, String type) {
        StatementTimings timings = null;
        String error = null;
        String planJson = null;

        // 计时
        try {
            timings = timeReadonly(s);
        } catch (SQLException e) {
            error = describe("timing", e);
        }

        // 计划（独立尝试，不因计划失败而丢弃计时）
        // 只读分支仅用于 select：AUTO/ANALYZE 模式下用 EXPLAIN ANALYZE 安全采集
        try (Connection c = pool.getConnection()) {
            boolean useAnalyze = config.getExplainMode() == ExplainMode.ANALYZE
                    || config.getExplainMode() == ExplainMode.AUTO;
            planJson = useAnalyze
                    ? PlanCollector.explainAnalyze(c, s.getSql(), binder, s, true)
                    : PlanCollector.explainOnly(c, s.getSql(), binder, s);
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

        // 计时：事务包裹后回滚，保证镜像库不写脏数据
        try {
            timings = timeDml(s);
        } catch (SQLException e) {
            error = describe("timing", e);
        }

        // 计划
        // DML（含 merge）：ANALYZE 模式下 EXPLAIN ANALYZE 会真正执行，必须走事务+回滚
        // AUTO 模式下 DML 用纯 EXPLAIN（不执行），安全
        try (Connection c = pool.getConnection()) {
            if (config.getExplainMode() == ExplainMode.ANALYZE) {
                planJson = PlanCollector.explainAnalyzeDml(c, s.getSql(), binder, s);
            } else {
                planJson = PlanCollector.explainOnly(c, s.getSql(), binder, s);
            }
        } catch (SQLException e) {
            if (error == null) {
                error = describe("plan", e);
            }
        }

        return buildResultWithPlan(s, timings, error, planJson, false, null);
    }

    // ----------------------- 计时实现 -----------------------

    /** select 计时：只读，无需事务控制。 */
    private StatementTimings timeReadonly(ManifestStatement s) throws SQLException {
        int warmup = config.getWarmup();
        int iter = config.getIterations();
        long[] samples = new long[Math.max(0, iter)];
        try (Connection c = pool.getConnection()) {
            // 预热
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
        try (PreparedStatement ps = c.prepareStatement(s.getSql())) {
            binder.bind(ps, s);
            try (ResultSet rs = ps.executeQuery()) {
                // 完整消费 ResultSet，避免提前关闭影响时延测量
                while (rs.next()) {
                    // 触发行读取
                    rs.getObject(1);
                }
            }
        }
    }

    /** insert/update/delete 计时：关闭自动提交，每次迭代后回滚，永不提交。 */
    private StatementTimings timeDml(ManifestStatement s) throws SQLException {
        int warmup = config.getWarmup();
        int iter = config.getIterations();
        long[] samples = new long[Math.max(0, iter)];
        try (Connection c = pool.getConnection()) {
            c.setAutoCommit(false);
            try {
                // 预热（同样回滚）
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
                try {
                    c.rollback();
                } catch (SQLException ignored) {
                    // 忽略回滚失败
                }
                try {
                    c.setAutoCommit(true);
                } catch (SQLException ignored) {
                    // 忽略
                }
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
        // 解析计划树
        PlanNode topNode = null;
        if (planJson != null) {
            try {
                topNode = PlanParser.parse(planJson);
            } catch (Exception e) {
                // 计划解析失败不致命，附加到 error
                if (error == null) {
                    error = "plan parse failed: " + e.getMessage();
                }
            }
        }

        String planTopNode = topNode == null ? null : topNode.getNodeType();
        double planTotalCost = topNode == null ? 0.0 : topNode.getTotalCost();

        // 启发式分析
        List<com.sqlguard.replay.plan.PlanFinding> findings =
                topNode == null
                        ? new ArrayList<com.sqlguard.replay.plan.PlanFinding>()
                        : PlanAnalyzer.analyze(topNode, config.getSeqScanRows());

        if (!skipped) {
            // PARAM001: 有占位符但无 fixture → 全绑 NULL，计时/计划结果不可信
            int phCount = ParamBinder.countPlaceholders(s.getSql());
            if (phCount > 0 && !binder.hasFixture(s.getId())) {
                findings.add(new com.sqlguard.replay.plan.PlanFinding(
                        "PARAM001",
                        com.sqlguard.replay.plan.PlanFinding.Severity.warning,
                        "SQL 含 " + phCount + " 个占位符但无 fixture，全绑 NULL → "
                                + "计时/慢SQL 检测结果不可信（WHERE col = NULL 恒为假）",
                        null, null));
            }

            // DYN003: 超大 IN 子句检测
            int maxInParams = countMaxInClauseParams(s.getSql());
            if (maxInParams > config.getMaxInClauseParams()) {
                findings.add(new com.sqlguard.replay.plan.PlanFinding(
                        "DYN003",
                        com.sqlguard.replay.plan.PlanFinding.Severity.warning,
                        "IN 子句包含 " + maxInParams + " 个参数（阈值 "
                                + config.getMaxInClauseParams() + "），"
                                + "可能导致查询计划退化或超出数据库 IN 限制",
                        null, null));
            }
        }

        // 慢 SQL 识别（出错或跳过时跳过）
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
                java.util.Collections.<com.sqlguard.replay.plan.PlanFinding>emptyList(),
                true,
                reason);
    }

    private static String describe(String phase, SQLException e) {
        return phase + " failed: " + e.getClass().getSimpleName() + ": " + e.getMessage();
    }

    /**
     * 统计 SQL 中最大的 IN 子句占位符数量。
     *
     * <p>扫描所有 {@code IN ( ? , ? , ... )} 模式，返回单条 IN 子句中 ? 的最大数量。
     * 用于检测 MyBatis foreach 生成的超大 IN 子句。
     */
    static int countMaxInClauseParams(String sql) {
        if (sql == null || sql.isEmpty()) {
            return 0;
        }
        int maxCount = 0;
        int len = sql.length();
        int i = 0;
        while (i < len) {
            // 查找 IN 关键字（大小写不敏感，前面是空白或行首）
            if (isInKeyword(sql, i)) {
                // 跳过 "IN"
                int j = i + 2;
                // 跳过空白
                while (j < len && Character.isWhitespace(sql.charAt(j))) {
                    j++;
                }
                // 期望 '('
                if (j < len && sql.charAt(j) == '(') {
                    j++; // 跳过 '('
                    int count = 0;
                    // 统计括号内的 ? 数量
                    while (j < len && sql.charAt(j) != ')') {
                        if (sql.charAt(j) == '?') {
                            count++;
                        }
                        // 跳过字符串字面量
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

    /**
     * 检查 sql 在位置 i 处是否为 IN 关键字（大小写不敏感，前面须为空白或行首）。
     */
    private static boolean isInKeyword(String sql, int i) {
        if (i + 2 > sql.length()) {
            return false;
        }
        char c1 = sql.charAt(i);
        char c2 = sql.charAt(i + 1);
        if (!(Character.toUpperCase(c1) == 'I' && Character.toUpperCase(c2) == 'N')) {
            return false;
        }
        // 后面不能跟字母（避免匹配 INDEX, INSERT 等）
        if (i + 2 < sql.length() && Character.isLetter(sql.charAt(i + 2))) {
            return false;
        }
        // 前面须为空白或行首
        if (i > 0) {
            char prev = sql.charAt(i - 1);
            if (!Character.isWhitespace(prev) && prev != '(') {
                return false;
            }
        }
        return true;
    }
}
