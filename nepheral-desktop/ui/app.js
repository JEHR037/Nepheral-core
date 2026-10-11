const { invoke } = window.__TAURI__.core;
const $ = (id) => document.getElementById(id);

// ---------- i18n bootstrap ----------
document.documentElement.lang = LANG;
document.querySelectorAll('[data-i18n]').forEach(n => { n.textContent = t(n.dataset.i18n); });
document.querySelectorAll('[data-i18n-ph]').forEach(n => { n.placeholder = t(n.dataset.i18nPh); });
$('lang-toggle').textContent = LANG === 'es' ? 'EN' : 'ES';
$('lang-toggle').onclick = () => setLang(LANG === 'es' ? 'en' : 'es');
// The confirmation dialogs are raised natively, outside this page.
invoke('set_ui_lang', { lang: LANG }).catch(() => {});

// ---------- infra ----------
function toast(msg, isErr) {
  const box = document.createElement('div');
  box.className = 'toast' + (isErr ? ' err' : '');
  box.textContent = msg;
  $('toasts').appendChild(box);
  setTimeout(() => box.remove(), isErr ? 8000 : 4000);
}
async function call(cmd, args) {
  try { return await invoke(cmd, args || {}); }
  catch (e) {
    const s = String(e);
    if (s.startsWith('locked:')) askUnlock();
    else if (s.startsWith('declined:')) toast(t('lk.declined'));
    else toast(friendlyError(e), true);
    throw e;
  }
}

// ---------- the wallet's lock ----------
// The node serves the network from launch. Chats and every action on the
// wallet wait for the password; payments, trades and votes are confirmed in
// a native dialog the page cannot answer.
let UNLOCKED = false;
function applyLock(unlocked) {
  const changed = unlocked !== UNLOCKED;
  UNLOCKED = unlocked;
  const chip = $('c-lock');
  chip.textContent = unlocked ? t('lk.unlocked') : t('lk.locked');
  chip.className = 'chip lockchip ' + (unlocked ? 'unlocked' : 'locked');
  chip.title = unlocked ? t('lk.chipTipUnlocked') : t('lk.chipTipLocked');
  $('chat-locked').style.display = unlocked ? 'none' : '';
  document.querySelector('#page-chat .chat-layout').style.display = unlocked ? '' : 'none';
  $('rooms-locked').style.display = unlocked ? 'none' : '';
  $('rooms-main').style.display = unlocked ? '' : 'none';
  if (!unlocked) $('room-active-card').style.display = 'none';
  if (changed && loaders[activePage]) loaders[activePage]();
}
let unlockWaiters = [];
let unlockRestore = false;
function unlockMode(restore) {
  unlockRestore = restore;
  $('ul-open').style.display = restore ? 'none' : '';
  $('ul-restore').style.display = restore ? '' : 'none';
  $('ul-title').textContent = t(restore ? 'v.restore.title' : 'lk.dlgTitle');
  $('ul-lead').textContent = t(restore ? 'lk.restoreLead' : 'lk.dlgLead');
  $('ul-yes').textContent = t(restore ? 'lk.restoreGo' : 'lk.unlock');
  $('ul-forgot').textContent = t(restore ? 'v.back' : 'v.forgot');
  $('ul-err').textContent = '';
}
function askUnlock() {
  return new Promise((resolve) => {
    unlockWaiters.push(resolve);
    const dlg = $('unlock-dlg');
    if (dlg.open) return;
    unlockMode(false);
    $('ul-pass').value = '';
    dlg.showModal();
    $('ul-pass').focus();
  });
}
function unlockDone(ok) {
  const dlg = $('unlock-dlg');
  if (dlg.open) dlg.close();
  ['ul-pass', 'ul-phrase', 'ul-new', 'ul-new2'].forEach((id) => { $(id).value = ''; });
  const waiters = unlockWaiters; unlockWaiters = [];
  waiters.forEach((r) => r(ok));
}
$('ul-forgot').onclick = () => unlockMode(!unlockRestore);
$('ul-no').onclick = () => unlockDone(false);
$('unlock-dlg').addEventListener('cancel', () => unlockDone(false));
async function unlockGo() {
  $('ul-err').textContent = '';
  $('ul-yes').disabled = true;
  try {
    if (unlockRestore) {
      const pw = $('ul-new').value;
      if (pw !== $('ul-new2').value) { $('ul-err').textContent = t('v.err.match'); return; }
      if (pw.length < 8) { $('ul-err').textContent = t('v.err.weak'); return; }
      $('ul-err').textContent = t('v.sealing');
      await invoke('wallet_restore', { phrase: $('ul-phrase').value, password: pw });
    } else {
      $('ul-err').textContent = t('v.opening');
      await invoke('wallet_unlock', { password: $('ul-pass').value });
    }
    applyLock(true);
    unlockDone(true);
  } catch (e) {
    const s = String(e);
    $('ul-err').textContent = /wrong password/i.test(s) ? t('v.err.wrong')
      : /mnemonic|phrase/i.test(s) ? t('v.err.phrase') : friendlyError(e);
  } finally {
    $('ul-yes').disabled = false;
  }
}
$('ul-yes').onclick = unlockGo;
$('ul-pass').addEventListener('keydown', (e) => { if (e.key === 'Enter') unlockGo(); });
document.querySelectorAll('[data-unlock]').forEach((b) => { b.onclick = () => askUnlock(); });
async function toggleLock() {
  if (UNLOCKED) {
    await invoke('wallet_lock').catch(() => {});
    applyLock(false);
  } else {
    askUnlock();
  }
}
$('c-lock').onclick = toggleLock;
// Redraws a scrolling list without taking it away from whoever is reading it.
//
// These lists refresh about once a second to re-age their timestamps, and each
// refresh used to end with scrollTop = scrollHeight. Scrolling up to read
// anything was therefore impossible: the next tick snapped you back down. The
// view now follows new content only when it was already at the bottom, which
// is where someone watching a conversation arrive actually is.
const STICK_TO_BOTTOM_PX = 48;
function stickyRender(box, render, forceBottom) {
  const atBottom =
    box.scrollHeight - box.scrollTop - box.clientHeight <= STICK_TO_BOTTOM_PX;
  const previous = box.scrollTop;
  render();
  box.scrollTop = forceBottom || atBottom ? box.scrollHeight : previous;
}

function fmtAgo(s) {
  return s < 60 ? t('ago.s', { n: s })
    : s < 3600 ? t('ago.m', { n: Math.floor(s / 60) })
    : t('ago.h', { n: Math.floor(s / 3600) });
}
function fmtUp(s) { return s < 60 ? s + 's' : s < 3600 ? Math.floor(s / 60) + 'm' : Math.floor(s / 3600) + 'h'; }
function copy(text, what) {
  navigator.clipboard.writeText(text).then(() => toast(t('common.copied', { what })));
}
function confirmDlg(text) {
  return new Promise((resolve) => {
    $('confirm-text').textContent = text;
    const dlg = $('confirm-dlg');
    const yes = $('confirm-yes'), no = $('confirm-no');
    const done = (v) => { dlg.close(); yes.onclick = no.onclick = null; resolve(v); };
    yes.onclick = () => done(true);
    no.onclick = () => done(false);
    dlg.showModal();
  });
}


function askDislike() {
  return new Promise((resolve) => {
    const dlg = $('rate-dlg');
    $('rate-note').value = '';
    $('rate-msg').textContent = '';
    const done = (v) => { dlg.close(); $('rate-yes').onclick = null; $('rate-no').onclick = null; resolve(v); };
    $('rate-yes').onclick = () => {
      const note = $('rate-note').value.trim();
      if ([...note].length < 8) { $('rate-msg').textContent = t('rate.noteShort'); return; }
      done({ reason: $('rate-reason').value, note });
    };
    $('rate-no').onclick = () => done(null);
    dlg.showModal();
  });
}

function ratingControls(m, submit) {
  const box = el('span', 'rate');
  if (!m.rateable) return box;
  if (m.rated !== null && m.rated !== undefined) {
    box.appendChild(el('span', 'dim', t(m.rated ? 'rate.didLike' : 'rate.didDislike')));
    return box;
  }
  const up = el('button', 'btn ghost sm', t('rate.like'));
  const down = el('button', 'btn ghost sm', t('rate.dislike'));
  up.title = t('rate.likeTip');
  down.title = t('rate.dislikeTip');
  const send = async (args) => {
    up.disabled = true; down.disabled = true;
    try { await call(submit.cmd, Object.assign({}, submit.args, args)); toast(t('rate.sent')); }
    catch (_) { up.disabled = false; down.disabled = false; }
  };
  up.onclick = () => send({ like: true, reason: null, note: null });
  down.onclick = async () => {
    const why = await askDislike();
    if (why) send({ like: false, reason: why.reason, note: why.note });
  };
  box.appendChild(up);
  box.appendChild(down);
  return box;
}

// ---------- tabs ----------
let activePage = 'chat';
const loaders = {};
document.querySelectorAll('.tab').forEach(b => b.addEventListener('click', () => selectPage(b.dataset.page)));
function selectPage(page) {
  activePage = page;
  document.querySelectorAll('.tab').forEach(x => x.classList.toggle('active', x.dataset.page === page));
  document.querySelectorAll('.page').forEach(x => x.classList.remove('active'));
  $('page-' + page).classList.add('active');
  if (loaders[page]) loaders[page]();
}

// ---------- header status (2s) ----------
async function refreshStatus() {
  try {
    const s = await invoke('status');
    $('c-peers').textContent = s.peers_connected;
    $('c-peers').className = s.peers_connected > 0 ? 'ok' : 'bad';
    $('c-height').textContent = s.chain_height;
    $('c-final').textContent = s.finalized_height;
    $('c-final').parentElement.title = t('chip.finalTip');
    $('c-blocks').textContent = s.chain_blocks;
    $('c-balance').textContent = s.balance;
    $('c-pid').textContent = s.peer_id_short;
    applyLock(s.unlocked);
    const m = $('c-model');
    m.classList.remove('warn');
    if (s.model === 'ready') { m.innerHTML = '<b>' + t('chip.model.ready') + '</b>'; m.classList.add('ok'); }
    else if (s.model === 'stopped') {
      m.innerHTML = '<b>' + t('chip.model.stopped') + '</b>';
      m.classList.remove('ok'); m.classList.add('warn');
      m.title = s.ai_status || '';
    }
    else if (s.model.startsWith('downloading')) {
      m.innerHTML = '<b>' + t('chip.model.downloading', { pct: s.model.split(':')[1] }) + '</b>';
      m.classList.remove('ok');
    } else { m.innerHTML = '<b>' + t('chip.model.none') + '</b>'; m.classList.remove('ok'); }
    if (s.first_run && $('tab-setup').style.display === 'none') {
      $('tab-setup').style.display = '';
      selectPage('setup');
    }
    const up = $('c-update');
    const rules = s.rules_upgrade;
    up.style.display = (UPDATE || s.update_available || rules || UPDATING) ? '' : 'none';
    if (UPDATING) {
      up.innerHTML = '<b>' + t('chip.updating') + '</b>';
    } else if (rules) {
      // New chain rules outrank a newer peer: they stop this node.
      const when = new Date(rules.activate_at_ms).toLocaleString();
      up.innerHTML = '<b>' + (rules.required ? t('chip.rulesNow') : t('chip.rulesSoon', { when })) + '</b>';
      up.title = rules.required ? t('chip.rulesNowTip') : t('chip.rulesSoonTip', { when });
      up.classList.add('bad');
    } else if (UPDATE) {
      up.innerHTML = '<b>' + t('chip.updateTo', { v: UPDATE.version }) + '</b>';
      up.title = t('chip.updateToTip');
      up.classList.remove('bad');
    } else if (s.update_available) {
      up.innerHTML = '<b>' + t('chip.update') + '</b>';
      up.title = t('chip.updateTip');
      up.classList.remove('bad');
    }
    // With a release to install, the chip installs it; without one (no
    // feed reachable yet), it hands over the download page.
    up.onclick = UPDATE ? installUpdate : () => copy('https://www.nepheral.net/download', t('chip.updateCopied'));
  } catch (_) {}
}
setInterval(refreshStatus, 2000);
refreshStatus();

