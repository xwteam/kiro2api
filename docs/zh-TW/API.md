# API 文檔

本文檔詳細說明 kiro2api 的所有 API 端點、請求格式和回應格式。kiro2api 是多協議 AI 中轉，後端為 Kiro（CodeWhisperer），統一提供 Claude 系模型。

## 認證

所有協議端點都走統一驗證。支援三種攜帶方式（三選一）：

**方式 1：Authorization Header（推薦，相容 OpenAI / Anthropic SDK）**
```bash
curl -H "Authorization: Bearer sk-your-api-key" http://localhost:8080/...
```

**方式 2：x-api-key Header**
```bash
curl -H "x-api-key: sk-your-api-key" http://localhost:8080/...
```

**方式 3：查詢參數 `?token=`**
```bash
curl "http://localhost:8080/...?token=sk-your-api-key"
```

> **注意：** API Key 由部署者透過 `API_KEY` 環境變數或 `config.json` 的 `apiKey` 設定。金鑰以常量時間比較。`apiKey` 留空時協議端點**開放訪問**（啟動會告警），對外部署務必設置。`/health`、`/v1/ping` 探活端點不需驗證。

## 路徑說明

每家介面同時支援兩套路徑：

**帶前綴路徑（四家明確區分）：**
- OpenAI：`/openai/v1`
- Claude：`/claude/v1`
- Gemini：`/gemini/v1beta`

**標準裸路徑（主流 SDK 填 base_url 無需加後綴開箱即用）：**
- OpenAI：`/v1/chat/completions`、`/v1/models`
- OpenAI Responses：`/v1/responses`
- Claude：`/v1/messages`、`/v1/messages/count_tokens`
- Gemini：`/v1beta/models/{model}:generateContent`、`:streamGenerateContent`、`/v1beta/models`

> **重要：** 裸 `/v1/models` 回傳 OpenAI 格式（同一路徑無法同時回傳兩種格式）；需要 Claude 格式的模型列表請用 `/claude/v1/models`。內部以 **Anthropic Messages 為中樞母格式**，其餘協議雙向轉換後複用同一條中轉內核。

## OpenAI 相容 API（`/v1` 或 `/openai/v1`）

### GET /models

列出本服務實際可服務的模型 id。

**請求：**
```bash
curl http://localhost:8080/v1/models \
  -H "Authorization: Bearer sk-your-api-key"
```

**回應：**
```json
{
  "object": "list",
  "data": [
    {
      "id": "claude-sonnet-4.5",
      "object": "model",
      "created": 1715970000,
      "owned_by": "kiro"
    }
  ]
}
```

> 💡 **模型選擇建議**：**可用模型取決於帳號訂閱檔位**。
> - 免費檔（KIRO FREE）通常只授權 `claude-sonnet-4.5`，適合絕大多數對話與 agent 場景，推薦作為預設選擇。
> - opus / GPT 等模型需更高檔位授權。
> - 請求不支援的模型會明確返回 `400`（`INVALID_MODEL_ID`），而非靜默失敗，也**不會瞎重試或誤傷帳號**。
>
> 傳入的模型名以**小寫子字串**比對到 Kiro 內部模型（未匹配到 → `400`）。建議客戶端先呼叫 `/models` 列出再使用（list-then-use）。本服務的串流介面為真正的增量串流，首字一生成即開始推送。

### POST /chat/completions

發送對話請求，支援流式和非流式回應。

**請求體：**
```json
{
  "model": "claude-sonnet-4.5",
  "messages": [
    {"role": "user", "content": "你好"}
  ],
  "stream": false,
  "temperature": 0.7,
  "max_tokens": 2048,
  "tools": [],
  "tool_choice": "auto"
}
```

**參數說明：**

| 參數 | 類型 | 必填 | 說明 |
|------|------|------|------|
| `model` | string | ✅ | 模型名稱（如 claude-sonnet-4.5） |
| `messages` | array | ✅ | 訊息陣列，每個訊息包含 role 和 content。`content` 可以是字串或物件陣列（支援多模態）；`role:"tool"` 攜帶工具結果 |
| `stream` | boolean | ❌ | 是否流式輸出（預設 false） |
| `temperature` | number | ❌ | 溫度參數（預設 0.7） |
| `max_tokens` | number | ❌ | 最大回應 token 數 |
| `tools` | array | ❌ | 函數定義陣列（巢狀格式 `{"type":"function","function":{...}}`） |
| `tool_choice` | string | ❌ | 工具選擇策略（auto/required/none） |

