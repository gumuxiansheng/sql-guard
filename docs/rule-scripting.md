# SqlGuard 瑙勫垯鑴氭湰缂栧啓鎵嬪唽

## 1. 姒傝堪

SqlGuard 浣跨敤 [Rhai](https://rhai.rs/) 鑴氭湰璇█缂栧啓鑷畾涔夎鍒欍€傛瘡鏉¤鍒欐槸涓€涓?`.rhai` 鏂囦欢锛岀敱寮曟搸閽堝姣忎釜 SQL 鏂囦欢鐙珛鎵ц涓€娆°€傝鍒欒剼鏈€氳繃閬嶅巻棰勮В鏋愮殑 AST 鏉ユ娴嬭繚瑙勶紝骞跺皢缁撴灉鍐欏叆 `violations` 鏁扮粍銆?
瑙勫垯鑴氭湰鍏峰浠ヤ笅鑳藉姏锛?- 璇诲彇 SQL 鍘熸枃銆佹枃浠惰矾寰勩€佽剼鏈被鍨嬬瓑涓婁笅鏂囦俊鎭?- 璁块棶鐢?sqlparser 瑙ｆ瀽鍚庣殑绠€鍖?AST
- 浣跨敤瀛楃涓插尮閰嶄綔涓哄厹搴曪紙涓嶆帹鑽愶紝AST 鏇寸簿纭級
- 涓婃姤澶氭潯杩濊锛屽寘鍚鍙枫€佸垪鍙?
## 2. 鎵ц妯″瀷

鍙傝€?[`src/rule/engine/runner.rs`](../src/rule/engine/runner.rs) 涓殑 `run_single_rule`锛?
1. 寮曟搸璇诲彇 `.rhai` 鑴氭湰鍐呭
2. **prepend `config/rules/lib/helpers.rhai`**锛堢紪璇戞椂閫氳繃 `include_str!` 宓屽叆浜岃繘鍒讹級锛屾敞鍏?5 涓叕鍏辫緟鍔╁嚱鏁帮細`guard_parse_error`銆乣parse_error_violation`銆乣violation`銆乣violation_line`銆乣violation_msg`
3. 鏋勯€犱竴涓?`Scope`锛屾敞鍏ヤ袱涓叏灞€鍙橀噺锛?   - `context` 鈥斺€?涓€涓?Map锛屽寘鍚?SQL 涓?AST 淇℃伅
   - `violations` 鈥斺€?涓€涓┖ Array锛岀敤浜庢敹闆嗚繚瑙?4. 鎵ц鎷兼帴鍚庣殑瀹屾暣鑴氭湰
5. 璇诲彇 `violations` 鏁扮粍锛屾瘡椤硅浆鎹负 `Violation` 缁撴瀯涓婃姤

> 鈿狅笍 **Rhai 鍊艰涔夋敞鎰?*锛歊hai 鏄€艰涔夎瑷€锛屽嚱鏁板唴瀵?Array 鍙傛暟鐨?push 涓嶄細鍐欏洖璋冪敤鑰呭彉閲忋€傚洜姝ゆ墍鏈?helper 鍑芥暟鍙繑鍥炲€硷紝`violations.push(...)` 蹇呴』鍦ㄨ剼鏈《灞傛墽琛屻€傝瑙?[`helpers.rhai`](../config/rules/lib/helpers.rhai) 涓殑娉ㄩ噴銆?
## 3. 鍏ㄥ眬鍙橀噺

### 3.1 `context`锛圡ap锛?
| 瀛楁 | 绫诲瀷 | 璇存槑 |
|------|------|------|
| `sql_content` | String | SQL 鍘熸枃锛堝彲鐢ㄤ簬娉ㄩ噴妫€鏌ョ瓑 AST 鏃犳硶瑕嗙洊鐨勫満鏅級 |
| `file_path` | String | 鏂囦欢缁濆璺緞 |
| `file_name` | String | 鏂囦欢鍚嶏紙濡?`create_users.sql`锛?|
| `script_type` | String | 鍒嗙被鍚庣殑鑴氭湰绫诲瀷锛堝 `ddl`銆乣dml`銆乣other`锛夛紝鐢?`sqlguard.toml` 涓?`[[classification.rules]]` 鍐冲畾 |
| `line_count` | INT | SQL 鏂囦欢鎬昏鏁?|
| `ast` | SqlAst | 瑙ｆ瀽鍚庣殑 AST锛岃涓嬫枃 |

璁块棶鏂瑰紡锛歚context["ast"]`銆乣context["sql_content"]` 绛夈€?
### 3.2 `violations`锛圓rray锛?
鑴氭湰閫氳繃 `violations.push(...)` 涓婃姤杩濊銆傛帹鑽愪娇鐢ㄥ唴缃?helper 鍑芥暟鏋勫缓 violation锛?
```rhai
// 甯﹁鍒楀彿锛堟帹鑽愶級
violations.push(violation("message", 10, 1));

// 浠呭甫琛屽彿
violations.push(violation_line("message", 10));

// 绾秷鎭紙鏃犺鍒楀彿锛?violations.push(violation_msg("some message"));

// 鍘熷 Map 瀛楅潰閲忥紙搴曞眰褰㈠紡锛屼笉闇€ helper锛?violations.push(#{
    "message": "violation message",
    "line": 10,
    "column": 1
});
```

`line`銆乣column` 鍧囦负鏁存暟锛屽彲鐪佺暐锛堢己鐪佹椂涓?`None`锛夈€俙severity` 涓?`rule_name` 鏉ヨ嚜 `sqlguard.toml`锛岃剼鏈棤闇€鎸囧畾銆?
## 4. AST API

AST 鍖呰绫诲瀷瀹氫箟浜?[`src/rule/engine/ast.rs`](../src/rule/engine/ast.rs)銆傛墍鏈夋柟娉曞潎浠?`&mut self` 娉ㄥ唽锛岃皟鐢ㄥ舰寮忎负 `obj.method()`銆?
### 4.1 `SqlAst`锛堥€氳繃 `context["ast"]` 鑾峰彇锛?
| 鏂规硶 | 杩斿洖绫诲瀷 | 璇存槑 |
|------|----------|------|
| `statements()` | Array<StmtInfo> | 鍏ㄩ儴璇彞鍒楄〃 |
| `has_parse_error()` | bool | 鏄惁瑙ｆ瀽澶辫触 |
| `parse_error()` | String | 瑙ｆ瀽閿欒淇℃伅锛堟棤閿欐椂涓虹┖涓诧級 |
| `kinds()` | Array<String> | 鎵€鏈夎鍙ョ殑 kind 鍒楄〃 |
| `create_tables()` | Array<CreateInfo> | 鍏ㄩ儴 CREATE TABLE 淇℃伅 |
| `drop_objects()` | Array<DropInfo> | 鍏ㄩ儴 DROP 淇℃伅 |
| `selects()` | Array<SelectInfo> | 鍏ㄩ儴 SELECT 淇℃伅 |
| `has_create_table()` | bool | 鏄惁鍚?CREATE TABLE |
| `has_drop_table()` | bool | 鏄惁鍚?DROP TABLE |
| `has_alter_table()` | bool | 鏄惁鍚?ALTER TABLE锛堜笖瑙ｆ瀽鍑鸿鎯咃級 |
| `has_comma_join_anywhere()` | bool | 鏁翠釜 SQL 鏂囨湰涓槸鍚︽娴嬪埌 `FROM a, b` 褰㈠紡鐨勯殣寮忛€楀彿 JOIN锛堟枃鏈壂鎻忥紝璺ㄨ鍙ョ敓鏁堬級 |
| `comments()` | Array<CommentInfo> | 婧愭枃鏈腑鐨勬墍鏈夋敞閲婏紙琛屾敞閲?`--` 涓庡潡娉ㄩ噴 `/* */`锛宻qlparser 瑙ｆ瀽鏃朵涪寮冿紝杩欓噷閫氳繃鐙珛鏂囨湰鎵弿寰楀埌锛?|

### 4.2 `StmtInfo`

| 鏂规硶 | 杩斿洖绫诲瀷 | 璇存槑 |
|------|----------|------|
| `kind()` | String | 璇彞绫诲瀷锛岃涓嬭〃 |
| `line()` | INT | 璧峰琛屽彿 |
| `end_line()` | INT | 璇彞缁撴潫琛岋紙鍚級锛岀敤浜庡垽鏂鍙ヨ竟鐣?|
| `line_range()` | (INT, INT) | 杩斿洖 `(start_line, end_line)` 鍏冪粍 |
| `column()` | INT | 璧峰鍒楀彿 |
| `has_create_table()` | bool | 鏄惁涓?CREATE TABLE |
| `has_drop_object()` | bool | 鏄惁涓?DROP |
| `has_select()` | bool | 鏄惁涓?SELECT |
| `has_insert()` | bool | 鏄惁涓?INSERT |
| `has_update()` | bool | 鏄惁涓?UPDATE |
| `has_delete()` | bool | 鏄惁涓?DELETE |
| `has_alter_table()` | bool | 鏄惁涓?ALTER TABLE锛堜笖瑙ｆ瀽鍑鸿鎯咃級 |
| `has_truncate()` | bool | 鏄惁涓?TRUNCATE |
| `has_create_view()` | bool | 鏄惁涓?CREATE VIEW |
| `has_create_index()` | bool | 鏄惁涓?CREATE INDEX |
| `has_transaction()` | bool | 鏄惁涓轰簨鍔¤鍙ワ紙START TRANSACTION / COMMIT / ROLLBACK锛?|
| `create_table()` | CreateInfo | CREATE TABLE 璇︽儏锛堥潪 CREATE TABLE 鏃惰繑鍥為粯璁ょ┖瀵硅薄锛?|
| `drop_object()` | DropInfo | DROP 璇︽儏 |
| `select()` | SelectInfo | SELECT 璇︽儏 |
| `insert()` | InsertInfo | INSERT 璇︽儏 |
| `update()` | UpdateInfo | UPDATE 璇︽儏 |
| `delete()` | DeleteInfo | DELETE 璇︽儏 |
| `alter_table()` | AlterTableInfo | ALTER TABLE 璇︽儏锛堥潪 ALTER TABLE 鏃惰繑鍥為粯璁ょ┖瀵硅薄锛?|
| `truncate()` | TruncateInfo | TRUNCATE 璇︽儏 |
| `create_view()` | ViewInfo | CREATE VIEW 璇︽儏 |
| `create_index()` | CreateIndexInfo | CREATE INDEX 璇︽儏 |
| `transaction()` | TransactionInfo | 浜嬪姟璇彞璇︽儏 |

**鏀寔鐨?`kind` 鍊?*锛?
| kind | SQL 璇彞 |
|------|----------|
| `CREATE_TABLE` | `CREATE TABLE` |
| `DROP_TABLE` | `DROP TABLE` |
| `DROP_VIEW` | `DROP VIEW` |
| `DROP_INDEX` | `DROP INDEX` |
| 鍏朵粬 `DROP_<TYPE>` | 鍏朵粬 DROP 瀵硅薄绫诲瀷锛圫CHEMA / ROLE / SEQUENCE / STAGE锛?|
| `SELECT` | `SELECT` / `Query` |
| `INSERT` | `INSERT` |
| `UPDATE` | `UPDATE` |
| `DELETE` | `DELETE` |
| `ALTER_TABLE` | `ALTER TABLE` |
| `TRUNCATE` | `TRUNCATE [TABLE]` |
| `CREATE_VIEW` | `CREATE [OR REPLACE] [MATERIALIZED] VIEW` |
| `CREATE_INDEX` | `CREATE [UNIQUE] INDEX` |
| `START_TRANSACTION` | `START TRANSACTION` / `BEGIN` |
| `COMMIT` | `COMMIT` |
| `ROLLBACK` | `ROLLBACK` |
| `GRANT` | `GRANT` |
| `REVOKE` | `REVOKE` |
| `MERGE` | `MERGE`锛坲psert锛宻qlparser 瑙ｆ瀽鏈夐檺锛?|
| `SET_VARIABLE` | `SET` |
| `USE` | `USE` |
| `OTHER` | 鏈瘑鍒殑璇彞 |

### 4.3 `CreateInfo`

| 鏂规硶 | 杩斿洖绫诲瀷 | 璇存槑 |
|------|----------|------|
| `table_name()` | String | 琛ㄥ悕 |
| `columns()` | Array<ColumnInfo> | 鍒楀畾涔夊垪琛?|
| `has_primary_key()` | bool | 鏄惁鍚富閿紙鍒楃骇鎴栬〃绾х害鏉燂級 |
| `if_not_exists()` | bool | 鏄惁甯?`IF NOT EXISTS` |
| `column_names()` | Array<String> | 鎵€鏈夊垪鍚?|
| `foreign_keys()` | Array<ForeignKeyInfo> | 琛ㄧ骇 + 鍒楃骇澶栭敭绾︽潫鍚堝苟鍒楄〃 |
| `checks()` | Array<CheckInfo> | 琛ㄧ骇 + 鍒楃骇 CHECK 绾︽潫鍚堝苟鍒楄〃 |
| `indexes()` | Array<IndexInfo> | 琛ㄧ骇 INDEX / KEY 绾︽潫鍒楄〃锛圡ySQL 椋庢牸锛?|
| `uniques()` | Array<UniqueInfo> | 琛ㄧ骇 UNIQUE 绾︽潫鍒楄〃锛堜笉鍚富閿級 |
| `has_foreign_key()` | bool | 鏄惁鍚换鎰忓閿?|
| `has_check()` | bool | 鏄惁鍚换鎰?CHECK 绾︽潫 |
| `has_index()` | bool | 鏄惁鍚换鎰?INDEX 绾︽潫 |
| `has_unique()` | bool | 鏄惁鍚换鎰?UNIQUE 绾︽潫 |

### 4.4 `ColumnInfo`

| 鏂规硶 | 杩斿洖绫诲瀷 | 璇存槑 |
|------|----------|------|
| `name()` | String | 鍒楀悕 |
| `data_type()` | String | 鏁版嵁绫诲瀷 |
| `is_primary_key()` | bool | 鏄惁涓哄垪绾?PRIMARY KEY |
| `is_not_null()` | bool | 鏄惁 NOT NULL |
| `is_unique()` | bool | 鏄惁 UNIQUE |
| `default_value()` | String | DEFAULT 琛ㄨ揪寮忔枃鏈紙鏃犲垯绌轰覆锛?|
| `is_auto_increment()` | bool | 鏄惁 AUTO_INCREMENT / AUTOINCREMENT |
| `comment()` | String | 鍒?COMMENT 鏂囨湰锛堟棤鍒欑┖涓诧級 |
| `has_check()` | bool | 鏄惁鍚垪绾?CHECK 绾︽潫 |
| `references_table()` | String | 鍒楃骇澶栭敭寮曠敤鐨勮〃鍚嶏紙鏃犲垯绌轰覆锛?|
| `has_foreign_key()` | bool | 鏄惁鍚垪绾?REFERENCES 澶栭敭 |

### 4.5 `DropInfo`

| 鏂规硶 | 杩斿洖绫诲瀷 | 璇存槑 |
|------|----------|------|
| `object_type()` | String | 瀵硅薄绫诲瀷锛坄TABLE`銆乣VIEW` 绛夛級 |
| `name()` | String | 瀵硅薄鍚嶏紙澶氫釜鏃朵互 `, ` 杩炴帴锛?|
| `if_exists()` | bool | 鏄惁甯?`IF EXISTS` |

### 4.5b `AlterTableInfo`

浠呭綋璇彞涓?`ALTER TABLE` 鏃剁敱 `s.alter_table()` 杩斿洖闈炵┖璇︽儏锛岀敤浜庡垽鏂〃鏄惁閫氳繃 ALTER 澧炲垹涓婚敭锛屼互鍙婄粨鏋勫寲閬嶅巻鎵€鏈?ALTER 鎿嶄綔銆?
| 鏂规硶 | 杩斿洖绫诲瀷 | 璇存槑 |
|------|----------|------|
| `table_name()` | String | 琚慨鏀圭殑琛ㄥ悕 |
| `adds_primary_key()` | bool | 鏄惁鍖呭惈澧炲姞涓婚敭鐨勬搷浣滐細`ADD PRIMARY KEY (...)`銆乣ADD CONSTRAINT x PRIMARY KEY (...)` 鎴?`ADD COLUMN col ... PRIMARY KEY` |
| `drops_primary_key()` | bool | 鏄惁鍖呭惈 `DROP PRIMARY KEY`锛堢Щ闄や富閿級 |
| `operations()` | Array<AlterOpInfo> | 鎵€鏈?ALTER 鎿嶄綔鐨勭粨鏋勫寲鍒楄〃锛堝惈 ADD/DROP COLUMN銆丷ENAME銆丄DD/DROP CONSTRAINT 绛夛級 |

### 4.6 `SelectInfo`

| 鏂规硶 | 杩斿洖绫诲瀷 | 璇存槑 |
|------|----------|------|
| `has_wildcard()` | bool | 鏄惁鍚?`*` 鎴?`table.*`锛堥€掑綊瑕嗙洊椤跺眰銆乁NION/闆嗗悎杩愮畻鍒嗘敮銆丗ROM/JOIN 瀛愭煡璇級 |
| `projection()` | Array<String> | 椤跺眰鎶曞奖椤规枃鏈〃绀?|
| `has_from_table()` | bool | 鏄惁鍚?FROM 琛?|
| `from_table()` | String | 绗竴寮?FROM 琛ㄥ悕锛堟棤鍒欑┖涓诧級 |
| `from_table_alias()` | String | 涓昏〃鍒悕锛坄FROM users u` 涓殑 `u`锛屾棤鍒欑┖涓诧級 |
| `has_from_table_alias()` | bool | 涓昏〃鏄惁甯﹀埆鍚?|
| `joins()` | Array<JoinInfo> | JOIN 瀛愬彞鍒楄〃 |
| `has_joins()` | bool | 鏄惁鏈?JOIN |
| `has_cross_join()` | bool | 鏄惁鍚?CROSS JOIN |
| `has_join_without_condition()` | bool | 鏄惁瀛樺湪鏃?ON/USING 鏉′欢鐨?JOIN锛圕ROSS JOIN 闄ゅ锛?|
| `has_subquery_in_from()` | bool | FROM 鏄惁鍚淳鐢熻〃锛堝瓙鏌ヨ锛?|
| `from_subquery_has_alias()` | bool | FROM 娲剧敓琛ㄦ槸鍚﹂兘甯﹀埆鍚?|
| `has_unqualified_column()` | bool | 澶氳〃鏌ヨ涓槸鍚﹀瓨鍦ㄦ湭闄愬畾鐨?*瑁稿垪**寮曠敤锛堜笉鍚嚱鏁?瀛楅潰閲忥級 |
| `is_union()` | bool | 鏄惁涓?UNION 闆嗗悎杩愮畻锛堜笉鍚?INTERSECT/EXCEPT锛?|
| `is_union_all()` | bool | UNION 鏄惁甯?`ALL`锛堝惈 `ALL BY NAME`锛?|
| `is_intersect()` | bool | 鏄惁涓?INTERSECT 闆嗗悎杩愮畻 |
| `is_except()` | bool | 鏄惁涓?EXCEPT 闆嗗悎杩愮畻 |
| `has_where()` | bool | 鏄惁鍚?WHERE 瀛愬彞 |
| `where_clause()` | String | WHERE 琛ㄨ揪寮忔枃鏈紙鏃犲垯绌轰覆锛?|
| `has_group_by()` | bool | 鏄惁鍚?GROUP BY 瀛愬彞 |
| `has_having()` | bool | 鏄惁鍚?HAVING 瀛愬彞 |
| `has_qualify()` | bool | 鏄惁鍚?QUALIFY 瀛愬彞锛圫nowflake 绛夋柟瑷€锛?|
| `has_order_by()` | bool | 鏄惁鍚?ORDER BY锛堝湪 Query 灞傦紝闆嗗悎杩愮畻涔熻兘鏌ュ埌锛?|
| `has_limit()` | bool | 鏄惁鍚?LIMIT |
| `has_offset()` | bool | 鏄惁鍚?OFFSET |
| `has_fetch()` | bool | 鏄惁鍚?FETCH锛圫QL:2008 椋庢牸鍒嗛〉锛?|
| `has_distinct()` | bool | 鏄惁鍚?DISTINCT |
| `ctes()` | Array<CteInfo> | WITH 瀛愬彞涓殑 CTE 鍒楄〃 |
| `has_cte()` | bool | 鏄惁鍚?WITH 瀛愬彞 |
| `is_recursive()` | bool | WITH 鏄惁甯?RECURSIVE 鍏抽敭瀛?|
| `subqueries()` | Array<SelectInfo> | 閫掑綊宓屽鐨勫瓙鏌ヨ鍒楄〃锛團ROM 娲剧敓琛?/ WHERE 鏍囬噺瀛愭煡璇?/ EXISTS / 闆嗗悎杩愮畻鎷彿鍒嗘敮锛?|
| `has_subquery()` | bool | 鏄惁鍚换鎰忓瓙鏌ヨ |
| `has_window_function()` | bool | 鎶曞奖/WHERE/HAVING 涓槸鍚﹀惈绐楀彛鍑芥暟锛坄OVER (...)`锛?|
| `window_functions()` | Array<WindowFuncInfo> | 绐楀彛鍑芥暟璇︽儏鍒楄〃 |
| `where_expr()` | ExprInfo | WHERE 琛ㄨ揪寮忛《灞備俊鎭紙鏃犲垯杩斿洖榛樿绌哄璞★級 |
| `has_where_expr()` | bool | WHERE 琛ㄨ揪寮忔槸鍚﹀凡瑙ｆ瀽 |
| `having_expr()` | ExprInfo | HAVING 琛ㄨ揪寮忛《灞備俊鎭?|
| `has_having_expr()` | bool | HAVING 琛ㄨ揪寮忔槸鍚﹀凡瑙ｆ瀽 |
| `projection_exprs()` | Array<ExprInfo> | 鎶曞奖椤圭殑琛ㄨ揪寮忎俊鎭垪琛?|
| `has_comma_join()` | bool | 璇?SELECT 璇彞鑼冨洿鍐呮槸鍚︽娴嬪埌 `FROM a, b` 褰㈠紡闅愬紡閫楀彿 JOIN |

### 4.6b `JoinInfo`

| 鏂规硶 | 杩斿洖绫诲瀷 | 璇存槑 |
|------|----------|------|
| `table_name()` | String | JOIN 鐨勭洰鏍囪〃鍚嶏紙涓嶅惈鍒悕锛?|
| `join_type()` | String | `INNER` / `LEFT` / `RIGHT` / `FULL` / `CROSS` / `OTHER` |
| `has_condition()` | bool | 鏄惁甯?ON/USING 鏉′欢锛圕ROSS JOIN 鎭掍负 true锛?|
| `alias()` | String | JOIN 鐩爣琛ㄥ埆鍚嶏紙`JOIN orders o` 涓殑 `o`锛屾棤鍒欑┖涓诧級 |
| `has_alias()` | bool | JOIN 鐩爣琛ㄦ槸鍚﹀甫鍒悕 |
| `condition_text()` | String | ON 瀛愬彞琛ㄨ揪寮忔枃鏈紙鏃犲垯绌轰覆锛?|
| `has_condition_text()` | bool | 鏄惁鏈?ON 瀛愬彞鏂囨湰 |

### 4.6c `InsertInfo` / `UpdateInfo` / `DeleteInfo`

| 鏂规硶 | 杩斿洖绫诲瀷 | 璇存槑 |
|------|----------|------|
| `InsertInfo.table_name()` | String | INSERT 鐩爣琛ㄥ悕 |
| `InsertInfo.columns()` | Array<String> | 鏄惧紡鎸囧畾鐨勫垪鍚嶅垪琛?|
| `InsertInfo.has_columns()` | bool | 鏄惁鏄惧紡鎸囧畾鍒?|
| `UpdateInfo.table_name()` | String | UPDATE 鐩爣琛ㄥ悕 |
| `UpdateInfo.has_where()` | bool | 鏄惁鍚?WHERE |
| `UpdateInfo.where_clause()` | String | WHERE 琛ㄨ揪寮忔枃鏈?|
| `DeleteInfo.table_name()` | String | DELETE 鐩爣琛ㄥ悕 |
| `DeleteInfo.has_where()` | bool | 鏄惁鍚?WHERE |
| `DeleteInfo.where_clause()` | String | WHERE 琛ㄨ揪寮忔枃鏈?|

## 5. 鏂板 Info 绫诲瀷锛圥1-P3 鑳藉姏鎵╁睍锛?
浠ヤ笅 Info 绫诲瀷鐢?[`src/rule/engine/ast.rs`](../src/rule/engine/ast.rs) 鍜?[`analyzer.rs`](../src/rule/engine/analyzer.rs) 鎻愪緵锛屽搴?CTE / 绐楀彛鍑芥暟 / 琛ㄨ揪寮忔爲 / 绾︽潫 / 浜嬪姟 / 瑙嗗浘 / 绱㈠紩 / 娉ㄩ噴绛?AST 鑳藉姏銆傛墍鏈夌粨鏋勪綋鍧?`Clone + Default`锛屾柟娉曚互 `&mut self` 娉ㄥ唽鍒?Rhai 寮曟搸銆?
### 5.1 `CteInfo`锛圕TE / WITH 瀛愬彞锛?
鐢?`SelectInfo.ctes()` 杩斿洖銆?
| 鏂规硶 | 杩斿洖绫诲瀷 | 璇存槑 |
|------|----------|------|
| `name()` | String | CTE 鍚嶇О |
| `column_count()` | INT | CTE 鏄惧紡鍒楁暟锛坄WITH x(a, b) AS (...)` 涓殑 2锛涙湭鏄惧紡鍒楀嚭鏃朵负 0锛?|
| `is_recursive()` | bool | 璇?CTE 鏄惁澶勪簬 `WITH RECURSIVE` 涓婁笅鏂?|

### 5.2 `WindowFuncInfo`锛堢獥鍙ｅ嚱鏁帮級

鐢?`SelectInfo.window_functions()` 杩斿洖銆傝瘑鍒?`Expr::Function { over: Some(_), .. }`銆?
| 鏂规硶 | 杩斿洖绫诲瀷 | 璇存槑 |
|------|----------|------|
| `function_name()` | String | 绐楀彛鍑芥暟鍚嶏紙濡?`ROW_NUMBER` / `SUM` / `RANK`锛?|
| `has_partition_by()` | bool | 鏄惁甯?`PARTITION BY` |
| `has_order_by()` | bool | 鏄惁甯︾獥鍙ｅ唴 `ORDER BY` |
| `has_window_frame()` | bool | 鏄惁甯︾獥鍙ｅ抚锛坄ROWS` / `RANGE` / `GROUPS BETWEEN ...`锛?|

### 5.3 `ExprInfo`锛堣〃杈惧紡椤跺眰淇℃伅锛?
鐢?`SelectInfo.where_expr()` / `having_expr()` / `projection_exprs()` 杩斿洖銆?*涓嶉€掑綊鏆撮湶瀛愯〃杈惧紡**锛堥伩鍏嶇被鍨嬬垎鐐革級锛屽彧鏆撮湶椤跺眰鍒ゅ畾銆?
| 鏂规硶 | 杩斿洖绫诲瀷 | 璇存槑 |
|------|----------|------|
| `kind()` | String | 琛ㄨ揪寮忕被鍨嬶紝鍙栧€艰涓嬭〃 |
| `text()` | String | 琛ㄨ揪寮忔枃鏈〃绀?|
| `function_name()` | String | 鍑芥暟鍚嶏紙浠?`FUNCTION` 绫诲瀷鏈夋晥锛?|
| `is_literal()` | bool | 鏄惁涓哄瓧闈㈤噺 |
| `is_column()` | bool | 鏄惁涓哄垪寮曠敤锛堝惈澶嶅悎 `t.c`锛?|
| `is_subquery()` | bool | 鏄惁鍚瓙鏌ヨ锛坄SUBQUERY` / `EXISTS`锛?|
| `operator()` | String | 浜屽厓/涓€鍏冭繍绠楃锛堝 `=` / `AND` / `NOT`锛?|
| `column_name()` | String | 鍒楀悕锛堜粎 `IDENTIFIER` / `COMPOUND_IDENTIFIER` 鏈夋晥锛?|
| `has_null_test()` | bool | 鏄惁涓?`IS NULL` / `IS NOT NULL` 鍒ゅ畾 |

`kind` 鍙栧€硷細`IDENTIFIER / COMPOUND_IDENTIFIER / LITERAL / NULL / BINARY_OP / UNARY_OP / FUNCTION / CASE / SUBQUERY / EXISTS / IN_LIST / BETWEEN / CAST / IS_NULL / IS_NOT_NULL / OTHER`銆?
### 5.4 `ForeignKeyInfo`锛堝閿害鏉燂級

鐢?`CreateInfo.foreign_keys()` 杩斿洖锛屽悎骞惰〃绾?`FOREIGN KEY (...) REFERENCES ...` 涓庡垪绾?`REFERENCES ...`銆?
| 鏂规硶 | 杩斿洖绫诲瀷 | 璇存槑 |
|------|----------|------|
| `name()` | String | 绾︽潫鍚嶏紙`CONSTRAINT x` 涓殑 `x`锛屾棤鍒欑┖涓诧級 |
| `columns()` | Array<String> | 鏈〃澶栭敭鍒楀悕 |
| `foreign_table()` | String | 寮曠敤鐨勭洰鏍囪〃鍚?|
| `referred_columns()` | Array<String> | 鐩爣琛ㄧ殑寮曠敤鍒楀悕 |
| `on_delete()` | String | ON DELETE 鍔ㄤ綔锛坄CASCADE` / `SET NULL` / `RESTRICT` / `NO ACTION` / `SET DEFAULT`锛屾棤鍒欑┖涓诧級 |
| `on_update()` | String | ON UPDATE 鍔ㄤ綔锛堝悓涓婏級 |

### 5.5 `CheckInfo`锛圕HECK 绾︽潫锛?
鐢?`CreateInfo.checks()` 杩斿洖锛屽悎骞惰〃绾т笌鍒楃骇銆?
| 鏂规硶 | 杩斿洖绫诲瀷 | 璇存槑 |
|------|----------|------|
| `name()` | String | 绾︽潫鍚嶏紙鏃犲垯绌轰覆锛?|
| `expr_text()` | String | CHECK 琛ㄨ揪寮忔枃鏈?|

### 5.6 `IndexInfo` / `UniqueInfo`锛堢储寮曚笌鍞竴绾︽潫锛?
`IndexInfo` 鐢?`CreateInfo.indexes()` 杩斿洖锛圡ySQL 椋庢牸 `KEY/INDEX`锛夈€俙UniqueInfo` 鐢?`CreateInfo.uniques()` 杩斿洖锛堜笉鍚富閿級銆?
| 鏂规硶 | 杩斿洖绫诲瀷 | 璇存槑 |
|------|----------|------|
| `IndexInfo.name()` | String | 绱㈠紩鍚?|
| `IndexInfo.columns()` | Array<String> | 绱㈠紩鍒楀悕 |
| `IndexInfo.is_unique()` | bool | 鏄惁涓?UNIQUE 绱㈠紩 |
| `UniqueInfo.name()` | String | 绾︽潫鍚?|
| `UniqueInfo.columns()` | Array<String> | 绾︽潫鍒楀悕 |

### 5.7 `AlterOpInfo`锛圓LTER 鎿嶄綔锛?
鐢?`AlterTableInfo.operations()` 杩斿洖銆傛瘡涓?ALTER 鎿嶄綔杞负涓€鏉¤褰曘€?
| 鏂规硶 | 杩斿洖绫诲瀷 | 璇存槑 |
|------|----------|------|
| `operation_type()` | String | 鎿嶄綔绫诲瀷锛岃涓嬭〃 |
| `column_name()` | String | 娑夊強鐨勫垪鍚嶏紙ADD/DROP/ALTER/RENAME COLUMN 鏃讹級 |
| `table_name()` | String | RENAME TABLE 鐨勬柊琛ㄥ悕 |
| `constraint_name()` | String | 娑夊強鐨勭害鏉熷悕锛圖ROP CONSTRAINT / DROP FOREIGN KEY 鏃讹級 |
| `detail()` | String | 璇︽儏鏂囨湰锛堜汉绫诲彲璇伙級 |

`operation_type` 鍙栧€硷細`ADD_COLUMN / DROP_COLUMN / ALTER_COLUMN / RENAME_COLUMN / RENAME_TABLE / ADD_CONSTRAINT / DROP_CONSTRAINT / DROP_PRIMARY_KEY / DROP_FOREIGN_KEY / DROP_UNIQUE / OTHER`銆?
### 5.8 `TruncateInfo` / `ViewInfo` / `CreateIndexInfo` / `TransactionInfo`

鐢?`StmtInfo.truncate()` / `create_view()` / `create_index()` / `transaction()` 杩斿洖銆?
| 鏂规硶 | 杩斿洖绫诲瀷 | 璇存槑 |
|------|----------|------|
| `TruncateInfo.table_name()` | String | TRUNCATE 鐨勮〃鍚?|
| `TruncateInfo.has_table_keyword()` | bool | 鏄惁鏄惧紡甯?`TABLE` 鍏抽敭瀛?|
| `ViewInfo.name()` | String | 瑙嗗浘鍚?|
| `ViewInfo.materialized()` | bool | 鏄惁涓?MATERIALIZED VIEW |
| `ViewInfo.is_replace()` | bool | 鏄惁涓?CREATE OR REPLACE |
| `ViewInfo.column_count()` | INT | 鏄惧紡鍒楁暟 |
| `CreateIndexInfo.name()` | String | 绱㈠紩鍚嶏紙鏃犲垯绌轰覆锛?|
| `CreateIndexInfo.table_name()` | String | 绱㈠紩鎵€鍦ㄨ〃鍚?|
| `CreateIndexInfo.columns()` | Array<String> | 绱㈠紩鍒楀悕 |
| `CreateIndexInfo.is_unique()` | bool | 鏄惁涓?UNIQUE 绱㈠紩 |
| `TransactionInfo.kind()` | String | `START_TRANSACTION` / `COMMIT` / `ROLLBACK` |

### 5.9 `CommentInfo`锛堟敞閲婏級

鐢?`SqlAst.comments()` 杩斿洖銆俿qlparser 鍦?parse 闃舵涓㈠純娉ㄩ噴锛岃繖閲岄€氳繃鐙珛鏂囨湰鎵弿寰楀埌銆?
| 鏂规硶 | 杩斿洖绫诲瀷 | 璇存槑 |
|------|----------|------|
| `text()` | String | 娉ㄩ噴鏂囨湰锛堜笉鍚?`--` 鎴?`/* */` 鍒嗛殧绗︼級 |
| `line()` | INT | 娉ㄩ噴鎵€鍦ㄨ鍙凤紙1-based锛?|
| `kind()` | String | `LINE`锛堣娉ㄩ噴锛夋垨 `BLOCK`锛堝潡娉ㄩ噴锛?|

## 6. Rhai 璇硶瑕佺偣

- **鍙橀噺澹版槑**锛歚let x = ...;`
- **瀵硅薄 Map 瀛楅潰閲?*锛歚#{ "key": value, "key2": value2 }`锛坄#` 鏄?Rhai 鐨勫璞″瓧闈㈤噺鍓嶇紑锛?- **鏁扮粍**锛歚[1, 2, 3]`锛屾柟娉?`push()`銆乣len()`銆佺储寮曡闂?`arr[0]`
- **寰幆**锛歚for x in arr { ... }`
- **瀛楃涓叉嫾鎺?*锛歚"a" + "b"`
- **Map 璁块棶**锛歚map["key"]` 鎴?`map.key`
- **娉ㄩ噴**锛歚//` 鍗曡
- **鏉′欢**锛歚if cond { ... } else if cond2 { ... } else { ... }`
- **閫掑綊鍑芥暟**锛歊hai 鏀寔鍦ㄨ剼鏈《灞?`fn name(arg) { ... }` 瀹氫箟鍑芥暟骞惰嚜璋冪敤锛堢敤浜庨€掑綊閬嶅巻瀛愭煡璇紝瑙?[no_order_by_in_subquery.rhai](../config/rules/dml/no_order_by_in_subquery.rhai)锛?
## 7. 瑙勫垯鑴氭湰妯℃澘

```rhai
// <rule_name>.rhai - <涓€鍙ヨ瘽鎻忚堪>
// 鍩轰簬 AST锛?绠€杩版娴嬮€昏緫>

// 1. 瑙ｆ瀽澶辫触鏃朵笂鎶ワ紙鎺ㄨ崘淇濈暀锛岄伩鍏嶆紡妫€锛?if guard_parse_error(context) {
    violations.push(parse_error_violation(context));
    return;
}

