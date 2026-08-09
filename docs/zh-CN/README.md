<div align="center">

<img src="../logo.png" width="128" height="128" alt="kiro2api">

<h1>kiro2api</h1>
<h3>多协议 AI 中转 · Kiro 后端</h3>
<p>一套代码同时兼容 OpenAI / Anthropic / OpenAI-Responses / Gemini 四大 AI SDK，由 Kiro（CodeWhisperer）后端统一提供 Claude 系模型，纯异步 Rust 架构，Docker 快速部署。</p>

<p>
  <img src="https://img.shields.io/badge/Rust-2024-orange?style=flat-square&logo=rust&logoColor=white" alt="Rust">
  <img src="https://img.shields.io/badge/axum-0.8-000000?style=flat-square&logo=rust&logoColor=white" alt="axum">
  <img src="https://img.shields.io/badge/tokio-async-4E9A06?style=flat-square&logo=rust&logoColor=white" alt="tokio">
  <img src="https://img.shields.io/badge/Docker-20.10+-2496ED?style=flat-square&logo=docker&logoColor=white" alt="Docker">
  <img src="https://img.shields.io/badge/arch-amd64%20%7C%20arm64-4285F4?style=flat-square&logo=linux&logoColor=white" alt="Arch">
  <img src="https://img.shields.io/badge/License-MIT-green?style=flat-square" alt="License">
  <img src="https://img.shields.io/badge/version-v0.8.0-success?style=flat-square" alt="Version">
</p>

<p>
  <a href="#-最近更新">最近更新</a> &bull;
  <a href="#-核心功能">核心功能</a> &bull;
  <a href="#-系统要求">系统要求</a> &bull;
  <a href="#-快速部署">快速部署</a> &bull;
  <a href="#-接入示例">接入示例</a> &bull;
  <a href="#-api-端点">API 端点</a> &bull;
  <a href="#-配置说明">配置说明</a> &bull;
  <a href="#-注意事项">注意事项</a> &bull;
  <a href="#-开发路线">开发路线</a>
</p>

<p>
  📖 文档语言：简体中文 | <a href="../zh-TW/README.md">繁體中文</a> | <a href="../en/README.md">English</a> | <a href="../ja/README.md">日本語</a> | <a href="../ko/README.md">한국어</a>
</p>

<br>

<a href="https://github.com/xwteam/kiro2api/issues"><img src="https://img.shields.io/github/issues/xwteam/kiro2api?style=flat-square" alt="Issues"></a>
<a href="https://github.com/xwteam/kiro2api/stargazers"><img src="https://img.shields.io/github/stars/xwteam/kiro2api?style=flat-square" alt="Stars"></a>

</div>

---

> [!NOTE]
> 本项目仅供研究和学习用途，请合理使用，不要用于任何商业目的。

> [!WARNING]
> 本项目与 Amazon / AWS / Kiro 无任何关联或授权关系。项目把 Kiro（CodeWhisperer）后端封装为多协议兼容 API，可能不符合相关服务条款。使用风险自负，作者不对任何账号处罚或数据丢失承担责任。

> [!TIP]
> 后端为 Kiro（CodeWhisperer）账号池。**可用模型取决于账号订阅档位**：免费档（KIRO FREE）通常只授权 `claude-sonnet-4.5`，opus/GPT 等需更高档位，可获得更完整的模型访问权限。

> [!IMPORTANT]
> `apiKey`/`API_KEY` 为空时，协议端点**开放访问**（启动会告警），对外部署务必设置。管理接口 `/api/admin/*` 只有在配置了 `adminApiKey`（缺省回退 `apiKey`）之后才受保护——**两个 key 都不配时管理接口和面板一样是开放的**，任何人都能增删凭据、改鉴权密钥；`/admin`、`/user` 面板本体则始终不鉴权。部署到公网必须设置 `ADMIN_API_KEY`。容器镜像已内置 `HOST=0.0.0.0`；裸机部署请勿轻易把 `HOST` 改成 `0.0.0.0`。请求不支持的模型会明确返回 `400`（`INVALID_MODEL_ID`），而非静默失败。

---

## 📝 最近更新

> 完整更新日志请查看 [CHANGELOG.md](../../CHANGELOG.md)。

