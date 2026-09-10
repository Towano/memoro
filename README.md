# Memoro

Memoro 是一个个人的、以 Git 为存储底座的 AI 记忆系统（Rust 实现）。记忆以
Markdown 文件保存在本地 Git 仓库中，每次写入自动产生提交，天然携带完整历史。
Memoro 以 MCP（Model Context Protocol）server 的形式运行，供 AI agent 通过统一的工具接口读写记忆、检索内容，并把本地仓库与远端 Git 仓库做备份同步。

## 当前支持范围

当前源码中的 Rust CLI 支持以下命令：

```text
memoro serve [--transport stdio|http] [--home <PATH>] [--host <HOST>] [--port <PORT>] [--token <TOKEN>]
memoro spaces add <NAME> [--readonly]
memoro spaces remove <NAME>
memoro spaces list
memoro sync setup <REPOSITORY_URL> [--space <NAME>] [--branch <BRANCH>] [--timeout <SECONDS>]
    [--deploy-key <PATH>] [--known-hosts <PATH>] [--force]
```

`serve` 支持 stdio 和 streamable HTTP 两种 MCP 传输方式。HTTP 服务固定挂载在
`/mcp`；CLI 默认监听 `127.0.0.1:8000`。`spaces` 用于管理已登记的记忆空间，
`sync setup` 用于为一个空间配置 `origin` 远端；记忆读写和同步状态、拉取、推送操作
仍通过 MCP 工具完成。项目当前不提供独立的 `init`、`status`、`backup` 或 release
下载命令。

已实现的 MCP 工具如下：

| 工具 | 说明 |
| --- | --- |
| `memory_create` | 创建一条新记忆 |
| `memory_patch` | 部分更新记忆字段 |
| `memory_replace` | 整体替换记忆内容 |
| `memory_delete` | 删除一条记忆 |
| `memory_get` | 按 ID 读取单条记忆 |
| `memory_list` | 列出记忆 |
| `memory_search` | 关键词检索记忆 |
| `spaces_list` | 列出全部记忆空间 |
| `sync_status` | 查看各空间的备份同步与冲突状态 |
| `sync_pull` | 从远端拉取备份 |
| `sync_push` | 把本地记忆推送到远端备份 |

## 从源码安装

需要 Rust stable 工具链。直接在项目根目录执行：

```bash
cargo install --path .
memoro serve
```

`cargo install --path .` 会把 `memoro` 安装到 Cargo 的 bin 目录（通常是
`~/.cargo/bin`）；请确保该目录在 `PATH` 中。开发构建也可以使用：

```bash
cargo build --release
./target/release/memoro serve
```

推送 `v*` 版本 tag 后，GitHub Actions 会构建 Linux、macOS 和 Windows 的多平台
release binary，并将各平台压缩包上传到该 tag 的 GitHub Release。
同一个工作流还会构建并推送 `linux/amd64` 与 `linux/arm64` 的 GHCR 镜像。
Release 页面位于 `https://github.com/Towano/memoro/releases`。

## Docker Compose 部署

Compose 默认只把宿主机的 `127.0.0.1:8000` 映射到容器的 `8000` 端口；容器内部服务仍监听
`0.0.0.0`，以便完成端口映射。数据使用宿主机的 `./data` bind mount，容器删除或重建不会自动删除该目录。

首次启动：

```bash
mkdir -p data
# distroless 容器使用 uid/gid 65532 写入 /data；按宿主机权限策略执行。
sudo chown 65532:65532 data

# HTTP 未设置 token 时不启用鉴权；对外提供服务前请设置自己的随机 token。
export MEMORO_TOKEN='replace-with-a-random-token'
docker compose up --build -d
```

使用方式：

- MCP HTTP 地址为 `http://127.0.0.1:8000/mcp`。
- 设置 `MEMORO_TOKEN` 后，每个请求都必须带 `Authorization: Bearer <token>`；文档中的
  `<token>` 只是占位符，不是可用凭据。
