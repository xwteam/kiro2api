<div align="center">

<img src="../logo.png" width="128" height="128" alt="kiro2api">

<h1>kiro2api</h1>
<h3>Multi-Protocol AI Relay · Kiro Backend</h3>
<p>Single codebase compatible with OpenAI / Anthropic / OpenAI-Responses / Gemini SDKs, backed by a Kiro (CodeWhisperer) account pool that serves Claude-family models, pure async Rust architecture, Docker quick deployment.</p>

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
  <a href="#-recent-updates">Recent Updates</a> &bull;
  <a href="#-core-features">Core Features</a> &bull;
  <a href="#-system-requirements">System Requirements</a> &bull;
  <a href="#-quick-deployment">Quick Deployment</a> &bull;
  <a href="#-integration-examples">Integration Examples</a> &bull;
  <a href="#-api-endpoints">API Endpoints</a> &bull;
  <a href="#-configuration">Configuration</a> &bull;
  <a href="#-important-notes">Important Notes</a> &bull;
  <a href="#-roadmap">Roadmap</a>
</p>

<p>
  📖 Documentation: <a href="../zh-CN/README.md">简体中文</a> | <a href="../zh-TW/README.md">繁體中文</a> | English | <a href="../ja/README.md">日本語</a> | <a href="../ko/README.md">한국어</a>
</p>

<br>

<a href="https://github.com/xwteam/kiro2api/issues"><img src="https://img.shields.io/github/issues/xwteam/kiro2api?style=flat-square" alt="Issues"></a>
<a href="https://github.com/xwteam/kiro2api/stargazers"><img src="https://img.shields.io/github/stars/xwteam/kiro2api?style=flat-square" alt="Stars"></a>

</div>

---

> [!NOTE]
> This project is for research and learning purposes only. Please use it responsibly and do not use it for any commercial purposes.

> [!IMPORTANT]
> When `apiKey`/`API_KEY` is empty, the protocol endpoints are **openly accessible** (startup logs a warning). Always set it for external deployments. The admin API `/api/admin/*` is only protected once `adminApiKey` (falling back to `apiKey`) has been configured — **with neither key set, the admin API is as open as the panels are**, and anyone can add or delete credentials and rotate the auth keys; the `/admin` and `/user` panels themselves are never gated. Always set `ADMIN_API_KEY` before putting the service on the public internet. The container image ships with `HOST=0.0.0.0` built in; for bare-metal deployments do not casually change `HOST` to `0.0.0.0`.

> [!TIP]
> The backend is a Kiro (CodeWhisperer) account pool. **Which models are available depends on the account's subscription tier**: the free tier (KIRO FREE) usually authorizes only `claude-sonnet-4.5`, while opus/GPT and the like require a higher tier — requesting an unsupported model returns a clear `400` (`INVALID_MODEL_ID`) rather than failing silently.

---

## 📝 Recent Updates

> The table below lists only the **10 most recent** updates; for the complete changelog see [CHANGELOG.md](../../CHANGELOG.md).