**多模態 content 格式**：

`content` 可以是字串（純文字）或物件陣列（支援文字和圖片）：

```json
{
  "role": "user",
  "content": [
    {"type": "text", "text": "這是什麼"},
    {
      "type": "image_url",
      "image_url": {
        "url": "data:image/png;base64,iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mNk+M9QDwADhgGAWjR9awAAAABJRU5ErkJggg=="
      }
    }
  ]
}
```

支援的 content 類型：
- `text`：純文字內容
- `image_url`：圖片，支援 Base64 Data URI（`data:image/...;base64,...`）

**非流式回應：**
```json
{
  "id": "chatcmpl-xxx",
  "object": "chat.completion",
  "created": 1715970000,
  "model": "claude-sonnet-4.5",
  "choices": [
    {
      "index": 0,
      "message": {
        "role": "assistant",
        "content": "你好！有什麼我可以幫助你的嗎？"
      },
      "finish_reason": "stop"
    }
  ],
  "usage": {
    "prompt_tokens": 10,
    "completion_tokens": 20,
    "total_tokens": 30
  }
}
```

工具呼叫時，`message` 內含 `tool_calls`，`finish_reason` 為 `"tool_calls"`。

**流式回應（SSE 格式，`chat.completion.chunk`）：**
```
data: {"choices":[{"delta":{"role":"assistant"},"index":0}]}
data: {"choices":[{"delta":{"content":"你"},"index":0}]}
data: {"choices":[{"delta":{"content":"好"},"index":0}]}
data: [DONE]
```

首幀帶 `delta.role`，末幀帶 `finish_reason`，以 `data: [DONE]` 收尾。

### POST /responses

OpenAI Responses API。為需要新版 Responses 協議（而非 Chat Completions）的客戶端提供支援——例如 **Codex CLI**，它在 2026 年 2 月起砍掉了對 Chat Completions 的支援，要把 Codex CLI 接到 kiro2api 就得靠這個介面。支援文字對話、流式輸出、工具（函數）呼叫。

**請求**：
```bash
curl -X POST http://localhost:8080/v1/responses \
  -H "Content-Type: application/json" \
  -H "Authorization: Bearer sk-your-api-key" \
  -d '{
    "model": "claude-sonnet-4.5",
    "input": "1+1等於幾？",
    "stream": false
  }'
```

**請求體**：

| 參數 | 類型 | 必填 | 說明 |
|------|------|------|------|
| `model` | string | ✅ | 模型名稱，如 `claude-sonnet-4.5` |
| `input` | string 或 array | ✅ | 字串（等同一條 user 訊息的簡寫），或輸入條目陣列（見下） |
| `instructions` | string | ❌ | 系統/開發者前置說明，加在對話最前面（→ system） |
| `stream` | boolean | ❌ | 是否流式回傳，預設 false |
| `tools` | array | ❌ | 函數呼叫工具定義，**扁平格式**：`{"type":"function","name","description","parameters"}`（注意：跟 Chat Completions 的巢狀格式 `{"type":"function","function":{...}}` 不一樣） |
| `tool_choice` | string 或 object | ❌ | `auto`、`none`、`required`，或 `{"type":"function","name":"..."}` 指定必須呼叫某個工具 |

**`input` 陣列條目類型**：
- `{"type":"message","role":"user"|"assistant"|"system","content":[...]}` —— 內容區塊：`{"type":"input_text","text":...}`、`{"type":"input_image","image_url":"..."}`、`{"type":"output_text","text":...}`
- `{"type":"function_call","call_id","name","arguments"}` —— 歷史裡助手呼叫工具的那一輪（多輪續聊需要客戶端自己重發完整歷史）
- `{"type":"function_call_output","call_id","output"}`（或 `"tool_result"`）—— 客戶端回傳的工具執行結果

**明確不支援（會報錯，不會假裝支援）**：`previous_response_id`——本服務不儲存伺服器端對話狀態，傳了這個欄位會回傳 400 `invalid_request_error`，而不是悄悄忽略。請每次請求都在 `input` 裡帶上完整對話歷史（Codex CLI 本身就是這麼做的）。

