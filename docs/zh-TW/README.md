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
  <img src="https://img.shields.io/badge/version-v0.15.0-success?style=flat-square" alt="Version">
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
> `apiKey`/`API_KEY` 為空**且尚未建立任何 API-KEY** 時，協議端點會**開放存取**（啟動會告警）；在管理面發出第一條 API-KEY 之後協議閘即收口，不帶有效金鑰的請求一律 `401`。對外部署務必設定。管理介面 `/api/admin/*` 只有在設定了 `adminApiKey`（缺省回退 `apiKey`）之後才受保護——**兩個 key 都不設時，管理介面跟面板一樣是開放的**，任何人都能增刪憑證、改驗證金鑰；`/admin`、`/user` 面板本體則始終不驗證。部署到公網必須設定 `ADMIN_API_KEY`。容器映像已內建 `HOST=0.0.0.0`；裸機部署請勿輕易把 `HOST` 改成 `0.0.0.0`。

> [!TIP]
> 後端為 Kiro（CodeWhisperer）帳號池。**可用模型取決於帳號訂閱檔位**：免費檔（KIRO FREE）通常只授權 `claude-sonnet-4.5`，opus/GPT 等需更高檔位——請求不支援的模型會明確傳回 `400`（`INVALID_MODEL_ID`），而非靜默失敗。

---

## 📝 最近更新

> 下表只列**最近 10 次**更新;完整更新日誌請查看 [CHANGELOG.md](../../CHANGELOG.md)。

