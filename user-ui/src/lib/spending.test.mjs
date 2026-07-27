/* 用户面板额度状态回归测试(纯 Node,零依赖;.ts 由 Node 的类型擦除直接加载,需 Node >= 22.18):
 *
 *   node --test user-ui/src/lib/spending.test.mjs
 *
 * 钉住一个真实可见的故障:额度状态牌子/额度条与中继的实际裁决对不上 ——
 * 面板显示绿色「正常」、条还没走满,而中继对这把 key 的每一发请求都回 402。
 * 根因是面板自己按 `已花 >= 上限` 推,而鉴权闸的准入判据是
 * `已花 + 单次预留 <= 上限`(单次预留 = 1 USD ≈ 1.39 credits),两者差着一整个预留额。
 */
import test from 'node:test';
import assert from 'node:assert/strict';
import { spendingView } from './spending.ts';

test('上限小于一次预留:后端说用完,面板必须报「额度已用完」并把条画满', () => {
  // 1.38 credits 的卡:鉴权闸从签发起就 402(0 + 1.389 > 1.38),后端 exhausted=true。
  const v = spendingView({
    spendingLimit: 1.38,
    limitUnit: 'credits',
    totalCost: 0,
    totalCredits: 0,
    exhausted: true,
  });
  // 旧判据 used >= limit 在这里为假 —— 正是它让面板显示绿色「正常」。
  assert.ok(v.used < v.limit, '本用例必须落在「已花 < 上限」区间,否则钉不住这个缺陷');
  assert.equal(v.exhausted, true, '后端已判定发不出请求,面板不得显示正常');
  assert.equal(v.percent, 100, '用完了条却没走满,等于告诉用户还有额度');
});

test('逼近上限:差不到一次预留时同样按已用完显示', () => {
  // 已花 2.5 USD / 上限 3.0 USD:闸上 2.5 + 1.0 > 3.0 → 402,后端给 exhausted=true。
  const v = spendingView({
    spendingLimit: 3.0,
    limitUnit: 'usd',
    totalCost: 2.5,
    totalCredits: 2.5 / 0.72,
    exhausted: true,
  });
  assert.ok(v.used < v.limit);
  assert.equal(v.exhausted, true);
  assert.equal(v.percent, 100);
});

test('额度真的还够用时不得误报已用完', () => {
  const v = spendingView({
    spendingLimit: 100,
    limitUnit: 'credits',
    totalCost: 7.2,
    totalCredits: 10,
    exhausted: false,
  });
  assert.equal(v.exhausted, false);
  assert.equal(v.percent, 10);
  assert.equal(v.used, 10, 'credits 计量必须读 totalCredits 而不是 totalCost');
});

test('不限额的 key 永远不算用完,也不画额度条', () => {
  const v = spendingView({
    spendingLimit: null,
    limitUnit: 'usd',
    totalCost: 99,
    totalCredits: 137.5,
    exhausted: false,
  });
  assert.equal(v.hasLimit, false);
  assert.equal(v.exhausted, false);
  assert.equal(v.percent, null);
});

test('上限 0(冻结)要画出来并显示已用完', () => {
  const v = spendingView({
    spendingLimit: 0,
    limitUnit: 'usd',
    totalCost: 0,
    totalCredits: 0,
    exhausted: true,
  });
  assert.equal(v.hasLimit, true, 'spendingLimit=0 是真实配置,不能当成不限额');
  assert.equal(v.exhausted, true);
  assert.equal(v.percent, 100, '0/0 会算出 NaN,必须按 100% 处理');
});

test('老后端不带 exhausted 字段时退回旧判据,不至于没有牌子', () => {
  const v = spendingView({
    spendingLimit: 3.0,
    limitUnit: 'usd',
    totalCost: 3.0,
    totalCredits: 3.0 / 0.72,
  });
  assert.equal(v.exhausted, true);
});

test('还没拿到数据时不崩、也不乱画', () => {
  const v = spendingView(undefined);
  assert.deepEqual(v, {
    isCredits: false,
    hasLimit: false,
    limit: 0,
    used: 0,
    exhausted: false,
    percent: null,
  });
});
