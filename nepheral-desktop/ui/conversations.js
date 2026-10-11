// Conversation memory, shared by the desktop and the phone
// (docs/CONVERSATIONS.md): the state of NAI's private memory, the card
// that asks before older context leaves the device, and private messages.
// Uses the page's own el(), t() and call().

const CV_MB = (bytes) => Math.round(bytes / 1e6);

// What the memory model is doing, as one line (and a button when there is
// something to do).
function cvMemoryNode(view) {
  const box = el('div', 'cv-mem');
  const m = view.model;
  const mb = CV_MB(view.size_bytes);
  let text;
  let action = null;
  if (m.state === 'ready') {
    text = t('cv.mem.ready');
    box.classList.add('ok');
  } else if (m.state === 'downloading') {
    const pct = m.total ? Math.floor((m.done * 100) / m.total) : 0;
    text = t('cv.mem.downloading', { pct, mb });
    const bar = el('div', 'cv-bar');
    const fill = el('div', 'cv-fill');
    fill.style.width = pct + '%';
    bar.appendChild(fill);
    box.appendChild(el('div', '', text));
    box.appendChild(bar);
    return box;
  } else if (m.state === 'loading') {
    text = t('cv.mem.loading');
  } else if (m.state === 'failed') {
    text = t('cv.mem.failed', { detail: m.error });
    action = t('cv.mem.retry');
  } else {
    text = t('cv.mem.missing', { mb });
    action = t('cv.mem.get');
  }
  box.appendChild(el('div', '', text));
  if (action) {
    const b = el('button', 'btn sm', action);
    b.onclick = async () => {
      b.disabled = true;
      try { await call('memory_download'); } catch (_) { b.disabled = false; }
    };
    box.appendChild(b);
  }
  return box;
}

// While "don't ask" runs: until when, and a way to end it early.
function cvSnoozeNode(view, after) {
  if (!view.snoozed_until_ms) return null;
  const until = new Date(view.snoozed_until_ms);
  const sameDay = until.toDateString() === new Date().toDateString();
  const when = sameDay
    ? until.toLocaleTimeString([], { hour: '2-digit', minute: '2-digit' })
    : until.toLocaleString([], { weekday: 'short', day: 'numeric', hour: '2-digit', minute: '2-digit' });
  const box = el('div', 'cv-mem');
  box.appendChild(el('div', '', t('cv.snoozed', { when })));
  const b = el('button', 'btn sm ghost', t('cv.snooze.resume'));
  b.onclick = async () => {
    b.disabled = true;
    try { await call('chat_snooze', { minutes: 0 }); } catch (_) {}
    after();
  };
  box.appendChild(b);
  return box;
}

// Ticked boxes survive the thread being drawn again: session:index -> bool.
const cvTicks = new Map();