let ast = context["ast"];

// 2. 閬嶅巻璇彞锛屾寜 kind 鍒嗘淳
for s in ast.statements() {
    if s.kind() == "CREATE_TABLE" {
        let ct = s.create_table();
        // 妫€娴嬫潯浠?        if !ct.has_primary_key() {
            violations.push(violation(
                "CREATE TABLE " + ct.table_name() + " must include a PRIMARY KEY",
                s.line(), s.column()
            ));
        }
    }
}
```

## 8. 鍦?`sqlguard.toml` 涓敞鍐岃鍒?
鍙傝€?[`sqlguard.toml.example`](../sqlguard.toml.example)锛?
```toml
[[rules]]
id = "DDL001"                                    # 瑙勫垯缂栧彿锛屽繀濉笖鍏ㄥ眬鍞竴
name = "no_drop_table"                            # 瑙勫垯鍚嶏紙鍑虹幇鍦ㄦ姤鍛婁腑锛?group = "ddl-safety"                              # 瑙勫垯鍒嗙粍锛屽彲閫?description = "Disallow DROP TABLE in DDL scripts" # 鍙€夛紝鎻忚堪
enabled = true                                    # 鏄惁鍚敤
script_path = "config/rules/ddl/no_drop_table.rhai"  # 鐩稿閰嶇疆鏂囦欢鎵€鍦ㄧ洰褰?applies_to = ["ddl"]                              # 浠呭杩欎簺 script_type 鐢熸晥
severity = "error"                                # "error" 浼氳 check 杩斿洖闈為浂锛?warning" 涓嶄細
```

瀛楁璇存槑锛?
| 瀛楁 | 蹇呭～ | 璇存槑 |
|------|------|------|
| `id` | 鏄?| 瑙勫垯缂栧彿锛屽叏灞€鍞竴銆侰LI 閫氳繃 `--rules` / `--exclude-rules` 鐢ㄥ畠寮曠敤瑙勫垯 |
| `name` | 鏄?| 瑙勫垯鍚嶏紝鍑虹幇鍦ㄦ姤鍛婁腑锛屼究浜庨槄璇?|
| `group` | 鍚?| 瑙勫垯鍒嗙粍鍚嶃€侰LI 閫氳繃 `--groups` / `--exclude-groups` 鐢ㄥ畠绛涢€?|
| `description` | 鍚?| 瑙勫垯鎻忚堪 |
| `enabled` | 鍚︼紙榛樿 `true`锛?| 鏄惁鍚敤 |
| `script_path` | 鏄?| Rhai 鑴氭湰璺緞锛岀浉瀵归厤缃枃浠舵墍鍦ㄧ洰褰?|
| `applies_to` | 鏄?| 浠呭杩欎簺 `script_type` 鐢熸晥锛堜笌 `[[classification.rules]]` 鐨?`type` 瀵瑰簲锛?|
| `severity` | 鍚︼紙榛樿 `"error"`锛?| `error` 璁?`check` 閫€鍑虹爜闈?0锛沗warning` 涓嶉樆鏂?|