| 日期 | 更新内容 |
|------|----------|
| 2026-08-03 | v0.8.0 - ✨ **支持用 Kiro API Key(`ksk_…`)导入账号**。这类凭据 key 本身就是数据面 bearer,不换令牌、不刷新、不过期,完全不走 OAuth 链路。导入写 `{"kiroApiKey":"ksk_xxx","authMethod":"api_key"}`,或调接口传 `kiroApiKey`(认 `ksk` 别名);给了 key 就按 API Key 处理,不看 authMethod 写了什么。实现按观测对齐:带 `tokentype: API_KEY` 头、machineId 用 `KiroAPIKey/` 盐、刷新链路显式短路、过期判定恒答否(否则缺省 `expiresAt=0` 会让账号一进池就被判死)。另:声明 `api_key` 却没给 key 的凭据**入池即禁用**,且「重置」拒绝救活它——重置改不了配置,复活后只会重新走回同一条错误路径 |
| 2026-08-03 | v0.7.14 - 🐛 首页「全局剩余积分」经常空白、要点刷新才有:聚合只累加**仍新鲜**(5 分钟 TTL)的缓存,于是超过 5 分钟没打开账号页首页就是空的——盘上明明有全部账号余额,却因「不新鲜」整个不显示,逼你点刷新,**而那次刷新正是这份缓存本该避免的上游调用**。`is_fresh` 该答「要不要重查」而非「要不要显示」,现展示取全部条目、并带出数据年龄。✨ 新增**活跃账号令牌提前续期**:近 24h 用过的账号在到期前 10 分钟后台续上,请求不必等刷新;**刻意只续活跃账号**——全池定时续期等于给 253 个账号造一条永不停歇的心跳(每小时 253 次上游调用、无人使用),而本项目账号已被上游以 `security precaution` 封过 24 个 |
| 2026-08-03 | v0.7.13 - 🐛 **codex 的 502:中转自己造出了畸形请求**。上游要求消息里有 `toolUse` 时 `toolConfig` 必须存在,而内置工具(`web_search`/`local_shell`)在转换时被合法丢弃(v0.7.1),客户端某轮只带内置工具时 `tools` 就成了空数组、历史里的工具调用却还在 → 「有工具调用、没有工具定义」。复现:带工具历史+仅内置工具 → 502,同样历史带一个函数工具 → 200。现在发往上游前会**从对话历史把调用过的工具名补成最小规格**,客户端显式声明的优先、同名不重复。另:`TOOL_CONFIG_MISSING` 归入确定性请求错误(此前落在瞬时错误,半小时 26 次波及 25 个账号) |
| 2026-08-03 | v0.7.12 - 🐛 **一个超长请求能把整个账号池打伤、最终 503**。上游对超长请求回 `400 CONTENT_LENGTH_EXCEEDS_THRESHOLD`,而确定性请求错误此前只认 `INVALID_MODEL_ID`,这个码落进了「瞬时错误」→ 一个换任何账号都不可能成功的请求被**跨账号重试一遍,每换一个就给它记一次失败**。实测一个下午把 253 个健康账号打成 149 带伤、26 冷却,随后开始回 `503 no available upstream account`。现归入 `InvalidRequest`(不重试/不冷却/不累计 strike)。同时客户端不再收到毫无信息的 `502 upstream request failed`,改回 `400` 并明说是上下文超限、**该错误不会自愈**(每轮重发完整历史,下一轮只会更长)、需缩短上下文或新开会话 |
| 2026-08-01 | v0.7.11 - 🐛 测试套件把临时目录漏在 `/tmp` 里、从不回收:125 处测试各自拼路径(`temp_dir().join(...)`),没有 guard 也没有收尾,进程一退出就成孤儿。一次磁盘打满排查中,`/tmp` **顶层堆着 9582 个**这样的残留,而它们只积累了 4 天;systemd-tmpfiles 对 `/tmp` 的老化是 30 天,远追不上。代价不在体积(合计仅 52M)而在污染——顶层近万条目录项,恰恰在排查磁盘问题时最碍事。现全部收进单一 per-process 根目录 `/tmp/kiro2api-tests/<pid>/`,每个测试进程启动时回收 **pid 已不在 `/proc`** 的旧根,下一轮自动清掉上一轮。**只改测试基础设施,运行时行为完全不变** |
| 2026-07-29 | v0.7.10 - 🐛 发版后面板仍显示旧版本/旧行为(后端已 0.7.9,面板「检查更新」却显示 0.7.6):静态资源**一个缓存头都不发**,HTTP 下浏览器因此可**启发式缓存**、自行决定存多久。这类问题最难查——服务端三个接口(`/health`、`server-info`、`check-update`)当时全返回 0.7.9,错的只是浏览器手里那份副本;此前几次「改了却像没生效」大概率也有这个原因。现加 `Cache-Control: no-cache` + 内容 SHA-256 强 `ETag`,支持 `If-None-Match` → `304`:不陈旧,也不必每次全量重传 |
| 2026-07-29 | v0.7.9 - 🐛 「可用账号」把所有不健康的号都算了进去(封禁/额度耗尽/令牌过期/续期被拒):统计卡在前端复算 `!a.disabled`,而这几类**都不是「禁用」**,`disabled` 恒为 false。于是同一页上「封禁 6」与「可用 253」并列。现只数健康档;**刻意不用后端的 `available`**——后端那个数答的是「中转此刻会去尝试哪些账号」,额度耗尽/过期的号冷却一过仍在其中(它们确实该被再试),两者不是同一个问题。仪表盘同步改成同一口径,免得两个页面各显示一个「可用」 |
| 2026-07-29 | v0.7.8 - 🐛 额度只用了 0.08 就被 402 拦死(v0.7.6 回归):单次在途预留取 1.0 credits,而 v0.7.6 后「已花」终于是真值,于是 1 credit 的上限从第一发起就 `0.08 + 1.0 > 1.00`——面板显示还剩九成,请求一个都发不出去。预留改为贴近实测:credits 0.25(实测 ~0.137/次)、USD 0.05(实测 ~$0.0003/次)。中途试过「按上限比例封顶预留」被测试否掉:`SpendCache` 复用快照的前提是 est ≥ 单次真实花费,砍小就漏放超支 |
| 2026-07-29 | v0.7.7 - 🐛 「永不过期」的密钥仍被表单显示成「首次使用后 1 天到期」(v0.7.6 声称修了但改动其实没写进文件)。后端存的一直是正确的 `null`,是**表单在撒谎**:每次打开都预填「1 天」、按钮不高亮,一旦在这个显示下保存,假值就变成真值。附带记下 v0.7.6 失手的原因:`str.replace()` 没匹配上时静默跳过,而脚本无条件打印了「已改」——那句话不是证据 |
| 2026-07-29 | v0.7.6 - 🐛 **API-KEY 额度限制此前形同虚设**:credits 用量被写成「花费USD÷0.72」的反算值,而上游回报的真实 credits 就在同一结构里被丢掉。实测一把设了 2.00 credits 上限的 key 显示 `0.00/2.00`、真实已用约 1.37(七成)——而**准入闸读的是同一个假数**,所以设了上限也拦不住任何东西,且面板上看不出来。展示/准入闸/用户面板共 5 处改用真值;credits 的单次在途预留同步从 1.389(反算产物)改为 credits 原生的 1.0。另修:USD 用量把**输入 token 硬编码为 0**、少算了成本里通常更大的一半(现按 count_tokens 口径估算);「永不过期」的密钥被编辑表单静默改成「首次使用后 1 天到期」 |
| 2026-07-29 | v0.7.5 - 🐛 账号页「失败」「限流」两列张冠李戴:`failureCount` 装的是 `strikes`(连击数,一冷却就清零),`throttleCount` 装的是累计失败数(与限流无关)。于是被上游**封禁**的账号显示成「限流 1、失败 0」——两个数都在说假话,还把「账号被停用需联系客服」错报成「歇一会儿就好」。现在失败=累计失败数、限流=真实限流事件条数(一次遍历得出,不逐账号扫日志)。另:`admin-ui-v2/` 的 33 个面板测试**此前一次都没在 CI 跑过**,现已加入门禁 |
| 2026-07-29 | v0.7.4 - 🐛 「重置」与「手工启停」现在立刻落盘。v0.7.3 把封禁结论做成持久的,但重置只改活池不写盘:点完重置账号确实回到可用池,**下次重启又从盘上把封禁读回来**——运维明明操作过、状态却自己弹回去。封禁账号被挡在池外后永远等不到一次成功来清标签,重置是唯一出口,这个出口必须持久。手工启停同理(此前靠后续某次刷新顺带带下去,中间重启一次就没了)。另修:测试不再把带假 token 的 `credentials.json` 写进仓库根目录(会让别处「空池应回 503」的测试变 502) |
| 2026-07-29 | v0.7.3 - 🐛 封禁结论现在跨重启保留。v0.7.2 让封禁账号不再计入 `available`、不再被选中,但那个结论只活在内存里:每次重启/发版都会抹掉它,账号悄悄回到可用池,直到再失败一次才重新被挡——「253 个账号 1 个封禁、可用数却是 253」会在每次重启后重现,v0.7.2 只是把复现周期从「一次冷却」拉长到「一次重启」。现在结论随 `credentials.json` 落盘并在加载时还原;strike 与冷却仍不落盘(那是计时器,重启无非早重试一次) |
| 2026-07-29 | v0.7.2 - 🐛 修复被上游封禁的账号仍被算作可用、仍会被选中:`available` 此前只看「未禁用 && 不在冷却」,不看 `statusReason`。冷却是计时器到点自动回池,而封禁是上游结论(「账号已锁定,请联系客服验证身份」)不随时间解除,于是面板挂着「封禁」、可用数却把它算在内(253 个账号 1 个封禁,可用仍显示 253),且冷却一过就重新入选、必然再失败、循环烧真实请求。现在封禁账号不被选中、不计入 `available`、`healthStatus` 报 `unhealthy`;面板「重置」会一并清掉该结论(否则封禁号再无出口) |
| 2026-07-28 | v0.7.1 - 🐛 修复 Responses 接口无法接入 codex:工具数组里的**内置工具**(`web_search`/`local_shell`/`file_search`,照 OpenAI 规范就没有 `name`)此前会让整轮请求死在反序列化(`tools[13]: missing field \`name\``),一个内置工具废掉整个会话且错误只报下标。现在内置工具可解析、被丢弃并落 WARN(`responses_builtin_tool_dropped`)。同时修掉紧随其后的第二个坑:多轮回灌的 `reasoning`/`local_shell_call` 等条目此前判错,会导致**第一轮能通、第二轮必炸**,现改为整条跳过;函数工具也允许省略 `parameters` |
| 2026-07-28 | v0.7.0 - 令牌刷新失败此前被完全吞掉:日志只有「刷新中」紧接「跨账号重试」,中间**为什么失败**整个消失。线上真实事故:上游对整批账号回 `access_denied`,面板上只表现为「账号全过期了」,不手工 curl 上游根本分不清是账号被处置了还是中转坏了。现在失败即记录上游状态码与响应体,并写进 `statusReason`,新增「续期被拒」一档——与「过期了刷一下就好」严格分开,因为它刷多少次都没用。账号页另加「全选本页」与「批量禁用」 |
| 2026-07-28 | v0.6.0 - 账号列表每 30 秒静默自动刷新,并显示新鲜度。此前页面打开即冻结:账号被封、冷却结束恢复、令牌过期,屏幕上都不会变,除非手动刷新。照着一屏过时徽章做判断比没有徽章更糟——「封禁账号 (0)」看着像结论,其实可能是十分钟前的。只重拉便宜的列表接口,**绝不**按定时重跑余额扇出(那是逐个打上游的)。静默刷新保留页码、筛选、选中态与滚动位置;工具栏显示数字是几秒前的,刷新链路断了会转为警示色 |
| 2026-07-28 | v0.5.1 - 健康徽章与新加的状态筛选各算各的:筛选走 v0.5.0 的分档,徽章仍只看 `healthStatus`,于是「过期账号」那一档里的行照样挂着绿色「健康」。现在两者同源。额度耗尽此前也只认「被选中并失败过一次」,账号还没轮到就已经没额度的情况完全覆盖不到 —— 现在余额查询回来的剩余归零同样判为额度耗尽(与「还没查过」严格区分),且每条余额回来即刷新该行徽章与下拉条数 |
| 2026-07-28 | v0.5.0 - 账号管理新增状态筛选下拉(全部 / 健康 / 异常 / 禁用 / 封禁 / 过期 / 额度耗尽,每档带实时条数),并把「异常」拆成运维真正要分别处置的几档。上游停用账号时响应体带 `suspend` 字样,代码原本识别它,但只用来决定「别永久禁用、让它冷却」,分类完就丢了。现经 `GET /api/admin/credentials` 的新字段 `statusReason` 透出最近一次失败的具体原因。封禁判定优先于限流(上游停用响应常同时带限流措辞);分类只进展示层,不改变选号纪律——分错最多标签不准,绝不能让健康账号因措辞匹配被判死 |
| 2026-07-28 | v0.4.0 - 协议侧 `/models` 现在列出全部 17 个可服务模型,三个协议结果一致。此前 `GET /v1/models`、`GET /claude/v1/models`、`GET /v1beta/models` 各自硬编码**三条且互不相同**,而管理接口有 17 条——客户端「先列模型再按 id 调用」拿到的只是残缺子集,换个协议看到的还不一样。现由唯一目录 `src/models_catalog.rs` 支撑四个端点,并有测试保证目录里每个 id 都能被 `map_model` 识别、三协议逐项一致。另补齐 12 个从未写进 API 参考的线上路由 |
| 2026-07-28 | v0.3.1 - `POST /v1/messages/count_tokens` 的畸形请求体回 axum 默认的纯文本 `422` 而非 Anthropic 错误体。v0.3.0 把四个协议对话端点都改成了显式接管拒收,唯独漏了这个同属 Anthropic 协议、同样由 SDK 直接调用的端点——SDK 用 `response.json()` 读纯文本只会抛解析异常,真正的失败原因被吞掉 |
| 2026-07-28 | v0.3.0 - 🔍 对 v0.2.1 自身修复的独立复查。39 条确认项里**有 9 条只关掉了一部分**却被写成已完成,另有 **13 条候选从未被裁决**(复核者中途崩溃),其中 12 条确属真实缺陷,本版把这 21 处全部关掉。最要紧的一条:v0.2.1 宣称「已真正生效」的 API-KEY 凭据绑定**从头到尾没有生效过**——鉴权闸把白名单解析出来塞进请求扩展,而下游没有任何代码读它,绑定到某个账号的 key 照样被分到池里任意账号,四协议皆然。另修:客户端 IP 仍可伪造(`X-Forwarded-For` 取的是最左项,恰恰是调用方能写死的那一项);`api_keys.json` 损坏时 `next_id` 仍会归零,新建的 key 直接继承前任的用量明细与累计消费;停机仍会丢掉余额缓存与事件日志;上游错误体从未落库,面板失败详情线上恒空;`temperature`、`max_tokens`、`tool_choice` 三个参数文档写了但根本不生效,现已如实标注。v0.2.1 给绑定写的回归测试断言的是「值传到了请求扩展」,而不是「选号照它执行」——这正是一个死功能能带着全绿测试发版的原因;本轮每条修复的测试都先在修复前的代码上跑过、亲眼看它失败 |
| 2026-07-27 | v0.2.1 - 🔒 二轮审计修复（对抗式复核确认 39 项，含此前从未审计过的面板与文档）：密钥文件（`api_keys.json`、`config.json`）此前以全局可读权限落盘，手动 `chmod` 后每次写盘又被悄悄改回；客户端 IP 可被直连端口的任何人伪造；API-KEY 的凭据绑定只存不生效；`GET /api/admin/models` 每次打开面板都触发全量账号池上游扫描（改为单飞、限量并加冷却）；凭据文件损坏时被当成空池并覆写、导致账号全灭（改为先备份再逐条抢救）；API-KEY 变更在关停时丢失；OpenAI 并行工具调用产生非法的工具往返；部分 Gemini 请求体（内置工具、snake_case 字段、非图片 inlineData）被拒绝或改坏；2 MB 请求体上限会拒掉约 1.5 MB 的图片；另修复大量管理面板/用户面板问题 |
| 2026-07-26 | v0.2.0 - 🛡️ 全链路审计修复：API-KEY 消费上限四种协议全部生效（此前只在 Anthropic 端点生效，另外三种可无限消费且用量显示为零）；仅配置用户级 API-KEY 时管理端不再开放；上游报错、流中途传输中断与截断不再被当成正常完成上报；账号池刷新失败回写账号池；用量/账单重启不再丢失、账本文件回滚安全；`--credentials` 与跟随 `PORT` 的健康检查真正生效 |
| 2026-07-26 | v0.1.4 - 🐛 修复 Anthropic `system` 字段支持内容块数组（不只字符串）——Claude Code / 带 prompt 缓存的 SDK 把 system 发成数组时不再报 422 |
| 2026-07-26 | v0.1.3 - 📥 批量 JSON 导入改为实时逐条展示进度：进度条、成功/重复/失败实时计数、每行状态列表（验证中 → 已验证并附用量 / 重复 / 失败已回滚）；验证通过的账号即时保存，中途打断也不丢失 |
| 2026-07-25 | v0.1.2 - 🔔 检查更新弹窗改版：弹窗内展示当前语言的版本更新说明 + 可一键复制的升级命令；有新版本时按钮高亮为「更新到 vX」；修复纯 HTTP 下复制按钮失效 |
| 2026-07-25 | v0.1.1 - 🩹 面板与账号导入修复：模型测试在未创建自定义 key 时默认用主 API-KEY；批量导入改为逐条「验活 + 去重」；修复批量导入较大清单时失败；用户面板/全站 favicon + 128x128 logo 与各语言 README 版本徽章；交叉编译多架构镜像构建 |
| 2026-07-25 | v0.1.0 - 🚀 首个版本：四协议前端（Anthropic 中枢 + OpenAI / OpenAI-Responses / Gemini）、Kiro 账号池（多账号轮询 / 分级冷却 / 令牌自愈）、端点回退与跨账号重试、统一鉴权闸、`/admin` 管理面板与 `/user` 用户面板、每日/账号用量统计、失败/限流日志、账号余额缓存、实时日志（SSE）、三种交互式登录流、Docker 多架构（amd64/arm64）交付与 CI |

