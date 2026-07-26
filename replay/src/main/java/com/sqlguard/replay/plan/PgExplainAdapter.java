package com.sqlguard.replay.plan;

import com.sqlguard.replay.manifest.ManifestStatement;
import com.sqlguard.replay.param.ParamBinder;

import java.sql.Connection;
import java.sql.PreparedStatement;
import java.sql.ResultSet;
import java.sql.SQLException;

/**
 * PostgreSQL / openGauss / GaussDB 的 EXPLAIN 适配器。
 *
 * <p>语法：
 * <ul>
 *   <li>纯计划：{@code EXPLAIN (FORMAT JSON) <sql>}</li>
 *   <li>执行+计划：{@code EXPLAIN (FORMAT JSON, ANALYZE, BUFFERS) <sql>}</li>
 *   <li>DML 安全：{@code EXPLAIN (FORMAT JSON, ANALYZE) <sql>} 放入事务后回滚</li>
 * </ul>
 */
public final class PgExplainAdapter implements ExplainAdapter {

    @Override
    public String explainOnly(Connection c, String sql, ParamBinder binder, ManifestStatement s)
            throws SQLException {
        String explainSql = "EXPLAIN (FORMAT JSON) " + sql;
        return runExplain(c, explainSql, binder, s);
    }

    @Override
    public String explainAnalyze(Connection c, String sql, ParamBinder binder,
                                 ManifestStatement s, boolean buffers)
            throws SQLException {
        String opts = buffers ? "FORMAT JSON, ANALYZE, BUFFERS" : "FORMAT JSON, ANALYZE";
        String explainSql = "EXPLAIN (" + opts + ") " + sql;
        return runExplain(c, explainSql, binder, s);
    }

    @Override
    public String explainAnalyzeDml(Connection c, String sql, ParamBinder binder,
                                     ManifestStatement s)
            throws SQLException {
        String explainSql = "EXPLAIN (FORMAT JSON, ANALYZE) " + sql;
        boolean prevAuto = c.getAutoCommit();
        c.setAutoCommit(false);
        try {
            String result = runExplain(c, explainSql, binder, s);
            c.rollback();
            return result;
        } finally {
            try { c.rollback(); } catch (SQLException ignored) { }
            try { c.setAutoCommit(prevAuto); } catch (SQLException ignored) { }
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
