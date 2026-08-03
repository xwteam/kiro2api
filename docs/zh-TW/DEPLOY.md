# 部署指南

本指南涵蓋使用 Docker 部署 kiro2api 的完整步驟，Docker 是生產環境的推薦部署方式。

## 系統要求

| 元件 | 最低要求 | 推薦配置 |
|------|---------|---------|
| Docker | 20.10+ | 最新穩定版 |
| 記憶體 | 512 MB | 2 GB+ |
| 磁碟空間 | 500 MB | 2 GB+ |
| 作業系統 | Linux / macOS / Windows | Linux（效能最佳） |
| 架構 | amd64 / arm64 | 官方映像檔多架構，自動匹配 |
| 網路 | 能訪問 AWS CodeWhisperer/Kiro 端點 | 穩定連線 |

> **提示：** 使用 Docker 部署無需在本機安裝 Rust 環境，只需 Docker 和一份有效的 Kiro 憑證即可。若要從原始碼建構才需要 Rust 2024 edition。

## 取得 Kiro 憑證

kiro2api 的後端是 Kiro（CodeWhisperer）帳號池，需要有效的 Kiro 憑證才能運作。按照以下步驟取得：

### 步驟 1：準備 Kiro 帳號

1. 你需要一個可用的 Kiro（CodeWhisperer）帳號
2. **可用模型取決於帳號訂閱檔位**：免費檔（KIRO FREE）通常只授權 `claude-sonnet-4.5`，opus/GPT 等需更高檔位
3. 請求不支援的模型會明確回傳 `400`（`INVALID_MODEL_ID`），而非靜默失敗

### 步驟 2：取得憑證欄位

從你的 Kiro 客戶端 / 現有 Kiro 憑證中匯出以下欄位，或使用管理面板的三種互動式登入（Builder ID 裝置碼 / IAM SSO 授權碼 / 社交令牌）現場取得：

| 欄位 | 說明 |
|------|------|
| `accessToken` / `refreshToken` | 存取令牌與刷新令牌（到期自動刷新） |
| `expiresAt` | 令牌過期時間（RFC3339 格式） |
| `authMethod` | `social`（帶 `profileArn`）或 `idc`（帶 `clientId`/`clientSecret`） |
| `profileArn` / `machineId` | `social` 登入方式所需的附加欄位 |

**提示：** 憑證可直接 drop-in 現有 Kiro 資料，無需轉換格式。

### 步驟 3：整理成陣列

1. 把一個或多個帳號憑證整理成 JSON 陣列
2. 每個帳號一個物件，確保欄位完整
3. 安全地儲存以供下一步使用（放進 `data/credentials.json`）

> **警告：** 令牌到期由服務在記憶體中自動刷新（單飛協調，避免並發刷新級聯 `401`），刷新成功後原子落盤回 `credentials.json`。真正的憑證失效才會永久停用該帳號，配額/風控/限流一律冷卻自癒。

## Docker 部署

### 快速開始（單帳號）

