# 使用指南

本文档详细说明如何使用 kiro2api 服务。

## Web 管理面板

### 访问面板

启动服务后，在浏览器中访问：

```
http://localhost:8080/admin
```

或使用服务器 IP：

```
http://服务器IP:8080/admin
```

### 登录

首次访问需要输入 `adminApiKey` 进行登录（未单独设置时回退使用 `apiKey`）。密钥在 `.env` 或 `config.json` 中配置，首次启动若为空会在日志中告警。

### 仪表盘

仪表盘显示服务的实时状态和概览信息。

#### 系统信息卡片

显示以下信息：
- **版本**：kiro2api 版本号
- **Rust 版本**：构建所用的 Rust 版本
- **操作系统**：服务器操作系统和内核版本
- **内存使用**：当前内存占用 / 总内存
- **CPU 使用率**：实时 CPU 使用百分比
- **进程 ID**：服务进程 ID
- **运行模式**：Docker 或直接运行
- **运行时间**：服务启动以来的实时计时

#### 二维码卡片

显示赞助二维码图片和文字配置，支持：
- 点击图片放大查看
- 实时拉取远程配置
- 修改无需重建容器

#### 账号状态总览

显示账号池的实时状态：
- 总账号数
- 活跃账号数
- 全局剩余积分
- 负载均衡策略
- 每个账号的状态、权重与请求计数

#### 检查更新

对比 GitHub Release，提示是否有新版本可用。

### 账号管理（凭据）

#### 查看账号列表

显示所有已配置的 Kiro（CodeWhisperer）凭据及其状态：
- 账号 ID
- 标签
- 状态（健康度、是否冷却中）
- 权重与优先级
- 失败 / 限流计数
- 余额

#### 添加账号

kiro2api 支持三种交互式登录流，无需手动拼接凭据：

- **Builder ID**：设备码授权流
- **IAM Identity Center（SSO）**：授权码流
- **社交令牌**：直接导入社交登录凭据

也可以选择批量导入：每行一个 bearer / SSO token，或粘贴凭据数组 / `{accounts}` 对象。

#### 更新账号

选择账号可调整优先级 / 权重、启停、重置冷却状态。修改运行期即时生效，无需重启服务。

#### 删除账号

选择账号，点击"删除"按钮确认删除。

> [!NOTE]
> 令牌到期无需手动更新：kiro2api 会自动内存刷新并原子落盘 `credentials.json`。只有真正的凭据失效才会永久禁用，配额 / 风控 / 限流一律进入分级冷却后自愈。

### 集成示例

设置页内置"集成示例"面板，按**协议 × 语言**组合生成可直接复制的代码片段（OpenAI / Anthropic / Gemini SDK 及 cURL），填好 `base_url` 与密钥即可使用。

### 实时日志

显示结构化日志，支持：
- **方向过滤**：查看最新日志或最早日志
- **文本搜索**：按关键词搜索日志
- **分页显示**：分页浏览记录
- **SSE 实时推送**：新日志实时滚动
- **快照下载**：导出 `.txt` 日志

> [!NOTE]
> 日志功能需 `logCapacity > 0`（默认 `1000`）。设为 `0` 时关闭日志捕获，日志端点返回 503。

### 使用统计

显示服务的使用统计信息：
- 每日维度用量汇总
- 单账号维度用量汇总（含客户端 IP 与账号标签）
- 失败 / 限流日志
- 实时 RPM 视图
- 按日下钻

### API Key 管理

集中管理发放给调用方的对外 API-KEY：

#### 添加 API Key

1. 点击"添加 Key"按钮
2. 设置消费上限 / 有效期（可选）
3. 设置标签（可选）
4. 点击保存

#### 用量与记录

- 查看或清零单个 key 的累计用量
- 按模型细分统计
- 浏览分页请求记录

#### 启用/禁用

点击 Key 行的开关按钮，可快速启用或禁用该 Key。

### 设置

可视化管理运行时配置，修改即时生效。

#### 负载均衡

- **负载均衡模式**：`priority`（等权轮询）或 `balanced`（按 `weight` 加权）
- 运行期切换，无需重启

#### 密钥轮换

- 运行期轮换 `apiKey` / `adminApiKey`，即时生效、无需重启
- `server-info` 显示脱敏后的主 key 与 kiro2api 版本

### 主题切换

顶部控制栏点击主题按钮，在深色和浅色主题之间切换。

### 服务重启

顶部控制栏点击重启按钮，可一键重启服务。

