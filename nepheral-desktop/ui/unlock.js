// The unlock window. Nothing about the wallet is on disk in the clear, so
// the app starts here: open the vault with the password, create one, move a
// plain-text phrase from an older version into one, or restore from the 24
// words when the password is lost. The node and the main window start only
// after the vault is open (gate.rs).
const { invoke } = window.__TAURI__.core;
const $ = (id) => document.getElementById(id);

let mode = 'unlock';
let status = 'sealed';
let showing = 'form'; // form | phrase | pdf

// Relabels in place: reloading would lose the phrase, which is shown once.
function relabel() {
  document.documentElement.lang = LANG;
  document.querySelectorAll('[data-i18n]').forEach((n) => { n.textContent = t(n.dataset.i18n); });
  document.querySelectorAll('[data-i18n-ph]').forEach((n) => { n.placeholder = t(n.dataset.i18nPh); });
  $('lang-toggle').textContent = LANG === 'es' ? 'EN' : 'ES';
}
relabel();
$('lang-toggle').onclick = () => {
  LANG = LANG === 'es' ? 'en' : 'es';
  try { localStorage.setItem('nepheral_lang', LANG); } catch (_) {}
  relabel();
  if (showing === 'form') {
    const keep = [$('u-pass').value, $('u-pass2').value, $('u-phrase').value];
    show(mode);
    [$('u-pass').value, $('u-pass2').value, $('u-phrase').value] = keep;
    strength();
  } else {
    $('u-title').textContent = t('v.phraseTitle');
    $('u-lead').textContent = t('v.phraseLead');
  }
};

const MODES = {
  // [title, lead, button, alt link, alt target, needs confirm, needs phrase]
  unlock: ['v.unlock.title', 'v.unlock.lead', 'v.unlock.go', 'v.forgot', 'restore', false, false],
  create: ['v.create.title', 'v.create.lead', 'v.create.go', 'v.haveWords', 'restore', true, false],
  migrate: ['v.migrate.title', 'v.migrate.lead', 'v.migrate.go', null, null, true, false],
  restore: ['v.restore.title', 'v.restore.lead', 'v.restore.go', 'v.back', null, true, true],
};

function show(m) {
  mode = m;
  const [title, lead, go, alt, , confirm, phrase] = MODES[m];
  $('u-title').textContent = t(title);
  $('u-lead').textContent = t(lead);
  $('u-go').textContent = t(go);
  $('u-pass').placeholder = t(confirm ? 'v.newPass' : 'v.pass');
  $('u-pass').autocomplete = confirm ? 'new-password' : 'current-password';
  $('u-pass2').placeholder = t('v.confirm');
  $('u-pass2').style.display = confirm ? '' : 'none';
  $('u-strength').style.display = confirm ? '' : 'none';
  $('u-phrase').style.display = phrase ? '' : 'none';
  $('u-phrase').placeholder = t('v.phrasePh');
  $('u-alt').style.display = alt ? '' : 'none';
  if (alt) $('u-alt').textContent = t(alt);
  $('u-err').textContent = '';
  $('u-pass').value = ''; $('u-pass2').value = '';
  strength();
  (phrase ? $('u-phrase') : $('u-pass')).focus();
}

// A rough guide, not a rule: length counts most.
function strength() {
  const p = $('u-pass').value;
  let score = Math.min(p.length, 20) / 20;
  const classes = [/[a-z]/, /[A-Z]/, /\d/, /[^A-Za-z0-9]/].filter((r) => r.test(p)).length;
  score = Math.min(1, score * (0.6 + classes * 0.12));
  const bar = $('u-strength').firstElementChild;
  bar.style.width = Math.round(score * 100) + '%';
  bar.className = score < 0.4 ? 'weak' : score < 0.75 ? 'fair' : 'good';
}
$('u-pass').addEventListener('input', strength);

function busy(on, key) {
  $('u-form').style.display = on ? 'none' : '';
  $('u-wait').style.display = on ? '' : 'none';
  if (key) $('u-wait-text').textContent = t(key);
}

function explain(e) {
  const s = String(e || '');
  if (/wrong password/i.test(s)) return t('v.err.wrong');
  if (/at least \d+ characters/i.test(s)) return t('v.err.weak');
  if (/recovery phrase|mnemonic|phrase is invalid/i.test(s)) return t('v.err.phrase');
  return s;
}

