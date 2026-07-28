# 使用指南

本指南涵蓋 kiro2api Web 面板功能、支援的模型、第三方客戶端整合和進階使用方式。

## Web 面板概覽

kiro2api 內建靜態管理面板（`/admin`）與使用者面板（`/user`），由 rust-embed 在編譯期嵌入。訪問 `http://localhost:8080/admin`，並使用 `adminApiKey`（未設定則回退 `apiKey`）登入。

### 儀表板

儀表板顯示系統概覽和實時狀態：

- **運行時間**：服務啟動後的實時計時
- **系統資訊**：版本、Rust、作業系統、記憶體使用、CPU 使用率、PID、運行模式
- **全域剩餘積分**：帳號池餘額彙總（共享快取，雙向同步）
- **贊助二維碼卡片**：實時拉取遠端配置，點擊圖片可放大檢視
- **帳號狀態**：帳號池概覽，顯示健康帳號數與冷卻狀態
- **可用模型**：列出當前可用的所有模型 id
- **檢查更新**：儀表板載入時會靜默自動檢查更新。當 GitHub 上存在更新版本時，「檢查更新」按鈕會高亮為「更新到 vX」；點擊後開啟「更新服務 vX」對話框，在可捲動的方框中顯示**當前介面語言**的發行說明，並附上升級指令 `docker compose pull && docker compose up -d` 與一鍵複製按鈕。對話框僅作提示/展示，**絕不會自動執行升級**

### 帳號管理

在帳號管理頁面對 Kiro（CodeWhisperer）帳號池進行操作：

**新增帳號（三種互動式登入流）：**
1. 點擊「新增帳號」按鈕
2. 選擇登入方式：**Builder ID**（裝置碼）/ **IAM Identity Center（SSO）授權碼** / **社交令牌**
3. 依畫面完成授權，服務自動寫入 `credentials.json`
4. 選擇性設定優先級 / 權重與標籤

**批次匯入：**
1. 點擊「批次匯入」
2. 貼上憑據陣列 JSON、`{accounts}` 物件，或每行一個 bearer / SSO token
3. 系統**逐條**納入帳號池：每加入一個帳號後立即呼叫一次上游 `getUsageLimits` 查詢餘額以**探活驗證**——存活的帳號保留，失效的帳號自動回滾/刪除並過濾掉
4. 匯入時**按 refresh token 去重**：已在帳號池中的帳號會被跳過，避免同一帳號重複匯入（兩份憑證爭搶同一輪替令牌會互相失效、浪費配額、觸發上游風控）
5. 匯入過程有**即時介面**：進度條 + 「正在處理第 i/N 個帳號」、即時累計的成功/重複/失敗統計，以及一份逐條狀態清單，每一列即時更新（待處理 → 檢查中 → 驗證中 → 已驗證並顯示用量 / 重複 / 失敗已排除）
6. 已驗證的帳號**即時落盤**，途中中斷仍會保留已成功的部分；匯入進行中對話框**無法關閉**

**啟停 / 重置帳號：**
1. 在帳號列表中找到目標帳號
2. 點擊「啟用 / 停用」或「重置」清除冷卻與失敗計數

**餘額查詢：**
1. 點擊帳號旁的「查詢餘額」
2. 系統會拉取該帳號的剩餘積分並寫入共享快取

### 即時日誌

結構化日誌檢視器提供：

- **方向過濾**：查看最新或最舊的日誌
- **文字搜尋**：按關鍵字搜尋
- **分頁**：分頁瀏覽記錄
- **SSE 即時推送**：新日誌實時滾入，無需刷新
- **下載**：一鍵匯出快照 `.txt`
- **日誌持久化**：需 `logCapacity > 0`（環形緩衝），設為 `0` 則日誌端點返回 `503`

### 使用統計

查看服務使用情況：

- **累計請求數**：每日與單帳號維度彙總
- **含客戶端 IP 與帳號標籤**：可按日下鑽
- **失敗 / 限流日誌**：分類記錄異常
- **即時 RPM**：每帳號每分鐘請求檢視

### 模型測試

從面板直接向任一可用模型傳送一個測試請求，透過中轉核心直達後端並顯示原始結果，方便驗證某個帳號/模型是否真的可用：

1. 選擇要測試的模型（可選填端點）
2. 點擊傳送
3. 面板用你已建立的某個 API-KEY 呼叫中轉端點並展示原始回應

- **未建立自訂 key 時預設回退主 API 金鑰**（`adminApiKey` / `apiKey`），開箱即可測試
- 該 key 僅儲存在瀏覽器本機（localStorage），且僅用於呼叫中轉端點

