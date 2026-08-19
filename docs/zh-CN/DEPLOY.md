# 部署指南

本文档详细说明如何部署 kiro2api 服务。

## 环境要求

| 组件 | 最低版本 | 说明 |
|------|---------|------|
| Docker | 20.10+ | 推荐使用 Docker 部署 |
| Docker Compose | 1.29+ | 编排工具 |
| 内存 | 512MB+ | 建议 1GB 以上 |
| 磁盘 | 500MB+ | 用于存储凭据、配置和日志 |
| 架构 | amd64 / arm64 | 官方镜像多架构，自动匹配 |
| 操作系统 | Linux/Mac/Windows | 任何支持 Docker 的系统 |
| 网络 | 直连 `*.amazonaws.com` | 需要能访问 AWS CodeWhisperer/Kiro 端点 |

> 仅从源码构建时才需要本地安装 Rust（2024 edition）；Docker 部署无需本地 Rust 环境，只需 Docker 和一份有效的 Kiro 凭据即可。

## 获取 Kiro 凭据

### 前置条件

- 拥有有效的 Kiro（CodeWhisperer）账号
- 可用的登录方式：Builder ID / IAM SSO（IdC）/ 社交登录之一
- 可直接复用你 Kiro 客户端中已有的凭据

### 获取方式

kiro2api 支持两种途径获取凭据：

**方式一：从已有 Kiro 客户端导出**

如果你本地已登录过 Kiro，直接复用其凭据文件即可 drop-in。需要以下字段：

| 字段 | 特征 | 说明 |
|------|------|------|
| `accessToken` | 访问令牌 | 到期后由服务自动刷新 |
| `refreshToken` | 刷新令牌 | 用于自动续期 |
| `expiresAt` | RFC3339 时间戳 | 令牌过期时间 |
| `authMethod` | `social` 或 `idc` | 决定携带 `profileArn` 还是 `clientId`/`clientSecret` |

**方式二：管理面板交互式登录**

无需手动导出，在 `/admin` 管理面板现场完成登录，支持三种流程：

| 登录流 | 说明 |
|--------|------|
| Builder ID 设备码 | 弹出设备码，浏览器授权后自动回填凭据 |
| IAM SSO 授权码 | 输入 SSO Start URL + region，走授权码换令牌 |
| 社交令牌 | 直接粘贴社交登录令牌 |

### 获取技巧

- 优先复用现有 Kiro 凭据，可零成本 drop-in，无需重新登录
- `authMethod=social` 的账号必须带 `profileArn`，否则无法路由
- `expiresAt` 到期不必手动更新，服务会**自动内存刷新**并原子落盘
- 一份 `credentials.json` 可放入多个账号，服务自动轮询

### 凭据有效期

- `accessToken` 到期后服务**自动刷新**，无需人工介入
- 刷新采用单飞协调（避免并发刷新级联 `401`），成功后原子写回 `credentials.json`
- 只有 `refreshToken` 真正失效（凭据永久失效）时账号才被禁用
- 配额 / 风控 / 限流一律走冷却自愈，不会误伤账号

## Docker 部署

### 快速开始

```bash
# 1. 克隆仓库
git clone https://github.com/xwteam/kiro2api.git
cd kiro2api

# 2. 复制环境变量模板
cp .env.example .env

# 3. 编辑 .env 文件，填入对外调用密钥
# 使用你喜欢的编辑器打开 .env
nano .env
# 或
vim .env

# 4. 准备持久化目录
mkdir -p data
```

### 配置 .env 文件

编辑 `.env` 文件，至少填入一个对外调用密钥：

