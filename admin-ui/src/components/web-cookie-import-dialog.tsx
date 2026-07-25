import { useState } from 'react'
import { toast } from 'sonner'
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
import { useAddCredential } from '@/hooks/use-credentials'
import { extractErrorMessage } from '@/lib/utils'

interface WebCookieImportDialogProps {
  open: boolean
  onOpenChange: (open: boolean) => void
}

type WebCookieProvider = 'google' | 'github'

export function WebCookieImportDialog({ open, onOpenChange }: WebCookieImportDialogProps) {
  const [provider, setProvider] = useState<WebCookieProvider>('google')
  const [refreshToken, setRefreshToken] = useState('')
  const [email, setEmail] = useState('')
  const [nickname, setNickname] = useState('')

  const { mutate, isPending } = useAddCredential()

  const resetForm = () => {
    setProvider('google')
    setRefreshToken('')
    setEmail('')
    setNickname('')
  }

  const handleSubmit = (e: React.FormEvent) => {
    e.preventDefault()

    if (!refreshToken.trim()) {
      toast.error('请输入 RefreshToken')
      return
    }

    mutate(
      {
        refreshToken: refreshToken.trim(),
        authMethod: 'social',
        email: email.trim() || undefined,
        nickname: nickname.trim() || undefined,
      },
      {
        onSuccess: (data) => {
          toast.success(data.message)
          onOpenChange(false)
          resetForm()
        },
        onError: (error: unknown) => {
          toast.error(`添加失败: ${extractErrorMessage(error)}`)
        },
      }
    )
  }

  return (
    <Dialog
      open={open}
      onOpenChange={(newOpen) => {
        if (!newOpen && !isPending) resetForm()
        onOpenChange(newOpen)
      }}
    >
      <DialogContent className="sm:max-w-lg max-h-[85vh] flex flex-col">
        <DialogHeader>
          <DialogTitle>Web Cookie 导入</DialogTitle>
          <DialogDescription>
            登录 app.kiro.dev 后，在浏览器 DevTools → Application → Cookies 中找到
            RefreshToken cookie 并复制其值，粘贴到下方即可导入账号。
          </DialogDescription>
        </DialogHeader>

        <form onSubmit={handleSubmit} className="flex flex-col min-h-0 flex-1">
          <div className="space-y-4 py-4 overflow-y-auto flex-1 pr-1">
            {/* 登录方式 */}
            <div className="space-y-2">
              <label htmlFor="webcookie-provider" className="text-sm font-medium">
                登录方式
              </label>
              <select
                id="webcookie-provider"
                value={provider}
                onChange={(e) => setProvider(e.target.value as WebCookieProvider)}
                disabled={isPending}
                className="flex h-10 w-full rounded-md border border-input bg-background px-3 py-2 text-sm ring-offset-background focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring focus-visible:ring-offset-2 disabled:cursor-not-allowed disabled:opacity-50"
              >
                <option value="google">Google</option>
                <option value="github">GitHub</option>
              </select>
            </div>

            {/* RefreshToken */}
            <div className="space-y-2">
              <label htmlFor="webcookie-refreshtoken" className="text-sm font-medium">
                RefreshToken <span className="text-red-500">*</span>
              </label>
              <textarea
                id="webcookie-refreshtoken"
                placeholder="粘贴从 Cookies 中复制的 RefreshToken 值"
                value={refreshToken}
                onChange={(e) => setRefreshToken(e.target.value)}
                disabled={isPending}
                className="flex min-h-[120px] w-full rounded-md border border-input bg-background px-3 py-2 text-sm ring-offset-background placeholder:text-muted-foreground focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring focus-visible:ring-offset-2 disabled:cursor-not-allowed disabled:opacity-50 font-mono"
              />
              <p className="text-xs text-muted-foreground">
                步骤：浏览器打开 app.kiro.dev 并登录 → F12 打开 DevTools → Application
                （应用）标签 → Cookies → 找到 RefreshToken → 复制其 Value
              </p>
            </div>

            {/* 用户名/邮箱 */}
            <div className="space-y-2">
              <label htmlFor="webcookie-email" className="text-sm font-medium">
                用户名 / 邮箱
              </label>
              <Input
                id="webcookie-email"
                type="text"
                placeholder="请输入账号邮箱（用于标识账号，可留空）"
                value={email}
                onChange={(e) => setEmail(e.target.value)}
                disabled={isPending}
              />
            </div>

            {/* 备注昵称 */}
            <div className="space-y-2">
              <label htmlFor="webcookie-nickname" className="text-sm font-medium">
                备注昵称
              </label>
              <Input
                id="webcookie-nickname"
                type="text"
                placeholder="可选，便于识别账号"
                value={nickname}
                onChange={(e) => setNickname(e.target.value)}
                disabled={isPending}
              />
            </div>
          </div>

          <DialogFooter>
            <Button
              type="button"
              variant="outline"
              onClick={() => onOpenChange(false)}
              disabled={isPending}
            >
              取消
            </Button>
            <Button type="submit" disabled={isPending}>
              {isPending ? '添加中...' : '添加'}
            </Button>
          </DialogFooter>
        </form>
      </DialogContent>
    </Dialog>
  )
}
