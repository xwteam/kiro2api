# API 文檔

本文檔詳細說明 kiro2api 的所有 API 端點、請求格式和回應格式。kiro2api 是多協議 AI 中轉，後端為 Kiro（CodeWhisperer），統一提供 Claude 系模型。

## 認證

所有協議端點都走統一驗證。驗證閘共受理**六條**攜帶通道（見本節末的完整優先順序），以下三種最常用：

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

除上述三種外，驗證閘還受理 Gemini 生態的原生通道 `x-goog-api-key` 標頭與 `?key=` 查詢參數，以及 `?api_key=`（每個請求都會解析，只是它主要為無法設標頭的 SSE `EventSource` 而設）。驗證閘合計受理六條通道，完整優先順序為：`Authorization: Bearer` > `x-api-key` > `x-goog-api-key` > 查詢參數（`?api_key=` > `?token=` > `?key=`）。

> **注意：** API Key 由部署者透過 `API_KEY` 環境變數或 `config.json` 的 `apiKey` 設定。金鑰以常量時間比較。`apiKey` 留空**且尚未建立任何 API-KEY** 時，協議端點才**開放訪問**（啟動會告警）；一旦在管理面發出第一條 API-KEY，協議閘即收口，不帶有效金鑰的請求一律 `401`。對外部署務必設置 `apiKey`。`/health`、`/v1/ping` 探活端點不需驗證。

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

