package com.sqlguard.replay.slow;

import com.sqlguard.replay.replay.ReplayResult;
import com.sqlguard.replay.replay.StatementTimings;

/**
 * 慢 SQL 识别：基于中位数（p50）与阈值判断慢等级。
 *
 * <p>判级使用中位数而非 p99，因为默认 iterations=3 时 p99=max，
 * 单次 GC/抖动即可触发 error 并让进程 exit 1，CI 噪声严重。
 * 中位数对小样本更稳定，p99 仍保留在报告中供参考。
 *
 * <p>出错或跳过的语句不在本类判断范围（调用方负责跳过）。
 */
public final class SlowDetector {

    private SlowDetector() {
    }

    /**
     * @param timings      计时样本
     * @param slowWarnMs   警告阈值（p50 >= 此值 => warn）
     * @param slowErrorMs  错误阈值（p50 >= 此值 => error）
     */
    public static ReplayResult.SlowLevel detect(StatementTimings timings,
                                                long slowWarnMs,
                                                long slowErrorMs) {
        if (timings == null || timings.getIterations() == 0) {
            return ReplayResult.SlowLevel.none;
        }
        double p50 = timings.getP50Ms();
        if (p50 >= slowErrorMs) {
            return ReplayResult.SlowLevel.error;
        }
        if (p50 >= slowWarnMs) {
            return ReplayResult.SlowLevel.warn;
        }
        return ReplayResult.SlowLevel.none;
    }
}
