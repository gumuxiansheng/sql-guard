package com.sqlguard.replay.replay;

import org.junit.jupiter.api.Test;

import static org.junit.jupiter.api.Assertions.assertFalse;
import static org.junit.jupiter.api.Assertions.assertTrue;

/**
 * 验证 {@link Replayer#isCreateAsSelect(String)} 的 token 扫描器正确识别 CTAS，
 * 并避免原子串匹配在注释/字符串字面量上的误判。
 */
class CtasDetectionTest {

    @Test
    void plainCreateTableAsSelect() {
        assertTrue(Replayer.isCreateAsSelect(
                "CREATE TABLE t AS SELECT * FROM users"));
    }

    @Test
    void plainCreateViewAsSelect() {
        assertTrue(Replayer.isCreateAsSelect(
                "CREATE VIEW v AS SELECT id, name FROM users"));
    }

    @Test
    void caseInsensitive() {
        assertTrue(Replayer.isCreateAsSelect(
                "create table t as select * from users"));
        assertTrue(Replayer.isCreateAsSelect(
                "Create Table T As Select * From users"));
    }

    @Test
    void multilineWithNewlines() {
        assertTrue(Replayer.isCreateAsSelect(
                "CREATE TABLE t\nAS\nSELECT * FROM users"));
    }

    @Test
    void commentContainingAsSelectShouldNotMatch() {
        // 原 substring 实现会误匹配注释里的 "AS SELECT"
        assertFalse(Replayer.isCreateAsSelect(
                "CREATE TABLE t (id int); -- AS SELECT something"));
    }

    @Test
    void blockCommentContainingAsSelectShouldNotMatch() {
        assertFalse(Replayer.isCreateAsSelect(
                "CREATE TABLE t (id int) /* AS SELECT */"));
    }

    @Test
    void stringLiteralContainingAsSelectShouldNotMatch() {
        assertFalse(Replayer.isCreateAsSelect(
                "CREATE TABLE t (note varchar) " // 这里没有 AS SELECT
                        + "; SELECT 'AS SELECT' FROM t"));
    }

    @Test
    void asAliasWithoutSelectShouldNotMatch() {
        // 列别名 AS alias 不应触发 CTAS 误判
        assertFalse(Replayer.isCreateAsSelect(
                "CREATE TABLE t (id int); SELECT id AS alias FROM t"));
    }

    @Test
    void createIndexShouldNotMatch() {
        assertFalse(Replayer.isCreateAsSelect(
                "CREATE INDEX idx_name ON users(name)"));
    }

    @Test
    void createTableWithoutAsSelectShouldNotMatch() {
        assertFalse(Replayer.isCreateAsSelect(
                "CREATE TABLE t (id int, name varchar(100))"));
    }

    @Test
    void alterTableShouldNotMatch() {
        assertFalse(Replayer.isCreateAsSelect(
                "ALTER TABLE t ADD COLUMN c int"));
    }

    @Test
    void createTableWithCommentedAsSelectClause() {
        // 真正的 AS SELECT 被注释掉了，应不匹配
        assertFalse(Replayer.isCreateAsSelect(
                "CREATE TABLE t (id int) -- AS SELECT * FROM users\n"));
    }

    @Test
    void nestedBlockComment() {
        // 嵌套块注释中的 AS SELECT 应被跳过
        assertFalse(Replayer.isCreateAsSelect(
                "CREATE TABLE t (id int) /* outer /* AS SELECT */ */"));
        // 真实 AS SELECT 在嵌套注释外
        assertTrue(Replayer.isCreateAsSelect(
                "CREATE TABLE t (id int) /* outer /* inner */ */ AS SELECT * FROM t2"));
    }

    @Test
    void quotedIdentifierShouldNotMatch() {
        // PG 引用标识符 "AS SELECT" 不应被识别为关键字
        assertFalse(Replayer.isCreateAsSelect(
                "CREATE TABLE \"AS SELECT\" (id int)"));
    }

    @Test
    void emptyAndNull() {
        assertFalse(Replayer.isCreateAsSelect(null));
        assertFalse(Replayer.isCreateAsSelect(""));
        assertFalse(Replayer.isCreateAsSelect("   "));
    }

    @Test
    void escapedSingleQuoteInString() {
        // SQL 转义单引号 '' 不应结束字符串
        assertFalse(Replayer.isCreateAsSelect(
                "CREATE TABLE t (note varchar); SELECT 'it''s AS SELECT' FROM t"));
    }

    @Test
    void keywordBoundaryNotSubstring() {
        // INDEX 不应匹配 IN，BETWEEN 不应匹配 AS
        assertFalse(Replayer.isCreateAsSelect(
                "CREATE INDEX idx ON t(name)"));
        assertFalse(Replayer.isCreateAsSelect(
                "SELECT * FROM t WHERE id BETWEEN 1 AND 10"));
    }

    @Test
    void realCtasWithLeadingComment() {
        // 带行注释的真实 CTAS（AS SELECT 紧接）
        assertTrue(Replayer.isCreateAsSelect(
                "-- build mirror table\n"
                        + "CREATE TABLE t_mirror AS SELECT * FROM users"));
    }

    @Test
    void cteStyleCtasNotDetected() {
        // 已知限制：CREATE TABLE t AS WITH cte AS (...) SELECT ... 形式
        // AS 后紧接 WITH 而非 SELECT，扫描器（与原 substring 实现一致）不识别。
        // 这是与原实现行为一致的限制，非回归。
        assertFalse(Replayer.isCreateAsSelect(
                "CREATE TABLE t_mirror AS\n"
                        + "WITH recent AS (SELECT * FROM t) SELECT * FROM recent"));
    }
}
