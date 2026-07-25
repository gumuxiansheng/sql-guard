package com.sqlguard.replay.config;

import com.sqlguard.replay.replay.ExplainMode;

import java.nio.file.Path;
import java.nio.file.Paths;
import java.util.Collections;
import java.util.LinkedHashSet;
import java.util.Set;

/**
 * 重放工具的全部配置项（不可变 bean）。
 *
 * <p>所有字段在构造后只读，便于在多线程重放时安全传递。
 */
public final class ReplayConfig {

    private final Path manifestPath;
    private final Path outputDir;
    private final ExplainMode explainMode;
    private final int iterations;
    private final int warmup;
    private final boolean allowDdl;
    private final Set<String> types;            // 空 = 不过滤
    private final long slowWarnMs;
    private final long slowErrorMs;
    private final Path paramFixture;            // 可为 null
    private final String jdbcUrl;
    private final String jdbcUser;
    private final String jdbcPassword;
    private final Path driverJar;
    private final String driverClass;
    private final long seqScanRows;             // DP001/DP003/DP007 阈值
    private final int poolSize;
    private final long statementTimeoutMs;

    public ReplayConfig(Path manifestPath,
                        Path outputDir,
                        ExplainMode explainMode,
                        int iterations,
                        int warmup,
                        boolean allowDdl,
                        Set<String> types,
                        long slowWarnMs,
                        long slowErrorMs,
                        Path paramFixture,
                        String jdbcUrl,
                        String jdbcUser,
                        String jdbcPassword,
                        Path driverJar,
                        String driverClass,
                        long seqScanRows,
                        int poolSize,
                        long statementTimeoutMs) {
        this.manifestPath = manifestPath;
        this.outputDir = outputDir;
        this.explainMode = explainMode;
        this.iterations = iterations;
        this.warmup = warmup;
        this.allowDdl = allowDdl;
        this.types = types == null
                ? Collections.<String>emptySet()
                : Collections.unmodifiableSet(new LinkedHashSet<String>(types));
        this.slowWarnMs = slowWarnMs;
        this.slowErrorMs = slowErrorMs;
        this.paramFixture = paramFixture;
        this.jdbcUrl = jdbcUrl;
        this.jdbcUser = jdbcUser;
        this.jdbcPassword = jdbcPassword;
        this.driverJar = driverJar;
        this.driverClass = driverClass;
        this.seqScanRows = seqScanRows;
        this.poolSize = poolSize;
        this.statementTimeoutMs = statementTimeoutMs;
    }

    public Path getManifestPath() {
        return manifestPath;
    }

    public Path getOutputDir() {
        return outputDir;
    }

    public ExplainMode getExplainMode() {
        return explainMode;
    }

    public int getIterations() {
        return iterations;
    }

    public int getWarmup() {
        return warmup;
    }

    public boolean isAllowDdl() {
        return allowDdl;
    }

    public Set<String> getTypes() {
        return types;
    }

    public long getSlowWarnMs() {
        return slowWarnMs;
    }

    public long getSlowErrorMs() {
        return slowErrorMs;
    }

    public Path getParamFixture() {
        return paramFixture;
    }

    public String getJdbcUrl() {
        return jdbcUrl;
    }

    public String getJdbcUser() {
        return jdbcUser;
    }

    public String getJdbcPassword() {
        return jdbcPassword;
    }

    public Path getDriverJar() {
        return driverJar;
    }

    public String getDriverClass() {
        return driverClass;
    }

    public long getSeqScanRows() {
        return seqScanRows;
    }

    public int getPoolSize() {
        return poolSize;
    }

    public long getStatementTimeoutMs() {
        return statementTimeoutMs;
    }

    /**
     * 把 SQL 类型过滤集合规范化为小写、去空、去重；空串视为不过滤。
     */
    public static Set<String> parseTypes(String csv) {
        Set<String> out = new LinkedHashSet<String>();
        if (csv == null) {
            return out;
        }
        for (String part : csv.split(",")) {
            String t = part.trim().toLowerCase();
            if (!t.isEmpty()) {
                out.add(t);
            }
        }
        return out;
    }

    /**
     * 把字符串路径转为 Path，null 或空串返回 null。
     */
    public static Path toPathOrNull(String s) {
        if (s == null || s.trim().isEmpty()) {
            return null;
        }
        return Paths.get(s.trim());
    }
}
