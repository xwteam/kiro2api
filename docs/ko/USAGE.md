# 사용 가이드

kiro2api의 Web 패널, API 클라이언트 연동, 그리고 고급 기능 사용법을 설명합니다.

## Web 패널 기능

kiro2api는 두 개의 내장 정적 패널(rust-embed로 컴파일 시 임베드)을 제공합니다: `adminApiKey`로 로그인하는 **관리 패널**(`/admin`)과, key 보유자가 **자신의 API-KEY**로 로그인하는 **사용자 패널**(`/user`)입니다.

### 접속 방법

브라우저에서 `http://서버IP:8080/admin`으로 접속하면 로그인 페이지가 나타납니다.

**로그인**:
- `adminApiKey` 입력 (미설정 시 `apiKey`로 폴백, `.env`의 `ADMIN_API_KEY`/`API_KEY` 또는 로그에서 확인)
- "로그인" 버튼 클릭

### 대시보드

메인 페이지에서 서비스 상태를 한눈에 확인:

- **운행 시간**: 서비스 시작 이후 경과 시간 실시간 표시
- **전역 잔여 적분**: 모든 계정의 잔여 크레딧 집계
- **시스템 정보**: 버전, Rust, OS, 메모리, CPU, PID, 실행 모드
- **후원 QR 카드**: 원격 설정을 실시간으로 가져와 표시
- **업데이트 확인**: GitHub Release와 버전 비교

#### 업데이트 확인 대화상자

대시보드를 열면 패널이 조용히 자동으로 업데이트를 확인합니다. GitHub에 더 새로운 릴리스가 있으면 "업데이트 확인" 버튼이 **「vX로 업데이트」**로 강조됩니다.

버튼을 클릭하면 **"서비스 업데이트 vX"** 대화상자가 열리며 다음을 표시합니다:
- **현재 UI 언어**의 릴리스 노트 섹션을 스크롤 가능한 상자에 표시
- 업그레이드 명령 `docker compose pull && docker compose up -d`와 원클릭 복사 버튼

이 대화상자는 안내/표시만 하며, 업그레이드를 자동으로 실행하지 않습니다.

### 계정 관리

"자격 증명(Credentials)" 탭에서 Kiro(CodeWhisperer) 계정 풀을 관리:

**계정 추가** — `credentials.json`을 직접 건드리지 않고 세 가지 대화형 로그인으로 추가:
1. "계정 추가" 버튼 클릭
2. 로그인 방식 선택:
   - **Builder ID**(디바이스 코드)
   - **IAM Identity Center(SSO)**(권한 코드)
   - **소셜 토큰**(bearer 토큰 일괄 가져오기, 한 줄에 하나)
3. 안내에 따라 인증 완료

**일괄 가져오기**:
- 한 줄에 하나씩의 bearer/SSO 토큰, 또는 붙여넣은 자격 증명 배열 / `{accounts}` 객체 형식을 한 번에 배치 가져오기
- kiro2api는 계정을 **하나씩** 추가하며, 각 계정을 추가한 직후 그 계정의 잔액을 한 번 조회(실제 업스트림 `getUsageLimits` 호출)하여 **생존 여부를 검증**합니다. 살아 있는 계정은 유지되고, 죽은 계정은 자동으로 롤백/삭제되어 걸러집니다.
- **리프레시 토큰 기준 중복 제거**: 이미 풀에 있는 계정은 건너뛰므로 같은 계정을 두 번 가져오지 않습니다(두 자격 증명이 같은 회전 토큰을 두고 경쟁하면 상호 무효화, 쿼터 낭비, 업스트림 리스크 컨트롤을 유발함)
- **실시간 진행 표시**: 가져오기 대화상자는 처리 과정을 실시간으로 보여줍니다 — 진행률 바와 「계정 i/N 처리 중」 표시줄, 실시간 누적 성공/중복/실패 통계, 그리고 각 행이 실시간으로 갱신되는 계정별 상태 목록(대기 중 → 확인 중 → 검증 중 → 사용량과 함께 검증 완료 / 중복 / 실패-제외). 검증된 계정은 **즉시 저장**되므로 도중에 중단해도 이미 성공한 계정은 유지되며, 가져오기가 진행되는 동안에는 대화상자를 닫을 수 없습니다.

