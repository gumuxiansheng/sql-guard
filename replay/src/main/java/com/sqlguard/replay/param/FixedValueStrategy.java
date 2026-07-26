package com.sqlguard.replay.param;

import com.sqlguard.replay.manifest.ManifestStatement;

/**
 * 默认参数生成策略：所有占位符绑同一个固定值。
 *
 * <p>简单但实用：用 "1" 作为默认值，对整数主键/外键查询足够走索引扫描。
 * 字符串字段绑 "1" 也不会报错（JDBC 驱动会做类型转换，最多查不到数据）。
 *
 * <p>比全绑 NULL 好得多：WHERE id = 1 会正常走索引，EXPLAIN 计划有参考价值。
 */
public final class FixedValueStrategy implements DefaultParamStrategy {

    private final String value;

    /**
     * 默认使用 "1" 作为占位符值。
     */
    public FixedValueStrategy() {
        this("1");
    }

    /**
     * 自定义固定值。
     *
     * @param value 所有占位符绑定的值
     */
    public FixedValueStrategy(String value) {
        this.value = value;
    }

    @Override
    public String generate(ManifestStatement stmt, int index) {
        return value;
    }
}
