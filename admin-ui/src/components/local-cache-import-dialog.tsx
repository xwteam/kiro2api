import { useRef, useState } from 'react'
import { toast } from 'sonner'
import { Upload } from 'lucide-react'
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

interface LocalCacheImportDialogProps {
  open: boolean
  onOpenChange: (open: boolean) => void
}

type LocalCacheProvider = 'builderid' | 'enterprise' | 'google' | 'github'

const NEEDS_CLIENT_CREDS: Record<LocalCacheProvider, boolean> = {
  builderid: true,
  enterprise: true,
  google: false,
  github: false,
}

function readFileAsText(file: File): Promise<string> {
  return new Promise((resolve, reject) => {
    const reader = new FileReader()
    reader.onload = (ev) => {
      const text = ev.target?.result
      if (typeof text === 'string') resolve(text)
      else reject(new Error('文件读取失败'))
    }
    reader.onerror = () => reject(new Error('文件读取失败'))
    reader.readAsText(file)
  })
}

export function LocalCacheImportDialog({ open, onOpenChange }: LocalCacheImportDialogProps) {
  const [provider, setProvider] = useState<LocalCacheProvider>('builderid')
  const [tokenJson, setTokenJson] = useState('')
  const [clientJson, setClientJson] = useState('')
  const [email, setEmail] = useState('')
  const [nickname, setNickname] = useState('')

  const tokenFileRef = useRef<HTMLInputElement>(null)
  const clientFileRef = useRef<HTMLInputElement>(null)

  const { mutate, isPending } = useAddCredential()

  const needsClientCreds = NEEDS_CLIENT_CREDS[provider]

  const resetForm = () => {
    setProvider('builderid')
    setTokenJson('')
    setClientJson('')
    setEmail('')
    setNickname('')
    if (tokenFileRef.current) tokenFileRef.current.value = ''
    if (clientFileRef.current) clientFileRef.current.value = ''
  }

  const handleTokenFile = async (e: React.ChangeEvent<HTMLInputElement>) => {
    const file = e.target.files?.[0]
    if (!file) return
    try {
      setTokenJson(await readFileAsText(file))
    } catch (error) {
      toast.error(extractErrorMessage(error))
    }
  }

  const handleClientFile = async (e: React.ChangeEvent<HTMLInputElement>) => {
    const file = e.target.files?.[0]
    if (!file) return
    try {
      setClientJson(await readFileAsText(file))
    } catch (error) {
      toast.error(extractErrorMessage(error))
    }
  }

  const handleSubmit = (e: React.FormEvent) => {
    e.preventDefault()

    if (!tokenJson.trim()) {
      toast.error('请粘贴或上传 kiro-auth-token.json 内容')
      return
    }

    let refreshToken: string | undefined
    let accessToken: string | undefined
    let region: string | undefined

    try {
      const parsed = JSON.parse(tokenJson)
      if (typeof parsed.refreshToken === 'string') refreshToken = parsed.refreshToken
      if (typeof parsed.accessToken === 'string') accessToken = parsed.accessToken
      if (typeof parsed.region === 'string') region = parsed.region
    } catch {
      toast.error('kiro-auth-token.json 内容不是有效的 JSON')
      return
    }

    if (!refreshToken || !refreshToken.trim()) {
      toast.error('未在 JSON 中找到有效的 refreshToken 字段')
      return
    }

    let clientId: string | undefined
    let clientSecret: string | undefined

    if (needsClientCreds) {
      if (!clientJson.trim()) {
        toast.error('该登录方式需要粘贴或上传 IdC 客户端注册 JSON（{hash}.json）')
        return
      }
      try {
        const parsedClient = JSON.parse(clientJson)
        if (typeof parsedClient.clientId === 'string') clientId = parsedClient.clientId
        if (typeof parsedClient.clientSecret === 'string') clientSecret = parsedClient.clientSecret
      } catch {
        toast.error('IdC 客户端注册 JSON 内容不是有效的 JSON')
        return
      }
      if (!clientId?.trim() || !clientSecret?.trim()) {
        toast.error('未在 JSON 中找到有效的 clientId / clientSecret 字段')
        return
      }
    }

    const authMethod = clientId && clientSecret ? 'idc' : 'social'

    // 注：后端 AddCredentialRequest 目前不接受 accessToken 字段（仅凭 refreshToken
    // 刷新获取），此处解析出的 accessToken 仅用于校验文件格式，不随请求发送。
    void accessToken

    mutate(
      {
        refreshToken: refreshToken.trim(),
        authMethod,
        clientId: clientId?.trim() || undefined,
        clientSecret: clientSecret?.trim() || undefined,
        authRegion: region?.trim() || undefined,
        apiRegion: region?.trim() || undefined,
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
          <DialogTitle>本地缓存导入</DialogTitle>
          <DialogDescription>
            从本机 Kiro IDE 的本地缓存文件导入账号，无需重新登录。
          </DialogDescription>
        </DialogHeader>

        <form onSubmit={handleSubmit} className="flex flex-col min-h-0 flex-1">
          <div className="space-y-4 py-4 overflow-y-auto flex-1 pr-1">
            {/* 登录方式 */}
            <div className="space-y-2">
              <label htmlFor="localcache-provider" className="text-sm font-medium">
                登录方式
              </label>
              <select
                id="localcache-provider"
                value={provider}
                onChange={(e) => setProvider(e.target.value as LocalCacheProvider)}
                disabled={isPending}
                className="flex h-10 w-full rounded-md border border-input bg-background px-3 py-2 text-sm ring-offset-background focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring focus-visible:ring-offset-2 disabled:cursor-not-allowed disabled:opacity-50"
              >
                <option value="builderid">AWS Builder ID</option>
                <option value="enterprise">Enterprise (IAM Identity Center)</option>
                <option value="google">Google</option>
                <option value="github">GitHub</option>
              </select>
            </div>

            {/* kiro-auth-token.json */}
            <div className="space-y-2">
              <label htmlFor="localcache-token" className="text-sm font-medium">
                kiro-auth-token.json <span className="text-red-500">*</span>
              </label>
              <textarea
                id="localcache-token"
                placeholder='粘贴 kiro-auth-token.json 的内容，例如 {"refreshToken":"...","accessToken":"...","region":"us-east-1"}'
                value={tokenJson}
                onChange={(e) => setTokenJson(e.target.value)}
                disabled={isPending}
                className="flex min-h-[120px] w-full rounded-md border border-input bg-background px-3 py-2 text-sm ring-offset-background placeholder:text-muted-foreground focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring focus-visible:ring-offset-2 disabled:cursor-not-allowed disabled:opacity-50 font-mono"
              />
              <div className="flex items-center gap-2">
                <Button
                  type="button"
                  variant="outline"
                  size="sm"
                  onClick={() => tokenFileRef.current?.click()}
                  disabled={isPending}
                >
                  <Upload className="h-4 w-4 mr-2" />
                  上传文件
                </Button>
                <input
                  ref={tokenFileRef}
                  type="file"
                  accept=".json,application/json"
                  className="hidden"
                  onChange={handleTokenFile}
                />
              </div>
              <p className="text-xs text-muted-foreground">
                文件位置：macOS/Linux <code>~/.aws/sso/cache/kiro-auth-token.json</code>，
                Windows <code>%USERPROFILE%\.aws\sso\cache\kiro-auth-token.json</code>
              </p>
            </div>

            {/* IdC 客户端注册 JSON — 仅 Builder ID / Enterprise */}
            {needsClientCreds && (
              <div className="space-y-2">
                <label htmlFor="localcache-client" className="text-sm font-medium">
                  IdC 客户端注册 JSON（{'{hash}'}.json） <span className="text-red-500">*</span>
                </label>
                <textarea
                  id="localcache-client"
                  placeholder='粘贴 {hash}.json 的内容，例如 {"clientId":"...","clientSecret":"..."}'
                  value={clientJson}
                  onChange={(e) => setClientJson(e.target.value)}
                  disabled={isPending}
                  className="flex min-h-[100px] w-full rounded-md border border-input bg-background px-3 py-2 text-sm ring-offset-background placeholder:text-muted-foreground focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring focus-visible:ring-offset-2 disabled:cursor-not-allowed disabled:opacity-50 font-mono"
                />
                <div className="flex items-center gap-2">
                  <Button
                    type="button"
                    variant="outline"
                    size="sm"
                    onClick={() => clientFileRef.current?.click()}
                    disabled={isPending}
                  >
                    <Upload className="h-4 w-4 mr-2" />
                    上传文件
                  </Button>
                  <input
                    ref={clientFileRef}
                    type="file"
                    accept=".json,application/json"
                    className="hidden"
                    onChange={handleClientFile}
                  />
                </div>
                <p className="text-xs text-muted-foreground">
                  同目录下以哈希值命名的 JSON 文件（如 <code>a1b2c3....json</code>），
                  与 kiro-auth-token.json 位于同一 <code>.aws/sso/cache/</code> 目录
                </p>
              </div>
            )}

            {/* 用户名/邮箱 */}
            <div className="space-y-2">
              <label htmlFor="localcache-email" className="text-sm font-medium">
                用户名 / 邮箱
              </label>
              <Input
                id="localcache-email"
                type="text"
                placeholder="请输入账号邮箱（用于标识账号，可留空）"
                value={email}
                onChange={(e) => setEmail(e.target.value)}
                disabled={isPending}
              />
            </div>

            {/* 备注昵称 */}
            <div className="space-y-2">
              <label htmlFor="localcache-nickname" className="text-sm font-medium">
                备注昵称
              </label>
              <Input
                id="localcache-nickname"
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
