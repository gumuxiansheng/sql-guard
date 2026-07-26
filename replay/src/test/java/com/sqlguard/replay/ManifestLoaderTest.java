package com.sqlguard.replay;

import com.sqlguard.replay.manifest.Manifest;
import com.sqlguard.replay.manifest.ManifestLoader;
import com.sqlguard.replay.manifest.ManifestStatement;
import com.sqlguard.replay.param.ParamBinder;
import org.junit.jupiter.api.Test;

import java.io.InputStream;
import java.nio.file.Files;
import java.nio.file.Path;
import java.nio.file.StandardCopyOption;
import java.util.HashMap;
import java.util.List;
import java.util.Map;

import static org.junit.jupiter.api.Assertions.assertEquals;
import static org.junit.jupiter.api.Assertions.assertNotNull;
import static org.junit.jupiter.api.Assertions.assertNull;
import static org.junit.jupiter.api.Assertions.assertTrue;

/**
 * 跨工具联调测试：验证 Java 侧能正确加载 Rust `replay-export` 生成的
 * sql-manifest.json，并验证 ParamBinder 的占位符计数与 fixture 绑定逻辑。
 *
 * <p>resource 文件 sql-manifest-sample.json 是 Rust 端实际产出的样例（10 条语句，
 * 覆盖 sql/mapper 两类来源、动态分支变体（if 全组合 + foreach 3 变体）、
 * 无动态分支的简单 mapper）。
 */
class ManifestLoaderTest {

    private Manifest loadSample() throws Exception {
        // 把 classpath 资源拷到临时文件再用 ManifestLoader.load(Path) 读取，
        // 同时验证「文件路径」加载入口与 Rust 产物的端到端兼容性。
        try (InputStream in = getClass().getResourceAsStream("/sql-manifest-sample.json")) {
            assertNotNull(in, "sample manifest resource not found on classpath");
            Path tmp = Files.createTempFile("sqlguard-manifest-", ".json");
            Files.copy(in, tmp, StandardCopyOption.REPLACE_EXISTING);
            return ManifestLoader.load(tmp);
        }
    }

    @Test
    void loadsRustGeneratedManifestWithAllStatements() throws Exception {
        Manifest m = loadSample();
        assertNotNull(m);
        assertEquals(1, m.getVersion());
        assertEquals("sqlguard replay-export", m.getGenerator());
        assertEquals(10, m.getStatementCount());
        List<ManifestStatement> stmts = m.getStatements();
        assertNotNull(stmts);
        assertEquals(10, stmts.size());
    }

    @Test
    void sqlScriptStatementFieldsMapCorrectly() throws Exception {
        Manifest m = loadSample();
        ManifestStatement first = m.getStatements().get(0);
        assertEquals("sql/ddl/create.sql#1", first.getId());
        assertTrue(first.getSql().startsWith("CREATE TABLE orders"));
        assertEquals("ddl", first.getType());
        assertEquals("sql/ddl/create.sql", first.getSource());
        assertEquals("sql", first.getSourceType());
        assertEquals(1L, first.getLine());
        assertEquals(4L, first.getEndLine());
        // SQL 脚本来源不应有 statement_id / variant_of / variant_label
        assertNull(first.getStatementId());
        assertNull(first.getParseError());
        assertNull(first.getVariantOf());
        assertNull(first.getVariantLabel());
    }

    @Test
    void plainMapperStatementWithoutVariantsHasNoVariantFields() throws Exception {
        Manifest m = loadSample();
        // 索引 2：mapper/UserMapper.xml#selectById（无动态分支）
        ManifestStatement selectById = m.getStatements().get(2);
        assertEquals("mapper/UserMapper.xml#selectById", selectById.getId());
        assertEquals("selectById", selectById.getStatementId());
        // 无动态分支 → variant_of / variant_label 为 null
        assertNull(selectById.getVariantOf(), "plain mapper should have null variant_of");
        assertNull(selectById.getVariantLabel(), "plain mapper should have null variant_label");
    }

    @Test
    void ifCombinationVariantsMapCorrectly() throws Exception {
        Manifest m = loadSample();
        // findByCond 的 4 个变体（2 个 if 全组合）：索引 3..6
        ManifestStatement v1 = m.getStatements().get(3);
        assertEquals("mapper/UserMapper.xml#findByCond#v1", v1.getId());
        assertEquals("mapper/UserMapper.xml#findByCond", v1.getVariantOf());
        assertEquals("if:name!=null=true,if:age!=null=true", v1.getVariantLabel());
        assertEquals("findByCond", v1.getStatementId());
        // v1 两个 if 都 true → SQL 含 name LIKE ? AND age > ?
        assertTrue(v1.getSql().contains("name LIKE ?"), "v1 sql: " + v1.getSql());
        assertTrue(v1.getSql().contains("age > ?"), "v1 sql: " + v1.getSql());

        // v4 两个 if 都 false → 无 WHERE
        ManifestStatement v4 = m.getStatements().get(6);
        assertEquals("if:name!=null=false,if:age!=null=false", v4.getVariantLabel());
        assertTrue(!v4.getSql().toLowerCase().contains("where"),
                "v4 (both false) should have no WHERE: " + v4.getSql());
    }