**계정 편집/삭제**:
- 우선순위(`priority`) / 가중치(`weight`) 편집
- 활성화 / 비활성화 토글
- 실패·스로틀 카운터 초기화, 계정 삭제

**상태 확인**:
- 각 계정의 상태(활성/냉각/실패), 가중치, 실패/스로틀 수, 잔액 표시

### 계정 로그인

kiro2api는 세 가지 대화형 로그인 플로우를 지원하여, 관리 패널에서 새 Kiro 자격 증명을 현장에서 발급받을 수 있습니다.

**Builder ID(디바이스 코드)**:
1. 관리 패널 "자격 증명" 탭에서 "계정 추가" → Builder ID 선택
2. 표시된 디바이스 코드로 브라우저에서 인증
3. 승인 후 자격 증명이 풀에 추가

**IAM Identity Center(SSO)**:
1. "계정 추가" → IAM Identity Center 선택
2. SSO 권한 코드 플로우로 인증
3. 승인 후 자격 증명이 풀에 추가

**소셜 토큰**:
1. "계정 추가" → 소셜 토큰 선택
2. bearer/SSO 토큰을 한 줄에 하나씩 붙여넣기
3. 저장하면 풀에 추가

### 실시간 로그

"로그(Logs)" 탭에서 구조화된 로그 확인:

**기능**:
- 구조화된 표 형식
- 방향 필터 (요청/응답/오류)
- 텍스트 검색
- 페이지 분할
- SSE 실시간 푸시
- 스냅샷 및 `.txt` 다운로드

**로그 관리**:
- 실시간 로그는 링 버퍼(`logCapacity`)에 보존됩니다.
- `logCapacity > 0`일 때만 로그 캡처가 활성화됩니다(배포 가이드 참조). `0`이면 로그 엔드포인트는 503을 반환합니다.

### 사용 통계

"통계(Stats)" 탭에서 서비스 사용 현황 분석:

**개요**:
- 일별 및 계정별 사용량 요약
- 실패 / 스로틀 로그
- 실시간 RPM 뷰

**세분화**:
- 계정 라벨 및 클라이언트 IP 포함
- 일 단위 하위 드릴다운

### 모델 테스트

"모델 테스트(Model Test)" 탭에서 임의의 사용 가능한 모델에 중계를 통해 직접 테스트 요청을 보내고 원본 결과를 확인합니다. 계정/모델이 실제로 작동하는지 검증하는 데 유용합니다.

**사용 방법**:
1. 모델 선택(선택적으로 엔드포인트 지정)
2. "전송" 클릭 → 중계를 통해 요청을 보내고 원본 응답 표시

**API 키 처리**:
- 요청은 발급받은 API-KEY 중 하나로 중계 엔드포인트를 호출합니다.
- **아직 커스텀 key를 하나도 발급하지 않았다면 마스터 API 키(`adminApiKey`/`apiKey`)로 폴백**하므로 별도 설정 없이 바로 테스트할 수 있습니다.
- 이 key는 브라우저(localStorage)에만 저장되며, 중계 엔드포인트 호출에만 사용됩니다.

### API Key 관리

"API 키" 탭에서 호출자에게 발급하는 대외 API Key 중앙 관리:

**Key 추가**:
1. "Key 추가" 버튼 클릭
2. 지출 한도 / 만료 설정
3. 라벨 입력
4. "추가" 클릭

**Key 관리**:
- 각 Key의 상태 표시 (활성/비활성)
- "활성화/비활성화" 토글로 상태 변경
- 라벨 수정
- key별 사용량 확인·초기화
- 페이지 단위 사용량 기록 조회

**지출 한도의 적용 범위**:
- 지출 한도와 사용량 집계는 **4개 프로토콜 프런트엔드(Anthropic / OpenAI / OpenAI-Responses / Gemini) 전부**에서 동일하게 적용됩니다.
- 어느 엔드포인트로 호출하든 같은 key의 한도에 합산되며, 한도를 넘어서면 해당 프로토콜의 오류로 거부됩니다.

### 설정

"설정(Settings)" 탭에서 실행 중 설정 관리:

**부하 분산**:
- 회전 전략: `priority`(등가 라운드로빈) / `balanced`(가중치 기반) 런타임 전환

**인증 키**:
- `apiKey` / `adminApiKey` 교체(즉시 적용·재시작 불필요)

**통합 예시**:
- 프로토콜 × 언어별 복사 가능한 코드 조각

