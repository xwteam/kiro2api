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
  const start = SRC.indexOf('function accountBucket(acc) {');
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