---

## 🌟 核心功能

> 📖 详细使用文档：[USAGE.md](USAGE.md)

### 🔌 四协议前端，一套后端

- 一个服务同时提供 **OpenAI Chat**、**Anthropic Messages**、**OpenAI Responses**、**Gemini 原生** 四种 SDK 格式
- 内部以 **Anthropic Messages 为中枢母格式**，其余协议双向转换后复用同一条中转内核
- 每个协议都支持**流式（SSE）**、**函数调用（工具）真透传**、**图片输入（多模态）**
- **双前缀挂载**：每协议同时挂标准裸前缀与显式厂商前缀（`/openai/v1`、`/claude/v1`、`/gemini/v1beta`），主流 SDK 填 `base_url` 即插即用

### 🔐 安全与认证

- 六条通道任选其一，按 `Authorization: Bearer` > `x-api-key` > `x-goog-api-key` > `?api_key=` > `?token=` > `?key=` 的优先级取第一条命中的；常量时间比较，失败即 `401`
- `adminApiKey`（缺省回退 `apiKey`）保护 `/api/admin/*`，两者都未配置时该闸为开放模式；持有者用自己的 **API-KEY** 访问 `/api/user/*`
- `/health`、`/v1/ping` 等探活端点不鉴权

### 🔄 账号池与令牌自愈