### API-KEY 管理

集中管理發放給呼叫方的對外 key：

**新增 Key：**
1. 點擊「新增 Key」
2. 設定消費上限 / 有效期與標籤
3. 點擊「儲存」

**消費上限在四協議一律生效：**
- Anthropic / OpenAI / OpenAI-Responses / **Gemini** 四個協議前端都按鑑權閘解析出的 key 歸屬記帳，額度用盡即返回 `402`
- 因此無論呼叫方走哪條協議，用量與花費都會如實計入該 key，不存在「換個端點就能繞開上限」的口子
- **注意會提前擋下**：真實花費要等回應算完 token 才知道，所以鑑權閘在放行前會先為這筆在途請求預留一份保守的名義額度（USD 單位 `1.0`，credits 單位 `1.0 / 0.72 ≈ 1.39`），請求結束即釋放。判斷式是 `已用 + 在途預留 + 本次預留 > 上限` 就回 `402`，也就是**剩餘額度不足一次預留時就開始拒**，而不是真的花到滿。因此面板上的「已用 / 上限」還沒跑滿時就可能全數 `402`；上限本身若小於一次預留（例如只給 `0.5` USD 的 key），該 key 會從一開始就無法呼叫——設上限時請留出餘裕

**用量與記錄：**
- 檢視或清零單 key 的累計用量
- 瀏覽分頁請求記錄

**切換狀態：**
- 點擊 Key 旁的開關啟用 / 禁用

### 設定

可視化配置管理，修改**即時生效**（無需重啟服務）：

- **負載均衡**：執行期切換 `priority`（等權輪詢）/ `balanced`（按 weight 加權）
- **鑑權密鑰**：輪換 `apiKey` / `adminApiKey`
- **整合示例**：協議 × 語言可複製片段（OpenAI / Anthropic / Responses / Gemini）
- **伺服器 / 系統資訊**：版本、Rust、OS、記憶體、CPU、PID、執行模式在**儀表板**上顯示。面板上看到的去敏金鑰來自 `GET /api/admin/config/auth-keys`（唯一會去敏的那支）；**驅動系統資訊的 `GET /api/admin/server-info` 本身回傳的 `masterApiKey` 是完整明文、不去敏**——別把該回應原樣貼進 issue、日誌或第三方工具
- **一鍵重啟服務**：無需回到終端

### 右上角控制欄

- **運行狀態徽章**：服務健康度一目了然
- **GitHub**：跳轉倉庫
- **服務重啟**：一鍵重啟服務
- **主題切換**：深色 / 淺色模式
- **語言選擇**：繁體中文 / 簡體中文 / English / 日本語 / 한국어

### 使用者面板

把 `http://localhost:8080/user` 發給 key 持有者。他們用**自己的 API-KEY**（無需 admin 權限）登入，檢視該 key 的額度、按模型細分的累計用量與分頁請求記錄。由 `/api/user/*` 驅動，絕不暴露其它 key 或管理操作。

## 圖片輸入（多模態）

kiro2api 支援多模態內容，四種協議前端均可傳入圖片。支援三種 API 格式的圖片傳輸。

> [!IMPORTANT]
> **圖片必須內聯為 Base64**。Kiro 資料面只收內聯 base64，本服務**不會**替你去下載遠端圖片：任何 `http://` / `https://` 圖片 URL（OpenAI 的 `image_url.url`、Anthropic 的 `source.type:"url"` 或帶 http(s) `url` 的圖片來源，以及 `tool_result` 裡內嵌的這兩種圖片區塊）都會被**明確拒絕**並回 `400`（錯誤訊息是服務端原文，簡體，見下方反例）——刻意報錯而不是靜默丟圖，免得模型根本沒收到圖你卻毫不知情。請自行先把圖片抓下來轉成 `data:image/...;base64,...` 再送。（Gemini 原生格式本身只有 `inlineData`（base64），不存在遠端 URL 寫法。）
>
> **唯一例外是 OpenAI Responses（`/v1/responses`）的 `input_image`**：那條路徑上非 `data:` 的 URL（以及缺省的 `image_url`）是**靜默丟棄**——不報錯、不回 `400`，該張圖直接不會送到模型。所以走 Responses 時更要自己確保圖片已經是 Base64 Data URI，別指望用 `400` 幫你把關。

### OpenAI 格式

在 `messages` 陣列中使用 `image_url` 類型，`url` 必須是 Base64 Data URI：

**Base64 圖片示例**：

