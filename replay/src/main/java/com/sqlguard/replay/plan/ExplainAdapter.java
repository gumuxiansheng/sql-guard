package com.sqlguard.replay.plan;

import com.sqlguard.replay.manifest.ManifestStatement;
import com.sqlguard.replay.param.ParamBinder;

import java.sql.Connection;
import java.sql.SQLException;

/**
 * EXPLAIN 语法适配器：按数据库方言生成正确的 EXPLAIN 语句。
 *
 * <p>各实现负责：
 * <ul>
 *   <li>生成纯 EXPLAIN 语句（不执行 SQL 本身）</li>
 *   <li>生成 EXPLAIN ANALYZE 语句（会执行 SQL）</li>
 *   <li>对 DML 生成安全的 EXPLAIN ANALYZE 语句（事务包裹后回滚）</li>
 * </ul>
 */
public interface ExplainAdapter {

    /**
     * 纯 EXPLAIN（不执行 SQL），对 DML 也安全。
     */
    String explainOnly(Connection c, String sql, ParamBinder binder, ManifestStatement s)
            throws SQLException;

    /**
     * EXPLAIN ANALYZE（会真正执行 SQL），仅用于只读语句。
     *
     * <p>对于不支持 ANALYZE 的数据库（如 MySQL），fallback 到纯 EXPLAIN。
     */
    String explainAnalyze(Connection c, String sql, ParamBinder binder,
                          ManifestStatement s, boolean buffers)
            throws SQLException;

    /**
     * 对 DML 执行 EXPLAIN ANALYZE：放进事务后回滚。
     *
     * <p>对于不支持 ANALYZE 的数据库，fallback 到纯 EXPLAIN（天然不执行，无需事务）。
     */
    String explainAnalyzeDml(Connection c, String sql, ParamBinder binder,
                             ManifestStatement s)
            throws SQLException;
}
