package com.sqlguard.replay.plan;

import com.sqlguard.replay.manifest.ManifestStatement;
import com.sqlguard.replay.param.ParamBinder;

import java.sql.Connection;
import java.sql.PreparedStatement;
import java.sql.ResultSet;
import java.sql.SQLException;

/**
 * 执行 EXPLAIN / EXPLAIN ANALYZE 并返回 FORMAT JSON 的原始字符串。
 *
 * <p>所有方法都借用调用方传入的 Connection，不负责关闭连接。
 */
public final class PlanCollector {

    private PlanCollector() {
    }

    /**
     * 纯 EXPLAIN（不执行 SQL 本身），对 DML 也是安全的。
     * 形如：EXPLAIN (FORMAT JSON) <sql>
     */
    public static String explainOnly(Connection c, String sql, ParamBinder binder, ManifestStatement s)
            throws SQLException {
        String explainSql = "EXPLAIN (FORMAT JSON) " + sql;
        return runExplain(c, explainSql, binder, s);
    }

    /**
     * EXPLAIN ANALYZE（会真正执行 SQL）—— 仅用于只读的 select / merge。
     * 形如：EXPLAIN (FORMAT JSON, ANALYZE, BUFFERS) <sql>
     *
     * @param buffers 是否带 BUFFERS 选项
     */
    public static String explainAnalyze(Connection c, String sql, ParamBinder binder,
                                        ManifestStatement s, boolean buffers)
            throws SQLException {
        String opts = buffers ? "FORMAT JSON, ANALYZE, BUFFERS" : "FORMAT JSON, ANALYZE";
        String explainSql = "EXPLAIN (" + opts + ") " + sql;
        return runExplain(c, explainSql, binder, s);
    }

    /**
     * 对 DML 执行 EXPLAIN ANALYZE：放进事务后回滚，使 DML 的实际执行被撤销。
     * 调用方传入的连接当前自动提交状态会被恢复。
     */
    public static String explainAnalyzeDml(Connection c, String sql, ParamBinder binder,
                                           ManifestStatement s) throws SQLException {
        String explainSql = "EXPLAIN (FORMAT JSON, ANALYZE) " + sql;
        boolean prevAuto = c.getAutoCommit();
        c.setAutoCommit(false);
        try {
            String result = runExplain(c, explainSql, binder, s);
            c.rollback();
            return result;
        } finally {
            try {
                c.rollback();
            } catch (SQLException ignored) {
                // 忽略
            }
            try {
                c.setAutoCommit(prevAuto);
            } catch (SQLException ignored) {
                // 忽略
            }
        }
    }

    private static String runExplain(Connection c, String explainSql, ParamBinder binder,
                                     ManifestStatement s) throws SQLException {
        try (PreparedStatement ps = c.prepareStatement(explainSql)) {
            binder.bind(ps, s);
            try (ResultSet rs = ps.executeQuery()) {
                if (rs.next()) {
                    return rs.getString(1);
                }
            }
        }
        return null;
    }
}