| Date | Update |
|------|--------|
| 2026-08-10 | v0.15.0 - 🔁 **Quota-exhausted accounts no longer have to be relearned after every restart.** The user asked directly: "aren't exhausted accounts already disabled — why do requests still reach them?" They were disabled, but **only in memory**: v0.10.2 deliberately kept runtime verdicts off disk so one permission blip could not write an account off permanently — but quota is precisely the kind that has a **known recovery time**. So every deploy wiped the knowledge and the pool rediscovered it using **the user's requests** (measured: 13 exhausted accounts meant two 502s before the third request succeeded). It is now persisted with its reset time, survives restarts and returns to the pool automatically; the time comes from upstream's `nextResetAt` when available, falling back to the first of next month. Also: **the server-side `web_search` tool actually works now** — it used to be merely tolerated (no longer a 400 since v0.11.0) while never searching, so the model answered with ordinary text and the client believed a search had happened. Such requests are now intercepted before the data plane, sent to the upstream `/mcp` endpoint, and returned as `server_tool_use` + `web_search_tool_result` |
| 2026-08-10 | v0.14.1 - 🔧 **Two things found while verifying the previous releases in production.** ① `usage.input_tokens` was **always 0** in responses: the estimate added in v0.13.0 only fed billing and was never written back, so the number existed in the invoice but not in the reply the client uses to compute cost and context usage — both the non-streaming `usage` and the streaming `message_start` now carry it. ② **With a batch of quota-exhausted accounts in the pool, a user's first few requests failed in a row**: quota exhaustion shared the 3-attempt budget with auth failures, and with 13 exhausted accounts two requests returned 502 before the third succeeded (each request burned three accounts before disabling them). Quota exhaustion is **deterministic** for the billing period, so it now shares the account-level deterministic class with "model unavailable" and gets an allowance sized to the pool, while transient and auth failures keep their small cap of 3. When the pool really is exhausted the answer is a `429` that says so, instead of an unhelpful 502 |
| 2026-08-10 | v0.14.0 - 🧩 **Closing the comparison out.** ① An assistant turn in the history could carry an **empty** `content`, which upstream rejects the whole request over — a turn that only called tools carries no text, and while user turns had long had a non-empty fallback, assistant turns never did, so replaying any conversation whose previous turn was a pure tool call failed reliably (the most common shape in a tool chain). A single space is now used as the placeholder. ② **Corrupt frames were rescanned byte by byte even though their boundary was known**: once the prelude CRC passes, `total_len` is trustworthy, but every error kind used to fall back to byte-wise resync, so a message-CRC failure had the decoder rescan the frame's entire payload as noise — slow, and liable to assemble a plausible-looking prelude out of payload bytes and emit a message that never existed. Frames with a known boundary are now skipped whole. ③ **`tlsBackend` is now switchable at runtime** instead of being a compile-time choice that required a new image: native-tls uses the system trust store while rustls carries its own roots, and behind a self-signed-CA proxy typically only one of them completes the handshake — presenting as "cannot refresh tokens" or "cannot connect", with nothing on the surface pointing at TLS |
| 2026-08-09 | v0.13.0 - 🧠 **Extended thinking is now fully wired** — the feature was missing entirely. On the request side the `thinking` field was silently dropped, so upstream never received the directive; on the response side upstream wraps its reasoning in `<thinking>…</thinking>` inside ordinary text, which we passed through verbatim, so clients rendered the whole reasoning as the answer. Directives are now generated for `enabled` / `adaptive` and injected at the front of the system prompt, and the reasoning is split into proper `thinking` blocks (`thinking_delta` when streaming), with streaming and non-streaming sharing one incremental splitter. Ordinary text passes through with **zero added latency** — only the tail from the last `<` is held back, not a blanket tag-length buffer, which would make every downstream chunk lag by nearly ten bytes (a regression caught by a test during implementation). Also fixed: **token estimation under-counted Chinese roughly threefold** (a global chars/4 became script-weighted; this feeds usage stats and USD limits); **streaming accounting reported 0 input tokens** while the non-streaming path had long estimated them; and the **context window was pinned at 200K for every model** — upstream `maxInputTokens` was parsed and then thrown away, so a 1M-context model was under-reported fivefold |
| 2026-08-09 | v0.12.0 - 🎚️ **Accounts on different subscription tiers can finally coexist** — reproduced and verified against a real API-key account the user lent for the investigation. Two half-causes: (1) `INVALID_MODEL_ID` was classed as a *request-level* error and returned 400 immediately, when it is really *account-level*: the available model set depends on the tier, while the model list this relay exposes is the **union** across all accounts, so a model from that union landing on an account without it always fails. It is now `ModelUnavailable` — no penalty to the account, but try another one. (2) The cross-account budget was 3 attempts while the account that supports the model may sit 14th in the pool, so model-unavailable no longer consumes the account-failure budget. Also: the pool now remembers which account lacks which model (a second request for the same model selected 1 account, versus 14 for the first); **`priority` had been a mere alias for `weight` and never affected selection** — lower numbers now win and imports default to 999; and refreshing models always failed outside us-east-1 (`codewhisperer.{region}` does not resolve there), which now falls back to `q.{region}` |
| 2026-08-09 | v0.11.1 - 🔬 **Two things production testing turned up.** ① **An empty tool description makes upstream reject the whole request** — measured: the same tool with a description returns 200 and a proper `tool_use`; drop the description and it is `400 {"message":"Invalid tool use format.","reason":"REQUEST_BODY_INVALID"}`. v0.11.0 changed `null` to an empty string, but upstream wants a **non-empty** one — the tool name is now used as a fallback. ② `REQUEST_BODY_INVALID` was treated as retryable. It is deterministic — every account fails the same way — so one malformed request burned several accounts' retry budget (four in one measured run) and still returned an unhelpful 502. It is now in the no-retry, no-penalty class and returns a 400 that points at the tool specification |
| 2026-08-09 | v0.11.0 - 🧰 **Fixes for the class of bugs that made upstream reject the request.** ① Anthropic's **server-side tools** (`web_search` and friends) carry no `input_schema`, yet that field was mandatory — so using the officially supported shape got the request rejected at our own layer with a 400. ② A tool's `description` could serialize to **null**; the real client always sends a string there. ③ `input_schema` was passed through verbatim, so a shape like `properties: null` had upstream reject the **entire request** — schemas are now normalized (shape only, semantics untouched). ④ Over-long tool names were neither shortened (upstream caps at 63) nor mappable back — they are now shortened deterministically and the short→original map is carried to both exits, so the client never sees a tool it did not declare. ⑤ Frames with `:message-type == "error"` were ignored entirely, turning an upstream error into a 200 with an empty message. ⑥ Missing panel assets returned 200 + HTML instead of 404 — **which made "curl the file to check what's deployed" lie to you**. Also adds `POST /api/admin/credentials/{id}/refresh` |
| 2026-08-09 | v0.10.2 - 🩹 **Fix: a runtime stop was being written to disk, so "a restart revives it" was hollow.** v0.10.0 made quota-exhausted and repeatedly-rejected accounts stop being used, explicitly in memory only — but `snapshot_credentials()` overwrote `cred.disabled` with the runtime flag when persisting, so one quota exhaustion or two 401/403s wrote the account off **permanently** in `credentials.json`, unrecoverable by restart — worse than the behaviour it replaced (one production account was lost this way). Persistence now takes only the durable verdict; the two paths that should persist (operator disable, body-confirmed invalidation) already set `cred.disabled` themselves. **If your `credentials.json` has a `"disabled": true` you never set, flip it back to false to recover the account** |
| 2026-08-09 | v0.10.1 - 🔌 **Per-account outbound proxy, plus the session identity fields that were missing.** ① The three proxy fields were **accepted and then dropped**, and `hasProxy` was hardcoded to `false` — the panel said it was configured while every request went out direct. They are now persisted and effective, with precedence credential > global > direct (`"direct"` forces a direct connection), and **one account's data plane, token refresh, balance, model list and background renewal all share a single exit** (a data plane behind a proxy while refresh comes from the main IP is worse than no proxy at all). ② `conversationId` was regenerated per request and was not UUID-shaped → it now prefers the session UUID carried in the client's `metadata.user_id`, so one session shares one id. ③ `agentContinuationId` **was never sent**; it is now. ④ Credentials added through the panel never got a frozen machineId (v0.10.0 only covered those already in the file) → now frozen the moment they enter the pool. ⑤ `isCurrent` was hardcoded `false`; it now reports the truth |
| 2026-08-09 | v0.10.0 - 🎯 **Aligned behavioural shape with the real client.** A module-by-module comparison against a long-stable peer implementation overturned the previous two releases' diagnosis: that implementation reuses connections, does not pin HTTP/1.1, and defaults to rustls — none of the three things we had bet on. The real differences: ① `priority` used to **rotate accounts on every request**, so one exit IP showed hundreds of machineIds interleaving by the second → now it **sticks to one account until that account becomes unusable**; ② suspended / quota-exhausted accounts used to **return to the pool** after a 5- or 30-minute cooldown, i.e. hammered a wall forever → now they stop being used (in-memory only; a reset revives them); ③ **token refresh requests carried no User-Agent at all** (measured on the wire), on Kiro's own endpoint, on a path every account must take → now filled in per the two real shapes (axios and sso-oidc); ④ **machineId changed on every refresh** (derived from the rotating refreshToken) → now frozen and persisted at load; ⑤ ksk accounts collapsed to one global constant machineId → now derived per credential type. Also: 429 reclassified as transient throttling, three data-plane endpoints collapsed to one, `amz-sdk-invocation-id` is now a UUID v4, header order aligned, `claude-opus-5` mapping added, and SSE gained a 25-second keep-alive |

