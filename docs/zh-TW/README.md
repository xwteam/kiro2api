<div align="center">

<img src="../logo.png" width="128" height="128" alt="kiro2api">

<h1>kiro2api</h1>
<h3>多協議 AI 中轉 · Kiro 後端</h3>
<p>一套程式碼同時相容 OpenAI / Anthropic / OpenAI-Responses / Gemini 四大 AI SDK，由 Kiro（CodeWhisperer）後端統一提供 Claude 系模型，純非同步 Rust 架構，Docker 快速部署。</p>

<p>
  <img src="https://img.shields.io/badge/Rust-2024-orange?style=flat-square&logo=rust&logoColor=white" alt="Rust">
  <img src="https://img.shields.io/badge/axum-0.8-000000?style=flat-square&logo=rust&logoColor=white" alt="axum">
  <img src="https://img.shields.io/badge/tokio-async-4E9A06?style=flat-square&logo=rust&logoColor=white" alt="tokio">
  <img src="https://img.shields.io/badge/Docker-20.10+-2496ED?style=flat-square&logo=docker&logoColor=white" alt="Docker">
  <img src="https://img.shields.io/badge/arch-amd64%20%7C%20arm64-4285F4?style=flat-square&logo=linux&logoColor=white" alt="Arch">
  <img src="https://img.shields.io/badge/License-MIT-green?style=flat-square" alt="License">
  <img src="https://img.shields.io/badge/version-v0.1.0-success?style=flat-square" alt="Version">
</p>

<p>
  <a href="#-最近更新">最近更新</a> &bull;
  <a href="#-核心功能">核心功能</a> &bull;
  <a href="#-系統需求">系統需求</a> &bull;
  <a href="#-快速部署">快速部署</a> &bull;
  <a href="#-接入範例">接入範例</a> &bull;
  <a href="#-api-端點">API 端點</a> &bull;
  <a href="#-設定說明">設定說明</a> &bull;
  <a href="#-注意事項">注意事項</a> &bull;
  <a href="#-開發路線">開發路線</a>
</p>

<p>
  📖 文件語言：<a href="../zh-CN/README.md">簡體中文</a> | 繁體中文 | <a href="../en/README.md">English</a> | <a href="../ja/README.md">日本語</a> | <a href="../ko/README.md">한국어</a>
</p>

<br>

<a href="https://github.com/xwteam/kiro2api/issues"><img src="https://img.shields.io/github/issues/xwteam/kiro2api?style=flat-square" alt="Issues"></a>
<a href="https://github.com/xwteam/kiro2api/stargazers"><img src="https://img.shields.io/github/stars/xwteam/kiro2api?style=flat-square" alt="Stars"></a>

</div>

---

> [!NOTE]
> 本專案僅供研究和學習用途，請合理使用，不要用於任何商業目的。

> [!WARNING]
> 本專案與 Amazon / AWS / Kiro 無關聯。透過封裝 Kiro（CodeWhisperer）後端提供多協議相容 API，可能不符合相關服務條款。使用風險自負，作者不對任何帳號處罰或資料遺失承擔責任。

> [!IMPORTANT]
> `apiKey`/`API_KEY` 為空時，協議端點會**開放存取**（啟動會告警）。對外部署務必設定。容器映像已內建 `HOST=0.0.0.0`；裸機部署請勿輕易把 `HOST` 改成 `0.0.0.0`（目前 `/admin`、`/user` 面板本體尚未接驗證，受保護的是 `/api/admin/*`、`/api/user/*` 介面）。

> [!TIP]
> 後端為 Kiro（CodeWhisperer）帳號池。**可用模型取決於帳號訂閱檔位**：免費檔（KIRO FREE）通常只授權 `claude-sonnet-4.5`，opus/GPT 等需更高檔位——請求不支援的模型會明確傳回 `400`（`INVALID_MODEL_ID`），而非靜默失敗。

---

## 📝 最近更新

