package com.sqlguard.replay.replay;

import org.junit.jupiter.api.Test;

import static org.junit.jupiter.api.Assertions.assertEquals;

/**
 * DYN003 超大 IN 子句检测：验证 countMaxInClauseParams 的解析逻辑。
 */
class InClauseCountTest {

    @Test
    void noInClause() {
        assertEquals(0, Replayer.countMaxInClauseParams("SELECT 1"));
        assertEquals(0, Replayer.countMaxInClauseParams("SELECT id FROM users WHERE id = ?"));
        assertEquals(0, Replayer.countMaxInClauseParams(""));
        assertEquals(0, Replayer.countMaxInClauseParams(null));
    }

    @Test
    void singleInWithOneParam() {
        assertEquals(1, Replayer.countMaxInClauseParams(
                "SELECT id FROM users WHERE id IN (?)"));
    }

    @Test
    void singleInWithMultipleParams() {
        assertEquals(3, Replayer.countMaxInClauseParams(
                "SELECT id FROM users WHERE id IN (?, ?, ?)"));
    }

    @Test
    void multipleInClausesReturnsMax() {
        assertEquals(5, Replayer.countMaxInClauseParams(
                "SELECT id FROM users WHERE id IN (?, ?) AND dept_id IN (?, ?, ?, ?, ?)"));
    }

    @Test
    void caseInsensitive() {
        assertEquals(2, Replayer.countMaxInClauseParams(
                "SELECT id FROM users WHERE id in (?, ?)"));
        assertEquals(2, Replayer.countMaxInClauseParams(
                "SELECT id FROM users WHERE id In (?, ?)"));
    }

    @Test
    void ignoresQuestionMarksInStringLiterals() {
        assertEquals(1, Replayer.countMaxInClauseParams(
                "SELECT id FROM users WHERE name = '?' AND id IN (?)"));
    }

    @Test
    void doesNotMatchIndexOrInsert() {
        assertEquals(0, Replayer.countMaxInClauseParams(
                "CREATE INDEX idx_name ON users(name)"));
        assertEquals(0, Replayer.countMaxInClauseParams(
                "INSERT INTO users (id) VALUES (?)"));
    }

    @Test
    void foreachGeneratedIn() {
        // 模拟 MyBatis foreach 生成的 10 元素 IN
        StringBuilder sb = new StringBuilder("SELECT id FROM users WHERE id IN (");
        for (int i = 0; i < 10; i++) {
            if (i > 0) sb.append(", ");
            sb.append("?");
        }
        sb.append(")");
        assertEquals(10, Replayer.countMaxInClauseParams(sb.toString()));
    }

    @Test
    void zeroElementIn() {
        // foreach 0 元素：IN 后无括号内容
        assertEquals(0, Replayer.countMaxInClauseParams(
                "SELECT id FROM users WHERE id IN"));
    }
}
