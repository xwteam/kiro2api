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

回傳本服務**完整的可服務模型目錄**（17 個 id）。三個協議的 `/models` 共用同一份目錄（`src/models_catalog.rs`），只是各自套上該協議的外形。

**請求：**
```bash
curl http://localhost:8080/v1/models \
  -H "Authorization: Bearer sk-your-api-key"
```

**回應**（節選前 3 條；實際 17 條，完整順序見下）：
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
      "id": "claude-sonnet-4.6",
      "object": "model",
      "created": 1700000000,
      "owned_by": "kiro2api"
    },
    {
      "id": "claude-sonnet-5",
      "object": "model",
      "created": 1700000000,
      "owned_by": "kiro2api"
    }
  ]
}
```

`object` 恆為 `"list"`、每條的 `object` 恆為 `"model"`；`created` 是**寫死的常數** `1700000000`（不讀時鐘），`owned_by` 是**寫死的常數** `"kiro2api"`。

<a id="model-catalog"></a>**完整目錄（17 條，順序即回應順序，三個協議一致）：**

`claude-sonnet-4.5`、`claude-sonnet-4.6`、`claude-sonnet-5`、`claude-opus-4.5`、`claude-opus-4.6`、`claude-opus-4.7`、`claude-opus-4.8`、`claude-haiku-4.5`、`claude-fable-5`、`deepseek-3.2`、`glm-5`、`qwen3-coder-next`、`minimax-m2.1`、`minimax-m2.5`、`gpt-5.6-terra`、`gpt-5.6-luna`、`gpt-5.6-sol`

> [!IMPORTANT]
> **「先列模型、再照列出的 id 呼叫」現在真的成立**：目錄裡的每個 id 都是模型名對映表認得的內部 id（有測試把這條契約釘住），因此列出來的 id 不會在本地被判為「無法識別的模型名」。三個協議列的是**同一批 id**，換協議不會看到不一樣的清單。
>
> 但這份目錄仍是**寫死的常數**、端點完全不讀帳號池，因此**不依你帳號的訂閱檔位過濾**——池子裡一個帳號都沒有時，它照樣回傳這 17 個 id。上游若判定某模型對當前帳號檔位不可用，仍會回 `400`（上游 reason `INVALID_MODEL_ID`）。要看帳號**實際授權**的動態並集請用 `GET /api/admin/models`（快取命中時回上游並集，未命中才回落到這同一份目錄）。
>
> 除目錄裡的 17 個 id 外，對映表另認特殊值 `auto`（路由別名，不在 `/models` 清單裡）。

> 💡 **模型選擇建議**：**可用模型取決於帳號訂閱檔位**。
> - 免費檔（KIRO FREE）通常只授權 `claude-sonnet-4.5`，適合絕大多數對話與 agent 場景，推薦作為預設選擇。
> - opus / GPT 等模型需更高檔位授權。
> - 請求不支援的模型會明確返回 `400`（`INVALID_MODEL_ID`），而非靜默失敗，也**不會瞎重試或誤傷帳號**。
>
> 傳入的模型名以**小寫子字串**比對到 Kiro 內部模型（未匹配到 → `400`）。注意 `/models` 不做檔位過濾，**它列出的模型照樣可能回 `400`（`INVALID_MODEL_ID`）**——但這回只會來自**上游檔位判定**，不會再是本地對映失敗；別把它當成「本帳號已授權」的白名單，請以實際請求的結果為準並處理好 `400`。本服務的串流介面為真正的增量串流，首字一生成即開始推送。

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

**`input` 陣列條目類型**（能映射到中樞的**只有**這三種：`function_call`、`function_call_output`、`message`；`type` 缺省但帶 `role` 時視同 `message`。其餘 `type` 值——`reasoning`、`local_shell_call` 等 Responses 側產物——**整條跳過，不影響其餘條目**，不再否掉整個請求：客戶端做多輪時會把上一輪的**整個** `output` 原樣回灌，裡面必然帶這類條目，判成錯誤的後果是第一輪能通、第二輪必炸，v0.7.1 已修正）：
> ⚠️ **工具陣列裡的內建工具會被丟棄。** 照 OpenAI 規範，`tools` 裡除了 `type:"function"`，還可以有 `web_search`、`local_shell`、`file_search` 等**內建工具**——它們由 OpenAI 服務端自己執行，**照規範就沒有 `name` 欄位**。本服務的中樞沒有等價物，無法代為執行，故**解析後丟棄並記一條 WARN**（`responses_builtin_tool_dropped`，帶 `tool_type`），不會因此拒絕整個請求（v0.7.1 之前會回 `400 tools[N]: missing field name`，一個內建工具就廢掉整輪對話）。**後果**：模型不具備該內建能力（如聯網搜尋）。帶 `name` 的 `function` / `custom` 工具照常生效；`parameters` 可省略，缺省按空物件 schema 處理。

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

以 Anthropic 形狀回傳**同一份 17 條目錄**（id 與順序和 `/v1/models`、`/v1beta/models` 逐字一致，見上方[完整目錄](#model-catalog)），同樣不依帳號檔位過濾。裸 `/v1/models` 回傳 OpenAI 格式，故 Claude 形狀請走 `/claude/v1/models`（避開與 OpenAI 衝突）——差別只在**外形**，內容不再有差異。

**請求：**
```bash
curl http://localhost:8080/claude/v1/models \
  -H "Authorization: Bearer sk-your-api-key"