// ---------- updates ----------
// A newer release of this same build (CPU, Vulkan or CUDA), from the
// download depot's signed feed.
let UPDATE = null;
let UPDATING = false;
async function checkUpdate() {
  try { UPDATE = await invoke('update_check'); } catch (_) { /* offline, or no feed yet */ }
}
async function installUpdate() {
  if (UPDATING) return;
  UPDATING = true;
  refreshStatus();
  try { await call('update_install'); }
  catch (_) { UPDATING = false; refreshStatus(); }
}
setTimeout(checkUpdate, 20000);
setInterval(checkUpdate, 6 * 3600 * 1000);

// ============================================================
// CHAT
// ============================================================
let chatActive = null;
let sessionCount = 0;

$('chat-new-local').onclick = async () => { chatActive = await call('chat_new', { kind: 'local' }); loadChatSessions(); };
$('chat-new-p2p').onclick = async () => { chatActive = await call('chat_new', { kind: 'p2p' }); loadChatSessions(); };

// The session being renamed: the list is not redrawn under the field.
let chatRenaming = null;

async function loadChatSessions() {
  if (chatRenaming !== null) return;
  const rows = await call('chat_sessions');
  sessionCount = rows.length;
  const box = $('chat-sessions');
  box.innerHTML = '';
  if (!rows.length) box.appendChild(el('div', 'hint', t('chat.noSessions')));
  for (const r of rows) {
    const item = el('div', 'sess-item' + (r.id === chatActive ? ' active' : ''));
    const left = el('div', 'sess-left');
    left.appendChild(el('div', r.kind === 'local' ? 'ok' : '', r.label + (r.awaiting ? ' …' : '')));
    left.appendChild(el('div', 'kind', t('chat.msgs', { n: r.msgs }) + ' · ' + fmtAgo(r.idle_secs)));
    if (r.last) left.appendChild(el('div', 'kind sess-last', r.last));
    item.appendChild(left);
    // Renaming happens in place: the label becomes a field until Enter.
    const ren = el('button', 'btn ghost sm', '✎');
    ren.title = t('cv.rename');
    ren.onclick = (ev) => {
      ev.stopPropagation();
      chatRenaming = r.id;
      const input = document.createElement('input');
      input.value = r.label;
      input.className = 'sess-rename';
      left.replaceChildren(input);
      input.focus();
      input.select();
      input.onclick = (e) => e.stopPropagation();
      const finish = async (save) => {
        if (chatRenaming !== r.id) return;
        chatRenaming = null;
        if (save && input.value.trim()) await call('chat_rename', { id: r.id, label: input.value });
        loadChatSessions();
      };
      input.onkeydown = (e) => {
        if (e.key === 'Enter') finish(true);
        if (e.key === 'Escape') finish(false);
      };
      input.onblur = () => finish(true);
    };
    item.appendChild(ren);
    const x = el('button', 'btn ghost sm', '✕');
    x.title = t('cv.delete');
    x.onclick = async (ev) => {
      ev.stopPropagation();
      if (r.msgs && !(await confirmDlg(t('cv.delete.ask')))) return;
      await call('chat_leave', { id: r.id });
      if (chatActive === r.id) chatActive = null;
      loadChatSessions();
    };
    item.appendChild(x);
    item.onclick = async () => {
      chatActive = r.id;
      await call('chat_select', { id: r.id });
      loadChatSessions();
    };
    box.appendChild(item);
  }
  renderChatHead(rows.find(r => r.id === chatActive));
  renderThread();
}

let chatKind = 'local';
function renderChatHead(row) {
  if (typeof chatPrivate !== 'undefined' && chatPrivate) return;
  if (!row) {
    $('chat-head').style.display = 'none';
    $('input').placeholder = t('chat.ph.none');
    return;
  }
  $('chat-head').style.display = 'flex';
  chatKind = row.kind;
  $('chat-title').textContent = row.label;
  $('chat-kind-hint').textContent = ' — ' + t(row.kind === 'local' ? 'chat.kind.local' : 'chat.kind.p2p');
  $('trust-wrap').style.display = row.kind === 'p2p' ? '' : 'none';
  $('input').placeholder = t(row.kind === 'local' ? 'chat.ph.local' : 'chat.ph.p2p');
}

$('trust').onchange = async () => {
  const th = await call('set_trust_filter', { level: $('trust').value });
  if ($('trust').value !== 'any') toast(t('chat.trustSet', { n: th }));
};

function emptyState(big, small, actions) {
  const box = el('div', 'empty-state');
  box.appendChild(el('div', 'glyph', '◇'));
  box.appendChild(el('div', 'big', big));
  if (small) {
    const s = el('div', '', small);
    s.style.maxWidth = '52ch';
    box.appendChild(s);
  }
  if (actions) box.appendChild(actions);
  return box;
}

async function renderThread() {
  const thread = $('thread');
  if (chatActive === null) {
    thread.innerHTML = '';
    const actions = el('div', 'row');
    actions.style.justifyContent = 'center';
    const bl = el('button', 'btn xl', t('chat.newLocal'));
    bl.onclick = () => $('chat-new-local').click();
    const bm = el('button', 'btn xl ghost', t('chat.newMesh'));
    bm.onclick = () => $('chat-new-p2p').click();
    actions.appendChild(bl); actions.appendChild(bm);
    thread.appendChild(emptyState(t('chat.emptyTitle'), t('chat.emptyBody'), actions));
    return;
  }
  let tr;
  try { tr = await invoke('chat_thread', { id: chatActive }); }
  catch (_) { return; }

  // Messages only ever arrive, so their count and lengths identify what is
  // drawn. When that has not changed, the only thing this tick can show is a
  // different age -- and rebuilding the thread for that throws away the
  // scroll position and any text the reader has selected.
  const signature = tr.messages.length + ':' +
    tr.messages.map((m) => (m.from_me ? 'u' : 'a') + m.content.length + (m.rated === null || m.rated === undefined ? '' : (m.rated ? '+' : '-'))).join(',') +
    (tr.awaiting ? '|w' : '') + '|' + cvSignature(tr);
  const sameSession = threadDrawn.session === chatActive;

  if (sameSession && threadDrawn.signature === signature) {
    tr.messages.forEach((m, i) => {
      const node = threadDrawn.times[i];
      if (!node) return;
      const next = fmtAgo(m.elapsed_secs);
      // Past a minute the text is identical from one second to the next, so
      // most ticks change nothing at all.
      if (node.textContent !== next) node.textContent = next;
    });
    if (tr.awaiting && threadDrawn.awaiting) {
      threadDrawn.awaiting.textContent = awaitingLine(tr.awaiting_secs, tr.taken);
    }
    return;
  }

  const times = [];
  let awaitingNode = null;
  stickyRender(thread, () => {
    thread.innerHTML = '';
    if (!tr.messages.length) {
      thread.appendChild(emptyState(t('chat.noMsgs'),
        t(tr.kind === 'local' ? 'chat.noMsgsLocal' : 'chat.noMsgsMesh')));
    }
    tr.messages.forEach((m, index) => {
      const box = el('div', 'msg ' + (m.from_me ? 'user' : 'assistant') + (m.private ? ' private' : ''));
      const who = el('div', m.from_me ? 'warn' : 'ok', (m.from_me ? t('chat.you') : t('chat.agent')) + ' ▶' + (m.private ? '  🔒 ' + t('cv.private.on') : ''));
      who.style.fontSize = '11px'; who.style.fontWeight = '600';
      box.appendChild(who);
      // The agent's reply is Markdown; what the user typed is not, so
      // their own asterisks stay as they wrote them.
      if (m.error) {
        box.className = 'msg error';
        box.appendChild(el('div', 'md-plain', cvErrorText(m)));
      } else if (m.from_me) {
        box.appendChild(el('div', 'md-plain', m.content));
      } else {
        const body = el('div', 'md');
        body.appendChild(renderMarkdown(m.content));
        box.appendChild(body);
      }
      const foot = el('div', 'foot');
      const age = el('div', 'dim', fmtAgo(m.elapsed_secs));
      times.push(age);
      foot.appendChild(age);
      // Only on answers: copying back what you just typed is not worth a
      // button, and an answer is the thing people actually take elsewhere.
      if (!m.from_me) foot.appendChild(copyButton(m.content));
      if (!m.from_me) foot.appendChild(ratingControls(m, { cmd: 'rate_chat_reply', args: { id: chatActive, index } }));
      foot.appendChild(cvPrivateButton(chatActive, index, m.private, () => { threadDrawn.signature = null; renderThread(); }));
      box.appendChild(foot);
      thread.appendChild(box);
    });
    if (tr.awaiting) {
      awaitingNode = el('div', 'msg status', awaitingLine(tr.awaiting_secs, tr.taken));
      thread.appendChild(awaitingNode);
    }
    if (tr.pending) {
      const session = chatActive;
      thread.appendChild(cvPendingCard(session, tr.pending, (text) => {
        if (text && !$('input').value.trim()) $('input').value = text;
        threadDrawn.signature = null;
        renderThread();
      }));
    }
  }, !sameSession || threadJumpToBottom);
  threadJumpToBottom = false;

  threadDrawn = { session: chatActive, signature, times, awaiting: awaitingNode };
}

// Copies an answer, and says so on the button itself.
//
// Selecting the text by hand is awkward: .msg is a pre-wrap block, and until
// recently the thread rebuilt itself under the cursor about once a second,
// which dropped the selection mid-drag.
function copyButton(text) {
  const b = el('button', 'btn ghost sm copy', t('chat.copy'));
  b.onclick = async () => {
    const ok = await copyText(text);
    b.textContent = t(ok ? 'chat.copied' : 'chat.copyFailed');
    setTimeout(() => { b.textContent = t('chat.copy'); }, 1500);
  };
  return b;
}

// navigator.clipboard is the right API, but it needs a secure context and a
// user gesture and rejects rather than throwing where it is refused. The
// textarea path still works there, and a copy that fails both ways is
// reported on the button instead of looking like it worked.
async function copyText(text) {
  try {
    if (navigator.clipboard && window.isSecureContext) {
      await navigator.clipboard.writeText(text);
      return true;
    }
  } catch (_) { /* fall through to the older path */ }
  try {
    const ta = document.createElement('textarea');
    ta.value = text;
    ta.setAttribute('readonly', '');
    ta.style.position = 'fixed';
    ta.style.top = '-1000px';
    ta.style.opacity = '0';
    document.body.appendChild(ta);
    ta.select();
    const ok = document.execCommand('copy');
    document.body.removeChild(ta);
    return ok;
  } catch (_) {
    return false;
  }
}

function awaitingLine(secs, taken) {
  if (taken) return '✎  ' + t('chat.writing') + '  (' + secs + 's)';
  const quips = t('quips');
  return '◐  ' + quips[Math.floor(secs / 2) % quips.length] + '  (' + secs + 's)';
}

// What the thread currently shows, so a tick can tell "nothing changed" from
// "a message arrived" without rebuilding it to find out.
let threadDrawn = { session: null, signature: null, times: [], awaiting: null };
// Set when the reader's own action should win over where they had scrolled:
// sending a message means they want to see it, wherever they were looking.
let threadJumpToBottom = false;

async function chatSend() {
  const text = $('input').value.trim();
  if (!text) return;

  // Someone who has typed a message and pressed Send has said what they want.
  // Refusing because no conversation exists yet made the button do nothing at
  // all, with no hint that a session had to be opened first, so the message is
  // given one instead of being dropped.
  if (chatActive === null) {
    chatActive = await call('chat_new', { kind: 'local' });
    await loadChatSessions();
  }

  $('input').value = '';
  threadJumpToBottom = true;
  const priv = chatPrivate;
  setChatPrivate(false);
  await call('chat_send', { id: chatActive, content: text, private: priv });
  renderThread();
}
$('send').onclick = chatSend;