- 未设置或设置为空的 `MEMORO_TOKEN` 不启用鉴权，仅适合已由其他方式保护的本机环境。
- 查看日志：`docker compose logs -f memoro`。

停止、升级和卸载是不同操作：

```bash
# 停止容器，保留容器和 ./data
docker compose stop

# 移除 Compose 创建的容器和网络，仍保留 ./data
docker compose down

# 从当前源码升级镜像；./data 会继续复用
docker compose up --build -d
```

卸载 Docker 部署时，先执行 `docker compose down` 移除容器和网络；它不会删除 `./data`。

只有在确认已经备份并且确实要永久删除本地记忆时，才显式删除数据目录：

```bash
docker compose down
rm -rf ./data
```

上面的 `rm -rf ./data` 不属于停止或卸载步骤，会永久删除该部署保存的本地记忆。

## 本地运行与数据位置

```bash
memoro serve
```

`serve` 缺省使用 stdio 传输：在 MCP 客户端（如 Claude Desktop、Claude Code 等）里把
`memoro serve` 配置为 stdio 类型的命令即可。数据缺省保存在 `~/.memoro`，可通过
`MEMORO_HOME` 或 `--home` 修改。CLI 参数优先于同名环境变量，环境变量优先于缺省值。

| 参数 / 变量 | 缺省值 | 说明 |
| --- | --- | --- |
| `--transport` / `MEMORO_TRANSPORT` | `stdio` | `stdio` 或 `http` |
| `--home` / `MEMORO_HOME` | `~/.memoro` | 数据根目录（空间仓库、本地配置 `config.json`） |
| `--host` / `MEMORO_HOST` | `127.0.0.1` | HTTP 模式监听地址 |
| `--port` / `MEMORO_PORT` | `8000` | HTTP 模式监听端口 |
| `--token` / `MEMORO_TOKEN` | 未设置 | HTTP 模式 Bearer 鉴权令牌；未设置不启用鉴权 |
| `MEMORO_GIT_NAME` | `Memoro` | 记忆仓库 Git 提交使用的作者名 |
| `MEMORO_GIT_EMAIL` | `memoro@localhost` | 记忆仓库 Git 提交使用的作者邮箱 |

## 备份与同步

每个记忆空间都是独立的本地 Git 仓库（`$MEMORO_HOME/spaces/<name>`）。为空间配置远端
仓库后，即可用 `sync_push` 推送备份、`sync_pull` 拉取恢复。远端访问凭据按非交互方式
准备：HTTPS 走 Git Credential Manager，SSH 走已加载的 key / agent（也支持 deploy key）。
请将凭据保存在 Git 凭据管理器或 SSH agent 中，不要写入 README、Compose 文件或 Git URL。

Memoro 绝不自动 merge、rebase 或 force-push。当远端与本地历史不兼容（例如两端都有新提交）
时，Memoro 会在仓库中记录冲突标记——`refs/memoro/sync-conflict` 指向需要处理的提交——
并拒绝继续同步。此时请人工整合两边的历史（或改用空仓库），完成后冲突标记会被清除。
`sync_status` 可随时查看当前状态。

## 开发与检查

```bash
cargo fmt --check
cargo test
cargo clippy --all-targets --all-features -- -D warnings
```

`python/` 目录是迁移前的参考实现（语义规格），不是当前安装入口，请勿修改。运行其测试：

```bash
cd python && ../.venv/bin/python -m pytest
```

## 卸载本地安装

卸载通过 Cargo 安装的 CLI 不会删除记忆数据：

```bash
cargo uninstall memoro
```

该命令只移除 `memoro` 可执行文件；`~/.memoro`（或 `MEMORO_HOME` 指定的目录）仍会保留。
如需删除数据，请先备份，再按实际目录显式删除，并将其与卸载命令分开执行。

## 许可

MIT License，见 [LICENSE](LICENSE)；第三方来源见 [NOTICE.md](NOTICE.md)。