### 登出

顶部控制栏点击登出按钮，退出登录。

## 用户面板

除了管理面板，kiro2api 还内置面向 API-KEY 持有者的用户面板。

### 访问面板

把以下地址发给 key 持有者：

```
http://服务器IP:8080/user
```

### 登录

持有者用**自己的 API-KEY**（无需 admin 权限）登录。

### 功能

- 查看该 key 的额度与剩余量
- 按模型细分的累计用量
- 分页请求记录

由 `/api/user/*` 驱动，绝不暴露其它 key 或管理操作。

## 图片上传

kiro2api 支持多模态内容，包括图片输入。支持三种 API 格式的图片传输。

### OpenAI 格式

在 `messages` 数组中使用 `image_url` 类型，支持 Base64 Data URI 和远程 HTTP URL：

**Base64 图片示例**：

```bash
curl -X POST http://localhost:8080/v1/chat/completions \
  -H "Content-Type: application/json" \
  -H "Authorization: Bearer sk-你的API密钥" \
  -d '{
    "model": "claude-sonnet-4.5",
    "messages": [
      {
        "role": "user",
        "content": [
          {"type": "text", "text": "这是什么"},
          {
            "type": "image_url",
            "image_url": {
              "url": "data:image/png;base64,iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mNk+M9QDwADhgGAWjR9awAAAABJRU5ErkJggg=="
            }
          }
        ]
      }
    ]
  }'
```

**远程 URL 图片示例**：

```bash
curl -X POST http://localhost:8080/v1/chat/completions \
  -H "Content-Type: application/json" \
  -H "Authorization: Bearer sk-你的API密钥" \
  -d '{
    "model": "claude-sonnet-4.5",
    "messages": [
      {
        "role": "user",
        "content": [
          {"type": "text", "text": "分析这张图片"},
          {
            "type": "image_url",
            "image_url": {
              "url": "https://example.com/image.jpg"
            }
          }
        ]
      }
    ]
  }'
```

### Claude 格式

在 `content` 数组中使用 `image` 类型：

```bash
curl -X POST http://localhost:8080/v1/messages \
  -H "Content-Type: application/json" \
  -H "Authorization: Bearer sk-你的API密钥" \
  -d '{
    "model": "claude-sonnet-4.5",
    "max_tokens": 1024,
    "messages": [
      {
        "role": "user",
        "content": [
          {"type": "text", "text": "这是什么"},
          {
            "type": "image",
            "source": {
              "type": "base64",
              "media_type": "image/png",
              "data": "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mNk+M9QDwADhgGAWjR9awAAAABJRU5ErkJggg=="
            }
          }
        ]
      }
    ]
  }'
```

### Gemini 原生格式

在 `parts` 数组中使用 `inlineData`：

```bash
curl -X POST http://localhost:8080/v1beta/models/claude-sonnet-4.5:generateContent \
  -H "Content-Type: application/json" \
  -H "Authorization: Bearer sk-你的API密钥" \
  -d '{
    "contents": [
      {
        "parts": [
          {"text": "这是什么"},
          {
            "inlineData": {
              "mimeType": "image/png",
              "data": "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mNk+M9QDwADhgGAWjR9awAAAABJRU5ErkJggg=="
            }
          }
        ]
      }
    ]
  }'
```

图片在四种协议间统一转换后进入 Anthropic 中枢母格式，再透传给 Kiro 后端。

## 支持的模型

### 模型可用性取决于订阅档位

kiro2api 的后端是 Kiro（CodeWhisperer）账号池，**可用模型取决于账号订阅档位**。请求前建议先 list-then-use，用 `/v1/models`（或 `/claude/v1/models`、`/v1beta/models`）查询本服务实际可服务的模型 id。

| 档位 | 通常可用模型 |
|---------|------|
| 免费档（KIRO FREE） | `claude-sonnet-4.5` |
| 更高档位 | 在 Sonnet 之外授权 opus / GPT 等更多模型 |

> [!IMPORTANT]
> 请求不支持的模型会明确返回 `400`（`INVALID_MODEL_ID`），而非静默失败。这类确定性错误**不会瞎重试、不会误伤账号**，上游原因会直接回给客户端。

### 模型名映射

- 客户端传入的模型名按**小写子串**匹配到 Kiro 内部模型，未匹配到返回 `400`
- `/models` 端点返回本服务实际可服务的模型 id，建议客户端 list-then-use

## 第三方客户端接入