- **多账号轮询**：`priority`（等权轮询，默认）与 `balanced`（按 `weight` 加权）两种策略，可在管理面板运行期切换
- 每账号独立 RPM 限流、分级冷却；连续失败按类别（永久失效 / 歧义鉴权 / 配额 / 瞬时）差异化处置
- token 到期**自动内存刷新**（单飞协调，避免并发刷新级联 401），刷新成功原子落盘 `credentials.json`
- 支持 Builder ID 设备码 / IAM SSO 授权码 / 社交令牌三种登录流，凭据可 drop-in 现有 Kiro 数据

### 🔀 端点回退与跨账号重试

- Kiro IDE → CodeWhisperer → AmazonQ 多端点按序回退，`429`/网络错自动切换
- 账号级失败自动跨账号重试；确定性请求错误（如不支持的模型 `INVALID_MODEL_ID`）**不瞎重试、不误伤账号**，直接把上游原因回给客户端
- body-aware 失败分类：只有真正的凭据失效才永久禁用，配额/风控/限流一律冷却自愈

### 🖥 Web 管理面板

- 内置静态管理台（`/admin`），凭 `adminApiKey` 登录，`/api/admin/*` 富接口驱动
- **仪表盘**：运行时间实时计时、全局剩余积分、系统信息（版本/Rust/OS/内存/CPU/PID/运行模式）、赞助二维码卡（实时拉取远程配置）、**检查更新**（GitHub Release 比对，弹窗展示当前语言的版本更新说明 + 可一键复制的升级命令）
- **账号管理**：增删改查、三种交互式登录、批量导入（逐条验活 + 去重）、优先级/权重、余额查询
- **模型测试**：从面板向任意模型发一条测试请求验证连通性；未创建自定义 key 时默认用主 API-KEY
- **API-KEY 管理**：发放/禁用/改标签、消费上限与有效期（在四种协议前端统一计量并拦截）、按 key 用量与分页记录
- **用量统计**：每日/账号维度、含客户端 IP 与账号标签、按日下钻
- **实时日志**：结构化表格 + 方向过滤 + 搜索 + 分页 + SSE 实时推送 + 下载
- **设置**：运行期切负载均衡/鉴权密钥、集成示例（协议×语言可复制片段）、**一键重启服务**
- 顶部控制栏：运行状态徽章、GitHub、重启、深浅色主题、5 语言切换

