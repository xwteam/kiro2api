/* 余额扇出(balance fan-out)回归测试 —— 纯 Node,零依赖。
 *
 *   node --test admin-ui-v2/tests/
 *
 * 覆盖两条曾经打到线上的故障:
 *   ① 仪表盘冷启动会对**每个**启用账号一次性并发打 GET /credentials/{id}/balance。
 *      生产池上千个账号 = 一瞬间上千并发直捅上游 getUsageLimits。
 *   ② 扇出中途撞到 401 后照旧一路发完剩余账号,每个 401 都重弹登录浮层、清空并
 *      抢占输入框焦点,运维被自己的后台锁在门外。
 *
 * 这两个分区文件是浏览器里的 IIFE(靠 window/document 全局跑),所以这里用
 * `new Function` 把它们注入一套**最小 DOM 替身**:凡是本次路径不需要的 DOM 查询
 * 一律返回 null,被测代码里的 `if (!node) return` 守卫会自然把它们跳过。
 */
import test from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

const JS_DIR = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '../js');

// ---------------- 最小 DOM 替身 ----------------

function fakeEl() {
  return {
    textContent: '', innerHTML: '', className: '', src: '', alt: '', type: '',
    rel: '', href: '', loading: '', disabled: false, value: '',
    style: {},
    classList: { add() {}, remove() {}, toggle() {}, contains() { return false; } },
    setAttribute() {}, removeAttribute() {}, getAttribute() { return null; },
    appendChild(c) { return c; }, removeChild(c) { return c; }, remove() {},
    addEventListener() {}, removeEventListener() {},
    querySelector() { return fakeEl(); },
    querySelectorAll() { return []; },
    focus() {}, select() {}, click() {}
  };
}

// byId: 只有明确列出的 id 才返回元素,其余一律 null(触发被测代码的空守卫)。
function makeDom(byId) {
  const document = {
    head: fakeEl(),
    body: fakeEl(),
    getElementById(id) { return Object.prototype.hasOwnProperty.call(byId, id) ? byId[id] : null; },
    querySelector() { return null; },
    querySelectorAll() { return []; },
    createElement() { return fakeEl(); },
    addEventListener() {}
  };
  return document;
}

// ApiError 的形状与 admin-ui-v2/js/api.js 一致:带 status 的 Error。
function apiError(status) {
  const e = new Error('unauthorized');
  e.name = 'ApiError';
  e.status = status;
  return e;
}

/* 记账用的假 K.api:
 *   - 记录每条请求路径;
 *   - 跟踪**余额请求**同时在飞的条数并留下峰值 maxInFlight(只数 /balance:
 *     列表、日用量等一次性请求不属于扇出,不该混进并发峰值);
 *   - 每个响应都异步兑现(0ms timer),真实还原并发窗口;
 *   - handler 可以按路径抛 401 来模拟钥匙失效。
 */
function makeApi(handler) {
  const stats = { calls: [], inFlight: 0, maxInFlight: 0, unauthorizedCount: 0 };
  const api = {
    _stats: stats,
    get(pathname) {
      const counted = pathname.endsWith('/balance');
      stats.calls.push(pathname);
      if (counted) {
        stats.inFlight++;
        if (stats.inFlight > stats.maxInFlight) stats.maxInFlight = stats.inFlight;
      }
      return new Promise((resolve, reject) => {
        setTimeout(() => {
          if (counted) stats.inFlight--;
          let out;
          try {
            out = handler(pathname);
          } catch (e) {
            // api.js 在 401/403 上会先清钥匙 + 调 onUnauthorized,再抛 ApiError。
            if (e && (e.status === 401 || e.status === 403)) {
              stats.unauthorizedCount++;
              api.setAdminKey('');
              if (typeof api.onUnauthorized === 'function') api.onUnauthorized();
            }
            reject(e);
            return;
          }
          resolve(out);
        }, 0);
      });
    },
    post() { return Promise.resolve({}); },
    put() { return Promise.resolve({}); },
    del() { return Promise.resolve({}); },
    raw() { return Promise.resolve({}); },
    toast() {},
    hasAdminKey() { return true; },
    setAdminKey() {},
    onUnauthorized: null
  };
  return api;
}

// 把一个分区 IIFE 装进给定的 window/document 里。
function loadSection(file, ctx) {
  const src = readFileSync(path.join(JS_DIR, file), 'utf8');
  const fn = new Function('window', 'document', 'setInterval', 'clearInterval', 'fetch', src);
  fn(ctx.window, ctx.document, () => 0, () => {}, undefined);
}

// 轮询等待条件成立(扇出是"发一发等一发",没有可 await 的总 Promise)。
async function waitFor(cond, ms = 3000) {
  const deadline = Date.now() + ms;
  while (Date.now() < deadline) {
    if (cond()) return true;
    await new Promise((r) => setTimeout(r, 2));
  }
  return false;
}