**回應（非流式）**：
```json
{
  "id": "resp_xxx",
  "object": "response",
  "created_at": 1715970000,
  "status": "completed",
  "model": "claude-sonnet-4.5",
  "output": [
    {
      "id": "msg_xxx",
      "type": "message",
      "role": "assistant",
      "status": "completed",
      "content": [
        {"type": "output_text", "text": "1+1等於2", "annotations": []}
      ]
    }
  ],
  "usage": {
    "input_tokens": 10,
    "input_tokens_details": {"cached_tokens": 0},
    "output_tokens": 5,
    "output_tokens_details": {"reasoning_tokens": 0},
    "total_tokens": 15
  },
  "previous_response_id": null,
  "instructions": null,
  "error": null
}
```

**回應（流式）**：嚴格按官方協議順序發送帶命名的 SSE 事件，每個事件都帶遞增的 `sequence_number`。**沒有** `data: [DONE]` 結尾標記（那是 Chat Completions 的老約定）——完成訊號是 `response.completed`（失敗是 `response.failed`）：

```
event: response.created
data: {"type":"response.created","sequence_number":0,"response":{...}}

event: response.in_progress
data: {"type":"response.in_progress","sequence_number":1,...}

event: response.output_item.added
data: {"type":"response.output_item.added","sequence_number":2,...}

event: response.content_part.added
data: {"type":"response.content_part.added","sequence_number":3,...}

event: response.output_text.delta
data: {"type":"response.output_text.delta","sequence_number":4,"delta":"1"}

event: response.output_text.done
data: {"type":"response.output_text.done","sequence_number":5,"text":"1+1等於2"}

event: response.content_part.done
data: {"type":"response.content_part.done","sequence_number":6,...}

event: response.output_item.done
data: {"type":"response.output_item.done","sequence_number":7,...}

event: response.completed
data: {"type":"response.completed","sequence_number":8,"response":{...}}
```

工具呼叫場景下，`response.output_item.added`（類型 `function_call`）之後跟的是 `response.function_call_arguments.delta` / `response.function_call_arguments.done` / `response.output_item.done`，而不是上面的文字事件。

**工具呼叫範例**：
```bash
curl -X POST http://localhost:8080/v1/responses \
  -H "Content-Type: application/json" \
  -H "Authorization: Bearer sk-your-api-key" \
  -d '{
    "model": "claude-sonnet-4.5",
    "input": "查一下巴黎的天氣",
    "tools": [
      {
        "type": "function",
        "name": "get_weather",
        "description": "取得指定城市的天氣",
        "parameters": {
          "type": "object",
          "properties": {
            "city": {"type": "string"}
          },
          "required": ["city"]
        }
      }
    ]
  }'
```
回應 `output` 裡會含有一個 `function_call` 條目：
```json
{"id": "fc_xxx", "type": "function_call", "status": "completed", "call_id": "call_xxx", "name": "get_weather", "arguments": "{\"city\": \"巴黎\"}"}
```

## Claude 相容 API（`/v1` 對話入口；`/claude/v1` 顯式前綴）

### GET /models

列出所有可用模型（Anthropic 形狀）。裸 `/v1/models` 回傳 OpenAI 格式，故 Claude 形狀請走 `/claude/v1/models`（避開與 OpenAI 衝突）。

**請求：**
```bash
curl http://localhost:8080/claude/v1/models \
  -H "Authorization: Bearer sk-your-api-key"
```

**回應：**
```json
{
  "data": [
    {
      "id": "claude-sonnet-4.5",
      "type": "model",
      "display_name": "Claude Sonnet 4.5"
    }
  ]
}
```

### POST /messages

發送訊息請求（Claude 格式，中樞母格式）。

**請求體：**
```json
{
  "model": "claude-sonnet-4.5",
  "max_tokens": 1024,
  "system": "你是一個有幫助的助手",
  "messages": [
    {"role": "user", "content": "你好"}
  ],
  "stream": false
}
```

`content` 支援字串或區塊陣列（`text` / `image` / `tool_use` / `tool_result`）。

**回應：**
```json
{
  "id": "msg-xxx",
  "type": "message",
  "role": "assistant",
  "content": [
    {
      "type": "text",
      "text": "你好！有什麼我可以幫助你的嗎？"
    }
  ],
  "model": "claude-sonnet-4.5",
  "stop_reason": "end_turn",
  "usage": {
    "input_tokens": 10,
    "output_tokens": 20
  }
}
```