**서비스 제어**:
- 원클릭 서비스 재시작
- 재시작·종료 시 진행 중인 요청을 기다리는 드레인 시간은 **최대 8초**로 제한되므로, 마지막 사용량 통계 플러시가 언제나 실행됩니다
- `server-info`는 마스킹된 마스터 key와 kiro2api 버전을 표시

모든 변경사항은 즉시 적용됩니다.

### 우측 상단 제어 표시줄

- **운행 상태 배지**: 서비스 상태 표시
- **GitHub**: 저장소 링크
- **서비스 재시작**: 서비스 재시작 버튼
- **테마 전환**: 밝은 테마/어두운 테마 전환
- **다국어**: 5개 언어 전환

### 사용자 패널

**사용자 패널** — key 보유자에게 `http://서버IP:8080/user`를 전달합니다. 그들은 **자신의 API-KEY**(관리자 권한 불필요)로 로그인하여 해당 key의 할당량, 모델별로 세분화된 누적 사용량, 페이지 단위 요청 기록을 확인합니다. `/api/user/*`로 구동되며 다른 key나 관리 기능을 노출하지 않습니다.

## 이미지 업로드

kiro2api는 이미지 입력을 포함한 멀티모달 콘텐츠를 지원합니다. 3가지 API 형식의 이미지 전송을 지원합니다.

### OpenAI 형식

`messages` 배열에서 `image_url` 타입을 사용합니다. Base64 Data URI와 원격 HTTP URL을 모두 지원합니다.

**Base64 이미지 예시**:

```bash
curl -X POST http://localhost:8080/openai/v1/chat/completions \
  -H "Content-Type: application/json" \
  -H "Authorization: Bearer sk-당신의API키" \
  -d '{
    "model": "claude-sonnet-4.5",
    "messages": [
      {
        "role": "user",
        "content": [
          {"type": "text", "text": "이것은 무엇입니까"},
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

**원격 URL 이미지 예시**:

```bash
curl -X POST http://localhost:8080/openai/v1/chat/completions \
  -H "Content-Type: application/json" \
  -H "Authorization: Bearer sk-당신의API키" \
  -d '{
    "model": "claude-sonnet-4.5",
    "messages": [
      {
        "role": "user",
        "content": [
          {"type": "text", "text": "이 이미지를 분석하세요"},
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

### Claude 형식

`content` 배열에서 `image` 타입을 사용합니다.

```bash
curl -X POST http://localhost:8080/claude/v1/messages \
  -H "Content-Type: application/json" \
  -H "Authorization: Bearer sk-당신의API키" \
  -d '{
    "model": "claude-sonnet-4.5",
    "max_tokens": 1024,
    "messages": [
      {
        "role": "user",
        "content": [
          {"type": "text", "text": "이것은 무엇입니까"},
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

### Gemini 네이티브 형식

`parts` 배열에서 `inlineData`를 사용합니다.

```bash
curl -X POST http://localhost:8080/gemini/v1beta/models/claude-sonnet-4.5:generateContent \
  -H "Content-Type: application/json" \
  -H "Authorization: Bearer sk-당신의API키" \
  -d '{
    "contents": [
      {
        "parts": [
          {"text": "이것은 무엇입니까"},
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

## 함수 호출(도구)

kiro2api는 4가지 프로토콜 전반에서 함수 호출(도구)을 **진짜 그대로 전달(true passthrough)**합니다(Anthropic `tool_use` / OpenAI `tool_calls` / Gemini `functionCall`). 시뮬레이션하지 않습니다.

```bash
curl -X POST http://localhost:8080/openai/v1/chat/completions \
  -H "Content-Type: application/json" \
  -H "Authorization: Bearer sk-당신의API키" \
  -d '{
    "model": "claude-sonnet-4.5",
    "messages": [
      {"role": "user", "content": "베이징의 오늘 날씨는 어떤가요"}
    ],
    "tools": [
      {
        "type": "function",
        "function": {
          "name": "get_weather",
          "description": "지정한 도시의 날씨를 가져옵니다",
          "parameters": {
            "type": "object",
            "properties": {"city": {"type": "string"}},
            "required": ["city"]
          }
        }
      }
    ]
  }'
```

모델이 도구를 호출하기로 결정하면 `choices[0].message.tool_calls`(OpenAI) 또는 해당 프로토콜의 대응 형식으로 반환됩니다.

## 지원 모델

kiro2api의 백엔드는 Kiro(CodeWhisperer) 계정 풀입니다. **사용 가능한 모델은 계정 구독 등급에 따라 결정됩니다.** 무료 등급(KIRO FREE)은 일반적으로 `claude-sonnet-4.5`만 인가하며, opus/GPT 등은 더 높은 등급이 필요합니다.

| 모델 ID | 설명 |
|--------|------|
| `claude-sonnet-4.5` | Claude 계열 모델, 무료 등급에서도 일반적으로 사용 가능 |

**모델명 매핑**: 클라이언트가 전달한 모델명은 **소문자 부분 문자열** 매칭으로 Kiro 내부 모델에 해석됩니다. 매칭되지 않으면 `400`(`INVALID_MODEL_ID`)을 명확히 반환하며, 무작정 재시도하거나 계정에 피해를 주지 않습니다.

**목록 후 사용 권장**: `GET /v1/models`(또는 `/claude/v1/models`, `/v1beta/models`)로 본 서비스가 실제로 서빙 가능한 모델 id를 먼저 조회한 뒤 사용하는 것을 권장합니다.

## 서드파티 클라이언트 연동

base URL은 **표준 베어 프리픽스**를 사용합니다: OpenAI = `{host}/v1`, Anthropic = `{host}`(SDK가 자동으로 `/v1/messages` 보완), Gemini = `{host}/v1beta`. 명시적 벤더 프리픽스 `/openai/v1`, `/claude/v1`, `/gemini/v1beta`도 사용할 수 있습니다.

### ChatGPT-Next-Web

1. 설정 열기
2. "API 설정" 섹션에서:
   - **API URL**: `http://서버IP:8080/openai/v1`
   - **API Key**: `sk-당신의API키`
3. 모델 선택: `claude-sonnet-4.5` 등
4. 대화 시작

### LobeChat

1. 설정 열기
2. "모델 제공자" 섹션에서:
   - **제공자**: Custom
   - **API URL**: `http://서버IP:8080/openai/v1`
   - **API Key**: `sk-당신의API키`
3. 모델 선택
4. 대화 시작

### OpenCat

1. 설정 열기
2. "API 설정"에서:
   - **API Endpoint**: `http://서버IP:8080/openai/v1`
   - **API Key**: `sk-당신의API키`
3. 모델 선택
4. 대화 시작

### 일반 OpenAI 호환 클라이언트

모든 OpenAI 호환 클라이언트에서:

```
API URL: http://서버IP:8080/openai/v1
API Key: sk-당신의API키
```

## 토큰 자가 치유

kiro2api는 Kiro 자격 증명의 토큰 수명 주기를 자동으로 관리합니다. 토큰을 수동으로 갱신할 필요가 없습니다.

### 자동 갱신

토큰이 만료되면 서비스가 자동으로 처리:
- **메모리 내 자동 갱신**: 토큰 만료 시 자동으로 갱신(single-flight 조정으로 동시 갱신에 의한 401 캐스케이드 방지)
- **원자적 디스크 쓰기**: 갱신에 성공하면 `credentials.json`에 원자적으로 기록
- **차등 처리**: 연속 실패는 카테고리별(영구 무효 / 모호한 인증 / 할당량 / 일시적)로 차등 처리하며, 진짜 자격 증명 무효만 영구 비활성화하고 할당량/리스크 관리/속도 제한은 모두 냉각 후 자가 치유

### 엔드포인트 폴백과 계정 간 재시도

- **엔드포인트 폴백**: Kiro IDE → CodeWhisperer → AmazonQ 순으로 다중 엔드포인트를 폴백하며, `429`/네트워크 오류 시 자동 전환
- **계정 간 재시도**: 계정 수준 실패 시 자동으로 다른 계정으로 재시도
- **결정적 오류는 재시도 안 함**: 지원되지 않는 모델(`INVALID_MODEL_ID`) 같은 결정적 요청 오류는 무작정 재시도하거나 계정에 피해를 주지 않고, 업스트림 원인을 그대로 클라이언트에 반환

## 다국어 전환

우측 상단에서 언어를 선택:

- 简体中文 (중국어 간체)
- 繁體中文 (중국어 번체)
- English (영어)
- 日本語 (일본어)
- 한국어 (한국어)

모든 페이지가 선택한 언어로 즉시 전환됩니다.

## 대화 컨텍스트 관리

### 자동 관리 (권장)

클라이언트가 messages 배열 히스토리 자동 관리:

```python
from openai import OpenAI

client = OpenAI(
    api_key="sk-당신의API키",
    base_url="http://localhost:8080/v1"
)

messages = []

# 첫 번째 메시지
messages.append({"role": "user", "content": "안녕하세요"})
response = client.chat.completions.create(
    model="claude-sonnet-4.5",
    messages=messages
)
messages.append({"role": "assistant", "content": response.choices[0].message.content})

# 두 번째 메시지 (컨텍스트 유지)
messages.append({"role": "user", "content": "이전 대화 기억하세요?"})
response = client.chat.completions.create(
    model="claude-sonnet-4.5",
    messages=messages
)
```

### OpenAI Responses 사용 시 주의

OpenAI Responses 엔드포인트(`/v1/responses`)에서는 `previous_response_id`가 지원되지 않습니다(400 반환). 본 서비스는 서버 측 세션 기억이 없으므로, 매 요청마다 전체 대화 이력을 함께 전달해야 합니다.

## 성능 최적화

### 회전 전략 선택

**priority** (기본, 등가 라운드로빈):
- 모든 계정을 순차적으로 등가 사용
- 부하 균등 분산
- 단일 계정 과부하 방지

**balanced** (가중치 기반):
- 계정별 `weight`에 따라 가중 분배
- 특정 계정에 더 많은/적은 트래픽을 배정
- 불균형한 계정 용량에 유용

부하 분산 모드는 관리 패널 "설정"에서 런타임으로 전환할 수 있습니다.

### 계정당 속도 제한

`MAX_RPM_PER_CREDENTIAL`로 계정당 분당 요청 상한을 설정:
- 기본값: `0`(무제한)
- 높을수록: 처리량 증가, 리스크 관리 위험 증가
- 낮을수록: 안정성 증가, 처리량 감소

각 계정은 독립적인 RPM 제한과 차등 냉각을 가집니다.

### 냉각과 폴백

- 연속 실패한 계정은 카테고리별로 차등 냉각되어 자동으로 자가 치유됩니다.
- 엔드포인트 폴백(Kiro/CodeWhisperer/AmazonQ)과 계정 간 재시도로 가용성을 높입니다.

## 문제 해결

### 401 Unauthorized

**원인**: API Key 오류 또는 누락

**해결책**:
1. API Key 확인 (로그 또는 `.env` 파일)
2. 요청 헤더에 `Authorization: Bearer sk-...` 포함 확인
3. 또는 `x-api-key: sk-...` 헤더, 혹은 `?token=sk-...` 쿼리 사용

### 400 INVALID_MODEL_ID

**원인**: 요청한 모델이 계정 구독 등급에서 인가되지 않았거나, 모델명이 내부 모델에 매칭되지 않음

**해결책**:
1. `GET /v1/models`로 실제 사용 가능한 모델 id 확인
2. 무료 등급(KIRO FREE)은 일반적으로 `claude-sonnet-4.5`만 인가됨
3. opus/GPT 등이 필요하면 더 높은 구독 등급의 계정 추가

### 사용 가능한 계정 없음

**원인**: 모든 계정이 냉각 중이거나 비활성화됨

**해결책**:
1. 관리 패널 "자격 증명" 탭에서 계정 상태 확인
2. 냉각 중인 계정은 자동으로 자가 치유될 때까지 대기
3. 실패 카운터 초기화 또는 새 계정 추가

### 응답 시간 초과

**원인**: 네트워크 지연 또는 업스트림 응답 지연

**해결책**:
1. 배포 서버가 AWS CodeWhisperer/Kiro 엔드포인트(`*.amazonaws.com`)에 접근 가능한지 확인
2. 요청 타임아웃 값 증가
3. 계정당 RPM 제한 조정

### 로그가 표시되지 않음

**원인**: `logCapacity`가 `0`으로 설정되어 로그 캡처가 비활성화됨

**해결책**:
1. `config.json`에서 `logCapacity`를 `> 0`(예: `1000`)으로 설정
2. 서비스 재시작 후 관리 패널 "로그" 탭에서 확인

---

더 자세한 내용은 [API 레퍼런스](API.md), [배포 가이드](DEPLOY.md), [프로젝트 README](../../README.md)를 참조하십시오.