### 👤 用户面板

- 内置用户台（`/user`），持有者用自己的 **API-KEY** 登录（无需 admin 权限）
- 查看该 key 的额度、累计用量与分页记录，由 `/api/user/*` 驱动

### 🧭 模型名映射

- 客户端传入的模型名按**小写子串**匹配到 Kiro 内部模型（未匹配到 → `400`）
- 协议侧 `/models` 端点返回的是**固定短清单**，不读账号池、也不按订阅档位过滤；完整目录见管理接口 `GET /api/admin/models`。档位未授权的模型即使出现在清单里，请求仍会 `400`（`INVALID_MODEL_ID`）

### ⚡ 高性能架构

- 基于 **Rust + axum 0.8 + tokio**，全链路异步非阻塞
- AWS eventstream 帧解码、账号池串行占锁最小临界区、网络发出即释放
- 强类型 serde 校验，每种协议独立适配器模块
- 多阶段 Docker 构建、非 root 运行（gosu）、多架构镜像、健康检查

---

## 📋 系统要求

| 依赖 | 版本 | 说明 |
|------|------|------|
| Rust | 2024 edition | 仅从源码构建时需要；Docker 部署无需本地安装 |
| Docker | 20.10+ | 推荐使用 Docker 部署 |
| Kiro 账号 | — | 需有效的 Kiro（CodeWhisperer）凭据（Builder ID / IdC / 社交登录） |
| 架构 | amd64 / arm64 | 官方镜像多架构，二选一自动匹配 |

> [!TIP]
> 使用 Docker 部署无需本地安装 Rust 环境，只需 Docker 和有效的 Kiro 凭据即可。

---

## ⚡ 快速部署