// The next message is a private note: kept in the conversation, never sent.
let chatPrivate = false;
function setChatPrivate(on) {
  chatPrivate = on;
  $('private-toggle').classList.toggle('on', on);
  $('private-toggle').title = t(on ? 'cv.private.on' : 'cv.private.composer');
  $('input').classList.toggle('private', on);
  if (on) $('input').placeholder = t('cv.ph.private');
  else renderChatHead(null === chatActive ? null : { kind: chatKind, label: $('chat-title').textContent });
}
$('private-toggle').onclick = () => setChatPrivate(!chatPrivate);
setChatPrivate(false);

// NAI's private memory: its model's state, and keeping conversations.
let memoryDrawn = '';
async function refreshMemory() {
  let v;
  try { v = await invoke('memory_view'); } catch (_) { return; }
  const key = JSON.stringify(v);
  if (key === memoryDrawn) return;
  memoryDrawn = key;
  const snoozed = cvSnoozeNode(v, () => { memoryDrawn = ''; refreshMemory(); });
  $('chat-mem-box').replaceChildren(cvMemoryNode(v), ...(snoozed ? [snoozed] : []));
  $('keep-convs').checked = v.keep_conversations;
}
$('keep-convs').onchange = async () => {
  try { await call('memory_keep', { keep: $('keep-convs').checked }); }
  catch (_) { $('keep-convs').checked = !$('keep-convs').checked; }
  memoryDrawn = '';
  refreshMemory();
};
$('input').addEventListener('keydown', (e) => { if (e.key === 'Enter' && !e.shiftKey) { e.preventDefault(); chatSend(); } });

loaders.chat = () => { refreshMemory(); if (UNLOCKED) loadChatSessions(); };
setInterval(() => { if (activePage === 'chat' && UNLOCKED) loadChatSessions(); }, 900);
setInterval(() => { if (activePage === 'chat') refreshMemory(); }, 1500);

// ============================================================
// ROOMS
// ============================================================
let activeRoom = null;

$('room-create').onclick = async () => { activeRoom = await call('room_create'); loadRooms(); };
$('room-join').onclick = async () => {
  const code = $('room-code').value.trim();
  if (!code) return;
  activeRoom = await call('room_join', { code });
  $('room-code').value = '';
  loadRooms();
};

async function loadRooms() {
  const rows = await call('rooms_list');
  const hint = $('room-hint');
  hint.innerHTML = '';
  if (!rows.length) {
    hint.textContent = t('rooms.none');
    $('room-active-card').style.display = 'none';
    return;
  }
  for (const r of rows) {
    const b = el('button', 'fchip' + (r.code === activeRoom ? ' active' : ''),
      t('rooms.listItem', { code: r.code, members: r.members, msgs: r.msgs }));
    b.onclick = () => { activeRoom = r.code; loadRooms(); };
    hint.appendChild(b);
  }
  if (!activeRoom && rows.length) activeRoom = rows[0].code;
  renderRoom();
}

async function renderRoom() {
  if (!activeRoom) { $('room-active-card').style.display = 'none'; return; }
  let v;
  try { v = await invoke('room_view', { code: activeRoom }); }
  catch (_) { $('room-active-card').style.display = 'none'; return; }
  $('room-active-card').style.display = '';
  $('room-title').textContent = '◆ ' + v.code;
  let membersLine = t('rooms.members', { n: v.members }) + ' ' + t('rooms.youAre', { me: v.me });
  if (v.member_names.length) membersLine += ' · ' + v.member_names.join(', ');
  if (v.closing_in_secs !== null && v.closing_in_secs !== undefined) {
    membersLine += ' · ' + t('rooms.closing', { s: v.closing_in_secs });
  }
  $('room-members').textContent = membersLine;
  const box = $('room-thread');
  const signature = v.chat.length + ':' +
    v.chat.map((m) => m.codename + m.content.length + (m.rated === null || m.rated === undefined ? '' : (m.rated ? '+' : '-'))).join(',') +
    (v.ai_thinking ? '|t' : '');
  const sameRoom = roomDrawn.code === activeRoom;

  if (sameRoom && roomDrawn.signature === signature) {
    v.chat.forEach((m, i) => {
      const node = roomDrawn.times[i];
      if (!node) return;
      const next = fmtAgo(m.elapsed_secs);
      if (node.textContent !== next) node.textContent = next;
    });
    return;
  }

  const times = [];
  stickyRender(box, () => {
    box.innerHTML = '';
    if (!v.chat.length) box.appendChild(el('div', 'hint', t('rooms.empty')));
    for (const m of v.chat) {
      const row = el('div', 'room-msg' + (m.is_ai ? ' ai' : ''));
      row.appendChild(el('span', 'who', m.codename + ' ▶'));
      if (m.error) {
        row.appendChild(el('span', 'bad', cvErrorText(m)));
      } else if (m.is_ai) {
        const body = el('span', 'md');
        body.appendChild(renderMarkdown(m.content));
        row.appendChild(body);
      } else {
        row.appendChild(el('span', '', m.content));
      }
      const age = el('div', 'dim', fmtAgo(m.elapsed_secs));
      times.push(age);
      row.appendChild(age);
      if (m.is_ai) row.appendChild(ratingControls(m, { cmd: 'rate_room_reply', args: { code: activeRoom, messageId: m.id } }));
      box.appendChild(row);
    }
    if (v.ai_thinking) box.appendChild(el('div', 'hint', t('rooms.thinking')));
  }, !sameRoom || roomJumpToBottom);
  roomJumpToBottom = false;

  roomDrawn = { code: activeRoom, signature, times };
}

// As for the chat thread: what is drawn, so a tick that only re-ages the
// messages does not rebuild the room and scroll it away from the reader.
let roomDrawn = { code: null, signature: null, times: [] };
let roomJumpToBottom = false;

$('room-send').onclick = async () => {
  const msg = $('room-input').value.trim();
  if (!msg || !activeRoom) return;
  $('room-input').value = '';
  roomJumpToBottom = true;
  await call('room_send', { code: activeRoom, content: msg });
  renderRoom();
};
$('room-input').addEventListener('keydown', (e) => { if (e.key === 'Enter') { e.preventDefault(); $('room-send').click(); } });
$('room-leave').onclick = async () => {
  if (!activeRoom) return;
  await call('room_leave', { code: activeRoom });
  activeRoom = null;
  loadRooms();
};

loaders.rooms = () => { if (UNLOCKED) loadRooms(); };
setInterval(() => { if (activePage === 'rooms' && UNLOCKED) renderRoom(); }, 1200);

// ============================================================
// PEERS
// ============================================================
// The table is paginated because the peer list grows with the network.
// `peersPage` lives at module scope on purpose: loadPeers() re-runs every
// 2s from the poll below, and rebuilding the page cursor inside the render
// would snap the user back to page 1 twice a second.
const PEERS_PER_PAGE = 15;
let peersPage = 0;
let peersPagerEls = null;

// Tri-state AI readiness cell. `null` means the node genuinely does not
// know (capabilities arrive keyed by node identity, the table is keyed by
// address), so it says so instead of guessing "no".
function peerAiCell(ready) {
  const key = ready === true ? 'yes' : ready === false ? 'no' : 'unknown';
  const td = el('td', ready === true ? 'ok' : 'dim', t('peers.ai.' + key));
  td.title = t('peers.ai.' + key + 'Tip');
  return td;
}

function ensurePeersPager() {
  if (peersPagerEls) return peersPagerEls;
  const card = $('page-peers').querySelector('.card');
  const hint = el('div', 'hint');
  const row = el('div', 'row');
  const prev = el('button', 'btn ghost sm', t('peers.prev'));
  const label = el('span', 'hint');
  const next = el('button', 'btn ghost sm', t('peers.next'));
  prev.onclick = () => { if (peersPage > 0) { peersPage--; loadPeers(); } };
  next.onclick = () => { peersPage++; loadPeers(); };
  row.appendChild(prev); row.appendChild(label); row.appendChild(next);
  card.appendChild(hint);
  card.appendChild(row);
  peersPagerEls = { hint, row, prev, label, next };
  return peersPagerEls;
}

async function loadPeers() {
  const v = await call('peers_view');
  $('peers-count').textContent = t('peers.count', { c: v.connected.length, r: v.relayed.length, d: v.discovered });

  // Row builders, not rows: only the visible slice is turned into DOM.
  const rows = [];
  for (const p of v.connected) {
    rows.push(() => {
      const tr = el('tr');
      tr.appendChild(el('td', 'mono', p.addr));
      tr.appendChild(el('td', p.connected ? 'ok' : 'bad', t(p.connected ? 'peers.connected' : 'peers.disconnected')));
      tr.appendChild(peerAiCell(p.ai_ready === undefined ? null : p.ai_ready));
      const who = el('td', 'mono dim', (p.peer ? p.peer + ' · ' : '') + (p.model || '—'));
      const badge = sealBadge(p.seal);
      if (badge) who.appendChild(badge);
      tr.appendChild(who);
      tr.appendChild(el('td', 'dim', fmtAgo(p.last_seen_secs)));
      const rating = el('td', 'dim', p.rating === null || p.rating === undefined
        ? t('peers.noRating')
        : t('peers.rating', { p: p.rating, n: p.rating_votes }));
      rating.title = t('peers.ratingTip');
      tr.appendChild(rating);
      return tr;
    });
  }
  for (const p of v.relayed) {
    rows.push(() => {
      const tr = el('tr');
      tr.appendChild(el('td', 'mono dim', t('peers.viaRelayAddr')));
      const state = el('td', 'ok', t('peers.viaRelay'));
      state.title = t('peers.viaRelayTip');
      tr.appendChild(state);
      tr.appendChild(peerAiCell(p.ai_ready));
      const who = el('td', 'mono dim', p.peer + ' · ' + (p.model || '—'));
      const badge = sealBadge(p.seal);
      if (badge) who.appendChild(badge);
      tr.appendChild(who);
      tr.appendChild(el('td', 'dim', fmtAgo(p.last_seen_secs)));
      tr.appendChild(el('td', '', ''));
      return tr;
    });
  }
  const pages = Math.max(1, Math.ceil(rows.length / PEERS_PER_PAGE));
  // Peers come and go under the poll, so clamp instead of trusting the
  // cursor — otherwise a shrinking list strands the user on a blank page.
  if (peersPage > pages - 1) peersPage = pages - 1;
  if (peersPage < 0) peersPage = 0;
  const start = peersPage * PEERS_PER_PAGE;

  const body = $('peers-body');
  body.innerHTML = '';
  for (const build of rows.slice(start, start + PEERS_PER_PAGE)) body.appendChild(build());
  if (!rows.length) {
    const tr = el('tr');
    const td = el('td', 'hint', t('peers.none'));
    td.colSpan = 7;
    tr.appendChild(td);
    body.appendChild(tr);
  }

  const pg = ensurePeersPager();
  pg.hint.textContent = v.caps_known_total
    ? t('peers.ai.summary', { r: v.ai_ready_total, k: v.caps_known_total })
    : t('peers.ai.summaryNone');
  pg.row.style.display = pages > 1 ? '' : 'none';
  pg.label.textContent = t('peers.page', { n: peersPage + 1, total: pages });
  pg.prev.disabled = peersPage === 0;
  pg.next.disabled = peersPage >= pages - 1;
}
{
  const card = $('page-peers').querySelector('.card');
  const row = el('div', 'row');
  const inp = el('input', 'grow mono');
  inp.placeholder = t('peers.addrPh');
  const b = el('button', 'btn', t('peers.connect'));
  b.onclick = () => {
    const addr = inp.value.trim();
    if (!addr) return;
    call('peer_connect', { addr }).then(() => toast(t('peers.dialing', { addr })));
    inp.value = '';
  };
  inp.addEventListener('keydown', (e) => { if (e.key === 'Enter') b.click(); });
  row.appendChild(inp); row.appendChild(b);
  card.insertBefore(row, card.children[1]);
}
loaders.peers = loadPeers;
setInterval(() => { if (activePage === 'peers') loadPeers(); }, 2000);