回傳一份**固定的**常用模型 id 短清單：`claude-sonnet-4.5`、`claude-opus-4.6`、`gpt-5.6-sol`。

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
      "created": 1700000000,
      "owned_by": "kiro2api"
    },
    {
      "id": "claude-opus-4.6",
      "object": "model",
      "created": 1700000000,
      "owned_by": "kiro2api"
    },
    {
      "id": "gpt-5.6-sol",
      "object": "model",
      "created": 1700000000,
      "owned_by": "kiro2api"
    }
  ]
}
```

> [!IMPORTANT]
> 這份清單是**寫死的常數**，端點完全不讀帳號池，因此**不會依你帳號的訂閱檔位過濾**——池子裡一個帳號都沒有時，它照樣回傳這三個 id。各協議的固定清單也並非完全一致：`/v1beta/models` 列的是同樣這三個 id、只是帶 Gemini 慣用的 `models/` 前綴（`models/claude-sonnet-4.5` / `models/claude-opus-4.6` / `models/gpt-5.6-sol`），而 `/claude/v1/models` 是唯一不同的一份——它以 `claude-haiku-4.5` 取代 `gpt-5.6-sol`。但模型名解析本身是協議無關的（四個協議共用同一張對映表）。
>
> 中轉的模型名對映表實際認得 17 個內部 id（`claude-sonnet-4.5/4.6/5`、`claude-opus-4.5/4.6/4.7/4.8`、`claude-haiku-4.5`、`claude-fable-5`、`deepseek-3.2`、`glm-5`、`qwen3-coder-next`、`minimax-m2.1/m2.5`、`gpt-5.6-terra/luna/sol`），另加特殊值 `auto`。要看完整目錄或帳號實際授權的動態並集請用 `GET /api/admin/models`。

> 💡 **模型選擇建議**：**可用模型取決於帳號訂閱檔位**。
> - 免費檔（KIRO FREE）通常只授權 `claude-sonnet-4.5`，適合絕大多數對話與 agent 場景，推薦作為預設選擇。
> - opus / GPT 等模型需更高檔位授權。
> - 請求不支援的模型會明確返回 `400`（`INVALID_MODEL_ID`），而非靜默失敗，也**不會瞎重試或誤傷帳號**。
>
> 傳入的模型名以**小寫子字串**比對到 Kiro 內部模型（未匹配到 → `400`）。注意 `/models` 不做檔位過濾，**它列出的模型照樣可能回 `400`（`INVALID_MODEL_ID`）**，別把它當成「本帳號已授權」的白名單；請以實際請求的結果為準並處理好 `400`。本服務的串流介面為真正的增量串流，首字一生成即開始推送。

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
| `temperature` | number | ❌ | **無效**：本服務的請求體結構根本沒有這個欄位，傳了會被當成未知鍵直接丟棄（不報錯、也沒有任何預設值） |
| `max_tokens` | number | ❌ | **無效**：會被解析，但 Kiro 資料面的 wire 沒有對應欄位，轉換時刻意不轉發，因此**不會**限制回應長度 |
| `tools` | array | ❌ | 函數定義陣列（巢狀格式 `{"type":"function","function":{...}}`） |
| `tool_choice` | string | ❌ | **無效**：會被解析並帶到中樞格式，但同樣不轉發給上游，無法強制/禁止工具呼叫 |

> [!IMPORTANT]
> `temperature` / `max_tokens` / `tool_choice` 只是為了讓官方 SDK 能原樣送出而被**相容接受**，三者都到不了 Kiro 後端（Kiro 資料面 wire 沒有取樣參數、長度上限或工具選擇欄位），**傳了不會報錯，但也不會生效**。回應裡出現 `finish_reason:"length"` 只代表**上游自己**判定截斷，與你傳的 `max_tokens` 無關。

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
| `tool_choice` | string 或 object | ❌ | **無效**：`auto`、`none`、`required`、`{"type":"function","name":"..."}` 都能解析通過，但與 Chat Completions 同理**不會轉發給上游**，無法真的強制或禁止某個工具 |
| `max_output_tokens` | number | ❌ | **無效**：同 Chat Completions 的 `max_tokens`，解析後不轉發，不會限制回應長度 |

**`input` 陣列條目類型**（**只認**這三種：`function_call`、`function_call_output`、`message`；`type` 缺省但帶 `role` 時視同 `message`。其餘任何 `type` 值都會被拒，訊息為 `不支持的输入条目类型: <type>`——而且 `input` 是 untagged 聯合，**一條壞條目就否掉整個請求體**，回 `400`）：
- `{"type":"message","role":"user"|"assistant"|"system","content":[...]}` —— 內容區塊也只認三種：`{"type":"input_text","text":...}`、`{"type":"input_image","image_url":"data:image/...;base64,..."}`、`{"type":"output_text","text":...}`
- `{"type":"function_call","call_id","name","arguments"}` —— 歷史裡助手呼叫工具的那一輪（多輪續聊需要客戶端自己重發完整歷史）
- `{"type":"function_call_output","call_id","output"}` —— 客戶端回傳的工具執行結果。本服務**沒有** `tool_result` 這個條目類型（`tool_result` 是 Anthropic Messages 的內容區塊名，不是 Responses 的輸入條目）：寫成 `{"type":"tool_result",…}` 不會被當成同義詞，而是整條請求 `400`

> [!IMPORTANT]
> `input_image` 的 `image_url` 只認內聯的 `data:<mime>;base64,<...>`，而且**這個協議的處理方式與其它三家不同**：非 `data:` 的遠端 URL（以及 `image_url` 缺省）在這裡是**靜默丟棄**——不報錯、也不回 `400`，那張圖直接不進入送往上游的內容，模型根本收不到。同樣的遠端 URL 在 OpenAI `/chat/completions` 與 Anthropic `/v1/messages` 上則是明確回 `400`。要傳圖務必先轉成 Base64 Data URI。

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
        {"type": "output_text", "text": "1+1等於2"}
      ]
    }
  ],
  "usage": {
    "input_tokens": 10,
    "output_tokens": 5,
    "total_tokens": 15
  }
}
```

> [!NOTE]
> 上面就是**完整**的頂層欄位集合，別按官方 Responses API 的形狀去讀多餘的鍵：回應**沒有** `previous_response_id`、`instructions`、`error` 這幾個欄位（不是 `null`，是整個鍵不出現），`usage` 只有 `input_tokens` / `output_tokens` / `total_tokens`（**沒有** `input_tokens_details` / `output_tokens_details`），`output_text` 區塊也**沒有** `annotations`。唯一的可選欄位是 `incomplete_details`：只有 `status` 為 `incomplete`（命中截斷）時才出現，形如 `{"reason":"max_output_tokens"}`。要判斷失敗請看 HTTP 狀態碼與錯誤體，不要讀 `error` 欄位。

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