```env
# 必填：对外调用密钥（留空则协议端点开放访问，启动会告警）
API_KEY=sk-你的对外调用密钥

# 管理端独立鉴权 key（不写这一行才回退用 API_KEY；写成空值等于把 config.json 里的管理密钥清掉）
# 公网部署必须设置，否则 /api/admin/* 无鉴权
ADMIN_API_KEY=sk-你的管理端密钥

# 可选：监听地址（容器镜像已内置 HOST=0.0.0.0）
HOST=0.0.0.0

# 可选：服务端口（默认 8080）。compose 的端口映射与健康检查都跟随该值，改这里即可换端口
PORT=8080

# 可选：默认 AWS region（账号 profileArn 内的 region 优先，默认 us-east-1）
REGION=us-east-1

# 可选：负载均衡策略（priority 等权轮询 / balanced 按 weight 加权，默认 priority）
LOAD_BALANCING_MODE=priority

# 可选：每账号每分钟请求上限（0 = 无限，默认 0）
MAX_RPM_PER_CREDENTIAL=0

# 可选：凭据文件路径。镜像不设这个变量：内置默认值 credentials.json 会就近解析到 -c 所指
# 配置文件的目录，容器以 -c /app/data/config.json 启动，因此默认落点就是 /app/data/credentials.json
# （在挂载卷内）。正因为没烘焙成 ENV，config.json 里的 credentialsPath 才仍然生效。
# 它同时决定用量统计、api_keys.json 与余额缓存的目录（取其父目录），自定义时务必指向挂载卷内
# CREDENTIALS_PATH=/app/data/credentials.json
```

### 配置注意事项

- **不要加引号**：`API_KEY=sk-xxx` 而不是 `API_KEY="sk-xxx"`
- **不要有空格**：`API_KEY=sk-xxx` 而不是 `API_KEY = sk-xxx`
- **务必设置密钥**：`API_KEY` 留空时协议端点开放访问，对外部署必须填；`ADMIN_API_KEY`、`API_KEY` 都不设时 `/api/admin/*` 同样开放，公网部署必须设置 `ADMIN_API_KEY`
- **不要留空值**：`API_KEY=`、`ADMIN_API_KEY=` 这样的空值会覆盖 `config.json` 里已配的密钥，不想用就把整行注释掉
- **敏感信息**：不要将 `.env`、`credentials.json` 提交到 Git，已在 `.gitignore` 中

### 放入 Kiro 凭据

把账号凭据放到 `data/credentials.json`（数组，可直接 drop-in 现有 Kiro 凭据）：

```json
[
  {
    "id": 12345,
    "accessToken": "...",
    "refreshToken": "...",
    "expiresAt": "2026-07-25T12:00:00Z",
    "authMethod": "social",
    "profileArn": "arn:aws:codewhisperer:us-east-1:...:profile/...",
    "machineId": "..."
  }
]
```

### 启动服务

```bash
# 后台启动
docker compose up -d

# 查看启动日志
docker compose logs -f

# 看到以下信息表示启动成功：
# 账号池就绪
# 服务监听 0.0.0.0:8080
```

> 镜像为多架构（amd64/arm64），容器内以非 root 用户 `appuser`（UID 1000）运行：`docker-entrypoint.sh` 先以 root `chown` 挂载卷再 `gosu` 降权（无缝升级 legacy root 创建的 data）。镜像内置 `HEALTHCHECK`（探测端口按 `PORT` 环境变量 > `data/config.json` 的 `port` > `8080` 解析，与应用监听端口一致），compose 使用 `restart: unless-stopped`。

### 停止服务

```bash
# 停止服务
docker compose down

# 停止并删除数据卷（谨慎：会清空 data 持久化数据）
docker compose down -v
```

### 查看日志

```bash
# 实时查看日志
docker compose logs -f

# 查看最后 100 行日志
docker compose logs --tail=100

# 查看特定服务的日志
docker compose logs kiro2api
```

## 多账号配置

### 为什么需要多账号

- 提高并发处理能力
- 实现负载均衡（`priority` / `balanced` 两种策略）
- 增加服务稳定性（单账号冷却时自动切换）
- 分摊每账号的 RPM 限流

### 配置多账号

kiro2api 的账号池就是 `data/credentials.json` 这个**数组**，往里面追加账号即可：

