package com.sqlguard.replay;

import com.sqlguard.replay.config.ReplayConfig;
import com.sqlguard.replay.db.ConnectionPool;
import com.sqlguard.replay.manifest.Manifest;
import com.sqlguard.replay.manifest.ManifestLoader;
import com.sqlguard.replay.manifest.ManifestStatement;
import com.sqlguard.replay.param.ParamBinder;
import com.sqlguard.replay.replay.ExplainMode;
import com.sqlguard.replay.replay.ReplayResult;
import com.sqlguard.replay.replay.Replayer;
import com.sqlguard.replay.report.ReplayReport;
import com.sqlguard.replay.report.ReportWriter;

import java.io.File;
import java.net.URL;
import java.net.URLClassLoader;
import java.nio.file.Path;
import java.sql.Driver;
import java.sql.DriverManager;
import java.util.ArrayList;
import java.util.HashMap;
import java.util.List;
import java.util.Map;
import java.util.Set;

/**
 * sqlguard-replay 入口：解析 CLI、加载驱动、构建连接池、重放、写报告。
 */
public final class Main {

    private Main() {
    }

    public static void main(String[] args) {
        try {
            int exit = run(args);
            System.exit(exit);
        } catch (Throwable t) {
            System.err.println("FATAL: " + t.getClass().getSimpleName() + ": " + t.getMessage());
            t.printStackTrace(System.err);
            System.exit(2);
        }
    }

    private static int run(String[] args) throws Exception {
        ReplayConfig config = parseArgs(args);

        // 校验必填项
        if (config.getJdbcUrl() == null || config.getJdbcUrl().isEmpty()) {
            System.err.println("ERROR: --jdbc-url (or env REPLAY_DB_URL) is required");
            return 2;
        }
        // 驱动 jar：未显式指定时自动从 replay/lib/ 发现
        Path driverJar = config.getDriverJar();
        if (driverJar == null) {
            driverJar = discoverDriverJar();
            if (driverJar == null) {
                System.err.println("ERROR: --driver-jar not specified and no jar found in replay/lib/. "
                        + "Put the GaussDB JDBC driver under replay/lib/ or pass --driver-jar <path>.");
                return 2;
            }
            System.err.println("[replay] auto-discovered driver jar: " + driverJar);
        }
        File driverFile = driverJar.toFile();
        if (!driverFile.isFile()) {
            System.err.println("ERROR: driver jar not found: " + driverFile);
            return 2;
        }
        if (config.getManifestPath() == null) {
            System.err.println("ERROR: --manifest <path> is required");
            return 2;
        }

        // 1. 通过 URLClassLoader 动态加载 JDBC 驱动并注册到 DriverManager
        loadDriver(driverJar, config.getDriverClass());

        // 2. 构建连接池
        try (ConnectionPool pool = new ConnectionPool(
                config.getJdbcUrl(),
                config.getJdbcUser(),
                config.getJdbcPassword(),
                config.getPoolSize(),
                30000L,
                config.getStatementTimeoutMs())) {

            // 3. 加载清单
            Manifest manifest = ManifestLoader.load(config.getManifestPath());
            List<ManifestStatement> all = manifest.getStatements();
            if (all == null) {
                all = new ArrayList<ManifestStatement>();
            }
            System.err.println("[replay] manifest loaded: " + all.size() + " statements");

            // 4. 过滤
            List<ManifestStatement> filtered = filterStatements(all, config);

            // 5. 加载参数 fixture
            ParamBinder binder = ParamBinder.load(config.getParamFixture());

            // 6. 逐条重放
            Replayer replayer = new Replayer(config, pool, binder);
            List<ReplayResult> results = new ArrayList<ReplayResult>(filtered.size());
            for (ManifestStatement s : filtered) {
                System.err.println("[replay] " + s.getId() + " (" + s.getType() + ") ...");
                ReplayResult r;
                try {
                    r = replayer.replay(s);
                } catch (Throwable t) {
                    // Replayer 自身已吞 SQLException；这里只兜底防进程崩
                    r = new ReplayResult(
                            s.getId(), s.getType(), s.getSource(), s.getLine(), s.getSql(),
                            null, ReplayResult.SlowLevel.none,
                            "unexpected: " + t.getClass().getSimpleName() + ": " + t.getMessage(),
                            null, null, 0.0, null, false, null);
                }
                results.add(r);
                printProgress(s, r);
            }

            // 7. 聚合并写报告（SlowDetector + PlanAnalyzer 已在 Replayer 内完成）
            ReplayReport report = ReportWriter.buildReport(results);
            ReportWriter.write(report, config.getOutputDir());
            System.err.println("[replay] report written to " + config.getOutputDir().toAbsolutePath());

            // 8. 退出码：仅 slowError 或执行错误视为失败
            for (ReplayResult r : results) {
                if (r.getSlowLevel() == ReplayResult.SlowLevel.error) {
                    return 1;
                }
                if (r.getError() != null && !r.isSkipped()) {
                    return 1;
                }
            }
            return 0;
        }
    }

