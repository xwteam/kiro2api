/**
 * 账号状态分档(accountBucket)的回归测试。
 *
 * 为什么单独测这个函数:面板上「异常」此前把三类完全不同的情况混成一档 —— 被上游封禁、
 * 额度耗尽、单纯限流冷却。运维要采取的动作各不相同(换号 / 等重置 / 等一会儿),分错档
 * 就会按错误的方式处置。分档规则是纯函数,直接从源码里摘出来测,不需要 DOM。
 */
import test from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

const SRC = readFileSync(
  path.resolve(path.dirname(fileURLToPath(import.meta.url)), '../js/sec-accounts.js'),
  'utf8'
);

// 从源码里摘出 accountBucket 的函数体(连同它读的常量),用 new Function 求值。
function loadBucket() {
  const start = SRC.indexOf('function accountBucket(acc, remaining) {');
  assert.ok(start > 0, '源码里应存在 accountBucket');
  let depth = 0, i = SRC.indexOf('{', start), end = -1;
  for (; i < SRC.length; i++) {
    if (SRC[i] === '{') depth++;
    else if (SRC[i] === '}') { depth--; if (depth === 0) { end = i + 1; break; } }
  }
  assert.ok(end > start, 'accountBucket 的括号应闭合');
  return new Function(SRC.slice(start, end) + '; return accountBucket;')();
}

const bucket = loadBucket();
const FUTURE = new Date(Date.now() + 864e5).toISOString();
const PAST = new Date(Date.now() - 864e5).toISOString();

test('禁用优先于一切:被系统永久禁用的号不该再按「封禁」或「异常」呈现', () => {
  assert.equal(bucket({ disabled: true, statusReason: 'banned', healthStatus: 'unhealthy' }), 'disabled');
});

test('余额归零即判额度耗尽 —— 不必等它被选中失败一次', () => {
  // 只等 statusReason==='quota' 的话,还没轮到的账号会一直显示「健康」,
  // 而它下一次被选中必然失败。
  assert.equal(bucket({ healthStatus: 'healthy', expiresAt: FUTURE }, 0), 'quota');
  assert.equal(bucket({ healthStatus: 'healthy', expiresAt: FUTURE }, -5), 'quota');
  assert.equal(bucket({ healthStatus: 'healthy', expiresAt: FUTURE }, 12.5), 'healthy');
});

test('还没查过余额 ≠ 没额度:undefined 不得被当成 0', () => {
  assert.equal(bucket({ healthStatus: 'healthy', expiresAt: FUTURE }, undefined), 'healthy');
  assert.equal(bucket({ healthStatus: 'healthy', expiresAt: FUTURE }), 'healthy');
});

test('额度耗尽优先于过期:两者都成立时,先看到「额度耗尽」', () => {
  assert.equal(bucket({ expiresAt: PAST }, 0), 'quota');
});

test('续期被拒单列一档 —— 与「过期了刷一下就好」不是一回事', () => {
  // 线上真实事故:上游对整批账号回 access_denied,若混进「异常」或「过期」,
  // 运维会一直等它自愈,而它永远不会自愈。
  assert.equal(bucket({ statusReason: 'refresh_denied', healthStatus: 'unhealthy', expiresAt: PAST }), 'refreshDenied');
});

test('封禁与额度耗尽从「异常」里分出来', () => {
  assert.equal(bucket({ statusReason: 'banned', healthStatus: 'unhealthy', expiresAt: FUTURE }), 'banned');
  assert.equal(bucket({ statusReason: 'quota', healthStatus: 'unhealthy', expiresAt: FUTURE }), 'quota');
});

test('封禁优先于过期:被封的号即使令牌也过期了,要看到的是「封禁」', () => {
  assert.equal(bucket({ statusReason: 'banned', expiresAt: PAST }), 'banned');
});