| 日期 | 更新內容 |
|------|----------|
| 2026-08-10 | v0.15.0 - 🔁 **额度耗尽的账号不再每次重启都要重新学一遍**(用户直接问到:"额度用完的号不是已经禁用了吗,为什么还会请求到已禁用的账号?")。是禁用了,但只在**内存**里——v0.10.2 为免一次抖动把账号永久写死,让运行期停用不落盘;而配额恰恰是**有明确恢复时刻**的那类。于是每次发版重启就把"谁没额度"忘光,再拿**用户的请求**去重新发现(实测 13 个耗尽号:前 2 次 502、第 3 次才成)→ 现按恢复时刻落盘,重启仍记得、到点自动回池;恢复时刻优先取上游 `nextResetAt`,没有才按下月一号估。另:**服务端内置搜索 `web_search` 真正接上了** —— 此前只是"容忍"这个工具声明(不再 400),但从不真的搜索,模型照常回段文本、客户端以为搜过了。现在这类请求在进数据面前被截住,调上游 `/mcp` 端点拿结果再合成 `server_tool_use` + `web_search_tool_result` |
| 2026-08-10 | v0.14.1 - 🔧 **线上验证时当场发现的两件事**。①响应里 `usage.input_tokens` **恒为 0**:v0.13.0 补的输入估算只喂给了计费、没回写客户端 —— 账单里有、响应里没有,而客户端拿它算成本和上下文占用 → 非流式 `usage` 与流式 `message_start` 现在都带同一个值;②**池里有一批耗尽的号时,用户前几次请求会连续失败**:配额耗尽此前与鉴权失败共用 3 次重试预算,实测 13 个耗尽号要连烧 2 次 502、第 3 次才成 → 配额是**确定性**结论(本周期换谁都一样),现与"模型不可用"同归账号级确定性档、按池大小给预算;瞬态/鉴权仍保持 3 次小上限。全池确实耗尽时回 `429` 并说清是额度问题,而不是语焉不详的 502 |
| 2026-08-10 | v0.14.0 - 🧩 **對照收尾**。①歷史裡助手輪 `content` 可能是**空串**,上游據此拒掉整條請求(純工具呼叫那一輪就是空,使用者輪早有兜底、助手輪一直漏著)→ 用單個空格佔位;②**損壞幀被逐位元組重掃,而它的邊界本來已知**(prelude CRC 通過則 `total_len` 可信)→ 整幀跳過;③**`tlsBackend` 改為執行時可切**(此前編譯期二選一)——走自簽 CA 代理時往往只有一個後端握得上手 |
| 2026-08-09 | v0.13.0 - 🧠 **擴展思考(thinking)完整接上**,此前整個功能缺失:請求側 `thinking` 欄位被靜默丟棄,回應側上游把思考用 `<thinking>…</thinking>` 包在普通文字裡下發、我們原樣透傳 → 客戶端把整段思考當正文顯示。現按 enabled/adaptive 生成指令注入 system 最前,並切成獨立 `thinking` 區塊,串流與非串流共用同一份增量切分器;**普通文字零延遲透傳**。另修:**token 估算對中文低估約三倍**;**串流記帳 input token 恆為 0**;**上下文視窗全表釘死 200K**(上游 `maxInputTokens` 解析了又丟)→ 拆成兩個欄位 |
| 2026-08-09 | v0.12.0 - 🎚️ **不同訂閱檔位的帳號終於能共存**(使用者提供的真實 ksk 實測驗證)。①`INVALID_MODEL_ID` 被歸為**請求級**錯誤直接回 400,而它其實是**帳號級**的——可用模型由檔位決定,而我們對客戶端暴露的是全池**並集** → 拆出 `ModelUnavailable`,不罰帳號但換號再試;②換號預算只有 3 次,而支援該模型的號可能排第 14 → 模型不可用**不佔**帳號故障預算。另:記住「誰不支援哪個模型」;**`priority` 此前只是 `weight` 的別名、從不參與選號** → 現數字越小越優先,匯入一律 999;非 us-east-1 帳號重新整理模型必然失敗 → 回落 `q.{region}` |
| 2026-08-09 | v0.11.1 - 🔬 **線上實測挖出的兩件事**。①**工具描述為空時上游拒掉整條請求**(實測:同一工具帶描述 200,去掉描述 → `400 Invalid tool use format / REQUEST_BODY_INVALID`)。v0.11.0 把 null 改成空串,而上游要的是**非空** → 現用工具名兜底;②`REQUEST_BODY_INVALID` 被當成可重試:它是確定性的,此前一條畸形請求連燒幾個帳號的重試配額最後回個 502 → 現直接回 400 並點明多半是工具規格的問題 |
| 2026-08-09 | v0.11.0 - 🧰 **修「會讓請求被上游拒」的一類**。①服務端內建工具(web_search 等)沒有 `input_schema`,而該欄位此前是必填 → 客戶端一用官方寫法,請求就在我們這層 400;②工具 `description` 會序列化成 **null**;③`input_schema` 原樣透傳,形狀不合法會被上游拒掉**整條請求** → 現統一規範化;④超長工具名不縮短(上游上限 63)、縮短後也無法還原 → 現確定性縮短並把 `短名→原名` 帶到出口;⑤`:message-type == "error"` 的框架級錯誤幀被整個忽略 → 上游報錯卻還原成 200+空訊息;⑥面板缺失資源返回 200+HTML 而非 404。另:新增 `POST /api/admin/credentials/{id}/refresh` |
| 2026-08-09 | v0.10.2 - 🩹 **修:執行期停用被寫進磁碟,「重啟即復活」形同虛設**。v0.10.0 宣告只置記憶體態,但落盤時 `snapshot_credentials()` 拿執行時 `disabled` 覆寫了 `cred.disabled` —— 一次額度耗盡或兩次 401/403 就把帳號**永久**寫死在 credentials.json 裡,比修復前更糟。現落盤只取持久結論。**若你的 credentials.json 裡有你沒手工停過的 `"disabled": true`,改回 false 即可恢復** |
| 2026-08-09 | v0.10.1 - 🔌 **出站代理按帳號分流 + 補齊工作階段身分欄位**。①代理三件套此前**收下即丟**、`hasProxy` 還硬編碼 false —— 面板顯示配好了、實際全程直連;現真正落庫生效,優先級 憑證級 > 全域 > 直連(`"direct"` 顯式直連),且**同一帳號的資料面/重新整理/餘額/模型清單/背景續期一律同一出口**;②`conversationId` 此前每請求新生成且不是 UUID 形狀 → 改為優先取客戶端 `metadata.user_id` 裡的 session UUID,同工作階段共用;③此前**根本不發** `agentContinuationId`,已補;④管理面新增的帳號不會凍結 machineId,現入池即凍結;⑤`isCurrent` 硬編碼 false,現報真值 |
| 2026-08-09 | v0.10.0 - 🎯 **按行為形態對齊真實客戶端**。對照一個長期穩定的同類實作逐模組比對後,前兩版對封號的歸因被推翻:那份實作既複用連線、也不鎖 HTTP/1.1、TLS 還預設 rustls,我們賭的三件事它一件沒做。真正的差異是:①`priority` 此前**每請求換一個帳號**,上游在同一 IP 上看到幾百個 machineId 秒級交替 → 改為**黏住一個帳號直到它不可用**;②封停/額度耗盡的帳號此前冷卻 5/30 分鐘後**自動回池**,等於永不停止地去撞牆 → 改為停止使用(記憶體態,重設可復活);③**令牌重新整理請求連 User-Agent 都沒有**(實測位元組),而那是 Kiro 自家端點、每個帳號必走 → 按 axios/sso-oidc 兩種真實形態補齊;④**machineId 每重新整理一次就變**(由會輪換的 refreshToken 現算)→ 載入時凍結落盤;⑤ksk 帳號 machineId 退化成全域常數 → 按型別互斥衍生。另:429 改判瞬時限流、資料面端點三個收斂為一個、`amz-sdk-invocation-id` 改 UUID v4、標頭順序對齊、補 `claude-opus-5` 對應、SSE 加 25 秒保活 |