```

**回應**（節選前 2 條；實際 17 條）：
```json
{
  "data": [
    {
      "type": "model",
      "id": "claude-sonnet-4.5",
      "display_name": "Claude Sonnet 4.5",
      "created_at": "2026-01-01T00:00:00Z"
    },
    {
      "type": "model",
      "id": "claude-sonnet-4.6",
      "display_name": "Claude Sonnet 4.6",
      "created_at": "2026-01-01T00:00:00Z"
    }
  ],
  "has_more": false,
  "first_id": "claude-sonnet-4.5",
  "last_id": "gpt-5.6-sol"
}
```

`display_name` 取自目錄的展示名（如 `Claude Opus 4.8`、`GPT-5.6 Sol`）；`created_at` 是**寫死的常數** `"2026-01-01T00:00:00Z"`；`has_more` 恆為 `false`（不分頁）；`first_id` / `last_id` 就是目錄的首尾 id，即 `claude-sonnet-4.5` 與 `gpt-5.6-sol`（目錄為空時這兩個鍵直接不出現）。

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

以 Gemini 形狀回傳**同一份 17 條目錄**（id 與順序和 `/v1/models`、`/claude/v1/models` 逐字一致，見上方[完整目錄](#model-catalog)），只是每個 id 套上 Gemini 慣用的 `models/` 前綴；同樣不依帳號檔位過濾。

**請求：**
```bash
curl http://localhost:8080/v1beta/models \
  -H "x-goog-api-key: sk-your-api-key"
```

**回應**（節選前 2 條；實際 17 條）：
```json
{
  "models": [
    {
      "name": "models/claude-sonnet-4.5",
      "supportedGenerationMethods": ["generateContent", "streamGenerateContent"]
    },
    {
      "name": "models/claude-sonnet-4.6",
      "supportedGenerationMethods": ["generateContent", "streamGenerateContent"]
    }
  ]
}
```

每條只有 `name` 與 `supportedGenerationMethods` 兩個鍵：`supportedGenerationMethods` 是**寫死的常數**陣列 `["generateContent","streamGenerateContent"]`（逐條相同，不反映該模型的真實能力）；`displayName` 一律不填，因此該鍵**不會出現**在回應裡（要展示名請走 `/claude/v1/models` 或 `GET /api/admin/models`）。呼叫時**要去掉 `models/` 前綴**:路由是 `/v1beta/{model_action}` 形式,路徑參數只捕獲**一個**路徑段,把 `name` 原樣填回會變成 `/v1beta/models/models/claude-sonnet-4.5:generateContent`(兩段)而直接 404。正確寫法是 `/v1beta/models/claude-sonnet-4.5:generateContent`。

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
      "statusReason": "none",
      "throttleCount": 0
    }
  ]
}
```

> **`failureCount` 是累計失敗數,`throttleCount` 是限流事件條數。** 兩者曾經錯位,導致被封禁的帳號顯示成「限流 1、失敗 0」。

`statusReason` 記錄**最近一次失敗的原因**——`none` / `banned`(被上游停用)/ `quota` / `token_expired` / `throttled` / `refresh_denied`——答「為什麼不能用」。

> **`banned` 會真正把帳號擋在池外**,其餘原因只影響展示。冷卻是計時器,到點自動回池;封禁是上游給的結論(原話為「帳號已鎖定,請聯絡客服驗證身分」),不隨時間解除。若只按計時器放行,冷卻一過帳號就重新入選、再失敗、再冷卻,循環燒真實請求,而 `available` 還把它算作可用——面板一邊掛著「封禁」、計數一邊說沒事,兩個數字互相矛盾。因此封禁帳號不被選中、不計入 `available`、`healthStatus` 報 `unhealthy`。它不會自癒(永遠等不到那次成功來清標籤),**唯一出口是面板的「重置」**(`POST /api/admin/credentials/{id}/reset`,會一併清掉該結論)。其餘原因仍在帳號下次成功時自動清空。該結論隨 `credentials.json` 落盤(`statusReason` 鍵)、重啟後還原——只活在記憶體裡的話,每次發版都會把它抹掉、帳號悄悄回池。**strike 計數與冷卻截止時刻仍不落盤**:那兩個是計時器,重啟從零開始無非早重試一次;結論不同,它決定帳號能不能進池。

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

