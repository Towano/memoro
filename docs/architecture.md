# Memoro 架构

## 总体调用链

Memoro 的核心调用链是：

`MemoroServer → Service → MemoryStore → GitRepository`

- `MemoroServer` 提供协议入口、请求解析和响应封装。
- `Service` 负责用例编排、参数组合与并发控制边界。
- `MemoryStore` 负责空间内存储、查询、版本和同步语义。
- `GitRepository` 负责 Git 对象、引用、提交与工作区状态。

横向模块包括 `markdown`、`search`、`sync` 与 `locking`。
`markdown` 负责 frontmatter、规范化内容和 Markdown 序列化。
`search` 负责索引构建、分词与相关性排序。
`sync` 负责远端同步、快进判断和冲突引用记录。
`locking` 为读写路径提供空间级并发保护。

## 空间与文件布局

每个空间都是独立的 Git 仓库，路径为：

`$MEMORO_HOME/spaces/<name>`

空间内的 Markdown 文件按资源类型分目录：

- persona：`$MEMORO_HOME/spaces/<name>/persona/*.md`
- projects：`$MEMORO_HOME/spaces/<name>/projects/*.md`
- playbooks：`$MEMORO_HOME/spaces/<name>/playbooks/*.md`

空间之间不共享 Git 历史、工作区状态或同步引用。

## 内容、版本与并发

每个 Markdown 文件的 frontmatter 都必须严格校验；不符合 schema 的内容拒绝写入。
写入前先生成 canonical Markdown，再以其 SHA-256 作为 revision。
更新请求可携带 `base_revision`，仅在它匹配当前 revision 时覆盖文件。
不匹配时拒绝更新，避免并发请求静默覆盖彼此的修改。
写入使用读写锁、原子写入，并为每次变更创建单文件 commit。
检测到脏仓库时拒绝写入，防止应用状态覆盖用户或外部进程的修改。

## 搜索与同步

搜索使用 BM25F，对字段权重进行区分；CJK 文本使用 bigram 分词。
`sync` 只允许 fast-forward；不能快进时不自动合并或改写本地历史。
发生分叉时，将冲突状态写入 `refs/memoro/sync-conflict`。

## 接入与边界

服务支持 stdio 接入，也提供 HTTP ` /mcp` 端点。
HTTP 提供免鉴权的 `/health`，其他请求使用 Bearer token。
`python/` 是参考实现，不是 Rust 运行时依赖。
热加载和自动安装脚本目前未实现；版本 tag 会触发 GitHub Actions 构建多平台 release
二进制与 GHCR 多架构镜像。