---

## 🌟 核心功能

> 📖 詳細使用文件：[USAGE.md](USAGE.md)

### 🔌 四協議前端，一套後端

- 一個服務同時提供 **OpenAI Chat**、**Anthropic Messages**、**OpenAI Responses**、**Gemini 原生** 四種 SDK 格式
- 內部以 **Anthropic Messages 為中樞母格式**，其餘協議雙向轉換後複用同一條中轉核心
- 每個協議都支援**串流（SSE）**、**函式呼叫（工具）真透傳**、**圖片輸入（多模態）**
- **雙前綴掛載**：每協議同時掛標準裸前綴與顯式廠商前綴（`/openai/v1`、`/claude/v1`、`/gemini/v1beta`），主流 SDK 填 `base_url` 即插即用

### 🔐 統一驗證閘

- 六條攜帶通道，依優先順序：`Authorization: Bearer` > `x-api-key` > `x-goog-api-key` > 查詢參數（`?api_key=` > `?token=` > `?key=`），常數時間比較，失敗即 `401`
- `adminApiKey`（缺省回退 `apiKey`）保護 `/api/admin/*`，兩者都未設定時該閘為開放模式；持有者用自己的 **API-KEY** 存取 `/api/user/*`
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
- **儀表板**：執行時間即時計時、全域剩餘積分、系統資訊（版本/Rust/OS/記憶體/CPU/PID/執行模式）、贊助二維碼卡（即時拉取遠端配置）、**檢查更新**（GitHub Release 比對；對話框顯示當前語言的發行說明 + 可複製的升級指令）
- **帳號管理**：增刪改查、三種互動式登入、批次匯入（逐條探活驗證 + 去重，即時進度條與逐條狀態清單）、優先級/權重、餘額查詢
- **模型測試**：從面板向任一模型傳送測試請求以驗證連通性；未建立自訂 key 時預設回退主 API 金鑰
- **API-KEY 管理**：發放/停用/改標籤、按 key 用量與分頁記錄；消費上限與用量計量在 Anthropic / OpenAI / OpenAI-Responses / Gemini 四個協議前端一律生效
- **用量統計**：每日/帳號維度、含客戶端 IP 與帳號標籤、按日下鑽
- **即時日誌**：結構化表格 + 方向過濾 + 搜尋 + 分頁 + SSE 即時推送 + 下載
- **設定**：執行期切負載平衡/驗證金鑰、集成範例（協議×語言可複製片段）、**一鍵重啟服務**
- 頂部控制欄：執行狀態徽章、GitHub、重啟、深淺色主題、5 語言切換

### 👤 使用者面板

- 內建使用者台（`/user`），持有者用自己的 **API-KEY** 登入（無需 admin 權限）
- 檢視該 key 的額度、累計用量與分頁記錄，由 `/api/user/*` 驅動

### 🧭 模型名映射