async function go() {
  const pass = $('u-pass').value;
  const [, , , , , confirm, phrase] = MODES[mode];
  if (confirm && pass !== $('u-pass2').value) { $('u-err').textContent = t('v.err.match'); return; }
  if (confirm && pass.length < 8) { $('u-err').textContent = t('v.err.weak'); return; }
  const cmd = { unlock: 'vault_unlock', create: 'vault_create', migrate: 'vault_migrate', restore: 'vault_restore' }[mode];
  const args = phrase ? { phrase: $('u-phrase').value, password: pass } : { password: pass };
  busy(true, mode === 'unlock' ? 'v.opening' : 'v.sealing');
  try {
    const opened = await invoke(cmd, args);
    $('u-pass').value = ''; $('u-pass2').value = ''; $('u-phrase').value = '';
    if (opened && opened.phrase && opened.phrase.length) showPhrase(opened.phrase);
    else enter();
  } catch (e) {
    busy(false);
    $('u-err').textContent = explain(e);
  }
}

function showPhrase(words) {
  busy(false);
  showing = 'phrase';
  $('u-form').style.display = 'none';
  $('u-show').style.display = '';
  $('u-title').textContent = t('v.phraseTitle');
  $('u-lead').textContent = t('v.phraseLead');
  const box = $('u-words');
  box.innerHTML = '';
  words.forEach((w, i) => {
    const d = document.createElement('div');
    d.className = 'w';
    const n = document.createElement('i');
    n.textContent = String(i + 1);
    d.appendChild(n);
    d.appendChild(document.createTextNode(w));
    box.appendChild(d);
  });
  $('u-saved').onchange = () => { $('u-enter').disabled = !$('u-saved').checked; };
  $('u-enter').onclick = () => { box.innerHTML = ''; enter(); };
}

// The PDF swaps in for the words so the window keeps its size; paper stays
// the recommendation on both screens.
function pdfPanel(on) {
  showing = on ? 'pdf' : 'phrase';
  $('u-show').style.display = on ? 'none' : '';
  $('u-pdf').style.display = on ? '' : 'none';
  if (on) $('u-pdf-pass').focus();
}
$('u-pdf-open').onclick = () => pdfPanel(true);
$('u-pdf-back').onclick = () => pdfPanel(false);
$('u-pdf-go').onclick = async () => {
  const pass = $('u-pdf-pass').value;
  const msg = $('u-pdf-msg');
  msg.className = 'hint bad';
  if (pass !== $('u-pdf-pass2').value) { msg.textContent = t('v.err.match'); return; }
  if (pass.length < 8) { msg.textContent = t('v.err.weak'); return; }
  $('u-pdf-go').disabled = true;
  msg.className = 'hint';
  msg.textContent = t('v.sealing');
  try {
    const path = await invoke('vault_export_pdf', { password: pass, lang: LANG });
    if (path) {
      $('u-pdf-pass').value = ''; $('u-pdf-pass2').value = '';
      msg.className = 'hint ok';
      msg.textContent = t('v.pdf.saved', { path });
    } else {
      msg.textContent = '';
    }
  } catch (e) {
    msg.className = 'hint bad';
    msg.textContent = explain(e);
  }
  $('u-pdf-go').disabled = false;
};

async function enter() {
  showing = 'form';
  $('u-show').style.display = 'none';
  $('u-pdf').style.display = 'none';
  busy(true, 'v.starting');
  try {
    await invoke('vault_enter');
  } catch (e) {
    // The vault exists by now, whatever the form said before.
    busy(false);
    status = 'sealed';
    show('unlock');
    $('u-err').textContent = explain(e);
  }
}

// The phone keeps paper as the only copy: no PDF to save into its files.
if (MOBILE) $('u-pdf-open').style.display = 'none';

$('u-go').onclick = go;
[$('u-pass'), $('u-pass2')].forEach((n) => n.addEventListener('keydown', (e) => { if (e.key === 'Enter') go(); }));
$('u-alt').onclick = () => {
  const target = MODES[mode][4];
  show(target || (status === 'missing' ? 'create' : status === 'legacy' ? 'migrate' : 'unlock'));
};

(async () => {
  try { status = (await invoke('vault_status')).status; } catch (_) { status = 'sealed'; }
  show(status === 'missing' ? 'create' : status === 'legacy' ? 'migrate' : 'unlock');
})();
