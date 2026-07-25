package com.sqlguard.replay.db;

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
 * 每条连接初始化时执行 {@code SET statement_timeout = <ms>} 防止失控查询挂住。
 */
public final class ConnectionPool implements AutoCloseable {

    private final HikariDataSource ds;

    public ConnectionPool(String jdbcUrl,
                          String user,
                          String password,
                          int poolSize,
                          long connectionTimeoutMs,
                          long statementTimeoutMs) {
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
        // 每条连接初始化时设置语句超时，防止慢查询挂住
        config.setConnectionInitSql("SET statement_timeout = " + statementTimeoutMs);
        config.setPoolName("sqlguard-replay");
        this.ds = new HikariDataSource(config);
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