### ChatGPT-Next-Web

1. 部署 ChatGPT-Next-Web
2. 打开设置页面
3. 在"API 设置"中填入：
   - **API 地址**：`http://服务器IP:8080/v1`（或 `/openai/v1`）
   - **API Key**：`sk-你的API密钥`
4. 选择模型为 `claude-sonnet-4.5` 或其他可用模型
5. 开始对话

### LobeChat

1. 部署 LobeChat
2. 打开设置页面
3. 在"模型提供商"中选择"OpenAI"
4. 填入：
   - **API 地址**：`http://服务器IP:8080/v1`
   - **API Key**：`sk-你的API密钥`
5. 选择模型
6. 开始对话

### OpenCat

1. 打开 OpenCat 应用
2. 进入设置
3. 添加自定义 API 端点：
   - **API 地址**：`http://服务器IP:8080/v1`
   - **API Key**：`sk-你的API密钥`
4. 选择模型
5. 开始对话

### 通用 OpenAI 兼容客户端

任何支持自定义 API 端点的 OpenAI 兼容客户端都可以使用：

```python
from openai import OpenAI

client = OpenAI(
    api_key="sk-你的API密钥",
    base_url="http://服务器IP:8080/v1"
)

response = client.chat.completions.create(
    model="claude-sonnet-4.5",
    messages=[{"role": "user", "content": "Hello"}]
)
```

## 令牌自愈与容错

kiro2api 内置一整套账号池自愈机制，无需人工看守凭据。

### 令牌自动刷新

- token 到期**自动内存刷新**，采用单飞协调，避免并发刷新级联 401
- 刷新成功后原子落盘 `credentials.json`
- 无需手动更新 accessToken

### 端点回退

上游按序回退，`429` / 网络错自动切换到下一个端点：

```
Kiro IDE → CodeWhisperer → AmazonQ
```

### 跨账号重试

- 账号级失败自动跨账号重试
- 确定性请求错误（如不支持的模型 `INVALID_MODEL_ID`）**不瞎重试、不误伤账号**，直接把上游原因回给客户端

### 失败分类（body-aware）

服务会按类别差异化处置连续失败：

| 类别 | 处置 |
|------|------|
| 永久失效（真正的凭据失效） | 永久禁用账号 |
| 歧义鉴权 / 配额 / 风控 / 限流 / 瞬时 | 分级冷却后自愈 |

只有真正的凭据失效才永久禁用，其余一律冷却自愈。

## 多语言支持

Web 面板支持多种语言，点击顶部控制栏切换：
- 简体中文
- 繁體中文
- English
- 日本語
- 한국어

## 对话上下文

### 客户端自行维护历史

kiro2api **无服务端会话记忆**，请把完整对话历史随每次请求带上。客户端 SDK 会自动维护对话历史：

```python
messages = [
    {"role": "user", "content": "第一条消息"},
    {"role": "assistant", "content": "回复"},
    {"role": "user", "content": "第二条消息"}
]
```

### Responses 的 previous_response_id

OpenAI Responses 协议的 `previous_response_id` **不支持（会返回 400）**：本服务不保存服务端会话，请在 `input` / `messages` 中携带完整上下文。

## 流式和非流式请求

### 流式请求

设置 `stream: true` 获取实时流式响应：

```bash
curl -X POST http://localhost:8080/v1/chat/completions \
  -H "Authorization: Bearer sk-你的API密钥" \
  -d '{
    "model": "claude-sonnet-4.5",
    "messages": [{"role": "user", "content": "写一首诗"}],
    "stream": true
  }'
```

OpenAI Chat 流式返回 `chat.completion.chunk` 行并以 `data: [DONE]` 收尾；Anthropic / Gemini / Responses 各按自身协议输出（Responses 为命名事件 + 单调 `sequence_number`，无 `[DONE]`）。

### 非流式请求

设置 `stream: false` 获取完整响应：

```bash
curl -X POST http://localhost:8080/v1/chat/completions \
  -H "Authorization: Bearer sk-你的API密钥" \
  -d '{
    "model": "claude-sonnet-4.5",
    "messages": [{"role": "user", "content": "写一首诗"}],
    "stream": false
  }'
```

> [!NOTE]
> `stream: false` 时服务内部仍解码 AWS eventstream 事件流，收集完毕后一次性返回完整 JSON。

## 函数调用

支持四种协议的工具调用，且在协议间**真透传**（Anthropic `tool_use` / OpenAI `tool_calls` / Gemini `functionCall`），不做模拟：

