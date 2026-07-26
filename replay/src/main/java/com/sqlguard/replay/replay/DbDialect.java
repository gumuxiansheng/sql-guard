package com.sqlguard.replay.replay;

/**
 * 数据库方言枚举，决定 EXPLAIN 语法、计划解析、连接初始化等行为。
 */
public enum DbDialect {
    POSTGRESQL,
    MYSQL;

    /**
     * 从 JDBC URL 自动推断方言。
     *
     * <p>jdbc:postgresql://... → POSTGRESQL
     * <p>jdbc:mysql://...     → MYSQL
     * <p>openGauss/GaussDB    → POSTGRESQL（兼容 PG 协议）
     */
    public static DbDialect fromJdbcUrl(String url) {
        if (url == null || url.isEmpty()) {
            return POSTGRESQL;
        }
        String lower = url.toLowerCase();
        if (lower.startsWith("jdbc:mysql")
                || lower.startsWith("jdbc:mariadb")) {
            return MYSQL;
        }
        // postgresql, opengauss, gaussdb 都走 PG 语法
        return POSTGRESQL;
    }

    /**
     * 从字符串解析方言，不区分大小写。
     * 支持：pg/postgres/postgresql, mysql/mariadb
     */
    public static DbDialect parse(String s) {
        if (s == null || s.isEmpty()) {
            return POSTGRESQL;
        }
        String lower = s.trim().toLowerCase();
        if ("mysql".equals(lower) || "mariadb".equals(lower)) {
            return MYSQL;
        }
        return POSTGRESQL;
    }
}
