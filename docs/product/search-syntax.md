# 查询语法规范

> 需求 ID 见 [SPEC.md](../SPEC.md)。

## 1. 查询语法完备规范

### 1.1 搜索项结构与运算符优先级
搜索项标准结构：`[!][修饰符:...[修饰符:]][函数:]<文本>`。
- **与 (AND)**：空格分隔。如 `foo bar` 表示同时包含 `foo` 与 `bar`。
- **或 (OR)**：竖线 `|` 分隔。如 `foo|bar`。
- **非 (NOT)**：前置感叹号 `!`。如 `!foo`。
- **分组 (Grouping)**：尖括号 `<...>`。如 `!<foo bar>`。
- **字面量转义**：双引号 `"..."`。引号内所有字符（包括空格、竖线、感叹号）按纯字面量匹配。
- **核心优先级规则**：**默认或（`|`）先于与（空格）执行求值**，即 `apple banana|orange` 等价于 `apple <banana|orange>`。

### 1.2 通配符与路径匹配规则
- `*` 匹配除反斜杠 `\` 外的零个或多个字符。
- `**` 匹配包含反斜杠 `\` 的跨层级多级路径。
- `?` 匹配除反斜杠 `\` 外的单个字符。
- **通配符全词规则**：**只要搜索项中包含通配符，即自动切换为匹配完整文件名（Whole Filename）**；子串模糊匹配需显式写作 `*foo*`。
- **路径自适应规则**：搜索项含 `\` 时自动切换为完整路径匹配；以 `\` 结尾限定匹配文件夹；支持 `c:`、`c:|d:`、`!e:` 盘符限定。

### 1.3 修饰符完整支持矩阵

| 修饰符名称 | 简写别名 | 作用范围与行为定义 | 优先级 |
|:---|:---|:---|:---|
| `case:` / `no-case:` | | 开启 / 关闭大小写敏感区分 | P0 |
| `whole-words:` / `no-whole-words:` | `ww:` | 仅匹配独立单词（基于非字母数字字符边界切分） | P0 |
| `whole:` | `wfn:`, `entire:` | 强制匹配整个完整文件名 | P0 |
| `path:` / `no-path:` | `\:` | 开启 / 关闭完整绝对路径匹配 | P0 |
| `regex:` | | 将该搜索项作为正则表达式求值 | P0 |
| `file:` / `folder:` | | 仅匹配文件 / 仅匹配目录 | P0 |
| `prefix:` / `suffix:` | | 匹配单词前缀 / 单词后缀（按单词边界断词） | P1 |
| `start-with:` / `end-with:` | `^:`, `$:` | 匹配文件名的最开头 / 最末尾字符 | P1 |
| `diacritics:` / `no-diacritics:` | `diac:` | 区分 / 忽略音标变音符号 | P1 |
| `ignore-punctuation:` | `nopunc:` | 检索时忽略所有标点符号 | P1 |
| `ignore-whitespace:` | `nows:` | 检索时忽略所有空白字符 | P1 |
| `pinyin:` | `py:` | 拼音及首字母缩写拼音匹配 | P1 |

> 表中 P1 条目按目标层级规划；`pinyin:`、`nopunc:`、`nows:`、`diacritics:` 均已实现并提供“搜索”菜单图形开关。闭环状态以 [SPEC §3.10](../SPEC.md) 为准。

### 1.4 过滤函数与日期区间完整语法

| 函数名 | 语法示例 | 行为说明 |
|:---|:---|:---|
| `ext:` | `ext:rs;toml;md` | 分号分隔多个扩展名；忽略前导点与通配符，如 `ext:*.tar.gz` |
| `size:` | `size:>10mb`、`size:1kb..500mb` | 文件逻辑大小过滤（`1KB = 1024 Bytes`，支持 B/KB/MB/GB/TB） |
| `date-modified:` | `dm:today`、`dm:2026-01-01..2026-10-15` | 修改时间；支持相对时长（`2days`、`3weeks`、`4hours`）、`YYMMDD`、月份星期名、`mtd/ytd/qtd` |
| `date-created:` | `dc:>2026`、`dc:yesterday` | 创建时间；取值格式与 `dm:` 一致 |
| `parent:` | `parent:C:\Windows` | 仅匹配指定绝对路径的直接子项，忽略末尾反斜杠 |
| `content:` | `content:架构`、`content:"error code"` | 触发 Tantivy 全文内容索引；支持复合分组 `content:<abc 123>`；单条查询至多 16 个内容检索项（按解析后带内容检索的项数计，含重复与否定项），超限在管道请求边界以错误码 416 整体拒绝 |
| `len:` | `len:>20`、`len:4..16` | 按文件名字符长度过滤 |
| `attrib:` | `attrib:HS`、`attrib:!D` | 按 DOS 文件属性位过滤（支持 `R/H/S/D/A/P/C/L` 及前导 `!` 取反） |
| `depth:` | `depth:<=3`、`depth:2..4` | 按绝对路径层级深度过滤 |
| `child:` / `childcount:` / `childfilecount:` / `childfoldercount:` / `empty:` | `child:Cargo.toml`、`empty:` | 按目录子项名、直接子项数或空目录过滤 |
| `dupe:` / `namedupe:` / `sizedupe:` / `namepartdupe:` / `unique:` | `dupe:name;size`、`unique:` | 按名称、大小、主干或组合检测重复或唯一项 |
| `length:` / `author:` / `tags:` / `dimensions:` / `width:` / `height:` / `framerate:` | `author:张三`、`dim:1920x1080` | 扩展属性侧表检索（需先启用属性索引；未启用时明确降级） |
| `sort:` | `sort:size-descending;name-ascending` | 在查询内声明单键或多键排序顺序（别名 `date-modified` / `date-created` / `ext` / `dimensions` / `framerate` 等） |

### 1.5 快速过滤器宏
- 内置宏：`audio:`、`doc:`、`exe:`、`image:`、`video:`、`zip:`。
- 宏支持使用竖线 `|` 组合，例如 `audio:|video:`。各类宏对应的文件扩展名集合可在配置文件中灵活定制。

### 1.6 正则表达式与 ReDoS 防护
- 正则表达式采用 Rust 官方 `regex` 库执行，匹配时间为 $O(M \times N)$，无回溯型 ReDoS 风险。
- 正则解析器具有内存编译缓存，快速轮询或百万行滚动时复用已编译的有限状态机。

### 1.7 与 PCRE / Everything 语法差异
- 正则表达式：采用 Rust `regex` 引擎保证线性时间复杂度，不支持环视断言与反向引用。
- 搜索语言扩展（`QRY-10`）已实现：`pinyin:`/`py:`（含全拼/首字母）、`diacritics:`/`no-diacritics:`、`ignore-punctuation:`/`nopunc:`、`ignore-whitespace:`/`nows:`、`len:`、`attrib:`、`depth:`、`child:`/`childcount:`/`childfilecount:`/`childfoldercount:`/`empty:`、`dupe:`/`namedupe:`/`sizedupe:`/`namepartdupe:`/`unique:`、`sort:` 多键排序，以及 `IDX-12` 扩展属性函数（`length:`/`author:`/`tags:`/`dimensions:`/`width:`/`height:`/`framerate:`）。单条查询最多包含 8 个不同的 `child:` 模式与 8 个排序键；超出 `child:` 上限以错误码 416 拒绝，排序键超出部分由服务端截断。
- 未实现边界：字符实体转义（如 `&sp:`）、正则捕获列与自定义宏。
