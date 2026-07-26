# Usage Guide

This guide covers using kiro2api through the web panels and integrating it with third-party clients.

## Web Management Panel

Access the web panel at `http://localhost:8080` (or your server IP).

### Login

1. Open `http://localhost:8080/admin` in your browser
2. Enter your `adminApiKey` (falls back to `apiKey` if unset)
3. Click **Login**

The key comes from `.env` (`ADMIN_API_KEY` / `API_KEY`) or `config.json`. The `/api/admin/*` endpoints are guarded by this key — but only once one of the two is actually configured; with neither set the admin API is open to anyone. The `/api/user/*` endpoints are reached by callers with their own **API-KEY**.

### Dashboard

The dashboard provides a system overview:

- **Service Status**: Running time (live counter), version, and run mode
- **Account Pool**: Number of active Kiro accounts and their health status
- **Global Credits**: Remaining balance aggregated across the pool
- **System Info**: Rust build info, OS, memory usage, CPU usage, PID
- **Configuration**: Current load-balancing mode and per-credential RPM limit
- **Check for Updates**: On dashboard load the panel silently auto-checks GitHub for a newer release. When one exists, the button highlights as **Update to vX**; clicking it opens an **Update Service vX** dialog that shows the release notes for the current UI language in a scrollable box, plus the upgrade command `docker compose pull && docker compose up -d` with a one-click copy button. It only informs and displays — it never runs the upgrade automatically
- **Sponsor QR Codes**: Shareable codes pulled from remote config

### Account Management

Manage your Kiro (CodeWhisperer) accounts:

1. Click **Credentials** in the sidebar
2. View all configured accounts with their status (health, weight, failure/throttle counts, balance)
3. **Add Account**: Bring in accounts without touching `credentials.json` — interactive **Builder ID** (device code), **IAM Identity Center (SSO)** login, **social token** import, or **batch import**. Batch import accepts one bearer/SSO token per line, or a pasted credentials array / `{accounts}` object, and processes accounts **one by one with a live display**: a progress bar and a "processing account i/N" line, running **success / duplicate / failed** counts, and a per-account status list where each row updates in real time (pending → checking → verifying → verified with that account's usage / duplicate / failed-excluded). For each account it queries the balance once (a real upstream `getUsageLimits` call) to **verify it is alive** — a live account is kept, a dead one is automatically rolled back/deleted and filtered out. It also **dedupes by `refreshToken`**: an account already in the pool is skipped, so the same account is never imported twice (which would make two credentials race the same rotating token — mutual invalidation, wasted quota, upstream risk-control). Verified accounts are saved immediately, so interrupting mid-import keeps whatever already succeeded, and the dialog cannot be closed while importing
4. **Enable / Disable / Reset**: Toggle an account on/off or clear its cooldown state
5. **Edit Priority / Weight**: Tune how the load balancer rotates the account
6. **Check Balance**: Query the remaining credits for an account
7. **Remove Account**: Delete an account permanently

Changes take effect immediately without restarting.

### Logs

View live server logs:

1. Click **Logs** in the sidebar
2. Logs stream in real time over **SSE**, with a structured table
3. **Filter by Direction**: View requests, responses, or errors
4. **Search**: Filter logs by text content
5. **Pagination**: Browse older entries page by page
6. **Download**: Export the current buffer as a `.txt` file

Live logs require `logCapacity > 0` in `config.json` (default `5000`; see the [Deployment Guide](DEPLOY.md)). When set to `0`, log endpoints return `503`.

### Usage Statistics

Monitor API usage and performance:

1. Click **Stats** in the sidebar
2. View summary metrics:
   - Daily and per-account request totals
   - Failure and throttle logs
   - A live requests-per-minute view
3. Usage records include the **client IP** and **account label**
4. Drill down by day for historical trends

### Model Test

Verify that an account/model actually works, straight through the relay:

1. Click **Model Test** in the sidebar
2. Pick a model (and, optionally, a specific endpoint)
3. Click **Send** — the test request goes through the relay and the raw result is shown
4. This calls the relay with one of your created API keys. When no custom keys have been created yet, it **defaults to the master API key** (`adminApiKey` / `apiKey`) so testing works out of the box

The key is stored only in the browser (localStorage) and is used solely to call the relay endpoint.

### API Keys Management

Manage the outbound keys you hand to callers:

1. Click **API keys** in the sidebar
2. **View Keys**: List all configured keys (values masked)
3. **Add Key**: Issue a new key, set a spending limit and expiry
4. **Enable / Disable**: Toggle keys on/off without deleting
5. **Delete Key**: Remove a key permanently
6. **Inspect Usage**: Reset or browse paginated per-key usage records

Each key holder can also sign in to the **User Panel** (`/user`) with their own key to review quota and usage.

### Settings

Configure runtime behavior:

1. Click **Settings** in the sidebar
2. Modify settings that take effect immediately:
   - **Load Balancing**: Switch between `priority` (equal-weight round-robin) and `balanced` (weighted by `weight`)
   - **Auth Keys**: Rotate `apiKey` / `adminApiKey` at runtime (no restart)
   - **Integration Snippets**: Copy ready-made examples per protocol × language
   - **Server Info**: Shows the (masked) master key and the kiro2api version
3. Changes apply live without restarting the service

### Service Control

Manage the service from the top control bar:

- **Restart Service**: Restart the service in one click (useful after configuration changes)
- **Theme**: Toggle between light and dark themes
- **Language**: Switch the interface language (5 languages)
- **GitHub**: Jump to the repository

## Image Input

kiro2api supports multimodal content, including image input. Three API formats are supported for image transmission.

### OpenAI Format

Use `image_url` type in the `messages` array. Supports Base64 Data URI:

```bash
curl -X POST http://localhost:8080/v1/chat/completions \
  -H "Content-Type: application/json" \
  -H "Authorization: Bearer sk-your-api-key" \
  -d '{
    "model": "claude-sonnet-4.5",
    "messages": [
      {
        "role": "user",
        "content": [
          {"type": "text", "text": "What is this"},
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

### Claude Format

Use `image` type in the `content` array:

```bash
curl -X POST http://localhost:8080/v1/messages \
  -H "Content-Type: application/json" \
  -H "Authorization: Bearer sk-your-api-key" \
  -d '{
    "model": "claude-sonnet-4.5",
    "max_tokens": 1024,
    "messages": [
      {
        "role": "user",
        "content": [
          {"type": "text", "text": "What is this"},
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

### Gemini Native Format

Use `inlineData` in the `parts` array:

```bash
curl -X POST http://localhost:8080/v1beta/models/claude-sonnet-4.5:generateContent \
  -H "Content-Type: application/json" \
  -H "Authorization: Bearer sk-your-api-key" \
  -d '{
    "contents": [
      {
        "parts": [
          {"text": "What is this"},
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

## Supported Models

The models you can call depend on your **Kiro account subscription tier**. The free tier (KIRO FREE) typically authorizes only `claude-sonnet-4.5`; opus / GPT tiers require a higher subscription.

Incoming model names are matched to Kiro's internal models by **lowercase substring**. A name that matches nothing returns `400` (`INVALID_MODEL_ID`) — the service does not blindly retry or harm the account, it returns the upstream reason directly.

| Model ID | Description |
|----------|-------------|
| `claude-sonnet-4.5` | Claude Sonnet, available on the free tier; the recommended default |

**List then use**: Query the `/models` endpoint (or `/claude/v1/models`, `/v1beta/models`) to see the ids this service can actually serve, then call one of those. This keeps clients correct across subscription changes.

## Third-Party Client Integration

> [!NOTE]
> Base URLs use the **standard bare prefixes**: OpenAI = `{host}/v1`, Anthropic = `{host}` (the SDK appends `/v1/messages`), Gemini = `{host}/v1beta`. Explicit vendor prefixes `/openai/v1`, `/claude/v1`, `/gemini/v1beta` also work.

### ChatGPT-Next-Web

1. Deploy ChatGPT-Next-Web or open the web interface
2. Click **Settings** (bottom-left)
3. Under **API Settings**:
   - **API Key**: Enter your kiro2api API Key (sk-...)
   - **API Endpoint**: `http://SERVER_IP:8080/v1`
4. Click **Save**
5. Start a new conversation and select `claude-sonnet-4.5`

### LobeChat

1. Open LobeChat settings
2. Go to **Model Provider** → **OpenAI**
3. Configure:
   - **API Key**: Your kiro2api API Key
   - **Base URL**: `http://SERVER_IP:8080/v1`
4. Save and refresh
5. The model appears in the model selector

### OpenCat (iOS)

1. Open OpenCat app
2. Tap **Settings** → **API Configuration**
3. Add custom endpoint:
   - **Name**: kiro2api
   - **API Key**: Your kiro2api API Key
   - **Base URL**: `http://SERVER_IP:8080/v1`
4. Select kiro2api as your provider
5. Choose `claude-sonnet-4.5` and start chatting

### Python SDK (OpenAI)

```python
from openai import OpenAI

client = OpenAI(
    api_key="sk-your-api-key",
    base_url="http://localhost:8080/v1"
)

response = client.chat.completions.create(
    model="claude-sonnet-4.5",
    messages=[
        {"role": "user", "content": "Explain quantum computing"}
    ],
    stream=True
)

for chunk in response:
    print(chunk.choices[0].delta.content or "", end="")
```

### Python SDK (Anthropic/Claude)

```python
import anthropic

client = anthropic.Anthropic(
    api_key="sk-your-api-key",
    base_url="http://localhost:8080"
)

message = client.messages.create(
    model="claude-sonnet-4.5",
    max_tokens=1024,
    messages=[
        {"role": "user", "content": "Write a haiku about programming"}
    ]
)

print(message.content[0].text)
```

### Python SDK (Gemini)

```python
from google import genai

client = genai.Client(
    api_key="sk-your-api-key",
    http_options={"base_url": "http://localhost:8080/v1beta"}
)

response = client.models.generate_content(
    model="claude-sonnet-4.5",
    contents="Hello"
)
print(response.text)
```

### cURL

```bash
# Non-streaming request
curl -X POST http://localhost:8080/v1/chat/completions \
  -H "Content-Type: application/json" \
  -H "Authorization: Bearer sk-your-api-key" \
  -d '{
    "model": "claude-sonnet-4.5",
    "messages": [{"role": "user", "content": "Hi"}]
  }'

# Streaming request
curl -X POST http://localhost:8080/v1/chat/completions \
  -H "Content-Type: application/json" \
  -H "Authorization: Bearer sk-your-api-key" \
  -d '{
    "model": "claude-sonnet-4.5",
    "messages": [{"role": "user", "content": "Hi"}],
    "stream": true
  }'
```

## Conversation Context

kiro2api holds **no server-side session memory**. To have a multi-turn conversation, send the full history with each request.

### Method 1: Client-Side History

Most clients (ChatGPT-Next-Web, LobeChat, etc.) maintain conversation history locally and resend it automatically. Simply continue the conversation in the UI.

### Method 2: Resend History Programmatically

For programmatic use, append prior turns to the `messages` array yourself:

```python
history = [{"role": "user", "content": "Remember this: I like Python"}]
r1 = client.chat.completions.create(model="claude-sonnet-4.5", messages=history)
history.append({"role": "assistant", "content": r1.choices[0].message.content})

# Continue the same conversation by resending the full history
history.append({"role": "user", "content": "What do I like?"})
r2 = client.chat.completions.create(model="claude-sonnet-4.5", messages=history)
```

The OpenAI Responses endpoint follows the same rule: `previous_response_id` is **not supported** (returns `400`), because there is no server-side session to resume.

## Token & Account Maintenance

kiro2api keeps accounts alive automatically — you rarely touch credentials by hand.

### Token Self-Healing

When an access token expires, the service refreshes it **in memory** with single-flight coordination (so concurrent requests don't cascade into 401s), then atomically writes the result back to `credentials.json`. No manual refresh, no restart.

### Failure Classification & Cooldown

Failures are classified from the response body:

- **Permanent credential failure** → the account is disabled
- **Ambiguous auth / quota / throttle / transient** → the account is cooled down and heals on its own

Only genuine credential invalidation disables an account; quota, risk-control, and rate limits all recover via cooldown.

### Endpoint Fallback & Cross-Account Retry

The upstream data plane falls back in order **Kiro IDE → CodeWhisperer → AmazonQ**, switching automatically on `429` or network errors. Account-level failures also trigger a cross-account retry. Deterministic request errors (such as an unsupported model, `INVALID_MODEL_ID`) are **not** retried and do **not** penalize the account — the upstream reason is returned to the caller directly.

## Performance Tips

1. **Use Multiple Accounts**: Distribute load across several Kiro accounts for better throughput
2. **Pick a Load-Balancing Mode**: Use `priority` for equal-weight round-robin, or `balanced` to weight accounts by their `weight`
3. **Cap Per-Account RPM**: Set `MAX_RPM_PER_CREDENTIAL` to protect each account from overload (`0` = unlimited)
4. **Match Model to Tier**: Free-tier accounts serve `claude-sonnet-4.5`; requesting an unauthorized model just returns `400`
5. **Enable Live Logs Sparingly**: `logCapacity` drives the ring buffer and SSE stream; keep it modest on busy deployments

## Troubleshooting

### "Unauthorized" Error (401)

- Verify the API Key is correct
- Check the Authorization header format: `Authorization: Bearer sk-xxx` (or `x-api-key: sk-xxx`, or `?token=sk-xxx`)
- If the token was refreshing during the request, retry once — self-healing may have re-issued it
- Rotate the key in **Settings** if needed

### "No Available Accounts"

- All accounts may be disabled or cooling down
- Check account health in **Credentials**; re-enable or reset an account
- Only permanent credential failures disable an account — quota/throttle cases heal on their own after cooldown
- Add another account via the interactive login flows

### Slow Responses

- Check account health status
- Reduce concurrent requests or cap `MAX_RPM_PER_CREDENTIAL`
- Add more accounts for load distribution
- Confirm the server can reach the AWS CodeWhisperer/Kiro endpoints (`*.amazonaws.com`)

### Model Not Found (400 `INVALID_MODEL_ID`)

- Verify the model name — matching is by lowercase substring
- Query `GET /v1/models` to see the ids this service can actually serve
- Model availability depends on your Kiro account subscription tier