- 客戶端傳入的模型名按**小寫子字串**匹配到 Kiro 內部模型（未匹配到 → `400`）
- 協議端點的 `/models` 傳回一份**寫死的**常用 id 短清單，**不依帳號訂閱檔位過濾**（列出的模型仍可能回 `400`）；要看帳號實際授權的動態並集請用 `GET /api/admin/models`

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
# 管理端獨立金鑰；公網部署必填（不設則 /api/admin/* 回退用 API_KEY 驗證，兩者都不設即開放）。
# 不需要就把整行註解掉或留空——空值（含純空白）一律視為未設定，不會覆蓋 config.json 裡已設定的金鑰。
ADMIN_API_KEY=sk-你的管理端金鑰
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
# {"service":"kiro2api","status":"ok","version":"0.15.0"}

# 查看模型清單（固定短清單，不依帳號檔位過濾）
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
> 所有 API 請求都需要攜帶 API Key。最常用的兩種攜帶方式是：
> - `Authorization: Bearer sk-xxx`（推薦，相容 OpenAI/Anthropic SDK）
> - `x-api-key: sk-xxx`
>
> 驗證閘另受理 `x-goog-api-key` 標頭與 `?api_key=` / `?token=` / `?key=` 查詢參數，共六條通道，優先順序為：`Authorization: Bearer` > `x-api-key` > `x-goog-api-key` > `?api_key=` > `?token=` > `?key=`。
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
| GET | `/models` | 模型清單（寫死的短清單，不依帳號檔位過濾） |
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
| GET | `/claude/v1/models` | 模型清單（Anthropic 形狀，避開與 OpenAI `/v1/models` 衝突；同為寫死的短清單，且與 OpenAI／Gemini 那兩份內容不一致） |
| POST | `/claude/v1/messages` · `.../count_tokens` | 顯式前綴變體 |

### Gemini 原生（`/v1beta` 或 `/gemini/v1beta`）

| 方法 | 端點 | 功能 |
|------|------|------|
| GET | `/models` | 模型清單（寫死的短清單，不依帳號檔位過濾） |
| POST | `/models/{m}:generateContent` | 內容生成（非串流） |
| POST | `/models/{m}:streamGenerateContent` | 串流生成（`?alt=sse`，camelCase） |

### 管理 / 使用者 / 運維

| 方法 | 端點 | 功能 |
|------|------|------|
| GET | `/admin` · `/api/admin/*` | 管理面板 + 管理介面（憑 `adminApiKey`，未設定任何 key 時開放：憑證 CRUD / 登入 / API-KEY / 用量 / 日誌 / 餘額 / 設定 / 檢查更新 / 重啟） |
| GET | `/user` · `/api/user/*` | 使用者面板 + 介面（憑自身 API-KEY） |
| GET | `/health` · `/v1/ping` | 探活（不驗證） |

> URL 裡的 `localhost:8080` 只是範例；連接埠由 `PORT`/`config.json` 配置，按你的部署替換。
>
> 憑證可走驗證閘接受的任一通道，優先順序：`Authorization: Bearer` > `x-api-key` > `x-goog-api-key` > 查詢參數（`?api_key=` > `?token=` > `?key=`）。Gemini 原生的 `x-goog-api-key` 與 `?key=` **同樣受理**，官方 `google-genai` SDK 換掉 `base_url` 即可直用；要換的是**值**——一律填**本服務**的 API Key，不是廠商真金鑰。

---

## ⚙ 設定說明

優先級：**命令列參數 > 環境變數 > `config.json` > 內建預設**。命令列只有兩個參數：`-c/--config`（設定檔路徑）與 `--credentials`（憑證檔案路徑，不給則由 `CREDENTIALS_PATH`/`config.json`/預設值決定）。掛載卷 `./data` 存放 `config.json`、`credentials.json`、日誌與執行態。

> 憑證路徑同時決定用量統計（`stats/`）、API-KEY 儲存（`api_keys.json`）與餘額快取的落盤目錄——它們都取 `credentials.json` 的上層目錄。內建預設值會相對 `-c` 指定的設定檔所在目錄解析，而容器以 `-c /app/data/config.json` 啟動，預設憑證路徑因此落在 `/app/data/credentials.json`，這些資料預設就落在掛載卷裡；自訂路徑時請一併指向掛載卷，否則容器重建即遺失。

