// Shared by the desktop (app.js) and the phone app (mobile.js): DOM helpers
// and the Markdown renderer for answers. Loaded before either.

function el(tag, cls, text) {
  const n = document.createElement(tag);
  if (cls) n.className = cls;
  if (text !== undefined) n.textContent = text;
  return n;
}

// ============================================================
// MARKDOWN
// ============================================================
// Models answer in Markdown, and the chat showed it raw: asterisks, hashes
// and backticks as literal characters.
//
// Built as DOM nodes, never as HTML text. Everything a model writes is
// untrusted input arriving over a mesh from a stranger's machine, and the
// one rule that makes that safe is that no part of it is ever parsed as
// markup: text only ever reaches the page through textContent.

// Inline spans, longest markers first so ** is not read as two *.
const MD_INLINE = /(`[^`\n]+`)|(\*\*[^*\n]+\*\*)|(__[^_\n]+__)|(~~[^~\n]+~~)|(\*[^*\n]+\*)|(_[^_\n]+_)/;

function mdInline(text, into) {
  let rest = text;
  for (;;) {
    const m = MD_INLINE.exec(rest);
    if (!m) break;
    if (m.index > 0) into.appendChild(document.createTextNode(rest.slice(0, m.index)));
    const tok = m[0];
    if (tok.startsWith('`')) into.appendChild(el('code', 'md-code', tok.slice(1, -1)));
    else if (tok.startsWith('**')) into.appendChild(el('strong', '', tok.slice(2, -2)));
    else if (tok.startsWith('__')) into.appendChild(el('strong', '', tok.slice(2, -2)));
    else if (tok.startsWith('~~')) into.appendChild(el('s', '', tok.slice(2, -2)));
    else into.appendChild(el('em', '', tok.slice(1, -1)));
    rest = rest.slice(m.index + tok.length);
  }
  if (rest) into.appendChild(document.createTextNode(rest));
}

function mdPara(text) {
  const p = el('p', 'md-p');
  mdInline(text, p);
  return p;
}

// Bullets and numbers at any indentation. Two spaces make a nested level;
// deeper nesting is flattened rather than guessed at.
function mdListItem(line) {
  const m = /^(\s*)(?:([-*+])|(\d+)[.)])\s+(.*)$/.exec(line);
  if (!m) return null;
  return {
    depth: Math.min(Math.floor(m[1].replace(/\t/g, '  ').length / 2), 3),
    ordered: m[3] !== undefined,
    text: m[4],
  };
}

function renderMarkdown(src) {
  const out = document.createDocumentFragment();
  const lines = String(src).split('\n');
  let i = 0;

  while (i < lines.length) {
    const line = lines[i];

    // Fenced code. An unterminated fence runs to the end rather than
    // swallowing the block and showing nothing.
    const fence = /^\s*```(.*)$/.exec(line);
    if (fence) {
      const body = [];
      i += 1;
      while (i < lines.length && !/^\s*```/.test(lines[i])) { body.push(lines[i]); i += 1; }
      i += 1;
      const pre = el('pre', 'md-pre');
      pre.appendChild(el('code', '', body.join('\n')));
      out.appendChild(pre);
      continue;
    }

    if (/^\s*(?:---+|\*\*\*+|___+)\s*$/.test(line)) {
      out.appendChild(el('hr', 'md-hr'));
      i += 1;
      continue;
    }

    const head = /^(#{1,6})\s+(.*)$/.exec(line);
    if (head) {
      const h = el('div', 'md-h md-h' + head[1].length);
      mdInline(head[2], h);
      out.appendChild(h);
      i += 1;
      continue;
    }

    if (/^\s*>\s?/.test(line)) {
      const body = [];
      while (i < lines.length && /^\s*>\s?/.test(lines[i])) {
        body.push(lines[i].replace(/^\s*>\s?/, ''));
        i += 1;
      }
      const q = el('div', 'md-quote');
      q.appendChild(renderMarkdown(body.join('\n')));
      out.appendChild(q);
      continue;
    }

    if (mdListItem(line)) {
      // One run of items; a change of depth opens or closes a nested list.
      const stack = [];
      const open = (ordered, depth) => {
        const list = el(ordered ? 'ol' : 'ul', 'md-list');
        if (stack.length) stack[stack.length - 1].list.lastChild.appendChild(list);
        else out.appendChild(list);
        stack.push({ list, depth });
      };
      while (i < lines.length) {
        const item = mdListItem(lines[i]);
        if (!item) {
          // A blank line inside a list is a separator, not its end.
          if (/^\s*$/.test(lines[i]) && mdListItem(lines[i + 1] || '')) { i += 1; continue; }
          break;
        }
        while (stack.length && stack[stack.length - 1].depth > item.depth) stack.pop();
        if (!stack.length || stack[stack.length - 1].depth < item.depth) open(item.ordered, item.depth);
        const li = el('li', 'md-li');
        mdInline(item.text, li);
        stack[stack.length - 1].list.appendChild(li);
        i += 1;
      }
      continue;
    }

    if (/^\s*$/.test(line)) { i += 1; continue; }

    // A paragraph runs until a blank line or the start of another block.
    const body = [];
    while (
      i < lines.length &&
      !/^\s*$/.test(lines[i]) &&
      !/^\s*```/.test(lines[i]) &&
      !/^#{1,6}\s/.test(lines[i]) &&
      !/^\s*>/.test(lines[i]) &&
      !mdListItem(lines[i])
    ) {
      body.push(lines[i]);
      i += 1;
    }
    out.appendChild(mdPara(body.join('\n')));
  }

  return out;
}
