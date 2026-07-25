import { useState } from 'react'
import { toast } from 'sonner'
import { Loader2, ExternalLink } from 'lucide-react'
import {
  Dialog,
  DialogContent,
  DialogHeader,
  DialogTitle,
  DialogDescription,
  DialogFooter,
} from '@/components/ui/dialog'
import { Button } from '@/components/ui/button'
import { Input } from '@/components/ui/input'
import { useStartIamSsoLogin, useCompleteIamSsoLogin } from '@/hooks/use-credentials'
import { extractErrorMessage } from '@/lib/utils'

interface IamSsoLoginDialogProps {
  open: boolean
  onOpenChange: (open: boolean) => void
}

type Phase = 'form' | 'authorize'

export function IamSsoLoginDialog({ open, onOpenChange }: IamSsoLoginDialogProps) {
  const [startUrl, setStartUrl] = useState('')
  const [region, setRegion] = useState('us-east-1')
  const [phase, setPhase] = useState<Phase>('form')
  const [sessionId, setSessionId] = useState('')
  const [authorizeUrl, setAuthorizeUrl] = useState('')
  const [callbackUrl, setCallbackUrl] = useState('')

  const { mutateAsync: startLogin, isPending: starting } = useStartIamSsoLogin()
  const { mutateAsync: completeLogin, isPending: completing } = useCompleteIamSsoLogin()

  const reset = () => {
    setStartUrl('')
    setRegion('us-east-1')
    setPhase('form')
    setSessionId('')
    setAuthorizeUrl('')
    setCallbackUrl('')
  }

  const handleStart = async () => {
    if (!startUrl.trim()) {
      toast.error('请输入 Start URL')
      return
    }
    if (!region.trim()) {
      toast.error('请输入区域')
      return
    }
    try {
      const res = await startLogin({ startUrl: startUrl.trim(), region: region.trim() })
      setSessionId(res.sessionId)
      setAuthorizeUrl(res.authorizeUrl)
      setPhase('authorize')
    } catch (error) {
      toast.error(`启动失败: ${extractErrorMessage(error)}`)
    }
  }

  const handleComplete = async () => {
    if (!callbackUrl.trim()) {
      toast.error('请粘贴回调 URL')
      return
    }
    try {
      const res = await completeLogin({ sessionId, callbackUrl: callbackUrl.trim() })
      toast.success(res.message || `IAM SSO 登录成功${res.email ? `：${res.email}` : ''}`)
      reset()
      onOpenChange(false)
    } catch (error) {
      toast.error(`完成失败: ${extractErrorMessage(error)}`)
    }
  }

  const busy = starting || completing

  return (
    <Dialog
      open={open}
      onOpenChange={(newOpen) => {
        if (!newOpen && !busy) reset()
        onOpenChange(newOpen)
      }}
    >
      <DialogContent className="sm:max-w-lg">
        <DialogHeader>
          <DialogTitle>IAM Identity Center 登录</DialogTitle>
          <DialogDescription>
            输入企业 Start URL 与区域，打开授权链接，批准后把浏览器地址栏里的回调 URL 粘回来。
          </DialogDescription>
        </DialogHeader>

        <div className="space-y-4 py-2">
          <div className="space-y-2">
            <label htmlFor="sso-start-url" className="text-sm font-medium">
              Start URL <span className="text-red-500">*</span>
            </label>
            <Input
              id="sso-start-url"
              placeholder="https://your-org.awsapps.com/start"
              value={startUrl}
              onChange={(e) => setStartUrl(e.target.value)}
              disabled={phase === 'authorize' || busy}
            />
          </div>
          <div className="space-y-2">
            <label htmlFor="sso-region" className="text-sm font-medium">
              区域 <span className="text-red-500">*</span>
            </label>
            <Input
              id="sso-region"
              placeholder="us-east-1"
              value={region}
              onChange={(e) => setRegion(e.target.value)}
              disabled={phase === 'authorize' || busy}
            />
          </div>

          {phase === 'authorize' && (
            <div className="space-y-3 rounded-md border p-3">
              <a
                href={authorizeUrl}
                target="_blank"
                rel="noopener noreferrer"
                className="inline-flex items-center gap-2 text-sm text-primary underline-offset-4 hover:underline break-all"
              >
                <ExternalLink className="h-4 w-4 shrink-0" />
                打开授权页面
              </a>
              <div className="space-y-2">
                <label htmlFor="sso-callback" className="text-sm font-medium">
                  回调 URL <span className="text-red-500">*</span>
                </label>
                <Input
                  id="sso-callback"
                  placeholder="http://127.0.0.1/oauth/callback?code=...&state=..."
                  value={callbackUrl}
                  onChange={(e) => setCallbackUrl(e.target.value)}
                  disabled={completing}
                />
                <p className="text-xs text-muted-foreground">
                  批准后浏览器会跳转到 127.0.0.1（可能显示无法访问，属正常），复制该地址栏完整 URL 粘贴到此处。
                </p>
              </div>
            </div>
          )}
        </div>

        <DialogFooter>
          <Button
            type="button"
            variant="outline"
            onClick={() => { if (!busy) { reset(); onOpenChange(false) } }}
            disabled={busy}
          >
            取消
          </Button>
          {phase === 'form' ? (
            <Button type="button" onClick={handleStart} disabled={starting}>
              {starting ? <><Loader2 className="h-4 w-4 mr-2 animate-spin" />启动中…</> : '获取授权链接'}
            </Button>
          ) : (
            <Button type="button" onClick={handleComplete} disabled={completing}>
              {completing ? <><Loader2 className="h-4 w-4 mr-2 animate-spin" />完成中…</> : '完成登录'}
            </Button>
          )}
        </DialogFooter>
      </DialogContent>
    </Dialog>
  )
}
