/* Qoder Switch 前端逻辑（Tauri invoke） */
const { invoke } = window.__TAURI__.core;

let STATE = null;          // 账号页状态
let SESS = null;           // 会话页状态
let CSTATS = null;         // 积分统计
let TSTATS = {};           // Token 统计（按版本缓存）
let SESS_FILTER = 'all';
let CREDIT = { view: 'all', shareRange: 'today', chartRange: '30d', chartAcc: 'all', detailAcc: null };
let TSTAT = { src: 'cn', range: '30d', dist: 'projects', page: 0 };

/* ───────── 基础 UI ───────── */
function esc(s) {
  return String(s ?? '').replace(/[&<>"]/g, c => ({ '&': '&amp;', '<': '&lt;', '>': '&gt;', '"': '&quot;' }[c]));
}
function toast(m, ok) {
  const t = document.getElementById('toast');
  t.textContent = m;
  t.style.background = ok === false ? '#a8323a' : '#2c3038';
  t.classList.add('show');
  clearTimeout(t._tm);
  t._tm = setTimeout(() => t.classList.remove('show'), 2600);
}
function fmtBytes(n) {
  if (!n) return '—';
  if (n > 1048576) return (n / 1048576).toFixed(1) + ' MB';
  if (n > 1024) return (n / 1024).toFixed(0) + ' KB';
  return n + ' B';
}
function cnNum(n) {  // 中文单位：不足 1 万直接显示数字
  n = Number(n) || 0;
  if (n < 1e4) return String(Math.round(n));
  if (n < 1e8) { const v = n / 1e4; return (v >= 100 ? v.toFixed(0) : v.toFixed(1)) + ' 万'; }
  const v = n / 1e8; return (v >= 100 ? v.toFixed(0) : v.toFixed(2)) + ' 亿';
}
function fmtTok(n) {
  n = Number(n) || 0;
  if (TOKFMT === 'cn') return cnNum(n);
  if (n >= 1e6) return (n / 1e6).toFixed(1) + 'M';
  if (n >= 1e4) return (n / 1e3).toFixed(1) + 'K';
  return String(Math.round(n));
}
/* 统计页加载占位（首次打开 / 手动刷新时） */
function showLoading(metricsId, blockIds) {
  const m = document.getElementById(metricsId);
  if (m) m.innerHTML = `<div class="loading-wrap" style="grid-column:1/-1"><div class="loading-ring"></div><div class="loading-txt">${tr('正在加载统计数据', 'Loading statistics')}</div></div>`;
  (blockIds || []).forEach(id => {
    const el = document.getElementById(id);
    if (el) el.innerHTML = `<div class="loading-wrap"><div class="loading-ring"></div><div class="loading-txt">${tr('正在加载', 'Loading')}</div></div>`;
  });
}
/* ───────── 主题（浅色 / 深色） ───────── */
function applyTheme(t) {
  document.body.classList.toggle('dark', t === 'dark');
  try { localStorage.setItem('qs-theme', t); } catch (_) {}
  try { invoke('set_theme', { theme: t }).catch(() => {}); } catch (_) {}
  renderThemeSeg();
}
function currentTheme() {
  let t = 'light';
  try { t = localStorage.getItem('qs-theme') || 'light'; } catch (_) {}
  return t === 'dark' ? 'dark' : 'light';
}
function renderThemeSeg() {
  const el = document.getElementById('themeSeg');
  if (!el) return;
  const cur = currentTheme();
  el.innerHTML = [['light', tr('浅色', 'Light')], ['dark', tr('深色', 'Dark')]].map(([k, l]) =>
    `<button class="${cur === k ? 'active' : ''}" onclick="window.__setTheme('${k}')">${l}</button>`).join('');
}
window.__setTheme = (t) => { applyTheme(t); };

/* ───────── 语言 / 时区 / 数字格式 ───────── */
/* LANG: 'system' | 'zh' | 'en'；TZ: 'system' | IANA；TOKFMT: 'km' | 'cn' */
let LANG = 'system', TZ = 'system', TOKFMT = 'km';
try { LANG = localStorage.getItem('qs-lang') || 'system'; } catch (_) {}
try { TZ = localStorage.getItem('qs-tz') || 'system'; } catch (_) {}
try { TOKFMT = localStorage.getItem('qs-tokfmt') || 'km'; } catch (_) {}
function effLang() {
  if (LANG === 'zh' || LANG === 'en') return LANG;
  return (navigator.language || '').toLowerCase().startsWith('zh') ? 'zh' : 'en';
}
/* 文案切换：t('中文', 'English') */
function tr(zh, en) { return effLang() === 'en' ? en : zh; }
/* 后端中文消息的前端映射（英文界面时替换为英文） */
const TMSG = [
  [/^领取成功 \+(.+)$/, '+$1 Credits claimed'],
  [/今日无可领积分（已领过或未刷新）/, 'No claimable credits today (already claimed or not refreshed)'],
  [/^领取失败（(.+)）$/, 'Claim failed ($1)'],
  [/^干跑：可领 (.+) 项$/, 'Dry run: $1 claimable'],
  [/^查询活动失败 HTTP /, 'Campaign query failed HTTP '],
  [/^已切换到 (.+)（(.+)）$/, 'Switched to $1 ($2)'],
  [/^当前已是该账号（(.+)）$/, 'Already on this account ($1)'],
  [/^正在退出 (.+)…$/, 'Quitting $1…'],
  [/^无法退出 (.+)，请手动关闭后重试$/, 'Cannot quit $1 — close it manually and retry'],
  [/^正在写入目标账号（(.+)）登录态…$/, 'Writing login state for $1…'],
  [/^正在启动 (.+)…$/, 'Launching $1…'],
  [/^(.+) 当前未登录或读取失败$/, '$1 not signed in or read failed'],
  [/^(.+) 当前未登录$/, '$1 not signed in'],
  [/^账号不存在：(.+)$/, 'Account not found: $1'],
  [/^AES 解密失败/, 'AES decryption failed'],
  [/^钥匙串读取失败\((.+)\)/, 'Keychain read failed ($1)'],
  [/^钥匙串读取超时（(.+)）：(.+)$/, 'Keychain read timed out ($1): $2'],
  [/Qoder CN 国内版/g, 'Qoder CN Edition'],
  [/Qoder 国际版/g, 'Qoder Intl Edition'],
  [/国内版/g, 'CN Edition'],
  [/国际版/g, 'Intl Edition'],
];
function tmsg(s) {
  s = String(s ?? '');
  if (effLang() !== 'en') return s;
  for (const [re, to] of TMSG) s = s.replace(re, to);
  return s;
}
function edLabel(e) { return e === 'cn' ? tr('国内版', 'CN Edition') : tr('国际版', 'Intl Edition'); }
function edShort(e) { return e === 'cn' ? tr('国内', 'CN') : tr('国际', 'Intl'); }

const TZ_OPTIONS = [
  ['system', null], ['UTC', 'UTC'], ['Asia/Shanghai', 'UTC+8'], ['Asia/Hong_Kong', 'UTC+8'],
  ['Asia/Tokyo', 'UTC+9'], ['Asia/Singapore', 'UTC+8'], ['Europe/London', 'UTC+0'],
  ['Europe/Berlin', 'UTC+1'], ['America/New_York', 'UTC-5'], ['America/Los_Angeles', 'UTC-8'],
];
function tzLabel(k) {
  if (k === 'system') return tr('跟随系统', 'Follow System');
  const o = TZ_OPTIONS.find(x => x[0] === k);
  return o && o[1] ? `${k}（${o[1]}）` : k;
}
function renderPrefSelects() {
  const ls = document.getElementById('langSel');
  if (ls) ls.innerHTML = [['system', tr('跟随系统', 'Follow System')], ['zh', '中文'], ['en', 'English']]
    .map(([k, l]) => `<option value="${k}" ${LANG === k ? 'selected' : ''}>${l}</option>`).join('');
  const zs = document.getElementById('tzSel');
  if (zs) zs.innerHTML = TZ_OPTIONS.map(([k]) =>
    `<option value="${k}" ${TZ === k ? 'selected' : ''}>${tzLabel(k)}</option>`).join('');
}
window.setPref = async (key, value) => {
  if (key === 'lang') { LANG = value; try { localStorage.setItem('qs-lang', value); } catch (_) {} }
  if (key === 'timezone') { TZ = value; try { localStorage.setItem('qs-tz', value); } catch (_) {} }
  if (key === 'tokenFmt') { TOKFMT = value; try { localStorage.setItem('qs-tokfmt', value); } catch (_) {} }
  try { await invoke('set_setting', { key, value }); } catch (e) { toast(tmsg(String(e)), false); }
  if (key === 'lang') { location.reload(); return; }  // 语言切换整页重载，最干净
  applyI18nStatic();
  renderPrefSelects();
  renderThemeSeg();
  /* 重渲染当前页，让时间/数字立即按新偏好显示 */
  const active = document.querySelector('.page.active');
  if (active) {
    const pg = active.id.replace('page-', '');
    if (pg === 'accounts') load();
    else if (pg === 'sessions') loadSessions();
    else if (pg === 'credits') loadCreditStats();
    else if (pg === 'tokens') renderTokenPage();
    else if (pg === 'settings') renderSettingsTimes();
  }
};
/* 设置页上依赖时区的时间文本（领取记录 + 上次自动领取） */
function renderSettingsTimes() {
  renderLog();
  const lr = document.getElementById('lastRun');
  if (lr) lr.textContent =
    STATE && STATE.scheduler.lastRun ? (tr('上次自动领取 ', 'Last auto-claim ') + fmtTimeStr(STATE.scheduler.lastRun)) : tr('立即检查所有账号的可领活动', 'Check claimable campaigns for all accounts now');
}

/* 静态文案按 data-zh/data-en 属性切换 */
function applyI18nStatic() {
  const en = effLang() === 'en';
  document.querySelectorAll('[data-zh]').forEach(el => {
    const v = en ? el.dataset.en : el.dataset.zh;
    if (v != null) el.textContent = v;
  });
  document.querySelectorAll('[data-zh-ph]').forEach(el => {
    el.placeholder = en ? el.dataset.enPh : el.dataset.zhPh;
  });
}

/* 时区感知的时间显示（仅展示层；统计口径仍为系统时区） */
function tzParts(d, withSec) {
  if (TZ === 'system') return null;
  try {
    const f = new Intl.DateTimeFormat('en-CA', { timeZone: TZ, year: 'numeric', month: '2-digit', day: '2-digit', hour: '2-digit', minute: '2-digit', ...(withSec ? { second: '2-digit' } : {}), hour12: false });
    const o = {};
    for (const p of f.formatToParts(d)) o[p.type] = p.value;
    const hm = `${o.year}-${o.month}-${o.day} ${o.hour === '24' ? '00' : o.hour}:${o.minute}`;
    return withSec ? hm + ':' + (o.second === '24' ? '00' : (o.second || '00')) : hm;
  } catch (_) { return null; }
}

/* 后端存储的是系统时区的 "YYYY-MM-DD HH:MM:SS" 字符串，展示时按 TZ 偏好转换 */
function fmtTimeStr(s) {
  const str = String(s || '');
  const m = str.match(/^(\d{4})-(\d{2})-(\d{2})[ T](\d{2}):(\d{2}):(\d{2})/);
  if (!m || TZ === 'system') return str;
  const d = new Date(+m[1], +m[2] - 1, +m[3], +m[4], +m[5], +m[6]);
  return tzParts(d, true) || str;
}

applyTheme(currentTheme());

/* ───────── 指标卡 SVG 图标 ───────── */
const ICONS = {
  coins:  '<circle cx="8.5" cy="8.5" r="5.5"/><path d="M17.8 10.6a5.5 5.5 0 1 1-7.2 7.2"/><path d="M7 6.8h3M8.5 6.8v5"/>',
  down:   '<path d="m22 17-8.5-8.5-5 5L2 7"/><path d="M16 17h6v-6"/>',
  cal:    '<rect x="3" y="4.5" width="18" height="17" rx="2.5"/><path d="M16 2.5v4M8 2.5v4M3 10.5h18"/>',
  clock:  '<circle cx="12" cy="12" r="9"/><path d="M12 7v5l3.5 2"/>',
  layers: '<path d="m12 2.5 9.5 5.5L12 13.5 2.5 8z"/><path d="m2.5 12.5 9.5 5.5 9.5-5.5"/><path d="m2.5 17 9.5 5.5L21.5 17"/>',
  tin:    '<path d="M12 3v11"/><path d="m7 9.5 5 5 5-5"/><path d="M4.5 20.5h15"/>',
  tout:   '<path d="M12 21V10"/><path d="m7 14.5 5-5 5 5"/><path d="M4.5 3.5h15"/>',
  bolt:   '<path d="M13 2.5 4.5 13.5H11l-1 8 8.5-11H12z"/>',
};
const ic = (n) => `<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round">${ICONS[n] || ''}</svg>`;
function fmtMs(ms) {
  if (!ms) return '—';
  const s = tzParts(new Date(Number(ms)));
  if (s) return s;
  const d = new Date(Number(ms));
  return d.getFullYear() + '-' + String(d.getMonth() + 1).padStart(2, '0') + '-' + String(d.getDate()).padStart(2, '0')
    + ' ' + String(d.getHours()).padStart(2, '0') + ':' + String(d.getMinutes()).padStart(2, '0');
}
function fmtNum(v, max = 2) {
  return Number(v || 0).toLocaleString('zh-CN', { maximumFractionDigits: max });
}
function fmtDur(ms) {
  if (!ms || ms <= 0) return '—';
  if (ms < 60000) return Math.round(ms / 1000) + ' ' + tr('秒', 's');
  const m = Math.floor(ms / 60000);
  return m + ' ' + tr('分', 'm') + ' ' + Math.round((ms % 60000) / 1000) + ' ' + tr('秒', 's');
}
function nowStr() {
  const s = tzParts(new Date());
  if (s) return s.slice(5);
  const d = new Date();
  return `${d.getMonth() + 1}-${String(d.getDate()).padStart(2, '0')} ${String(d.getHours()).padStart(2, '0')}:${String(d.getMinutes()).padStart(2, '0')}`;
}
function dateKeyOf(d) {
  return d.getFullYear() + '-' + String(d.getMonth() + 1).padStart(2, '0') + '-' + String(d.getDate()).padStart(2, '0');
}
function daysAgoKey(n) {
  const d = new Date(); d.setDate(d.getDate() - n);
  return dateKeyOf(d);
}
/* ── 自定义时区感知的日期键（统计/筛选口径与所选时区一致）──
   TZ=system → 本地时区；否则按所选 IANA 时区的墙上时间 */
function tzOffsetMin() {
  if (TZ === 'system') return null;
  try {
    const now = new Date();
    const f = new Intl.DateTimeFormat('en-CA', { timeZone: TZ, year: 'numeric', month: '2-digit', day: '2-digit', hour: '2-digit', minute: '2-digit', second: '2-digit', hour12: false });
    const o = {};
    for (const p of f.formatToParts(now)) o[p.type] = p.value;
    const wall = `${o.year}-${o.month}-${o.day}T${o.hour === '24' ? '00' : o.hour}:${o.minute}:${o.second}Z`;
    return Math.round((Date.parse(wall) - Math.floor(now.getTime() / 1000) * 1000) / 60000);
  } catch (_) { return 0; }
}
function tzShifted() { /* 一个 UTC 字段 = 目标时区墙上时间的 Date */
  const o = TZ === 'system' ? -new Date().getTimezoneOffset() : (tzOffsetMin() || 0);
  return new Date(Date.now() + o * 60000);
}
function tzTodayKey() { return tzShifted().toISOString().slice(0, 10); }
function tzDaysAgoKey(n) { return new Date(tzShifted().getTime() - n * 864e5).toISOString().slice(0, 10); }

/* 自定义弹窗（WKWebView 不支持 alert/confirm/prompt） */
let modalResolve = null;
let modalOnClose = null;
function showModal({ title, bodyHTML, buttons, tabs, onTab }) {
  document.getElementById('modalTitle').innerHTML = title;
  document.getElementById('modalBody').innerHTML = bodyHTML;
  const tabsEl = document.getElementById('modalTabs');
  const foot = document.getElementById('modalFoot');
  foot.innerHTML = '';
  (buttons || [{ label: '好', primary: true }]).forEach(b => {
    const btn = document.createElement('button');
    if (b.primary) btn.className = 'primary';
    if (b.danger) btn.className = 'danger';
    btn.textContent = b.label;
    btn.onclick = () => {
      if (b.onClick) { b.onClick(btn); return; }
      closeModal(b.value);
    };
    foot.appendChild(btn);
  });
  if (tabs && tabs.length) {
    tabsEl.style.display = 'flex';
    tabsEl.innerHTML = '';
    tabs.forEach((t, i) => {
      const el = document.createElement('div');
      el.className = 'mtab' + (i === 0 ? ' active' : '');
      el.textContent = t;
      el.onclick = () => {
        tabsEl.querySelectorAll('.mtab').forEach(x => x.classList.remove('active'));
        el.classList.add('active');
        onTab && onTab(i);
      };
      tabsEl.appendChild(el);
    });
  } else tabsEl.style.display = 'none';
  document.getElementById('modalMask').classList.add('show');
}
function closeModal(value) {
  document.getElementById('modalMask').classList.remove('show');
  if (modalOnClose) { const f = modalOnClose; modalOnClose = null; f(); }
  if (modalResolve) { modalResolve(value); modalResolve = null; }
}
function confirmDialog(text, okLabel = '确定', danger = false) {
  return new Promise(res => {
    modalResolve = res;
    showModal({
      title: '确认',
      bodyHTML: esc(text),
      buttons: [
        { label: '取消', value: false },
        { label: okLabel, primary: !danger, danger, value: true },
      ],
    });
  });
}
function promptDialog(title, label, value = '') {
  return new Promise(res => {
    modalResolve = res;
    showModal({
      title: esc(title),
      bodyHTML: `<div style="margin-bottom:8px;color:var(--muted)">${esc(label)}</div>
                 <input type="text" id="modalInput" value="${esc(value)}">`,
      buttons: [
        { label: '取消', value: null },
        { label: '保存', primary: true, onClick: () => closeModal(document.getElementById('modalInput').value) },
      ],
    });
    setTimeout(() => document.getElementById('modalInput')?.focus(), 60);
  });
}
function showProgress(title) {
  document.getElementById('progressTitle').textContent = title;
  document.getElementById('progressSteps').textContent = '';
  document.getElementById('progress').classList.add('show');
}
function hideProgress(steps) {
  document.getElementById('progress').classList.remove('show');
  if (steps && steps.length) toast(steps[steps.length - 1], true);
}

/* ───────── 导航 ───────── */
function hideChartTip() { if (_ctip) _ctip.style.display = 'none'; }
document.querySelectorAll('.nav-item').forEach(el => {
  el.onclick = () => {
    hideChartTip();
    document.querySelectorAll('.dd-menu').forEach(m => { m.style.display = 'none'; });
    document.querySelectorAll('.nav-item').forEach(x => x.classList.remove('active'));
    el.classList.add('active');
    document.querySelectorAll('.page').forEach(p => p.classList.remove('active'));
    document.getElementById('page-' + el.dataset.page).classList.add('active');
    if (el.dataset.page === 'sessions') loadSessions();
    if (el.dataset.page === 'credits') loadCreditStats();
    if (el.dataset.page === 'tokens') loadTokenStats();
    if (el.dataset.page === 'accounts') load();
  };
});

/* ═════════ 账号页 ═════════ */
async function load() {
  try {
    STATE = await invoke('get_state');
    /* 偏好以 ~/.qoder-switch/settings.json 为准（重装/迁移不丢失） */
    const sp = STATE.settings || {};
    if (sp.theme === 'dark' || sp.theme === 'light') applyTheme(sp.theme);
    if ((sp.lang || 'system') !== LANG) {
      LANG = sp.lang || 'system';
      try { localStorage.setItem('qs-lang', LANG); } catch (_) {}
      location.reload();
      return;
    }
    if ((sp.timezone || 'system') !== TZ) {
      TZ = sp.timezone || 'system';
      try { localStorage.setItem('qs-tz', TZ); } catch (_) {}
    }
    if ((sp.tokenFmt || 'km') !== TOKFMT) {
      TOKFMT = sp.tokenFmt || 'km';
      try { localStorage.setItem('qs-tokfmt', TOKFMT); } catch (_) {}
    }
    renderPrefSelects();
    renderAccounts();
    renderLog();
    document.getElementById('autoToggle').classList.toggle('on', !!STATE.settings.autoClaim);
    document.getElementById('lastRun').textContent =
      STATE.scheduler.lastRun ? (tr('上次自动领取 ', 'Last auto-claim ') + fmtTimeStr(STATE.scheduler.lastRun)) : tr('立即检查所有账号的可领活动', 'Check claimable campaigns for all accounts now');
  } catch (e) {
    document.getElementById('accountsBody').innerHTML =
      `<div class="section"><div class="empty" style="color:var(--red)">${tr('初始化失败：', 'Init failed: ')}${esc(String(e))}</div></div>`;
  }
}

function edSection(e) {
  const ed = STATE.editions[e];
  if (!ed || !ed.installed) return '';
  const accs = STATE.accounts.filter(a => a.edition === e);
  let h = `<div class="section"><div class="sec-head">
    <h2>${esc(tmsg(ed.label))}</h2>
    <span class="pill"><span class="dot ${ed.running ? 'on' : ''}"></span>${ed.running ? tr('运行中', 'Running') : tr('未运行', 'Not Running')}</span>
    ${ed.currentName ? `<span class="pill on">${tr('当前：', 'Current: ')}${esc(ed.currentName)}</span>` : `<span class="pill off">${tr('未登录', 'Not Signed In')}</span>`}
    <div style="margin-left:auto;display:flex;gap:7px">
      <button onclick="openAddAccount('${e}',this)">＋ ${tr('添加账号', 'Add Account')}</button>
      <button onclick="capture('${e}',this)">${tr('采集当前账号', 'Capture Current')}</button>
      <button onclick="openApp('${e}')">${tr('打开应用', 'Open App')}</button>
    </div></div>`;
  if (!accs.length) h += `<div class="empty">${tr('账号库暂无该版本账号 —— 可「添加账号」走浏览器登录，或先在客户端登录后「采集当前账号」', 'No accounts for this edition yet — use “Add Account” to sign in via browser, or “Capture Current” after signing in the IDE')}</div>`;
  else { h += '<div class="cards">'; for (const a of accs) h += accountCard(a); h += '</div>'; }
  return h + '</div>';
}

/* token 过期状态徽标 */
function expiryPill(a) {
  if (!a.expiresAt) return `<span class="pill">${tr('有效期未知', 'Expiry unknown')}</span>`;
  const exp = new Date(a.expiresAt).getTime();
  if (!Number.isFinite(exp)) return `<span class="pill">${tr('有效期未知', 'Expiry unknown')}</span>`;
  const days = (exp - Date.now()) / 864e5;
  if (days <= 0) return `<span class="pill off">${tr('token 已过期', 'Token expired')}</span>`;
  if (days < 1) return `<span class="pill warn">${Math.max(1, Math.ceil(days * 24))} ${tr('小时后过期', 'h to expiry')}</span>`;
  if (days < 3) return `<span class="pill warn">${Math.ceil(days)} ${tr('天后过期', 'd to expiry')}</span>`;
  return `<span class="pill on">${tr('有效期至 ', 'Valid to ')}${esc(a.expiresAt.slice(5, 10))}</span>`;
}

/* 无头像账号的固定主题色头像（迷你猫标） */
/* 头像兜底：与 app icon / logo 同款「双气泡交换」标 */
const AVFB = '<svg viewBox="0 0 64 64" xmlns="http://www.w3.org/2000/svg">'
  + '<circle cx="32" cy="32" r="32" fill="#fbfdfa"/>'
  + '<circle cx="32" cy="32" r="31" fill="none" stroke="#e4eee6" stroke-width="1.6"/>'
  + '<path fill="#42624b" d="M16.5 18.75 H33.5 Q37.5 18.75 37.5 22.75 V31 H32.88 Q30.75 31 30.75 33.13 V36.88 H16.5 Q12.5 36.88 12.5 32.88 V22.75 Q12.5 18.75 16.5 18.75 Z"/>'
  + '<path fill="#42624b" d="M29.5 36.88 H38.75 L34.13 42.13 Z"/>'
  + '<path fill="#42624b" d="M31.13 33.38 H47.5 Q51.5 33.38 51.5 37.38 V45.63 Q51.5 49.63 47.5 49.63 H31.13 Q27.13 49.63 27.13 45.63 V37.38 Q27.13 33.38 31.13 33.38 Z"/>'
  + '<path fill="#fbfdfa" d="M38.75 38.13 V39.25 H47.63 Q48.75 41.5 47.63 43.75 H38.75 V44.88 L33.75 41.5 Z"/>'
  + '</svg>';

/* 手机号脱敏 + 睁眼/闭眼切换 */
const REVEAL = {};
const maskPhone = (p) => {
  p = String(p || '');
  if (p.length >= 8) return p.slice(0, 3) + '****' + p.slice(-4);
  if (p.length >= 4) return p.slice(0, 1) + '***' + p.slice(-1);
  return p.replace(/./g, '*');
};
const ICON_EYE_ON = '<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round"><path d="M2.5 12S6 5.5 12 5.5 21.5 12 21.5 12 18 18.5 12 18.5 2.5 12 2.5 12Z"/><circle cx="12" cy="12" r="3"/></svg>';
const ICON_EYE_OFF = '<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round"><path d="M2.5 12S6 5.5 12 5.5 21.5 12 21.5 12 18 18.5 12 18.5 2.5 12 2.5 12Z"/><circle cx="12" cy="12" r="3"/><path d="M4 4l16 16"/></svg>';
window.__toggleReveal = (id) => { REVEAL[id] = !REVEAL[id]; renderAccounts(); };

function accountCard(a) {
  const hasPhone = !!a.phone && !a.email;
  const contact = a.email || (hasPhone ? (REVEAL[a.id] ? a.phone : maskPhone(a.phone)) : a.uid.slice(0, 13));
  const eyeBtn = hasPhone ? `<span class="eye" title="${REVEAL[a.id] ? tr('隐藏手机号', 'Hide phone') : tr('显示手机号', 'Show phone')}" onclick="event.stopPropagation();__toggleReveal('${a.id}')">${REVEAL[a.id] ? ICON_EYE_ON : ICON_EYE_OFF}</span>` : '';
  return `<div class="card ${a.isCurrent ? 'current' : ''}">
    <div class="head">
      <div class="avatar fb" title="${esc(a.name) || ''}">${AVFB}</div>
      <div><div class="name">${esc(a.name) || tr('未命名', 'Unnamed')}</div>
      <div class="sub">${esc(contact)}${eyeBtn}</div></div>
    </div>
    ${a.note ? `<div class="note">📝 ${esc(a.note)}</div>` : ''}
    <div class="sub">
      <span class="pill">${edLabel(a.edition)}</span>
      ${a.isCurrent ? `<span class="pill on">${tr('当前登录', 'Signed In')}</span>` : ''}
      ${expiryPill(a)}
    </div>
    <div class="ops">
      <button class="primary" onclick="doSwitch('${a.id}',this)" ${a.isCurrent ? 'disabled' : ''}>${tr('切换', 'Switch')}</button>
      <button onclick="claimOne('${a.id}',this)">${tr('领取积分', 'Claim Credits')}</button>
      <button onclick="showInfo('${a.id}')">${tr('积分/详情', 'Usage Detail')}</button>
      <button onclick="refreshAcc('${a.id}',this)" title="${tr('用 refreshToken 换新登录凭证', 'Refresh login credentials with the refresh token')}">${tr('刷新', 'Refresh')}</button>
      <button onclick="editNote('${a.id}')">${tr('备注', 'Note')}</button>
      <button class="danger" onclick="delAccount('${a.id}')">${tr('删除', 'Delete')}</button>
    </div>
  </div>`;
}

function renderAccounts() {
  document.getElementById('accountsBody').innerHTML = ['cn', 'intl'].map(edSection).join('');
}

function renderLog() {
  const logs = [...(STATE.claimLog || [])].reverse();
  const el = document.getElementById('claimLog');
  if (!logs.length) { el.textContent = tr('暂无记录', 'No records yet'); return; }
  el.innerHTML = logs.map(l => {
    const lines = l.results.map(r => {
      const cls = r.phase === 'claimed' ? 'ok' : (r.phase === 'error' || r.phase === 'login_required' ? 'err' : '');
      return `<span class="${cls}">[${edShort(r.edition)}] ${esc(r.name)}：${esc(tmsg(r.msg))}</span>`;
    }).join('\n');
    return `<div>${esc(fmtTimeStr(l.time))}\n${lines}</div>`;
  }).join('\n');
}

/* ── 清除领取记录（两步确认） ── */
async function clearClaimLog(btn) {
  if (btn.dataset.arm !== '1') {
    btn.dataset.arm = '1';
    btn.textContent = tr('确认清除？', 'Confirm?');
    setTimeout(() => { if (btn.isConnected) { btn.dataset.arm = ''; btn.textContent = tr('清除记录', 'Clear'); } }, 3000);
    return;
  }
  btn.dataset.arm = '';
  btn.disabled = true;
  try {
    const r = await invoke('clear_claim_log');
    toast(tmsg(r.message) || tr('已清除', 'Cleared'), r.ok !== false);
    STATE && (STATE.claimLog = []);
    renderLog();
  } catch (e) { toast(tmsg(String(e)), false); }
  btn.disabled = false;
  btn.textContent = tr('清除记录', 'Clear');
}
window.clearClaimLog = clearClaimLog;

async function capture(ed, btn) {
  btn.disabled = true;
  try {
    const r = await invoke('capture', { edition: ed });
    toast(tmsg(r.message), r.ok);
  } catch (e) { toast(tmsg(String(e)), false); }
  btn.disabled = false;
  load();
}

/* ── 账号刷新 ── */
async function refreshAcc(id, btn) {
  btn.disabled = true; btn.textContent = tr('刷新中…', 'Refreshing…');
  try {
    const r = await invoke('refresh_account', { id });
    toast(tmsg(r.message), r.ok);
  } catch (e) { toast(tmsg(String(e)), false); }
  btn.disabled = false; btn.textContent = tr('刷新', 'Refresh');
  load();
}

/* ── 添加账号（OAuth 设备登录） ── */
let addPollTimer = null;
async function openAddAccount(ed, btn) {
  const edLabel = ed === 'cn' ? '国内版' : '国际版';
  let stop = false;
  modalOnClose = () => { stop = true; clearTimeout(addPollTimer); };
  showModal({
    title: `添加账号（${edLabel}）`,
    bodyHTML: `<div class="step-line" id="oauthBody">
        <div>将打开系统浏览器进入 <b>Qoder ${edLabel}</b> 官方登录页，完成登录后凭证会自动采集入库。</div>
        <div style="margin-top:10px"><span class="spin"></span>正在发起登录…</div>
      </div>`,
    buttons: [{ label: '关闭', value: null }],
  });
  try {
    const r = await invoke('login_begin', { edition: ed });
    const attempt = r.attempt;
    await invoke('open_url', { url: r.url });
    document.getElementById('oauthBody').innerHTML = `
      <div><b>浏览器已打开登录页</b>，请登录并授权。完成后本窗口会自动继续。</div>
      <div style="margin:8px 0;color:var(--muted)">如浏览器未打开，可复制链接：</div>
      <div class="oauth-url"><a href="${esc(r.url)}" target="_blank">${esc(r.url)}</a></div>
      <div style="margin-top:10px" id="oauthStatus"><span class="spin"></span>等待登录授权中…（最多 5 分钟）</div>`;
    const t0 = Date.now();
    const poll = async () => {
      if (stop) return;
      if (Date.now() - t0 > 5 * 60 * 1000) {
        document.getElementById('oauthStatus').innerHTML = '<span style="color:var(--red)">等待超时，请重新发起登录</span>';
        return;
      }
      try {
        const pr = await invoke('login_poll', { attempt });
        if (pr.done) {
          document.getElementById('oauthBody').innerHTML =
            `<div style="color:var(--primary);font-weight:550">✔ ${esc(pr.message)}</div>
             <div class="step-line" style="margin-top:6px">已加入账号库，可在卡片上执行切换 / 领取 / 刷新。</div>`;
          const foot = document.getElementById('modalFoot');
          foot.innerHTML = '';
          const b = document.createElement('button');
          b.className = 'primary'; b.textContent = '完成';
          b.onclick = () => closeModal();
          foot.appendChild(b);
          load();
          return;
        }
      } catch (e) {
        document.getElementById('oauthStatus').innerHTML = `<span style="color:var(--red)">${esc(String(e))}</span>`;
        return;
      }
      addPollTimer = setTimeout(poll, 2500);
    };
    addPollTimer = setTimeout(poll, 1500);
  } catch (e) {
    document.getElementById('oauthBody').innerHTML =
      `<div style="color:var(--red)">发起登录失败：${esc(String(e))}</div>`;
  }
}

async function doSwitch(id, btn) {
  const acc = STATE.accounts.find(a => a.id === id);
  if (!acc) return;
  openSwitchDialog(acc, btn);
}

/* 切换账号弹窗（wb-switch 同款：关联会话 / 复制会话 两个 tab） */
async function openSwitchDialog(acc, btn) {
  const targetId = acc.id; // 注意：下方回调统一用 targetId，避免引用未定义变量
  const targetName = acc.name || acc.email || acc.uid;
  let copySelection = new Set();
  const renderTab = (tab) => {
    if (tab === 0) {
      invoke('session_links').then(links => {
        const groups = (links.groups || []).filter(g =>
          (g.members || []).some(m => m.edition === acc.edition));
        const body = document.getElementById('modalBody');
        if (!groups.length) {
          body.innerHTML = '<div class="empty">该版本暂无关联会话组<br><small>复制会话副本后会自动建立关联，切换时可一键同步</small></div>';
          return;
        }
        body.innerHTML = groups.map(g => {
          const st = g.contentState === 'consistent'
            ? '<span class="pill on">内容一致</span>'
            : '<span class="pill warn">待同步</span>';
          const ms = (g.membersDetail || []).map(m =>
            `<div><b>[${m.edition === 'cn' ? '国内' : '国际'}]</b> ${esc(m.title || m.sessionId.slice(0, 8))} ${m.exists ? '' : '（文件缺失）'}</div>`).join('');
          return `<div class="grp"><div class="gh">${st}<span class="pill">${(g.members || []).length} 个副本</span>
            <button class="sm" onclick="window.__syncGroup('${g.id}')">同步</button></div>
            <div class="members">${ms}</div></div>`;
        }).join('');
      });
    } else {
      invoke('list_sessions', { edition: acc.edition, limit: 30 }).then(r => {
        const list = (r.sessions || []).slice(0, 12);
        const body = document.getElementById('modalBody');
        if (!list.length) { body.innerHTML = '<div class="empty">该版本暂无会话</div>'; return; }
        body.innerHTML = `
          <label class="check-row" style="border-bottom:1px solid var(--border);padding-bottom:8px;margin-bottom:4px">
            <input type="checkbox" id="selAllCb">
            <div class="t"><b>全选</b><small>共 ${list.length} 个会话</small></div>
          </label>` + list.map(s => `
          <label class="check-row"><input type="checkbox" data-sid="${s.id}" data-ed="${s.edition}"
            ${copySelection.has(s.edition + ':' + s.id) ? 'checked' : ''}>
            <div class="t">${esc(s.title || '(无标题)')}<small>${esc(s.cwd || '')} · ${fmtMs(s.updatedAt)}</small></div>
          </label>`).join('');
        body.querySelectorAll('input[data-sid]').forEach(cb => {
          cb.onchange = () => {
            const key = cb.dataset.ed + ':' + cb.dataset.sid;
            cb.checked ? copySelection.add(key) : copySelection.delete(key);
            const selAll = document.getElementById('selAllCb');
            if (selAll) selAll.checked = list.every(s => copySelection.has(s.edition + ':' + s.id));
          };
        });
        const selAll = document.getElementById('selAllCb');
        if (selAll) {
          selAll.checked = list.every(s => copySelection.has(s.edition + ':' + s.id));
          selAll.onchange = () => {
            list.forEach(s => {
              const key = s.edition + ':' + s.id;
              selAll.checked ? copySelection.add(key) : copySelection.delete(key);
            });
            body.querySelectorAll('input[data-sid]').forEach(cb => { cb.checked = selAll.checked; });
          };
        }
      });
    }
  };
  showModal({
    title: `切换到「${esc(targetName)}」`,
    tabs: ['关联会话', '复制会话'],
    onTab: renderTab,
    bodyHTML: '<div class="empty">加载中…</div>',
    buttons: [
      { label: '取消', value: null },
      { label: '确认切换', primary: true, onClick: async () => {
          closeModal();
          btn && (btn.disabled = true);
          showProgress('正在复制会话…');
          try {
            const picks = [...copySelection].map(k => {
              const [ed, sid] = [k.slice(0, k.indexOf(':')), k.slice(k.indexOf(':') + 1)];
              return { ed, sid };
            });
            const steps = [];
            for (const p of picks) {
              try {
                const r = await invoke('copy_session', { edition: p.ed, sessionId: p.sid });
                steps.push(tmsg(r.message) || tr('已复制会话副本', 'Session copy created'));
                showProgress(`${tr('正在复制会话', 'Copying sessions')}（${steps.length}/${picks.length}）…`);
              } catch (e) { steps.push(tr('复制会话失败：', 'Copy failed: ') + e); }
            }
            showProgress(tr('正在切换账号…', 'Switching account…'));
            const r = await invoke('switch_account', { id: targetId });
            hideProgress(steps.concat((r.steps || []).map(tmsg)));
            toast(tmsg(r.message), r.ok);
          } catch (e) { hideProgress(); toast(tmsg(String(e)), false); }
          btn && (btn.disabled = false);
          load();
        } },
    ],
  });
  renderTab(0);
}
window.__syncGroup = async (gid) => {
  try {
    const r = await invoke('sync_session_group', { groupId: gid });
    toast(tmsg(r.message), r.ok);
  } catch (e) { toast(tmsg(String(e)), false); }
  closeModal();
};

async function claimOne(id, btn) {
  btn.disabled = true;
  try {
    const r = await invoke('claim', { id });
    toast(tmsg(r.message), r.ok);
  } catch (e) { toast(tmsg(String(e)), false); }
  btn.disabled = false;
  load();
}

async function showInfo(id) {
  const a = STATE.accounts.find(x => x.id === id);
  showModal({
    title: `${tr('用量明细', 'Usage Detail')} · ${esc(a ? (a.name || a.id) : id)}`,
    bodyHTML: `<div id="usageBody" class="usage-modal"><span class="spin"></span>${tr('查询中…', 'Querying…')}</div>`,
    buttons: [{ label: tr('关闭', 'Close'), value: null }],
  });
  try {
    const r = await invoke('credits', { id });
    const el = document.getElementById('usageBody');
    if (!el) return;
    if (!r.ok) { el.textContent = tmsg(r.message); return; }
    const d = r.data;
    let h = '';
    const u = d.usage;
    if (u && (u.userQuota || u.addOnQuota)) {
      const cards = [
        { name: tr('订阅版本的资源', 'Subscription quota'), desc: tr('当前订阅计划内的额度。', 'Quota included in the current subscription plan.'), q: u.userQuota },
        { name: tr('个人资源包', 'Personal bonus packs'), desc: tr('通过活动或购买获得，仅供个人使用。', 'Earned via campaigns or purchase; personal use only.'), q: u.addOnQuota },
      ];
      for (const c of cards) {
        const q = c.q || {};
        const total = Number(q.total) || 0, used = Number(q.used) || 0, rem = Number(q.remaining) || 0;
        const pct = total > 0 ? Math.min(100, Math.round(used / total * 100)) : 0;
        h += `<div class="ucard">
          <div class="un"><span>${c.name}<small>${c.desc}</small></span><span class="ur">${tr('剩余 ', 'Remaining ')}<b>${fmtNum(rem)}</b></span></div>
          <div class="ubar"><i style="width:${pct}%"></i></div>
          <div class="ufoot"><span>${fmtNum(used)} / ${fmtNum(total)}（${tr('已使用 ', 'used ')}${pct}%）</span><span>${esc(q.unit || 'credits')}</span></div>
        </div>`;
      }
      const at = Number(u.addOnQuota && u.addOnQuota.total) || 0;
      if (at > 0) {
        const arem = Number(u.addOnQuota.remaining) || 0;
        h += `<div class="upack"><span><i class="sq"></i>${tr('获赠资源包（总计: ', 'Bonus packs (total: ')}${fmtNum(at)}）</span><span>${tr('剩余 ', 'Remaining ')}${fmtNum(arem)} credits · ${tr('有效期至 ', 'valid to ')}${usageExpiry(u)}</span></div>`;
      }
    }
    if (d.summary) h += `<div class="umeta">${tr('累计消耗 ', 'Total used ')}<b>${(d.summary.totalCredits || 0).toFixed(2)}</b> Credits（${tr('单日峰值 ', 'daily peak ')}${(d.summary.peakCredits || 0).toFixed(2)} / ${esc(d.summary.peakDate || '-')}）</div>`;
    const ST = { CLAIMED: tr('已领取', 'Claimed'), CLAIMABLE: tr('可领取', 'Claimable'), ACHIEVEMENT_NOT_COMPLETED: tr('任务未完成', 'Not Completed') };
    const camps = d.campaigns || [];
    if (camps.length) {
      h += `<div class="umeta">${tr('最近活动：', 'Recent campaigns: ')}<div class="ucamps">` + camps.map(c => {
        const st = c.claimStatus || '';
        const cn = ST[st] || st || '—';
        const cls = st === 'CLAIMABLE' ? 'warn' : (st === 'CLAIMED' ? 'on' : '');
        const b = c.benefit || {};
        const nm = String(c.title || c.name || c.displayName || b.name || b.title || b.displayName || b.description || c.description || c.campaignId || '');
        const isId = !c.title && !c.name && !c.displayName && !b.name && !b.title && !b.displayName && !b.description && !c.description;
        const disp = isId
          ? (nm.length > 12 ? nm.slice(0, 8) + '…' + nm.slice(-4) : nm)
          : (nm.length > 28 ? nm.slice(0, 26) + '…' : nm);
        const amt = Number(b.amount) || 0;
        const amtTxt = amt > 0 ? `<b class="camt">+${fmtNum(amt)}</b>` : '';
        return `<span class="ucamp" title="${esc(nm)}${c.campaignId ? ' · ' + esc(c.campaignId) : ''}"><span class="pill ${cls}">${esc(cn)}</span><span class="cid">${esc(disp)}</span>${amtTxt}</span>`;
      }).join('') + '</div></div>';
    }
    if (!u && !d.summary && !camps.length) h += `<div class="umeta">${tr('暂无用量数据', 'No usage data')}</div>`;
    el.innerHTML = h;
  } catch (e) {
    const el = document.getElementById('usageBody');
    if (el) el.textContent = tr('查询失败：', 'Query failed: ') + e;
  }
}

function usageExpiry(u) {
  const t = Number(u && u.expiresAt) || 0;
  // 253402214400000 ≈ 9999-12-31（表示长期有效），不展示
  if (!t || t >= 253402214400000) return tr('长期有效', 'Never expires');
  const dt = new Date(t);
  if (isNaN(dt.getFullYear()) || dt.getFullYear() < 2000) return tr('长期有效', 'Never expires');
  return `${dt.getFullYear()}-${dt.getMonth() + 1}-${dt.getDate()}`;
}

async function editNote(id) {
  const a = STATE.accounts.find(x => x.id === id);
  const n = await promptDialog(tr('账号备注', 'Account Note'), tr('备注会显示在账号卡片上', 'The note is shown on the account card'), a ? a.note : '');
  if (n === null) return;
  try {
    await invoke('set_note', { id, note: n });
    toast(tr('已保存备注', 'Note saved'), true);
  } catch (e) { toast(tmsg(String(e)), false); }
  load();
}

async function delAccount(id) {
  const ok = await confirmDialog(tr('从账号库删除该账号？（不影响客户端当前登录）', 'Remove this account from the store? (the IDE stays signed in)'), tr('删除', 'Delete'), true);
  if (!ok) return;
  try {
    const r = await invoke('delete_account', { id });
    toast(tmsg(r.message), r.ok);
  } catch (e) { toast(tmsg(String(e)), false); }
  load();
}

async function toggleAuto() {
  try {
    const r = await invoke('set_auto_claim', { enabled: !STATE.settings.autoClaim });
    toast(tmsg(r.message), r.ok);
  } catch (e) { toast(tmsg(String(e)), false); }
  load();
}

async function claimAllBtn(btn) {
  btn.disabled = true; btn.textContent = tr('领取中…', 'Claiming…');
  try {
    const r = await invoke('claim', {});
    toast(tmsg(r.message), r.ok);
  } catch (e) { toast(tmsg(String(e)), false); }
  btn.disabled = false; btn.textContent = tr('立即全部领取', 'Claim All');
  load();
}

async function openApp(ed) { try { await invoke('open_app', { edition: ed }); } catch (e) { toast(tmsg(String(e)), false); } }

async function doExport() {
  try { const r = await invoke('export_accounts'); toast(tmsg(r.message), r.ok); }
  catch (e) { toast(tmsg(String(e)), false); }
}
async function doImport() {
  const path = await promptDialog(tr('导入账号备份', 'Import Accounts'), tr('输入备份文件完整路径', 'Enter the full path of the backup file'), '');
  if (!path) return;
  try { const r = await invoke('import_accounts', { path }); toast(tmsg(r.message), r.ok); }
  catch (e) { toast(tmsg(String(e)), false); }
  load();
}

function showLaunchdHelp() {
  showModal({
    title: tr('系统定时任务（launchd）', 'Scheduled Task (launchd)'),
    bodyHTML: `${tr('在终端执行一次：', 'Run once in Terminal:')}<br><code class="path">bash "/Users/dalong/Documents/codeWork/other/qoder switch/qoder-switch-app/install_auto_claim.sh"</code>
      <br><br>${tr('注册后每天', 'Once registered, all accounts are claimed daily at')} <b>10:05</b> ${tr('和', 'and')} <b>22:05</b>${tr(' 自动领取全部账号积分，无需打开本应用。', ' without opening the app.')}<br>
      ${tr('日志：', 'Log:')}<code class="path">~/.qoder-switch/auto-claim.log</code>`,
  });
}

/* ═════════ 会话页 ═════════ */
async function loadSessions(refresh) {
  const tabsEl = document.getElementById('sessEdTabs');
  if (!tabsEl.children.length || refresh) {
    const eds = [['cn', edLabel('cn')], ['intl', edLabel('intl')]];
    const current = SESS?.edition || 'cn';
    tabsEl.innerHTML = eds.map(([k, label]) =>
      `<div class="ed-tab ${k === current ? 'active' : ''}" data-ed="${k}">${label}</div>`).join('');
    tabsEl.querySelectorAll('.ed-tab').forEach(el => {
      el.onclick = () => { SESS = { edition: el.dataset.ed, filter: SESS_FILTER }; loadSessions(); };
    });
  }
  const ed = SESS?.edition || 'cn';
  tabsEl.querySelectorAll('.ed-tab').forEach(el =>
    el.classList.toggle('active', el.dataset.ed === ed));
  const q = document.getElementById('sessSearch').value;
  try {
    const [r, links] = await Promise.all([
      invoke('list_sessions', { edition: ed, query: q || null, limit: 100 }),
      invoke('session_links'),
    ]);
    renderGroups(links, ed);
    renderSessions(r, links, ed);
  } catch (e) {
    document.getElementById('sessGrid').innerHTML = `<div class="empty">${esc(String(e))}</div>`;
  }
}

function renderGroups(links, ed) {
  const groups = (links.groups || []).filter(g => (g.members || []).some(m => m.edition === ed));
  document.getElementById('grpCount').textContent = groups.length + ' ' + tr('组', 'groups');
  const body = document.getElementById('groupsBody');
  if (!groups.length) { body.innerHTML = `<div class="empty">${tr('暂无关联组 —— 复制会话副本后自动建立关联', 'No linked groups yet — copying a session creates one')}</div>`; return; }
  body.innerHTML = groups.map(g => {
    const st = g.contentState === 'consistent'
      ? `<span class="pill on">${tr('内容一致', 'In Sync')}</span>`
      : `<span class="pill warn">${tr('待同步', 'Needs Sync')}</span>`;
    const ms = (g.membersDetail || []).map(m =>
      `<div><b>[${edShort(m.edition)}]</b> ${esc(m.title || m.sessionId.slice(0, 8))}${m.exists ? '' : tr('（文件缺失）', ' (file missing)')}</div>`).join('');
    return `<div class="grp"><div class="gh">${st}<span class="pill">${(g.members || []).length} ${tr('个副本', 'copies')}</span>
      <button class="sm primary" onclick="__syncGroupPage('${g.id}')">${tr('同步', 'Sync')}</button></div>
      <div class="members">${ms}</div></div>`;
  }).join('');
}
window.__syncGroupPage = async (gid) => {
  try {
    const r = await invoke('sync_session_group', { groupId: gid });
    toast(tmsg(r.message), r.ok);
  } catch (e) { toast(tmsg(String(e)), false); }
  loadSessions();
};

function renderSessions(r, links, ed) {
  document.getElementById('sessCount').textContent = (r.total || 0) + ' ' + tr('个', '');
  const filters = [['all', tr('全部', 'All')], ['linked', tr('已关联', 'Linked')], ['consistent', tr('内容一致', 'In Sync')], ['modified', tr('待同步', 'Needs Sync')]];
  document.getElementById('sessFilters').innerHTML = filters.map(([k, label]) => {
    let n;
    if (k === 'all') n = (r.sessions || []).length;
    else if (k === 'linked') n = (r.sessions || []).filter(s => s.groupId).length;
    else n = (r.sessions || []).filter(s => s.contentState === k).length;
    return `<span class="fpill ${SESS_FILTER === k ? 'active' : ''}" onclick="__setFilter('${k}')">${label} ${n}</span>`;
  }).join('');
  let list = r.sessions || [];
  if (SESS_FILTER === 'linked') list = list.filter(s => s.groupId);
  else if (SESS_FILTER !== 'all') list = list.filter(s => s.contentState === SESS_FILTER);
  const grid = document.getElementById('sessGrid');
  if (!list.length) { grid.innerHTML = `<div class="empty">${tr('没有符合条件的会话', 'No sessions match the filter')}</div>`; return; }
  grid.innerHTML = list.map(s => {
    const st = !s.groupId
      ? ''
      : (s.contentState === 'consistent' ? `<span class="pill on">${tr('内容一致', 'In Sync')}</span>` : `<span class="pill warn">${tr('待同步', 'Needs Sync')}</span>`);
    return `<div class="sess">
      <div class="proj">${esc((s.cwd || '').split('/').slice(-1).join('/'))}</div>
      <div class="title">${esc(s.title || tr('(无标题)', '(Untitled)'))}</div>
      <div class="meta"><span>${fmtMs(s.updatedAt)}</span><span>${fmtBytes(s.size)}</span>${st}</div>
      <div class="ops">
        <button class="sm" onclick="__copySess('${s.edition}','${s.id}',this)">${tr('复制副本', 'Copy')}</button>
        ${s.groupId ? `<button class="sm" onclick="__syncGroupPage('${s.groupId}')">${tr('同步', 'Sync')}</button>` : ''}
        <button class="sm danger" onclick="__delSess('${s.edition}','${s.id}','${esc(s.title || '').replace(/'/g, '')}')">${tr('删除', 'Delete')}</button>
      </div>
    </div>`;
  }).join('');
}
window.__setFilter = (k) => { SESS_FILTER = k; if (SESS) SESS.filter = k; loadSessions(); };
window.__copySess = async (ed, sid, btn) => {
  btn.disabled = true; btn.textContent = tr('复制中…', 'Copying…');
  try {
    const r = await invoke('copy_session', { edition: ed, sessionId: sid });
    toast(tmsg(r.message), r.ok);
  } catch (e) { toast(tmsg(String(e)), false); }
  btn.disabled = false; btn.textContent = tr('复制副本', 'Copy');
  loadSessions();
};
window.__delSess = async (ed, sid, title) => {
  const ok = await confirmDialog(`删除会话「${title || sid.slice(0, 8)}」？\n\n会先备份到 ~/.qoder-switch/session-backups/，如应用正在运行将自动退出对应 Qoder。`, '删除', true);
  if (!ok) return;
  try {
    const r = await invoke('delete_session', { edition: ed, sessionId: sid });
    toast(r.ok ? tr('会话已删除（已备份）', 'Session deleted (backed up)') : (tmsg(r.message) || tr('删除失败', 'Delete failed')), r.ok);
  } catch (e) { toast(tmsg(String(e)), false); }
  loadSessions();
};

/* ═════════ 积分统计页（wb-switch 同款） ═════════ */
/* 快捷时间范围（Token/积分页共用），自定义区间由通用下拉组件提供 */
const TOKEN_RANGES = [['today', tr('今天', 'Today')], ['yesterday', tr('昨天', 'Yesterday')], ['7d', tr('近 7 天', 'Last 7 Days')], ['30d', tr('近 30 天', 'Last 30 Days')], ['month', tr('当月', 'This Month')], ['lastmonth', tr('上月', 'Last Month')], ['year', tr('今年', 'This Year')], ['all', tr('全部', 'All')]];
const CHART_RANGES = TOKEN_RANGES;
const SHARE_RANGES = TOKEN_RANGES;

async function loadCreditStats(refresh) {
  const btn = document.getElementById('creditRefreshBtn');
  if (refresh && btn) { btn.disabled = true; btn.textContent = '采集中…'; }
  // 首次打开 / 手动刷新：先展示加载动画
  if (!CSTATS || refresh) {
    showLoading('creditMetrics', ['shareBody', 'creditChart', 'creditDetailBody']);
  }
  try {
    CSTATS = await invoke('credit_stats', { tzOffset: tzOffsetMin() });
  } catch (e) {
    document.getElementById('creditMetrics').innerHTML =
      `<div class="metric" style="grid-column:1/-1;color:var(--red)">${esc(String(e))}</div>`;
  }
  if (refresh && btn) { btn.disabled = false; btn.textContent = '刷新统计'; }
  if (!CSTATS) return;
  renderCreditViewTabs();
  renderCreditPage();
}

function accHeat(a) { return (a.heat || []).map(x => ({ date: x.date || '', value: Number(x.value) || 0 })); }

function creditAggDaily(view) {
  // view: 'all' | 'cn' | 'intl' | 账号id
  const accs = (CSTATS.accounts || []).filter(a =>
    view === 'all' || view === a.edition || view === a.id);
  if (view === CSTATS?.accounts?.[0]?.id) {
    // 单账号直接用其数据
  }
  if (accs.length === 1 && view === accs[0].id) return accHeat(accs[0]);
  const m = new Map();
  for (const a of accs) for (const p of accHeat(a)) m.set(p.date, (m.get(p.date) || 0) + p.value);
  return [...m.entries()].map(([date, value]) => ({ date, value })).sort((x, y) => x.date.localeCompare(y.date));
}

function rangeSum(daily, range, custom) {
  return filterRangeDates(daily, range, custom).reduce((s, p) => s + p.value, 0);
}

function renderCreditViewTabs() {
  const accs = CSTATS.accounts || [];
  const opts = [['all', tr('全部', 'All')], ['cn', edLabel('cn')], ['intl', edLabel('intl')]];
  document.getElementById('creditViewTabs').innerHTML = opts.map(([k, label]) =>
    `<button class="${CREDIT.view === k ? 'active' : ''}" onclick="__setCreditView('${k}')">${label}</button>`).join('');
}
window.__setCreditView = (k) => { CREDIT.view = k; CREDIT.chartAcc = 'all'; CREDIT.detailAcc = null; renderCreditViewTabs(); renderCreditPage(); };

function viewAccounts() {
  return (CSTATS.accounts || []).filter(a => CREDIT.view === 'all' || CREDIT.view === a.edition || CREDIT.view === a.id);
}

function renderCreditPage() {
  const accs = viewAccounts();
  const daily = creditAggDaily(CREDIT.view);
  document.getElementById('creditUpdated').textContent = tr('当前数据更新于 ', 'Data updated at ') + nowStr();

  // ── 4 大指标 ──
  const qTotal = accs.reduce((s, a) => s + (Number(a.quotaTotal) || 0), 0);
  const qUsed = accs.reduce((s, a) => s + (Number(a.quotaUsed) || 0), 0);
  const remaining = Math.max(0, qTotal - qUsed);
  document.getElementById('creditMetrics').innerHTML = `
    <div class="metric"><div class="ml">${ic('coins')} ${tr('剩余积分', 'Remaining Credits')}</div><div class="mv">${qTotal > 0 ? fmtNum(remaining) : '—'}</div></div>
    <div class="metric"><div class="ml">${ic('down')} ${tr('今日消耗', 'Used Today')}</div><div class="mv">${fmtNum(rangeSum(daily, 'today'))}</div></div>
    <div class="metric"><div class="ml">${ic('cal')} ${tr('近 7 天消耗', 'Last 7 Days')}</div><div class="mv">${fmtNum(rangeSum(daily, '7d'))}</div></div>
    <div class="metric"><div class="ml">${ic('clock')} ${tr('本月消耗', 'This Month')}</div><div class="mv">${fmtNum(rangeSum(daily, 'month'))}</div></div>`;

  renderShareCard(accs);
  renderCreditChart(daily);
  renderCreditDetail(accs);
}

/* 账号消耗构成 */
function renderShareCard(accs) {
  bindRangeDD('shareRangeDD', {
    ranges: SHARE_RANGES,
    getKey: () => CREDIT.shareRange,
    setKey: (k) => { CREDIT.shareRange = k; renderShareCard(viewAccounts()); },
    getCustom: () => CREDIT.shareCustom,
    setCustom: (c) => { CREDIT.shareCustom = c; },
  });
  const rows = accs.map(a => ({ a, usage: rangeSum(accHeat(a), CREDIT.shareRange, CREDIT.shareCustom) }))
    .filter(r => r.a.ok);
  const groups = new Map();
  for (const r of rows) {
    const g = edLabel(r.a.edition);
    if (!groups.has(g)) groups.set(g, []);
    groups.get(g).push(r);
  }
  let total = 0; for (const r of rows) total += r.usage;
  const showGroup = CREDIT.view === 'all' && groups.size > 1;
  let html = `<div style="display:flex;justify-content:space-between;font-size:12px;color:var(--muted);margin-bottom:10px">
    <span>${tr('共 ', 'Total of ')}${rows.length} ${tr('个账号', 'accounts')}</span><span style="color:var(--text);font-weight:550">${tr('合计 ', 'Total ')}${fmtNum(total)} ${tr('积分', 'credits')}</span></div>`;
  if (!rows.length || total <= 0) {
    html += `<div class="empty">${tr('当前范围暂未观察到积分消耗', 'No credit usage observed in this range')}</div>`;
  } else {
    for (const [g, list] of groups) {
      const gTotal = list.reduce((s, r) => s + r.usage, 0);
      html += showGroup ? `<div style="font-size:12px;color:var(--muted);margin:10px 0 2px">${g} · ${tr('合计 ', 'total ')}${fmtNum(gTotal)} ${tr('积分', 'credits')}</div>` : '';
      for (const r of list) {
        const pct = gTotal > 0 ? (r.usage / gTotal) * 100 : 0;
        html += `<div class="hrow">
          <div class="hr-top"><span class="l">${esc(r.a.name || r.a.id)}</span>
            <span class="r">${fmtNum(r.usage)} ${tr('积分', 'credits')} <b>${pct < 0.05 ? '<0.1%' : pct.toFixed(1) + '%'}</b></span></div>
          <div class="hr-track"><div class="hr-fill" style="width:${Math.min(100, Math.max(0, pct))}%"></div></div>
        </div>`;
      }
    }
  }
  document.getElementById('shareBody').innerHTML = html;
}

/* 官方积分消耗趋势图 */
function renderCreditChart(daily) {
  // 账号筛选
  const accs = CSTATS.accounts || [];
  const chartAccOpts = [['all', CREDIT.view === 'all' ? tr('所有账号', 'All Accounts') : tr('本视图汇总', 'This View')]]
    .concat(accs.filter(a => CREDIT.view === 'all' || CREDIT.view === a.edition || CREDIT.view === a.id)
      .map(a => [a.id, a.name || a.id.slice(0, 12)]));
  document.getElementById('chartAccTabs').innerHTML = chartAccOpts.map(([k, l]) =>
    `<button class="${CREDIT.chartAcc === k ? 'active' : ''}" onclick="__setChartAcc('${k}')">${esc(l)}</button>`).join('');
  bindRangeDD('chartRangeDD', {
    ranges: CHART_RANGES,
    getKey: () => CREDIT.chartRange,
    setKey: (k) => { CREDIT.chartRange = k; renderCreditChart(creditAggDaily(CREDIT.view)); },
    getCustom: () => CREDIT.chartCustom,
    setCustom: (c) => { CREDIT.chartCustom = c; },
  });

  let data = CREDIT.chartAcc === 'all' ? daily : accHeat(accs.find(a => a.id === CREDIT.chartAcc));
  data = filterRangeDates(data, CREDIT.chartRange, CREDIT.chartCustom);

  const el = document.getElementById('creditChart');
  const sum = data.reduce((s, p) => s + p.value, 0);
  document.getElementById('chartSum').textContent = `${tr('当前口径合计 ', 'Total in range ')}${fmtNum(sum)} ${tr('积分', 'credits')}`;
  document.getElementById('chartUpdated').textContent = tr('数据更新于 ', 'Data updated at ') + nowStr();
  if (!data.length) { el.innerHTML = `<div class="empty" style="width:100%">${tr('暂无可展示的数据', 'No data to display')}</div>`; return; }
  const max = Math.max(...data.map(p => p.value), 0.001);
  el.innerHTML = data.map(p => {
    const pct = Math.max(p.value > 0 ? 3 : 1, (p.value / max) * 100);
    return `<div class="bar ${p.value > 0 ? '' : 'zero'}" style="height:${pct}%" data-d="${p.date}" data-v="${p.value.toFixed(2)}"></div>`;
  }).join('');
  attachChartTip(el, p => `${p.dataset.d}<br>${tr('消耗 ', 'Used ')}<b>${p.dataset.v}</b> ${tr('积分', 'credits')}`);
}
window.__setChartAcc = (k) => { CREDIT.chartAcc = k; renderCreditPage(); };

/* 积分明细（配额余量条） */
function renderCreditDetail(accs) {
  if (!CREDIT.detailAcc || !accs.some(a => a.id === CREDIT.detailAcc)) {
    CREDIT.detailAcc = accs[0]?.id || null;
  }
  document.getElementById('detailAccTabs').innerHTML = accs.map(a =>
    `<button class="${CREDIT.detailAcc === a.id ? 'active' : ''}" onclick="__setDetailAcc('${a.id}')">${esc(a.name || a.id.slice(0, 12))}</button>`).join('');
  const a = accs.find(x => x.id === CREDIT.detailAcc);
  const body = document.getElementById('creditDetailBody');
  if (!a) { body.innerHTML = `<div class="empty">${tr('暂无账号', 'No accounts')}</div>`; return; }
  document.getElementById('detailDesc').textContent = a.ok ? tr('套餐与加量包合并余量', 'Combined plan and add-on quota') : tr('该账号查询失败，请刷新统计重试', 'Query failed for this account — refresh and retry');
  if (!a.ok) { body.innerHTML = `<div class="empty">${tr('该账号官方用量暂不可用', 'Official usage unavailable for this account')}</div>`; return; }
  const total = Number(a.quotaTotal) || 0;
  const used = Number(a.quotaUsed) || 0;
  const remaining = Math.max(0, total - used);
  const pct = total > 0 ? (remaining / total) * 100 : 0;
  body.innerHTML = `
    <div class="hrow">
      <div class="hr-top"><span class="l">${esc(a.name)}</span>
        <span class="r">${tr('剩余 ', 'Remaining ')}<b>${fmtNum(remaining)}</b> / ${fmtNum(total)}</span></div>
      <div class="hr-track"><div class="hr-fill" style="width:${Math.min(100, Math.max(0, pct))}%"></div></div>
      <div class="hr-sub">${tr('已用 ', 'Used ')}${fmtNum(used)} ${tr('积分', 'credits')} · ${tr('累计消耗 ', 'total ')}${fmtNum(a.totalCredits)} · ${tr('单日峰值 ', 'daily peak ')}${fmtNum(a.peakCredits)}${a.peakDate ? '（' + esc(a.peakDate) + '）' : ''}</div>
    </div>`;
}
window.__setDetailAcc = (k) => { CREDIT.detailAcc = k; renderCreditDetail(viewAccounts()); };

/* 柱状图 tooltip（fixed 定位挂 body，视口内钳制，贴顶自动翻到柱子下方） */
let _ctip = null;
function chartTipEl() {
  if (!_ctip) {
    _ctip = document.createElement('div');
    _ctip.className = 'ctip';
    _ctip.style.cssText = 'position:fixed;background:#2c3038;border:1px solid #2c3038;color:#fff;font-size:11.5px;padding:6px 10px;border-radius:8px;pointer-events:none;white-space:nowrap;display:none;z-index:999;line-height:1.6;box-shadow:0 4px 16px rgba(0,0,0,.22)';
    document.body.appendChild(_ctip);
  }
  return _ctip;
}
function attachChartTip(el, htmlFn) {
  const tip = chartTipEl();
  tip.style.display = 'none';
  el.querySelectorAll('.bar').forEach(b => {
    b.onmouseenter = () => {
      tip.innerHTML = htmlFn(b);
      tip.style.display = 'block';
      const r = b.getBoundingClientRect();
      const tw = tip.offsetWidth, th = tip.offsetHeight, M = 8;
      let x = r.left + r.width / 2;
      x = Math.max(tw / 2 + M, Math.min(x, window.innerWidth - tw / 2 - M));
      const above = r.top - th - M >= M;          // 上方放得下 → 放上方，否则翻到下方
      const y = above ? r.top - M : r.bottom + M;
      tip.style.transform = above ? 'translate(-50%,-100%)' : 'translate(-50%,0)';
      tip.style.left = x + 'px';
      tip.style.top = y + 'px';
    };
    b.onmouseleave = () => { tip.style.display = 'none'; };
  });
}

/* ═════════ Token 统计页 ═════════ */

async function loadTokenStats(refresh) {
  const tabsEl = document.getElementById('tokenSrcTabs');
  tabsEl.innerHTML = [['cn', edLabel('cn')], ['intl', edLabel('intl')]].map(([k, l]) =>
    `<button class="${TSTAT.src === k ? 'active' : ''}" onclick="__setTokenSrc('${k}')">${l}</button>`).join('');
  const key = TSTAT.src;
  // 首次打开 / 手动刷新：先展示加载动画
  if (!TSTATS[key] || refresh) {
    showLoading('tokenMetrics', ['tokenChart', 'distBody']);
  }
  try {
    TSTATS[key] = await invoke('token_stats', { edition: key, tzOffset: tzOffsetMin() });
  } catch (e) {
    TSTATS[key] = { available: false, daily: [], projects: [], models: [], requests: [], totals: {} };
    toast(tmsg(String(e)), false);
  }
  renderTokenPage();
}
window.__setTokenSrc = (k) => { TSTAT.src = k; TSTAT.page = 0; loadTokenStats(); };

/* ── 通用时间范围控件：快捷 chips 平铺 + 范围框（点击弹双月日历） ──
   DD_STATE[slot] = { ranges, getKey, setKey(k), getCustom(), setCustom(c) } */
const DD_STATE = {};
const CAL_IC = '<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round"><rect x="3" y="4.5" width="18" height="17" rx="3"/><path d="M8 2.5v4M16 2.5v4M3 9.5h18"/></svg>';
const pad2 = (n) => String(n).padStart(2, '0');
const dKey = (y, m, d) => `${y}-${pad2(m + 1)}-${pad2(d)}`;
function fmtBoxDT(v) {
  const s = String(v || '');
  if (!s) return '…';
  return s.slice(0, 4) + '/' + s.slice(5, 7) + '/' + s.slice(8, 10) + ' ' + (s.slice(11, 19) || '00:00:00');
}
function bindRangeDD(slot, opts) {
  DD_STATE[slot] = opts;
  DD_STATE[slot + 'Pick'] = opts; /* 面板复用同一份配置 */
  DD_STATE[slot + 'Cal'] = null;
  rangeDDRender(slot);
}
/* 快捷区间的原始日期键 + 起止时刻（供日历初始化与范围框展示共用） */
function quickSpanRaw(key) {
  const today = tzTodayKey();
  const sh = tzShifted();
  const Y = sh.getUTCFullYear(), M = sh.getUTCMonth();
  switch (key) {
    case 'today': return [today, today, '00:00:00', '23:59:59'];
    case 'yesterday': { const y = tzDaysAgoKey(1); return [y, y, '00:00:00', '23:59:59']; }
    case '7d': return [tzDaysAgoKey(6), today, '00:00:00', '23:59:59'];
    case '30d': return [tzDaysAgoKey(29), today, '00:00:00', '23:59:59'];
    case 'month': return [dKey(Y, M, 1), today, '00:00:00', '23:59:59'];
    case 'lastmonth': {
      const a = new Date(Date.UTC(Y, M - 1, 1)), b = new Date(Date.UTC(Y, M, 0));
      return [dKey(a.getUTCFullYear(), a.getUTCMonth(), 1), dKey(b.getUTCFullYear(), b.getUTCMonth(), b.getUTCDate()), '00:00:00', '23:59:59'];
    }
    case 'year': return [Y + '-01-01', today, '00:00:00', '23:59:59'];
    default: return null; /* all 等 */
  }
}
/* 快捷区间的具体起止时间（整天口径 00:00:00 - 23:59:59，与按天聚合一致，点 tab 自动变） */
function quickSpan(key) {
  const sp = quickSpanRaw(key);
  if (!sp) return null;
  return [fmtBoxDT(sp[0] + ' 00:00:00'), fmtBoxDT(sp[1] + ' 23:59:59')];
}
function rangeDDRender(slot) {
  const s = DD_STATE[slot];
  const el = document.getElementById(slot);
  if (!s || !el) return;
  document.querySelectorAll('body > .rpanel').forEach(n => n.remove()); /* 清掉挂到 body 的旧弹层 */
  const key = s.getKey();
  const c = s.getCustom() || {};
  const qlabel = (s.ranges.find(r => r[0] === key) || [])[1];
  let boxLabel;
  if (key === 'custom') boxLabel = `${fmtBoxDT(c.from)} - ${fmtBoxDT(c.to)}`;
  else { const sp = quickSpan(key); boxLabel = sp ? `${sp[0]} - ${sp[1]}` : (qlabel || '—'); }
  el.innerHTML = `<div class="range-ctl">
    <div class="rtabs">${s.ranges.map(([k, l]) =>
      `<span class="rchip ${key === k ? 'active' : ''}" onclick="window.__ddPick('${slot}','${k}')">${l}</span>`).join('')}</div>
    <div class="range-box ${key === 'custom' ? 'on' : ''}" onclick="event.stopPropagation();window.__ddToggle('${slot}Pick')">
      <span class="rv">${esc(boxLabel)}</span>
      ${key === 'custom' ? `<span class="clr" title="${tr('清除自定义', 'Clear custom')}" onclick="event.stopPropagation();window.__ddClear('${slot}')">×</span>` : ''}
      ${CAL_IC}
    </div>
    <div class="rpanel" id="${slot}PickMenu" style="display:none" onclick="event.stopPropagation()"></div>
  </div>`;
}
function __calInit(slot) {
  const s = DD_STATE[slot]; if (!s) return null;
  const c = s.getCustom() || {};
  const nowT = tzShifted().toISOString().slice(11, 19); /* 当前时刻按所选时区 */
  let from, to, tf = '00:00:00', tt = nowT;
  const key = s.getKey();
  if (key === 'custom' && c.from && c.to) {
    from = c.from.slice(0, 10); to = c.to.slice(0, 10);
    tf = c.from.slice(11, 19) || tf; tt = c.to.slice(11, 19) || tt;
  } else {
    const sp = quickSpanRaw(key); /* 跟随当前选中的快捷 tab */
    if (sp) { from = sp[0]; to = sp[1]; tf = sp[2]; tt = sp[3]; }
    else { from = tzDaysAgoKey(6); to = tzTodayKey(); } /* 全部等：默认近 7 天便于点选 */
  }
  const base = new Date((to || from) + 'T00:00:00');
  return { y: base.getFullYear(), m: base.getMonth(), from, to, tf, tt };
}
function __calMonthHTML(slot, y, m, st) {
  const en = effLang() === 'en';
  const wd = en ? ['Su', 'Mo', 'Tu', 'We', 'Th', 'Fr', 'Sa'] : ['日', '一', '二', '三', '四', '五', '六'];
  const todayK = tzTodayKey();
  const off = new Date(y, m, 1).getDay();
  const days = new Date(y, m + 1, 0).getDate();
  const prevDays = new Date(y, m, 0).getDate();
  const cells = [];
  for (let i = off - 1; i >= 0; i--) cells.push({ y: m === 0 ? y - 1 : y, m: (m + 11) % 12, d: prevDays - i, out: true });
  for (let d = 1; d <= days; d++) cells.push({ y, m, d, out: false });
  let nd = 1;
  while (cells.length % 7) cells.push({ y: m === 11 ? y + 1 : y, m: (m + 1) % 12, d: nd++, out: true });
  let h = `<div class="cal-grid">` + wd.map(w => `<span class="cal-wd">${w}</span>`).join('');
  for (const c of cells) {
    const k = dKey(c.y, c.m, c.d);
    let cls = 'cal-day' + (c.out ? ' out' : '');
    if (st.from && st.to && k > st.from && k < st.to) cls += ' inr';
    if (k === st.from || k === st.to) cls += ' sel';
    if (k === todayK) cls += ' today';
    h += `<span class="${cls}" onclick="window.__calDay('${slot}',${c.y},${c.m},${c.d})">${c.d}</span>`;
  }
  return h + '</div>';
}
function __calRender(slot) {
  const st = DD_STATE[slot + 'Cal'];
  const menu = document.getElementById(slot + 'PickMenu');
  if (!st || !menu) return;
  const m2 = (st.m + 1) % 12, y2 = st.m === 11 ? st.y + 1 : st.y;
  const FICAL = `<span class="fical">${CAL_IC}</span>`;
  menu.innerHTML = `
    <div class="cal-top">
      <div class="cal-navs"><span class="cal-nav" title="${tr('上一年', 'Prev year')}" onclick="window.__calNav('${slot}',-12)">«</span><span class="cal-nav" title="${tr('上一月', 'Prev month')}" onclick="window.__calNav('${slot}',-1)">‹</span></div>
      <div class="cal-title">${st.y} - ${st.m + 1}</div>
      <div class="cal-title">${y2} - ${m2 + 1}</div>
      <div class="cal-navs"><span class="cal-nav" title="${tr('下一月', 'Next month')}" onclick="window.__calNav('${slot}',1)">›</span><span class="cal-nav" title="${tr('下一年', 'Next year')}" onclick="window.__calNav('${slot}',12)">»</span></div>
    </div>
    <div class="cal-cols">
      <div>${__calMonthHTML(slot, st.y, st.m, st)}</div>
      <div>${__calMonthHTML(slot, y2, m2, st)}</div>
    </div>
    <div class="cal-foot">
      <div class="fbox">${FICAL}<span class="fd">${esc(st.from || tr('开始日期', 'Start'))}</span><input type="time" step="1" id="${slot}Tf" value="${st.tf}" onchange="window.__calTime('${slot}','tf',this.value)"></div>
      <div class="fbox">${FICAL}<span class="fd">${esc(st.to || tr('结束日期', 'End'))}</span><input type="time" step="1" id="${slot}Tt" value="${st.tt}" onchange="window.__calTime('${slot}','tt',this.value)"></div>
      <button class="primary apply" onclick="window.__ddApply('${slot}Pick')">${tr('确定', 'OK')}</button>
    </div>`;
}
function closeRangePanels() {
  document.querySelectorAll('.rpanel').forEach(m => { m.style.display = 'none'; });
}
/* 弹层挂 body + fixed 定位：贴住范围框下方，放不下翻到上方，并做视口钳制 */
function positionPanel(slot) {
  const menu = document.getElementById(slot + 'PickMenu');
  const ctl = document.getElementById(slot);
  const box = ctl && ctl.querySelector('.range-box');
  if (!menu || !box) return;
  const r = box.getBoundingClientRect();
  const mw = menu.offsetWidth, mh = menu.offsetHeight;
  const left = Math.min(Math.max(12, r.right - mw), window.innerWidth - mw - 12);
  let top = r.bottom + 8;
  if (top + mh > window.innerHeight - 12) top = Math.max(12, r.top - mh - 8);
  menu.style.left = left + 'px';
  menu.style.top = top + 'px';
}
window.__ddToggle = (slotPick) => {
  const slot = slotPick.replace(/Pick$/, '');
  const menu = document.getElementById(slot + 'PickMenu');
  if (!menu) return;
  const show = menu.style.display !== 'block' || menu.parentElement !== document.body;
  closeRangePanels();
  if (show) {
    DD_STATE[slot + 'Cal'] = __calInit(slot);
    menu.style.display = 'block';
    if (menu.parentElement !== document.body) document.body.appendChild(menu);
    __calRender(slot);
    positionPanel(slot);
  }
};
window.__calNav = (slot, dm) => {
  const st = DD_STATE[slot + 'Cal']; if (!st) return;
  const t = st.y * 12 + st.m + dm;
  st.y = Math.floor(t / 12); st.m = ((t % 12) + 12) % 12;
  __calRender(slot);
};
window.__calDay = (slot, y, m, d) => {
  const st = DD_STATE[slot + 'Cal']; if (!st) return;
  const k = dKey(y, m, d);
  if (!st.from || (st.from && st.to)) { st.from = k; st.to = null; }
  else {
    st.to = k;
    if (st.to < st.from) { const tmp = st.from; st.from = st.to; st.to = tmp; }
  }
  __calRender(slot);
};
window.__calTime = (slot, which, v) => {
  const st = DD_STATE[slot + 'Cal']; if (!st) return;
  st[which] = v || (which === 'tf' ? '00:00:00' : '23:59:59');
};
window.__ddPick = (slot, k) => {
  const s = DD_STATE[slot]; if (!s) return;
  s.setKey(k);
  closeRangePanels();
};
window.__ddClear = (slot) => {
  const s = DD_STATE[slot]; if (!s) return;
  s.setKey((s.ranges[0] || [])[0] || 'today');
};
window.__ddApply = (slotPick) => {
  const slot = slotPick.replace(/Pick$/, '');
  const s = DD_STATE[slot]; const st = DD_STATE[slot + 'Cal'];
  if (!s || !st) return;
  if (!st.from) { toast(tr('请选择起始日期', 'Pick a start date first'), false); return; }
  let from = st.from + ' ' + (st.tf || '00:00:00');
  let to = (st.to || st.from) + ' ' + (st.tt || '23:59:59');
  if (from > to) { const tmp = from; from = to; to = tmp; }
  s.setCustom({ from, to });
  s.setKey('custom');
  closeRangePanels();
};
document.addEventListener('click', (e) => {
  if (!e.target.closest || (!e.target.closest('.range-ctl') && !e.target.closest('.rpanel'))) {
    closeRangePanels();
  }
});
/* 滚动/缩放时收起弹层，避免悬浮错位 */
window.addEventListener('scroll', closeRangePanels, true);
window.addEventListener('resize', closeRangePanels);

/* 按日期键过滤每日数据（today/yesterday/7d/30d/month/lastmonth/year/all/custom，日期键跟随所选时区） */
function filterRangeDates(daily, key, custom) {
  const today = tzTodayKey();
  const sh = tzShifted();
  const Y = sh.getUTCFullYear(), M = sh.getUTCMonth();
  const ym = today.slice(0, 7);
  if (key === 'custom') {
    const f = custom || {};
    return daily.filter(p => p.date && (!f.from || p.date >= f.from.slice(0, 10)) && (!f.to || p.date <= f.to.slice(0, 10)));
  }
  if (key === 'today') return daily.filter(p => p.date === today);
  if (key === 'yesterday') return daily.filter(p => p.date === tzDaysAgoKey(1));
  if (key === '7d') return daily.filter(p => p.date >= tzDaysAgoKey(6) && p.date <= today);
  if (key === 'month') return daily.filter(p => p.date.startsWith(ym + '-'));
  if (key === 'lastmonth') {
    const a = new Date(Date.UTC(Y, M - 1, 1)), b = new Date(Date.UTC(Y, M, 0));
    const from = dKey(a.getUTCFullYear(), a.getUTCMonth(), 1);
    const to = dKey(b.getUTCFullYear(), b.getUTCMonth(), b.getUTCDate());
    return daily.filter(p => p.date >= from && p.date <= to);
  }
  if (key === 'year') return daily.filter(p => p.date.startsWith(today.slice(0, 4) + '-'));
  if (key === 'all') return daily.filter(p => p.date);
  return daily.filter(p => p.date >= tzDaysAgoKey(29) && p.date <= today);
}

function renderTokenPage() {
  const d = TSTATS[TSTAT.src] || {};
  bindRangeDD('tokenRangeDD', {
    ranges: TOKEN_RANGES,
    getKey: () => TSTAT.range,
    setKey: (k) => { TSTAT.range = k; TSTAT.page = 0; renderTokenPage(); },
    getCustom: () => TSTAT.custom,
    setCustom: (c) => { TSTAT.custom = c; TSTAT.page = 0; },
  });
  const fs = document.getElementById('tokenFmtSeg');
  if (fs) fs.innerHTML = [['km', 'K/M'], ['cn', tr('万/亿', 'Wan/Yi')]].map(([k, l]) =>
    `<button class="${TOKFMT === k ? 'active' : ''}" onclick="setPref('tokenFmt','${k}')">${l}</button>`).join('');
  document.getElementById('distTabs').innerHTML =
    [['projects', tr('按项目', 'By Project')], ['models', tr('按模型', 'By Model')]].map(([k, l]) =>
      `<button class="${TSTAT.dist === k ? 'active' : ''}" onclick="__setTokenDist('${k}')">${l}</button>`).join('');

  /* 统一区间：指标卡 / 每日图 / 分布 / 请求明细全部按所选时间过滤 */
  const daily = filterRangeDates((d.daily || []).filter(p => p.date), TSTAT.range, TSTAT.custom);
  const sumTok = daily.reduce((s, p) => s + (p.tokens || 0), 0);
  const sumIn = daily.reduce((s, p) => s + (p.in || 0), 0);
  const sumOut = daily.reduce((s, p) => s + (p.out || 0), 0);
  const sumReq = daily.reduce((s, p) => s + (p.requests || 0), 0);
  const spanLabel = tr('范围内消耗', 'Used in Range');
  /* cn 模式按参考样式：完整数字为主体，下方 ≈ 万/亿 换算；km 模式维持 K/M 紧凑格式 */
  const metric = (label, tok, req) => {
    const val = TOKFMT === 'cn'
      ? `${tok.toLocaleString('en-US')}<small> Tokens</small><div class="ml" style="margin-top:4px">≈ ${cnNum(tok)}</div>`
      : `${fmtTok(tok)}<small> Tokens</small>`;
    return `<div class="metric"><div class="ml">${label}</div><div class="mv">${val}</div><div class="ml" style="margin-top:6px">${fmtNum(req, 0)} ${tr('次请求', 'requests')}</div></div>`;
  };
  const metricReq = (label, req) =>
    `<div class="metric"><div class="ml">${label}</div><div class="mv">${fmtNum(req, 0)}<small> ${tr('次请求', 'requests')}</small></div><div class="ml" style="margin-top:6px">${fmtTok(sumTok)} Tokens</div></div>`;
  document.getElementById('tokenMetrics').innerHTML =
    metric(`${ic('down')} ${spanLabel}`, sumTok, sumReq) +
    metric(`${ic('tin')} ${tr('输入 Tokens（估）', 'Input Tokens (est)')}`, sumIn, sumReq) +
    metric(`${ic('tout')} ${tr('输出 Tokens（估）', 'Output Tokens (est)')}`, sumOut, sumReq) +
    metricReq(`${ic('bolt')} ${tr('请求次数', 'Requests')}`, sumReq);

  renderTokenChart(d, daily);
  renderTokenDist(d);
  renderTokenReqs(d);
}
window.__setTokenDist = (k) => { TSTAT.dist = k; renderTokenPage(); };

function renderTokenChart(d, daily) {
  daily = daily || filterRangeDates((d.daily || []).filter(p => p.date), TSTAT.range, TSTAT.custom);

  const el = document.getElementById('tokenChart');
  const tokSum = daily.reduce((s, p) => s + (p.tokens || 0), 0);
  const reqSum = daily.reduce((s, p) => s + (p.requests || 0), 0);
  document.getElementById('tokenChartSum').textContent = `${tr('近段合计 ', 'Range total ')}${fmtTok(tokSum)} Tokens · ${fmtNum(reqSum, 0)} ${tr('次请求', 'requests')}`;
  document.getElementById('tokenChartUpdated').textContent = tmsg(d.label || '');
  if (!daily.length) { el.innerHTML = `<div class="empty" style="width:100%">${tr('暂无请求数据', 'No request data')}</div>`; return; }
  const maxTok = Math.max(...daily.map(p => p.tokens || 0), 1);
  el.innerHTML = daily.map(p => {
    const pct = Math.max((p.tokens || 0) > 0 ? 3 : 1, ((p.tokens || 0) / maxTok) * 100);
    return `<div class="bar ${(p.tokens || 0) > 0 ? '' : 'zero'}" style="height:${pct}%"
      data-d="${p.date}" data-t="${fmtTok(p.tokens)}" data-i="${fmtTok(p.in || 0)}" data-o="${fmtTok(p.out || 0)}" data-r="${p.requests || 0}"></div>`;
  }).join('');
  attachChartTip(el, p => `${p.dataset.d}<br>${tr('消耗 ', 'Used ')}<b>${p.dataset.t}</b> Tokens（${tr('估', 'est')}）<br>${tr('输入 ', 'In ')}${p.dataset.i} · ${tr('输出 ', 'Out ')}${p.dataset.o}<br>${tr('请求 ', 'Requests ')}<b>${p.dataset.r}</b>`);
}

/* 当前区间允许的日期集合（'全部' 返回 null 表示不过滤） */
function rangeDateSet(daily) {
  if (TSTAT.range === 'all') return null;
  return new Set(filterRangeDates(daily.filter(p => p.date), TSTAT.range, TSTAT.custom).map(p => p.date));
}

function renderTokenDist(d) {
  const raw = (TSTAT.dist === 'projects' ? d.projects : d.models) || [];
  document.getElementById('distDesc').textContent =
    TSTAT.dist === 'projects' ? tr('消耗 Tokens 按项目目录分布', 'Token usage by project directory') : tr('消耗 Tokens 按模型分布', 'Token usage by model');
  const allowed = rangeDateSet(d.daily || []);
  /* 按区间聚合：优先用后端按日细分；旧数据无 daily 时回退总量 */
  const list = raw.map(x => {
    const dm = x.daily || {};
    if (!allowed || !Object.keys(dm).length) return { name: x.name, tokens: x.tokens || 0, requests: x.requests || 0 };
    let tk = 0, rq = 0;
    for (const dt of allowed) { const e = dm[dt]; if (e) { tk += e.tokens || 0; rq += e.requests || 0; } }
    return { name: x.name, tokens: tk, requests: rq };
  }).filter(x => x.tokens > 0 || x.requests > 0);
  const body = document.getElementById('distBody');
  if (!list.length) { body.innerHTML = `<div class="empty">${tr('当前范围暂无数据', 'No data in range')}</div>`; return; }
  const max = Math.max(...list.map(x => x.tokens || 0), 1);
  const total = list.reduce((s, x) => s + (x.tokens || 0), 0);
  body.innerHTML = list.map(x => {
    const pct = ((x.tokens || 0) / max) * 100;
    const share = total > 0 ? ((x.tokens || 0) / total * 100).toFixed(1) : '0.0';
    return `<div class="hrow">
      <div class="hr-top"><span class="l" title="${esc(x.name)}">${esc(x.name)}</span>
        <span class="r">${fmtTok(x.tokens)} Tokens · ${x.requests} ${tr('次', 'req')} <b>${share}%</b></span></div>
      <div class="hr-track"><div class="hr-fill" style="width:${Math.max(1, pct)}%"></div></div>
    </div>`;
  }).join('');
}

const REQ_PAGE_SIZE = 50;
function renderTokenReqs(d) {
  const allowed = rangeDateSet(d.daily || []);
  const list = (d.requests || []).filter(r => r.ts && (!allowed || allowed.has(String(r.ts).slice(0, 10))));
  const pageCount = Math.max(1, Math.ceil(list.length / REQ_PAGE_SIZE));
  TSTAT.page = Math.min(Math.max(TSTAT.page, 0), pageCount - 1);
  const start = TSTAT.page * REQ_PAGE_SIZE;
  const rows = list.slice(start, start + REQ_PAGE_SIZE);
  document.getElementById('tokenReqBody').innerHTML = rows.length ? rows.map(r => `
    <tr>
      <td>${esc(r.ts)}</td>
      <td>${esc(r.model)}</td>
      <td class="ell" title="${esc(r.cwd)}">${esc((r.cwd || '').split('/').filter(Boolean).slice(-1)[0] || '—')}</td>
      <td class="ell" title="${esc(r.title)}">${esc(r.title || '—')}</td>
      <td class="num">${fmtTok(r.inTokens)}</td>
      <td class="num">${fmtTok(r.outTokens)}</td>
      <td class="num">${fmtDur(r.durationMs)}</td>
    </tr>`).join('') : `<tr><td colspan="7" style="text-align:center;color:var(--muted);padding:24px">${tr('暂无请求记录', 'No request records')}</td></tr>`;
  document.getElementById('reqPagerInfo').textContent =
    `${start + 1}–${Math.min(start + REQ_PAGE_SIZE, list.length)} / ${list.length} ${tr('条', 'records')}`;
  document.getElementById('reqPrev').disabled = TSTAT.page === 0;
  document.getElementById('reqNext').disabled = TSTAT.page >= pageCount - 1;
}
function reqPage(delta) { TSTAT.page += delta; renderTokenPage(); }
window.reqPage = reqPage;

/* ═════════ 启动 ═════════ */
applyI18nStatic();
renderPrefSelects();
renderThemeSeg();
load();
setInterval(() => {
  if (document.querySelector('#page-accounts.active')) load();
}, 30000);
