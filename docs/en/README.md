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
  <img src="https://img.shields.io/badge/version-v0.2.1-success?style=flat-square" alt="Version">
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

> For the complete changelog, see [CHANGELOG.md](CHANGELOG.md).

| Date | Update |
|------|--------|
| 2026-07-27 | v0.2.1 - 🔒 Follow-up audit fixes (39 findings confirmed by adversarial review, this round also covering the panels and the docs, which had never been audited): secret-bearing files (`api_keys.json`, `config.json`) were written world-readable and silently re-widened on every flush even after a manual `chmod`; the client IP could be forged by anyone reaching the port directly; an API-KEY credential binding was stored but never enforced; `GET /api/admin/models` kicked off an unbounded full-pool upstream sweep on every dashboard visit (now single-flight, bounded, with a cooldown); a corrupt credentials file was treated as an empty pool and then overwritten, destroying every account (now backed up and salvaged entry by entry); API-KEY changes were lost on shutdown; OpenAI parallel tool calls produced an invalid tool round-trip; several Gemini payloads (builtin tools, snake_case keys, non-image `inlineData`) were rejected or mangled; the 2 MB body limit rejected ~1.5 MB images; plus many admin/user panel fixes |
| 2026-07-26 | v0.2.0 - 🛠 Full-chain audit fixes: API-KEY spending limits now apply on all four protocols (they previously only took effect on the Anthropic endpoint, so the other three could spend without limit and showed zero usage); the admin plane is no longer open when only user-level API-KEYs are configured; upstream errors, mid-stream transport interruptions and truncation are no longer reported as a normal completion on any protocol; account-pool refresh failures now feed back into the pool; usage/billing is no longer lost on restart and the ledger file stays rollback-safe; `--credentials` and the PORT-aware health check now actually work |
| 2026-07-26 | v0.1.4 - 🐛 Fix: the Anthropic `system` field now accepts a content-block array (not only a string) — no more 422 when Claude Code / prompt-caching SDKs send it as an array |
| 2026-07-26 | v0.1.3 - Bulk JSON import now shows live per-account progress: a progress bar, running success/duplicate/failed stats, and a per-row status list (verifying → verified with usage / duplicate / failed, rolled back); verified accounts are saved immediately, so interrupting mid-import does not lose them |
| 2026-07-25 | v0.1.2 - Update dialog revamp: the check-update dialog shows localized release notes + a copyable upgrade command; the button highlights "Update to vX" when an update is available; fixed the copy button over plain HTTP |
| 2026-07-25 | v0.1.1 - Panel & account-import fixes: Model Test defaults to the master API key; batch import switched to per-item "verify liveness + dedup"; fixed batch import failing on larger lists; user-panel/all-page favicon + 128x128 logo & version badge in every README; cross-compiled multi-arch image build |
| 2026-07-25 | v0.1.0 - 🚀 First release: four protocol front ends (Anthropic hub + OpenAI / OpenAI-Responses / Gemini), Kiro account pool (multi-account round-robin / tiered cooldown / token self-healing), endpoint fallback and cross-account retry, unified auth gate, `/admin` management panel and `/user` user panel, per-day / per-account usage stats, failure/throttle logs, account balance cache, live logs (SSE), three interactive login flows, Docker multi-arch (amd64/arm64) delivery with CI |

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
# {"service":"kiro2api","status":"ok","version":"0.2.1"}

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