```json
[
  {
    "id": 12345,
    "accessToken": "...",
    "refreshToken": "...",
    "expiresAt": "2026-07-25T12:00:00Z",
    "authMethod": "social",
    "profileArn": "arn:aws:codewhisperer:us-east-1:...:profile/...",
    "machineId": "..."
  },
  {
    "id": 67890,
    "accessToken": "...",
    "refreshToken": "...",
    "expiresAt": "2026-07-25T12:00:00Z",
    "authMethod": "idc",
    "clientId": "...",
    "clientSecret": "..."
  }
]
```

### 配置说明

| 字段 | 必填 | 说明 |
|------|------|------|
| `id` | 是 | 账号唯一标识 |
| `accessToken` / `refreshToken` | 是 | 访问令牌与刷新令牌（到期自动刷新） |
| `expiresAt` | 是 | 令牌过期时间（RFC3339） |
| `authMethod` | 是 | `social`（带 `profileArn`）或 `idc`（带 `clientId`/`clientSecret`） |
| `profileArn` | social 必填 | CodeWhisperer profile ARN，内含 region |
| `machineId` | 否 | 机器标识 |
| `disabled` | 否 | 置为 `true` 时从账号池中排除该账号 |

### 切换负载均衡策略

在 `.env` 或 `config.json` 中设置，也可在管理面板运行期切换：

```env
# priority：等权轮询（默认）
# balanced：按每个账号的 weight 加权分配
LOAD_BALANCING_MODE=priority
```

### 运行时添加账号

无需重启服务，在管理面板 `/admin` 的账号管理页可增删改查，支持三种交互式登录、批量导入、设置优先级/权重与余额查询，改动会原子落盘到 `credentials.json`。

## 验证部署

### 健康检查

```bash
# 基础健康检查（无需认证）
curl http://localhost:8080/health

# 输出示例：
# {"service":"kiro2api","status":"ok","version":"0.17.2"}
```

### 准备 API Key

API Key 由你在 `.env` 或 `config.json` 中自行设置：

```bash
# 查看 .env 中配置的对外调用密钥
cat .env | grep API_KEY

# 或查看 config.json
cat data/config.json | grep -i apiKey
```

### 测试 API

```bash
# 获取协议侧模型清单（固定短清单，不代表账号档位真的授权）
curl http://localhost:8080/v1/models \
  -H "Authorization: Bearer sk-你的API密钥"

# 发送测试请求
curl -X POST http://localhost:8080/v1/chat/completions \
  -H "Content-Type: application/json" \
  -H "Authorization: Bearer sk-你的API密钥" \
  -d '{
    "model": "claude-sonnet-4.5",
    "messages": [{"role": "user", "content": "你好"}]
  }'

# 流式请求
curl -X POST http://localhost:8080/v1/chat/completions \
  -H "Content-Type: application/json" \
  -H "Authorization: Bearer sk-你的API密钥" \
  -d '{
    "model": "claude-sonnet-4.5",
    "messages": [{"role": "user", "content": "你好"}],
    "stream": true
  }'
```

看到 AI 回复的文字即部署成功。如果返回 `401`，请检查 API Key 是否正确。

## 常见问题

### 令牌过期

**症状**：账号无法调用，日志提示令牌刷新失败

**解决方案**：
1. 正常情况下 `accessToken` 到期由服务**自动内存刷新**并原子落盘，无需干预
2. 若 `refreshToken` 也已失效，需重新获取凭据：在 `/admin` 面板重新走交互式登录（Builder ID / IAM SSO / 社交令牌）
3. 或直接更新 `data/credentials.json` 中该账号的 `refreshToken` 与 `expiresAt`
4. 更新凭据后无需重启，账号池会重新纳管

### 端口冲突

**症状**：启动时报错 `Address already in use`

**解决方案**：
1. 修改 `.env` 中的 PORT 值：
   ```env
   PORT=8081
   ```
2. 或停止占用该端口的其他服务
3. 重启 Docker Compose：
   ```bash
   docker compose down
   docker compose up -d
   ```