### POST /api/admin/credentials/{id}/models/refresh

對**單一帳號**實拉一次上游 `ListAvailableModels` 並回填模型快取（`GET /api/admin/models` 的動態並集就是從這份快取取的）。

**無請求體、無查詢參數**——整條路徑就是全部輸入。會先確保該帳號的 access token 新鮮（必要時刷新並寫回活池），再打上游。

**請求：**
```bash
curl -X POST http://localhost:8080/api/admin/credentials/12345/models/refresh \
  -H "Authorization: Bearer sk-your-admin-key"
```

**回應（200）：**
```json
{"success": true, "id": "12345", "count": 18}
```

`id` 是路徑上原樣回顯的**字串**（不是數字），`count` 為本次從上游拉到並寫進快取的模型條數。

**狀態碼：**

| 狀態碼 | 情形 | 回應體 |
|--------|------|--------|
| 200 | 拉取成功 | `{"success":true,"id":"…","count":N}` |
| 404 | 池中沒有這個 id | `{"error":"account not found","id":"…"}` |
| 502 | 上游拉取失敗 | `{"success":false,"id":"…","error":"models upstream HTTP 403: …"}` |

> **注意：** 本端點**不跳過已停用的帳號**——`disabled: true` 的帳號一樣會被實拉（只有下面的批次版本才會跳過停用帳號）。`502` 的 `error` 直接透出上游的狀態碼與短說明（如 `HTTP 403: Your User ID ... suspended`），便於面板顯示真因；同一條真因另會以 WARN 進日誌緩衝。回應**絕不含**任何 token。

### POST /api/admin/credentials/models/refresh

**按訂閱檔位**批次刷新模型快取。不是「刷全池」：不同檔位服務不同模型（KIRO FREE 約 9 個、KIRO PRO+ 約 18 個），但同一檔位的帳號結果相同，故每個**已快取到檔位**的帳號群只挑**一個代表**去實拉，並集自然涵蓋全檔位。

檔位還沒快取的帳號（例如冷啟動時餘額快取尚空）走**有界發現**：每刷成功一個就看並集有沒有變大，連續 3 次不再增長、或累計成功達 12 個、或未知帳號試完即停——避免冷啟動把上千帳號串行打一遍。已停用的帳號全程跳過。

**無請求體、無查詢參數。**

**請求：**
```bash
curl -X POST http://localhost:8080/api/admin/credentials/models/refresh \
  -H "Authorization: Bearer sk-your-admin-key"
```

**回應（200）：**
```json
{
  "success": true,
  "refreshed": 2,
  "failed": 1,
  "errors": [{"id": 7, "error": "models upstream HTTP 403: account suspended"}],
  "tiers": ["KIRO FREE", "KIRO PRO+"]
}
```

- `success` **恆為 `true`**：它只表示「這次批次呼叫本身跑完了」，不代表每個帳號都成功。要判斷有沒有出事請看 `failed` 與 `errors[]`。
- `refreshed` / `failed` —— 成功、失敗的帳號數。
- `errors[]` —— 逐帳號失敗明細，`id` 是**數字**（解析不出數字時為 `0`），`error` 為上游狀態碼 + 短說明。全部成功時為空陣列。
- `tiers[]` —— 本次實際刷成功所涵蓋的檔位名（如 `KIRO FREE`）；發現階段刷成功但仍讀不到檔位的，記一個字面量 `"unknown"` 佔位。
- 池空、或全部帳號都停用時 `refreshed` 為 `0`，仍回 `200`。

本端點只有 `200` 一種狀態碼（單帳號失敗不會冒泡成 `502`）。

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
- `GET /api/admin/credentials/{id}/failure-logs` —— 近期 401/403 失敗事件（欄位與下方 `…/throttle-logs` 逐字相同，差異見該小節末）。
- `GET /api/admin/credentials/{id}/balance` —— 帳號餘額（5 分鐘快取）。
- `GET /api/admin/usage/daily` —— 每日用量彙總（跨全部帳號，按日期降序的陣列）。
- `GET /api/admin/rpm` —— 即時 RPM 快照。

以下端點各有獨立小節：`…/usage/today`、`…/throttle-logs`、`/api/admin/usage/daily/{date}/records`、`/api/admin/usage/summary`、`/api/admin/credits/global`。

