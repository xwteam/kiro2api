import { useState } from 'react'
import { useQuery } from '@tanstack/react-query'
import {
  LogOut, RefreshCw, Activity, Zap, DollarSign, Clock,
  ArrowUpFromLine, ArrowDownToLine, FileText,
} from 'lucide-react'
import { getUsage } from '@/api/user'
import { storage } from '@/lib/storage'
import { Card, CardContent, CardHeader, CardTitle } from '@/components/ui/card'
import { Button } from '@/components/ui/button'
import { Badge } from '@/components/ui/badge'
import { Progress } from '@/components/ui/progress'
import { UsageLogPage } from '@/components/usage-log-page'

interface DashboardProps {
  onLogout: () => void
}

export function Dashboard({ onLogout }: DashboardProps) {
  const [showLog, setShowLog] = useState(false)
  const { data, isLoading, isError, refetch, isRefetching } = useQuery({
    queryKey: ['usage'],
    queryFn: getUsage,
    refetchInterval: 30000,
  })

  const handleLogout = () => {
    storage.removeApiKey()
    onLogout()
  }

  const formatTokens = (n: number) => n.toLocaleString('zh-CN')

  const formatCost = (n: number) => `$${n.toFixed(4)}`

  const formatDate = (iso: string | null) => {
    if (!iso) return null
    const d = new Date(iso)
    return d.toLocaleString('zh-CN', {
      year: 'numeric', month: '2-digit', day: '2-digit',
      hour: '2-digit', minute: '2-digit',
    })
  }

  const isCredits = data?.limitUnit === 'credits'
  const usedAmount = isCredits ? data?.totalCredits ?? 0 : data?.totalCost ?? 0

  // 「有没有设上限」必须用 != null 判断，不能用真值判断。
  // 后端把「不限额」表示为 null，而 spendingLimit = 0 是一种真实配置（冻结这把 key）：
  // 鉴权闸对 Some(0.0) 会把每一发请求都以 402 拒掉。旧写法 `data.spendingLimit &&`
  // 让 0 和 null 无法区分 —— 额度条整块不渲染、状态还是绿色「正常」，用户看到一把
  // 健康的 key，实际 100% 的调用都被拒绝。
  const hasSpendingLimit = data?.spendingLimit != null
  const spendingLimit = data?.spendingLimit ?? 0

  const getStatusBadge = () => {
    // 轮询失败时 react-query 会保留上一次成功的 data，此时继续显示绿色「正常」等于
    // 给用户一块过期的牌子。401（key 失效）由 api 层统一踢回登录页，这里兜住其余错误
    // （断网、后端 5xx 等），至少让用户知道这些数字已经不是实时的。
    if (isError) return <Badge variant="warning">连接异常</Badge>
    if (!data) return null
    if (data.expiresAt) {
      const expired = new Date(data.expiresAt) < new Date()
      if (expired) return <Badge variant="destructive">已过期</Badge>
    }
    if (hasSpendingLimit && usedAmount >= spendingLimit) {
      return <Badge variant="destructive">额度已用完</Badge>
    }
    return <Badge variant="success">正常</Badge>
  }

  const spendingPercent = hasSpendingLimit
    // 上限为 0 时 used/limit 会算出 NaN(0/0) 或 Infinity，Progress 会画歪，直接按 100% 处理。
    ? spendingLimit > 0
      ? Math.min((usedAmount / spendingLimit) * 100, 100)
      : 100
    : null

  const formatLimitAmount = (n: number) => (isCredits ? n.toFixed(2) : formatCost(n))

  // durationDays 是小数天（后端允许 0.25 = 6 小时这类「时卡」），按量级换单位，
  // 免得界面上出现「0.25 天」。
  const formatDuration = (days: number) => {
    if (days <= 0) return '立即到期'
    if (days >= 1) return `${Number(days.toFixed(2))} 天`
    const hours = days * 24
    if (hours >= 1) return `${Number(hours.toFixed(1))} 小时`
    return `${Math.round(hours * 60)} 分钟`
  }

  if (isLoading) {
    return (
      <div className="min-h-screen flex items-center justify-center bg-background">
        <RefreshCw className="h-8 w-8 animate-spin text-muted-foreground" />
      </div>
    )
  }

  if (showLog) {
    return <UsageLogPage onBack={() => setShowLog(false)} />
  }

  return (
    <div className="min-h-screen bg-background">
      {/* Header */}
      <header className="border-b">
        <div className="max-w-4xl mx-auto px-4 py-4 flex items-center justify-between">
          <div className="flex items-center gap-3">
            <h1 className="text-xl font-semibold">额度用量监控</h1>
            {getStatusBadge()}
          </div>
          <div className="flex items-center gap-2">
            <Button
              variant="ghost"
              size="sm"
              onClick={() => setShowLog(true)}
            >
              <FileText className="h-4 w-4 mr-1" />
              查看日志
            </Button>
            <Button
              variant="ghost"
              size="icon"
              onClick={() => refetch()}
              disabled={isRefetching}
              aria-label="刷新"
            >
              <RefreshCw className={`h-4 w-4 ${isRefetching ? 'animate-spin' : ''}`} />
            </Button>
            <Button variant="ghost" size="sm" onClick={handleLogout}>
              <LogOut className="h-4 w-4 mr-1" />
              退出
            </Button>
          </div>
        </div>
      </header>

      <main className="max-w-4xl mx-auto px-4 py-6 space-y-6">
        {/* 首次加载就失败时 data 是 undefined，下面所有卡片都不渲染 —— 不给提示的话
            用户看到的是一片空白，还以为面板坏了。 */}
        {isError && !data && (
          <Card>
            <CardContent className="py-12 text-center text-muted-foreground space-y-3">
              <div>无法获取用量数据，请检查网络后重试</div>
              <Button variant="outline" size="sm" onClick={() => refetch()} disabled={isRefetching}>
                重试
              </Button>
            </CardContent>
          </Card>
        )}

        {/* Key 信息 */}
        {data && (
          <Card>
            <CardHeader className="pb-3">
              <CardTitle className="text-base font-medium flex items-center gap-2">
                <Zap className="h-4 w-4" />
                {data.name}
              </CardTitle>
            </CardHeader>
            <CardContent className="space-y-3">
              <div className="grid grid-cols-2 gap-4 text-sm">
                {data.activatedAt && (
                  <div className="flex items-center gap-2 text-muted-foreground">
                    <Clock className="h-3.5 w-3.5" />
                    激活时间: {formatDate(data.activatedAt)}
                  </div>
                )}
                {data.expiresAt && (
                  <div className="flex items-center gap-2 text-muted-foreground">
                    <Clock className="h-3.5 w-3.5" />
                    到期时间: {formatDate(data.expiresAt)}
                  </div>
                )}
                {/* 惰性「N 天卡」在第一次调用之前 activatedAt / expiresAt 都是 null，
                    只有 durationDays 有值。旧版这两行都不渲染、durationDays 又从没被用过，
                    于是买家打开面板只看得到一个名字，完全不知道自己买的卡有多久、
                    什么时候开始计时。 */}
                {data.durationDays != null && !data.activatedAt && (
                  <div className="col-span-2 flex items-center gap-2 text-muted-foreground">
                    <Clock className="h-3.5 w-3.5 shrink-0" />
                    有效期: {formatDuration(data.durationDays)}（首次调用后开始计时）
                  </div>
                )}
              </div>
              {/* 额度进度条：用 hasSpendingLimit 判断，spendingLimit=0（冻结）也要画出来 */}
              {hasSpendingLimit && spendingPercent !== null && (
                <div className="space-y-1.5">
                  <div className="flex justify-between text-sm">
                    <span className="text-muted-foreground">
                      额度使用{isCredits ? '（credits）' : ''}
                    </span>
                    <span>{formatLimitAmount(usedAmount)} / {formatLimitAmount(spendingLimit)}</span>
                  </div>
                  <Progress value={spendingPercent} />
                </div>
              )}
            </CardContent>
          </Card>
        )}

        {/* 用量概览 */}
        {data && (
          <div className="grid grid-cols-2 md:grid-cols-4 gap-4">
            <Card>
              <CardContent className="pt-6">
                <div className="flex items-center gap-2 text-muted-foreground text-sm mb-1">
                  <Activity className="h-3.5 w-3.5" />
                  总请求数
                </div>
                <div className="text-2xl font-bold">{data.totalRequests.toLocaleString()}</div>
              </CardContent>
            </Card>
            <Card>
              <CardContent className="pt-6">
                <div className="flex items-center gap-2 text-muted-foreground text-sm mb-1">
                  <ArrowUpFromLine className="h-3.5 w-3.5" />
                  输入 Tokens
                </div>
                <div className="text-2xl font-bold">{formatTokens(data.totalInputTokens)}</div>
              </CardContent>
            </Card>
            <Card>
              <CardContent className="pt-6">
                <div className="flex items-center gap-2 text-muted-foreground text-sm mb-1">
                  <ArrowDownToLine className="h-3.5 w-3.5" />
                  输出 Tokens
                </div>
                <div className="text-2xl font-bold">{formatTokens(data.totalOutputTokens)}</div>
              </CardContent>
            </Card>
            <Card>
              <CardContent className="pt-6">
                <div className="flex items-center gap-2 text-muted-foreground text-sm mb-1">
                  <DollarSign className="h-3.5 w-3.5" />
                  总费用
                </div>
                <div className="text-2xl font-bold">{formatCost(data.totalCost)}</div>
              </CardContent>
            </Card>
          </div>
        )}

        {/* 按模型分组 */}
        {data && data.byModel.length > 0 && (
          <Card>
            <CardHeader>
              <CardTitle className="text-base font-medium">按模型分组</CardTitle>
            </CardHeader>
            <CardContent>
              <div className="space-y-4">
                {data.byModel.map((m) => (
                  <div key={m.model} className="flex items-center justify-between py-2 border-b last:border-0">
                    <div>
                      <div className="font-medium text-sm">{m.model}</div>
                      <div className="text-xs text-muted-foreground mt-0.5">
                        {m.requests} 次请求
                      </div>
                    </div>
                    <div className="text-right">
                      <div className="text-sm font-medium">{formatCost(m.cost)}</div>
                      <div className="text-xs text-muted-foreground mt-0.5">
                        {formatTokens(m.inputTokens)} in / {formatTokens(m.outputTokens)} out
                      </div>
                    </div>
                  </div>
                ))}
              </div>
            </CardContent>
          </Card>
        )}

        {/* 无数据提示 */}
        {data && data.totalRequests === 0 && (
          <Card>
            <CardContent className="py-12 text-center text-muted-foreground">
              暂无用量数据
            </CardContent>
          </Card>
        )}
      </main>
    </div>
  )
}