> `PORT` 一处改动即可：应用监听、compose 的端口映射（`${PORT:-8080}:${PORT:-8080}`）与健康检查探测的端口都跟随它，无需再改 `docker-compose.yml`。裸机部署同理，`PORT` 优先于 `config.json` 里的 `port`。

### 请求不支持的模型返回 400

**症状**：请求返回 `400`（`INVALID_MODEL_ID`）

**解决方案**：
1. **可用模型取决于账号订阅档位**：免费档（KIRO FREE）通常只授权 `claude-sonnet-4.5`，opus/GPT 等需更高档位
2. 用管理接口 `GET /api/admin/models` 查询模型目录（各账号上游能力的并集；并集为空时先回落到内置的 17 条静态目录，此时不反映档位授权）。协议侧 `GET /v1/models` 只是编译期写死的固定短清单，不读账号池、也不按档位过滤，**不能**拿它当可用性依据
3. 这是**确定性请求错误**，服务不会瞎重试、不会误伤账号，会把上游原因直接回给客户端

### 认证失败

**症状**：API 请求返回 `401 Unauthorized`

**解决方案**：
1. 检查 API Key 是否正确
2. 确保密钥走了鉴权闸接受的六条通道之一（优先级 `Authorization: Bearer` > `x-api-key` > `x-goog-api-key` > `?api_key=` > `?token=` > `?key=`）：
   ```bash
   -H "Authorization: Bearer sk-你的API密钥"
   # 或
   -H "x-api-key: sk-你的API密钥"
   # 或（Gemini 官方 SDK 默认走它）
   -H "x-goog-api-key: sk-你的API密钥"
   # 或在 URL 上带 ?api_key= / ?token= / ?key=
   ```
3. 检查 API Key 是否在 `.env` 或 `config.json` 中正确配置

### 账号被冷却

**症状**：管理面板显示账号处于冷却状态

**解决方案**：
1. body-aware 失败分类会区分处置：只有真正的凭据失效才永久禁用，配额/风控/限流一律**冷却自愈**
2. 等待冷却结束后账号自动恢复，无需操作
3. 若频繁被限流，可降低单账号压力：设置 `MAX_RPM_PER_CREDENTIAL` 或增加账号数量

### 无法连接 AWS 端点

**症状**：请求返回网络错误或超时

**解决方案**：
1. 检查网络连接：确认能访问 `*.amazonaws.com`
2. 服务内置端点回退：Kiro IDE → CodeWhisperer → AmazonQ 按序切换，`429`/网络错自动重试
3. 如需代理，编辑 `docker-compose.yml` 添加代理环境变量：
   ```yaml
   environment:
     - HTTP_PROXY=http://proxy:port
     - HTTPS_PROXY=http://proxy:port
   ```

## 性能优化

### 调整负载均衡策略

编辑 `.env` 或 `config.json`：

```env
# priority：等权轮询（默认）
# balanced：按每个账号的 weight 加权分配，适合账号档位不均时
LOAD_BALANCING_MODE=balanced
```

### 调整每账号 RPM 限流

```env
# 每账号每分钟请求上限（0 = 无限）
# 设置合理上限可降低单账号被上游限流的风险
MAX_RPM_PER_CREDENTIAL=60
```

### 增加账号数量

在 `credentials.json` 中追加更多账号，账号池会自动轮询并在冷却时跨账号重试，是提高吞吐与稳定性的最直接方式。

### 调整日志容量

```env
# 在 config.json 中设置（logCapacity 仅在此配置）
# 生产环境可适当调小以降低内存占用；设为 0 则关闭日志捕获
```

```json
{
  "logCapacity": 5000
}
```

## 监控和维护

### 查看系统信息

管理面板 `/admin` 仪表盘展示运行时间、全局剩余积分、系统信息（版本 / Rust / OS / 内存 / CPU / PID / 运行模式），凭 `adminApiKey` 登录后即可查看。

### 查看用量统计