串流為 Anthropic 標準 SSE（`message_start` → `content_block_start` → `content_block_delta` → … → `message_stop`）。工具走 `tool_use` 區塊與 `input_json_delta`。顯式前綴變體：`POST /claude/v1/messages`。

### POST /messages/count_tokens

估算訊息的 token 數（粗略估算）。

**請求體：**
```json
{
  "model": "claude-sonnet-4.5",
  "messages": [
    {"role": "user", "content": "你好"}
  ]
}
```

**回應：**
```json
{
  "input_tokens": 10
}
```

顯式前綴變體：`POST /claude/v1/messages/count_tokens`。

## Gemini 原生 API（`/v1beta` 或 `/gemini/v1beta`）

### GET /models

列出所有可用模型。

**請求：**
```bash
curl http://localhost:8080/v1beta/models \
  -H "Authorization: Bearer sk-your-api-key"
```

### POST /models/{model}:generateContent

生成內容（非流式）。全欄位 **camelCase**。

**請求體：**
```json
{
  "contents": [
    {
      "role": "user",
      "parts": [{"text": "你好"}]
    }
  ],
  "systemInstruction": {
    "parts": [{"text": "你是一個有幫助的助手"}]
  },
  "generationConfig": {
    "temperature": 0.7,
    "maxOutputTokens": 2048
  }
}
```

`parts[]` 支援 `text` 與 `inline_data`（多模態）；`tools[].function_declarations` 定義函數。

**回應：**
```json
{
  "candidates": [
    {
      "content": {
        "role": "model",
        "parts": [{"text": "你好！有什麼我可以幫助你的嗎？"}]
      },
      "finishReason": "STOP"
    }
  ],
  "usageMetadata": {
    "promptTokenCount": 10,
    "candidatesTokenCount": 20,
    "totalTokenCount": 30
  }
}
```

工具呼叫時回傳 `functionCall`。

### POST /models/{model}:streamGenerateContent

流式生成內容（SSE 形態，`?alt=sse`，camelCase，無 `[DONE]`）。

**請求體：**
```json
{
  "contents": [
    {
      "role": "user",
      "parts": [{"text": "你好"}]
    }
  ]
}
```

> Gemini / OpenAI 客戶端一律用本服務的**統一驗證**（Bearer / `x-api-key` / `?token=`），不是廠商原生的 `?key=` / `x-goog-api-key`。

## 管理 API（`/admin` · `/api/admin/*`）

`/admin` 管理面板（靜態，rust-embed 嵌入）由 `/api/admin/*` 介面驅動。下列端點均需 `adminApiKey`（未設則回退 `apiKey`；兩者皆空時管理 API 開放——切勿如此對外暴露）。驗證攜帶方式同協議閘（`Authorization: Bearer` / `x-api-key` / `?token=`；無法設標頭的 SSE 日誌流用 `?api_key=`）。回應體一律 camelCase，**絕不含 access/refresh token 或任何金鑰**。

### GET /api/admin/credentials

取得帳號池狀態（同時作為隱式「登入校驗」面——回傳 200 即視為 key 有效）。

**請求：**
```bash
curl http://localhost:8080/api/admin/credentials \
  -H "Authorization: Bearer sk-your-admin-key"
```

**回應：**
```json
{
  "total": 2,
  "available": 2,
  "currentId": 12345,
  "credentials": [
    {
      "id": 12345,
      "priority": 1,
      "weight": 1,
      "disabled": false,
      "failureCount": 0,
      "isCurrent": true,
      "expiresAt": "2026-07-25T12:00:00Z",
      "authMethod": "social",
      "hasProfileArn": true,
      "successCount": 150,
      "lastUsedAt": "2026-07-25T10:30:00Z",
      "healthStatus": "healthy",
      "throttleCount": 0
    }
  ]
}
```

### POST /api/admin/credentials

新增一條憑證入池並落盤。

### PUT /api/admin/credentials/{id}

更新既有憑證。

### DELETE /api/admin/credentials/{id}

從池中移除憑證。

**請求：**
```bash
curl -X DELETE http://localhost:8080/api/admin/credentials/12345 \
  -H "Authorization: Bearer sk-your-admin-key"
```

### POST /api/admin/credentials/{id}/disabled

手動啟停帳號。

**請求體：**
```json
{"disabled": true}
```

**回應：**
```json
{"success": true, "message": "..."}
```

### POST /api/admin/credentials/{id}/priority

設定帳號優先級 / 權重。