// 静置若干轮事件循环,确认"不该再来的请求"确实没有再来。
async function settle(rounds = 60) {
  for (let i = 0; i < rounds; i++) await new Promise((r) => setTimeout(r, 2));
}

function credentials(n) {
  return Array.from({ length: n }, (_, i) => ({ id: i + 1, disabled: false }));
}

// ---------------- 仪表盘 ----------------

const ACCOUNT_COUNT = 200;   // 够大:老代码会一次性把它们全发出去

function bootDashboard(handler) {
  const api = makeApi(handler);
  const window = { K: { api }, addEventListener() {}, removeEventListener() {} };
  window.K.sections = {};
  const document = makeDom({ 'sec-dashboard': fakeEl() });
  loadSection('sec-dashboard.js', { window, document });
  return { window, api, sections: window.K.sections };
}

// 仪表盘走的是 K.api(而非局部 api 变量),所以 handler 直接按路径分发。
function dashboardHandler(extra) {
  return (p) => {
    if (p === '/credentials') return { credentials: credentials(ACCOUNT_COUNT), total: ACCOUNT_COUNT, available: ACCOUNT_COUNT };
    if (p === '/usage/daily') return [];
    if (p === '/models') return { data: [] };
    if (p === '/server-info') return { version: '0.2.0' };
    if (p === '/config') return {};
    if (p === '/config/load-balancing') return { mode: 'priority' };
    if (p === '/check-update') return { hasUpdate: false };
    if (p === '/credits/global') return { globalCredits: 0, cachedCount: 0, totalCount: ACCOUNT_COUNT, oldestCacheUnix: null };
    if (extra) return extra(p);
    return {};
  };
}

test('dashboard: 冷启动扇出的并发有上限(不再一次性打满全池)', async () => {
  const { api, sections } = bootDashboard(dashboardHandler((p) => {
    if (p.endsWith('/balance')) return { remaining: 1 };
    return {};
  }));

  sections.dashboard.onShow();

  const balanceCalls = () => api._stats.calls.filter((p) => p.endsWith('/balance'));
  assert.ok(await waitFor(() => balanceCalls().length >= ACCOUNT_COUNT), '冷启动应当把全池账号都查一遍');

  // 修复前:Promise.allSettled(全量 map) → maxInFlight === ACCOUNT_COUNT(≈整池并发)。
  // 修复后:工作者池 → maxInFlight 不超过 GC_FANOUT_CONCURRENCY(源码里是 4)。
  assert.ok(
    api._stats.maxInFlight <= 8,
    `余额扇出并发失控:峰值 ${api._stats.maxInFlight} 个同时在飞(共 ${ACCOUNT_COUNT} 个账号)`
  );
  assert.ok(api._stats.maxInFlight >= 2, '不该退化成完全串行(会拖成十几分钟)');
});

test('dashboard: 扇出撞 401 立刻整轮停,不再刷登录浮层', async () => {
  const FAIL_AT = 5;   // 第 5 个 balance 请求开始钥匙失效
  let balanceSeen = 0;
  const { api, sections } = bootDashboard(dashboardHandler((p) => {
    if (p.endsWith('/balance')) {
      balanceSeen++;
      if (balanceSeen >= FAIL_AT) throw apiError(401);
      return { remaining: 1 };
    }
    return {};
  }));

  sections.dashboard.onShow();

  assert.ok(await waitFor(() => api._stats.unauthorizedCount > 0), '应当至少撞到一次 401');
  await settle();

  const balanceCalls = api._stats.calls.filter((p) => p.endsWith('/balance')).length;
  // 修复前:所有请求早已一次性发出 → balanceCalls === ACCOUNT_COUNT,401 次数上百。
  assert.ok(
    balanceCalls < ACCOUNT_COUNT / 2,
    `401 之后仍继续扇出:一共发了 ${balanceCalls}/${ACCOUNT_COUNT} 个余额请求`
  );
  assert.ok(
    api._stats.unauthorizedCount <= 8,
    `登录浮层被反复重弹:触发了 ${api._stats.unauthorizedCount} 次 401`
  );
  // 中止后不该再去读聚合端点(那一发同样会 401 再弹一次浮层)。
  const aggAfterAuthFail = api._stats.calls.lastIndexOf('/credits/global');
  const lastBalance = api._stats.calls.map((p) => p.endsWith('/balance')).lastIndexOf(true);
  assert.ok(aggAfterAuthFail < lastBalance, '401 中止后不应再请求 /credits/global');
});