```python
response = client.chat.completions.create(
    model="claude-sonnet-4.5",
    messages=[{"role": "user", "content": "北京今天天气怎么样"}],
    tools=[{
        "type": "function",
        "function": {
            "name": "get_weather",
            "description": "获取指定城市的天气",
            "parameters": {
                "type": "object",
                "properties": {
                    "city": {"type": "string", "description": "城市名称"}
                },
                "required": ["city"]
            }
        }
    }]
)
```

## 常见使用场景

### 场景 1：简单对话

```python
from openai import OpenAI

client = OpenAI(
    api_key="sk-你的API密钥",
    base_url="http://localhost:8080/v1"
)

response = client.chat.completions.create(
    model="claude-sonnet-4.5",
    messages=[{"role": "user", "content": "你好"}]
)

print(response.choices[0].message.content)
```

### 场景 2：流式对话

```python
for chunk in client.chat.completions.create(
    model="claude-sonnet-4.5",
    messages=[{"role": "user", "content": "写一个 Python 快速排序"}],
    stream=True
):
    if chunk.choices[0].delta.content:
        print(chunk.choices[0].delta.content, end="")
```

### 场景 3：多轮对话

```python
messages = []

# 第一轮
messages.append({"role": "user", "content": "什么是机器学习"})
response = client.chat.completions.create(
    model="claude-sonnet-4.5",
    messages=messages
)
messages.append({"role": "assistant", "content": response.choices[0].message.content})

# 第二轮
messages.append({"role": "user", "content": "能举个例子吗"})
response = client.chat.completions.create(
    model="claude-sonnet-4.5",
    messages=messages
)
messages.append({"role": "assistant", "content": response.choices[0].message.content})

print(messages[-1]["content"])
```

### 场景 4：使用 Claude SDK

```python
import anthropic

client = anthropic.Anthropic(
    api_key="sk-你的API密钥",
    base_url="http://localhost:8080"
)

message = client.messages.create(
    model="claude-sonnet-4.5",
    max_tokens=1024,
    messages=[{"role": "user", "content": "Hello"}]
)

print(message.content[0].text)
```

### 场景 5：使用 Gemini SDK

```python
from google import genai

client = genai.Client(
    api_key="sk-你的API密钥",
    http_options={"base_url": "http://localhost:8080/v1beta"}
)

resp = client.models.generate_content(
    model="claude-sonnet-4.5",
    contents="Hello"
)

print(resp.text)
```

## 故障排查

### 请求返回 401

**原因**：API Key 无效或未提供

**解决**：
1. 检查 API Key 是否正确
2. 确保请求头中包含 `Authorization: Bearer sk-xxx`（或 `x-api-key: sk-xxx`、`?token=sk-xxx`）
3. 检查 API Key 是否在 `.env` 或 `config.json` 中正确配置

### 请求返回 400（INVALID_MODEL_ID）

**原因**：请求的模型不在当前账号订阅档位授权范围内

**解决**：
1. 先 `GET /v1/models` 查询实际可用的模型 id
2. 免费档（KIRO FREE）通常只授权 `claude-sonnet-4.5`
3. 需要 opus / GPT 等更高档模型时，升级账号订阅

### 所有账号不可用

**原因**：账号被冷却或凭据失效

**解决**：
1. 打开管理面板查看凭据状态与冷却情况
2. 令牌到期会自动内存刷新并原子落盘，一般无需干预
3. 若真正凭据失效被永久禁用，通过三种交互式登录流重新纳入账号

### 响应缓慢

**原因**：
1. 网络延迟（需能访问 `*.amazonaws.com`）
2. 账号被限流进入冷却
3. 服务器资源不足

**解决**：
1. 增加账号数量
2. 调整 `MAX_RPM_PER_CREDENTIAL` 与负载均衡策略
3. 增加服务器资源

### 对话上下文丢失

**原因**：本服务无服务端会话记忆，未在请求中携带完整历史

**解决**：
1. 在客户端维护完整的 `messages` 历史并随每次请求带上
2. OpenAI Responses 的 `previous_response_id` 不支持（会 400），请改带完整上下文

## 获取帮助

- 查看 [DEPLOY.md](DEPLOY.md) 了解部署方法
- 查看 [API.md](API.md) 了解 API 文档
- 查看 [README.md](../../README.md) 了解项目概况
- 提交 Issue：[GitHub Issues](https://github.com/xwteam/kiro2api/issues)