// ============================================================
// AI SETTINGS
// ============================================================
// The shell (catalog browser, manual-config fields, prompt editor) is built
// once and left alone afterward — only `sync` and the installed-models list
// are safe to rebuild on every poll tick. Rebuilding the whole tab every 4s
// used to blow away in-progress typing (search query, prompt textarea) and
// raced the catalog browser's own re-renders, which is what made results
// look like they were piling up after a second search.
let aiShellBuilt = false;
const aiEls = {};

function buildAiShell(v) {
  const root = $('ai-cards');
  root.innerHTML = '';

  aiEls.sync = el('div', 'card');
  root.appendChild(aiEls.sync);

  aiEls.installedCard = el('div', 'card');
  root.appendChild(aiEls.installedCard);

  aiEls.catalogCard = el('div', 'card');
  aiEls.rerenderCatalog = () => renderCatalog(aiEls.catalogCard, aiEls.rerenderCatalog);
  aiEls.rerenderCatalog();
  root.appendChild(aiEls.catalogCard);

  buildCfgCard(v);
  root.appendChild(aiEls.cfg);

  buildSpCard(v);
  root.appendChild(aiEls.sp);

  aiShellBuilt = true;
}

function renderSyncCard(v) {
  const sync = aiEls.sync;
  sync.innerHTML = '';
  // One download slot exists node-wide, so the catalog's buttons have to know
  // about the automatic first-run install too — otherwise picking a model
  // while that runs just produced a rejection toast.
  dlBusy = !!v.download;
  sync.appendChild(el('h3', '', t('ai.sync')));
  if (v.download_error && !v.download) {
    sync.appendChild(el('div', 'warn', t('ai.dlFailed', { detail: v.download_error })));
    // The catalog waits for the file to appear among the installed ones to
    // know a download finished. A failed one never appears, so its button
    // said "Downloading..." for as long as the window stayed open.
    catDownloadingFile = null;
  }
  if (v.download) {
    const [done, total] = v.download;
    const pct = total > 0 ? Math.floor(done * 100 / total) : 0;
    sync.appendChild(el('div', 'warn', t('ai.downloading', {
      done: Math.floor(done / 1048576), total: Math.floor(total / 1048576), pct,
    })));
    const bar = el('div', 'progress'); bar.appendChild(el('div'));
    bar.firstChild.style.width = pct + '%';
    sync.appendChild(bar);
  } else if (v.model_path) {
    sync.appendChild(el('div', 'ok', t('ai.ready', { path: v.model_path })));
  } else {
    sync.appendChild(el('div', 'hint', t('ai.notDownloaded')));
  }
  if (v.peers_with_model.length && !v.model_path && !v.download) {
    sync.appendChild(el('div', 'hint', t('ai.peersWithModel', { n: v.peers_with_model.length })));
    for (const [addr, size] of v.peers_with_model) {
      const row = el('div', 'row');
      row.appendChild(el('span', 'mono dim', addr + ' (' + Math.floor(size / 1048576) + ' MB)'));
      const b = el('button', 'btn ghost sm', t('ai.downloadFromPeer'));
      b.onclick = () => call('ai_download_from_peer', { addr }).then(() => toast(t('ai.transferRequested')));
      row.appendChild(b);
      sync.appendChild(row);
    }
  }
}

function buildCfgCard(v) {
  const cfg = aiEls.cfg = el('div', 'card');
  cfg.appendChild(el('h3', '', t('ai.engine')));
  aiEls.cfgStatus = el('div', v.engine_status === 'ready' ? 'ok' : 'hint', t('ai.status', { s: v.engine_status }));
  cfg.appendChild(aiEls.cfgStatus);
  const mkField = (label, val, ph) => {
    const row = el('div', 'row');
    row.appendChild(el('span', 'hint', label));
    const inp = el('input', 'grow');
    inp.value = val; if (ph) inp.placeholder = ph;
    row.appendChild(inp);
    cfg.appendChild(row);
    return inp;
  };
  const fModel = mkField(t('ai.modelName'), v.model_name);
  const fUrl = mkField(t('ai.backendUrl'), v.backend_url);
  const fDl = mkField(t('ai.downloadUrl'), v.download_url, 'https://huggingface.co/Qwen/…');
  const applyRow = el('div', 'row');
  const applyBtn = el('button', 'btn', t('ai.apply'));
  applyBtn.onclick = async () => {
    // Apply now waits for the engine to come up, so a failure is reported
    // instead of leaving the previous model answering under a new name.
    applyBtn.disabled = true;
    applyBtn.textContent = t('cat.switching');
    try {
      await call('ai_apply', { modelName: fModel.value, backendUrl: fUrl.value, downloadUrl: fDl.value });
      toast(t('ai.applied'));
    } catch {
      // call() has already shown the reason.
    } finally {
      applyBtn.disabled = false;
      applyBtn.textContent = t('ai.apply');
    }
  };
  applyRow.appendChild(applyBtn);
  const redetect = el('button', 'btn ghost', t('ai.redetect'));
  redetect.onclick = () => call('ai_redetect').then(() => { toast(t('ai.redetected')); loadAi(true); });
  if (!v.hw_available) { redetect.disabled = true; redetect.title = t('err.hwNeedsCpu'); }
  applyRow.appendChild(redetect);
  cfg.appendChild(applyRow);
  // The sizes inference runs with, editable. Automatic derives them from the
  // loaded model; a value entered here overrides that for every model until
  // it is cleared.
  cfg.appendChild(el('h3', '', t('ai.sizes')));
  cfg.appendChild(el('div', 'hint', t(v.tuning_manual ? 'ai.sizesManual' : 'ai.sizesAuto')));
  if (v.tuning) {
    const fCtx = mkField(t('ai.nCtx'), v.tuning.n_ctx);
    const fUb = mkField(t('ai.nUbatch'), v.tuning.n_ubatch);
    const fMax = mkField(t('ai.maxNew'), v.tuning.max_new_tokens);
    if (v.tuning.estimated_ram_mb > 0) {
      cfg.appendChild(el('div', 'dim', t('ai.sizesEstimate', { ram: v.tuning.estimated_ram_mb })));
    }
    const row = el('div', 'row');
    // Applying reloads the engine, which takes as long as loading a model,
    // so the buttons say they are busy rather than looking ignored.
    const run = async (btn, args, doneKey) => {
      const was = btn.textContent;
      row.querySelectorAll('button').forEach((b) => { b.disabled = true; });
      btn.textContent = t('cat.switching');
      try {
        await call('ai_set_tuning', args);
        toast(t(doneKey));
        loadAi(true);
      } catch (_) {
        // call() has already shown the reason.
      } finally {
        btn.textContent = was;
        row.querySelectorAll('button').forEach((b) => { b.disabled = false; });
      }
    };
    const apply = el('button', 'btn', t('ai.sizesApply'));
    apply.onclick = () => run(apply, {
      nCtx: parseInt(fCtx.value, 10),
      nUbatch: parseInt(fUb.value, 10),
      maxNewTokens: parseInt(fMax.value, 10),
    }, 'ai.sizesApplied');
    row.appendChild(apply);
    if (v.tuning_manual) {
      const auto = el('button', 'btn ghost', t('ai.sizesAutoBtn'));
      auto.onclick = () => run(auto, { nCtx: null, nUbatch: null, maxNewTokens: null }, 'ai.sizesAutoDone');
      row.appendChild(auto);
    }
    cfg.appendChild(row);
  } else {
    cfg.appendChild(el('div', 'warn', t('ai.tuningMissing')));
  }
}

function buildSpCard(v) {
  const sp = aiEls.sp = el('div', 'card');
  sp.appendChild(el('h3', '', t('ai.prompt')));
  aiEls.spMesh = el('div', 'hint');
  sp.appendChild(aiEls.spMesh);
  updateSpMesh(v);
  const ta = el('textarea');
  ta.rows = 4; ta.value = v.local_prompt;
  ta.placeholder = t(v.prompt_locked ? 'lk.promptLocked' : 'ai.promptPh');
  ta.disabled = v.prompt_locked;
  sp.appendChild(ta);
  const think = el('label', 'row');
  const cb = el('input'); cb.type = 'checkbox'; cb.checked = v.thinking_enabled;
  think.appendChild(cb);
  think.appendChild(el('span', 'hint', '💭 ' + t('ai.thinking')));
  sp.appendChild(think);
  const spRow = el('div', 'row');
  const save = el('button', 'btn', t('ai.save'));
  save.onclick = () => call('ai_save_prompt', { prompt: ta.value, thinkingEnabled: cb.checked }).then(() => toast(t('ai.saved')));
  const clear = el('button', 'btn ghost', t('ai.clear'));
  clear.onclick = () => { ta.value = ''; call('ai_save_prompt', { prompt: '', thinkingEnabled: cb.checked }).then(() => toast(t('ai.cleared'))); };
  save.disabled = clear.disabled = v.prompt_locked;
  spRow.appendChild(save); spRow.appendChild(clear);
  sp.appendChild(spRow);
}

function updateSpMesh(v) {
  if (false) {
    const preview = v.network_prompt.length > 80 ? v.network_prompt.slice(0, 80) + '…' : v.network_prompt;
    aiEls.spMesh.textContent = t('ai.meshPrompt', { p: preview });
  } else {
    aiEls.spMesh.textContent = t('ai.meshPromptNone');
  }
}

// `full=true` (tab switch, or right after an action that changes engine/
// tuning/model state) rebuilds everything. Poll ticks pass `full=false` and
// only touch the sync card + installed list, leaving search state, the
// manual-config inputs and the prompt textarea untouched.
async function loadAi(full) {
  const v = await call('ai_view');
  if (full || !aiShellBuilt) {
    buildAiShell(v);
  } else {
    aiEls.cfgStatus.className = v.engine_status === 'ready' ? 'ok' : 'hint';
    aiEls.cfgStatus.textContent = t('ai.status', { s: v.engine_status });
    updateSpMesh(v);
  }
  renderSyncCard(v);
  renderInstalled(aiEls.installedCard, () => loadAi(true));
}
// ---------- model catalog (shared by AI Settings and the wizard) ----------
let catRepos = null;      // cached repo list, null while loading/unsearched
let catOpenRepo = null;   // repo whose files are expanded
let catFiles = null;
let catUntrusted = false;
let catBusy = false;
let catQuery = '';        // typed search text, kept across re-renders
let catFilesBusy = false;
let catSearchGen = 0;     // invalidates a stale in-flight search
let catInstalledFiles = new Set(); // file names already on disk, kept fresh by renderInstalled's poll
let catDownloadingFile = null;      // file name this session just started downloading, if any
let dlBusy = false;                 // any download running, including the automatic first-run one

// Clears the previous result set immediately (so a second search never
// shows old + new results together) and tags each request with a
// generation counter so a slow response can't clobber a faster, newer one.
async function catLoadRepos(query) {
  const myGen = ++catSearchGen;
  catBusy = true;
  catRepos = null;
  try {
    const res = await call('catalog_search', { query: query || '', includeUntrusted: catUntrusted });
    if (myGen === catSearchGen) catRepos = res;
  } catch (_) {
    if (myGen === catSearchGen) catRepos = [];
  }
  if (myGen === catSearchGen) catBusy = false;
}

async function catOpen(repo) {
  catOpenRepo = repo;
  catFiles = null;
  catFilesBusy = true;
  try {
    catFiles = await call('catalog_files', { repo });
    const installed = await invoke('models_installed').catch(() => []);
    catInstalledFiles = new Set(installed.map((m) => m.file_name));
  } catch (_) { catFiles = []; }
  catFilesBusy = false;
}

function spinner() {
  return el('span', 'spinner');
}