回傳一份**固定的** Anthropic 形狀模型清單：`claude-sonnet-4.5`、`claude-opus-4.6`、`claude-haiku-4.5`（注意與 OpenAI／Gemini 形狀那兩份固定清單並不相同，同樣不依帳號檔位過濾）。裸 `/v1/models` 回傳 OpenAI 格式，故 Claude 形狀請走 `/claude/v1/models`（避開與 OpenAI 衝突）。

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
      "type": "model",
      "id": "claude-sonnet-4.5",
      "display_name": "Claude Sonnet 4.5",
      "created_at": "2026-01-01T00:00:00Z"
    }
  ],
  "has_more": false,
  "first_id": "claude-sonnet-4.5",
  "last_id": "claude-haiku-4.5"
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

> [!IMPORTANT]
> `max_tokens` 為**相容接受但不生效**：Anthropic 規範要求帶上它，本服務也照收，但 Kiro 資料面的 wire 沒有長度上限欄位，轉換時刻意不轉發，因此**不會**限制回應長度。回應裡的 `stop_reason:"max_tokens"` 只代表上游自己判定截斷（`ContentLengthExceededException`），與你傳的值無關。

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

回傳一份**固定的** Gemini 形狀模型清單：`models/claude-sonnet-4.5`、`models/claude-opus-4.6`、`models/gpt-5.6-sol`；同樣不依帳號檔位過濾。

**請求：**
```bash
curl http://localhost:8080/v1beta/models \
  -H "x-goog-api-key: sk-your-api-key"
```