> 完整更新日誌請查看 [CHANGELOG.md](../../CHANGELOG.md)。

| 日期 | 更新內容 |
|------|----------|
| 2026-07-25 | v0.1.0 - 🚀 首個版本：四協議前端（Anthropic 中樞 + OpenAI / OpenAI-Responses / Gemini）、Kiro 帳號池（多帳號輪詢 / 分級冷卻 / 令牌自癒）、端點回退與跨帳號重試、統一驗證閘、`/admin` 管理面板與 `/user` 使用者面板、每日/帳號用量統計、失敗/限流日誌、帳號餘額快取、即時日誌（SSE）、三種互動式登入流、Docker 多架構（amd64/arm64）交付與 CI |

---

## 🌟 核心功能

> 📖 詳細使用文件：[USAGE.md](USAGE.md)

### 🔌 四協議前端，一套後端

- 一個服務同時提供 **OpenAI Chat**、**Anthropic Messages**、**OpenAI Responses**、**Gemini 原生** 四種 SDK 格式
- 內部以 **Anthropic Messages 為中樞母格式**，其餘協議雙向轉換後複用同一條中轉核心
- 每個協議都支援**串流（SSE）**、**函式呼叫（工具）真透傳**、**圖片輸入（多模態）**
- **雙前綴掛載**：每協議同時掛標準裸前綴與顯式廠商前綴（`/openai/v1`、`/claude/v1`、`/gemini/v1beta`），主流 SDK 填 `base_url` 即插即用

### 🔐 統一驗證閘

- 三選一：`Authorization: Bearer` / `x-api-key` / `?token=`，常數時間比較，失敗即 `401`
- `adminApiKey`（缺省回退 `apiKey`）保護 `/api/admin/*`；持有者用自己的 **API-KEY** 存取 `/api/user/*`
- `/health`、`/v1/ping` 等探活端點不驗證

### 🔄 帳號池與令牌自癒

- **多帳號輪詢**：`priority`（等權輪詢，預設）與 `balanced`（按 `weight` 加權）兩種策略，可在管理面板執行期切換
- 每帳號獨立 RPM 限流、分級冷卻；連續失敗按類別（永久失效 / 歧義驗證 / 配額 / 瞬時）差異化處置
- token 到期**自動記憶體刷新**（單飛協調，避免並發刷新級聯 401），刷新成功原子落盤 `credentials.json`
- 支援 Builder ID 裝置碼 / IAM SSO 授權碼 / 社交令牌三種登入流，憑證可 drop-in 現有 Kiro 資料

### 🔀 端點回退與跨帳號重試

- Kiro IDE → CodeWhisperer → AmazonQ 多端點按序回退，`429`/網路錯自動切換
- 帳號級失敗自動跨帳號重試；確定性請求錯誤（如不支援的模型 `INVALID_MODEL_ID`）**不瞎重試、不誤傷帳號**，直接把上游原因回給客戶端
- body-aware 失敗分類：只有真正的憑證失效才永久停用，配額/風控/限流一律冷卻自癒

### 🖥 Web 管理面板

- 內建靜態管理台（`/admin`），憑 `adminApiKey` 登入，`/api/admin/*` 豐富介面驅動
- **儀表板**：執行時間即時計時、全域剩餘積分、系統資訊（版本/Rust/OS/記憶體/CPU/PID/執行模式）、贊助二維碼卡（即時拉取遠端配置）、**檢查更新**（GitHub Release 比對）
- **帳號管理**：增刪改查、三種互動式登入、批次匯入、優先級/權重、餘額查詢
- **API-KEY 管理**：發放/停用/改標籤、按 key 用量與分頁記錄
- **用量統計**：每日/帳號維度、含客戶端 IP 與帳號標籤、按日下鑽
- **即時日誌**：結構化表格 + 方向過濾 + 搜尋 + 分頁 + SSE 即時推送 + 下載
- **設定**：執行期切負載平衡/驗證金鑰、集成範例（協議×語言可複製片段）、**一鍵重啟服務**
- 頂部控制欄：執行狀態徽章、GitHub、重啟、深淺色主題、5 語言切換