`applies_to` 涓?`[[classification.rules]]` 涓殑 `type` 瀵瑰簲锛氬紩鎿庡湪 `run_rules_for_file` 涓繃婊も€斺€斿彧鏈?`script_type 鈭?applies_to` 鏃舵墠鎵ц璇ヨ鍒欍€?
鍔犺浇閰嶇疆鏃朵細鏍￠獙 `id` 蹇呭～涓斿敮涓€锛涚己澶辨垨閲嶅浼氫互 `Config error` 褰㈠紡鎶ラ敊銆?
## 9. 鍦ㄥ懡浠よ绛涢€夎鍒?
`check` 瀛愬懡浠ゆ敮鎸佹寜 `id` 鎴?`group` 绛涢€夎鎵ц鐨勮鍒欙紝鎵€鏈夌瓫閫夊弬鏁伴兘鎺ュ彈閫楀彿鍒嗛殧鐨勫垪琛紝骞舵敮鎸佸墠缂€閫氶厤 `prefix*`锛?
```bash
# 鍙窇鎸囧畾 id 鐨勮鍒?sqlguard check ./sql --rules DDL001,DDL002

# 鍙窇鎸囧畾鍒嗙粍
sqlguard check ./sql --groups ddl-safety

# 鎺掗櫎鏌愪簺瑙勫垯鎴栧垎缁?sqlguard check ./sql --exclude-rules DML001
sqlguard check ./sql --exclude-groups experimental

# 鍓嶇紑閫氶厤锛氬尮閰嶆墍鏈?DDL 寮€澶寸殑 id
sqlguard check ./sql --rules DDL*

# 缁勫悎浣跨敤
sqlguard check ./sql --groups ddl-safety --exclude-rules DDL003
```