    // ----------------------- 驱动加载 -----------------------

    private static void loadDriver(Path driverJar, String driverClass) throws Exception {
        URL[] urls = new URL[]{driverJar.toUri().toURL()};
        // 让 DriverManager 能从该 classloader 解析到驱动
        // URLClassLoader 不显式关闭：驱动实例已注册到 DriverManager，
        // 关闭 classloader 可能导致后续 JDBC 调用找不到驱动类（进程级生命周期）
        URLClassLoader loader = new URLClassLoader(urls, Main.class.getClassLoader());
        Thread.currentThread().setContextClassLoader(loader);
        Class<?> clazz = Class.forName(driverClass, true, loader);
        Driver driver = (Driver) clazz.newInstance();
        DriverManager.registerDriver(driver);
        System.err.println("[replay] driver registered: " + driverClass + " from " + driverJar);
    }

    /**
     * 未显式指定 --driver-jar 时，从常见位置自动发现 jar：
     * <ol>
     *   <li>当前工作目录下的 {@code replay/lib/*.jar}</li>
     *   <li>sqlguard-replay 自身 jar 所在目录的 {@code ../lib/*.jar}（打包后从任意位置运行）</li>
     * </ol>
     * 命中多个时取排序后的第一个，保证可重现。返回 null 表示未发现。
     */
    private static Path discoverDriverJar() {
        java.util.List<Path> candidates = new java.util.ArrayList<Path>();
        candidates.add(java.nio.file.Paths.get("replay", "lib"));
        // 打包后运行：从 Main.class 的 protection domain 推断 jar 位置
        try {
            java.security.CodeSource cs = Main.class.getProtectionDomain().getCodeSource();
            if (cs != null && cs.getLocation() != null) {
                Path jarPath = java.nio.file.Paths.get(cs.getLocation().toURI());
                if (jarPath.toString().endsWith(".jar")) {
                    candidates.add(jarPath.getParent().resolve("lib"));
                    candidates.add(jarPath.getParent().getParent().resolve("lib"));
                }
            }
        } catch (Exception ignored) {
            // 推断失败忽略，回退到 CWD 候选
        }
        for (Path dir : candidates) {
            Path found = pickFirstJar(dir);
            if (found != null) {
                return found;
            }
        }
        return null;
    }

    private static Path pickFirstJar(Path dir) {
        File d = dir.toFile();
        if (!d.isDirectory()) {
            return null;
        }
        File[] jars = d.listFiles((f, name) -> name.toLowerCase().endsWith(".jar"));
        if (jars == null || jars.length == 0) {
            return null;
        }
        java.util.Arrays.sort(jars, java.util.Comparator.comparing(File::getName));
        return jars[0].toPath();
    }

    // ----------------------- 过滤 -----------------------

    private static List<ManifestStatement> filterStatements(List<ManifestStatement> all,
                                                            ReplayConfig config) {
        Set<String> types = config.getTypes();
        boolean typeFilter = !types.isEmpty();

        List<ManifestStatement> out = new ArrayList<ManifestStatement>();
        for (ManifestStatement s : all) {
            String t = s.getType() == null ? "other" : s.getType().toLowerCase();
            if (typeFilter && !types.contains(t)) {
                continue;
            }
            // DDL 是否允许由 Replayer 根据 --allow-ddl 决定（产出 skipped 结果便于审计），
            // 这里不做额外过滤，保留全部匹配类型的语句。
            out.add(s);
        }
        return out;
    }

    // ----------------------- 进度打印 -----------------------

