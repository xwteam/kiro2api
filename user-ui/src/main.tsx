import React from 'react'
import ReactDOM from 'react-dom/client'
import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import App from './App'
import { isUnauthorizedError } from '@/api/user'
import './index.css'

const queryClient = new QueryClient({
  defaultOptions: {
    queries: {
      staleTime: 5000,
      refetchOnWindowFocus: false,
      // 401（key 已失效）不重试：重试改变不了结果，只会让「已失效」的提示重复触发，
      // 还要白等几秒退避才把用户踢回登录页。其余错误保持默认的 3 次重试。
      retry: (failureCount, error) => !isUnauthorizedError(error) && failureCount < 3,
    },
  },
})

ReactDOM.createRoot(document.getElementById('root')!).render(
  <React.StrictMode>
    <QueryClientProvider client={queryClient}>
      <App />
    </QueryClientProvider>
  </React.StrictMode>,
)