绛涢€夎涔夛細

- **鐧藉悕鍗?*锛坄--rules` / `--groups`锛夛細涓虹┖鏃惰〃绀轰笉闄愬埗锛涢潪绌烘椂瑙勫垯闇€鍛戒腑鐧藉悕鍗曟墠鎵ц銆?- **榛戝悕鍗?*锛坄--exclude-rules` / `--exclude-groups`锛夛細鍛戒腑鍗宠烦杩囷紝**浼樺厛绾ч珮浜庣櫧鍚嶅崟**銆?- **鍓嶇紑閫氶厤**锛歚DDL*` 鍖归厤鎵€鏈変互 `DDL` 寮€澶寸殑 id锛沗ddl-*` 鍖归厤鎵€鏈変互 `ddl-` 寮€澶寸殑 group銆?- **`enabled = false` 鐨勮鍒欐案杩滀笉浼氭墽琛?*锛屾棤璁虹瓫閫夊弬鏁板浣曘€?- 绛涢€夊弬鏁颁粎褰卞搷瑙勫垯鎵ц锛屼笉褰卞搷鐩綍缁撴瀯妫€鏌ヤ笌鏂囦欢鍒嗙被銆?
褰撲换涓€绛涢€夊弬鏁伴潪绌烘椂锛宍sqlguard` 浼氬湪 stderr 鎵撳嵃涓€琛屾縺娲荤殑绛涢€夋潯浠讹紝渚夸簬 CI 鏃ュ織杩芥函銆?
## 10. 瀹屾暣绀轰緥

### 10.1 绂佹 DROP TABLE锛圖DL锛?
鏉ヨ嚜 [`config/rules/ddl/no_drop_table.rhai`](../config/rules/ddl/no_drop_table.rhai)锛?
```rhai
if guard_parse_error(context) {
    violations.push(parse_error_violation(context));
    return;
}