<a id="pagination"></a>**分頁約定**（凡標註 `?page=&page_size=` 的端點共用這一套）：

- 查詢參數是 **snake_case 的 `page_size`**，不是 `pageSize`（**回應體**才是 camelCase 的 `pageSize`）。兩個參數都可省略。
- `page` 從 **1** 起算，預設 `1`；`page_size` 在 `/api/admin/*` 預設 **20**，在 `/api/user/usage/records` 預設 **50**。
- `page_size` 被鉗到至少 `1`；`page` 超出總頁數時鉗到**最後一頁**（因此 `?page=99` 回的是最後一頁而不是空集）；資料為空時回 `page:1`、`totalPages:0`。
- 回應一律是 `{records,total,page,pageSize,totalPages}`，`records` 按時間**降序**（最新在前）。
- 未知 id / 空存儲一律回**空頁 `200`**，不會 `404`、也不會 `500`（只有帳號 CRUD 那些端點才對未知 id 回 `404`）。

### GET /api/admin/credentials/{id}/usage/today

單帳號**當日**（CST，即 UTC+8）用量彙總。

**請求：**
```bash
curl http://localhost:8080/api/admin/credentials/12345/usage/today \
  -H "Authorization: Bearer sk-your-admin-key"
```

**回應（200）：**
```json
{
  "date": "2026-07-26",
  "credentialId": 12345,
  "totalRequests": 42,
  "totalInputTokens": 12800,
  "totalOutputTokens": 30500,
  "totalCost": 1.234,
  "totalCredits": 1.714
}
```

`date` 是伺服器當下時刻換算成 CST 的日期字串（`YYYY-MM-DD`），日界線按 **UTC+8** 切，與伺服器本身的時區設定無關。`credentialId` 是路徑 id 轉成的數字（解析不出數字時為 `0`）。`totalCredits` 累加各筆記錄的 `creditsUsed`（沒有該值的記錄計 `0`）。

還有一個 `totalCreditsSaved` 欄位在程式碼裡恆為 `None`，因此**永遠不會出現**在回應裡——別去讀它。

未知 id → **全零彙總 `200`**（`credentialId` 照樣回顯你傳的數字），不是 `404`。

### GET /api/admin/credentials/{id}/throttle-logs