### 👤 使用者面板

- 內建使用者台（`/user`），持有者用自己的 **API-KEY** 登入（無需 admin 權限）
- 檢視該 key 的額度、累計用量與分頁記錄，由 `/api/user/*` 驅動

### 🧭 模型名映射

- 客戶端傳入的模型名按**小寫子字串**匹配到 Kiro 內部模型（未匹配到 → `400`）
- `/models` 端點傳回本服務實際可服務的模型 id，建議客戶端 list-then-use

### ⚡ 高效能架構

- 基於 **Rust + axum 0.8 + tokio**，全鏈路非同步非阻塞
- AWS eventstream 幀解碼、帳號池串行佔鎖最小臨界區、網路發出即釋放
- 強型別 serde 校驗，每種協議獨立適配器模組
- 多階段 Docker 建置、非 root 執行（gosu）、多架構映像、健康檢查

---

## 📋 系統需求

| 依賴 | 版本 | 說明 |
|------|------|------|
| Rust | 2024 edition | 僅從原始碼建置時需要；Docker 部署無需本地安裝 |
| Docker | 20.10+ | 推薦使用 Docker 部署 |
| Kiro 帳號 | — | 需有效的 Kiro（CodeWhisperer）憑證（Builder ID / IdC / 社交登入） |
| 架構 | amd64 / arm64 | 官方映像多架構，二選一自動匹配 |

> [!TIP]
> 使用 Docker 部署無需本地安裝 Rust 環境，只需 Docker 和有效的 Kiro 憑證即可。

---

## ⚡ 快速部署

> 📖 詳細部署文件：[DEPLOY.md](DEPLOY.md)

> **前置條件**：你需要一份有效的 Kiro（CodeWhisperer）帳號憑證。

### 1. 取得 Kiro 憑證

從你的 Kiro 客戶端 / 已有 Kiro 憑證中匯出以下欄位，或使用管理面板的三種互動式登入（Builder ID 裝置碼 / IAM SSO 授權碼 / 社交令牌）現場取得：

| 欄位 | 說明 |
|------|------|
| `accessToken` / `refreshToken` | 存取令牌與刷新令牌（到期自動刷新） |
| `expiresAt` | 令牌過期時間（RFC3339） |
| `authMethod` | `social`（帶 `profileArn`）或 `idc`（帶 `clientId`/`clientSecret`） |

### 2. Docker 部署

```bash
# 複製倉庫
git clone https://github.com/xwteam/kiro2api.git
cd kiro2api

# 建立環境變數檔案
cp .env.example .env
```

編輯 `.env`，至少填一個對外呼叫金鑰 `API_KEY`：

```env
API_KEY=sk-你的對外呼叫金鑰
ADMIN_API_KEY=可選,管理端獨立金鑰（留空回退用 API_KEY）
```

把 Kiro 帳號憑證放到 `data/credentials.json`（陣列，可直接 drop-in 現有 Kiro 憑證）：

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

啟動服務：

```bash
mkdir -p data
docker compose up -d
```

查看日誌確認啟動成功：

```bash
docker compose logs -f
# 看到帳號池就緒、監聽連接埠即表示啟動成功
```

### 3. 驗證

```bash
# 健康檢查
curl http://localhost:8080/health
# {"service":"kiro2api","status":"ok","version":"0.1.0"}

# 查看可用模型
curl http://localhost:8080/v1/models \
  -H "Authorization: Bearer sk-你的API金鑰"

# 傳送測試請求
curl -X POST http://localhost:8080/v1/chat/completions \
  -H "Content-Type: application/json" \
  -H "Authorization: Bearer sk-你的API金鑰" \
  -d '{"model":"claude-sonnet-4.5","messages":[{"role":"user","content":"你好"}]}'
```