**回應：**
```json
{
  "models": [
    {
      "name": "models/claude-sonnet-4.5",
      "supportedGenerationMethods": ["generateContent", "streamGenerateContent"]
    }
  ]
}
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

> [!IMPORTANT]
> `generationConfig` 為**相容接受但不生效**：本服務只解析其中的 `maxOutputTokens` 一個鍵（`temperature` 等其餘鍵連解析都沒有，直接丟棄），而 `maxOutputTokens` 解析後也**不會轉發**給 Kiro 後端（資料面 wire 沒有對應欄位），因此**不能**用它限制回應長度。`finishReason:"MAX_TOKENS"` 只代表上游自己判定截斷，與你傳的值無關。

> [!NOTE]
> `toolConfig.functionCallingConfig.mode` 是唯一**部分**生效的工具控制項，且只有 `NONE` 兌現得了：`NONE` 時本服務靠**完全不下發工具規格**來執行「本輪禁止呼叫函數」。`AUTO` 就是預設行為；`ANY`（強制至少呼叫一次工具）在 Kiro 資料面 wire 上無從表達，**傳了等同 `AUTO`**，不會強制模型呼叫工具。`mode` 的值大小寫不拘，但外層 `toolConfig` 之下的內層鍵必須是 camelCase 的 `functionCallingConfig`（寫成 `function_calling_config` 讀不到，等同沒傳）。另外 `tools[]` 裡若只有內建工具條目（`googleSearch` / `codeExecution` / `urlContext`——本中轉無從兌現），會被如實歸為「沒有函數工具」。

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

> [!NOTE]
> 憑證可走驗證閘接受的任一通道，優先順序為：`Authorization: Bearer` > `x-api-key` > `x-goog-api-key` > 查詢參數（`?api_key=` > `?token=` > `?key=`）。Gemini 原生的 `x-goog-api-key` 標頭與 `?key=` 參數**同樣受理**，因此官方 `google-genai` SDK 只要換掉 `base_url` 就能直接用。要換的是**值**：一律填**本服務**的 API Key，不是 Google / OpenAI 廠商的真金鑰。

## 管理 API（`/admin` · `/api/admin/*`）

`/admin` 管理面板（靜態，rust-embed 嵌入）由 `/api/admin/*` 介面驅動。下列端點均需 `adminApiKey`（未設則回退 `apiKey`；兩者皆空時管理 API 開放——切勿如此對外暴露）。驗證攜帶方式同協議閘——同一支中介軟體，故六條通道全部受理，優先順序為：`Authorization: Bearer` > `x-api-key` > `x-goog-api-key` > 查詢參數（`?api_key=` > `?token=` > `?key=`）；無法設標頭的 SSE 日誌流慣用 `?api_key=`。回應體為 camelCase，**但 `GET /api/admin/config` 與 `GET /api/admin/models` 例外——這兩支是 snake_case，以對齊面板的資料模型**（舊端點 `GET /admin/api/stats` 的 summary 亦然）；所有回應**絕不含帳號的 access/refresh token**（`GET /api/admin/credentials` 只出狀態）。

> [!WARNING]
> 管理介面的回應**並非無密**：`GET`/`POST /api/admin/api-keys` 的 `key` 欄位是**完整明文**，`GET /api/admin/server-info` 的 `masterApiKey` 也是**完整明文**；只有 `GET /api/admin/config/auth-keys` 與 `GET /api/admin/config` 有去敏。本服務沒有「唯讀管理員」角色——拿到管理金鑰即可讀取、建立、輪換全部 key。請把管理介面的回應當金鑰對待：別貼進 issue、日誌或第三方工具。

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
  "currentId": -1,
  "credentials": [
    {
      "id": 12345,
      "priority": 1,
      "weight": 1,
      "disabled": false,
      "failureCount": 0,
      "isCurrent": false,
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

> **注意：** 帳號池是**每次請求**現選帳號，沒有「當前帳號」這種持久狀態，因此 `currentId` 恆為 `-1`、每一列的 `isCurrent` 恆為 `false`——兩個欄位都是為將來的黏著選號模式預留的，**請勿據此分支**。`priority` 即池內 `weight`（同一個值的兩種呈現）。`healthStatus` 取值為 `disabled` | `unhealthy` | `warning` | `healthy`。

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

設定帳號優先級——優先級**就是**池內 `weight`（`balanced` 模式下越大分到的流量越多），小於 1 會被鉗到 1。

請求體只有 `{"priority": <整數>}`，`priority` **必填**（缺了回 `422`）；本端點**沒有**獨立的 `weight` 欄位，多餘的鍵會被靜默忽略。要顯式設定權重請改用 `PUT /api/admin/credentials/{id}` 傳 `{"weight": N}`。

**請求：**
```bash
curl -X POST http://localhost:8080/api/admin/credentials/12345/priority \
  -H "Content-Type: application/json" \
  -H "Authorization: Bearer sk-your-admin-key" \
  -d '{"priority": 2}'
```

### POST /api/admin/credentials/{id}/reset

清失敗計數 / 冷卻。

### POST /api/admin/credentials/batch-import

批次匯入憑證。請求體必須是 `{"data": …}` 這層外殼（直接 POST 裸陣列會被拒為 `422`）；`data` 本身可以是陣列、KAM `{accounts}` 物件或單物件。逐條規整 / 校驗 / 落盤，回傳逐項結果與計數。

**請求：**
```bash
curl -X POST http://localhost:8080/api/admin/credentials/batch-import \
  -H "Content-Type: application/json" \
  -H "Authorization: Bearer sk-your-admin-key" \
  -d '{"data": [{"refreshToken": "RT-1", "email": "a@x.io"}]}'
```

逐條實際被讀取的欄位只有：`refreshToken`（**必填**，也可放在巢狀的 `credentials` 裡）、`clientId`、`clientSecret`、`region` / `authRegion` / `apiRegion`、`email`、`nickname`、`machineId`、`priority`。其餘鍵（含 `authMethod`、`accessToken`、`expiresAt`）都會被靜默忽略——登入方式由有無 `clientId` + `clientSecret` 推斷，access token 與到期時間由首次自動刷新補齊。

**回應：**
```json
{
  "success": true,
  "message": "imported 1 of 1 credential(s), 0 duplicate, 0 failed",
  "total": 1,
  "added": 1,
  "duplicate": 0,
  "failed": 0,
  "results": [{"index": 1, "status": "added", "credentialId": 2, "email": "a@x.io"}]
}
```

逐項的 `index` 從 **1** 起算；`status` 為 `added` | `duplicate` | `failed`；`credentialId` / `email` / `error` 在沒有值時直接不出現。**逐項結果沒有 `success` 欄位**（`success` 只在頂層）。

### 互動式登入 / 匯入

無需手改 `credentials.json` 即可納入新 Kiro 帳號。

- `POST /api/admin/login/builderid/start` → `POST /api/admin/login/builderid/poll` —— AWS Builder ID 裝置碼流；poll 回傳 `{success,completed,status,interval?,credentialId?,email?}`，成功即落庫。
- `POST /api/admin/login/iam-sso/start` → `POST /api/admin/login/iam-sso/complete` —— IAM Identity Center（SSO）流：start 回傳 `{sessionId,authorizeUrl}`；complete 消費回呼 URL（校驗 `state`）後落庫。
- `POST /api/admin/login/sso-token` —— 批次匯入原始 bearer / SSO token（每行一個）；回傳 `{added,failed:[{lineIndex,error}]}`。

### API 金鑰管理

你發給呼叫方的對外 key。

- `GET /api/admin/api-keys` · `POST /api/admin/api-keys` —— 列出 / 建立；回應的 `key` 欄位為**完整明文**（前端自行去敏顯示，「複製」按鈕需要完整值）。
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

- `GET /api/admin/config` —— 去敏配置檢視（僅布林 / 非密欄位）。**回應為 snake_case**：`{"host","port","region","load_balancing_mode","max_rpm_per_credential","kiro_version","system_version","node_version","credentials_path","api_key_set","admin_api_key_set"}`。
- `GET /api/admin/models` —— 帶 `display_name` / `type` / `max_tokens` 的模型列表，**同樣是 snake_case**。內容是各帳號上游 `ListAvailableModels` 的**動態並集**（快取命中即用；並集為空時回落到 17 條的靜態目錄，並在背景惰性回填）。**與協議端點的 `/v1/models` 不同源**——後者是寫死的三條短清單。
- `GET /api/admin/config/load-balancing` · `PUT …` —— 執行期讀取 / 切換負載平衡模式（`priority` / `balanced`），落盤 `config.json`。
- `GET /api/admin/config/auth-keys` · `PUT …` —— 執行期讀取（去敏）/ 輪換 `apiKey` 與 `adminApiKey`；即時生效（無需重啟）。
- `GET /api/admin/server-info` —— `{masterApiKey,version,kiroVersion,rustVersion,…}` 外加執行期指標（`serverTime`、`serverTimeUnix`、`os`、`memoryUsedBytes`、`memoryTotalBytes`、`cpuPercent`、`runMode`、`pid`、`uptimeSecs`）；`masterApiKey` 為所設定 `apiKey` 的**完整明文**（未設定則 `null`），此處**不去敏**——前端在瀏覽器自行去敏顯示、「複製」按鈕取完整值；要去敏形式請用 `GET /api/admin/config/auth-keys`。`version` 為 kiro2api 版本，`kiroVersion` 為偽裝上游 UA 版本。

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

`/user` 使用者面板（靜態，rust-embed 嵌入）由 `/api/user/*` 驅動。這些端點**不走** admin 閘——每次請求用呼叫方**自己的 API-KEY** 驗證：金鑰只從標頭提取，優先順序為 `Authorization: Bearer` > `x-api-key` > `x-goog-api-key`，**不受理查詢參數**（與協議閘 / admin 閘的差別就在這裡）；`POST /api/user/login` 額外優先採用 body 裡的 `{apiKey}`，缺了才回退標頭。handler 校驗後把資料面限定到該 key。key 非法 → `401`，體 `{"error":"…"}`。回應 camelCase；`credits = cost / 0.72`。

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
  "id": 7,
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
{"service":"kiro2api","status":"ok","version":"0.3.0"}
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
| 400 | 參數錯誤；模型名在本地未對映到內部模型（訊息為「無法識別的模型名: …」，**不帶** `INVALID_MODEL_ID`）；或上游判定該模型對當前帳號檔位不可用（上游 reason 為 `INVALID_MODEL_ID`，回給客戶端的訊息以 `Invalid model '<m>': not available for the current account.` 開頭） |
| 401 | 未認證（驗證閘已收口時：金鑰缺失、無效、已停用或已過期。協議閘的收口條件是設了 `apiKey` **或**已建立任何一條 API-KEY） |
| 402 | API-KEY 消費已達上限，體為 `{"type":"error","error":{"type":"billing_error","message":"api key spending limit exceeded"}}`。判定含在途預留（USD 單位 `1.0`／credits 單位約 `1.39`），故**剩餘額度不足一次預留時就開始拒**，並非真的花到滿 |
| 403 | 禁止 |
| 404 | 找不到（路徑不存在；或管理端點傳入了池中不存在的帳號 / key id） |
| 422 | 請求體反序列化失敗（欄位缺失或型別不符）。四個協議的對話端點已自行接管拒收、改回各自形狀的 `400`；`422` 主要出現在 `/api/admin/*`、`/api/user/login` 與 `/v1/messages/count_tokens` 這類直接用 `Json` 提取器的端點 |
| 429 | **上游**回報限流（事件流裡的 `ThrottlingException` 一類）。本服務自己的 `MAX_RPM_PER_CREDENTIAL` **不會**回 `429`——超限的帳號只是暫時不參與選號，全部帳號都選不出來時回的是 `503` |
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

> **例外：** 只有進到協議 handler 的錯誤才隨協議變形。驗證閘擋下的 `401` 與 `402` 掛在四個協議合併之後的同一層中介軟體上，因此**一律**回 Anthropic 形狀的 `{"type":"error","error":{"type":"authentication_error"|"billing_error","message":"…"}}`——OpenAI / Gemini 客戶端在這兩個狀態碼上讀不到 `error.code` / `error.status`。`/api/user/*` 的 `401` 又是另一種形狀：`{"error":"…"}`。

## 速率限制

每帳號可設每分鐘請求上限（`MAX_RPM_PER_CREDENTIAL`，`0` = 無限）。超過上限的帳號會被納入冷卻，若當下無其他可用帳號則回傳 `503`：

```json
{
  "type": "error",
  "error": {
    "type": "overloaded_error",
    "message": "no available upstream account"
  }
}
```

（訊息字面量就是小寫的 `no available upstream account`。OpenAI / Responses 形狀為 `{"error":{"message":"no available upstream account","type":"overloaded_error","code":null}}`；Gemini 形狀為 `{"error":{"code":503,"message":"no available upstream account","status":"UNAVAILABLE"}}`。）

多帳號輪詢（`priority` 等權 / `balanced` 加權）搭配分級冷卻，會自動繞開被限流的帳號。

## 最佳實踐

1. **別把 `/models` 當白名單**：協議端點的 `/models` 是寫死的短清單，不依帳號訂閱檔位過濾，列出的模型照樣可能回 `400`（`INVALID_MODEL_ID`）。可用模型取決於帳號訂閱檔位，請務必處理 `400`；要看帳號實際授權的動態並集請用 `GET /api/admin/models`。
2. **實現重試邏輯**：對於 5xx 錯誤實現指數退避重試；服務內部已對可自愈的失敗（配額 / 風控 / 限流）做分級冷卻與跨帳號重試，確定性錯誤（如 `INVALID_MODEL_ID`）不會瞎重試。
3. **監控使用統計**：定期檢查 `/api/admin/usage/daily` 與各帳號 `/balance`（5 分鐘快取）了解服務狀態。
4. **多帳號提升可用性**：在池中放入多條 Kiro 憑證，令牌到期會自動內存刷新並原子落盤，端點在 Kiro / CodeWhisperer / AmazonQ 間按序回退。
5. **使用流式輸出**：對於長回應，使用 `stream: true` 改善使用者體驗（四種協議均支援）。

---

> 更多用法與部署見 [USAGE](USAGE.md)、[DEPLOY](DEPLOY.md)，或根 [README](../../README.md)。