單帳號近期**限流（429）事件**分頁列表，降序。查詢參數見上方[分頁約定](#pagination)。

**請求：**
```bash
curl "http://localhost:8080/api/admin/credentials/12345/throttle-logs?page=1&page_size=10" \
  -H "Authorization: Bearer sk-your-admin-key"
```

**回應（200）：**
```json
{
  "records": [
    {
      "credentialId": 12345,
      "requestType": "api",
      "statusCode": 429,
      "responseBody": "too-many-requests",
      "createdAt": "2026-07-26T10:30:00Z"
    }
  ],
  "total": 1,
  "page": 1,
  "pageSize": 10,
  "totalPages": 1
}
```

`statusCode` 在本端點是**寫死的常數 `429`**（限流事件按定義就是 429，不反映上游回的其它碼）。`responseBody` 為上游回應體，按**字元**截斷到 200 字。`createdAt` 為 RFC3339 UTC、秒精度、`Z` 結尾。

`GET /api/admin/credentials/{id}/failure-logs` 的請求與回應**欄位逐字相同**，差別在資料來源與兩個取值：那邊記的是 401/403 鑑權失敗，`statusCode` 是上游真實回的碼（不是常數），`responseBody` 截到 **2000** 字而非 200。

> **注意：** 事件日誌按帳號有 LRU 上限，極高頻失敗時最舊的事件會被淘汰，因此 `total` 是**下界**而非歷史全量。

### GET /api/admin/usage/daily/{date}/records

指定 CST 日期的用量記錄分頁（跨全部帳號），降序。`{date}` 為 `YYYY-MM-DD`，按 **UTC+8** 切日界線。查詢參數見上方[分頁約定](#pagination)。

**請求：**
```bash
curl "http://localhost:8080/api/admin/usage/daily/2026-07-26/records?page=1&page_size=20" \
  -H "Authorization: Bearer sk-your-admin-key"
```

**回應（200）：**
```json
{
  "records": [
    {
      "model": "claude-sonnet-4.5",
      "inputTokens": 100,
      "outputTokens": 200,
      "estimatedCost": 0.05,
      "creditsUsed": 0.069,
      "cacheReadInputTokens": 10,
      "cacheCreationInputTokens": 20,
      "createdAt": "2026-07-26T10:30:00Z",
      "credentialId": 12345,
      "credentialLabel": "a@x.io",
      "clientIp": "203.0.113.7"
    }
  ],
  "total": 1,
  "page": 1,
  "pageSize": 20,
  "totalPages": 1
}
```

`credentialLabel` 由帳號池快照解析：暱稱 → 郵箱 → `#{id}`；池裡已無該帳號時該鍵**不出現**。`creditsUsed` / `cacheReadInputTokens` / `cacheCreationInputTokens` / `clientIp` 在無值時同樣**整個鍵不出現**（不是 `null`）。另有一個 `creditsSaved` 欄位恆為 `None`，**永遠不會出現**。

> **注意：** 該日記錄先裁到**最新 2000 條**再分頁，因此 `total` 最多 `2000`——高流量日的更早記錄查不到（`GET /api/admin/usage/daily` 的每日彙總不受此限，那邊才是完整口徑）。查無記錄的日期（含格式不合的日期字串）→ 空頁 `200`。

### GET /api/admin/usage/summary

跨全部帳號的**時間窗口**用量彙總 + 圖表分桶 + 執行健康指標。

**查詢參數**（二選一，都省略則預設 24 小時）：

| 參數 | 類型 | 說明 |
|------|------|------|
| `range` | string | 列舉 `6h` \| `24h` \| `3d` \| `7d` \| `30d`。**優先於 `hours`**；非法值 → `400` |
| `hours` | 正整數 | 任意小時數（僅在 `range` 缺省時採用）。`0` → `400` |

**請求：**
```bash
curl "http://localhost:8080/api/admin/usage/summary?range=24h" \
  -H "Authorization: Bearer sk-your-admin-key"
```

**回應（200）：**
```json
{
  "range": "24h",
  "windowSecs": 86400,
  "sinceUnix": 1785000000,
  "untilUnix": 1785086400,
  "bucketSecs": 3600,
  "totalRequests": 128,
  "totalInputTokens": 40960,
  "totalOutputTokens": 81920,
  "totalCost": 3.14159,
  "totalCredits": 4.36332,
  "dailyFallbackApplied": false,
  "series": [
    {
      "bucketStartUnix": 1785000000,
      "totalRequests": 12,
      "totalCost": 0.31,
      "totalCredits": 0.43
    }
  ],
  "successfulRequests": 128,
  "failedRequests": 3,
  "errorRate": 0.02290076335877863,
  "avgLatencyMs": 842.5,
  "rotationSuccessRate": 0.9770992366412214
}
```

- `range` 回顯規整後的窗口標籤：用 `range=` 時原樣回顯，用 `hours=N` 時為 `"<N>h"`，都缺省時為 `"24h"`。
- `windowSecs` / `sinceUnix` / `untilUnix` —— 窗口長度與**閉區間**端點（`untilUnix` = 當下）。
- `bucketSecs` —— 圖表分桶寬度：窗口 ≤ 24 小時為 `3600`（每小時），更長為 `86400`（每天）。`series` 按桶起始**升序**，空窗口 → 空陣列。
- 數值全為 f64/i64 **原始精度、未預先四捨五入**，格式化交給前端。
- `dailyFallbackApplied` —— 窗口 > 1 天時，原始記錄可能已被逐帳號上限淘汰，服務會逐個「完整落在窗口內的 CST 日」用每日彙總取 `max` 補齊 requests/cost/credits。此值為 `true` 表示真的補過。**tokens 沒有每日彙總可補**，故補齊發生時 `totalInputTokens` / `totalOutputTokens` 可能偏低。
- `successfulRequests` = 窗口內用量記錄條數；`failedRequests` = 窗口內失敗日誌（401/403）+ 限流日誌（429）條數。
- `errorRate` = `failedRequests / (successfulRequests + failedRequests)`，分母為 `0` 時為 `0.0`。
- `rotationSuccessRate` = `1 - errorRate`，分母為 `0` 時為 `1.0`。這是**近似值**：跨帳號重試鏈路本身沒有單獨埋點，服務以「最終有沒有落一筆成功用量記錄」當成功訊號來逼近。
- `avgLatencyMs` —— 窗口內**帶延遲樣本**的成功記錄均值（沒有 latency 的舊記錄不計入）；無樣本 → `0.0`。

> **注意：** 事件日誌有 LRU 上限，`failedRequests` 是下界，因此 `errorRate` 偏保守（只會低估、不會虛高）。空存儲 / 空窗口 → 全零 + 空 `series` 的 `200`，不會 `500`。

**錯誤（400）：**
```json
{
  "error": "invalid range",
  "allowed": ["6h", "24h", "3d", "7d", "30d"],
  "hint": "use ?range=<enum> or ?hours=<positive int>"
}
```

`hours=0` 的 `400` 則是另一種體：`{"error":"hours must be a positive integer"}`。

### GET /api/admin/credits/global

全池剩餘積分合計。**純讀共享餘額快取、零上游請求**——快取 miss 或已過期（TTL 5 分鐘）的帳號直接跳過，不會為了湊數去打上游。

**請求：**
```bash
curl http://localhost:8080/api/admin/credits/global \
  -H "Authorization: Bearer sk-your-admin-key"
```

**回應（200）：**
```json
{
  "globalCredits": 1234.5,
  "cachedCount": 8,
  "totalCount": 10,
  "oldestCacheUnix": 1785086100
}
```

- `globalCredits` —— 命中新鮮快取的各帳號 `remaining` 之和。
- `cachedCount` / `totalCount` —— 命中新鮮快取的帳號數 / 池內帳號總數。**兩者不等時代表這個合計是不完整的**（`totalCount - cachedCount` 個帳號沒被算進去），請照此在 UI 上提示，別當成全池真值。
- `oldestCacheUnix` —— 參與求和的快取條目裡最舊的抓取時刻（Unix 秒），供「更新於 X 前」展示；**一個都沒命中時為 `null`**（此時 `globalCredits` 為 `0`、`cachedCount` 為 `0`）。

快取的回填靠帳號頁自動查詢或儀表盤手動刷新（`GET /api/admin/credentials/{id}/balance`），本端點自己不觸發。只有 `200` 一種狀態碼。

### 配置與設定

- `GET /api/admin/config` —— 去敏配置檢視（僅布林 / 非密欄位）。**回應為 snake_case**：`{"host","port","region","load_balancing_mode","max_rpm_per_credential","kiro_version","system_version","node_version","credentials_path","api_key_set","admin_api_key_set"}`。
- `GET /api/admin/models` —— 帶 `display_name` / `type` / `max_tokens` 的模型列表，**同樣是 snake_case**。內容是各帳號上游 `ListAvailableModels` 的**動態並集**（快取命中即用；並集為空時回落到 17 條的靜態目錄，並在背景惰性回填，回填是單飛 + 有界 + 60 秒冷卻的）。**與協議端點的 `/models` 同源但取捨不同**：協議側一律直接用那份 17 條靜態目錄（模型發現要穩定可預期，不隨快取冷熱變動、也不為一次 `/models` 去打上游），本端點則**優先**回上游並集——那才反映各帳號檔位的真實可用性，靜態目錄只是它的回落項。
- `GET /api/admin/config/load-balancing` · `PUT …` —— 執行期讀取 / 切換負載平衡模式（`priority` / `balanced`），落盤 `config.json`。
- `GET /api/admin/config/auth-keys` · `PUT …` —— 執行期讀取（去敏）/ 輪換 `apiKey` 與 `adminApiKey`；即時生效（無需重啟）。
- `GET /api/admin/server-info` —— `{masterApiKey,version,kiroVersion,rustVersion,…}` 外加執行期指標（`serverTime`、`serverTimeUnix`、`os`、`memoryUsedBytes`、`memoryTotalBytes`、`cpuPercent`、`runMode`、`pid`、`uptimeSecs`）；`masterApiKey` 為所設定 `apiKey` 的**完整明文**（未設定則 `null`），此處**不去敏**——前端在瀏覽器自行去敏顯示、「複製」按鈕取完整值；要去敏形式請用 `GET /api/admin/config/auth-keys`。`version` 為 kiro2api 版本，`kiroVersion` 為偽裝上游 UA 版本。

### 即時日誌

需 `logCapacity > 0`，否則回傳 `503`。

- `GET /api/admin/logs/stream` —— SSE 流（先 history 事件，再逐條 log 事件帶心跳）。EventSource 無法設標頭，用 `?api_key=<admin key>` 驗證。
- `GET /api/admin/logs/snapshot` —— 目前緩衝的 JSON 陣列。
- `GET /api/admin/logs/download` —— 緩衝匯出為 `.txt` 附件。

### 版本檢查 / 更新 / 重啟

三支面板運維端點。**`/update` 不會自己動手更新，`/restart` 則真的會結束行程**——看清楚再打。

#### GET /api/admin/check-update

查 GitHub Releases 的最新版並與當前建置版本比對。**無查詢參數。**

**請求：**
```bash
curl http://localhost:8080/api/admin/check-update \
  -H "Authorization: Bearer sk-your-admin-key"
```

**回應（200）：**
```json
{
  "current": "0.4.0",
  "latest": "0.4.1",
  "hasUpdate": true,
  "updateUrl": "https://github.com/xwteam/kiro2api/releases/tag/v0.4.1",
  "releaseNotes": "..."
}
```

- `current` —— 本次建置的 crate 版本（編譯期寫入）。
- `latest` —— `releases/latest` 的 `tag_name` **去掉前導 `v`** 後的值。
- `hasUpdate` —— 就是 `latest != current` 的字串比對，**不做語意化版本大小比較**（回退到舊版部署時它同樣會是 `true`）。
- `updateUrl` —— Release 的 `html_url`；缺該欄位時回落到 `https://github.com/xwteam/kiro2api/releases`。
- `releaseNotes` —— Release 的 `body`；缺省為空字串。

> **注意：** 出站查詢失敗一律**保守處理、不報錯**：網路不通、倉庫沒有任何 Release、私有倉回 404、應答無法解析——這些情形統統回 `200` 且 `latest = current`、`hasUpdate = false`、`updateUrl` 為倉庫 Release 頁、`releaseNotes` 為空字串。所以 `hasUpdate:false` 的含義是「沒查到更新」，不等於「已確認是最新版」。本端點只有 `200` 一種狀態碼。

#### POST /api/admin/update

**只回傳一段要你自己去伺服器上執行的命令，服務不會自動更新、也不會碰任何檔案。**

**無請求體、無查詢參數。** 回應三個欄位全是寫死的常數，與部署現場無關（也不會偵測你到底是不是用 Docker Compose 跑的）。

**請求：**
```bash
curl -X POST http://localhost:8080/api/admin/update \
  -H "Authorization: Bearer sk-your-admin-key"
```

**回應（200）：**
```json
{
  "status": "ok",
  "message": "请在服务器上执行以下命令完成更新:",
  "command": "docker compose pull && docker compose up -d"
}
```

`status` 恆為 `"ok"`；`message` 是寫死的**簡體中文**字面量（介面不隨語系變化，別拿翻譯後的文案去比對）；`command` 恆為 `docker compose pull && docker compose up -d`。只有 `200` 一種狀態碼。

#### POST /api/admin/restart

**真的會結束行程。** 需帶 `?confirm=true` 二次確認，防單擊誤觸中斷可用性。

**查詢參數：**

| 參數 | 類型 | 預設 | 說明 |
|------|------|------|------|
| `confirm` | boolean | `false` | 必須顯式 `true`，否則回 `400` 且**不重啟** |

**請求：**
```bash
curl -X POST "http://localhost:8080/api/admin/restart?confirm=true" \
  -H "Authorization: Bearer sk-your-admin-key"
```

**回應（200）：**
```json
{"status": "ok", "message": "Server restarting..."}
```

`status` 與 `message` 都是寫死的常數。回應**先送出**，隨後由背景任務延時 0.5 秒、把去抖存儲（統計、API-KEY、餘額快取、失敗/限流事件日誌四項）全部刷盤，再 `exit(0)`。

**缺 `confirm` 的回應（400，且行程不受影響）：**
```json
{
  "error": {
    "message": "重启需二次确认,请带查询参数 ?confirm=true",
    "type": "confirmation_required"
  }
}
```

> [!WARNING]
> 退出後**誰把它拉起來是部署方的事**。容器以 `restart: unless-stopped` 執行時等於重啟；裸機無守護行程時，這一下等於**停機**——請先確認有 systemd / supervisor 保活再打。
>
> 刷盤那一步不是可有可無的收尾：`exit(0)` 不跑解構子，不刷就會丟掉最近一個去抖週期（約 5 秒）內的寫入。最要命的是 API-KEY——管理員在面板上刪掉一把外洩的 key、順手點「重啟」，沒有刷盤的話行程會帶著舊的 `api_keys.json` 重新拉起，剛吊銷的 key 復活、照樣驗證通過。

### 舊管理端點（保留向後相容）

- `GET /admin/api/stats` —— `{accounts:[…], summary:{total,active,disabled,in_cooldown}}`。
- `GET /admin/api/config` —— 去敏配置。**與 `GET /api/admin/config` 是同一個 handler**，回應逐字相同（snake_case），不是另一份實作。

#### POST /admin/api/accounts/{id}/disable（與 `…/enable`）

手動啟停帳號。這一組是 [`POST /api/admin/credentials/{id}/disabled`](#post-apiadmincredentialsiddisabled) 的**舊別名**：底層改的是帳號池裡同一個 `disabled` 旗標，效果完全一致——差別只在**外形**，所以下面只列外形差異，不重複語意。

- **無請求體**：停 / 啟由路徑末段決定（`disable` 固定寫入 `true`，`enable` 固定寫入 `false`），不像新端點那樣讀 `{"disabled": bool}`。傳了 body 也會被忽略。
- **回應形狀不同**：這裡是 `{"ok":true,"id":"12345","disabled":true}`（`id` 為路徑上原樣回顯的字串），新端點回的是 `{"success":true,"message":"credential disabled"}`。

**請求：**
```bash
curl -X POST http://localhost:8080/admin/api/accounts/12345/disable \
  -H "Authorization: Bearer sk-your-admin-key"
```

**回應（200）：**
```json
{"ok": true, "id": "12345", "disabled": true}
```

未知 id → `404`，體為 `{"error":"account not found","id":"12345"}`（與新端點同一個 404 體）。

> **注意：** 兩支端點（含新版的 `…/disabled`）**自己都只改記憶體、不觸發落盤**。但 `disabled` 這個旗標本身**是會被序列化進 `credentials.json` 的**，而任何一次整池快照落盤——憑證的新增 / 更新 / 刪除、設優先級、以及中轉途中的令牌自動刷新寫回——都會**順帶**把當下記憶體裡的停用狀態一併寫進檔案。所以「重啟後會不會復位」取決於這中間有沒有發生過那樣一次落盤，兩種結果都可能出現，別把任一種當成穩定契約。另外 `PUT /api/admin/credentials/{id}` 的請求體**沒有** `disabled` 欄位，無法用它顯式設定停用。

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

取得**該 key 自己**的用量記錄，分頁、降序。查詢參數同上方[分頁約定](#pagination)，唯一差別是 **`page_size` 預設 `50`**（不是 admin 那邊的 `20`）。

**請求：**
```bash
curl "http://localhost:8080/api/user/usage/records?page=1&page_size=20" \
  -H "x-api-key: sk-your-api-key"
```

**回應（200）：**
```json
{
  "records": [
    {
      "model": "claude-sonnet-4.5",
      "inputTokens": 100,
      "outputTokens": 200,
      "estimatedCost": 0.05,
      "creditsUsed": 0.069,
      "cacheReadInputTokens": 10,
      "cacheCreationInputTokens": 20,
      "createdAt": "2026-07-26T10:30:00Z",
      "clientIp": "203.0.113.7"
    }
  ],
  "total": 5,
  "page": 1,
  "pageSize": 20,
  "totalPages": 3
}
```

記錄只包含歸屬於這把 key 的流量（別的 key 的記錄不會混進來）。`creditsUsed` / `cacheReadInputTokens` / `cacheCreationInputTokens` / `clientIp` 無值時**整個鍵不出現**。另有 `credentialLabel` 與 `creditsSaved` 兩個欄位在使用者面**恆不填**，因此**永遠不會出現**——別去讀它們（帳號標籤屬於管理面資訊，只有 `/api/admin/*` 的記錄端點才給）。

key 校驗失敗 → `401`，體為 `{"error":"…"}`；key 有效但沒有記錄 → 空頁 `200`。

## 系統 API

### GET /health

健康檢查（Docker 探針適配，不需驗證）。

**請求：**
```bash
curl http://localhost:8080/health
```

**回應：**
```json
{"service":"kiro2api","status":"ok","version":"0.7.9"}
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
| 422 | 請求體反序列化失敗（欄位缺失或型別不符）。四個協議的對話端點與 `/v1/messages/count_tokens` 已自行接管拒收、改回各自形狀的 `400`；`422` 主要出現在 `/api/admin/*` 與 `/api/user/login` 這類直接用 `Json` 提取器的端點 |
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

1. **「先列再用」可以，但別把 `/models` 當白名單**：協議端點的 `/models` 現在回的是完整的 17 條目錄、三個協議一致，且每個 id 本地都認得，照著列出的 id 呼叫不會撞上「無法識別的模型名」。但它是寫死的目錄、**不依帳號訂閱檔位過濾**，上游仍可能判定該模型對當前帳號不可用而回 `400`（`INVALID_MODEL_ID`），請務必處理 `400`；要看帳號實際授權的動態並集請用 `GET /api/admin/models`。
2. **實現重試邏輯**：對於 5xx 錯誤實現指數退避重試；服務內部已對可自愈的失敗（配額 / 風控 / 限流）做分級冷卻與跨帳號重試，確定性錯誤（如 `INVALID_MODEL_ID`）不會瞎重試。
3. **監控使用統計**：定期檢查 `/api/admin/usage/daily` 與各帳號 `/balance`（5 分鐘快取）了解服務狀態。
4. **多帳號提升可用性**：在池中放入多條 Kiro 憑證，令牌到期會自動內存刷新並原子落盤，端點在 Kiro / CodeWhisperer / AmazonQ 間按序回退。
5. **使用流式輸出**：對於長回應，使用 `stream: true` 改善使用者體驗（四種協議均支援）。

---

> 更多用法與部署見 [USAGE](USAGE.md)、[DEPLOY](DEPLOY.md)，或根 [README](../../README.md)。
