import axios from 'axios'
import { storage } from '@/lib/storage'
import type { LoginRequest, LoginResponse, UsageResponse, UsageRecordsPage } from '@/types/api'

const api = axios.create({
  baseURL: '/api/user',
  headers: {
    'Content-Type': 'application/json',
  },
})

// 请求拦截器添加 API Key
api.interceptors.request.use((config) => {
  const apiKey = storage.getApiKey()
  if (apiKey) {
    config.headers['x-api-key'] = apiKey
  }
  return config
})

// ==================== 401 掉线处理 ====================

type UnauthorizedHandler = (message: string) => void

let unauthorizedHandler: UnauthorizedHandler | null = null

// 注册「key 已失效」回调（由 App 注册，见 App.tsx）。
export function setUnauthorizedHandler(fn: UnauthorizedHandler | null) {
  unauthorizedHandler = fn
}

// 判断是不是 401（后端 resolve_key 对 未命中/停用/过期 一律回 401）。
export function isUnauthorizedError(error: unknown): boolean {
  return axios.isAxiosError(error) && error.response?.status === 401
}

// 响应拦截器：把 401 统一当成「这把 key 已经不能用了」处理 —— 清掉本地 key 并把
// 用户踢回登录页。和 admin-ui-v2 的 api.js（401 → setAdminKey('') + onUnauthorized）
// 是同一套路。
//
// 为什么必须有：管理员停用/删除这把 key，或者 key 到期之后，面板每 30s 的轮询会一直
// 401，而 react-query 失败时会保留上一次成功的 data。没有这层处理的话，页面会拿着
// 旧数据继续显示绿色「正常」和旧的额度条，用户完全不知道自己已经被拒绝了。
api.interceptors.response.use(
  (res) => res,
  (error: unknown) => {
    // /login 的 401 由登录页自己 toast（否则同一次失败会弹两条提示），
    // 且 App 启动时的静默校验也走 /login，自己会清 key。
    const url = axios.isAxiosError(error) ? (error.config?.url ?? '') : ''
    if (isUnauthorizedError(error) && !url.includes('/login')) {
      storage.removeApiKey()
      const body = axios.isAxiosError<{ error?: string }>(error)
        ? error.response?.data
        : undefined
      unauthorizedHandler?.(body?.error || 'API Key 已失效，请重新登录')
    }
    return Promise.reject(error)
  }
)

// 登录验证
export async function login(apiKey: string): Promise<LoginResponse> {
  const { data } = await api.post<LoginResponse>('/login', { apiKey } as LoginRequest)
  return data
}

// 获取用量数据
export async function getUsage(): Promise<UsageResponse> {
  const { data } = await api.get<UsageResponse>('/usage')
  return data
}

// 获取分页请求日志
export async function getUsageRecords(page = 1, pageSize = 50): Promise<UsageRecordsPage> {
  const { data } = await api.get<UsageRecordsPage>('/usage/records', {
    params: { page, page_size: pageSize },
  })
  return data
}