```bash
# 複製倉庫
git clone https://github.com/xwteam/kiro2api.git
cd kiro2api

# 複製環境範本
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

**重要事項：**
- `API_KEY` 為空**且尚未建立任何 API-KEY** 時，協議端點才**開放存取**（啟動時會告警）；在管理面發出第一條 API-KEY 之後協議閘即收口（不帶有效金鑰一律 `401`）。對外部署務必設定 `API_KEY`
- 容器映像檔已內建 `HOST=0.0.0.0`；裸機部署請勿輕易把 `HOST` 改成 `0.0.0.0`
- `/api/admin/*` 只有在設定了 `adminApiKey`（未設則回退 `apiKey`）之後才受保護；**兩個 key 都不設時管理介面完全開放**，任何人都能增刪憑證、改驗證金鑰、重啟服務，公網部署務必設定 `ADMIN_API_KEY`
- `/admin`、`/user` 面板本體始終不驗證，真正的閘在其 `/api/**` 介面上
- **空值等同未設定**：`API_KEY=`、`ADMIN_API_KEY=` 這種空賦值（含只有空白的值）在載入時一律被忽略，`config.json` 裡已設定的金鑰**繼續生效**、不會被覆蓋；環境變數的值也會先去除前後空白

啟動服務：

```bash
mkdir -p data
docker compose up -d
```

檢查日誌確認啟動成功：

```bash
docker compose logs -f
```

查看以下訊息：
- 帳號池就緒、監聽埠 `8080` — 服務已就緒
- 憑證缺失或全部失效告警 — 檢查 `credentials.json`

### 多帳號設定（負載均衡）

為了提高吞吐量和冗餘性，`credentials.json` 陣列中可放入多個 Kiro 帳號：

```json
[
  {
    "id": 12345,
    "accessToken": "...",
    "refreshToken": "...",
    "expiresAt": "2026-07-25T12:00:00Z",
    "authMethod": "social",
    "profileArn": "arn:aws:codewhisperer:us-east-1:...:profile/...",
    "weight": 2
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

每個帳號獨立 RPM 限流、分級冷卻；連續失敗按類別（永久失效 / 歧義鑑權 / 配額 / 瞬時）差異化處置。加 `"disabled": true` 可排除某帳號。

**負載均衡策略：**
- `priority`（預設）：等權輪詢，均勻分配請求到各帳號
- `balanced`：按每個帳號的 `weight` 加權分配

在 `.env` 中更改策略：
```env
LOAD_BALANCING_MODE=balanced
```

也可在管理面板執行期即時切換，無需重啟。

### 互動式登入 / 動態帳號管理

無需手動編輯檔案即可新增或管理帳號。在 `http://localhost:8080/admin` 開啟管理面板（憑 `adminApiKey` 登入），前往 **帳號管理**：

- **三種互動式登入**：Builder ID 裝置碼 / IAM SSO 授權碼 / 社交令牌，現場取得憑證
- **批次匯入**：貼上憑證陣列一次性匯入
- **增刪改查**：調整優先級/權重、標籤、餘額查詢
- 新增後即時生效，無需重啟服務

## 令牌自癒與端點回退

Kiro 令牌會定期過期。服務內建自動自癒機制，多數情況下無需人工介入。

### 自動令牌刷新

當某帳號的 `accessToken` 接近過期時，服務會自動在記憶體中用 `refreshToken` 刷新。刷新採用**單飛協調**（single-flight），避免並發請求同時刷新導致級聯 `401`。刷新成功後原子落盤回 `credentials.json`，重啟後仍有效。

### 端點回退與跨帳號重試

- **端點回退**：Kiro IDE → CodeWhisperer → AmazonQ 多端點按序回退，遇 `429`/網路錯自動切換
- **跨帳號重試**：帳號級失敗自動換下一個可用帳號
- **不誤傷帳號**：確定性請求錯誤（如不支援的模型 `INVALID_MODEL_ID`）**不瞎重試**，直接把上游原因回給客戶端
- **body-aware 失敗分類**：只有真正的憑證失效才永久停用，配額/風控/限流一律進入分級冷卻後自癒

### 透過 Web 面板手動管理

1. 在 `http://localhost:8080/admin` 開啟管理面板
2. 使用 `adminApiKey` 登入
3. 前往 **帳號管理**
4. 可查看每個帳號的餘額、狀態、冷卻情況
5. 需要時重新登入或更新憑證

無需重啟服務。

## 驗證

### 健康檢查

```bash
curl http://localhost:8080/health
```

預期回應：
```json
{"service":"kiro2api","status":"ok","version":"0.7.13"}
```

### 列出模型清單

```bash
curl http://localhost:8080/v1/models \
  -H "Authorization: Bearer sk-你的API金鑰"
```

> 這支回傳的是**寫死的**三條短清單，不依帳號訂閱檔位過濾——只能用來確認服務通了，不能拿來判斷「哪些模型我能用」。要看帳號實際授權的模型並集，請用管理端的 `GET /api/admin/models`。

### 測試 API 請求

```bash
curl -X POST http://localhost:8080/v1/chat/completions \
  -H "Content-Type: application/json" \
  -H "Authorization: Bearer sk-你的API金鑰" \
  -d '{
    "model": "claude-sonnet-4.5",
    "messages": [{"role": "user", "content": "你好"}]
  }'
```

你應該收到 AI 的回應。如果收到 `401` 錯誤，請驗證 API Key 是否正確。

> 對外協議呼叫需帶金鑰，可走驗證閘接受的任一通道，優先順序：`Authorization: Bearer <key>` > `x-api-key: <key>` > `x-goog-api-key: <key>`（Gemini 原生）> 查詢參數（`?api_key=` > `?token=` > `?key=`），皆為常量時間比較。`/health`、`/v1/ping` 探活端點不驗證。

## 常見問題排除

### 模型不可用（回傳 400）

**症狀：** 請求回傳 `400` 並帶 `INVALID_MODEL_ID`

**解決方案：**
1. **可用模型取決於帳號訂閱檔位**：免費檔（KIRO FREE）通常只授權 `claude-sonnet-4.5`
2. **別靠 `/v1/models` 排查**：協議端點的模型清單是寫死的短清單，不依帳號檔位過濾，出現在清單裡的模型照樣可能回 `400`；要看帳號實際授權了哪些模型，請用管理端的 `GET /api/admin/models`
3. 若需 opus/GPT 等模型，需升級 Kiro 帳號訂閱檔位
4. 這不是 bug：服務刻意不對確定性錯誤瞎重試，也不會誤傷帳號

### 憑證失效 / 帳號被停用

**症狀：** 日誌顯示帳號被永久停用，或全部帳號不可用

**解決方案：**
1. 確認 `refreshToken` 仍有效（真正失效才會永久停用）
2. 透過管理面板重新互動式登入該帳號
3. 新增更多帳號以實現自動跨帳號重試
4. 檢查日誌：`docker compose logs -f`

### 連接埠已被佔用

**症狀：** `Error: bind: address already in use`

**解決方案：**
```bash
# 查找使用連接埠 8080 的程序
lsof -i :8080

# 終止程序
kill -9 <PID>

# 或改用其他連接埠：編輯 .env 的 PORT，再重啟
# PORT=8081
docker compose down && docker compose up -d
```

> `PORT` 一處改動即可：應用監聽、compose 的連接埠映射（`${PORT:-8080}:${PORT:-8080}`）與健康檢查探測的連接埠都跟隨它，無需再改 `docker-compose.yml`。裸機部署同理，`PORT` 優先於 `config.json` 裡的 `port`。

### 網路無法訪問上游

**症狀：** 請求逾時或回傳網路錯誤，日誌顯示端點回退全部失敗

**解決方案：**
1. 確認部署伺服器能訪問 AWS CodeWhisperer/Kiro 端點（`*.amazonaws.com`）
2. 端點回退會依序嘗試 Kiro IDE → CodeWhisperer → AmazonQ，全部失敗才報錯
3. 檢查伺服器防火牆 / 代理設定
4. 若在受限網路，需為容器配置出站代理

### 令牌頻繁刷新失敗

**症狀：** 日誌反覆出現刷新 `401`

**解決方案：**
1. 確認 `refreshToken` 未被上游撤銷
2. 確認 `authMethod` 及對應欄位正確（`social` 需 `profileArn`；`idc` 需 `clientId`/`clientSecret`）
3. 透過管理面板重新登入取得新憑證
4. 刷新採單飛協調，不會因並發而級聯失敗

## 配置參考

優先順序：**命令列參數 > 環境變數 > `config.json` > 內建預設值**。命令列只有兩個參數：`-c/--config`（設定檔路徑）與 `--credentials`（憑證檔案路徑，不給則由 `CREDENTIALS_PATH`/`config.json`/預設值決定）。

| 變數 | 預設值 | 說明 |
|------|--------|------|
| `API_KEY` | — | 對外呼叫金鑰（留空**且未建立任何 API-KEY** 時協議端點開放存取，啟動告警） |
| `ADMIN_API_KEY` | 回退 `API_KEY` | 管理端獨立鑑權 key，保護 `/api/admin/*`；與 `API_KEY` 都不設時該介面開放，公網部署必填 |
| `HOST` | `127.0.0.1`（映像檔內建 `0.0.0.0`） | 監聽位址 |
| `PORT` | `8080` | 服務連接埠（compose 的連接埠映射與健康檢查都跟隨該值） |
| `REGION` | `us-east-1` | 僅供 `GET /api/admin/config` 的配置展示；**不影響實際呼叫**——資料面與令牌刷新的 region 取自帳號 `profileArn`，其次該帳號自身的 `region` 欄位，最後回落寫死的 `us-east-1` |
| `LOAD_BALANCING_MODE` | `priority` | 負載均衡：`priority`（等權輪詢）/ `balanced`（按 `weight` 加權） |
| `MAX_RPM_PER_CREDENTIAL` | `0` | 每帳號每分鐘請求上限，`0` = 無限制 |
| `CREDENTIALS_PATH` | `credentials.json`（相對 `-c` 設定檔所在目錄解析，容器內即 `/app/data/credentials.json`） | 憑證檔案路徑；被命令列 `--credentials` 覆蓋 |

> 憑證路徑同時決定用量統計（`stats/`）、API-KEY 儲存（`api_keys.json`）與餘額快取的落盤目錄——它們都取 `credentials.json` 的上層目錄。**映像檔並不設定 `CREDENTIALS_PATH`**（唯一內建的 ENV 是 `HOST=0.0.0.0`）：內建預設值會就近解析到 `-c` 所指設定檔的所在目錄，而容器以 `-c /app/data/config.json` 啟動，故這些資料預設就落在掛載卷 `/app/data` 裡；也正因為映像檔不烘焙這個變數，`config.json` 裡的 `credentialsPath` 仍然生效（環境變數層優先級高於 `config.json`，若烘焙進映像檔反而會把使用者自訂的路徑靜默改道）。自訂路徑時請一併指向掛載卷，否則容器重建即遺失。

**`data/config.json`（camelCase，均可選；`logCapacity` 僅在此配置）：**

```json
{
  "host": "0.0.0.0",
  "port": 8080,
  "region": "us-east-1",
  "apiKey": "sk-你的對外呼叫金鑰",
  "adminApiKey": "可選，管理端",
  "credentialsPath": "/app/data/credentials.json",
  "loadBalancingMode": "priority",
  "maxRpmPerCredential": 0,
  "logCapacity": 5000,
  "kiroVersion": "0.11.107",
  "systemVersion": "win32#10.0.22631",
  "nodeVersion": "22.22.0"
}
```

- `logCapacity`：即時日誌環形緩衝條數，`> 0` 啟用日誌捕獲（管理面板日誌頁回放/SSE），`0` 關閉（日誌端點回傳 `503`）；預設 `5000`。
- `kiroVersion`/`systemVersion`/`nodeVersion`：偽裝 UA 版本號，從配置注入。

## Docker Compose 參考

關鍵卷及其用途：

```yaml
volumes:
  - ./data:/app/data                  # 持久化資料（config.json、credentials.json、日誌、執行態）
  - /etc/localtime:/etc/localtime:ro  # 系統時區
```

容器內以非 root 使用者 `appuser`（UID 1000）執行：`docker-entrypoint.sh` 先以 root `chown` 掛載卷再 `gosu` 降權（無縫升級舊版 root 建立的 `data`）。映像檔內建 `HEALTHCHECK` 與 compose healthcheck（探測連接埠按 `PORT` 環境變數 > `data/config.json` 的 `port` > `8080` 解析，與應用監聽的連接埠一致），`restart: unless-stopped`。

在 `docker-compose.yml` 中修改時區：
```yaml
environment:
  - TZ=Asia/Taipei  # 改為你的時區
```

<details>
<summary><b>裸機 / 本機執行（從原始碼建構）</b></summary>

無需 Docker 時，可用 Rust 2024 edition 直接建構：

```bash
cargo build --release
API_KEY=sk-xxx ./target/release/kiro2api \
  -c data/config.json \
  --credentials data/credentials.json
```

> 設定優先順序：**命令列參數 > 環境變數 > `config.json` > 內建預設值**。`--credentials` 不給時，由 `CREDENTIALS_PATH` / `config.json` 的 `credentialsPath` / 內建預設的 `credentials.json`（就近解析到 `-c` 所指設定檔的所在目錄；裸機用預設的 `-c config.json` 時該路徑無目錄，等同相對目前工作目錄）決定；用量統計、`api_keys.json` 與餘額快取都落在該檔案的上層目錄裡。

> 裸機部署請勿輕易把 `HOST` 改成 `0.0.0.0`。`/admin`、`/user` 面板本體始終不驗證，`/api/admin/*` 只有在設定了 `adminApiKey`（未設則回退 `apiKey`）之後才受保護——一個都不設時管理介面對所有人開放；`/api/user/*` 不走該閘，始終要求呼叫方自帶有效 API-KEY（無效/停用/過期即 401）。

</details>

<details>
<summary><b>發布與升級</b></summary>

- **CI**：`push tags: v*` → 建構多架構映像檔（amd64/arm64）並發布到 GHCR（tag = `X.Y.Z` + `X.Y` + `latest`）。
- **升級**：`docker compose pull && docker compose up -d`（掛載卷 `./data` 的擁有者由 entrypoint 自動修正）。
- **上線切換建議**：生產上線建議先在旁路埠起新映像檔、與線上並行比對（相同請求 → 輸出一致）通過後再切換，舊映像檔留存可回滾。

</details>

## 後續步驟

- 閱讀 [USAGE.md](USAGE.md) 了解 Web 面板和客戶端整合
- 閱讀 [API.md](API.md) 查看詳細的 API 端點文檔
- 查看 [README.md](../../README.md) 了解架構和進階功能