### POST /api/admin/credentials/{id}/reset

清失敗計數 / 冷卻。

### POST /api/admin/credentials/batch-import

批次匯入憑證。接受陣列、KAM `{accounts}` 物件或單物件；逐條規整 / 校驗 / 落盤，回傳逐項結果與計數。

### 互動式登入 / 匯入

無需手改 `credentials.json` 即可納入新 Kiro 帳號。

- `POST /api/admin/login/builderid/start` → `POST /api/admin/login/builderid/poll` —— AWS Builder ID 裝置碼流；poll 回傳 `{success,completed,status,interval?,credentialId?,email?}`，成功即落庫。
- `POST /api/admin/login/iam-sso/start` → `POST /api/admin/login/iam-sso/complete` —— IAM Identity Center（SSO）流：start 回傳 `{sessionId,authorizeUrl}`；complete 消費回呼 URL（校驗 `state`）後落庫。
- `POST /api/admin/login/sso-token` —— 批次匯入原始 bearer / SSO token（每行一個）；回傳 `{added,failed:[{lineIndex,error}]}`。

### API 金鑰管理

你發給呼叫方的對外 key。

- `GET /api/admin/api-keys` · `POST /api/admin/api-keys` —— 列出 / 建立。
- `PUT /api/admin/api-keys/{id}` · `DELETE /api/admin/api-keys/{id}` —— 更新 / 刪除。
- `GET /api/admin/api-keys/usage` —— 全部 key 的用量。
- `GET /api/admin/api-keys/{id}/usage` · `DELETE …/usage` —— 單 key 用量 / 清零。
- `GET /api/admin/api-keys/{id}/usage/records` —— 分頁用量記錄（`?page=&page_size=`）。

### 用量與統計

- `GET /api/admin/credentials/{id}/usage/records` —— 單帳號分頁用量記錄。
- `GET /api/admin/credentials/{id}/usage/today` —— 單帳號當日彙總。
- `GET /api/admin/credentials/{id}/failure-logs` · `…/throttle-logs` —— 近期失敗 / 限流事件。
- `GET /api/admin/credentials/{id}/balance` —— 帳號餘額（5 分鐘快取）。
- `GET /api/admin/usage/daily` —— 每日用量彙總。
- `GET /api/admin/usage/daily/{date}/records` —— 指定日期的記錄。
- `GET /api/admin/rpm` —— 即時 RPM 快照。

### 配置與設定

- `GET /api/admin/config` —— 去敏配置檢視（僅布林 / 非密欄位）。
- `GET /api/admin/models` —— 帶 `display_name` / `type` / `max_tokens` 的模型列表（與 `/v1/models` 同源模型集）。
- `GET /api/admin/config/load-balancing` · `PUT …` —— 執行期讀取 / 切換負載平衡模式（`priority` / `balanced`），落盤 `config.json`。
- `GET /api/admin/config/auth-keys` · `PUT …` —— 執行期讀取（去敏）/ 輪換 `apiKey` 與 `adminApiKey`；即時生效（無需重啟）。
- `GET /api/admin/server-info` —— `{masterApiKey,version,kiroVersion}`；`masterApiKey` 已**去敏**（未設定則 `null`），`version` 為 kiro2api 版本，`kiroVersion` 為偽裝上游 UA 版本。

### 即時日誌

需 `logCapacity > 0`，否則回傳 `503`。

- `GET /api/admin/logs/stream` —— SSE 流（先 history 事件，再逐條 log 事件帶心跳）。EventSource 無法設標頭，用 `?api_key=<admin key>` 驗證。
- `GET /api/admin/logs/snapshot` —— 目前緩衝的 JSON 陣列。
- `GET /api/admin/logs/download` —— 緩衝匯出為 `.txt` 附件。

### 舊管理端點（保留向後相容）

- `GET /admin/api/stats` —— `{accounts:[…], summary:{total,active,disabled,in_cooldown}}`。
- `GET /admin/api/config` —— 去敏配置。
- `POST /admin/api/accounts/{id}/enable` | `disable` —— 手動啟停（記憶體內，重啟後復位為檔案值）。

## 使用者 API（`/user` · `/api/user/*`）

`/user` 使用者面板（靜態，rust-embed 嵌入）由 `/api/user/*` 驅動。這些端點**不走** admin 閘——每次請求用呼叫方**自己的 API-KEY** 驗證（`x-api-key` 標頭，或登入 body 裡的 `{apiKey}`）；handler 校驗後把資料面限定到該 key。key 非法 → `401`，體 `{"error":"…"}`。回應 camelCase；`credits = cost / 0.72`。