// The card for a question waiting to be confirmed. `done` runs once it has
// been sent or taken back, with the text to put back in the input box.
function cvPendingCard(session, pending, done) {
  const card = el('div', 'cv-confirm');
  card.appendChild(el('div', 'cv-title', t('cv.confirm.title')));
  const lead = pending.summary
    ? t('cv.confirm.summary', { n: pending.fragments.length })
    : t('cv.confirm.share', { pct: Math.round(pending.share * 100) });
  const sensitive = pending.fragments.some((f) => f.sensitive);
  card.appendChild(el('div', 'hint', lead + (sensitive ? ' ' + t('cv.confirm.sensitive') : '')));
  card.appendChild(el('div', 'hint', t('cv.confirm.review')));
  const list = el('div', 'cv-list');
  for (const f of pending.fragments) {
    const key = session + ':' + f.index;
    if (!cvTicks.has(key)) cvTicks.set(key, true);
    const row = el('label', 'cv-frag' + (f.sensitive ? ' sensitive' : ''));
    const box = document.createElement('input');
    box.type = 'checkbox';
    box.checked = cvTicks.get(key);
    box.onchange = () => cvTicks.set(key, box.checked);
    row.appendChild(box);
    const body = el('div', 'cv-frag-body');
    const who = el('b', '', (f.from_me ? t('cv.you') : 'NAI') + (f.sensitive ? ' · ' + t('cv.sensitive') : ''));
    body.appendChild(who);
    body.appendChild(el('div', 'md-plain', f.text));
    row.appendChild(body);
    list.appendChild(row);
  }
  card.appendChild(list);
  // "Don't ask for a while": the pause starts with this question.
  const snooze = el('div', 'cv-snooze');
  const snoozeLabel = el('label', 'cv-keep');
  const snoozeBox = document.createElement('input');
  snoozeBox.type = 'checkbox';
  snoozeLabel.appendChild(snoozeBox);
  snoozeLabel.appendChild(el('span', '', t('cv.snooze.ask')));
  const snoozeFor = document.createElement('select');
  for (const [minutes, key] of [[60, 'cv.snooze.h1'], [480, 'cv.snooze.h8'], [1440, 'cv.snooze.d1'], [10080, 'cv.snooze.w1']]) {
    const o = document.createElement('option');
    o.value = String(minutes);
    o.textContent = t(key);
    snoozeFor.appendChild(o);
  }
  snoozeFor.onchange = () => { snoozeBox.checked = true; };
  snooze.appendChild(snoozeLabel);
  snooze.appendChild(snoozeFor);
  card.appendChild(snooze);
  const pause = async () => {
    if (!snoozeBox.checked) return;
    try { await call('chat_snooze', { minutes: Number(snoozeFor.value) }); } catch (_) {}
  };
  const actions = el('div', 'cv-actions');
  const forget = () => { for (const f of pending.fragments) cvTicks.delete(session + ':' + f.index); };
  const send = el('button', 'btn', t('cv.confirm.send'));
  send.onclick = async () => {
    const keep = pending.fragments.map((f) => f.index).filter((i) => cvTicks.get(session + ':' + i));
    forget();
    await pause();
    await call('chat_confirm', { id: session, keep });
    done(null);
  };
  const bare = el('button', 'btn ghost', t('cv.confirm.bare'));
  bare.onclick = async () => {
    forget();
    await pause();
    await call('chat_confirm', { id: session, keep: [] });
    done(null);
  };
  const cancel = el('button', 'btn ghost', t('cv.confirm.cancel'));
  cancel.onclick = async () => {
    forget();
    const text = await call('chat_cancel', { id: session });
    done(text);
  };
  actions.appendChild(send);
  actions.appendChild(bare);
  actions.appendChild(cancel);
  card.appendChild(actions);
  return card;
}

// A message's lock: marks it private (never chosen, never sent) or not.
function cvPrivateButton(session, index, isPrivate, after) {
  const b = el('button', 'btn ghost sm cv-lock' + (isPrivate ? ' on' : ''), '🔒');
  b.title = t(isPrivate ? 'cv.private.unmark' : 'cv.private.mark');
  b.onclick = async (ev) => {
    ev.stopPropagation();
    await call('chat_private', { id: session, index, private: !isPrivate });
    after();
  };
  return b;
}

// A failure shown in place of an answer, in the UI's language.
const CV_ERRORS = {
  no_ai: 'cerr.no_ai', offline: 'cerr.offline', no_channel: 'cerr.no_channel',
  busy: 'cerr.busy', timeout: 'cerr.timeout', local_not_ready: 'cerr.local_not_ready',
  local_busy: 'cerr.local_busy', local_error: 'cerr.local_error', generic: 'cerr.generic',
};
function cvErrorText(m) {
  const detail = m.content.replace(/^Local engine error:\s*/, '');
  return t(CV_ERRORS[m.error] || 'cerr.generic', { detail });
}

// Why the node is not on the network, while it is not.
const CV_NET = {
  trying: 'net.why.trying', blocked: 'net.why.blocked', no_internet: 'net.why.no_internet',
  clock: 'net.why.clock', update: 'net.why.update',
};
function cvNetReason(problem) {
  return problem ? t(CV_NET[problem] || 'net.why.trying') : '';
}

// The hardware a node seals its key with, shown as a badge.
function sealBadge(seal) {
  if (!seal) return null;
  const label = { tpm: 'TPM', ftpm: 'fTPM', pluton: 'Pluton', tee: 'TEE', strongbox: 'StrongBox' }[seal] || seal;
  const b = el('span', 'seal-badge', '⛨ ' + label);
  b.title = t('seal.tip.' + seal);
  return b;
}

// What identifies the drawn state of a thread's memory parts.
function cvSignature(view) {
  return view.messages.map((m) => (m.private ? 'p' : '')).join('') + '|' +
    (view.pending ? view.pending.fragments.map((f) => f.index).join('.') : '');
}