/// Renders the browser into `root`. `onChange` re-renders the host view.
function renderCatalog(root, onChange) {
  root.innerHTML = '';
  root.appendChild(el('h3', '', t('cat.title')));
  root.appendChild(el('div', 'hint', t('cat.sub')));

  if (catOpenRepo) {
    const back = el('button', 'btn ghost sm', t('cat.back'));
    back.onclick = () => { catOpenRepo = null; catFiles = null; onChange(); };
    root.appendChild(back);
    root.appendChild(el('div', 'row', catOpenRepo));
    if (catFilesBusy || !catFiles) {
      const row = el('div', 'row'); row.appendChild(spinner());
      row.appendChild(el('span', 'hint', t('cat.loadingFiles')));
      root.appendChild(row);
      return;
    }
    if (!catFiles.length) { root.appendChild(el('div', 'warn', t('cat.noFiles'))); return; }
    for (const f of catFiles) {
      const card = el('div', 'tier fade-in');
      const head = el('div', 'row');
      head.appendChild(el('b', '', (f.quant || f.local_name) + ' · ' + f.size_mb + ' MB'));
      // Whether the engine can read the file at all outranks whether it
      // would fit in memory: a size verdict on a format llama.cpp cannot
      // parse is a promise about a model that will never run.
      const unreadable = f.runnable === 'no';
      if (unreadable) {
        head.appendChild(el('span', 'bad', t('cat.cannotRun')));
      } else {
        const cls = f.fit === 'comfortable' ? 'ok' : f.fit === 'tight' ? 'warn' : 'bad';
        head.appendChild(el('span', cls, t('cat.fit.' + f.fit)));
      }
      card.appendChild(head);
      // Two files in different directories of one repository can carry the
      // same quantisation and size, and listed by quantisation alone they
      // were indistinguishable. The path is shown when there is one.
      if (f.file !== f.local_name) {
        card.appendChild(el('div', 'dim', f.file));
      }
      if (unreadable) {
        card.appendChild(el('div', 'hint',
          t('cat.cannotRunWhy', { ids: (f.unsupported_types || []).join(', ') })));
      } else {
        card.appendChild(el('div', 'hint',
          t('cat.fitDetail', { needed: f.needed_mb, available: f.available_mb }) + ' · ' +
          t(f.uses_gpu ? 'cat.speedGpu' : 'cat.speed', { n: f.est_secs })));
        if (f.runnable === 'unknown') {
          card.appendChild(el('div', 'hint', t('cat.runUnknown') + ' — ' + t('cat.runUnknownWhy')));
        }
      }
      if (catInstalledFiles.has(f.local_name)) {
        card.appendChild(el('span', 'ok', t('cat.alreadyInstalled')));
      } else if (f.fit !== 'toobig' && !unreadable) {
        const downloading = catDownloadingFile === f.local_name;
        const blocked = dlBusy || (catDownloadingFile !== null && !downloading);
        const dl = el('button', 'btn sm',
          downloading ? t('cat.downloadPending') : blocked ? t('cat.downloadBusy') : t('cat.download'));
        dl.disabled = downloading || blocked;
        dl.onclick = async () => {
          dl.disabled = true;
          dl.textContent = t('cat.downloadPending');
          try {
            await call('catalog_download', { url: f.url, fileName: f.file, sha256: f.sha256 });
            catDownloadingFile = f.local_name;
            toast(t('cat.downloading', { file: f.local_name }));
          } catch (_) {
            // call() already toasted the reason (already installed / busy) —
            // just fall through to restore the button via onChange().
          }
          onChange();
        };
        card.appendChild(dl);
      }
      root.appendChild(card);
    }
    return;
  }

  const searchRow = el('div', 'row');
  const q = el('input', 'grow');
  q.placeholder = t('cat.searchPh');
  q.value = catQuery;
  q.oninput = () => { catQuery = q.value; };
  const runSearch = () => {
    catQuery = q.value;
    const p = catLoadRepos(catQuery);
    onChange();       // catBusy/catRepos flip synchronously above — show the spinner now
    p.then(onChange);  // then paint the real results (or lack thereof)
  };
  const go = el('button', 'btn', t('cat.search'));
  go.onclick = runSearch;
  q.addEventListener('keydown', (e) => { if (e.key === 'Enter') runSearch(); });
  searchRow.appendChild(q); searchRow.appendChild(go);
  root.appendChild(searchRow);

  const unt = el('label', 'row');
  const cb = el('input'); cb.type = 'checkbox'; cb.checked = catUntrusted;
  cb.onchange = () => { catUntrusted = cb.checked; runSearch(); };
  unt.appendChild(cb);
  unt.appendChild(el('span', 'hint', t('cat.untrusted')));
  root.appendChild(unt);

  // First view: list the trusted publishers' models without waiting for a
  // search, so the picker is never an empty "searching" spinner.
  if (catRepos === null && !catBusy) catLoadRepos(catQuery).then(onChange);
  if (catBusy || catRepos === null) {
    const row = el('div', 'row'); row.appendChild(spinner());
    row.appendChild(el('span', 'hint', t('cat.searching')));
    root.appendChild(row);
    return;
  }
  if (!catRepos.length) { root.appendChild(el('div', 'hint', t('cat.noResults'))); return; }
  for (const r of catRepos) {
    const card = el('div', 'tier fade-in');
    const head = el('div', 'row');
    head.appendChild(el('b', '', r.id));
    if (!r.trusted) head.appendChild(el('span', 'warn', '⚠'));
    card.appendChild(head);
    const bits = [t('cat.downloads', { n: r.downloads.toLocaleString() })];
    if (r.params_b) bits.push(t('cat.params', { n: r.params_b }));
    card.appendChild(el('div', 'hint', bits.join(' · ')));
    if (!r.trusted) card.appendChild(el('div', 'hint warn', t('cat.untrustedWarn')));
    card.onclick = () => {
      const p = catOpen(r.id);
      onChange();       // catOpenRepo/catFilesBusy flip synchronously above
      p.then(onChange);
    };
    root.appendChild(card);
  }
}

function renderInstalled(root, onChange) {
  root.innerHTML = '';
  root.appendChild(el('h3', '', t('cat.installed')));
  invoke('models_installed').then((list) => {
    catInstalledFiles = new Set(list.map((m) => m.file_name));
    if (catDownloadingFile && catInstalledFiles.has(catDownloadingFile)) {
      catDownloadingFile = null;
      // File-list view has no text input to disturb, so it's safe to
      // refresh it here to flip the button to the Installed badge.
      if (catOpenRepo && aiEls.rerenderCatalog) aiEls.rerenderCatalog();
    }
    if (!list.length) { root.appendChild(el('div', 'hint', t('cat.noInstalled'))); return; }
    for (const m of list) {
      const row = el('div', 'sess');
      row.appendChild(el('span', m.active ? 'ch' : '', (m.active ? '● ' : '○ ') + m.file_name));
      row.appendChild(el('span', 'dim', m.size_mb + ' MB'));
      const right = el('span', 'right');
      if (m.active) {
        right.appendChild(el('span', 'ok', t('cat.inUse')));
      } else {
        const use = el('button', 'btn ghost sm', t('cat.use'));
        use.onclick = async () => {
          // The switch now waits for the model to actually load, which takes
          // seconds for a file of this size. Without a busy state the button
          // looks like it did nothing, and a failure is only reported at the
          // end — so the row has to show that something is under way.
          use.disabled = true;
          use.textContent = t('cat.switching');
          try {
            await call('model_switch', { fileName: m.file_name });
            toast(t('cat.switched'));
            onChange();
          } catch {
            // call() has already shown the reason; restore the button so the
            // model that is loaded stays selectable.
            use.disabled = false;
            use.textContent = t('cat.use');
          }
        };
        right.appendChild(use);
      }
      const del = el('button', 'btn danger sm', t('cat.delete'));
      del.onclick = async () => {
        if (!(await confirmDlg(t('cat.confirmDelete', { file: m.file_name })))) return;
        await call('model_delete', { fileName: m.file_name });
        toast(t('cat.deleted'));
        onChange();
      };
      right.appendChild(del);
      row.appendChild(right);
      root.appendChild(row);
    }
  }).catch(() => {});
}

loaders.ai = () => loadAi(true);
setInterval(() => { if (activePage === 'ai') loadAi(false); }, 4000);

// ============================================================
// NETWORK
// ============================================================
async function loadNetwork() {
  const v = await call('network_view');
  const root = $('net-cards');
  root.innerHTML = '';

  const info = el('div', 'card');
  info.appendChild(el('h3', '', t('net.node')));
  const rows = [
    [t('net.network'), v.network], [t('net.addr'), v.local_addr],
    [t('net.transport'), t('net.transportVal')],
    [t('net.security'), t('net.securityVal')],
    [t('net.uptime'), v.uptime], [t('net.peers'), String(v.active_peers)],
    [t('net.custody'), String(v.custody)], [t('net.pending'), String(v.pending_queries)],
    [t('net.engine'), v.ai_engine],
  ];
  const tbl = el('table'); const tb = el('tbody');
  for (const [k, val] of rows) {
    const tr = el('tr');
    tr.appendChild(el('td', 'hint', k));
    tr.appendChild(el('td', 'mono', val));
    tb.appendChild(tr);
  }
  tbl.appendChild(tb); info.appendChild(tbl);
  root.appendChild(info);

  const boot = el('div', 'card');
  boot.appendChild(el('h3', '', t('net.bootstrap')));
  if (!v.bootstrap.length) boot.appendChild(el('div', 'hint', t('net.bootstrapNone')));
  for (const b of v.bootstrap) boot.appendChild(el('div', 'mono dim', b));
  const brow = el('div', 'row');
  const binp = el('input', 'grow mono'); binp.placeholder = t('peers.addrPh');
  const badd = el('button', 'btn ghost sm', t('net.add'));
  badd.onclick = () => { if (binp.value.trim()) { call('bootstrap_add', { addr: binp.value.trim() }); binp.value = ''; setTimeout(loadNetwork, 500); } };
  brow.appendChild(binp); brow.appendChild(badd);
  boot.appendChild(brow);
  root.appendChild(boot);

  const netCard = el('div', 'card');
  netCard.appendChild(el('h3', '', t('net.switchTitle')));
  netCard.appendChild(el('div', 'hint', t('net.switchHint', { current: v.network })));
  const nrow = el('div', 'row');
  for (const name of ['testnet', 'mainnet']) {
    const nb = el('button', 'btn ghost sm' + (v.network === name ? ' active' : ''), name);
    nb.disabled = v.network === name;
    nb.onclick = async () => {
      const msg = await call('network_set', { name });
      if (msg) toast(msg);
    };
    nrow.appendChild(nb);
  }
  netCard.appendChild(nrow);
  root.appendChild(netCard);

  const log = el('div', 'card');
  log.appendChild(el('h3', '', t('net.log')));
  if (!v.log.length) log.appendChild(el('div', 'hint', t('net.logEmpty')));
  for (const line of v.log) log.appendChild(el('div', 'hint mono', line));
  root.appendChild(log);
}
loaders.network = loadNetwork;
setInterval(() => { if (activePage === 'network') loadNetwork(); }, 2500);