---

## 🌟 Core Features

> 📖 Detailed usage guide: [USAGE.md](USAGE.md)

### 🔌 Four Protocol Front Ends, One Back End

- A single service simultaneously exposes **OpenAI Chat**, **Anthropic Messages**, **OpenAI Responses**, and **Gemini native** SDK formats
- Internally, **Anthropic Messages is the hub (mother) format**; every other protocol is converted both ways and reuses the same relay core
- Every protocol supports **streaming (SSE)**, **true function calling (tool) pass-through**, and **image input (multimodal)**
- **Dual-prefix mounting**: each protocol is served on both the standard bare prefix and an explicit vendor prefix (`/openai/v1`, `/claude/v1`, `/gemini/v1beta`), so mainstream SDKs just fill in `base_url` and work out of the box

### 🔐 Unified Authentication Gate

- Six accepted channels, first match wins: `Authorization: Bearer` > `x-api-key` > `x-goog-api-key` > `?api_key=` > `?token=` > `?key=`, constant-time comparison, `401` on failure
- `adminApiKey` (falling back to `apiKey`) protects `/api/admin/*` — when neither is configured the gate runs in open mode; holders use their own **API-KEY** to reach `/api/user/*`
- Liveness endpoints such as `/health` and `/v1/ping` require no authentication

### 🔄 Account Pool & Token Self-Healing

- **Multi-account round-robin**: `priority` (equal-weight round-robin, default) and `balanced` (weighted by `weight`) strategies, switchable at runtime from the admin panel
- Per-account RPM rate limiting and tiered cooldown; consecutive failures are handled differently by category (permanent invalidation / ambiguous auth / quota / transient)
- Tokens are **refreshed in memory automatically** on expiry (single-flight coordinated to avoid concurrent-refresh 401 cascades); on success the refreshed credentials are atomically written back to `credentials.json`
- Three login flows supported — Builder ID device code / IAM SSO authorization code / social token — and credentials can drop in existing Kiro data

### 🔀 Endpoint Fallback & Cross-Account Retry

