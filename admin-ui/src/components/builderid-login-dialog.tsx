import { useState, useEffect, useRef, useCallback } from 'react'
import { toast } from 'sonner'
import { Loader2, Copy, ExternalLink, CheckCircle2 } from 'lucide-react'
import {
  Dialog,
  DialogContent,
  DialogHeader,
  DialogTitle,
  DialogDescription,
  DialogFooter,
} from '@/components/ui/dialog'
import { Button } from '@/components/ui/button'
import { useStartBuilderIdLogin, usePollBuilderIdLogin } from '@/hooks/use-credentials'
import { extractErrorMessage } from '@/lib/utils'
import type { BuilderIdStartResponse } from '@/types/api'

interface BuilderIdLoginDialogProps {
  open: boolean
  onOpenChange: (open: boolean) => void
}

const REGIONS = ['us-east-1', 'us-west-2', 'eu-central-1', 'ap-southeast-1']

type Phase = 'idle' | 'waiting'

export function BuilderIdLoginDialog({ open, onOpenChange }: BuilderIdLoginDialogProps) {
  const [region, setRegion] = useState('us-east-1')
  const [phase, setPhase] = useState<Phase>('idle')
  const [session, setSession] = useState<BuilderIdStartResponse | null>(null)

  const { mutateAsync: startLogin, isPending: starting } = useStartBuilderIdLogin()
  const { mutateAsync: pollLogin } = usePollBuilderIdLogin()

  // 可变轮询间隔（slow_down 时增大），单位毫秒
  const intervalRef = useRef(5000)
  const pollTimer = useRef<ReturnType<typeof setTimeout> | null>(null)
  const activeRef = useRef(false)

  const cleanup = useCallback(() => {
    activeRef.current = false
    if (pollTimer.current) { clearTimeout(pollTimer.current); pollTimer.current = null }
  }, [])

  const reset = useCallback(() => {
    cleanup()
    setPhase('idle')
    setSession(null)
    intervalRef.current = 5000
  }, [cleanup])

  // 组件卸载或对话框关闭时停止定时器
  useEffect(() => cleanup, [cleanup])

  const scheduleNextPoll = useCallback((sessionId: string) => {
    if (!activeRef.current) return
    pollTimer.current = setTimeout(async () => {
      if (!activeRef.current) return
      try {
        const res = await pollLogin({ sessionId })
        if (!activeRef.current) return
        if (res.status === 'completed') {
          cleanup()
          setPhase('idle')
          setSession(null)
          toast.success(res.email ? `Builder-ID 登录成功：${res.email}` : 'Builder-ID 登录成功')
          onOpenChange(false)
          return
        }
        if (res.status === 'slow_down') {
          const bumpMs = (res.interval ?? Math.round(intervalRef.current / 1000)) * 1000
          intervalRef.current = Math.max(intervalRef.current + 5000, bumpMs)
        }
        scheduleNextPoll(sessionId)
      } catch (error) {
        cleanup()
        setPhase('idle')
        setSession(null)
        toast.error(`轮询失败: ${extractErrorMessage(error)}`)
      }
    }, intervalRef.current)
  }, [pollLogin, cleanup, onOpenChange])

  const handleStart = async () => {
    try {
      const res = await startLogin({ region })
      setSession(res)
      setPhase('waiting')
      intervalRef.current = Math.max(1000, res.interval * 1000)
      activeRef.current = true

      scheduleNextPoll(res.sessionId)
    } catch (error) {
      toast.error(`启动失败: ${extractErrorMessage(error)}`)
    }
  }

  const copyCode = () => {
    if (session?.userCode) {
      navigator.clipboard.writeText(session.userCode).then(
        () => toast.success('已复制授权码'),
        () => toast.error('复制失败，请手动复制'),
      )
    }
  }

  return (
    <Dialog
      open={open}
      onOpenChange={(newOpen) => {
        if (!newOpen) reset()
        onOpenChange(newOpen)
      }}
    >
      <DialogContent className="sm:max-w-md">
        <DialogHeader>
          <DialogTitle>Builder-ID 登录（设备码）</DialogTitle>
          <DialogDescription>
            选择区域后打开授权链接，输入授权码完成登录，登录成功后自动落库。
          </DialogDescription>
        </DialogHeader>

        <div className="space-y-4 py-2">
          <div className="space-y-2">
            <label htmlFor="bid-region" className="text-sm font-medium">区域</label>
            <select
              id="bid-region"
              value={region}
              onChange={(e) => setRegion(e.target.value)}
              disabled={phase === 'waiting' || starting}
              className="flex h-10 w-full rounded-md border border-input bg-background px-3 py-2 text-sm ring-offset-background focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring focus-visible:ring-offset-2 disabled:cursor-not-allowed disabled:opacity-50"
            >
              {REGIONS.map((r) => <option key={r} value={r}>{r}</option>)}
            </select>
          </div>

          {phase === 'waiting' && session && (
            <div className="space-y-3 rounded-md border p-3">
              <div className="space-y-1">
                <div className="text-sm font-medium">授权码</div>
                <div className="flex items-center gap-2">
                  <code className="flex-1 rounded bg-muted px-2 py-1 font-mono text-lg tracking-widest">
                    {session.userCode}
                  </code>
                  <Button type="button" size="icon" variant="outline" onClick={copyCode} title="复制">
                    <Copy className="h-4 w-4" />
                  </Button>
                </div>
              </div>
              <a
                href={session.verificationUri}
                target="_blank"
                rel="noopener noreferrer"
                className="inline-flex items-center gap-2 text-sm text-primary underline-offset-4 hover:underline"
              >
                <ExternalLink className="h-4 w-4" />
                打开授权页面
              </a>
              <div className="flex items-center gap-2 text-sm text-muted-foreground">
                <Loader2 className="h-4 w-4 animate-spin" />
                等待授权中…
              </div>
            </div>
          )}
        </div>

        <DialogFooter>
          <Button
            type="button"
            variant="outline"
            onClick={() => { reset(); onOpenChange(false) }}
          >
            {phase === 'waiting' ? '取消' : '关闭'}
          </Button>
          {phase === 'idle' && (
            <Button type="button" onClick={handleStart} disabled={starting}>
              {starting ? <><Loader2 className="h-4 w-4 mr-2 animate-spin" />启动中…</> : <><CheckCircle2 className="h-4 w-4 mr-2" />开始登录</>}
            </Button>
          )}
        </DialogFooter>
      </DialogContent>
    </Dialog>
  )
}
