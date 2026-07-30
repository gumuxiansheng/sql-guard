-- 测试固件：DDL 建表语句（含主键、外键、索引）
CREATE TABLE users (
    id INT PRIMARY KEY AUTO_INCREMENT,
    name VARCHAR(100) NOT NULL,
    email VARCHAR(255) UNIQUE,
    dept_id INT,
    CONSTRAINT fk_dept FOREIGN KEY (dept_id) REFERENCES departments(id),
    INDEX idx_name (name)
);

CREATE TABLE departments (
    id INT PRIMARY KEY,
    name VARCHAR(100) NOT NULL
);