let ast = context["ast"];
for s in ast.statements() {
    if s.kind() == "DROP_TABLE" {
        let d = s.drop_object();
        violations.push(violation(
            "DROP TABLE is not allowed in DDL scripts: " + d.name(),
            s.line(), s.column()
        ));
    }
}
```

### 10.2 CREATE TABLE 蹇呴』鏈変富閿紙DDL锛?
鏉ヨ嚜 [`config/rules/ddl/primary_key_required.rhai`](../config/rules/ddl/primary_key_required.rhai)锛?
```rhai
if guard_parse_error(context) {
    violations.push(parse_error_violation(context));
    return;
}

let ast = context["ast"];
for s in ast.statements() {
    if s.kind() == "CREATE_TABLE" {
        let ct = s.create_table();
        if !ct.has_primary_key() {
            violations.push(violation(
                "CREATE TABLE " + ct.table_name() + " must include a PRIMARY KEY",
                s.line(), s.column()
            ));
        }
    }
}
```

### 10.3 绂佹 SELECT *锛圖ML锛?
鏉ヨ嚜 [`config/rules/dml/no_select_all.rhai`](../config/rules/dml/no_select_all.rhai)锛?
```rhai
if guard_parse_error(context) {
    violations.push(parse_error_violation(context));
    return;
}

