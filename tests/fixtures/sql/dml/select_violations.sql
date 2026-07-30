-- 测试固件：DML 违规语句集合
-- DML001: SELECT *
SELECT * FROM users;

-- DML005: 未限定列（多表 JOIN）
SELECT id, name FROM users u JOIN orders o ON u.id = o.user_id;

-- DML007: LIMIT 无 ORDER BY
SELECT id, name FROM users LIMIT 10;

-- DML106: 普通 UNION（应使用 UNION ALL）
SELECT id FROM a UNION SELECT id FROM b;

-- 合规查询
SELECT u.id, u.name FROM users u WHERE u.id = 1 ORDER BY u.id LIMIT 10;
