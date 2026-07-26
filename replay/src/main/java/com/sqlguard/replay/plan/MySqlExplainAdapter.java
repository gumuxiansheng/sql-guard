package com.sqlguard.replay.plan;

import com.sqlguard.replay.manifest.ManifestStatement;
import com.sqlguard.replay.param.ParamBinder;

import java.sql.Connection;
import java.sql.PreparedStatement;
import java.sql.ResultSet;
import java.sql.SQLException;

/**
 * MySQL / MariaDB 的 EXPLAIN 适配器。
 *
 * <p>语法：
 * <ul>
 *   <li>纯计划：{@code EXPLAIN FORMAT=JSON <sql>}</li>
 *   <li>MySQL 不支持 EXPLAIN ANALYZE（MySQL 8.0.18+ 支持 EXPLAIN ANALYZE 但输出格式是表而非 JSON），
 *       所以 ANALYZE 模式 fallback 到纯 EXPLAIN</li>
 *   <li>DML：MySQL 的 EXPLAIN 不执行 SQL，天然安全，无需事务包裹</li>
 * </ul>
 *
 * <p>注意：MySQL 的 EXPLAIN 对 INSERT/UPDATE/DELETE 需要 MySQL 8.0+ 才支持。
 * 旧版 MySQL 的 EXPLAIN 仅支持 SELECT。遇到不支持的语句会抛 SQLException，
 * Replayer 会捕获并记录到 error 字段。
 */
public final class MySqlExplainAdapter implements ExplainAdapter {

    @Override
    public String explainOnly(Connection c, String sql, ParamBinder binder, ManifestStatement s)
            throws SQLException {
        String explainSql = "EXPLAIN FORMAT=JSON " + sql;
        return runExplain(c, explainSql, binder, s);
    }

    @Override
    public String explainAnalyze(Connection c, String sql, ParamBinder binder,
                                 ManifestStatement s, boolean buffers)
            throws SQLException {
        // MySQL 没有 EXPLAIN (FORMAT JSON, ANALYZE)，fallback 到纯 EXPLAIN
        // MySQL 8.0.18+ 有 EXPLAIN ANALYZE 但输出是表格非 JSON，不便解析
        return explainOnly(c, sql, binder, s);
    }

    @Override
    public String explainAnalyzeDml(Connection c, String sql, ParamBinder binder,
                                     ManifestStatement s)
            throws SQLException {
        // MySQL 的 EXPLAIN 不执行 SQL，DML 天然安全
        return explainOnly(c, sql, binder, s);
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
