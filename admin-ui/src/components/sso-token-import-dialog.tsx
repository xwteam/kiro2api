import { useState } from 'react'
import { toast } from 'sonner'
import { CheckCircle2, XCircle, Loader2 } from 'lucide-react'
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
import { useImportSsoToken } from '@/hooks/use-credentials'
import { extractErrorMessage } from '@/lib/utils'
import type { SsoTokenImportResponse } from '@/types/api'

interface SsoTokenImportDialogProps {
  open: boolean
  onOpenChange: (open: boolean) => void
}

export function SsoTokenImportDialog({ open, onOpenChange }: SsoTokenImportDialogProps) {
  const [text, setText] = useState('')
  const [region, setRegion] = useState('us-east-1')
  const [result, setResult] = useState<SsoTokenImportResponse | null>(null)

  const { mutateAsync: importToken, isPending: importing } = useImportSsoToken()

  const reset = () => {
    setText('')
    setRegion('us-east-1')
    setResult(null)
  }

  const handleImport = async () => {
    if (!text.trim()) {
      toast.error('请至少输入一行 SSO Token')
      return
    }

    try {
      // 设计更正：整段 textarea 作为单个 bearerToken 一次性发送，
      // 后端负责按行拆分导入；此处不做客户端拆分、不逐行调用。
      const res = await importToken({
        bearerToken: text,
        region: region.trim() || undefined,
      })
      setResult(res)
      if (res.failed.length === 0) {
        toast.success(`成功导入 ${res.added} 个账号`)
      } else {
        toast.info(`导入完成：成功 ${res.added} 个，失败 ${res.failed.length} 个`)
      }
    } catch (error) {
      toast.error(`导入失败: ${extractErrorMessage(error)}`)
    }
  }

  return (
    <Dialog
      open={open}
      onOpenChange={(newOpen) => {
        if (!newOpen && !importing) reset()
        onOpenChange(newOpen)
      }}
    >
      <DialogContent className="sm:max-w-2xl max-h-[80vh] flex flex-col">
        <DialogHeader>
          <DialogTitle>SSO Token 导入</DialogTitle>
          <DialogDescription>
            每行粘贴一个 SSO Token（x-amz-sso_authn cookie / bearer），一次性提交后由后端批量导入并落库。
          </DialogDescription>
        </DialogHeader>

        <div className="flex-1 overflow-y-auto space-y-4 py-2">
          <div className="space-y-2">
            <label htmlFor="ssotok-region" className="text-sm font-medium">区域</label>
            <Input
              id="ssotok-region"
              placeholder="us-east-1"
              value={region}
              onChange={(e) => setRegion(e.target.value)}
              disabled={importing}
            />
          </div>
          <div className="space-y-2">
            <label className="text-sm font-medium">SSO Token（每行一个）</label>
            <textarea
              placeholder={'每行一个 token\n例如:\naoaAAAA...\nbpbBBBB...'}
              value={text}
              onChange={(e) => setText(e.target.value)}
              disabled={importing}
              className="flex min-h-[160px] w-full rounded-md border border-input bg-background px-3 py-2 text-sm ring-offset-background placeholder:text-muted-foreground focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring focus-visible:ring-offset-2 disabled:cursor-not-allowed disabled:opacity-50 font-mono"
            />
          </div>

          {result && (
            <>
              <div className="flex gap-4 text-sm">
                <span className="text-green-600 dark:text-green-400 flex items-center gap-1">
                  <CheckCircle2 className="h-4 w-4" />
                  已添加 {result.added} 个账号
                </span>
                {result.failed.length > 0 && (
                  <span className="text-red-600 dark:text-red-400 flex items-center gap-1">
                    <XCircle className="h-4 w-4" />
                    失败 {result.failed.length} 个
                  </span>
                )}
              </div>
              {result.failed.length > 0 && (
                <div className="border rounded-md divide-y max-h-[300px] overflow-y-auto">
                  {result.failed.map((f) => (
                    <div key={f.lineIndex} className="p-3 flex items-start gap-3">
                      <XCircle className="w-5 h-5 text-red-500 shrink-0" />
                      <div className="flex-1 min-w-0 text-sm">
                        line #{f.lineIndex + 1}: {f.error}
                      </div>
                    </div>
                  ))}
                </div>
              )}
            </>
          )}
        </div>

        <DialogFooter>
          <Button
            type="button"
            variant="outline"
            onClick={() => { if (!importing) { reset(); onOpenChange(false) } }}
            disabled={importing}
          >
            {importing ? '导入中...' : result ? '关闭' : '取消'}
          </Button>
          {!result && (
            <Button type="button" onClick={handleImport} disabled={importing || !text.trim()}>
              {importing ? <><Loader2 className="h-4 w-4 mr-2 animate-spin" />导入中…</> : '开始导入'}
            </Button>
          )}
        </DialogFooter>
      </DialogContent>
    </Dialog>
  )
}