- Kiro IDE → CodeWhisperer → AmazonQ multi-endpoint fallback in order, auto-switching on `429`/network errors
- Account-level failures automatically retry across accounts; deterministic request errors (such as an unsupported model, `INVALID_MODEL_ID`) are **not blindly retried and do not penalize accounts** — the upstream reason is returned directly to the client
- Body-aware failure classification: only genuine credential invalidation is permanently disabled, while quota/risk-control/rate-limit all cool down and self-heal

### 🖥 Web Management Panel

- Built-in static admin console (`/admin`), signed in with `adminApiKey`, driven by a rich `/api/admin/*` API
- **Dashboard**: live uptime counter, global remaining credits, system info (version/Rust/OS/memory/CPU/PID/run mode), sponsor QR-code cards (pulled live from remote config), and **update check** (compared against GitHub Releases) — the dialog shows the localized release notes plus a copyable upgrade command
- **Account management**: CRUD, three interactive logins, batch import (per-item verify-liveness + dedup), priority/weight, balance query
- **API-KEY management**: issue/disable/relabel, per-key spending limit and expiry (enforced on all four protocol front ends), per-key usage with paginated records
- **Model Test**: send a test request to any model from the panel to verify connectivity; defaults to the master API key when no custom keys exist
- **Usage stats**: per-day / per-account dimensions, including client IP and account label, drillable by day
- **Live logs**: structured table + direction filter + search + pagination + SSE real-time push + download
- **Settings**: switch load balancing / auth keys at runtime, integration examples (copyable snippets by protocol × language), and **one-click service restart**
- Top control bar: run-status badge, GitHub, restart, light/dark theme, 5-language switcher

### 👤 User Panel

- Built-in user console (`/user`); a holder signs in with their own **API-KEY** (no admin rights needed)
- View that key's quota, cumulative usage, and paginated records, driven by `/api/user/*`

### 🧭 Model Name Mapping

- The model name passed by the client is matched to one of 18 internal Kiro model ids by **lowercase substring** (no match → `400`) — the 17 of the admin catalog plus `auto`, which that catalog does not list
- The protocol `/models` endpoints return a **fixed, hard-coded three-entry list** — it is compiled in, not derived from your pool, so it is neither tier-filtered nor the full set of accepted names. `GET /api/admin/models` is the real catalog (live per-pool union, falling back to all 17)

### ⚡ High-Performance Architecture

- Built on **Rust + axum 0.8 + tokio**, fully async and non-blocking end to end
- AWS eventstream frame decoding, minimal critical section for the serialized account-pool lock, released the moment the request goes out on the wire
- Strongly typed serde validation, an independent adapter module per protocol
- Multi-stage Docker build, non-root execution (gosu), multi-arch images, health checks

---

## 🏗 Architecture

```
                               kiro2api
┌─────────────────────────────────────────────────────────────┐
│                                                             │
│  Client (OpenAI SDK / Anthropic SDK / Gemini SDK / cURL)    │
│       |                                                     │
│  POST /v1/chat/completions        (or /openai/v1/...)       │
│  POST /v1/messages                (or /claude/v1/...)       │
│  POST /v1/responses               (or /openai/v1/...)       │
│  POST /v1beta/models/:m:generateContent (or /gemini/...)    │
│       |                                                     │
│       v                                                     │
│  +-----------+    +----------------+    +---------------+   │
│  |  Adapters |--->|  Anthropic Hub |--->| Account Pool  |   │
│  |  (4 proto)|    |  (mother fmt)  |    | (load balance)|   │
│  +-----------+    +----------------+    +---------------+   │
│                                               |             │
│                                    ┌──────────┼────────┐    │
│                                    v          v        v    │
│                               Account-0  Account-1   ...   │
│                                                             │
│  +-----------+    +----------------+    +---------------+   │
│  |   Auth    |    | Token Refresh  |    | Failure       |   │
│  | Bearer/key|    | Single-flight  |    | Classify+Cool │   │
│  +-----------+    +----------------+    +---------------+   │
│                                                             │
│  +-----------+    +----------------+    +---------------+   │
│  | Endpoint  |    | Usage / Balance|    |  Live Logs    |   │
│  | Fallback  |    |  Stats + Cache |    |   (SSE)       │   │
│  +-----------+    +----------------+    +---------------+   │
│                                                             │
└─────────────────────────────────────────────────────────────┘
                           |
                  Kiro Data Plane (AWS eventstream)
             Kiro IDE → CodeWhisperer → AmazonQ fallback
                           |
                           v
                  generateAssistantResponse
                    (Claude-family models)
```

---

## 📋 System Requirements

| Dependency | Version | Notes |
|-----------|---------|-------|
| Rust | 2024 edition | Only needed to build from source; not required for Docker deployment |
| Docker | 20.10+ | Docker deployment recommended |
| Kiro account | — | Requires valid Kiro (CodeWhisperer) credentials (Builder ID / IdC / social login) |
| Architecture | amd64 / arm64 | Official images are multi-arch and auto-match one of the two |