// ============================================================
// LEADERBOARD
// ============================================================
async function loadLeaderboard() {
  const v = await call('leaderboard_view');
  const body = $('lb-body');
  body.innerHTML = '';
  const healthCls = (h) => h >= 1 ? 'ok' : h >= 0.5 ? 'warn' : 'bad';
  // A node with no ratings sits at the 50% prior, which is not the same as
  // "rated 50%" - say so rather than showing a number nobody voted for.
  const rated = (r) => (r.likes + r.dislikes) > 0;
  const qualityCls = (r) => !rated(r) ? '' : r.quality >= 60 ? 'ok' : r.quality >= 25 ? 'warn' : 'bad';
  // Percentage only: the raw counts run into four and five digits on a real
  // network and there is no width for them. They go in the title instead.
  const qualityText = (r) => rated(r) ? r.quality + '%' : t('lb.quality.none');
  const pend = (val, p) => p > 0 ? t('lb.pending', { v: val, p }) : String(val);
  const my = v.my;
  const meRow = el('tr');
  meRow.style.fontWeight = '600';
  const meWho = el('td', 'ok', t('lb.me') + ' — ' + t('lb.band.' + my.band));
  const meBadge = sealBadge(my.seal);
  if (meBadge) meWho.appendChild(meBadge);
  meRow.appendChild(meWho);
  meRow.appendChild(el('td', '', ''));
  meRow.appendChild(el('td', '', pend(my.aporte, my.aporte_pending)));
  meRow.appendChild(el('td', '', pend(my.consumo, my.consumo_pending)));
  meRow.appendChild(el('td', '', pend(my.wins, my.wins_pending)));
  meRow.appendChild(el('td', '', ''));
  meRow.appendChild(el('td', healthCls(my.health), my.health.toFixed(2) + '×'));
  body.appendChild(meRow);
  for (const r of v.rows) {
    const tr = el('tr');
    if (r.is_self) tr.style.opacity = '0.55';
    const who = el('td', 'mono' + (r.is_self ? ' ok' : ''),
      r.peer + (r.is_self ? ' (' + t('lb.me') + ')' : '') + ' · ' + t('lb.band.' + r.band));
    const badge = sealBadge(r.seal);
    if (badge) who.appendChild(badge);
    tr.appendChild(who);
    tr.appendChild(el('td', '', (r.has_ai ? '✓ ' : '— ') + r.model));
    tr.appendChild(el('td', '', String(r.aporte)));
    tr.appendChild(el('td', '', ''));
    tr.appendChild(el('td', '', String(r.wins)));
    const qtd = el('td', qualityCls(r), qualityText(r));
    if (rated(r)) qtd.title = t('lb.quality.votes', { l: r.likes, d: r.dislikes });
    tr.appendChild(qtd);
    tr.appendChild(el('td', healthCls(r.health), r.health.toFixed(2) + '× · ' + t('lb.up', { t: fmtUp(r.uptime_secs) })));
    body.appendChild(tr);
  }
  const bands = el('tr');
  const td = el('td', 'hint', t('lb.bands', { p50: v.bands[0], p90: v.bands[1], p99: v.bands[2] }));
  td.colSpan = 7;
  bands.appendChild(td);
  body.appendChild(bands);
}
loaders.leaderboard = loadLeaderboard;
setInterval(() => { if (activePage === 'leaderboard') loadLeaderboard(); }, 3000);

// ============================================================
// WALLET
// ============================================================
let walletHistory = [];
let walletFilter = 'all';
let priorityCost = 10;

async function loadWallet() {
  const w = await call('wallet_view');
  priorityCost = w.priority_cost;
  $('w-balance').innerHTML = w.balance + ' <span>' + t('w.credits') + ' · ' + w.peer_id.slice(0, 8) + '…' + w.peer_id.slice(-6) + '</span>';
  $('w-addr').textContent = w.peer_id;
  $('w-addr').title = t('common.copy');
  $('w-addr').onclick = () => copy(w.peer_id, t('lk.wallet'));
  $('w-node').textContent = w.node_id.slice(0, 12) + '…';
  $('w-deleg').textContent = '· ' + t('lk.deleg.' + w.delegation);
  $('w-lock-state').textContent = UNLOCKED ? t('lk.unlocked') : t('lk.locked');
  $('w-lock-state').className = UNLOCKED ? 'ok' : 'warn';
  $('w-lock-btn').textContent = UNLOCKED ? t('lk.lock') : t('lk.unlock');
  invoke('lock_state').then((l) => {
    const sel = $('w-autolock');
    if (document.activeElement !== sel) sel.value = String(l.auto_lock_minutes);
  }).catch(() => {});
  $('w-open').textContent = t('w.open', { cost: w.priority_cost });
  const grid = $('w-redeem-grid');
  grid.innerHTML = '';
  const line = (label, val, extra, cls) => {
    const d = el('div', 'row');
    d.appendChild(el('span', 'hint grow', label));
    d.appendChild(el('span', cls || '', val));
    if (extra) d.appendChild(el('span', 'dim', extra));
    grid.appendChild(d);
  };
  const pend = (v, p) => p > 0 ? t('lb.pending', { v, p }) : String(v);
  line(t('w.contributed'), pend(w.aporte, w.aporte_pending), t('w.contributedNote', { n: w.shares_avail }), 'ok');
  line(t('w.consumed'), pend(w.consumo, w.consumo_pending), t('w.consumedNote'), 'warn');
  line(t('w.wins'), pend(w.wins, w.wins_pending), t('w.winsNote', { n: w.lotteries_avail }), '');
  $('w-redeemable').textContent = t('w.redeemable', {
    units: w.shares_avail + w.lotteries_avail, rate: w.rate, credits: w.redeemable_credits,
  });

  const box = $('w-sessions');
  box.innerHTML = '';
  if (!w.sessions.length) box.appendChild(el('div', 'hint', t('w.noSessions')));
  for (const s of w.sessions) {
    const item = el('div', 'sess');
    item.appendChild(el('span', 'ch', '● ' + (s.channel || t('w.universal'))));
    const tok = el('span', 'tok', s.token.slice(0, 8) + '…' + s.token.slice(-6));
    tok.title = t('w.copyToken');
    tok.style.cursor = 'pointer';
    tok.onclick = () => copy(s.token, 'Token');
    item.appendChild(tok);
    const ttlCls = s.expires_in_min < 5 ? 'bad' : s.expires_in_min < 30 ? 'warn' : 'ok';
    const right = el('span', 'right', t('w.infs', { n: s.inferences_remaining }) + ' · ');
    right.appendChild(el('span', ttlCls, t('w.min', { n: s.expires_in_min })));
    item.appendChild(right);
    box.appendChild(item);
  }

  const b = $('w-ltc-body');
  b.innerHTML = '';
  if (w.direct_sale_wallet) {
    // Selling never stops. When the validator cannot reach its payment
    // source the buyer is told to expect a delay, and why - payments made
    // meanwhile stay on the Litecoin chain and are credited once it clears.
    if (!w.sale_healthy) {
      const delay = el('div', 'callout warn');
      delay.appendChild(el('div', '', t('w.salesDelayed')));
      if (w.sale_health_reason) delay.appendChild(el('div', 'hint mono', w.sale_health_reason));
      b.appendChild(delay);
    }
    const rate = el('div', '', t('w.ltcRate', { n: w.credits_per_ltc }));
    rate.title = t('w.tip.rate');
    b.appendChild(rate);
    const addr = el('div', 'row');
    addr.appendChild(el('span', 'hint', t('w.ltcSend')));
    const a = el('span', 'mono', w.direct_sale_wallet);
    a.style.cursor = 'pointer'; a.title = t('common.copy');
    a.onclick = () => copy(w.direct_sale_wallet, 'LTC');
    addr.appendChild(a);
    b.appendChild(addr);
    // Said before the address is copied, not after the money is sent.
    b.appendChild(el('div', 'callout warn', t('ltc.private')));
    const opHint = el('div', 'hint', t('w.ltcOp'));
    opHint.title = t('w.tip.opReturn');
    b.appendChild(opHint);
    const o = el('div', 'mono', w.peer_id);
    o.style.cursor = 'pointer'; o.title = t('common.copy');
    o.onclick = () => copy(w.peer_id, 'OP_RETURN');
    b.appendChild(o);
    b.appendChild(el('div', 'hint', t('w.ltcConfirm')));
  } else {
    b.textContent = t('w.ltcPaused');
  }

  walletHistory = w.history;
  renderHistory();
}

function renderHistory() {
  const body = $('w-history');
  body.innerHTML = '';
  const rows = walletHistory.filter(h => walletFilter === 'all' || h.filter === walletFilter);
  if (!rows.length) {
    const tr = el('tr');
    const td = el('td', 'hint', walletHistory.length ? t('w.noMatching') : t('w.noActivity'));
    td.colSpan = 3;
    tr.appendChild(td);
    body.appendChild(tr);
    return;
  }
  for (const h of rows) {
    const tr = el('tr');
    tr.appendChild(el('td', 'dim', '#' + h.height));
    const kind = el('td', 'mono', h.kind + (h.credit ? ' ' + h.credit : ''));
    kind.style.color = h.color;
    tr.appendChild(kind);
    tr.appendChild(el('td', 'mono dim', h.detail));
    body.appendChild(tr);
  }
}

document.querySelectorAll('#w-filters .fchip').forEach(c => c.onclick = () => {
  document.querySelectorAll('#w-filters .fchip').forEach(x => x.classList.remove('active'));
  c.classList.add('active');
  walletFilter = c.dataset.f;
  renderHistory();
});

$('w-redeem').onclick = async () => {
  $('w-redeem-msg').textContent = t('w.redeeming');
  await call('redeem', {});
  toast(t('w.redeemed'));
  setTimeout(() => { loadWallet(); $('w-redeem-msg').textContent = ''; }, 3000);
};
$('w-redeem-custom').onclick = async () => {
  const sh = parseInt($('w-red-sh').value || '0', 10);
  const lot = parseInt($('w-red-lot').value || '0', 10);
  if (sh <= 0 && lot <= 0) return;
  await call('redeem', { shares: sh, lotteries: lot });
  toast(t('w.redeemed'));
  setTimeout(loadWallet, 3000);
};
$('t-send').onclick = async () => {
  const to = $('t-to').value.trim();
  const amount = parseInt($('t-amount').value || '0', 10);
  if (to.length !== 64) { $('t-msg').textContent = t('err.recipient'); return; }
  if (amount <= 0) { $('t-msg').textContent = t('err.amount'); return; }
  $('t-msg').textContent = t('w.transferring');
  try {
    await call('transfer', { to, credits: amount });
    $('t-msg').textContent = t('w.transferred');
    $('t-to').value = ''; $('t-amount').value = '';
    setTimeout(loadWallet, 3000);
  } catch (_) { $('t-msg').textContent = ''; }
};
$('w-open').onclick = async () => {
  $('w-open').disabled = true;
  $('w-msg').textContent = t('w.opening');
  try {
    await call('open_session', { channel: null });
    $('w-msg').textContent = t('w.opened');
    loadWallet();
  } catch (_) { $('w-msg').textContent = ''; }
  $('w-open').disabled = false;
};
loaders.wallet = loadWallet;

$('w-lock-btn').onclick = async () => { await toggleLock(); loadWallet(); };
for (const o of $('w-autolock').options) {
  o.textContent = o.value === '0' ? t('lk.never') : t('lk.min', { n: o.value });
}
$('w-autolock').onchange = async () => {
  await call('set_auto_lock', { minutes: parseInt($('w-autolock').value, 10) });
  toast(t('lk.autoSaved'));
};

// The vault's password: changed here, never stored, needed to open the app.
$('sec-go').onclick = async () => {
  const old = $('sec-old').value, nw = $('sec-new').value;
  if (nw !== $('sec-new2').value) { $('sec-msg').textContent = t('v.err.match'); return; }
  if (nw.length < 8) { $('sec-msg').textContent = t('v.err.weak'); return; }
  $('sec-go').disabled = true;
  $('sec-msg').textContent = t('v.sealing');
  try {
    await invoke('vault_change_password', { old, new: nw });
    $('sec-msg').textContent = t('sec.changed');
    $('sec-old').value = ''; $('sec-new').value = ''; $('sec-new2').value = '';
  } catch (e) {
    $('sec-msg').textContent = /wrong password/i.test(String(e)) ? t('v.err.wrong') : String(e);
  }
  $('sec-go').disabled = false;
};
setInterval(() => { if (activePage === 'wallet') loadWallet(); }, 4000);

