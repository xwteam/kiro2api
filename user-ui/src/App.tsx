import { useState, useEffect } from 'react'
import { useQueryClient } from '@tanstack/react-query'
import { toast } from 'sonner'
import { storage } from '@/lib/storage'
import { login, setUnauthorizedHandler } from '@/api/user'
import { LoginPage } from '@/components/login-page'
import { Dashboard } from '@/components/dashboard'
import { Toaster } from '@/components/ui/sonner'
import type { LoginResponse } from '@/types/api'

function App() {
  const queryClient = useQueryClient()
  const [isLoggedIn, setIsLoggedIn] = useState(false)
  const [checking, setChecking] = useState(true)

  useEffect(() => {
    // 检查已保存的 API Key 是否仍然有效
    const savedKey = storage.getApiKey()
    if (savedKey) {
      login(savedKey)
        .then(() => setIsLoggedIn(true))
        .catch(() => storage.removeApiKey())
        .finally(() => setChecking(false))
    } else {
      setChecking(false)
    }
  }, [])

  useEffect(() => {
    // 任何非登录接口收到 401 = 这把 key 已经失效（被停用 / 到期 / 被删）。
    // 网络层已经清掉本地 key（见 api/user.ts 的响应拦截器），这里负责回到登录页并
    // 说明原因 —— 否则面板会拿着上一次成功的数据继续显示绿色「正常」，而用户的每
    // 一次 API 调用其实都在被拒绝。toast 固定 id 是为了多个查询同时 401 时只弹一条。
    setUnauthorizedHandler((message) => {
      setIsLoggedIn(false)
      toast.error(message, { id: 'user-unauthorized' })
    })
    return () => setUnauthorizedHandler(null)
  }, [])

  useEffect(() => {
    // 退出登录（或被 401 踢下线）后清空 react-query 缓存。
    //
    // 为什么必须清：queryClient 建在模块作用域（main.tsx），默认 gcTime 5 分钟，退出
    // 登录只是删了 localStorage 里的 key，缓存原封不动。同一个浏览器里换第二把 key
    // 登录时，['usage'] / ['usageRecords',...] 直接缓存命中 —— 新用户会先看到上一把
    // key 的名字、用量，甚至请求日志里上一位用户的客户端 IP。
    //
    // 为什么放在 effect 里而不是写在 handleLogout 内：这样 clear() 发生在 Dashboard
    // 卸载之后，缓存里已经没有活跃订阅者，不会因为「查询被移除」再触发一次没带 key
    // 的请求（那一发必然 401，会紧接着再弹一条失效提示）。
    if (!isLoggedIn) queryClient.clear()
  }, [isLoggedIn, queryClient])

  const handleLogin = (_data: LoginResponse) => {
    setIsLoggedIn(true)
  }

  const handleLogout = () => {
    setIsLoggedIn(false)
  }

  if (checking) return null

  return (
    <>
      {isLoggedIn ? (
        <Dashboard onLogout={handleLogout} />
      ) : (
        <LoginPage onLogin={handleLogin} />
      )}
      <Toaster position="top-right" />
    </>
  )
}

export default App
