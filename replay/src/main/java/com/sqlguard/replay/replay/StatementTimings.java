package com.sqlguard.replay.replay;

import java.util.Arrays;

/**
 * 单条语句的耗时样本（纳秒），并提供 ms 级统计量。
 *
 * <p>样本数较小时使用 max 作为 p99 的保守估计；p50 取中位数。
 */
public final class StatementTimings {

    private final long[] samplesNanos;

    public StatementTimings(long[] samplesNanos) {
        this.samplesNanos = samplesNanos == null ? new long[0] : Arrays.copyOf(samplesNanos, samplesNanos.length);
    }

    public long[] getSamplesNanos() {
        return Arrays.copyOf(samplesNanos, samplesNanos.length);
    }

    public int getIterations() {
        return samplesNanos.length;
    }

    private long[] sortedNanos() {
        long[] s = getSamplesNanos();
        Arrays.sort(s);
        return s;
    }

    /** 最小耗时（ms）。无样本返回 0。 */
    public double getMinMs() {
        if (samplesNanos.length == 0) {
            return 0.0;
        }
        long min = Long.MAX_VALUE;
        for (long n : samplesNanos) {
            if (n < min) {
                min = n;
            }
        }
        return nanosToMs(min);
    }

    /** 中位数 p50（ms）。无样本返回 0。 */
    public double getP50Ms() {
        if (samplesNanos.length == 0) {
            return 0.0;
        }
        long[] s = sortedNanos();
        int mid = s.length / 2;
        if (s.length % 2 == 1) {
            return nanosToMs(s[mid]);
        }
        return nanosToMs((s[mid - 1] + s[mid]) / 2.0);
    }

    /**
     * p99（ms）。样本数 n 较少时直接取 max 作为保守估计，避免对小样本做无意义的插值。
     */
    public double getP99Ms() {
        if (samplesNanos.length == 0) {
            return 0.0;
        }
        long[] s = sortedNanos();
        // 样本量较小时用 max 近似 p99
        if (s.length < 100) {
            return nanosToMs(s[s.length - 1]);
        }
        int idx = (int) Math.ceil(0.99 * s.length) - 1;
        if (idx < 0) {
            idx = 0;
        }
        if (idx >= s.length) {
            idx = s.length - 1;
        }
        return nanosToMs(s[idx]);
    }

    /** 平均耗时（ms）。无样本返回 0。 */
    public double getAvgMs() {
        if (samplesNanos.length == 0) {
            return 0.0;
        }
        long sum = 0L;
        for (long n : samplesNanos) {
            sum += n;
        }
        return nanosToMs((double) sum / samplesNanos.length);
    }

    private static double nanosToMs(long nanos) {
        return nanos / 1_000_000.0;
    }

    private static double nanosToMs(double nanos) {
        return nanos / 1_000_000.0;
    }
}