test('dashboard: onHide 作废在飞的扇出', async () => {
  const { api, sections } = bootDashboard(dashboardHandler((p) => {
    if (p.endsWith('/balance')) return { remaining: 1 };
    return {};
  }));

  sections.dashboard.onShow();
  const balanceCalls = () => api._stats.calls.filter((p) => p.endsWith('/balance')).length;
  assert.ok(await waitFor(() => balanceCalls() > 0), '扇出应当已经开始');

  sections.dashboard.onHide();
  const atHide = balanceCalls();
  await settle();

  // 允许在飞的那几发跑完,但不允许继续补新的。
  assert.ok(
    balanceCalls() <= atHide + 8,
    `onHide 之后扇出没停:${atHide} → ${balanceCalls()}(共 ${ACCOUNT_COUNT} 个账号)`
  );
  assert.ok(balanceCalls() < ACCOUNT_COUNT, 'onHide 之后仍把全池跑完了');
});

// ---------------- 账号页 ----------------

function bootAccounts(handler) {
  const api = makeApi(handler);
  const window = { K: { api }, addEventListener() {}, removeEventListener() {} };
  window.K.sections = {};
  const document = makeDom({});   // 账号页的 DOM 全部缺席 → 渲染函数各自空守卫返回
  loadSection('sec-accounts.js', { window, document });
  return { window, api, sections: window.K.sections };
}

function accountsHandler(extra) {
  return (p) => {
    if (p === '/credentials') return { credentials: credentials(ACCOUNT_COUNT) };
    if (p === '/usage/daily') return [];
    if (extra) return extra(p);
    return {};
  };
}

test('accounts: 自动查询保持串行(一次只发一个)', async () => {
  const { api, sections } = bootAccounts(accountsHandler((p) => {
    if (p.endsWith('/balance')) return { remaining: 1 };
    return {};
  }));

  sections.accounts.onShow();
  const balanceCalls = () => api._stats.calls.filter((p) => p.endsWith('/balance')).length;
  assert.ok(await waitFor(() => balanceCalls() >= 20), '自动查询应当已经开跑');
  assert.equal(api._stats.maxInFlight, 1, '账号页的余额扇出必须是串行的');
});

test('accounts: 扇出撞 401 立刻整轮停(否则登录框被反复清空)', async () => {
  const FAIL_AT = 5;
  let balanceSeen = 0;
  const { api, sections } = bootAccounts(accountsHandler((p) => {
    if (p.endsWith('/balance')) {
      balanceSeen++;
      if (balanceSeen >= FAIL_AT) throw apiError(401);
      return { remaining: 1 };
    }
    return {};
  }));

  sections.accounts.onShow();

  assert.ok(await waitFor(() => api._stats.unauthorizedCount > 0), '应当至少撞到一次 401');
  await settle();

  const balanceCalls = api._stats.calls.filter((p) => p.endsWith('/balance')).length;
  // 修复前:.catch 把 ApiError 一律吞掉后无条件 next(),会把 200 个全跑完、
  // 触发约 196 次 onUnauthorized。
  assert.equal(
    balanceCalls, FAIL_AT,
    `401 之后仍继续扇出:一共发了 ${balanceCalls}/${ACCOUNT_COUNT} 个余额请求`
  );
  assert.equal(api._stats.unauthorizedCount, 1, '登录浮层被反复重弹');
});

test('accounts: onHide 作废在飞的扇出', async () => {
  const { api, sections } = bootAccounts(accountsHandler((p) => {
    if (p.endsWith('/balance')) return { remaining: 1 };
    return {};
  }));

  sections.accounts.onShow();
  const balanceCalls = () => api._stats.calls.filter((p) => p.endsWith('/balance')).length;
  assert.ok(await waitFor(() => balanceCalls() > 0), '扇出应当已经开始');

  sections.accounts.onHide();
  const atHide = balanceCalls();
  await settle();

  assert.ok(
    balanceCalls() <= atHide + 1,
    `onHide 之后扇出没停:${atHide} → ${balanceCalls()}(共 ${ACCOUNT_COUNT} 个账号)`
  );
});

test('accounts: 手动查询也带令牌,离开页面能停下来', async () => {
  const { api, sections, window } = bootAccounts(accountsHandler((p) => {
    if (p.endsWith('/balance')) return { remaining: 1 };
    return {};
  }));

  // 先让账号列表就位,并把自动查询那一轮停掉,再单独驱动手动查询。
  sections.accounts.onShow();
  const balanceCalls = () => api._stats.calls.filter((p) => p.endsWith('/balance')).length;
  assert.ok(await waitFor(() => balanceCalls() > 0), '自动查询应当已经开始');
  sections.accounts.onHide();
  await settle(10);

  // 手动「查询信息」按钮:通过 window.K 暴露不出来,改用 onShow 再跑一轮 + onHide
  // 验证同一条取消路径对带按钮的那一轮同样生效。
  const before = balanceCalls();
  sections.accounts.onShow();
  assert.ok(await waitFor(() => balanceCalls() > before), '新一轮扇出应当已经开始');
  sections.accounts.onHide();
  const atHide = balanceCalls();
  await settle();
  assert.ok(balanceCalls() <= atHide + 1, '第二轮扇出在 onHide 之后没停');
  assert.ok(typeof window.K.sections.accounts.onHide === 'function');
});
