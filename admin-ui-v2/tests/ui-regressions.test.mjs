/* 管理后台前端回归测试(纯 Node,零依赖):
 *
 *   node --test admin-ui-v2/tests/
 *
 * 每个用例都钉住一个真实修过的线上/可见故障,拿掉对应修复就会挂:
 *   ① api.js:登录被拒绝(403)被当成"管理员密钥无效",把人踢出后台;
 *   ② 账号页:每行 RPM 恒为 0(列表接口根本不返回这个字段);
 *   ③ 账号页:自动查询期间点「查询信息」静默无响应;
 *   ③b 账号页:自动查询期间重载列表(导入/删号/刷新),在飞那轮既不作废也不重启,
 *       继续照旧快照跑完 —— 新导入的账号永远查不到「剩余」;
 *   ③c 账号页:离开再进来会开出并发的第二轮余额扇出(扇出必须严格串行);
 *   ④ 账号页:失败/限流日志的"详情"列永远是「—」;
 *   ⑤ 账号页:清空昵称提示"已保存",实际后端根本没改;
 *   ⑥ API 管理:编辑保存失败也弹"API 密钥已保存";
 *   ⑦ 仪表盘 / 使用统计:统计接口挂了却印出 0.0000、$0.0000 冒充真实零流量;
 *   ⑧ 模型测试:只有主 API Key 时,发完第一条消息密钥下拉框被永久禁用。
 *
 * 分区文件是浏览器里的 IIFE,这里用 `new Function` 把它们注入一套**迷你 DOM**。
 * 迷你 DOM 只实现被测路径用得上的那部分(树 / class / 属性 / dataset / 事件),
 * 遇到 `host.innerHTML = <静态模板串>` 这种解析不了的情况,则按选择器返回稳定的
 * 替身元素,测试照样能读到它的 textContent。
 */
import test from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

const JS_DIR = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '../js');

// ---------------------------------------------------------------- 迷你 DOM ----

function clsList(el) { return String(el.className || '').split(/\s+/).filter(Boolean); }
function hasCls(el, c) { return clsList(el).indexOf(c) >= 0; }
function dataKey(attr) { return attr.replace(/^data-/, '').replace(/-([a-z])/g, (_, c) => c.toUpperCase()); }

// 单个复合选择器:tag / #id / .cls / [attr] / [attr="v"]
function matchCompound(el, compound) {
  if (!el || el.nodeType !== 1) return false;
  // 先把 [attr="v"] 整段摘出去再解析 tag/#id/.cls —— 属性值里带点号
  // (如 [data-i18n-title="common.edit"])会被当成类名要求,永远匹配不上。
  const attrs = [];
  const rest = String(compound).replace(/\[([\w-]+)(?:=["']?([^\]"']*)["']?)?\]/g, (_, name, val) => {
    attrs.push([name, val]);
    return '';
  });
  const tagM = /^([a-zA-Z][\w-]*)/.exec(rest);
  if (tagM && el.tagName !== tagM[1].toUpperCase()) return false;
  const idM = /#([\w-]+)/.exec(rest);
  if (idM && el.id !== idM[1]) return false;
  const clsRe = /\.([\w-]+)/g;
  let m;
  while ((m = clsRe.exec(rest))) { if (!hasCls(el, m[1])) return false; }
  for (const [name, want] of attrs) {
    let val = el.getAttribute(name);
    if (val == null && name.indexOf('data-') === 0) {
      const dk = dataKey(name);
      val = Object.prototype.hasOwnProperty.call(el.dataset, dk) ? el.dataset[dk] : null;
    }
    if (val == null) return false;
    if (want !== undefined && String(val) !== want) return false;
  }
  return true;
}

// 后代选择器:最后一段必须命中自身,前面各段按顺序在祖先链上命中。
function matchesSelector(el, sel) {
  const parts = String(sel).trim().split(/\s+/);
  if (!matchCompound(el, parts[parts.length - 1])) return false;
  let i = parts.length - 2;
  let node = el.parentNode;
  while (i >= 0 && node) {
    if (matchCompound(node, parts[i])) i--;
    node = node.parentNode;
  }
  return i < 0;
}