test('过期按到期时刻判定,未到期不算', () => {
  assert.equal(bucket({ expiresAt: PAST, healthStatus: 'healthy' }), 'expired');
  assert.equal(bucket({ expiresAt: FUTURE, healthStatus: 'healthy' }), 'healthy');
});

test('冷却中与仅有失败计数都归「异常」', () => {
  assert.equal(bucket({ healthStatus: 'unhealthy', expiresAt: FUTURE }), 'abnormal');
  assert.equal(bucket({ healthStatus: 'warning', expiresAt: FUTURE }), 'abnormal');
});

test('无任何异常信号即健康;字段缺失也不能崩', () => {
  assert.equal(bucket({ healthStatus: 'healthy', expiresAt: FUTURE, statusReason: 'none' }), 'healthy');
  assert.equal(bucket({}), 'healthy');
  assert.equal(bucket({ expiresAt: 'not-a-date' }), 'healthy');
});

// ---------------------------------------------------------------------------
// 徽章与筛选必须同源。
//
// 这正是本次两个线上问题的根因:筛选用 accountBucket、徽章用 healthStatus,两者互不知情,
// 于是「过期账号」那一档里的行挂着绿色的「健康」,余额归零的号也一直显示健康。
// 这里断言 healthBadge 的分支表是从分档推出来的,而不是另起一套。
test('healthBadge 由 accountBucket 驱动,不再单看 healthStatus', () => {
  const src = SRC.slice(SRC.indexOf('function healthBadge(acc)'));
  const body = src.slice(0, src.indexOf('\n  }'));
  assert.ok(
    /bucketOf\(acc\)/.test(body),
    'healthBadge 必须先取 accountBucket 的结果,否则徽章会和筛选给出矛盾的结论'
  );
  for (const k of ['banned', 'expired', 'quota']) {
    assert.ok(
      new RegExp(`\\b${k}:`).test(body),
      `healthBadge 缺少 ${k} 这一档的徽章文案,该档的行会回落成「健康」`
    );
  }
});

// ---------------------------------------------------------------------------
// 账号列表的静默自动刷新。
//
// 页面此前打开即冻结:账号被封、恢复、过期,面板全都不动。运维照着一屏过时徽章做判断,
// 比没有徽章更糟 —— 尤其「封禁账号 (0)」这种数字看着像结论,其实可能是十分钟前的。
test('自动刷新只重拉列表,绝不顺带重跑余额扇出', () => {
  const i = SRC.indexOf('function refreshAccountsQuietly()');
  assert.ok(i > 0, '应存在 refreshAccountsQuietly');
  const body = SRC.slice(i, SRC.indexOf('\n  }', i));
  assert.ok(/api\.get\('\/credentials'\)/.test(body), '应重拉列表接口');
  assert.ok(
    !/autoQueryBalances|runBalanceFanout/.test(body),
    '自动刷新里绝不能触发余额扇出 —— 那是逐个打上游的,每 30 秒来一遍等于给上游送风控'
  );
  assert.ok(/page\s*=\s*1/.test(body) === false, '静默刷新不得重置页码,否则运维翻的页会被顶回第一页');
});

test('页面不可见时不刷新,离开分区时停掉定时器', () => {
  const i = SRC.indexOf('function refreshAccountsQuietly()');
  const body = SRC.slice(i, SRC.indexOf('\n  }', i));
  assert.ok(/document\.hidden/.test(body), '标签页在后台就不该继续拉');
  assert.ok(/stopAutoRefresh\(\)/.test(SRC.slice(SRC.indexOf('onHide: function'))), 'onHide 必须停掉定时器');
});

test('新鲜度提示存在,且超时会标记为陈旧', () => {
  const i = SRC.indexOf('function paintFreshness()');
  assert.ok(i > 0, '应存在 paintFreshness —— 让运维看得出屏幕上的数字是几秒前的');
  const body = SRC.slice(i, SRC.indexOf('\n  }', i));
  assert.ok(/is-stale/.test(body), '长时间没刷新成功要显式标记,别让陈旧快照冒充实时数据');
});