> 📖 详细部署文档：[DEPLOY.md](DEPLOY.md)

> **前置条件**：你需要一份有效的 Kiro（CodeWhisperer）账号凭据。

### 1. 获取 Kiro 凭据

从你的 Kiro 客户端 / 已有 Kiro 凭据中导出以下字段，或使用管理面板的三种交互式登录（Builder ID 设备码 / IAM SSO 授权码 / 社交令牌）现场获取：

| 字段 | 说明 |
|------|------|
| `accessToken` / `refreshToken` | 访问令牌与刷新令牌（到期自动刷新） |
| `expiresAt` | 令牌过期时间（RFC3339） |
| `authMethod` | `social`（带 `profileArn`）或 `idc`（带 `clientId`/`clientSecret`） |
| `machineId` | 机器标识（社交登录凭据附带） |

> [!TIP]
> 三种登录流均可在管理面板「账号管理」页交互式完成，无需手工拼装凭据。

### 2. Docker 部署

```bash
# 克隆仓库
git clone https://github.com/xwteam/kiro2api.git
cd kiro2api

# 创建环境变量文件
cp .env.example .env
```

编辑 `.env`，至少填一个对外调用密钥 `API_KEY`：

```env
API_KEY=sk-你的对外调用密钥
# 管理端独立密钥；公网部署必填（不设则 /api/admin/* 回退用 API_KEY 鉴权，两者都不设即开放）。
# 不需要就把整行注释掉——写成空值会覆盖 config.json 里已配的密钥。
ADMIN_API_KEY=sk-你的管理端密钥
```

> [!IMPORTANT]
> 注意事项：
> - `API_KEY` 留空时协议端点开放访问（启动会告警），对外部署务必设置
> - `ADMIN_API_KEY` 与 `API_KEY` 都留空时 `/api/admin/*` 也是开放的，公网部署必须设置
> - 值不需要加引号，不要有多余的空格或换行
> - 裸机部署请勿轻易把 `HOST` 改成 `0.0.0.0`（面板本体尚未接鉴权）

把 Kiro 账号凭据放到 `data/credentials.json`（数组，可直接 drop-in 现有 Kiro 凭据）：

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

启动服务：

```bash
mkdir -p data
docker compose up -d
```

查看日志确认启动成功：

```bash
docker compose logs -f
# 看到账号池就绪、监听端口即表示启动成功
```

### 多账号配置（可选）

`credentials.json` 是一个数组，追加多个对象即启用多账号轮询。每账号可带 `weight`（配合 `balanced` 策略加权）与标签；负载均衡策略由 `LOAD_BALANCING_MODE`（`priority` / `balanced`）控制，也可在管理面板运行期切换。

> [!TIP]
> 单账号即可跑通；追加账号后自动进入轮询 + 分级冷却，单账号被限流/配额耗尽时自动切到下一个可用账号。

### 令牌自愈

kiro2api 内置令牌自愈机制：token 到期**自动内存刷新**（单飞协调，避免并发刷新级联 401），刷新成功原子落盘 `credentials.json`，无需重启服务。只有真正的凭据失效才永久禁用，配额/风控/限流一律冷却自愈。

> [!NOTE]
> 上游端点按 Kiro IDE → CodeWhisperer → AmazonQ 顺序回退，`429`/网络错自动切换；确定性请求错误（如不支持的模型）不重试、不误伤账号。

### 3. 验证

```bash
# 健康检查
curl http://localhost:8080/health
# {"service":"kiro2api","status":"ok","version":"0.8.0"}

# 查看协议侧模型清单（固定短清单，不代表账号档位真的授权）
curl http://localhost:8080/v1/models \
  -H "Authorization: Bearer sk-你的API密钥"

# 发送测试请求
curl -X POST http://localhost:8080/v1/chat/completions \
  -H "Content-Type: application/json" \
  -H "Authorization: Bearer sk-你的API密钥" \
  -d '{"model":"claude-sonnet-4.5","messages":[{"role":"user","content":"你好"}]}'
```

看到 AI 回复的文字即部署成功。如果返回 401，请检查 API Key 是否正确。

---

## 🧪 接入示例

> [!NOTE]
> 所有 API 请求都需要携带 API Key。鉴权闸接受六条通道，按 `Authorization: Bearer` > `x-api-key` > `x-goog-api-key` > `?api_key=` > `?token=` > `?key=` 的优先级取第一条命中的：
> - `Authorization: Bearer sk-xxx`（推荐，兼容 OpenAI/Anthropic SDK）
> - `x-api-key: sk-xxx`
> - `x-goog-api-key: sk-xxx`（Gemini 官方 SDK 默认走它）
> - URL query：`?api_key=sk-xxx`、`?token=sk-xxx` 或 `?key=sk-xxx`（无法设请求头的场景，如浏览器 `EventSource`）
>
> base URL 用**标准裸前缀**：OpenAI = `{host}/v1`，Anthropic = `{host}`（SDK 自动补 `/v1/messages`），Gemini = `{host}/v1beta`。也可用显式厂商前缀 `/openai/v1`、`/claude/v1`、`/gemini/v1beta`。

<details>
<summary><b>OpenAI SDK（Python）</b></summary>

```python
from openai import OpenAI

client = OpenAI(
    base_url="http://localhost:8080/v1",
    api_key="sk-你的API密钥",
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
    api_key="sk-你的API密钥",
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
    api_key="sk-你的API密钥",
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
# 非流式请求
curl -X POST http://localhost:8080/v1/chat/completions \
  -H "Content-Type: application/json" \
  -H "Authorization: Bearer sk-你的API密钥" \
  -d '{"model":"claude-sonnet-4.5","messages":[{"role":"user","content":"Hi"}]}'

# 流式请求
curl -X POST http://localhost:8080/v1/chat/completions \
  -H "Content-Type: application/json" \
  -H "Authorization: Bearer sk-你的API密钥" \
  -d '{"model":"claude-sonnet-4.5","messages":[{"role":"user","content":"Hi"}],"stream":true}'
```

