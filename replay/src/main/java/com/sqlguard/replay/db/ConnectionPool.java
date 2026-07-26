package com.sqlguard.replay.db;

import com.sqlguard.replay.replay.DbDialect;
import com.zaxxer.hikari.HikariConfig;
import com.zaxxer.hikari.HikariDataSource;

import javax.sql.DataSource;
import java.sql.Connection;
import java.sql.SQLException;

/**
 * HikariCP 连接池封装。
 *
 * <p>关键点：JDBC 驱动由 Main 通过 URLClassLoader 动态注册到 DriverManager，
 * 因此这里只设置 jdbcUrl/user/password，不调用 setDriverClassName。
 * 每条连接初始化时执行语句超时设置，防止失控查询挂住。
 *
 * <p>按方言设置不同的 initSql：
 * <ul>
 *   <li>PostgreSQL/openGauss: {@code SET statement_timeout = <ms>}</li>
 *   <li>MySQL/MariaDB: {@code SET SESSION MAX_EXECUTION_TIME = <ms>}</li>
 * </ul>
 */
public final class ConnectionPool implements AutoCloseable {

    private final HikariDataSource ds;

    public ConnectionPool(String jdbcUrl,
                          String user,
                          String password,
                          int poolSize,
                          long connectionTimeoutMs,
                          long statementTimeoutMs) {
        this(jdbcUrl, user, password, poolSize, connectionTimeoutMs, statementTimeoutMs,
                DbDialect.fromJdbcUrl(jdbcUrl));
    }

    public ConnectionPool(String jdbcUrl,
                          String user,
                          String password,
                          int poolSize,
                          long connectionTimeoutMs,
                          long statementTimeoutMs,
                          DbDialect dialect) {
        HikariConfig config = new HikariConfig();
        config.setJdbcUrl(jdbcUrl);
        if (user != null) {
            config.setUsername(user);
        }
        if (password != null) {
            config.setPassword(password);
        }
        config.setMaximumPoolSize(poolSize <= 0 ? 4 : poolSize);
        config.setConnectionTimeout(connectionTimeoutMs <= 0 ? 30000L : connectionTimeoutMs);
        // 每条连接初始化时设置语句超时，按方言选择语法
        // statementTimeoutMs <= 0 时不设置 initSql（用于测试环境如 H2）
        if (statementTimeoutMs > 0) {
            config.setConnectionInitSql(buildInitSql(dialect, statementTimeoutMs));
        }
        config.setPoolName("sqlguard-replay");
        this.ds = new HikariDataSource(config);
    }

    private static String buildInitSql(DbDialect dialect, long statementTimeoutMs) {
        if (dialect == DbDialect.MYSQL) {
            // MySQL 的 MAX_EXECUTION_TIME 单位是毫秒
            return "SET SESSION MAX_EXECUTION_TIME = " + statementTimeoutMs;
        }
        // PG/openGauss 的 statement_timeout 单位是毫秒
        return "SET statement_timeout = " + statementTimeoutMs;
    }

    public Connection getConnection() throws SQLException {
        return ds.getConnection();
    }

    public DataSource getDataSource() {
        return ds;
    }

    @Override
    public void close() {
        if (ds != null && !ds.isClosed()) {
            ds.close();
        }
    }
}