function walk(root, fn) {
  const kids = root.children || [];
  for (const c of kids) {
    if (c && c.nodeType === 1) {
      if (fn(c) === false) return false;
      if (walk(c, fn) === false) return false;
    }
  }
  return true;
}

function findFirst(root, sel) {
  let hit = null;
  walk(root, (el) => { if (matchesSelector(el, sel)) { hit = el; return false; } });
  return hit;
}
function findAll(root, sel) {
  const out = [];
  walk(root, (el) => { if (matchesSelector(el, sel)) out.push(el); });
  return out;
}

function textOf(node) {
  if (!node) return '';
  if (node.nodeType === 3) return node.text;
  return node._text + (node.children || []).map(textOf).join('');
}

function createDom() {
  const byId = new Map();

  function makeText(txt) {
    const n = { nodeType: 3, text: String(txt), children: [], parentNode: null, remove() {} };
    Object.defineProperty(n, 'textContent', { get() { return n.text; }, set(v) { n.text = String(v); } });
    return n;
  }

  function makeEl(tag) {
    const el = {
      nodeType: 1,
      tagName: String(tag || 'div').toUpperCase(),
      children: [],
      parentNode: null,
      className: '',
      style: {},
      dataset: {},
      attrs: {},
      listeners: {},
      checked: false, disabled: false, hidden: false, title: '', type: '',
      href: '', target: '', rel: '', src: '', alt: '', rows: 0, loading: '',
      _text: '', _id: '', _value: '', _memo: new Map()
    };
    Object.defineProperty(el, 'id', {
      get() { return el._id; },
      set(v) { el._id = String(v); byId.set(el._id, el); }
    });
    // 真实 DOM 的 input.value 恒为字符串(赋数字会被转成串);分区代码里的
    // fval() 直接 .trim(),不转成串这里就会炸,和浏览器行为对不上。
    Object.defineProperty(el, 'value', {
      get() { return el._value; },
      set(v) { el._value = v == null ? '' : String(v); }
    });
    Object.defineProperty(el, 'textContent', {
      get() { return textOf(el); },
      set(v) { el._text = v == null ? '' : String(v); el.children = []; }
    });
    Object.defineProperty(el, 'innerHTML', {
      get() { return ''; },
      set() { el._text = ''; el.children = []; }
    });
    Object.defineProperty(el, 'firstChild', { get() { return el.children[0] || null; } });
    el.classList = {
      add(c) { if (!hasCls(el, c)) el.className = (el.className + ' ' + c).trim(); },
      remove(c) { el.className = clsList(el).filter((x) => x !== c).join(' '); },
      toggle(c, on) {
        const want = on === undefined ? !hasCls(el, c) : !!on;
        if (want) el.classList.add(c); else el.classList.remove(c);
      },
      contains(c) { return hasCls(el, c); }
    };
    el.appendChild = (c) => { if (!c) return c; el.children.push(c); c.parentNode = el; return c; };
    el.insertBefore = (c, ref) => {
      const i = el.children.indexOf(ref);
      el.children.splice(i < 0 ? el.children.length : i, 0, c);
      c.parentNode = el;
      return c;
    };
    el.removeChild = (c) => {
      const i = el.children.indexOf(c);
      if (i >= 0) el.children.splice(i, 1);
      if (c) c.parentNode = null;
      return c;
    };
    el.remove = () => { if (el.parentNode) el.parentNode.removeChild(el); };
    el.setAttribute = (k, v) => { el.attrs[k] = String(v); if (k === 'id') el.id = v; };
    el.getAttribute = (k) => (Object.prototype.hasOwnProperty.call(el.attrs, k) ? el.attrs[k] : null);
    el.hasAttribute = (k) => Object.prototype.hasOwnProperty.call(el.attrs, k);
    el.removeAttribute = (k) => { delete el.attrs[k]; };
    el.addEventListener = (type, fn) => { (el.listeners[type] = el.listeners[type] || []).push(fn); };
    el.removeEventListener = () => {};
    el.querySelector = (sel) => {
      const hit = findFirst(el, sel);
      if (hit) return hit;
      // 解析不了的静态模板(host.innerHTML = '<...>')→ 只对 **#id** 选择器给一个
      // 稳定替身,init() 拿到的和测试里再查一次拿到的是同一个对象。其余选择器一律
      // 返回 null,否则像 `if (s.querySelector('input')) return;` 这种"存在即跳过"
      // 的守卫会被替身骗过,行为和浏览器对不上。
      if (!/^#[\w-]+$/.test(String(sel).trim())) return null;
      if (!el._memo.has(sel)) el._memo.set(sel, makeEl('div'));
      return el._memo.get(sel);
    };
    el.querySelectorAll = (sel) => findAll(el, sel);
    el.closest = () => null;
    el.contains = (n) => {
      let found = false;
      walk(el, (x) => { if (x === n) { found = true; return false; } });
      return found;
    };
    el.scrollIntoView = () => {};
    el.focus = () => {};
    el.select = () => {};
    el.click = () => fire(el, 'click');
    return el;
  }

  const document = {
    head: makeEl('head'),
    body: makeEl('body'),
    documentElement: makeEl('html'),
    createElement: makeEl,
    createElementNS: (_ns, tag) => makeEl(tag),
    createTextNode: makeText,
    getElementById: (id) => byId.get(id) || null,
    querySelector: (sel) => findFirst(document.body, sel),
    querySelectorAll: (sel) => findAll(document.body, sel),
    addEventListener() {},
    removeEventListener() {}
  };

  // 挂一个分区宿主节点(id 注册进 byId,并挂到 body 上供全局选择器命中)。
  function mountSection(id) {
    const host = makeEl('section');
    host.id = id;
    document.body.appendChild(host);
    return host;
  }

  return { document, mountSection, makeEl };
}

// 触发某个元素上注册的事件(冒泡等一律不模拟,够用即可)。
function fire(el, type, evt) {
  const list = (el && el.listeners && el.listeners[type]) || [];
  list.slice().forEach((fn) => fn(evt || { target: el, stopPropagation() {}, preventDefault() {} }));
}

// ---------------------------------------------------------------- 测试脚手架 ----

function apiError(status, message) {
  const e = new Error(message || 'request failed');
  e.name = 'ApiError';
  e.status = status;
  return e;
}

/* 假 K.api:记录每次调用、可按路径延迟/抛错;toast 全部留痕供断言。
 * 另外单独跟踪**余额请求**的同时在飞条数并留下峰值 maxInFlight —— 余额扇出必须
 * 严格串行,这个峰值是唯一能证明它没并发的观测点(列表/日用量之类的一次性请求
 * 不属于扇出,不计入)。 */
function makeApi(handler, opts) {
  opts = opts || {};
  const stats = { calls: [], puts: [], posts: [], toasts: [], inFlight: 0, maxInFlight: 0 };
  function run(method, pathname, body) {
    stats.calls.push(pathname);
    if (method === 'PUT') stats.puts.push({ path: pathname, body: body });
    if (method === 'POST') stats.posts.push({ path: pathname, body: body });
    const counted = pathname.endsWith('/balance');
    if (counted) {
      stats.inFlight++;
      if (stats.inFlight > stats.maxInFlight) stats.maxInFlight = stats.inFlight;
    }
    const delay = typeof opts.delay === 'function' ? opts.delay(pathname) : (opts.delay || 0);
    return new Promise((resolve, reject) => {
      setTimeout(() => {
        if (counted) stats.inFlight--;
        let out;
        try { out = handler(pathname, method, body); } catch (e) { reject(e); return; }
        resolve(out);
      }, delay);
    });
  }
  return {
    _stats: stats,
    get: (p) => run('GET', p),
    post: (p, b) => run('POST', p, b),
    put: (p, b) => run('PUT', p, b),
    del: (p, b) => run('DELETE', p, b),
    raw: (p) => run('GET', p),
    toast: (msg, type) => { stats.toasts.push({ msg: msg, type: type }); },
    hasAdminKey: () => true,
    setAdminKey: () => {},
    eventSource: () => ({ close() {}, addEventListener() {} }),
    onUnauthorized: null
  };
}

function loadSection(file, ctx) {
  const src = readFileSync(path.join(JS_DIR, file), 'utf8');
  const fn = new Function(
    'window', 'document', 'setInterval', 'clearInterval', 'fetch', 'localStorage', 'navigator',
    src
  );
  fn(ctx.window, ctx.document, () => 0, () => {}, ctx.fetch, ctx.localStorage, ctx.navigator || {});
}

function boot(file, sectionId, handler, opts) {
  const dom = createDom();
  const api = makeApi(handler, opts);
  const host = dom.mountSection(sectionId);
  const window = {
    K: { api: api },
    location: { origin: 'http://test.local' },
    addEventListener() {}, removeEventListener() {}
  };
  window.K.sections = {};
  loadSection(file, {
    window,
    document: dom.document,
    fetch: (opts && opts.fetch) || undefined,
    localStorage: { getItem: () => null, setItem: () => {}, removeItem: () => {} }
  });
  return { dom, api, window, host, sections: window.K.sections };
}

async function waitFor(cond, ms = 3000) {
  const deadline = Date.now() + ms;
  while (Date.now() < deadline) {
    if (cond()) return true;
    await new Promise((r) => setTimeout(r, 2));
  }
  return false;
}
async function settle(rounds = 40) {
  for (let i = 0; i < rounds; i++) await new Promise((r) => setTimeout(r, 2));
}

// ------------------------------------------------------------------ api.js ----

function loadApi(res) {
  const src = readFileSync(path.join(JS_DIR, 'api.js'), 'utf8');
  const store = new Map([['kiro2api_admin_key', 'ADMIN-KEY']]);
  const localStorage = {
    getItem: (k) => (store.has(k) ? store.get(k) : null),
    setItem: (k, v) => store.set(k, String(v)),
    removeItem: (k) => store.delete(k)
  };
  const dom = createDom();
  const window = { K: {} };
  const fetch = async () => ({
    status: res.status,
    ok: res.status >= 200 && res.status < 300,
    text: async () => res.body
  });
  new Function('window', 'document', 'fetch', 'localStorage', src)(window, dom.document, fetch, localStorage);
  return { api: window.K.api, store };
}

test('api: 登录被拒绝的 403 不该清会话、不该谎报"管理员密钥无效"', async () => {
  // AWS 设备页点「拒绝」→ 后端 LoginError::Denied → 403 +「授权被拒绝」。
  const { api, store } = loadApi({ status: 403, body: JSON.stringify({ success: false, error: '授权被拒绝' }) });
  let unauthorized = 0;
  api.onUnauthorized = () => { unauthorized++; };

  await assert.rejects(
    () => api.post('/login/builderid/poll', { sessionId: 'abc' }),
    (e) => {
      assert.equal(e.status, 403);
      // 修复前:被替换成 tr('login.error') =「管理员密钥无效」。
      assert.equal(e.message, '授权被拒绝', '403 的后端原因被吞掉了');
      return true;
    }
  );
  assert.equal(unauthorized, 0, '403 触发了登录浮层(把管理员踢下线)');
  assert.equal(store.get('kiro2api_admin_key'), 'ADMIN-KEY', '403 把本地管理员密钥清掉了');
});

test('api: 401 仍然清会话 + 弹登录浮层(别把修复做过头)', async () => {
  const { api, store } = loadApi({ status: 401, body: JSON.stringify({ error: 'unauthorized' }) });
  let unauthorized = 0;
  api.onUnauthorized = () => { unauthorized++; };

  await assert.rejects(() => api.get('/credentials'), (e) => e.status === 401);
  assert.equal(unauthorized, 1, '401 必须触发 onUnauthorized');
  assert.equal(store.has('kiro2api_admin_key'), false, '401 必须清掉本地管理员密钥');
});

// ---------------------------------------------------------------- 账号页 ----

function accountsHandler(extra) {
  return (p, method, body) => {
    if (p === '/credentials') {
      return {
        credentials: [
          { id: 1, nickname: 'acct-1', email: 'a@x.com', disabled: false, successCount: 3, failureCount: 2, throttleCount: 1 },
          { id: 2, nickname: 'acct-2', email: 'b@x.com', disabled: false, successCount: 0, failureCount: 0, throttleCount: 0 }
        ]
      };
    }
    if (p === '/usage/daily') return [];
    if (p === '/rpm') return { global: 12, byCredential: { 1: 12, 2: 5 }, byApiKey: {} };
    if (extra) {
      const r = extra(p, method, body);
      if (r !== undefined) return r;
    }
    if (p.endsWith('/balance')) return { remaining: 1 };
    return {};
  };
}

function bootAccounts(handler, opts) {
  return boot('sec-accounts.js', 'sec-accounts', handler, opts);
}

test('accounts: 每行 RPM 来自 /rpm 快照,而不是列表里根本不存在的字段', async () => {
  const { api, dom, sections } = bootAccounts(accountsHandler());
  sections.accounts.onShow();

  // 修复前:压根不会请求 /rpm(每行 fmtInt(undefined || 0) 恒印 0)。
  assert.ok(await waitFor(() => api._stats.calls.indexOf('/rpm') >= 0), '账号页从没请求过 /rpm');

  const cells = () => dom.document.querySelectorAll('.account-row[data-acct-id] .stat-n[data-rpm-cell]');
  assert.ok(await waitFor(() => cells().length === 2 && cells()[0].textContent === '12'), 'RPM 没有刷进行内');
  assert.equal(cells()[0].textContent, '12');
  assert.equal(cells()[1].textContent, '5');
});

test('accounts: 自动查询期间点「查询信息」要真的顶掉它,而不是静默无视', async () => {
  // 余额请求放慢,保证点击时自动那一轮还在跑(生产是上千个账号、好几分钟)。
  const { api, dom, sections } = bootAccounts(accountsHandler(), {
    delay: (p) => (p.endsWith('/balance') ? 30 : 0)
  });
  sections.accounts.onShow();

  const balanceCalls = () => api._stats.calls.filter((p) => p.endsWith('/balance')).length;
  assert.ok(await waitFor(() => balanceCalls() > 0), '自动查询应当已经开跑');

  const btn = dom.document.getElementById('accountsQueryInfoBtn');
  assert.ok(btn, '工具栏里应当有「查询信息」按钮');
  fire(btn, 'click');

  // 修复前:queryAllInfo 开头 `if (queryingInfo) return;` —— 按钮不禁用、不提示、
  // 什么都不发生,用户只会以为按钮坏了。
  assert.equal(btn.disabled, true, '点击后按钮没有任何反馈(看起来像坏了)');

  // 手动这一轮跑完会弹一条汇总提示(自动那轮 toast=false,不会弹)。
  assert.ok(await waitFor(() => api._stats.toasts.some((x) => x.msg === 'imp.result'), 5000),
    '手动查询从没真正执行过');
  assert.equal(btn.disabled, false, '手动轮结束后按钮应当解禁');
});

test('accounts: 列表重载要顶掉在飞的余额扇出(新导入的账号必须查得到「剩余」)', async () => {
  // 场景 = 生产上最常见的那一步:池子很大、进页面的自动查询要跑好几分钟,运维在这
  // 期间做了一次批量导入/删号(或者只是点了「刷新」),列表换了一批账号。
  //
  // 修复前 autoQueryBalances 开头是 `if (queryingInfo) return;`(而且在 ++token
  // 之前),重载既不作废也不重启在飞的那一轮 —— 它继续照着**重载前**的快照往下走:
  // 新导入的账号一个都查不到、「剩余」永远是未设置,刚删掉的账号还在被请求余额。
  let roster = [1, 2, 3, 4, 5, 6, 7, 8];
  const { api, dom, sections } = bootAccounts((p) => {
    if (p === '/credentials') {
      return { credentials: roster.map((id) => ({ id: id, nickname: 'acct-' + id, disabled: false })) };
    }
    if (p === '/usage/daily') return [];
    if (p === '/rpm') return { global: 0, byCredential: {}, byApiKey: {} };
    if (p.endsWith('/balance')) return { remaining: 12.5 };
    return {};
  }, { delay: (pth) => (pth.endsWith('/balance') ? 25 : 0) });

  const balancePaths = () => api._stats.calls.filter((p) => p.endsWith('/balance'));
  sections.accounts.onShow();
  assert.ok(await waitFor(() => balancePaths().length >= 2), '自动查询应当已经开跑');

  // 批量导入 / 删号之后的那次 loadAccounts():4~8 没了,31/32 是新进来的。
  roster = [1, 2, 3, 31, 32];
  const cut = api._stats.calls.length;
  const reloadBtn = dom.document.querySelector('.section-actions button[data-i18n="common.refresh"]');
  assert.ok(reloadBtn, '工具栏里应当有「刷新」按钮');
  fire(reloadBtn, 'click');

  // ① 新账号必须真的被查一遍(修复前:这两发请求一辈子不会出现)。
  assert.ok(
    await waitFor(() => {
      const after = balancePaths();
      return after.indexOf('/credentials/31/balance') >= 0 && after.indexOf('/credentials/32/balance') >= 0;
    }, 5000),
    '重载后新导入的账号从没被查过余额'
  );
  await settle();

  // ② 已经删掉的账号不该再被请求余额(允许重载那一刻手上正在飞的那一发落地)。
  const staleAfterReload = api._stats.calls.slice(cut)
    .filter((p) => /^\/credentials\/([4-8])\/balance$/.test(p));
  assert.ok(
    staleAfterReload.length <= 1,
    `重载后仍在请求已删除账号的余额:${JSON.stringify(staleAfterReload)}`
  );

  // ③ 运维眼里的最终样子:新列表每一行都有数字「剩余」,不再停在「未设置」。
  const balCell = (id) => dom.document.querySelector(
    '.account-row[data-acct-id="' + id + '"] .stat-n[data-bal-cell]'
  );
  roster.forEach((id) => {
    const cell = balCell(id);
    assert.ok(cell, '账号 #' + id + ' 的行没渲染出来');
    assert.equal(cell.textContent, '12.5', '账号 #' + id + ' 的「剩余」没被填上');
  });
});

test('accounts: 离开再进来不能开出并发的第二轮扇出', async () => {
  // 上一条修好"重载要顶掉在飞那轮"之后,自动查询改成了"等在飞那轮收工再开新一轮"。
  // 这就要求 queryingInfo 必须诚实:onHide 若照旧顺手把它清零,手上那一发请求还没
  // 落地就会被放行,第二轮当场开跑 —— 两轮同时打上游,正是扇出注释里那条"绝不成批
  // 爆发"的硬约束不许发生的事。
  const { api, sections } = bootAccounts(accountsHandler(), {
    delay: (p) => (p.endsWith('/balance') ? 40 : 0)
  });

  sections.accounts.onShow();
  assert.ok(await waitFor(() => api._stats.calls.some((p) => p.endsWith('/balance'))), '自动查询应当已经开跑');

  // 切走再切回来:此刻第一轮手上那一发余额请求(40ms)还在飞。
  sections.accounts.onHide();
  sections.accounts.onShow();
  await settle();

  assert.equal(api._stats.maxInFlight, 1, '离开再进来开出了并发的第二轮余额扇出');
});

test('accounts: 失败日志弹窗要显示上游响应体,而不是一列「—」', async () => {
  const BODY = 'ThrottlingException: too many requests';
  const { dom, sections } = bootAccounts(accountsHandler((p) => {
    if (p.indexOf('/failure-logs') >= 0) {
      return {
        records: [{ credentialId: 1, requestType: 'api', statusCode: 403, responseBody: BODY, createdAt: '2026-07-26T10:00:00Z' }],
        total: 1, page: 1, pageSize: 30
      };
    }
    return undefined;
  }));
  sections.accounts.onShow();

  // 失败计数格:kind='bad'(优先级格也是 is-link,但它是 neutral)。
  const failStat = () => dom.document.querySelectorAll('.account-row .account-stat.bad.is-link')[0];
  assert.ok(await waitFor(() => !!failStat()), '账号行应当已经渲染');
  fire(failStat(), 'click');

  // 修复前:详情格读 lg.message/detail/error —— 端点回的是 responseBody,全是「—」。
  assert.ok(await waitFor(() => textOf(dom.document.body).indexOf(BODY) >= 0),
    '失败日志弹窗没有显示上游响应体(详情列还是 —)');
});

test('accounts: 清空昵称不能谎报"已保存"(后端把空串当作不修改)', async () => {
  const { api, dom, sections } = bootAccounts(accountsHandler());
  sections.accounts.onShow();

  const editBtn = () => dom.document.querySelectorAll('.account-row [data-i18n-title="common.edit"]')[0];
  assert.ok(await waitFor(() => !!editBtn()), '账号行应当已经渲染');
  fire(editBtn(), 'click');

  const nick = dom.document.getElementById('accNick');
  assert.ok(nick, '编辑弹窗应当有昵称输入框');
  assert.equal(nick.value, 'acct-1');
  nick.value = '';   // 用户清空昵称

  const saveBtn = dom.document.querySelectorAll('.modal-footer button').filter((b) => b.textContent === 'common.save')[0];
  assert.ok(saveBtn, '编辑弹窗应当有保存按钮');
  fire(saveBtn, 'click');
  await settle(10);

  // 修复前:发出 {"nickname":""} → 后端 200 但丢弃该字段 → 弹「账号已保存」,
  // 而身后那一行昵称纹丝不动。
  assert.deepEqual(api._stats.puts, [], '不该把注定被丢弃的空串发给后端');
  const msgs = api._stats.toasts.map((x) => x.msg);
  assert.ok(msgs.indexOf('edit.cannotClear') >= 0, '应当明确告诉用户昵称无法清空');
  assert.ok(msgs.indexOf('acc.saved') < 0, '什么都没改却弹了"已保存"');
});

// -------------------------------------------------------------- API 管理 ----

test('apikeys: 编辑保存失败要说"保存失败",不能弹"API 密钥已保存"', async () => {
  const key = { id: 1, name: 'k1', key: 'sk-aaaabbbbccccdddd', enabled: true, spendingLimit: 10, limitUnit: 'usd' };
  const { api, dom, sections } = boot('sec-apikeys.js', 'sec-apikeys', (p, method) => {
    if (p === '/api-keys' && method === 'GET') return [key];
    if (p === '/api-keys/usage') return [];
    if (p === '/server-info') return {};
    if (p === '/rpm') return { global: 0, byCredential: {}, byApiKey: {} };
    if (p === '/credentials') return { credentials: [] };
    if (p === '/api-keys/1' && method === 'PUT') throw apiError(500, 'persist failed');
    return {};
  });
  sections.apikeys.onShow();

  const editBtn = () => dom.document.querySelectorAll('.key-card [data-i18n-title="common.edit"]')[0];
  assert.ok(await waitFor(() => !!editBtn()), '密钥卡片应当已经渲染');
  fire(editBtn(), 'click');
  await settle(5);

  const saveBtn = dom.document.querySelectorAll('.modal-footer button').filter((b) => b.textContent === 'common.save')[0];
  assert.ok(saveBtn, '编辑弹窗应当有保存按钮');
  fire(saveBtn, 'click');

  assert.ok(await waitFor(() => api._stats.toasts.some((x) => x.type === 'error')), 'PUT 失败应当有错误提示');
  const err = api._stats.toasts.filter((x) => x.type === 'error').pop().msg;
  // 修复前:'key.saved' + ': ' + 错误 →「API 密钥已保存: persist failed」。
  assert.ok(err.indexOf('keyForm.saveFailed') === 0, '失败提示复用了成功文案: ' + err);
  assert.ok(err.indexOf('key.saved') < 0, '失败提示里不该出现"已保存"');
});

// ------------------------------------------------------ 仪表盘 / 使用统计 ----

test('dashboard: 日用量接口挂掉时今日两张卡显示 —,不能伪造 0', async () => {
  const { host, sections } = boot('sec-dashboard.js', 'sec-dashboard', (p) => {
    if (p === '/usage/daily') throw apiError(500, 'stats down');
    if (p === '/credentials') return { credentials: [], total: 0, available: 0 };
    if (p === '/models') return { data: [] };
    if (p === '/server-info') return { version: '0.2.0' };
    if (p === '/config') return {};
    if (p === '/check-update') return { hasUpdate: false };
    if (p === '/credits/global') return { globalCredits: 0, cachedCount: 0, totalCount: 0, oldestCacheUnix: null };
    return {};
  });
  sections.dashboard.onShow();

  const credits = host.querySelector('#dashTodayCredits');
  const cost = host.querySelector('#dashTodayCost');
  // 等 render() 里那批 allSettled 全部落地(账号卡被写过就说明渲染跑完了)。
  assert.ok(await waitFor(() => host.querySelector('#dashAccounts').textContent !== ''), '仪表盘应当已渲染');
  await settle(10);
  // 修复前:'0.0000' / '$0.0000' —— 和真实的零流量长得一模一样。
  assert.equal(credits.textContent, '—', '统计接口失败却印出了积分 0');
  assert.equal(cost.textContent, '—', '统计接口失败却印出了费用 0');
});

test('usage: 汇总接口失败时费用/积分卡显示 —,不能伪造 $0.0000', async () => {
  const { host, sections } = boot('sec-usage.js', 'sec-usage', (p) => {
    if (p.indexOf('/usage/summary') >= 0) throw apiError(502, 'stats down');
    if (p === '/usage/daily') return [];
    return {};
  });
  sections.usage.onShow();
  await settle(15);

  // 修复前:请求数/错误率/延迟是 '—',费用和积分却是 '$0.0000' / '0.0000'。
  assert.equal(host.querySelector('#usSumCost').textContent, '—', '汇总失败却印出了费用 0');
  assert.equal(host.querySelector('#usSumCredits').textContent, '—', '汇总失败却印出了积分 0');
  assert.equal(host.querySelector('#usSumRequests').textContent, '—');
});

// ---------------------------------------------------------------- 模型测试 ----

test('modeltest: 只配了主 API Key 时,发完一条消息密钥下拉框不能被锁死', async () => {
  const MASTER = 'sk-master-1234567890';
  const fetchStub = async () => ({ ok: false, status: 500, text: async () => '' });
  const { host, sections } = boot('sec-modeltest.js', 'sec-modeltest', (p) => {
    if (p === '/server-info') return { masterApiKey: MASTER };
    if (p === '/api-keys') return [];            // 一个密钥都没创建 —— 明确支持的场景
    if (p === '/models') return { data: [{ id: 'model-a' }] };
    return {};
  }, { fetch: fetchStub });

  sections.modeltest.onShow();
  const keySel = host.querySelector('#mtKey');
  assert.ok(await waitFor(() => keySel.disabled === false), '有主 key 时下拉框应当可用');

  // 模拟一次发送(上游 500,走 !res.ok 分支 → setSending(false))。
  keySel.value = MASTER;
  host.querySelector('#mtModel').value = 'model-a';
  host.querySelector('#mtPrompt').value = 'hi';
  fire(host.querySelector('#mtSendBtn'), 'click');
  await settle(20);

  // 修复前:setSending(false) 里 `on || !state.keys.length` → 没创建密钥就恒 true,
  // 下拉框被永久禁用,整场会话都点不开。
  assert.equal(keySel.disabled, false, '发送结束后密钥下拉框被锁死了');
});