// ============================================================
// MARKET — the NAI notice board, laid out as a book.
//
// Both sides face each other so the spread reads at a glance instead of down
// a scroll. Asks are covered: posting one moves the credits into chain escrow,
// so a listing can never advertise credits the seller has already spent. Bids
// are advertisements only — the chain cannot hold Litecoin, so it does not
// pretend to. Settlement releases an ask's escrow to the buyer once the
// off-chain payment is observed; the network never touches the Litecoin.
// ============================================================
function bookRow(r, isAsk) {
  const tr = el('tr');
  if (r.mine) tr.classList.add('mine');

  const unit = el('td', 'num', String(r.sats_per_credit));
  unit.title = t('m.tip.unit');
  tr.appendChild(unit);

  const credits = el('td', 'num', String(r.credits));
  credits.title = t('m.tip.total', { s: r.sats });
  tr.appendChild(credits);

  // Counterparty and what the chain knows about them, in one cell — every
  // figure read from chain state, never self-reported.
  const who = el('td');
  const name = el('span', 'mono', r.mine ? t('m.you') : r.peer_short);
  if (r.mine) name.classList.add('ok');
  name.title = r.mine ? t('m.tip.you') : r.peer;
  who.appendChild(name);
  const rep = el('div', 'rep', t('m.rep.summary', { t: r.trades, d: r.days_active }));
  rep.title = t('m.tip.reputation', {
    t: r.trades, d: r.days_active, s: r.shares, w: r.wins,
  });
  who.appendChild(rep);
  tr.appendChild(who);

  const act = el('td');
  if (r.mine) {
    const b = el('button', 'btn ghost sm', t('m.cancel'));
    b.onclick = async () => {
      b.disabled = true;
      try {
        await call('market_cancel', { id: r.id, isAsk });
        toast(t('m.cancelled'));
        setTimeout(loadMarket, 3000);
      } catch (_) { b.disabled = false; }
    };
    act.appendChild(b);
  } else if (isAsk && r.ltc_address) {
    if (r.claim_pending) {
      // One claim per listing at a time; the oracle is already looking.
      act.appendChild(el('span', 'lbl', t(r.claim_mine ? 'm.claim.yours' : 'm.claim.other')));
    } else {
      const b = el('button', 'btn ghost sm', t('m.buy'));
      b.title = t('m.tip.buy');
      b.onclick = () => openBuy(r);
      act.appendChild(b);
    }
  }
  tr.appendChild(act);
  return tr;
}

function fillSide(tbody, rows, isAsk, emptyKey) {
  tbody.innerHTML = '';
  if (!rows.length) {
    const tr = el('tr');
    const td = el('td', 'book-empty', t(emptyKey));
    td.colSpan = 4;
    tr.appendChild(td);
    tbody.appendChild(tr);
    return;
  }
  for (const r of rows) tbody.appendChild(bookRow(r, isAsk));
}

function standingChip(labelKey, have, need) {
  const met = have >= need;
  const c = el('span', 'mkt-need ' + (met ? 'met' : 'unmet'),
    t(labelKey, { have, need }));
  return c;
}

// Buying from a listing happens off chain, so the dialog walks the buyer
// through it: pay the seller, attach the code that names this wallet and
// this listing, then hand the chain the payment's id to confirm.
let lastMarket = null;
async function openBuy(r) {
  let memo;
  try { memo = await call('market_fill_memo', { askId: r.id }); } catch (_) { return; }
  $('buy-title').textContent = t('m.buy.title', { n: r.credits, s: r.sats });
  $('buy-step1').textContent = t('m.buy.step1', { s: r.sats });
  $('buy-addr').textContent = r.ltc_address;
  $('buy-memo').textContent = memo;
  $('buy-txid').value = '';
  const sv = lastMarket && lastMarket.standing;
  const blocked = sv && !sv.eligible;
  $('buy-msg').textContent = blocked ? t('m.buy.blocked', { why: sv.reason || '' }) : '';
  $('buy-yes').disabled = !!blocked;
  $('buy-copy-addr').onclick = () => copy(r.ltc_address, 'LTC');
  $('buy-copy-memo').onclick = () => copy(memo, 'OP_RETURN');
  const dlg = $('buy-dlg');
  $('buy-no').onclick = () => dlg.close();
  $('buy-yes').onclick = async () => {
    const txid = $('buy-txid').value.trim();
    if (!txid) { $('buy-msg').textContent = t('m.buy.needTxid'); return; }
    $('buy-yes').disabled = true;
    try {
      await call('market_claim', { askId: r.id, paymentRef: txid });
      dlg.close();
      toast(t('m.buy.claimed'));
      setTimeout(loadMarket, 3000);
    } catch (e) {
      $('buy-msg').textContent = friendlyError(e);
      $('buy-yes').disabled = false;
    }
  };
  dlg.showModal();
}

async function loadMarket() {
  let v;
  try { v = await invoke('market_view'); } catch (_) { return; }
  lastMarket = v;

  document.querySelector('#page-market .mkt-bar').title = t('m.intro');
  document.querySelector('#m-ask-post').closest('.card').querySelector('h3').title = t('m.sellHint');

  $('m-balance').textContent = v.balance;
  $('m-balance').title = t('m.tip.balance');
  $('m-escrow').textContent = v.my_escrow;
  $('m-escrow').title = t('m.tip.escrow');

  // Standing sits in the bar rather than in a block of its own: an account
  // that cannot trade needs to know why, not to lose the book to the notice.
  const st = $('m-standing');
  st.innerHTML = '';
  const sv = v.standing;
  st.title = '';
  if (sv.eligible) {
    const ok = el('span', 'mkt-need met', t('m.standing.ok'));
    ok.title = t('m.tip.standing');
    st.appendChild(ok);
  } else {
    const lead = el('span', 'lbl bad', t('m.standing.no'));
    lead.title = t('m.tip.standing');
    st.appendChild(lead);
    st.title = t('m.standing.why');
    st.appendChild(standingChip('m.standing.days', sv.days_active, sv.days_required));
    st.appendChild(standingChip('m.standing.shares', sv.shares, sv.shares_required));
    st.appendChild(standingChip('m.standing.wins', sv.wins, sv.wins_required));
  }

  fillSide($('m-asks'), v.asks, true, 'm.noAsks');
  fillSide($('m-bids'), v.bids, false, 'm.noBids');

  for (const id of ['m-ask-post', 'm-bid-post']) {
    $(id).disabled = !sv.eligible;
    $(id).title = sv.eligible ? '' : t('m.standing.blocked');
  }
}

$('m-ask-post').onclick = async () => {
  const credits = parseInt($('m-ask-credits').value || '0', 10);
  const sats = parseInt($('m-ask-sats').value || '0', 10);
  const addr = $('m-ask-addr').value.trim();
  if (credits <= 0 || sats <= 0) { $('m-ask-msg').textContent = t('err.amount'); return; }
  if (!addr) { $('m-ask-msg').textContent = t('m.errAddr'); return; }
  const ok = await confirmDlg(t('m.confirmAsk', { n: credits, s: sats }));
  if (!ok) return;
  $('m-ask-msg').textContent = t('m.posting');
  try {
    await call('market_post_ask', { credits, sats, ltcAddress: addr });
    $('m-ask-msg').textContent = t('m.posted');
    $('m-ask-credits').value = ''; $('m-ask-sats').value = '';
    setTimeout(() => { loadMarket(); $('m-ask-msg').textContent = ''; }, 3000);
  } catch (_) { $('m-ask-msg').textContent = ''; }
};

$('m-bid-post').onclick = async () => {
  const credits = parseInt($('m-bid-credits').value || '0', 10);
  const sats = parseInt($('m-bid-sats').value || '0', 10);
  if (credits <= 0 || sats <= 0) { $('m-bid-msg').textContent = t('err.amount'); return; }
  $('m-bid-msg').textContent = t('m.posting');
  try {
    await call('market_post_bid', { credits, sats });
    $('m-bid-msg').textContent = t('m.posted');
    $('m-bid-credits').value = ''; $('m-bid-sats').value = '';
    setTimeout(() => { loadMarket(); $('m-bid-msg').textContent = t('m.buyHint'); }, 3000);
  } catch (_) { $('m-bid-msg').textContent = t('m.buyHint'); }
};

loaders.market = loadMarket;
setInterval(() => { if (activePage === 'market') loadMarket(); }, 5000);

// The last governance view, read by the proposal form for current values.
let lastGov = null;

// Three levers anyone can understand. The chain accepts more kinds (the
// sale oracle, membership); those are operator matters, not everyday votes.
const GOV_FIELDS = {
  emission_target: ['amount'],
  channel_cost: ['channel', 'amount'],
  daily_mint_cap: ['amount'],
};

// Shows what the chosen lever is set to now, so a proposal is a change
// from a known value rather than a number out of nowhere.
function govCurrent(kind) {
  const v = lastGov;
  if (!v) return '';
  if (kind === 'emission_target') return t('g.now.emission', { n: v.emission_target });
  if (kind === 'daily_mint_cap') return t('g.now.cap', { n: v.daily_mint_cap });
  if (kind === 'channel_cost') {
    const ch = $('g-channel').value;
    const row = (v.session_costs || []).find(([c]) => c === ch);
    return t('g.now.cost', { n: row ? row[1] : '–' });
  }
  return '';
}

function govFieldsFor(kind) {
  const f = GOV_FIELDS[kind] || [];
  $('g-channel').style.display = f.includes('channel') ? '' : 'none';
  $('g-amount').style.display = f.includes('amount') ? '' : 'none';
  $('g-now').textContent = govCurrent(kind);
}
$('g-kind').onchange = () => govFieldsFor($('g-kind').value);
$('g-channel').onchange = () => govFieldsFor($('g-kind').value);
govFieldsFor($('g-kind').value);

function govRow(p, v) {
  const tr = el('tr');
  tr.appendChild(el('td', 'mono', String(p.id)));
  const what = el('td', null, p.summary);
  if (p.membership) what.appendChild(el('div', 'hint', t('g.membership')));
  what.appendChild(el('div', 'hint mono', t('g.by', { who: p.proposer })));
  tr.appendChild(what);
  tr.appendChild(el('td', null, p.status));
  const tally = el('td', null, t('g.tally', { aye: p.aye, nay: p.nay, all: p.electorate }));
  if (p.status === 'open') tally.appendChild(el('div', 'hint ' + (p.passes ? 'ok' : 'bad'), t(p.passes ? 'g.passing' : 'g.failing')));
  tr.appendChild(tally);
  tr.appendChild(el('td', 'hint', t('g.timeline', { close: p.voting_closes_at, enact: p.enactable_at })));
  const act = el('td');
  if (p.status === 'open') {
    if (v.head < p.voting_closes_at) {
      if (p.my_vote === null || p.my_vote === undefined) {
        const yes = el('button', 'btn sm', t('g.yes'));
        const no = el('button', 'btn ghost sm', t('g.no'));
        const vote = async (aye) => {
          yes.disabled = true; no.disabled = true;
          try { await call('governance_vote', { id: p.id, aye }); toast(t('g.voted')); setTimeout(loadGovernance, 3000); }
          catch (_) { yes.disabled = false; no.disabled = false; }
        };
        yes.onclick = () => vote(true);
        no.onclick = () => vote(false);
        yes.disabled = v.my_weight === 0; no.disabled = v.my_weight === 0;
        act.appendChild(yes); act.appendChild(no);
      } else {
        act.appendChild(el('span', 'hint', t(p.my_vote ? 'g.youYes' : 'g.youNo')));
      }
    } else if (v.head >= p.enactable_at) {
      const b = el('button', 'btn sm', t('g.enact'));
      b.onclick = async () => {
        b.disabled = true;
        try { await call('governance_enact', { id: p.id }); toast(t('g.enacting')); setTimeout(loadGovernance, 3000); }
        catch (_) { b.disabled = false; }
      };
      act.appendChild(b);
    } else {
      act.appendChild(el('span', 'hint', t('g.waiting')));
    }
  }
  tr.appendChild(act);
  return tr;
}

