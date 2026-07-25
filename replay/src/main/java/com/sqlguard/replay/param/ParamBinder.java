package com.sqlguard.replay.param;

import com.fasterxml.jackson.core.type.TypeReference;
import com.fasterxml.jackson.databind.ObjectMapper;
import com.sqlguard.replay.manifest.ManifestStatement;

import java.io.IOException;
import java.nio.file.Files;
import java.nio.file.Path;
import java.sql.PreparedStatement;
import java.sql.SQLException;
import java.sql.Types;
import java.util.Collections;
import java.util.LinkedHashMap;
import java.util.List;
import java.util.Map;

/**
 * 占位符绑定器：统计 SQL 中 {@code ?} 个数（忽略字符串字面量内的），
 * 把 fixture 中的值绑定到 PreparedStatement；未提供 fixture 的占位符绑 NULL。
 *
 * <p>fixture 文件格式：{@code {"<statement id>": ["v1","v2"], ...}}，
 * 每个值以 String 绑定，由 JDBC 驱动做类型转换。
 */
public final class ParamBinder {

    private static final ObjectMapper MAPPER = new ObjectMapper();

    private final Map<String, List<String>> fixtures;   // statement id -> values

    public ParamBinder() {
        this.fixtures = Collections.emptyMap();
    }

    public ParamBinder(Map<String, List<String>> fixtures) {
        this.fixtures = fixtures == null
                ? Collections.<String, List<String>>emptyMap()
                : Collections.unmodifiableMap(new LinkedHashMap<String, List<String>>(fixtures));
    }

    /**
     * 从 JSON 文件加载 fixture。
     */
    public static ParamBinder load(Path fixturePath) throws IOException {
        if (fixturePath == null) {
            return new ParamBinder();
        }
        try (java.io.InputStream in = Files.newInputStream(fixturePath)) {
            Map<String, List<String>> m = MAPPER.readValue(in,
                    new TypeReference<Map<String, List<String>>>() {
                    });
            return new ParamBinder(m);
        }
    }

    /**
     * 绑定参数到 PreparedStatement。
     *
     * <p>对每个 {@code ?}（按出现顺序），优先使用 fixture 中对应的值；
     * 未提供则绑 NULL。
     */
    public void bind(PreparedStatement ps, ManifestStatement s) throws SQLException {
        int count = countPlaceholders(s.getSql());
        List<String> values = s.getId() == null ? null : fixtures.get(s.getId());
        for (int i = 1; i <= count; i++) {
            if (values != null && (i - 1) < values.size()) {
                String v = values.get(i - 1);
                if (v == null) {
                    ps.setNull(i, Types.NULL);
                } else {
                    ps.setObject(i, v);
                }
            } else {
                ps.setNull(i, Types.NULL);
            }
        }
    }

    /**
     * 判断指定 statement id 是否有 fixture（即使列表为空也算"有定义"）。
     * 用于在 Replayer 中产出"全绑 NULL"警告。
     */
    public boolean hasFixture(String statementId) {
        return statementId != null && fixtures.containsKey(statementId);
    }

    /**
     * 统计 SQL 中 {@code ?} 个数，忽略以下上下文中的 {@code ?}：
     * <ul>
     *   <li>单引号字符串字面量（{@code '...'}），转义的 {@code ''} 视为字面量内容</li>
     *   <li>行注释（{@code -- ...} 至行尾）</li>
     *   <li>块注释（{@code /* ... *&#47;}，支持嵌套）</li>
     *   <li>dollar-quoted 字符串（{@code $$...$$} / {@code $tag$...$tag$}）</li>
     * </ul>
     * 注意：PG 的 JSON 运算符 {@code ?} / {@code ?|} / {@code ?&} 在 JDBC 中需用 {@code ??} 转义，
     * 本方法把每个 {@code ?} 都计为占位符，调用方需确保 SQL 已按 JDBC 规范转义。
     */
    public static int countPlaceholders(String sql) {
        if (sql == null || sql.isEmpty()) {
            return 0;
        }
        int count = 0;
        int len = sql.length();
        int i = 0;
        while (i < len) {
            char ch = sql.charAt(i);

            // 单引号字符串
            if (ch == '\'') {
                i++;
                while (i < len) {
                    if (sql.charAt(i) == '\'') {
                        if (i + 1 < len && sql.charAt(i + 1) == '\'') {
                            i += 2; // 转义 ''
                        } else {
                            i++; // 闭合
                            break;
                        }
                    } else {
                        i++;
                    }
                }
                continue;
            }

            // 行注释 -- ... \n
            if (ch == '-' && i + 1 < len && sql.charAt(i + 1) == '-') {
                i += 2;
                while (i < len && sql.charAt(i) != '\n') {
                    i++;
                }
                continue;
            }

            // 块注释 /* ... */（支持嵌套）
            if (ch == '/' && i + 1 < len && sql.charAt(i + 1) == '*') {
                i += 2;
                int depth = 1;
                while (i < len && depth > 0) {
                    if (sql.charAt(i) == '/' && i + 1 < len && sql.charAt(i + 1) == '*') {
                        depth++;
                        i += 2;
                    } else if (sql.charAt(i) == '*' && i + 1 < len && sql.charAt(i + 1) == '/') {
                        depth--;
                        i += 2;
                    } else {
                        i++;
                    }
                }
                continue;
            }

            // dollar-quoted $$...$$ 或 $tag$...$tag$
            if (ch == '$') {
                int tagEnd = findDollarQuoteEnd(sql, i);
                if (tagEnd > 0) {
                    // tagEnd 是结束 $ 的位置
                    i = tagEnd + 1;
                    continue;
                }
                // 单个 $ 不是 dollar-quoted，按普通字符处理
            }

            // 占位符
            if (ch == '?') {
                count++;
            }
            i++;
        }
        return count;
    }

    /**
     * 查找 dollar-quoted 字符串的结束位置。
     *
     * @param start 起始 $ 的位置
     * @return 结束 $ 的位置（含），不是 dollar-quoted 返回 -1
     */
    private static int findDollarQuoteEnd(String sql, int start) {
        int len = sql.length();
        // 解析开标签：$ 或 $tag$
        int j = start + 1;
        StringBuilder tag = new StringBuilder("$");
        while (j < len) {
            char c = sql.charAt(j);
            if (c == '$') {
                tag.append('$');
                break;
            }
            if (Character.isLetterOrDigit(c) || c == '_') {
                tag.append(c);
                j++;
            } else {
                // 不是合法的 dollar-quoted 标签
                return -1;
            }
        }
        if (j >= len) {
            return -1;
        }
        String openTag = tag.toString();
        // j 现在指向开标签的结束 $
        int searchFrom = j + 1;
        int closeIdx = sql.indexOf(openTag, searchFrom);
        if (closeIdx < 0) {
            // 未闭合，消费到末尾
            return len - 1;
        }
        // closeIdx 指向闭标签的开始 $，结束位置是闭标签的最后一个 $
        return closeIdx + openTag.length() - 1;
    }
}