</details>

<details>
<summary><b>函数调用（工具）</b></summary>

```python
resp = client.chat.completions.create(
    model="claude-sonnet-4.5",
    messages=[{"role": "user", "content": "北京今天天气怎么样"}],
    tools=[{
        "type": "function",
        "function": {
            "name": "get_weather",
            "description": "获取指定城市的天气",
            "parameters": {
                "type": "object",
                "properties": {"city": {"type": "string"}},
                "required": ["city"]
            }
        }
    }]
)
```

> 工具调用在四种协议间**真透传**（Anthropic `tool_use` / OpenAI `tool_calls` / Gemini `functionCall`），不做模拟。

</details>

---

## 📡 API 端点

> 📖 详细 API 文档：[API.md](API.md)

> **双前缀并存**：每协议同时提供「标准裸路径」和「显式厂商前缀路径」。裸路径让官方 SDK 填 `base_url` 时无需加后缀，开箱即用；厂商前缀用于四家明确区分。

### OpenAI 兼容（`/v1` 或 `/openai/v1`）

| 方法 | 端点 | 功能 |
|------|------|------|
| GET | `/models` | 模型列表（固定短清单，非账号池实际可服务集） |
| POST | `/chat/completions` | 对话补全（流式返回 `chat.completion.chunk` + `[DONE]`，含工具/图片） |

### OpenAI Responses（`/v1/responses` 或 `/openai/v1/responses`）

| 方法 | 端点 | 功能 |
|------|------|------|
| POST | `/responses` | Responses API（流式为命名事件 + 单调 `sequence_number`，无 `[DONE]`；`previous_response_id` 返回 400） |

### Anthropic 兼容（`/v1` 对话入口；`/claude/v1` 显式前缀）

| 方法 | 端点 | 功能 |
|------|------|------|
| POST | `/v1/messages` | Messages（流式/工具/图片） |
| POST | `/v1/messages/count_tokens` | token 估算 |
| GET | `/claude/v1/models` | 模型列表（Anthropic 形状，避开与 OpenAI `/v1/models` 冲突） |
| POST | `/claude/v1/messages` · `.../count_tokens` | 显式前缀变体 |

### Gemini 原生（`/v1beta` 或 `/gemini/v1beta`）

| 方法 | 端点 | 功能 |
|------|------|------|
| GET | `/models` | 模型列表 |
| POST | `/models/{m}:generateContent` | 内容生成（非流式） |
| POST | `/models/{m}:streamGenerateContent` | 流式生成（`?alt=sse`，camelCase） |

### 管理 / 用户 / 运维

| 方法 | 端点 | 功能 |
|------|------|------|
| GET | `/admin` · `/api/admin/*` | 管理面板 + 管理接口（凭 `adminApiKey`，未配置任何 key 时开放：凭据 CRUD / 登录 / API-KEY / 用量 / 日志 / 余额 / 设置 / 检查更新 / 重启） |
| GET | `/user` · `/api/user/*` | 用户面板 + 接口（凭自身 API-KEY） |
| GET | `/health` · `/v1/ping` | 探活（不鉴权） |

> URL 里的 `localhost:8080` 只是示例；端口由 `PORT`/`config.json` 配置，按你的部署替换。
>
> 密钥可走鉴权闸接受的任意通道，优先级为 `Authorization: Bearer` > `x-api-key` > `x-goog-api-key` > query（`?api_key=` > `?token=` > `?key=`）。Gemini 原生的 `x-goog-api-key` 与 `?key=` 同样被接受，官方 `google-genai` SDK 只换 `base_url` 就能用；要换的是**值**——一律传本服务的 API-KEY，不是真的 Google/OpenAI 厂商密钥。

---

## ⚙ 配置说明

优先级：**命令行参数 > 环境变量 > `config.json` > 内置默认**。命令行只有两个参数：`-c/--config`（配置文件路径）与 `--credentials`（凭据文件路径，不给则由 `CREDENTIALS_PATH`/`config.json`/默认值决定）。挂载卷 `./data` 存放 `config.json`、`credentials.json`、日志与运行态。

> 凭据路径同时决定用量统计（`stats/`）、API-KEY 存储（`api_keys.json`）与余额缓存的落盘目录——它们都取 `credentials.json` 的父目录。内置默认值解析在 `-c` 指定的配置文件所在目录下，容器以 `-c /app/data/config.json` 启动，因此默认落点就是 `/app/data/credentials.json`，这些数据默认就落在挂载卷里；自定义路径时请一并指向挂载卷，否则容器重建即丢。

**环境变量**（见 `.env.example`）：

| 变量 | 必填 | 默认值 | 说明 |
|------|------|--------|------|
| `API_KEY` | ✅ | — | 对外调用密钥（留空则协议端点开放访问，启动告警） |
| `ADMIN_API_KEY` | ❌ | 回退 `API_KEY` | 管理端独立鉴权 key；与 `API_KEY` 都不设时 `/api/admin/*` 开放，公网部署必填 |
| `HOST` | ❌ | `127.0.0.1`（镜像内置 `0.0.0.0`） | 监听地址 |
| `PORT` | ❌ | `8080` | 服务端口（compose 的端口映射与健康检查都跟随该值） |
| `REGION` | ❌ | `us-east-1` | 默认 AWS region（账号 `profileArn` 内的 region 优先） |
| `LOAD_BALANCING_MODE` | ❌ | `priority` | 负载均衡：`priority`（等权轮询）/ `balanced`（按 weight 加权） |
| `MAX_RPM_PER_CREDENTIAL` | ❌ | `0` | 每账号每分钟请求上限，`0` = 无限 |
| `CREDENTIALS_PATH` | ❌ | `credentials.json`，解析在 `-c` 配置文件所在目录（容器内即 `/app/data/credentials.json`） | 凭据文件路径；被命令行 `--credentials` 覆盖 |

