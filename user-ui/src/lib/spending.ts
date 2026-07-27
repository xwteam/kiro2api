/** 额度状态的纯逻辑：面板的「已用完」牌子和额度条都由这里算，便于单独回归。 */

/** 算额度状态只需要这几项；单列结构体，避免把整个 UsageResponse 拖进纯逻辑模块。 */
export interface SpendingInput {
  spendingLimit: number | null
  limitUnit: string
  totalCost: number
  totalCredits: number
  /** 后端按鉴权闸的准入算式给出的「下一发请求会不会被 402」。老后端没有这个字段。 */
  exhausted?: boolean
}

export interface SpendingView {
  /** 计量单位是不是 credits（决定展示用哪一栏数字、以及怎么格式化）。 */
  isCredits: boolean
  /** 有没有设消费上限。null 表示不限额；0 是一种真实配置（冻结这把 key），必须区分开。 */
  hasLimit: boolean
  limit: number
  used: number
  /** 额度是否已用完（用完 = 中继会把请求 402 拒掉）。 */
  exhausted: boolean
  /** 额度条百分比；不限额时为 null（不画条）。 */
  percent: number | null
}

export function spendingView(data: SpendingInput | undefined): SpendingView {
  const isCredits = data?.limitUnit === 'credits'
  const used = (isCredits ? data?.totalCredits : data?.totalCost) ?? 0

  // 「有没有设上限」必须用 != null 判断，不能用真值判断。
  // 后端把「不限额」表示为 null，而 spendingLimit = 0 是一种真实配置（冻结这把 key）：
  // 鉴权闸对 Some(0.0) 会把每一发请求都以 402 拒掉。旧写法 `data.spendingLimit &&`
  // 让 0 和 null 无法区分 —— 额度条整块不渲染、状态还是绿色「正常」，用户看到一把
  // 健康的 key，实际 100% 的调用都被拒绝。
  const hasLimit = data?.spendingLimit != null
  const limit = data?.spendingLimit ?? 0

  // 「用完没用完」以**后端**为准：后端用的是鉴权闸那份准入算式
  // （已花 + 单次预留 > 上限 即 402，见 src/user/handler.rs 的 spending_exhausted）。
  // 面板自己按 `已花 >= 上限` 推会漏掉「还差不到一次预留」的那一整段：那段里中继对每一发
  // 请求都回 402，面板却显示绿色「正常」+ 一条没走完的额度条。上限本身小于一次预留时
  // （例如 1.38 credits）更是整把 key 从签发起就发不出任何请求，面板照样显示正常。
  // `??` 兜底旧后端（不带 exhausted 字段时退回旧判据，总比没有牌子强）。
  const exhausted = hasLimit && (data?.exhausted ?? used >= limit)

  const percent = !hasLimit
    ? null
    : // 已用完就把条画满：后端说发不出请求了，条却停在 72% 会让用户以为还有额度。
      exhausted
      ? 100
      : // 上限为 0 时 used/limit 会算出 NaN(0/0) 或 Infinity，Progress 会画歪，直接按 100% 处理。
        limit > 0
        ? Math.min((used / limit) * 100, 100)
        : 100

  return { isCredits, hasLimit, limit, used, exhausted, percent }
}