**環境變數**（見 `.env.example`）：

| 變數 | 必填 | 預設值 | 說明 |
|------|------|--------|------|
| `API_KEY` | ✅ | — | 對外呼叫金鑰（留空**且未建立任何 API-KEY** 時協議端點開放存取，啟動告警） |
| `ADMIN_API_KEY` | ❌ | 回退 `API_KEY` | 管理端獨立驗證 key；與 `API_KEY` 都不設時 `/api/admin/*` 開放，公網部署必填 |
| `HOST` | ❌ | `127.0.0.1`（映像內建 `0.0.0.0`） | 監聽位址 |
| `PORT` | ❌ | `8080` | 服務連接埠（compose 的連接埠映射與健康檢查都跟隨該值） |
| `REGION` | ❌ | `us-east-1` | 僅供 `GET /api/admin/config` 的配置展示；**不影響實際呼叫**——資料面與令牌刷新的 region 取自帳號 `profileArn`，其次該帳號自身的 `region` 欄位，最後回落寫死的 `us-east-1` |
| `LOAD_BALANCING_MODE` | ❌ | `priority` | 負載平衡：`priority`（等權輪詢）/ `balanced`（按 weight 加權） |
| `MAX_RPM_PER_CREDENTIAL` | ❌ | `0` | 每帳號每分鐘請求上限，`0` = 無限 |
| `CREDENTIALS_PATH` | ❌ | `credentials.json`（相對 `-c` 設定檔所在目錄解析，容器內即 `/app/data/credentials.json`） | 憑證檔案路徑；被命令列 `--credentials` 覆蓋 |

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
  "logCapacity": 5000,
  "kiroVersion": "0.11.107",
  "systemVersion": "win32#10.0.22631",
  "nodeVersion": "22.22.0"
}
```

- `logCapacity`：即時日誌環形緩衝條數，`>0` 啟用日誌捕獲（管理面板日誌頁回放/SSE），`0` 關閉（日誌端點傳回 503）；預設 `5000`。
- `kiroVersion`/`systemVersion`/`nodeVersion`：偽裝 UA 版本號，從配置注入。

---

## ⚠ 注意事項

1. **對外部署務必設定 `API_KEY` 與 `ADMIN_API_KEY`**：`API_KEY` 留空且未建立任何 API-KEY 時協議端點開放存取（啟動會告警，發出第一條 API-KEY 後即收口）；`adminApiKey`/`apiKey` 都不設時 `/api/admin/*` 同樣開放，憑證、API-KEY、驗證設定都能被任意改寫。`/admin`、`/user` 面板本體始終不驗證（真正的閘在其 `/api/**` 介面上）；裸機部署慎改 `HOST=0.0.0.0`。

2. **可用模型取決於帳號訂閱檔位**：免費檔（KIRO FREE）通常只授權 `claude-sonnet-4.5`；請求不支援的模型傳回 `400`（`INVALID_MODEL_ID`），不瞎重試、不誤傷帳號。

3. **令牌自癒**：token 到期自動記憶體刷新並原子落盤 `credentials.json`；真正的憑證失效才永久停用，配額/風控/限流一律冷卻自癒。

4. **串流輸出**：四種協議均支援串流；`stream:false` 時服務內部仍解碼事件流，收集完畢後一次性傳回完整 JSON。上游報錯或串流中途傳輸中斷（連線重置 / 讀取逾時 / 分塊未收尾）時，一律以該協議自身的錯誤事件收束（Anthropic `error` 事件、OpenAI 錯誤 chunk 且不補 `[DONE]`、Responses `response.failed`、Gemini 錯誤區塊），**絕不會被當成正常完成**；**上游自己**判定截斷（它自己的長度預算或上下文耗盡）時如實回報截斷原因（`max_tokens` / `length` / `MAX_TOKENS` / `incomplete`）——這與客戶端傳的 `max_tokens` / `maxOutputTokens` 無關，那些參數**相容接受但不轉發給上游、不會限制回應長度**（詳見 [API.md](API.md)）。

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
- [x] 統一驗證閘（Bearer / x-api-key / x-goog-api-key / `?api_key=` / `?token=` / `?key=`）
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
