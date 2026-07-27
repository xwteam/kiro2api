import { useState, useEffect } from 'react'

export interface GeoInfo {
  country: string
  regionName: string
  city: string
  displayIp?: string
}

const geoCache = new Map<string, GeoInfo | null>()
let publicIpCache: string | null | undefined = undefined // undefined = not fetched yet

// 归属地接口必须是 https：面板线上是走 https 提供的，浏览器会把页面里
// 的 http 请求当成「主动混合内容」直接拦掉（连请求都发不出去）。旧实现打的是
// http://ip-api.com/batch，线上从来没有成功过一次，归属地那一列永远是空的。
// ip-api 免费版不支持 https（https://ip-api.com 直接回 403），所以换成同样免费、
// 支持 https + CORS + 中文地名（lang=zh-CN）的 ipwho.is。
// 注意：它按单个 IP 查询，没有 batch，所以下面做了去重 + 并发上限。
const GEO_ENDPOINT = 'https://ipwho.is'
const GEO_FIELDS = 'ip,success,country,region,city'

// 一页日志去重后的 IP 通常只有个位数；限并发只是为了极端情况下别把免费接口打到限流。
const GEO_CONCURRENCY = 4

// 传输层失败（断网 / 限流 / 5xx / 被墙）时的冷却窗口。
//
// 为什么需要：旧实现在 catch 里对每个 IP 写 geoCache.set(ip, null)，而缓存只用
// has() 判断，于是一次网络抖动就把这些 IP 永久钉死成「查不到」，整个标签页生命周期
// 内再也不会重试。现在这类失败一律不写缓存（保留重试机会），改用一个全局冷却窗口
// 防止每次渲染都立刻重打。
const GEO_RETRY_COOLDOWN_MS = 60_000
let geoCooldownUntil = 0

interface GeoApiResponse {
  success?: boolean
  country?: string
  region?: string
  city?: string
}

function isLocalhost(ip: string) {
  return ip === '127.0.0.1' || ip === '::1' || ip.startsWith('::ffff:127.')
}

async function getPublicIp(): Promise<string | null> {
  if (publicIpCache !== undefined) return publicIpCache
  try {
    const res = await fetch('https://api.ipify.org?format=json')
    const data = await res.json()
    publicIpCache = typeof data.ip === 'string' ? data.ip : null
  } catch {
    publicIpCache = null
  }
  return publicIpCache ?? null
}

// 查一个 IP 的归属地。
// 只有拿到「确定性结论」时才写缓存：接口回 success:true → 写归属地；回 success:false
// （保留地址、查不到，HTTP 404 也是这种）→ 写 null。其余情况（网络异常、429、5xx）
// 属于暂时性失败，不写缓存，改为进入冷却窗口，之后还能再试。
async function fetchGeoForIp(ip: string): Promise<void> {
  const url = `${GEO_ENDPOINT}/${encodeURIComponent(ip)}?lang=zh-CN&fields=${GEO_FIELDS}`
  let res: Response
  try {
    res = await fetch(url)
  } catch {
    geoCooldownUntil = Date.now() + GEO_RETRY_COOLDOWN_MS
    return
  }
  // 404 = 这个 IP 本身查不到（非法/保留地址），是确定性结论，照常读 body。
  if (!res.ok && res.status !== 404) {
    geoCooldownUntil = Date.now() + GEO_RETRY_COOLDOWN_MS
    return
  }
  let data: GeoApiResponse | null = null
  try {
    data = (await res.json()) as GeoApiResponse
  } catch {
    data = null
  }
  if (!data) {
    geoCooldownUntil = Date.now() + GEO_RETRY_COOLDOWN_MS
    return
  }
  geoCache.set(
    ip,
    data.success
      ? { country: data.country ?? '', regionName: data.region ?? '', city: data.city ?? '' }
      : null
  )
}

async function fetchGeoForIps(ips: string[]): Promise<void> {
  if (ips.length === 0) return
  if (Date.now() < geoCooldownUntil) return

  const queue = [...ips]
  const workers = Array.from({ length: Math.min(GEO_CONCURRENCY, queue.length) }, async () => {
    while (queue.length > 0) {
      const ip = queue.shift()
      if (!ip || geoCache.has(ip)) continue
      await fetchGeoForIp(ip)
      // 一旦进入冷却（接口挂了/被限流），剩下的 IP 就别再挨个撞了。
      if (Date.now() < geoCooldownUntil) return
    }
  })
  await Promise.all(workers)
}

export function useIpGeo(ips: string[]): Map<string, GeoInfo | null> {
  const [result, setResult] = useState<Map<string, GeoInfo | null>>(new Map())

  useEffect(() => {
    const uniqueIps = [...new Set(ips)].filter(Boolean)
    if (uniqueIps.length === 0) return

    // 翻页/刷新会让这个 effect 重跑，上一轮的异步结果不能再往新页面上写。
    let cancelled = false

    const localhostIps = uniqueIps.filter(isLocalhost)
    const regularIps = uniqueIps.filter((ip) => !isLocalhost(ip))

    const applyCache = (publicIp: string | null) => {
      if (cancelled) return
      const m = new Map<string, GeoInfo | null>()
      for (const ip of uniqueIps) {
        if (isLocalhost(ip)) {
          const geo = publicIp ? (geoCache.get(publicIp) ?? null) : null
          m.set(ip, geo ? { ...geo, displayIp: publicIp ?? undefined } : publicIp ? { country: '', regionName: '', city: '', displayIp: publicIp } : null)
        } else {
          m.set(ip, geoCache.get(ip) ?? null)
        }
      }
      setResult(m)
    }

    const run = async () => {
      // 解析普通 IP 的归属地
      const uncachedRegular = regularIps.filter((ip) => !geoCache.has(ip))
      await fetchGeoForIps(uncachedRegular)

      // 处理 localhost：获取公网 IP 并解析其归属地
      let publicIp: string | null = null
      if (localhostIps.length > 0) {
        publicIp = await getPublicIp()
        if (publicIp && !geoCache.has(publicIp)) {
          await fetchGeoForIps([publicIp])
        }
      }

      applyCache(publicIp)
    }

    run()

    return () => {
      cancelled = true
    }
  // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [ips.join(',')])

  return result
}