看到 AI 回覆的文字即部署成功。如果傳回 401，請檢查 API Key 是否正確。

---

## 🧪 接入範例

> [!NOTE]
> 所有 API 請求都需要攜帶 API Key。支援兩種方式：
> - `Authorization: Bearer sk-xxx`（推薦，相容 OpenAI/Anthropic SDK）
> - `x-api-key: sk-xxx`
>
> base URL 用**標準裸前綴**：OpenAI = `{host}/v1`，Anthropic = `{host}`（SDK 自動補 `/v1/messages`），Gemini = `{host}/v1beta`。也可用顯式廠商前綴 `/openai/v1`、`/claude/v1`、`/gemini/v1beta`。

<details>
<summary><b>OpenAI SDK（Python）</b></summary>

```python
from openai import OpenAI

client = OpenAI(
    base_url="http://localhost:8080/v1",
    api_key="sk-你的API金鑰",
)

resp = client.chat.completions.create(
    model="claude-sonnet-4.5",
    messages=[{"role": "user", "content": "Hello"}],
)
print(resp.choices[0].message.content)
```

</details>

<details>
<summary><b>Anthropic SDK（Python）</b></summary>

```python
import anthropic

client = anthropic.Anthropic(
    base_url="http://localhost:8080",
    api_key="sk-你的API金鑰",
)

msg = client.messages.create(
    model="claude-sonnet-4.5",
    max_tokens=1024,
    messages=[{"role": "user", "content": "Hello"}],
)
print(msg.content[0].text)
```

</details>

<details>
<summary><b>Gemini SDK（Python）</b></summary>

```python
from google import genai

client = genai.Client(
    api_key="sk-你的API金鑰",
    http_options={"base_url": "http://localhost:8080/v1beta"},
)

resp = client.models.generate_content(
    model="claude-sonnet-4.5",
    contents="Hello",
)
print(resp.text)
```

</details>

<details>
<summary><b>cURL</b></summary>

```bash
# 非串流請求
curl -X POST http://localhost:8080/v1/chat/completions \
  -H "Content-Type: application/json" \
  -H "Authorization: Bearer sk-你的API金鑰" \
  -d '{"model":"claude-sonnet-4.5","messages":[{"role":"user","content":"Hi"}]}'

# 串流請求
curl -X POST http://localhost:8080/v1/chat/completions \
  -H "Content-Type: application/json" \
  -H "Authorization: Bearer sk-你的API金鑰" \
  -d '{"model":"claude-sonnet-4.5","messages":[{"role":"user","content":"Hi"}],"stream":true}'
```

</details>

<details>
<summary><b>函式呼叫（工具）</b></summary>