let ast = context["ast"];
for s in ast.statements() {
    if s.has_select() {
        let sel = s.select();
        if sel.has_wildcard() {
            violations.push(violation(
                "SELECT * is not allowed. Specify columns explicitly.",
                s.line(), s.column()
            ));
        }
    }
}
```

### 10.4 鍩轰簬 AST 鐨?CTE 鏈娇鐢ㄦ娴嬶紙DML锛?
鏉ヨ嚜 [`config/rules/dml/no_unused_cte.rhai`](../config/rules/dml/no_unused_cte.rhai)锛屾紨绀?P0-3 鐨?`SelectInfo.ctes()` / `has_cte()` 鑳藉姏锛?
```rhai
let ast = context["ast"];

for s in ast.statements() {
    if s.has_select() {
        let sel = s.select();
        if sel.has_cte() {
            // 姹囨€讳富鏌ヨ涓庡瓙鏌ヨ鍙鏂囨湰
            let visible = "";
            for col in sel.projection() { visible += " " + col; }
            visible += " " + sel.where_clause();
            visible += " " + sel.from_table();
            visible += " " + sel.from_table_alias();
            for j in sel.joins() {
                visible += " " + j.table_name();
                visible += " " + j.alias();
            }
            let upper_visible = visible.to_upper();

            for cte in sel.ctes() {
                let cte_name = cte.name().to_upper();
                if cte_name != "" && !upper_visible.contains(cte_name) {
                    violations.push(#{
                        "message": "CTE '" + cte.name() + "' is defined but not referenced",
                        "line": s.line(),
                        "column": s.column()
                    });
                }
            }
        }
    }
}
```

### 10.5 鍩轰簬 AST 閫掑綊瀛愭煡璇㈢殑 ORDER BY 妫€娴嬶紙DML锛?
鏉ヨ嚜 [`config/rules/dml/no_order_by_in_subquery.rhai`](../config/rules/dml/no_order_by_in_subquery.rhai)锛屾紨绀?P1-4 鐨?`SelectInfo.subqueries()` 閫掑綊鑳藉姏 + P1-5 鐨勭獥鍙ｅ嚱鏁颁慨澶嶏細

```rhai
let ast = context["ast"];

