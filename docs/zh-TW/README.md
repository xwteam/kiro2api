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
  <img src="https://img.shields.io/badge/version-v0.10.0-success?style=flat-square" alt="Version">
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

> 完整更新日誌請查看 [CHANGELOG.md](../../CHANGELOG.md)。

| 日期 | 更新內容 |
|------|----------|
| 2026-08-09 | v0.10.0 - 🎯 **按行為形態對齊真實客戶端**。對照一個長期穩定的同類實作逐模組比對後,前兩版對封號的歸因被推翻:那份實作既複用連線、也不鎖 HTTP/1.1、TLS 還預設 rustls,我們賭的三件事它一件沒做。真正的差異是:①`priority` 此前**每請求換一個帳號**,上游在同一 IP 上看到幾百個 machineId 秒級交替 → 改為**黏住一個帳號直到它不可用**;②封停/額度耗盡的帳號此前冷卻 5/30 分鐘後**自動回池**,等於永不停止地去撞牆 → 改為停止使用(記憶體態,重設可復活);③**令牌重新整理請求連 User-Agent 都沒有**(實測位元組),而那是 Kiro 自家端點、每個帳號必走 → 按 axios/sso-oidc 兩種真實形態補齊;④**machineId 每重新整理一次就變**(由會輪換的 refreshToken 現算)→ 載入時凍結落盤;⑤ksk 帳號 machineId 退化成全域常數 → 按型別互斥衍生。另:429 改判瞬時限流、資料面端點三個收斂為一個、`amz-sdk-invocation-id` 改 UUID v4、標頭順序對齊、補 `claude-opus-5` 對應、SSE 加 25 秒保活 |
| 2026-08-09 | v0.9.1 - 🔧 **上游連線鎖 HTTP/1.1 + TLS 後端改 native-tls**。v0.9.0 加了 `Connection: close` 卻沒鎖協議——它是 HTTP/1.1 的標頭、**h2 明確禁止**,而我們協商到的正是 h2,所以那個標頭當時**只是擺設**(實測鎖定後出站協議由 HTTP/2 變為 HTTP/1.1)。TLS 後端預設改 **native-tls(OpenSSL)** 以貼合真實客戶端的 ClientHello 指紋。另:machineId 增加設定級兜底 |
| 2026-08-09 | v0.9.0 - 🔥 **整池帳號被上游成批封停:同一條 TCP 連線上輪換了多個帳號身分**。中轉走連線池(idle 90s),同一條 TCP/TLS 上依次發出不同帳號的令牌,而每個帳號在 user-agent 裡還各自聲稱是不同機器。線上 1046 個帳號燒到只剩 22 個健康,而**從未經過中轉**的帳號直查上游全部正常。現數據面帶 `Connection: close`,兩個客戶端均 `pool_max_idle_per_host(0)`;UA 的 SDK 版本對齊被觀測的真實客戶端。**診斷修正**:初版歸因於「換號太快」並做了熔斷器,經對照分析**被證偽**,熔斷器已整個回退 |
| 2026-08-03 | v0.8.1 - 🐛 面板上沒有地方填 Kiro API Key:v0.8.0 後端與介面都做好了,卻**沒給「新增帳號」表單加輸入框**,從介面看這功能等於不存在。現在認證方式下拉多一項 **API Key (`ksk_…`)**,選中即顯示 ksk 輸入框並隱藏 refreshToken 那一欄,提交也不再要求它 |
| 2026-08-03 | v0.8.0 - ✨ **支援用 Kiro API Key(`ksk_…`)匯入帳號**。key 本身就是資料面 bearer,不換令牌、不刷新、不過期。匯入寫 `{"kiroApiKey":"ksk_xxx","authMethod":"api_key"}`,或呼叫介面傳 `kiroApiKey`。實作按觀測對齊:帶 `tokentype: API_KEY` 標頭、machineId 用 `KiroAPIKey/` 鹽、刷新鏈路顯式短路、過期判定恆答否。另:宣告 `api_key` 卻沒給 key 的憑據**入池即停用**,且「重置」拒絕救活它 |
| 2026-08-03 | v0.7.14 - 🐛 首頁「全域剩餘積分」經常空白、要點刷新才有:聚合只累加**仍新鮮**(5 分鐘 TTL)的快取,超過 5 分鐘沒打開帳號頁首頁就是空的——盤上明明有全部帳號餘額卻不顯示,逼你點刷新,**而那次刷新正是這份快取本該避免的上游呼叫**。現展示取全部條目並帶出資料年齡。✨ 新增**活躍帳號令牌提前續期**:近 24h 用過的帳號在到期前 10 分鐘後台續上;**刻意只續活躍帳號**——全池定時續期等於給 253 個帳號造一條永不停歇的心跳,而本專案帳號已被上游封過 24 個 |
| 2026-08-03 | v0.7.13 - 🐛 **codex 的 502:中轉自己造出了畸形請求**。上游要求訊息裡有 `toolUse` 時 `toolConfig` 必須存在,而內建工具在轉換時被合法丟棄(v0.7.1),客戶端某輪只帶內建工具時 `tools` 成了空陣列、歷史裡的工具呼叫卻還在。現在發往上游前會**從對話歷史把呼叫過的工具名補成最小規格**。另:`TOOL_CONFIG_MISSING` 歸入確定性請求錯誤(此前落在瞬時錯誤,半小時 26 次波及 25 個帳號) |
| 2026-08-03 | v0.7.12 - 🐛 **一個超長請求能把整個帳號池打傷、最終 503**。上游對超長請求回 `400 CONTENT_LENGTH_EXCEEDS_THRESHOLD`,而確定性請求錯誤此前只認 `INVALID_MODEL_ID`,這個碼落進「瞬時錯誤」→ 換任何帳號都不可能成功的請求被跨帳號重試一遍,每換一個就記一次失敗。實測一個下午把 253 個健康帳號打成 149 帶傷、26 冷卻。現歸入 `InvalidRequest`(不重試/不冷卻/不累計 strike);客戶端也不再收到無資訊的 `502`,改回 `400` 並明說是上下文超限且不會自癒 |
| 2026-08-01 | v0.7.11 - 🐛 測試套件把暫存目錄漏在 `/tmp` 裡、從不回收:125 處測試各自拼路徑(`temp_dir().join(...)`),沒有 guard 也沒有收尾,行程一結束就成孤兒。一次磁碟爆滿排查中,`/tmp` **頂層堆著 9582 個**這樣的殘留,而它們只累積了 4 天;systemd-tmpfiles 對 `/tmp` 的老化是 30 天,遠追不上。代價不在體積(合計僅 52M)而在污染——頂層近萬條目錄項,恰恰在排查磁碟問題時最礙事。現全部收進單一 per-process 根目錄 `/tmp/kiro2api-tests/<pid>/`,每個測試行程啟動時回收 **pid 已不在 `/proc`** 的舊根,下一輪自動清掉上一輪。**只改測試基礎設施,執行期行為完全不變** |
| 2026-07-29 | v0.7.10 - 🐛 發版後面板仍顯示舊版本/舊行為(後端已 0.7.9,面板「檢查更新」卻顯示 0.7.6):靜態資源**一個快取標頭都不發**,瀏覽器因此可**啟發式快取**、自行決定存多久。服務端三個介面當時全回 0.7.9,錯的只是瀏覽器手裡那份副本。現加 `Cache-Control: no-cache` + 內容 SHA-256 強 `ETag`,支援 `If-None-Match` → `304` |
| 2026-07-29 | v0.7.9 - 🐛 「可用帳號」把所有不健康的號都算了進去(封禁/額度耗盡/令牌過期/續期被拒):統計卡在前端複算 `!a.disabled`,而這幾類都不是「停用」,`disabled` 恆為 false。現只數健康檔;刻意不用後端的 `available`(那個數答的是「中轉此刻會去嘗試哪些帳號」,額度耗盡/過期的號冷卻一過仍在其中)。儀表板同步改成同一口徑 |
| 2026-07-29 | v0.7.8 - 🐛 額度只用了 0.08 就被 402 攔死(v0.7.6 回歸):單次在途預留取 1.0 credits,而 v0.7.6 後「已花」終於是真值,於是 1 credit 的上限從第一發起就 `0.08 + 1.0 > 1.00`。預留改為貼近實測:credits 0.25、USD 0.05。中途試過按上限比例封頂預留,被測試否掉(`SpendCache` 的前提是 est ≥ 單次真實花費) |
| 2026-07-29 | v0.7.7 - 🐛 「永不過期」的密鑰仍被表單顯示成「首次使用後 1 天到期」(v0.7.6 聲稱修了但改動其實沒寫進檔案)。後端存的一直是正確的 `null`,是**表單在撒謊**:每次開啟都預填「1 天」、按鈕不高亮,一旦在這個顯示下儲存,假值就變成真值 |
| 2026-07-29 | v0.7.6 - 🐛 **API-KEY 額度限制此前形同虛設**:credits 用量被寫成「花費USD÷0.72」的反算值,真實 credits 在同一結構裡被丟掉。實測設了 2.00 credits 上限的 key 顯示 `0.00/2.00`、真實已用約 1.37,而**准入閘讀的是同一個假數**,設了上限也攔不住任何東西。共 5 處改用真值;單次在途預留從 1.389 改為 credits 原生的 1.0。另修:USD 用量把輸入 token 硬編碼為 0;「永不過期」的密鑰被編輯表單靜默改成「首次使用後 1 天到期」 |
| 2026-07-29 | v0.7.5 - 🐛 帳號頁「失敗」「限流」兩列張冠李戴:`failureCount` 裝的是 `strikes`(連擊數,一冷卻就清零),`throttleCount` 裝的是累計失敗數(與限流無關)。於是被上游**封禁**的帳號顯示成「限流 1、失敗 0」,把「帳號被停用需聯絡客服」錯報成「歇一會兒就好」。現在失敗=累計失敗數、限流=真實限流事件條數。另:33 個面板測試此前一次都沒在 CI 跑過,現已加入門禁 |
| 2026-07-29 | v0.7.4 - 🐛 「重置」與「手工啟停」現在立刻落盤。v0.7.3 把封禁結論做成持久的,但重置只改活池不寫盤:點完重置帳號確實回到可用池,**下次重啟又從盤上把封禁讀回來**。封禁帳號被擋在池外後永遠等不到一次成功來清標籤,重置是唯一出口,這個出口必須持久。手工啟停同理。另修:測試不再把帶假 token 的 `credentials.json` 寫進倉庫根目錄 |
| 2026-07-29 | v0.7.3 - 🐛 封禁結論現在跨重啟保留。v0.7.2 讓封禁帳號不再計入 `available`、不再被選中,但那個結論只活在記憶體裡:每次重啟/發版都會抹掉它,帳號悄悄回到可用池,直到再失敗一次才重新被擋。現在結論隨 `credentials.json` 落盤並在載入時還原;strike 與冷卻仍不落盤(那是計時器,重啟無非早重試一次) |
| 2026-07-29 | v0.7.2 - 🐛 修復被上游封禁的帳號仍被算作可用、仍會被選中:`available` 此前只看「未停用 && 不在冷卻」,不看 `statusReason`。冷卻是計時器到點自動回池,而封禁是上游結論(「帳號已鎖定,請聯絡客服驗證身分」)不隨時間解除,於是面板掛著「封禁」、可用數卻把它算在內,且冷卻一過就重新入選、必然再失敗、循環燒真實請求。現在封禁帳號不被選中、不計入 `available`、`healthStatus` 報 `unhealthy`;面板「重置」會一併清掉該結論 |
| 2026-07-28 | v0.7.1 - 🐛 修復 Responses 介面無法接入 codex:工具陣列裡的**內建工具**(`web_search`/`local_shell`/`file_search`,照 OpenAI 規範就沒有 `name`)此前會讓整輪請求死在反序列化(`tools[13]: missing field \`name\``),一個內建工具廢掉整個工作階段,且錯誤只報索引、看不出是哪類工具。現在內建工具可解析、被丟棄並落 WARN(`responses_builtin_tool_dropped`)。同時修掉緊隨其後的第二個坑:多輪回灌的 `reasoning`/`local_shell_call` 等項目此前判錯,會導致**第一輪能通、第二輪必炸**,現改為整條跳過;函式工具也允許省略 `parameters` |
| 2026-07-28 | v0.7.0 - 權杖刷新失敗此前被完全吞掉:日誌只有「刷新中」緊接「跨帳號重試」,中間**為什麼失敗**整個消失。線上真實事故:上游對整批帳號回 `access_denied`,面板上只表現為「帳號全過期了」。現在失敗即記錄上游狀態碼與回應體,並寫進 `statusReason`,新增「續期被拒」一檔——與「過期了刷一下就好」嚴格分開。帳號頁另加「全選本頁」與「批次停用」 |
| 2026-07-28 | v0.6.0 - 帳號列表每 30 秒靜默自動重新整理,並顯示新鮮度。此前頁面開啟即凍結:帳號被封、冷卻結束恢復、權杖過期,螢幕上都不會變,除非手動重新整理。照著一屏過時徽章做判斷比沒有徽章更糟——「封禁帳號 (0)」看著像結論,其實可能是十分鐘前的。只重拉便宜的列表介面,**絕不**按定時重跑餘額扇出。靜默重新整理保留頁碼、篩選、選取態與捲動位置;工具列顯示數字是幾秒前的 |
| 2026-07-28 | v0.5.1 - 健康徽章與新加的狀態篩選各算各的:篩選走 v0.5.0 的分檔,徽章仍只看 `healthStatus`,於是「過期帳號」那一檔裡的行照樣掛著綠色「健康」。現在兩者同源。額度耗盡此前也只認「被選中並失敗過一次」,帳號還沒輪到就已經沒額度的情況完全覆蓋不到 —— 現在餘額查詢回來的剩餘歸零同樣判為額度耗盡(與「還沒查過」嚴格區分),且每條餘額回來即刷新該行徽章與下拉條數 |
| 2026-07-28 | v0.5.0 - 帳號管理新增狀態篩選下拉(全部 / 健康 / 異常 / 停用 / 封禁 / 過期 / 額度耗盡,每檔帶即時條數),並把「異常」拆成維運真正要分別處置的幾檔。上游停用帳號時回應體帶 `suspend` 字樣,程式原本識別它,但只用來決定「別永久停用、讓它冷卻」,分類完就丟了。現經 `GET /api/admin/credentials` 的新欄位 `statusReason` 透出最近一次失敗的具體原因。封禁判定優先於限流;分類只進展示層,不改變選號紀律 |
| 2026-07-28 | v0.4.0 - 協議側 `/models` 現在列出全部 17 個可服務模型,三個協議結果一致。此前 `GET /v1/models`、`GET /claude/v1/models`、`GET /v1beta/models` 各自硬編碼**三條且互不相同**,而管理介面有 17 條——客戶端「先列模型再按 id 呼叫」拿到的只是殘缺子集,換個協議看到的還不一樣。現由唯一目錄 `src/models_catalog.rs` 支撐四個端點,並有測試保證目錄裡每個 id 都能被 `map_model` 識別、三協議逐項一致。另補齊 12 個從未寫進 API 參考的線上路由 |
| 2026-07-28 | v0.3.1 - `POST /v1/messages/count_tokens` 的畸形請求體回 axum 預設的純文字 `422` 而非 Anthropic 錯誤體。v0.3.0 把四個協議對話端點都改成了顯式接管拒收,唯獨漏了這個同屬 Anthropic 協議、同樣由 SDK 直接呼叫的端點——SDK 用 `response.json()` 讀純文字只會拋解析例外,真正的失敗原因被吞掉 |
| 2026-07-28 | v0.3.0 - 🔍 對 v0.2.1 自身修復的獨立複查。39 條確認項裡**有 9 條只關掉了一部分**卻被寫成已完成,另有 **13 條候選從未被裁決**(複核者中途崩潰),其中 12 條確屬真實缺陷,本版把這 21 處全部關掉。最要緊的一條:v0.2.1 宣稱「已真正生效」的 API-KEY 憑證綁定**從頭到尾沒有生效過**——鑑權閘把白名單解析出來塞進請求擴充,而下游沒有任何程式碼讀它,綁定到某個帳號的 key 照樣被分到池裡任意帳號,四協議皆然。另修:用戶端 IP 仍可偽造(`X-Forwarded-For` 取的是最左項,恰恰是呼叫方能寫死的那一項);`api_keys.json` 損壞時 `next_id` 仍會歸零,新建的 key 直接繼承前任的用量明細與累計消費;停機仍會丟掉餘額快取與事件日誌;上游錯誤體從未落庫,面板失敗詳情線上恆空;`temperature`、`max_tokens`、`tool_choice` 三個參數文件寫了但根本不生效,現已如實標註。v0.2.1 給綁定寫的迴歸測試斷言的是「值傳到了請求擴充」,而不是「選號照它執行」——這正是一個死功能能帶著全綠測試發版的原因;本輪每條修復的測試都先在修復前的程式碼上跑過、親眼看它失敗 |
| 2026-07-27 | v0.2.1 - 🛡 補充審計修復（對抗式複查確認 39 項問題，含此前從未受審的面板與文件）：安全面，帶機密的檔案（`api_keys.json`、`config.json`）以任何人可讀的權限落盤，且每次寫回都會靜默放寬回去、手動 `chmod` 也守不住；客戶端 IP 可被任何直連連接埠的人偽造；API-KEY 上綁定的憑證只儲存、從未生效。另修復：`GET /api/admin/models` 每次開儀表板都觸發無上限的整池上游掃描（現改為單次合流、限量並加冷卻）；憑證檔損毀時被當成空池並隨即覆寫、毀掉全部帳號（現改為先備份再逐條搶救）；API-KEY 變更在關閉時遺失；OpenAI 平行工具呼叫產生不合法的工具往返；部分 Gemini 酬載（內建工具、snake_case 鍵、非圖片 inlineData）被拒或被改壞；2 MB 請求體上限擋掉約 1.5 MB 的圖片；以及大量管理面板／使用者面板修復 |
| 2026-07-26 | v0.2.0 - 🔒 全鏈路審計修復：API-KEY 消費上限現在於 Anthropic / OpenAI / OpenAI-Responses / Gemini 四協議一律生效（此前只在 Anthropic 端點生效，改用其餘三協議即可無限消費，且這些流量的用量顯示為零）；只設定了使用者級 API-KEY 時管理面不再開放；上游錯誤、串流中途傳輸中斷與截斷不再被報成正常完成；帳號池刷新失敗會回饋到池；用量計費不再因重啟遺失、統計檔案保持可回滾；`--credentials` 與跟隨 `PORT` 的健康檢查現在真正生效 |
| 2026-07-26 | v0.1.4 - 🐛 修復 Anthropic `system` 欄位支援內容區塊陣列（不只字串）——Claude Code / 帶 prompt 快取的 SDK 把 system 發成陣列時不再回 422 |
| 2026-07-26 | v0.1.3 - 📥 批次 JSON 匯入改為即時逐條進度：進度條、即時累計成功/重複/失敗統計，以及逐條狀態清單（驗證中 → 已驗證並顯示用量 / 重複 / 失敗已回滾）；已驗證帳號即時落盤，匯入途中中斷也不會遺失 |
| 2026-07-25 | v0.1.2 - 🔄 檢查更新對話框改版：檢查更新對話框改為顯示當前介面語言的發行說明 + 可一鍵複製的升級指令；有更新時按鈕高亮為「更新到 vX」；修復純 HTTP 下複製按鈕失效 |
| 2026-07-25 | v0.1.1 - 🛠 面板與帳號匯入修復：模型測試在未建立自訂 key 時預設回退主 API 金鑰；批次匯入改為逐條「探活驗證 + 去重」；修復批次匯入在較大清單時失敗；使用者面板/全頁 favicon + 128×128 logo 與各語言 README 版本徽章；交叉編譯多架構映像建置 |
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
# {"service":"kiro2api","status":"ok","version":"0.10.0"}

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