管理面板「用量统计」页按每日/账号维度展示，含客户端 IP 与账号标签，可按日下钻。用户面板 `/user` 则供 API-KEY 持有者用自己的 key 自助查询额度与记录。

### 查看余额

管理面板账号管理页可查询各账号余额；仪表盘展示全局剩余积分（带缓存，TTL 内不重复查询上游）。

### 查看实时日志

管理面板「实时日志」页提供结构化表格、方向过滤、搜索、分页、SSE 实时推送与下载；也可在终端查看容器日志：

```bash
docker compose logs -f --tail=50
```

## 升级服务

```bash
# 拉取最新代码
git pull origin main

# 拉取最新镜像（挂载卷 ./data 属主由 entrypoint 自动修正）
docker compose pull

# 重启服务
docker compose up -d

# 查看升级日志
docker compose logs -f
```

> CI：推送 `v*` 标签会构建多架构镜像并发布到 GHCR（tag = `X.Y.Z` + `X.Y` + `latest`）。也可在管理面板「设置」页使用**检查更新**（与 GitHub Release 比对）。

## 备份和恢复

### 备份数据

```bash
# 备份 data 目录（包含凭据、配置、运行态等）
tar -czf kiro2api-backup-$(date +%Y%m%d).tar.gz data/

# 备份 .env 文件
cp .env .env.backup
```

### 恢复数据

```bash
# 恢复 data 目录
tar -xzf kiro2api-backup-20260719.tar.gz

# 恢复 .env 文件
cp .env.backup .env

# 重启服务
docker compose restart
```

## 裸机 / 本地运行

无需 Docker 时，可用 `cargo` 直接构建运行（需 Rust 2024 edition）：

```bash
# 编译 release 版本
cargo build --release

# 启动
API_KEY=sk-xxx ./target/release/kiro2api \
  -c data/config.json \
  --credentials data/credentials.json
```

> 配置优先级：**命令行参数 > 环境变量 > `config.json` > 内置默认**。`--credentials` 不给时由 `CREDENTIALS_PATH` / `config.json` 的 `credentialsPath` / 内置默认的 `credentials.json`（就近解析到 `-c` 所指配置文件的目录；`-c` 只给了无目录的文件名时才相对当前工作目录）决定；用量统计、`api_keys.json` 与余额缓存都落在该文件的父目录里。

> [!TIP]
> 裸机部署请勿轻易把 `HOST` 改成 `0.0.0.0`。`/admin`、`/user` 面板本体始终不鉴权，`/api/admin/*` 只有在配置了 `adminApiKey`（缺省回退 `apiKey`）之后才受保护——一个都不配时管理接口对所有人开放；`/api/user/*` 不走该闸，始终要求调用方自带有效 API-KEY（无效/停用/过期即 401）。

## 上线切换建议

生产上线建议先在旁路端口起新镜像，与线上并行比对（相同请求 → 输出一致）通过后再切换，旧镜像留盘可回滚。

## 安全建议

1. **务必设置 API Key**：`API_KEY` 留空时协议端点开放访问，对外部署务必设置（注意 `.env` 里的空值 `API_KEY=` 会覆盖 `config.json` 的配置，不用就整行注释掉）
2. **保护管理端**：对外暴露时务必设置 `adminApiKey`（或至少 `apiKey`），否则 `/api/admin/*` 可被用来管理凭据、密钥与设置
3. **保护凭据**：不要将 `.env`、`credentials.json` 提交到 Git
4. **谨慎绑定地址**：裸机部署慎改 `HOST=0.0.0.0`，面板本体尚未接鉴权
5. **限制访问**：在生产环境中使用防火墙限制 API 访问
6. **使用 HTTPS**：在生产环境中使用 HTTPS 反向代理（如 Nginx）

## 获取帮助

- 查看 [README](../../README.md) 了解项目概况
- 查看 [USAGE](USAGE.md) 了解使用方法
- 查看 [API](API.md) 了解 API 文档
- 提交 Issue：[GitHub Issues](https://github.com/xwteam/kiro2api/issues)