### POST /api/user/login

校驗 key 並回傳額度概覽。

**請求：**
```bash
curl -X POST http://localhost:8080/api/user/login \
  -H "Content-Type: application/json" \
  -d '{"apiKey": "sk-your-api-key"}'
```

**回應：**
```json
{
  "id": "key-123",
  "name": "My Key",
  "spendingLimit": 100,
  "limitUnit": "usd",
  "totalCost": 12.5,
  "totalCredits": 17.36,
  "expiresAt": "2026-12-31T00:00:00Z",
  "durationDays": 365,
  "activatedAt": "2026-07-25T00:00:00Z"
}
```

### GET /api/user/usage

取得該 key 的用量彙總（含 `byModel[]`）。

**請求：**
```bash
curl http://localhost:8080/api/user/usage \
  -H "x-api-key: sk-your-api-key"
```

### GET /api/user/usage/records

取得該 key 的用量記錄，分頁（`?page=&page_size=`，降序）。

**請求：**
```bash
curl "http://localhost:8080/api/user/usage/records?page=1&page_size=20" \
  -H "x-api-key: sk-your-api-key"
```

## 系統 API

### GET /health

健康檢查（Docker 探針適配，不需驗證）。

**請求：**
```bash
curl http://localhost:8080/health
```

**回應：**
```json
{"service":"kiro2api","status":"ok","version":"0.1.0"}
```

### GET /v1/ping

探活（不需驗證）。

**請求：**
```bash
curl http://localhost:8080/v1/ping
```

**回應：**
```json
{"pong": true}
```

## 錯誤碼

| 狀態碼 | 說明 |
|--------|------|
| 200 | 成功 |
| 400 | 參數錯誤 / 未對映模型（如 `INVALID_MODEL_ID`） |
| 401 | 未認證（已配置 apiKey 時，金鑰無效或缺失） |
| 403 | 禁止 |
| 429 | 觸發限流（超過 `MAX_RPM_PER_CREDENTIAL`） |
| 502 | 上游 Kiro 失敗 |
| 503 | 服務不可用（無可用帳號：全冷卻 / 停用 / 超 RPM） |

**錯誤回應格式（依協議而異）：**

Anthropic 形狀：
```json
{
  "type": "error",
  "error": {"type": "invalid_request_error", "message": "..."}
}
```

OpenAI / Responses 形狀：
```json
{
  "error": {"message": "...", "type": "invalid_request_error", "code": "..."}
}
```

Gemini 形狀：
```json
{
  "error": {"code": 400, "message": "...", "status": "INVALID_ARGUMENT"}
}
```

## 速率限制

每帳號可設每分鐘請求上限（`MAX_RPM_PER_CREDENTIAL`，`0` = 無限）。超過上限的帳號會被納入冷卻，若當下無其他可用帳號則回傳 `503`：

```json
{
  "type": "error",
  "error": {
    "type": "overloaded_error",
    "message": "No available credentials"
  }
}
```

多帳號輪詢（`priority` 等權 / `balanced` 加權）搭配分級冷卻，會自動繞開被限流的帳號。

## 最佳實踐

1. **list-then-use**：可用模型取決於帳號訂閱檔位，先呼叫 `/models` 列出實際可服務的模型再使用，避免請求到未授權模型收到 `400`。
2. **實現重試邏輯**：對於 5xx 錯誤實現指數退避重試；服務內部已對可自愈的失敗（配額 / 風控 / 限流）做分級冷卻與跨帳號重試，確定性錯誤（如 `INVALID_MODEL_ID`）不會瞎重試。
3. **監控使用統計**：定期檢查 `/api/admin/usage/daily` 與各帳號 `/balance`（5 分鐘快取）了解服務狀態。
4. **多帳號提升可用性**：在池中放入多條 Kiro 憑證，令牌到期會自動內存刷新並原子落盤，端點在 Kiro / CodeWhisperer / AmazonQ 間按序回退。
5. **使用流式輸出**：對於長回應，使用 `stream: true` 改善使用者體驗（四種協議均支援）。

---

> 更多用法與部署見 [USAGE](USAGE.md)、[DEPLOY](DEPLOY.md)，或根 [README](../../README.md)。