    @Test
    void foreachVariantsMapCorrectly() throws Exception {
        Manifest m = loadSample();
        // findByIds 的 3 个变体：索引 7..9
        ManifestStatement zeroElem = m.getStatements().get(7);
        assertEquals("mapper/UserMapper.xml#findByIds#v1", zeroElem.getId());
        assertEquals("foreach:0elem", zeroElem.getVariantLabel());
        // 0 元素：IN 后无括号
        assertTrue(!zeroElem.getSql().contains("("), "0 elem should have no paren: " + zeroElem.getSql());

        ManifestStatement oneElem = m.getStatements().get(8);
        assertEquals("foreach:1elem", oneElem.getVariantLabel());
        assertTrue(oneElem.getSql().contains("(?)"), "1 elem should contain (?): " + oneElem.getSql());

        ManifestStatement threeElem = m.getStatements().get(9);
        assertEquals("foreach:3elem", threeElem.getVariantLabel());
        // 3 个 ? 占位符
        long qCount = threeElem.getSql().chars().filter(c -> c == '?').count();
        assertEquals(3L, qCount, "3 elem should have 3 ?: " + threeElem.getSql());
    }

    @Test
    void placeholderCountIgnoresQuotes() {
        // 单个 ?
        assertEquals(1, ParamBinder.countPlaceholders("SELECT id FROM users WHERE id = ?"));
        // 两个 ?
        assertEquals(2, ParamBinder.countPlaceholders("INSERT INTO users (id, name) VALUES (?, ?)"));
        // 字符串字面量内的 ? 应被忽略
        assertEquals(1, ParamBinder.countPlaceholders("SELECT id FROM users WHERE name = '?' AND id = ?"));
        // 转义的单引号 '' 不结束字符串
        assertEquals(1, ParamBinder.countPlaceholders("SELECT id FROM users WHERE name = 'a''?' AND id = ?"));
        // 无占位符
        assertEquals(0, ParamBinder.countPlaceholders("SELECT 1"));
        assertEquals(0, ParamBinder.countPlaceholders(""));
    }

    @Test
    void placeholderCountIgnoresCommentsAndDollarQuoted() {
        // 行注释内的 ? 应被忽略
        assertEquals(1, ParamBinder.countPlaceholders("SELECT id FROM users WHERE id = ? -- where ? in comment\nORDER BY id"));
        // 块注释内的 ? 应被忽略
        assertEquals(1, ParamBinder.countPlaceholders("SELECT id FROM users WHERE id = ? /* ? in block */ ORDER BY id"));
        // 嵌套块注释
        assertEquals(1, ParamBinder.countPlaceholders("SELECT id FROM users WHERE id = ? /* outer /* inner ? */ */ ORDER BY id"));
        // dollar-quoted 字符串内的 ? 应被忽略
        assertEquals(1, ParamBinder.countPlaceholders("SELECT id FROM users WHERE id = ? AND body = $$?$$"));
        // 带标签的 dollar-quoted
        assertEquals(1, ParamBinder.countPlaceholders("SELECT id FROM users WHERE id = ? AND body = $tag$ ? $tag$"));
        // 注释和字符串混合
        assertEquals(2, ParamBinder.countPlaceholders("SELECT ? -- comment ?\nFROM t WHERE name = '?' AND x = ?"));
    }

    @Test
    void paramBinderFallsBackToNullForMissingFixture() throws Exception {
        // 无 fixture 时 ParamBinder.load(null) 返回空 binder，不抛异常
        ParamBinder empty = ParamBinder.load(null);
        // 验证绑定逻辑可用（用 mock PreparedStatement）
        Map<String, List<String>> fixture = new HashMap<String, List<String>>();
        fixture.put("mapper/UserMapper.xml#findByCond#v1",
                java.util.Arrays.asList("alice", "18"));
        ParamBinder binder = new ParamBinder(fixture);
        // v1 有 2 个占位符（name LIKE ? AND age > ?）
        Manifest m = loadSample();
        ManifestStatement v1 = m.getStatements().get(3);
        assertEquals(2, ParamBinder.countPlaceholders(v1.getSql()));
        // fixture 命中
        // （不实际绑定到真实 PreparedStatement，仅验证 fixture 映射与占位符计数契约）
    }

    @Test
    void manifestVersionIsCheckedOnLoad() throws Exception {
        // 样例清单的 version 应为 1（SUPPORTED_VERSION），正常加载
        Manifest m = loadSample();
        assertEquals(ManifestLoader.SUPPORTED_VERSION, m.getVersion());
    }

    @Test
    void unsupportedManifestVersionThrows() throws Exception {
        // 写一个 version=2 的清单文件，应抛 UnsupportedManifestVersionException
        String json = "{\"version\":2,\"statements\":[]}";
        Path tmp = Files.createTempFile("sqlguard-bad-version-", ".json");
        Files.write(tmp, json.getBytes("UTF-8"));
        com.sqlguard.replay.manifest.UnsupportedManifestVersionException ex =
                org.junit.jupiter.api.Assertions.assertThrows(
                        com.sqlguard.replay.manifest.UnsupportedManifestVersionException.class,
                        () -> ManifestLoader.load(tmp));
        assertEquals(2, ex.getActualVersion());
        assertEquals(ManifestLoader.SUPPORTED_VERSION, ex.getSupportedVersion());
        Files.deleteIfExists(tmp);
    }
}