> [!TIP]
> Docker deployment requires no local Rust installation, just Docker and valid Kiro credentials.

---

## ⚡ Quick Deployment

> 📖 Detailed deployment guide: [DEPLOY.md](DEPLOY.md)

> **Prerequisite**: You need a valid set of Kiro (CodeWhisperer) account credentials.

### 1. Get Kiro Credentials

Export the following fields from your Kiro client / existing Kiro credentials, or obtain them on the spot via the admin panel's three interactive login flows (Builder ID device code / IAM SSO authorization code / social token):

| Field | Description |
|-------|-------------|
| `accessToken` / `refreshToken` | Access token and refresh token (auto-refreshed on expiry) |
| `expiresAt` | Token expiry time (RFC3339) |
| `authMethod` | `social` (with `profileArn`) or `idc` (with `clientId`/`clientSecret`) |

### 2. Docker Deployment

```bash
# Clone repository
git clone https://github.com/xwteam/kiro2api.git
cd kiro2api

# Create environment file
cp .env.example .env
```

Edit `.env` and set at least one external access key `API_KEY`:

```env
API_KEY=sk-your-external-access-key
# Separate admin key; mandatory for public deployments (omit it and /api/admin/* falls back to API_KEY — with neither set it is wide open).
# Don't need it? Comment the whole line out — an empty value overrides the key already set in config.json.
ADMIN_API_KEY=sk-your-admin-key
```

Put your Kiro account credentials into `data/credentials.json` (an array, existing Kiro credentials can be dropped in directly):

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

Start the service:

```bash
mkdir -p data
docker compose up -d
```

Check logs to confirm successful startup:

```bash
docker compose logs -f
# Seeing the account pool ready and the listening port means startup succeeded
```

### 3. Verification

```bash
# Health check
curl http://localhost:8080/health
# {"service":"kiro2api","status":"ok","version":"0.15.0"}

# View available models
curl http://localhost:8080/v1/models \
  -H "Authorization: Bearer sk-your-api-key"

# Send test request
curl -X POST http://localhost:8080/v1/chat/completions \
  -H "Content-Type: application/json" \
  -H "Authorization: Bearer sk-your-api-key" \
  -d '{"model":"claude-sonnet-4.5","messages":[{"role":"user","content":"Hello"}]}'
```

Seeing AI response text means deployment succeeded. If you get 401, check your API Key.

---

## 🧪 Integration Examples

> [!NOTE]
> All API requests require an API Key. Six channels are accepted, tried in this order:
> - `Authorization: Bearer sk-xxx` (recommended, compatible with OpenAI/Anthropic SDKs)
> - `x-api-key: sk-xxx`
> - `x-goog-api-key: sk-xxx` (used by the official Gemini SDKs)
> - `?api_key=sk-xxx`, `?token=sk-xxx` or `?key=sk-xxx` in the query string, for clients that cannot set headers
>
> Use the **standard bare prefix** for the base URL: OpenAI = `{host}/v1`, Anthropic = `{host}` (the SDK appends `/v1/messages` automatically), Gemini = `{host}/v1beta`. You may also use the explicit vendor prefixes `/openai/v1`, `/claude/v1`, `/gemini/v1beta`.

<details>
<summary><b>OpenAI SDK (Python)</b></summary>

```python
from openai import OpenAI

client = OpenAI(
    base_url="http://localhost:8080/v1",
    api_key="sk-your-api-key",
)

resp = client.chat.completions.create(
    model="claude-sonnet-4.5",
    messages=[{"role": "user", "content": "Hello"}],
)
print(resp.choices[0].message.content)
```

</details>

<details>
<summary><b>Anthropic SDK (Python)</b></summary>

```python
import anthropic

client = anthropic.Anthropic(
    base_url="http://localhost:8080",
    api_key="sk-your-api-key",
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
<summary><b>Gemini SDK (Python)</b></summary>

```python
from google import genai