async function loadGovernance() {
  let v;
  try { v = await invoke('governance_view'); } catch (_) { return; }
  lastGov = v;
  govFieldsFor($('g-kind').value);
  $('g-authority').textContent = v.dissolved_at !== null && v.dissolved_at !== undefined
    ? t('g.dissolved', { at: v.dissolved_at })
    : (v.founder ? t('g.founder', { f: v.founder.slice(0, 16) }) : '–');
  $('g-weight').textContent = v.my_weight;
  $('g-head').textContent = v.head;
  $('g-partners').textContent = v.partners.length
    ? t('g.partners', { list: v.partners.map(p => p.slice(0, 16)).join(', ') })
    : t('g.noPartners');
  $('g-rules').textContent = t('g.rules', { vote: v.voting_period, delay: v.enactment_delay })
    + ' ' + t(v.oracle_replaced_by_vote ? 'g.proven' : 'g.unproven');
  const body = $('g-list');
  body.innerHTML = '';
  if (!v.proposals.length) {
    const tr = el('tr');
    const td = el('td', 'book-empty', t('g.none'));
    td.colSpan = 6;
    tr.appendChild(td);
    body.appendChild(tr);
  }
  for (const p of v.proposals) body.appendChild(govRow(p, v));
  $('g-submit').disabled = v.my_weight === 0;
  $('g-submit').title = v.my_weight === 0 ? t('g.noWeight') : '';
}

$('g-submit').onclick = async () => {
  const kind = $('g-kind').value;
  const fields = GOV_FIELDS[kind] || [];
  const args = { kind, key: null, text: null, amount: null };
  if (fields.includes('channel')) args.text = $('g-channel').value;
  if (fields.includes('amount')) {
    const n = parseInt($('g-amount').value || '', 10);
    args.amount = Number.isFinite(n) && n > 0 ? n : null;
    if (args.amount === null) { $('g-msg').textContent = t('err.amount'); return; }
  }
  const ok = await confirmDlg(t('g.confirm'));
  if (!ok) return;
  $('g-msg').textContent = t('m.posting');
  try {
    await call('governance_propose', args);
    $('g-msg').textContent = t('g.proposed');
    $('g-amount').value = '';
    setTimeout(() => { loadGovernance(); $('g-msg').textContent = ''; }, 3000);
  } catch (_) { $('g-msg').textContent = ''; }
};

loaders.governance = loadGovernance;
setInterval(() => { if (activePage === 'governance') loadGovernance(); }, 5000);

// ============================================================
// API
// ============================================================
async function loadApi() {
  const v = await call('api_view');
  const root = $('api-cards');
  root.innerHTML = '';

  const daemon = el('div', 'card');
  daemon.appendChild(el('h3', '', t('api.daemon')));
  daemon.appendChild(el('div', v.enabled ? 'ok' : 'warn',
    v.enabled ? t('api.listening', { addr: v.addr }) : t('api.disabled')));
  daemon.appendChild(el('div', 'hint', t('api.desc')));
  if (v.locked) {
    const lk = el('div', 'row');
    lk.appendChild(el('span', 'hint', t('api.lockedToken')));
    const ub = el('button', 'btn ghost sm', t('lk.unlock'));
    ub.onclick = () => askUnlock().then((ok) => { if (ok) loadApi(); });
    lk.appendChild(ub);
    daemon.appendChild(lk);
  }
  if (v.token) {
    const trow = el('div', 'row');
    trow.appendChild(el('span', 'hint', t('api.token')));
    const tok = el('span', 'mono', v.token.slice(0, 8) + '…');
    tok.style.cursor = 'pointer'; tok.title = t('common.copy');
    tok.onclick = () => copy(v.token, t('api.tokenCopied'));
    trow.appendChild(tok);
    daemon.appendChild(trow);
  }
  if (v.enabled && v.token) {
    const row = el('div', 'row');
    const curl = el('button', 'btn ghost sm', t('api.copyCurl'));
    curl.onclick = () => copy(
      'curl -H "Authorization: Bearer ' + v.token + '" http://' + v.addr + '/v1/models\n' +
      'curl http://' + v.addr + '/v1/chat/completions -H "Authorization: Bearer ' + v.token + '" -H \'Content-Type: application/json\' -d \'{"model":"nepheral-standard","messages":[{"role":"user","content":"hi"}]}\'',
      t('api.curlCopied'));
    const cont = el('button', 'btn ghost sm', t('api.copyCont'));
    cont.onclick = () => copy(JSON.stringify({
      models: [{ title: 'Nepheral Mesh', provider: 'openai', model: 'nepheral-standard', apiBase: 'http://' + v.addr + '/v1', apiKey: v.token }]
    }, null, 2), t('api.contCopied'));
    row.appendChild(curl); row.appendChild(cont);
    daemon.appendChild(row);
  }
  root.appendChild(daemon);

  const pol = el('div', 'card');
  pol.appendChild(el('h3', '', t('api.routing')));
  const chips = el('div', 'filter-chips');
  for (const tag of ['cross', 'standard', 'toptier', 'local']) {
    const c = el('button', 'fchip' + (v.policy_tag === tag ? ' active' : ''), t('api.mode.' + tag));
    c.onclick = () => call('api_set_routing', { tag }).then(() => setTimeout(loadApi, 300));
    chips.appendChild(c);
  }
  pol.appendChild(chips);
  root.appendChild(pol);

  const models = el('div', 'card');
  models.appendChild(el('h3', '', t('api.models')));
  const list = [
    ['nepheral-standard', t('api.m.standard')],
    ['nepheral-top-tier-{chat,large,frontier}', t('api.m.toptier')],
    ['nepheral-dev-*', t('api.m.dev')],
    ['nepheral-doc-*', t('api.m.doc')],
    ['nepheral-review-*', t('api.m.review')],
    ['nepheral-research-*', t('api.m.research')],
  ];
  for (const [m, desc] of list) {
    const r = el('div', 'row');
    r.appendChild(el('span', 'mono', m));
    r.appendChild(el('span', 'hint', desc));
    models.appendChild(r);
  }
  models.appendChild(el('div', 'hint', t('api.headers')));
  root.appendChild(models);
}
loaders.api = loadApi;

// ============================================================
// SETUP WIZARD
// ============================================================
// The wallet and its 24 words are made by the first-run window
// (unlock.html), so the wizard starts at the hardware step. Step numbers
// keep the indexes of su.steps, whose first two name that window's work.
const WIZ_FIRST = 2;
let wizStep = WIZ_FIRST;
let wizData = null;
let wizTier = null;

function wizNav(showBack) {
  const row = el('div', 'row');
  if (showBack && wizStep > WIZ_FIRST) {
    const back = el('button', 'btn ghost', t('common.back'));
    back.onclick = () => { wizStep = Math.max(WIZ_FIRST, wizStep - 1); renderWizard(); };
    row.appendChild(back);
  }
  const next = el('button', 'btn', t('common.next'));
  next.onclick = async () => {
    if (wizStep === 3 && wizData.backend_external) wizStep = 5;
    else wizStep = Math.min(5, wizStep + 1);
    renderWizard();
  };
  row.appendChild(next);
  return row;
}

async function renderWizard() {
  if (wizStep < WIZ_FIRST) wizStep = WIZ_FIRST;
  // Arriving at the download step installs the recommended model, unless
  // one the user picked is already downloading or on disk.
  if (wizStep === 4) {
    try { await invoke('setup_fetch_model'); } catch (_) {}
  }
  wizData = await call('setup_view');
  const steps = $('wiz-steps');
  const names = t('su.steps');
  steps.innerHTML = '';
  names.slice(WIZ_FIRST).forEach((s, j) => {
    const i = j + WIZ_FIRST;
    steps.appendChild(el('span', 'wstep' + (i === wizStep ? ' active' : i < wizStep ? ' done' : ''), (j + 1) + ' · ' + s));
  });
  const body = $('wiz-body');
  body.innerHTML = '';

  if (wizStep === 2) {
    body.appendChild(el('h3', '', t('su.hw')));
    try {
      const [hw, tuning] = await invoke('setup_hw_detect');
      body.appendChild(el('div', '', t('su.cpu', { n: hw.cpu_cores })));
      body.appendChild(el('div', '', t('su.ram', { n: hw.ram_mb })));
      body.appendChild(el('div', '', t('su.gpu', { s: hw.gpu })));
      body.appendChild(el('div', 'ok', t('su.recommended', { s: hw.recommended_tier })));
      if (hw.default_model_file) {
        body.appendChild(el('div', 'hint', t('su.willInstall', { f: hw.default_model_file, mb: hw.default_model_mb })));
      } else {
        body.appendChild(el('div', 'warn', t('su.noLocalModel')));
      }
      body.appendChild(el('div', 'hint', t('su.tuning', { ctx: tuning.n_ctx, ram: tuning.estimated_ram_mb })));
    } catch (e) {
      body.appendChild(el('div', 'warn', friendlyError(e)));
    }
    body.appendChild(wizNav(true));
  } else if (wizStep === 3) {
    body.appendChild(el('h3', '', t('su.model')));
    const ext = el('div', 'tier' + (wizTier === 'external' ? ' selected' : ''));
    ext.appendChild(el('b', '', t('su.external')));
    const extUrl = el('input', 'grow mono');
    extUrl.placeholder = t('su.externalPh');
    extUrl.value = wizData.external_url || '';
    extUrl.onclick = (e) => e.stopPropagation();
    ext.onclick = async () => {
      wizTier = 'external';
      await call('setup_set_model', { choice: { tier: 'external', external_url: extUrl.value } });
      renderWizard();
    };
    extUrl.onchange = () => call('setup_set_model', { choice: { tier: 'external', external_url: extUrl.value } });
    ext.appendChild(extUrl);
    body.appendChild(ext);
    body.appendChild(document.createElement('hr'));

    const installedBox = el('div');
    const rerenderWizInstalled = () => renderInstalled(installedBox, rerenderWizInstalled);
    rerenderWizInstalled();
    body.appendChild(installedBox);

    const catalogBox = el('div');
    const rerenderWizCatalog = () => renderCatalog(catalogBox, rerenderWizCatalog);
    rerenderWizCatalog();
    body.appendChild(catalogBox);

    body.appendChild(wizNav(true));
  } else if (wizStep === 4) {
    body.appendChild(el('h3', '', t('su.dl')));
    if (wizData.model_present && !wizData.download) {
      body.appendChild(el('div', 'ok', t('su.present')));
    } else if (wizData.download) {
      const [done, total] = wizData.download;
      const pct = total > 0 ? done * 100 / total : 0;
      const bar = el('div', 'progress'); bar.appendChild(el('div'));
      bar.firstChild.style.width = pct + '%';
      body.appendChild(bar);
      body.appendChild(el('div', 'hint', t('su.progress', {
        done: Math.floor(done / 1048576), total: Math.floor(total / 1048576),
      })));
      setTimeout(() => { if (activePage === 'setup' && wizStep === 4) renderWizard(); }, 1200);
    } else if (wizData.download_error) {
      body.appendChild(el('div', 'warn', t('su.dlFailed', { detail: wizData.download_error })));
    } else {
      body.appendChild(el('div', 'hint', t('su.notYet')));
    }
    body.appendChild(wizNav(true));
  } else {
    body.appendChild(el('h3', '', t('su.done')));
    body.appendChild(el('div', 'hint', t('su.yourAddr')));
    const pid = el('div', 'mono', wizData.peer_id.slice(0, 32) + '\n' + wizData.peer_id.slice(32));
    pid.style.whiteSpace = 'pre';
    body.appendChild(pid);
    const cp = el('button', 'btn ghost sm', t('su.copyAddr'));
    cp.onclick = () => copy(wizData.peer_id, 'ID');
    body.appendChild(cp);
    for (const k of ['su.next1', 'su.next2', 'su.next3', 'su.next4']) {
      body.appendChild(el('div', 'hint', '· ' + t(k)));
    }
    body.appendChild(el('div', 'warn', t('su.firstConn')));
    const enter = el('button', 'btn', t('su.enter'));
    enter.onclick = async () => {
      try {
        await call('setup_finish');
        $('tab-setup').style.display = 'none';
        selectPage('chat');
      } catch (_) {}
    };
    body.appendChild(enter);
    const back = el('button', 'btn ghost sm', t('common.back'));
    back.onclick = () => { wizStep = 4; renderWizard(); };
    body.appendChild(back);
  }
}
loaders.setup = renderWizard;

// initial
selectPage('chat');
