export interface LoginRequest {
  apiKey: string
}

export interface LoginResponse {
  id: number
  name: string
  spendingLimit: number | null
  limitUnit: 'usd' | 'credits'
  totalCost: number
  totalCredits: number
  expiresAt: string | null
  durationDays: number | null
  activatedAt: string | null
  // 后端按鉴权闸的准入算式给出的「下一发请求会不会被 402 拒掉」。
  // 可选是为了兼容还没带这个字段的老后端（前端会退回旧判据，见 @/lib/spending）。
  exhausted?: boolean
}

export interface ModelUsage {
  model: string
  requests: number
  inputTokens: number
  outputTokens: number
  cost: number
}

export interface UsageResponse {
  id: number
  name: string
  spendingLimit: number | null
  limitUnit: 'usd' | 'credits'
  expiresAt: string | null
  durationDays: number | null
  activatedAt: string | null
  totalRequests: number
  totalInputTokens: number
  totalOutputTokens: number
  totalCost: number
  totalCredits: number
  byModel: ModelUsage[]
  // 同 LoginResponse.exhausted：额度状态以后端的准入口径为准。
  exhausted?: boolean
}

export interface UsageRecordItem {
  model: string
  inputTokens: number
  outputTokens: number
  estimatedCost: number
  creditsUsed?: number
  creditsSaved?: number
  cacheReadInputTokens?: number
  cacheCreationInputTokens?: number
  createdAt: string
  clientIp?: string
  credentialLabel?: string
}

export interface UsageRecordsPage {
  records: UsageRecordItem[]
  total: number
  page: number
  pageSize: number
  totalPages: number
}