**`data/config.json`**（camelCase，均可选；`logCapacity` 仅在此配置）：

```json
{
  "host": "0.0.0.0",
  "port": 8080,
  "region": "us-east-1",
  "apiKey": "sk-你的对外调用密钥",
  "adminApiKey": "可选,管理端",
  "credentialsPath": "/app/data/credentials.json",
  "loadBalancingMode": "priority",
  "maxRpmPerCredential": 0,
  "logCapacity": 5000,
  "kiroVersion": "0.11.107",
  "systemVersion": "win32#10.0.22631",
  "nodeVersion": "22.22.0"
}
```

- `logCapacity`：实时日志环形缓冲条数，`>0` 启用日志捕获（管理面板日志页回放/SSE），`0` 关闭（日志端点返回 503）；默认 `5000`。
- `kiroVersion`/`systemVersion`/`nodeVersion`：伪装 UA 版本号，从配置注入。

---

## ⚠ 注意事项

1. **对外部署务必设置 `API_KEY` 与 `ADMIN_API_KEY`**：`API_KEY` 留空时协议端点开放访问（启动会告警）；`adminApiKey`/`apiKey` 都不配时 `/api/admin/*` 同样开放，凭据、API-KEY、鉴权设置都能被任意改写。`/admin`、`/user` 面板本体始终不鉴权（真正的闸在其 `/api/**` 接口上）；裸机部署慎改 `HOST=0.0.0.0`。

2. **可用模型取决于账号订阅档位**：免费档（KIRO FREE）通常只授权 `claude-sonnet-4.5`；请求不支持的模型返回 `400`（`INVALID_MODEL_ID`），不瞎重试、不误伤账号。

3. **令牌自愈**：token 到期自动内存刷新并原子落盘 `credentials.json`；真正的凭据失效才永久禁用，配额/风控/限流一律冷却自愈。

4. **流式输出**：四种协议均支持流式；`stream:false` 时服务内部仍解码事件流，收集完毕后一次性返回完整 JSON。上游报错或流传输中途中断时，流会以该协议自身的错误事件收尾，绝不会伪装成一次正常完成；**上游**命中自身输出预算或上下文耗尽时，如实回报截断原因。注意 `max_tokens` / `tool_choice` / `temperature` 等生成参数**接受但不生效**（上游线格式无对应字段，故意不转发），详见 [API.md](API.md)。

5. **网络环境**：部署服务器需能访问 AWS CodeWhisperer/Kiro 端点（`*.amazonaws.com`）。

---

## 🗺 开发路线

- [x] 四协议前端（OpenAI / Anthropic / OpenAI-Responses / Gemini）
- [x] Anthropic Messages 中枢母格式 + 统一中转内核
- [x] 流式（SSE）+ 函数调用真透传 + 图片多模态
- [x] Kiro 账号池（多账号轮询、分级冷却、负载均衡）
- [x] 令牌单飞自动刷新 + 原子落盘
- [x] 端点回退（Kiro/CodeWhisperer/AmazonQ）+ 跨账号重试
- [x] body-aware 失败分类（永久失效才禁用，其余冷却自愈）
- [x] 统一鉴权闸（Bearer / x-api-key / x-goog-api-key / `?api_key=` / `?token=` / `?key=`）
- [x] Web 管理面板（凭据/登录/API-KEY/用量/日志/余额/设置）
- [x] 用户面板（持有者用自身 API-KEY 登录）
- [x] 三种交互式登录流（Builder ID / IAM SSO / 社交令牌）
- [x] 每日/账号用量统计（含客户端 IP 与账号标签）
- [x] 实时日志（SSE）+ 余额缓存 + 动态模型清单
- [x] 集成示例（协议×语言可复制片段）
- [x] 服务重启 + 版本检查更新（GitHub Release 比对）
- [x] Docker 多架构（amd64/arm64）交付 + CI
- [ ] `/admin`、`/user` 面板本体鉴权
- [ ] GitHub Actions 自动构建并发布镜像

---

## ☕ 赞赏 & 共享

觉得有帮助？请作者喝杯咖啡，或加入微信交流群获取使用帮助。二维码见管理面板仪表盘。完整内容请查看 [SPONSORS.md](SPONSORS.md)。

欢迎 PR 和 Issue。

1. Fork 本仓库
2. 创建分支 `git checkout -b feature/your-feature`
3. 提交代码 `git commit -m "feat: add something"`
4. 推送并创建 Pull Request

---

## 🙏 致谢

感谢所有在 [Issues](https://github.com/xwteam/kiro2api/issues) 里提交 bug 复现、日志、兼容性反馈和功能建议的用户。这些反馈直接推动了账号池、令牌自愈、端点回退、多协议兼容、Web 面板等核心能力的迭代。

---

## 📄 许可协议

本项目采用 [MIT 许可](../../LICENSE)：

- **允许**：个人学习、研究、自用部署、二次开发
- **要求**：保留版权与许可声明

本项目与 Amazon / AWS / Kiro 无关联。使用者需自行承担风险并遵守相关服务条款。

---

<div align="center">
  <sub>Built with Rust + axum + tokio | Powered by Kiro (CodeWhisperer)</sub>
</div>