```bash
curl -X POST http://localhost:8080/openai/v1/chat/completions \
  -H "Content-Type: application/json" \
  -H "Authorization: Bearer sk-你的API密鑰" \
  -d '{
    "model": "claude-sonnet-4.5",
    "messages": [
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
    ]
  }'
```

**遠端 URL 會被拒絕（反例）**：

下面這種寫法**不成立**，會回 `400`：

```bash
curl -X POST http://localhost:8080/openai/v1/chat/completions \
  -H "Content-Type: application/json" \
  -H "Authorization: Bearer sk-你的API密鑰" \
  -d '{
    "model": "claude-sonnet-4.5",
    "messages": [
      {
        "role": "user",
        "content": [
          {"type": "text", "text": "分析這張圖片"},
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

回應（`400`，OpenAI 形狀）：

```json
{
  "error": {
    "message": "不支持远程图片 URL(https://example.com/image.jpg);请把图片内联为 data: URL(base64)后再发送",
    "type": "invalid_request_error",
    "code": null
  }
}
```

正確做法是先把該圖片抓下來轉成 base64，改用上面的 Data URI 寫法。

### Claude 格式

在 `content` 陣列中使用 `image` 類型：

```bash
curl -X POST http://localhost:8080/claude/v1/messages \
  -H "Content-Type: application/json" \
  -H "Authorization: Bearer sk-你的API密鑰" \
  -d '{
    "model": "claude-sonnet-4.5",
    "max_tokens": 1024,
    "messages": [
      {
        "role": "user",
        "content": [
          {"type": "text", "text": "這是什麼"},
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

在 `parts` 陣列中使用 `inlineData`：

```bash
curl -X POST http://localhost:8080/gemini/v1beta/models/claude-sonnet-4.5:generateContent \
  -H "Content-Type: application/json" \
  -H "Authorization: Bearer sk-你的API密鑰" \
  -d '{
    "contents": [
      {
        "parts": [
          {"text": "這是什麼"},
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

圖片會隨對話一併轉換為 Anthropic 中樞母格式後送往 Kiro 後端，四種協議行為一致。

## 支援的模型

### 模型名稱由後端訂閱檔位決定

kiro2api 的後端是 Kiro（CodeWhisperer）帳號池，**可用模型取決於帳號訂閱檔位**。客戶端傳入的模型名按**小寫子字串**匹配到 Kiro 內部模型：

| 模型名稱 | 說明 |
|---------|------|
| `claude-sonnet-4.5` | Claude 系旗艦，免費檔（KIRO FREE）通常即授權此模型 |
| `claude-opus-*` | 更強模型，需更高訂閱檔位授權 |

**訂閱檔位決定授權**：免費檔（KIRO FREE）通常只授權 `claude-sonnet-4.5`，opus 等需更高檔位。請求**不支援的模型**會明確返回 `400`（`INVALID_MODEL_ID`），而非靜默失敗，服務**不瞎重試、不誤傷帳號**。

**協議端點的模型清單是寫死的**：`GET /v1/models`、`/claude/v1/models`、`/v1beta/models` 各自回傳一份**固定的**三條短清單：`/v1/models` 與 `/v1beta/models` 是同樣的 sonnet-4.5 / opus-4.6 / gpt-5.6-sol（後者依 Gemini 慣例帶 `models/` 前綴），只有 `/claude/v1/models` 那份不同——它以 haiku-4.5 取代 gpt-5.6-sol。三份**都不讀帳號池、不依訂閱檔位過濾**——池子空的時候照樣這麼回。因此**不能**把它當成「已授權模型」的白名單，清單裡的模型仍可能回 `400`。要看帳號實際授權的動態並集（快取為空時回落到 17 條靜態目錄），請用管理端的 `GET /api/admin/models`。

## 第三方客戶端整合

> base URL 用**標準裸前綴**：OpenAI = `{host}/v1`，Anthropic = `{host}`（SDK 自動補 `/v1/messages`），Gemini = `{host}/v1beta`。也可用顯式廠商前綴 `/openai/v1`、`/claude/v1`、`/gemini/v1beta`。

### ChatGPT-Next-Web

1. 部署 ChatGPT-Next-Web
2. 在設定中新增自訂 API：
   - **API 位址**：`http://伺服器IP:8080/openai/v1`
   - **API Key**：你的 sk- 金鑰
3. 選擇 `claude-sonnet-4.5` 進行對話

### LobeChat

1. 部署 LobeChat
2. 在設定中新增自訂模型提供者：
   - **提供者名稱**：kiro2api
   - **API 端點**：`http://伺服器IP:8080/openai/v1`
   - **API Key**：你的 sk- 金鑰
3. 選擇 `claude-sonnet-4.5`

### OpenCat

1. 開啟 OpenCat 應用
2. 在設定中新增自訂 API：
   - **API 位址**：`http://伺服器IP:8080/openai/v1`
   - **API Key**：你的 sk- 金鑰
3. 選擇 `claude-sonnet-4.5`

### Python SDK（OpenAI 相容）

```python
from openai import OpenAI

client = OpenAI(
    api_key="sk-your-api-key",
    base_url="http://localhost:8080/v1"
)

response = client.chat.completions.create(
    model="claude-sonnet-4.5",
    messages=[{"role": "user", "content": "你好"}],
    stream=True
)

for chunk in response:
    print(chunk.choices[0].delta.content or "", end="")
```

### Python SDK（Claude 相容）

```python
import anthropic

client = anthropic.Anthropic(
    api_key="sk-your-api-key",
    base_url="http://localhost:8080"
)

message = client.messages.create(
    model="claude-sonnet-4.5",
    max_tokens=1024,
    messages=[{"role": "user", "content": "寫一個快速排序演算法"}]
)

print(message.content[0].text)
```

### Python SDK（Gemini 相容）

```python
from google import genai

client = genai.Client(
    api_key="sk-your-api-key",
    http_options={"base_url": "http://localhost:8080/v1beta"}
)

response = client.models.generate_content(
    model="claude-sonnet-4.5",
    contents="你好"
)
print(response.text)
```

> 驗證閘接受的通道依優先順序為：`Authorization: Bearer` > `x-api-key` > `x-goog-api-key` > 查詢參數（`?api_key=` > `?token=` > `?key=`）。Google 原生的 `x-goog-api-key` 標頭與 `?key=` 參數**同樣受理**，所以上面這段 `genai.Client(api_key=...)` 只要換掉 `base_url` 就能直接跑。要換的是**值**：`api_key` 一律填**本服務**的 API Key，不是 Google 的真金鑰。

### cURL

```bash
# 非流式請求
curl -X POST http://localhost:8080/v1/chat/completions \
  -H "Content-Type: application/json" \
  -H "Authorization: Bearer sk-your-api-key" \
  -d '{
    "model": "claude-sonnet-4.5",
    "messages": [{"role": "user", "content": "你好"}]
  }'

# 流式請求
curl -X POST http://localhost:8080/v1/chat/completions \
  -H "Content-Type: application/json" \
  -H "Authorization: Bearer sk-your-api-key" \
  -d '{
    "model": "claude-sonnet-4.5",
    "messages": [{"role": "user", "content": "你好"}],
    "stream": true
  }'
```

> 流式回應在上游報錯或**傳輸中途中斷**（連線重置 / 讀取逾時 / 分塊未收尾）時，一律以該協議自身的錯誤事件收束——Anthropic `error` 事件、OpenAI 錯誤 chunk 且不補 `[DONE]`、Responses `response.failed`、Gemini 錯誤區塊（絕不報 `STOP`），逾時映射 `504`、其餘 `502`，**絕不會被當成正常完成**，客戶端的重試邏輯因此能正常觸發。**上游自己**判定截斷（它自己的長度預算或上下文耗盡）時，流式與非流式同口徑如實回報截斷原因（`max_tokens` / `length` / `MAX_TOKENS` / `incomplete`）——注意這與你傳的 `max_tokens` / `max_output_tokens` / `maxOutputTokens` 無關：那幾個參數**相容接受但不轉發給上游，不會限制回應長度**（見 [API.md](API.md) 的參數表）。

## 令牌自癒與帳號池

kiro2api 無需手動維護 Cookie。Kiro 令牌到期會**自動內存刷新並原子落盤**，帳號池按類別對失敗差異化處置。

### 令牌自動刷新

- token 到期時**自動內存刷新**（單飛協調，避免並發刷新級聯 `401`）
- 刷新成功原子寫回 `credentials.json`，不中斷在途請求
- 只有**真正的憑據失效**才永久禁用；配額 / 風控 / 限流一律**冷卻自癒**

### 端點回退與跨帳號重試

- Kiro IDE → CodeWhisperer → AmazonQ 多端點按序回退，`429` / 網路錯自動切換
- 帳號級失敗自動**跨帳號重試**
- 確定性請求錯誤（如不支援的模型 `INVALID_MODEL_ID`）**不瞎重試、不誤傷帳號**，直接把上游原因回給客戶端

### 何時需要人工介入

- 帳號在管理面板顯示長期「失效」而非「冷卻」（憑據被上游撤銷）
- 全池餘額耗盡（免費檔積分用完）

處理方式：在帳號管理頁重新走**互動式登入**（Builder ID / IAM SSO / 社交令牌）納入新帳號，或**重置**被誤冷卻的帳號。

## 多語言切換

點擊右上角地球圖示選擇語言：

- 繁體中文
- 簡體中文
- English
- 日本語
- 한국어

所有頁面和訊息都會即時切換。

## 對話上下文

### 由客戶端維護（推薦）

kiro2api **不保留伺服端會話記憶**。大多數客戶端（ChatGPT-Next-Web、LobeChat 等）會自動維護 `messages` 歷史。只需在同一對話中繼續提問，把完整對話歷史隨每次請求帶上，上下文即可保留。

### OpenAI Responses 的注意事項

Responses 協議（`/v1/responses`）**不支援 `previous_response_id`**，傳入會返回 `400`。因為本服務無伺服端會話記憶，請把完整對話歷史隨每次請求帶上：

```python
# 每一輪都帶上完整歷史，服務不做跨請求記憶
response = client.responses.create(
    model="claude-sonnet-4.5",
    input="我叫什麼名字？（請在 input 中附上先前對話）"
)
```

## 進階功能

### 函數調用（工具真透傳）

工具呼叫在四種協議間**真透傳**（Anthropic `tool_use` / OpenAI `tool_calls` / Gemini `functionCall`），不做模擬：

```python
response = client.chat.completions.create(
    model="claude-sonnet-4.5",
    messages=[{"role": "user", "content": "北京今天天氣怎麼樣"}],
    tools=[{
        "type": "function",
        "function": {
            "name": "get_weather",
            "description": "取得指定城市的天氣",
            "parameters": {
                "type": "object",
                "properties": {"city": {"type": "string"}},
                "required": ["city"]
            }
        }
    }]
)
```

### 模型名映射

客戶端傳入的模型名按**小寫子字串**匹配到 Kiro 內部模型：

1. 傳入的模型名做小寫化處理
2. 與 Kiro 內部可服務模型做子字串匹配
3. 未匹配到 → 明確返回 `400`，訊息為「無法識別的模型名: …」（這是**本地**判定，回應體裡**沒有** `INVALID_MODEL_ID`；上游判定「該模型對當前檔位不可用」是另一種 `400`，reason 才是 `INVALID_MODEL_ID`）

因此無論客戶端寫 `claude-sonnet-4.5` 還是帶前後綴的變體，只要能匹配到內部模型即可服務。

### 負載均衡策略

在設定中執行期切換帳號池策略（即時生效）：

- **`priority`**（預設）：等權輪詢，各帳號依序輪流
- **`balanced`**：按 `weight` 加權分配流量

每帳號另有獨立 RPM 限流（`MAX_RPM_PER_CREDENTIAL`）與分級冷卻。

## 常見問題

**Q：如何重啟服務？**
A：點擊右上角控制欄的「重啟」按鈕，或執行 `docker compose restart`。停機時服務會以**最長 8 秒**的排空視窗收束在途請求與管理面的 SSE 長連線，確保用量 / 計費的最終刷盤一定會執行，重啟不丟數據。

**Q：日誌在哪裡？**
A：在 Web 面板的「日誌」頁面即時檢視（SSE），可下載快照 `.txt`。需 `logCapacity > 0` 才啟用日誌捕獲，設為 `0` 則日誌端點返回 `503`。

**Q：如何備份設定？**
A：所有運行態儲存在 `data/` 目錄（`config.json`、`credentials.json`、日誌），定期備份該目錄即可。

**Q：支援圖片輸入嗎？**
A：支援。四種協議前端均可傳入圖片（OpenAI `image_url` / Claude `image` / Gemini `inlineData`），詳見「圖片輸入」章節。

**Q：可以同時使用多個 Kiro 帳號嗎？**
A：可以。在帳號管理中納入多個帳號，服務會依 `priority` / `balanced` 策略自動輪詢，並在失敗時跨帳號重試。

**Q：為什麼請求 opus 模型返回 400？**
A：可用模型取決於帳號訂閱檔位。免費檔（KIRO FREE）通常只授權 `claude-sonnet-4.5`。注意**協議端點的 `GET /v1/models` 幫不上忙**——它是寫死的短清單，不依檔位過濾，`claude-opus-4.6` 本來就在裡面。要看帳號實際授權了哪些模型，請用管理端的 `GET /api/admin/models`（上游 `ListAvailableModels` 的動態並集）。