// 閫掑綊妫€鏌ュ瓙鏌ヨ锛歄RDER BY 蹇呴』閰嶅悎 LIMIT/OFFSET/FETCH
fn check_subquery(sel) {
    for sub in sel.subqueries() {
        if sub.has_order_by() && !sub.has_limit() && !sub.has_offset() && !sub.has_fetch() {
            violations.push(#{
                "message": "ORDER BY in subquery is typically ignored unless combined with LIMIT/OFFSET",
                "line": 1
            });
            return true;
        }
        if check_subquery(sub) { return true; }
    }
    return false;
}

for s in ast.statements() {
    if s.has_select() {
        check_subquery(s.select());
    }
}
```

### 10.6 鍩轰簬瀛楃涓插尮閰嶇殑绠€鍗曡鍒欙紙涓嶆帹鑽愶紝浣嗗彲鐢級

褰?AST 鏃犳硶瑕嗙洊鏃朵娇鐢細

```rhai
let sql = context["sql_content"];
let upper = sql.to_upper();

if upper.contains("DROP TABLE") {
    violations.push(violation_msg("DROP TABLE not allowed"));
}
```

娉ㄦ剰锛氬瓧绗︿覆鍖归厤浼氳鍒ゆ敞閲娿€佸瓧绗︿覆瀛楅潰閲忎腑鐨勫唴瀹癸紝搴斾紭鍏堜娇鐢?AST銆?
## 11. 璋冭瘯涓庢渶浣冲疄璺?
1. **濮嬬粓鍏堟鏌?`has_parse_error()`**锛氳В鏋愬け璐ユ椂 AST 涓虹┖锛岃鍒欎笉浼氳Е鍙戯紱寤鸿涓婃姤浠ュ厤婕忔銆?2. **浼樺厛鐢?AST 鑰岄潪瀛楃涓插尮閰?*锛欰ST 鑳藉尯鍒嗘敞閲娿€佸瓧闈㈤噺涓庣湡瀹炶鍙ャ€?3. **琛屽垪鍙峰敖閲忎粠 `StmtInfo` 鍙?*锛歚s.line()` / `s.column()` 璁╂姤鍛婃洿鏄撳畾浣嶃€?4. **`severity` 閫夋嫨**锛?   - `error` 鈥斺€?涓ラ噸杩濊锛屼細璁?`sqlguard check` 閫€鍑虹爜闈?0锛岄樆鏂?CI
   - `warning` 鈥斺€?浠呮彁绀猴紝涓嶉樆鏂?5. **`applies_to` 绮剧‘闄愬畾**锛欴DL 瑙勫垯鍙 `ddl` 绫诲瀷鐢熸晥锛岄伩鍏嶈鎶ャ€?6. **鑴氭湰閿欒澶勭悊**锛氳剼鏈繍琛屾椂鎶涢敊浼氳寮曟搸鎹曡幏骞朵笂鎶ヤ负璇ヨ鍒欑殑涓€鏉¤繚瑙勶紝浣嗚娉曢敊璇細瀵艰嚧瑙勫垯鏁翠綋澶辫触銆?7. **鑴氭湰璺緞**锛歚script_path` 鐩稿 `sqlguard.toml` 鎵€鍦ㄧ洰褰曡В鏋愶紝涔熸敮鎸佺粷瀵硅矾寰勩€?8. **閫掑綊閬嶅巻瀛愭煡璇?*锛氱敤椤跺眰 `fn` 瀹氫箟鍑芥暟锛屽湪 `for s in sel.subqueries()` 涓嚜璋冪敤鈥斺€擱hai 涓嶆敮鎸侀棴鍖呮崟鑾峰閮ㄥ彲鍙樺紩鐢紝浣?`violations.push()` 鍦ㄥ嚱鏁颁綋鍐呭彲姝ｅ父璋冪敤銆?
## 12. 闄愬埗涓庢敞鎰忎簨椤?
- AST 鍖呰绫诲瀷瑕嗙洊浜?SELECT/INSERT/UPDATE/DELETE/CREATE TABLE/DROP/ALTER TABLE/TRUNCATE/CREATE VIEW/CREATE INDEX/浜嬪姟璇彞绛変富瑕?DDL/DML 鍦烘櫙锛涘瓨鍌ㄨ繃绋嬩綋锛坄CREATE PROCEDURE` / `CREATE FUNCTION` 鐨?`BEGIN ... END` 鍧楋級sqlparser 0.45 瑙ｆ瀽鑳藉姏鏈夐檺锛岃鍒欏彧鑳藉熀浜?kind 妫€娴?瀛樺湪 CREATE PROCEDURE"锛屾棤娉曟鏌ヨ繃绋嬩綋鍐呭銆?- **鑺傜偣绾т綅缃俊鎭?*锛歴qlparser 0.45 鐨?AST 鑺傜偣鏈韩涓嶆惡甯︿綅缃紝`JoinInfo` / `ColumnInfo` / `ForeignKeyInfo` / `CheckInfo` / `IndexInfo` / `UniqueInfo` / `ExprInfo` 涓婄殑 `line()` / `column()` 鏂规硶棰勭暀杩斿洖 0锛岄渶鏈潵鍩轰簬 token 娴佸洖濉墠鑳界簿纭埌 JOIN/鍒楃骇銆?- **閫楀彿闅愬紡 JOIN**锛歴qlparser 鎶?`FROM a, b` 涓?`FROM a CROSS JOIN b` 閮借В鏋愪负 `JoinOperator::CrossJoin`锛孉ST 涓婃棤鏍囪鍖哄垎銆俙SelectInfo.has_comma_join()` 涓?`SqlAst.has_comma_join_anywhere()` 閫氳繃鏂囨湰鎵弿锛堣窡韪?paren depth 涓庡瓙鍙ョ粓姝㈠叧閿瓧锛夊洖閫€瀹炵幇锛屽彲闈犱絾闈?AST 鍘熺敓銆?- **娉ㄩ噴**锛歴qlparser 鍦?parse 闃舵涓㈠純娉ㄩ噴锛宍SqlAst.comments()` 閫氳繃鐙珛鏂囨湰鎵弿寰楀埌锛岃兘璇嗗埆 `--` 涓?`/* */`锛屼絾涓嶈兘鍖哄垎娉ㄩ噴鏄惁鍦ㄥ瓧绗︿覆瀛楅潰閲忓唴锛堟瀬绔満鏅鎶ワ級銆?- **琛ㄨ揪寮忔爲娣卞害**锛歚ExprInfo` 鍙毚闇查《灞傚垽瀹氾紝涓嶉€掑綊鏆撮湶瀛愯〃杈惧紡锛堥伩鍏嶇被鍨嬬垎鐐革級銆傝嫢闇€娣卞叆瀛愯〃杈惧紡锛屽缓璁粨鍚?`where_clause()` 鏂囨湰鍋氫簩娆¤В鏋愩€?- Rhai 涓暣鏁伴粯璁や负 `i64`锛宍line_count` 绛夊瓧娈垫寜 `i64` 娉ㄥ叆銆?- Rhai 涓嶆敮鎸侀棴鍖呮崟鑾峰閮ㄥ彲鍙樺紩鐢紝浣嗗彲浠ラ€氳繃 `for` 寰幆閬嶅巻骞剁洿鎺ヨ皟鐢?`violations.push()`锛屾垨鍦ㄩ《灞?`fn` 涓皟鐢ㄣ€?- 鍚屼竴鏂囦欢鐨?AST 鍙В鏋愪竴娆★紝鎵€鏈夎鍒欏叡浜€?- 姣忔潯瑙勫垯姣忔鎵ц閮戒細閲嶆柊鏋勫缓 Rhai 寮曟搸骞舵敞鍐屾墍鏈夋柟娉曪紝瑙勫垯杈冨鏃朵細鏈夋€ц兘寮€閿€銆?