    private static void printProgress(ManifestStatement s, ReplayResult r) {
        StringBuilder line = new StringBuilder();
        line.append("[replay]   ").append(s.getId());
        if (r.isSkipped()) {
            line.append(" -> SKIPPED (").append(r.getSkipReason()).append(")");
        } else if (r.getError() != null) {
            line.append(" -> ERROR: ").append(r.getError());
        } else {
            line.append(" -> ok, p99=");
            if (r.getTimings() != null) {
                line.append(String.format("%.2f", r.getTimings().getP99Ms())).append("ms");
            }
            line.append(", slow=").append(r.getSlowLevel());
            line.append(", findings=").append(r.getFindings().size());
        }
        System.err.println(line.toString());
    }

    // ----------------------- CLI 解析 -----------------------

    private static ReplayConfig parseArgs(String[] args) {
        Map<String, String> kv = new HashMap<String, String>();
        Set<String> flags = new java.util.HashSet<String>();
        for (int i = 0; i < args.length; i++) {
            String a = args[i];
            if (a.startsWith("--")) {
                String key = a.substring(2);
                // 支持 --key=value
                int eq = key.indexOf('=');
                if (eq >= 0) {
                    kv.put(key.substring(0, eq), key.substring(eq + 1));
                } else if (i + 1 < args.length && !args[i + 1].startsWith("--")) {
                    kv.put(key, args[++i]);
                } else {
                    flags.add(key);
                }
            }
        }

        Path manifestPath = ReplayConfig.toPathOrNull(getOpt(kv, "manifest", "sql-manifest.json"));
        Path outputDir = ReplayConfig.toPathOrNull(getOpt(kv, "output-dir", "."));
        ExplainMode mode = parseExplainMode(getOpt(kv, "explain-mode", "auto"));
        int iterations = parseInt(getOpt(kv, "iterations", "3"), 3);
        int warmup = parseInt(getOpt(kv, "warmup", "1"), 1);
        boolean allowDdl = flags.contains("allow-ddl") || "true".equalsIgnoreCase(kv.get("allow-ddl"));
        Set<String> types = ReplayConfig.parseTypes(getOpt(kv, "types", ""));
        long slowWarnMs = parseLong(getOpt(kv, "slow-warn-ms", "100"), 100L);
        long slowErrorMs = parseLong(getOpt(kv, "slow-error-ms", "1000"), 1000L);
        Path paramFixture = ReplayConfig.toPathOrNull(kv.get("param-fixture"));
        String jdbcUrl = getOpt(kv, "jdbc-url", System.getenv("REPLAY_DB_URL"));
        String jdbcUser = getOpt(kv, "jdbc-user", System.getenv("REPLAY_DB_USER"));
        String jdbcPassword = getOpt(kv, "jdbc-password", System.getenv("REPLAY_DB_PASSWORD"));
        Path driverJar = ReplayConfig.toPathOrNull(kv.get("driver-jar"));
        String driverClass = getOpt(kv, "driver-class", "org.postgresql.Driver");
        long seqScanRows = parseLong(getOpt(kv, "seq-scan-rows", "10000"), 10000L);
        int poolSize = parseInt(getOpt(kv, "pool-size", "4"), 4);
        long statementTimeoutMs = parseLong(getOpt(kv, "statement-timeout-ms", "30000"), 30000L);

        return new ReplayConfig(
                manifestPath, outputDir, mode, iterations, warmup, allowDdl, types,
                slowWarnMs, slowErrorMs, paramFixture, jdbcUrl, jdbcUser, jdbcPassword,
                driverJar, driverClass, seqScanRows, poolSize, statementTimeoutMs);
    }

    private static String getOpt(Map<String, String> kv, String key, String def) {
        String v = kv.get(key);
        if (v != null && !v.isEmpty()) {
            return v;
        }
        return def == null ? null : def;
    }

    private static ExplainMode parseExplainMode(String s) {
        if (s == null) {
            return ExplainMode.AUTO;
        }
        if ("analyze".equalsIgnoreCase(s)) {
            return ExplainMode.ANALYZE;
        }
        if ("explain".equalsIgnoreCase(s)) {
            return ExplainMode.EXPLAIN;
        }
        return ExplainMode.AUTO;
    }

    private static int parseInt(String s, int def) {
        try {
            return Integer.parseInt(s.trim());
        } catch (Exception e) {
            return def;
        }
    }

    private static long parseLong(String s, long def) {
        try {
            return Long.parseLong(s.trim());
        } catch (Exception e) {
            return def;
        }
    }
}