client = genai.Client(
    api_key="sk-your-api-key",
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
# Non-streaming request
curl -X POST http://localhost:8080/v1/chat/completions \
  -H "Content-Type: application/json" \
  -H "Authorization: Bearer sk-your-api-key" \
  -d '{"model":"claude-sonnet-4.5","messages":[{"role":"user","content":"Hi"}]}'

# Streaming request
curl -X POST http://localhost:8080/v1/chat/completions \
  -H "Content-Type: application/json" \
  -H "Authorization: Bearer sk-your-api-key" \
  -d '{"model":"claude-sonnet-4.5","messages":[{"role":"user","content":"Hi"}],"stream":true}'
```

</details>

<details>
<summary><b>Function Calling</b></summary>

```python
resp = client.chat.completions.create(
    model="claude-sonnet-4.5",
    messages=[{"role": "user", "content": "What's the weather in Beijing today"}],
    tools=[{
        "type": "function",
        "function": {
            "name": "get_weather",
            "description": "Get weather for a city",
            "parameters": {
                "type": "object",
                "properties": {"city": {"type": "string"}},
                "required": ["city"]
            }
        }
    }]
)
```

> Tool calls are **truly passed through** across all four protocols (Anthropic `tool_use` / OpenAI `tool_calls` / Gemini `functionCall`) — no simulation.

</details>

---

## 📡 API Endpoints

> 📖 Detailed API documentation: [API.md](API.md)

<details>
<summary><b>Click to expand the full endpoint list</b></summary>

> **Dual prefixes coexist**: each protocol is served on both a "standard bare path" and an "explicit vendor-prefix path". The bare path lets official SDKs fill in `base_url` without adding a suffix and work out of the box; the vendor prefix is for cleanly distinguishing the four vendors.

### OpenAI Compatible (`/v1` or `/openai/v1`)

| Method | Endpoint | Function |
|--------|----------|----------|
| GET | `/models` | Available models list |
| POST | `/chat/completions` | Chat completion (streaming returns `chat.completion.chunk` + `[DONE]`, with tools/images) |

### OpenAI Responses (`/v1/responses` or `/openai/v1/responses`)

| Method | Endpoint | Function |
|--------|----------|----------|
| POST | `/responses` | Responses API (streaming uses named events + a monotonic `sequence_number`, no `[DONE]`; `previous_response_id` returns 400) |

### Anthropic Compatible (`/v1` message entry; `/claude/v1` explicit prefix)

| Method | Endpoint | Function |
|--------|----------|----------|
| POST | `/v1/messages` | Messages (streaming/tools/images) |
| POST | `/v1/messages/count_tokens` | Token estimation |
| GET | `/claude/v1/models` | Models list (Anthropic shape, avoids clashing with OpenAI `/v1/models`) |
| POST | `/claude/v1/messages` · `.../count_tokens` | Explicit-prefix variants |

### Gemini Native (`/v1beta` or `/gemini/v1beta`)

| Method | Endpoint | Function |
|--------|----------|----------|
| GET | `/models` | Models list |
| POST | `/models/{m}:generateContent` | Content generation (non-streaming) |
| POST | `/models/{m}:streamGenerateContent` | Streaming generation (`?alt=sse`, camelCase) |

### Admin / User / Ops

| Method | Endpoint | Function |
|--------|----------|----------|
| GET | `/admin` · `/api/admin/*` | Admin panel + admin API (with `adminApiKey`, open when no key is configured: credential CRUD / login / API-KEY / usage / logs / balance / settings / update check / restart) |
| GET | `/user` · `/api/user/*` | User panel + API (with the holder's own API-KEY) |
| GET | `/health` · `/v1/ping` | Liveness (no auth) |

</details>

> The `localhost:8080` in the URLs is just an example; the port is configured via `PORT`/`config.json` — replace it for your deployment.
>
> The key may ride on any channel the gate accepts, in priority order `Authorization: Bearer` > `x-api-key` > `x-goog-api-key` > query (`?api_key=` > `?token=` > `?key=`). Gemini's native `x-goog-api-key` header and `?key=` parameter **are** honored, so the official SDK works with just a `base_url` swap — what must change is the *value*: always pass **this service's** key, never a real vendor key.

---

## ⚙ Configuration

Priority: **command-line flags > environment variables > `config.json` > built-in defaults**. There are exactly two command-line flags: `-c/--config` (config file path) and `--credentials` (credentials file path; when omitted, `CREDENTIALS_PATH` / `config.json` / the built-in default decides). The mounted volume `./data` holds `config.json`, `credentials.json`, logs, and runtime state.

> The credentials path also decides where usage stats (`stats/`), API-KEY storage (`api_keys.json`), and the balance cache are written — all of them use the parent directory of `credentials.json`. The built-in default resolves next to the config file given by `-c`, and the container starts with `-c /app/data/config.json`, so by default this data lands on the mounted volume; if you point the path somewhere else, point it inside the volume too, or everything is gone the moment the container is recreated.

**Environment variables** (see `.env.example`):

| Variable | Required | Default | Description |
|----------|----------|---------|-------------|
| `API_KEY` | ✅ | — | External access key (empty leaves the protocol endpoints openly accessible, startup warns) |
| `ADMIN_API_KEY` | ❌ | falls back to `API_KEY` | Separate auth key for the admin side; with neither this nor `API_KEY` set, `/api/admin/*` is open — mandatory for public deployments |
| `HOST` | ❌ | `127.0.0.1` (image ships `0.0.0.0`) | Listen address |
| `PORT` | ❌ | `8080` | Service port (the compose port mapping and the healthcheck both follow this value) |
| `REGION` | ❌ | `us-east-1` | Default AWS region (the region inside the account's `profileArn` takes precedence) |
| `LOAD_BALANCING_MODE` | ❌ | `priority` | Load balancing: `priority` (equal-weight round-robin) / `balanced` (weighted by weight) |
| `MAX_RPM_PER_CREDENTIAL` | ❌ | `0` | Per-account per-minute request cap, `0` = unlimited |
| `CREDENTIALS_PATH` | ❌ | `credentials.json`, resolved next to the `-c` config file (so `/app/data/credentials.json` in the container) | Credentials file path; overridden by the `--credentials` flag |

**`data/config.json`** (camelCase, all optional; `logCapacity` is configured here only):

```json
{
  "host": "0.0.0.0",
  "port": 8080,
  "region": "us-east-1",
  "apiKey": "sk-your-external-access-key",
  "adminApiKey": "optional, admin side",
  "credentialsPath": "/app/data/credentials.json",
  "loadBalancingMode": "priority",
  "maxRpmPerCredential": 0,
  "logCapacity": 5000,
  "kiroVersion": "0.11.107",
  "systemVersion": "win32#10.0.22631",
  "nodeVersion": "22.22.0"
}
```

- `logCapacity`: ring-buffer size for live logs; `>0` enables log capture (admin log-page replay/SSE), `0` disables it (the log endpoint returns 503); default `5000`.
- `kiroVersion`/`systemVersion`/`nodeVersion`: spoofed UA version numbers, injected from config.

---

## ⚠ Important Notes

1. **Always set both `API_KEY` and `ADMIN_API_KEY` for external deployments**: with `API_KEY` empty the protocol endpoints are openly accessible (startup warns); with neither `adminApiKey` nor `apiKey` configured, `/api/admin/*` is just as open — credentials, API-KEYs, and the auth settings can all be rewritten by anyone. The `/admin` and `/user` panels themselves are never gated (the real gate sits on their `/api/**` endpoints); be careful changing `HOST=0.0.0.0` on bare metal.

2. **Available models depend on the account subscription tier**: the free tier (KIRO FREE) usually authorizes only `claude-sonnet-4.5`; requesting an unsupported model returns `400` (`INVALID_MODEL_ID`), without blind retries or penalizing accounts.

3. **Token self-healing**: tokens are refreshed in memory automatically on expiry and atomically written back to `credentials.json`; only genuine credential invalidation is permanently disabled, while quota/risk-control/rate-limit all cool down and self-heal.

4. **Streaming Output**: all four protocols support streaming; when `stream:false`, the service still decodes the event stream internally and returns the complete JSON in one shot after collection. An upstream error or a mid-stream transport interruption always ends the stream with that protocol's own error event — never a normal finish — and hitting the upstream's output budget or exhausting the context is reported with the matching truncation reason (`length` / `max_tokens` / `MAX_TOKENS`) instead of a clean stop.

5. **Network Environment**: the deployment server must be able to reach the AWS CodeWhisperer/Kiro endpoints (`*.amazonaws.com`).

6. **Generation parameters are accepted but ignored**: `temperature` (and every other sampling knob), `max_tokens` / `max_output_tokens` / `maxOutputTokens`, and `tool_choice` are all dropped — the Kiro data plane has no wire fields for them, so nothing is forwarded and nothing errors. `max_tokens` does not cap the answer, and a tool cannot be forced. The one exception is Gemini `toolConfig` with `functionCallingConfig.mode: "NONE"`, honored by withholding the tool definitions. See [API.md](API.md#post-openaiv1chatcompletions).

7. **Images must be inline base64**: remote `http(s)://` image URLs are **rejected with `400`**, not fetched — encode the image as a `data:` URI (OpenAI `image_url`), an Anthropic `source.type: "base64"` block, or Gemini `inlineData`.

---

## 🗂 Project Structure

```
kiro2api/
├── src/
│   ├── main.rs / cli.rs / lib.rs   # entry, CLI, library root
│   ├── config.rs                   # config (env > config.json > default)
│   ├── http.rs                     # outbound HTTP client (hard timeout ceiling)
│   ├── logcap.rs                   # live-log ring buffer + SSE broadcast
│   ├── server/                     # axum route assembly, unified auth gate
│   ├── protocol/                   # four protocol adapters
│   │   ├── anthropic/              #   Anthropic Messages (hub mother format + relay core)
│   │   ├── openai/                 #   OpenAI Chat Completions
│   │   ├── responses/              #   OpenAI Responses
│   │   └── gemini/                 #   Gemini native v1beta
│   ├── kiro/                       # Kiro data plane
│   │   ├── pool.rs                 #   account pool (load balancing + failure classify + cooldown)
│   │   ├── provider.rs             #   upstream send + endpoint fallback
│   │   ├── convert.rs              #   model mapping + request/response conversion
│   │   ├── ensure_fresh.rs / refresh.rs  # single-flight token refresh
│   │   ├── eventstream/            #   AWS eventstream frame decoding
│   │   └── login/                  #   Builder ID / IAM SSO / social login flows
│   ├── admin/                      # /api/admin/* admin API
│   ├── user/                       # /api/user/* user API
│   ├── apikey/                     # API-KEY storage and validation
│   ├── balance/                    # balance cache (TTL)
│   ├── stats/                      # usage/failure/throttle stats + pricing
│   ├── models_cache/               # dynamic model-list cache
│   └── webui/                      # rust-embed static panel service (admin-ui-v2/, user-ui/dist)
├── admin-ui-v2/                    # static admin panel (HTML/CSS/JS, embedded at build time)
├── user-ui/                        # user panel (build output embedded)
├── data/                           # persistent data (Docker volume mount)
│   ├── config.json                 #   runtime config
│   └── credentials.json            #   Kiro account credentials
├── docs/                           # 5-language docs (README/USAGE/DEPLOY/API/SPONSORS)
├── Dockerfile                      # multi-stage build (multi-arch, non-root)
├── docker-compose.yml              # orchestration config
├── Cargo.toml / Cargo.lock
└── .env.example
```

---

## 🗺 Roadmap

- [x] Four protocol front ends (OpenAI / Anthropic / OpenAI-Responses / Gemini)
- [x] Anthropic Messages hub mother format + unified relay core
- [x] Streaming (SSE) + true function-call pass-through + image multimodal
- [x] Kiro account pool (multi-account round-robin, tiered cooldown, load balancing)
- [x] Single-flight automatic token refresh + atomic write-back
- [x] Endpoint fallback (Kiro/CodeWhisperer/AmazonQ) + cross-account retry
- [x] Body-aware failure classification (only permanent invalidation is disabled, the rest cool down and self-heal)
- [x] Unified auth gate (Bearer / x-api-key / x-goog-api-key / ?api_key= / ?token= / ?key=)
- [x] Web management panel (credentials/login/API-KEY/usage/logs/balance/settings)
- [x] User panel (holder signs in with their own API-KEY)
- [x] Three interactive login flows (Builder ID / IAM SSO / social token)
- [x] Per-day / per-account usage stats (including client IP and account label)
- [x] Live logs (SSE) + balance cache + dynamic model list
- [x] Integration examples (copyable snippets by protocol × language)
- [x] Service restart + version update check (compared against GitHub Releases)
- [x] Docker multi-arch (amd64/arm64) delivery + CI
- [ ] Auth for the `/admin` and `/user` panels themselves
- [ ] GitHub Actions auto-build and publish images

---

## ☕ Support & Contribute

Find this helpful? Buy the author a coffee or join the WeChat group for support. The QR codes are on the admin panel dashboard. For full details, see [SPONSORS.md](SPONSORS.md).

kiro2api is primarily maintained by one person — contributions via code, docs, fixes, or PRs are welcome.

1. Fork this repository
2. Create a branch `git checkout -b feature/your-feature`
3. Commit code `git commit -m "feat: add something"`
4. Push and create a Pull Request

---

## 🙏 Acknowledgments

Thanks to everyone who submitted bug reproductions, logs, compatibility feedback, and feature suggestions through [Issues](https://github.com/xwteam/kiro2api/issues). Your feedback directly drove the iteration of core capabilities such as the account pool, token self-healing, endpoint fallback, multi-protocol compatibility, and the Web panel.

---

## ⭐ Star History

<a href="https://star-history.com/#xwteam/kiro2api&Date">
 <picture>
   <source media="(prefers-color-scheme: dark)" srcset="https://api.star-history.com/chart?repos=xwteam/kiro2api&type=date&theme=dark&legend=top-left" />
   <source media="(prefers-color-scheme: light)" srcset="https://api.star-history.com/chart?repos=xwteam/kiro2api&type=date&legend=top-left" />
   <img alt="Star History Chart" src="https://api.star-history.com/chart?repos=xwteam/kiro2api&type=date&legend=top-left" />
 </picture>
</a>

---

## 📄 License

This project uses the [MIT License](../../LICENSE):

- **Allowed**: personal learning, research, self-hosted deployment, secondary development
- **Required**: retain the copyright and license notice

This project is not affiliated with Amazon / AWS / Kiro. Users assume all risks and must comply with the relevant terms of service.

---

## ⚠ Disclaimer

1. **Technical nature**: kiro2api is a technical research project that wraps the Kiro (CodeWhisperer) backend into a multi-protocol-compatible API. This project provides no AI service of its own; all generated content comes from upstream. Using this project may violate the relevant terms of service, and any consequences arising therefrom are the user's own responsibility.

2. **No warranty**: this project is provided "as is", without any express or implied warranty, including but not limited to merchantability or fitness for a particular purpose. The developers are not liable for account bans, data loss, or any other losses caused by using this project.

3. **Data & privacy**: this project runs entirely in the user's local environment; it does not collect, upload, or store any user data. Your credentials and API Key are stored only in local config — keep them safe and never leak them.

4. **Compliance responsibility**: users must ensure their usage complies with the laws and regulations of their region. Using this project for any illegal or non-compliant activity is strictly prohibited.

5. **Third-party services**: this project has no affiliation with or authorization from Amazon / AWS / Kiro. The availability, stability, and content accuracy of upstream services are the responsibility of their providers and have nothing to do with this project.

---

<div align="center">
  <sub>Built with Rust + axum + tokio | Powered by Kiro (CodeWhisperer)</sub>
</div>