```python
resp = client.chat.completions.create(
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

> 工具呼叫在四種協議間**真透傳**（Anthropic `tool_use` / OpenAI `tool_calls` / Gemini `functionCall`），不做模擬。

</details>

---

## 📡 API 端點

> 📖 詳細 API 文件：[API.md](API.md)

> **雙前綴並存**：每協議同時提供「標準裸路徑」和「顯式廠商前綴路徑」。裸路徑讓官方 SDK 填 `base_url` 時無需加後綴，開箱即用；廠商前綴用於四家明確區分。

### OpenAI 相容（`/v1` 或 `/openai/v1`）

| 方法 | 端點 | 功能 |
|------|------|------|
| GET | `/models` | 可用模型列表 |
| POST | `/chat/completions` | 對話補全（串流傳回 `chat.completion.chunk` + `[DONE]`，含工具/圖片） |

### OpenAI Responses（`/v1/responses` 或 `/openai/v1/responses`）

| 方法 | 端點 | 功能 |
|------|------|------|
| POST | `/responses` | Responses API（串流為具名事件 + 單調 `sequence_number`，無 `[DONE]`；`previous_response_id` 傳回 400） |

### Anthropic 相容（`/v1` 對話入口；`/claude/v1` 顯式前綴）

| 方法 | 端點 | 功能 |
|------|------|------|
| POST | `/v1/messages` | Messages（串流/工具/圖片） |
| POST | `/v1/messages/count_tokens` | token 估算 |
| GET | `/claude/v1/models` | 模型列表（Anthropic 形狀，避開與 OpenAI `/v1/models` 衝突） |
| POST | `/claude/v1/messages` · `.../count_tokens` | 顯式前綴變體 |

### Gemini 原生（`/v1beta` 或 `/gemini/v1beta`）

| 方法 | 端點 | 功能 |
|------|------|------|
| GET | `/models` | 模型列表 |
| POST | `/models/{m}:generateContent` | 內容生成（非串流） |
| POST | `/models/{m}:streamGenerateContent` | 串流生成（`?alt=sse`，camelCase） |

### 管理 / 使用者 / 運維

| 方法 | 端點 | 功能 |
|------|------|------|
| GET | `/admin` · `/api/admin/*` | 管理面板 + 管理介面（憑 `adminApiKey`：憑證 CRUD / 登入 / API-KEY / 用量 / 日誌 / 餘額 / 設定 / 檢查更新 / 重啟） |
| GET | `/user` · `/api/user/*` | 使用者面板 + 介面（憑自身 API-KEY） |
| GET | `/health` · `/v1/ping` | 探活（不驗證） |

> URL 裡的 `localhost:8080` 只是範例；連接埠由 `PORT`/`config.json` 配置，按你的部署替換。
>
> Gemini/OpenAI 客戶端一律用本服務的**統一驗證**（Bearer/`x-api-key`/`?token=`），不是廠商原生的 `?key=`/`x-goog-api-key`。

---

## ⚙ 設定說明

優先級：**環境變數 > `config.json` > 內建預設**。掛載卷 `./data` 存放 `config.json`、`credentials.json`、日誌與執行態。

**環境變數**（見 `.env.example`）：

| 變數 | 必填 | 預設值 | 說明 |
|------|------|--------|------|
| `API_KEY` | ✅ | — | 對外呼叫金鑰（留空則協議端點開放存取，啟動告警） |
| `ADMIN_API_KEY` | ❌ | 回退 `API_KEY` | 管理端獨立驗證 key |
| `HOST` | ❌ | `127.0.0.1`（映像內建 `0.0.0.0`） | 監聽位址 |
| `PORT` | ❌ | `8080` | 服務連接埠 |
| `REGION` | ❌ | `us-east-1` | 預設 AWS region（帳號 `profileArn` 內的 region 優先） |
| `LOAD_BALANCING_MODE` | ❌ | `priority` | 負載平衡：`priority`（等權輪詢）/ `balanced`（按 weight 加權） |
| `MAX_RPM_PER_CREDENTIAL` | ❌ | `0` | 每帳號每分鐘請求上限，`0` = 無限 |
| `CREDENTIALS_PATH` | ❌ | `/app/data/credentials.json` | 憑證檔案路徑 |

**`data/config.json`**（camelCase，均可選；`logCapacity` 僅在此配置）：

```json
{
  "host": "0.0.0.0",
  "port": 8080,
  "region": "us-east-1",
  "apiKey": "sk-你的對外呼叫金鑰",
  "adminApiKey": "可選,管理端",
  "credentialsPath": "/app/data/credentials.json",
  "loadBalancingMode": "priority",
  "maxRpmPerCredential": 0,
  "logCapacity": 1000,
  "kiroVersion": "0.11.107",
  "systemVersion": "win32#10.0.22631",
  "nodeVersion": "22.22.0"
}
```

- `logCapacity`：即時日誌環形緩衝條數，`>0` 啟用日誌捕獲（管理面板日誌頁回放/SSE），`0` 關閉（日誌端點傳回 503）；預設 `1000`。
- `kiroVersion`/`systemVersion`/`nodeVersion`：偽裝 UA 版本號，從配置注入。

---

## ⚠ 注意事項

1. **對外部署務必設定 `API_KEY`**：留空時協議端點開放存取（啟動會告警）。`/admin`、`/user` 面板本體尚未接驗證，受保護的是 `/api/admin/*`、`/api/user/*`；裸機部署慎改 `HOST=0.0.0.0`。

2. **可用模型取決於帳號訂閱檔位**：免費檔（KIRO FREE）通常只授權 `claude-sonnet-4.5`；請求不支援的模型傳回 `400`（`INVALID_MODEL_ID`），不瞎重試、不誤傷帳號。

3. **令牌自癒**：token 到期自動記憶體刷新並原子落盤 `credentials.json`；真正的憑證失效才永久停用，配額/風控/限流一律冷卻自癒。

4. **串流輸出**：四種協議均支援串流；`stream:false` 時服務內部仍解碼事件流，收集完畢後一次性傳回完整 JSON。

5. **網路環境**：部署伺服器需能存取 AWS CodeWhisperer/Kiro 端點（`*.amazonaws.com`）。

---

## 🗺 開發路線

- [x] 四協議前端（OpenAI / Anthropic / OpenAI-Responses / Gemini）
- [x] Anthropic Messages 中樞母格式 + 統一中轉核心
- [x] 串流（SSE）+ 函式呼叫真透傳 + 圖片多模態
- [x] Kiro 帳號池（多帳號輪詢、分級冷卻、負載平衡）
- [x] 令牌單飛自動刷新 + 原子落盤
- [x] 端點回退（Kiro/CodeWhisperer/AmazonQ）+ 跨帳號重試
- [x] body-aware 失敗分類（永久失效才停用，其餘冷卻自癒）
- [x] 統一驗證閘（Bearer / x-api-key / ?token=）
- [x] Web 管理面板（憑證/登入/API-KEY/用量/日誌/餘額/設定）
- [x] 使用者面板（持有者用自身 API-KEY 登入）
- [x] 三種互動式登入流（Builder ID / IAM SSO / 社交令牌）
- [x] 每日/帳號用量統計（含客戶端 IP 與帳號標籤）
- [x] 即時日誌（SSE）+ 餘額快取 + 動態模型清單
- [x] 集成範例（協議×語言可複製片段）
- [x] 服務重啟 + 版本檢查更新（GitHub Release 比對）
- [x] Docker 多架構（amd64/arm64）交付 + CI
- [ ] `/admin`、`/user` 面板本體驗證
- [ ] GitHub Actions 自動建置並發布映像

---

## ☕ 贊賞 & 共享

覺得有幫助？請作者喝杯咖啡，或加入微信交流群獲取使用幫助。二維碼見管理面板儀表板。完整內容請查看 [SPONSORS.md](SPONSORS.md)。

kiro2api 主要由個人維護，歡迎透過程式碼、文件、修復或 PR 參與建設。

1. Fork 本倉庫
2. 建立分支 `git checkout -b feature/your-feature`
3. 提交程式碼 `git commit -m "feat: add something"`
4. 推送並建立 Pull Request

---

## 🙏 致謝

感謝所有在 [Issues](https://github.com/xwteam/kiro2api/issues) 裡提交 bug 復現、日誌、相容性回饋和功能建議的使用者。這些回饋直接推動了帳號池、令牌自癒、端點回退、多協議相容、Web 面板等核心能力的迭代。

---

## 📄 授權協議

本專案採用 [MIT 授權](../../LICENSE)：

- **允許**：個人學習、研究、自用部署、二次開發
- **要求**：保留版權與授權聲明

本專案與 Amazon / AWS / Kiro 無關聯。使用者需自行承擔風險並遵守相關服務條款。

---

<div align="center">
  <sub>Built with Rust + axum + tokio | Powered by Kiro (CodeWhisperer)</sub>
</div>
