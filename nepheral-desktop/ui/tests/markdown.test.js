// Exercises the renderer against a minimal DOM, including the shape the
// model actually produced: bold labels, nested numbered lists under bullets,
// and an h3. Also pins the property that makes it safe to run on text from
// a stranger's node: nothing is ever parsed as markup.
const fs = require('fs');

// --- the smallest DOM the renderer needs -----------------------------------
class Node {
  constructor(tag) {
    this.tag = tag;
    this.className = '';
    this.children = [];
    this._text = '';
  }
  appendChild(n) {
    if (n && n.__fragment) n.children.forEach((c) => this.children.push(c));
    else this.children.push(n);
    return n;
  }
  set textContent(v) { this._text = String(v); }
  get textContent() { return this._text; }
  get lastChild() { return this.children[this.children.length - 1]; }
  render() {
    const inner = this._text + this.children.map((c) => (typeof c === 'string' ? c : c.render())).join('');
    return `<${this.tag}${this.className ? ` class="${this.className}"` : ''}>${inner}</${this.tag}>`;
  }
}
global.document = {
  createElement: (tag) => new Node(tag),
  createTextNode: (t) => String(t),
  createDocumentFragment: () => {
    const f = new Node('#fragment');
    f.__fragment = true;
    return f;
  },
};
global.el = (tag, cls, text) => {
  const n = document.createElement(tag);
  if (cls) n.className = cls;
  if (text !== undefined) n.textContent = text;
  return n;
};

// --- load the renderer -------------------------------------------------------
eval(fs.readFileSync(require('path').join(__dirname, '..', 'markdown.js'), 'utf8'));

let failures = 0;
function check(name, cond, detail) {
  if (cond) console.log('  ok   ' + name);
  else { console.log('  FAIL ' + name + (detail ? ' -> ' + detail : '')); failures += 1; }
}

const render = (s) => {
  const box = document.createElement('div');
  box.appendChild(renderMarkdown(s));
  return box.render();
};

// --- the shape from the screenshot -----------------------------------------
const sample = [
  '*   **The Logic:** If a subject inhales a very high concentration.',
  '*   **Mechanism of Action:**',
  '    1.  The subject inhales the agent.',
  '    2.  The cells release gas.',
  '',
  '### 2. The Deceptive One (The Chemical Method)',
  'This approach tries to confuse the system.',
].join('\n');

const out = render(sample);
check('bullets become a list', out.includes('<ul class="md-list">'));
check('nested numbers become an ordered list', out.includes('<ol class="md-list">'));
check('the nested list sits inside an item', /<li class="md-li">[^]*<ol/.test(out));
check('bold renders as strong', out.includes('<strong>The Logic:</strong>'));
check('no stray asterisks remain', !out.includes('**'), out.slice(0, 120));
check('heading becomes a heading', out.includes('md-h md-h3'));
check('heading hashes are gone', !out.includes('### '));
check('the trailing paragraph survives', out.includes('This approach tries to confuse'));

// --- other blocks -----------------------------------------------------------
check('fenced code', render('```\nlet x = 1;\n```').includes('<pre class="md-pre">'));
check('inline code', render('use `cargo test` here').includes('<code class="md-code">'));
check('quote', render('> quoted').includes('md-quote'));
check('rule', render('---').includes('md-hr'));
check('italics', render('this is *important*').includes('<em>important</em>'));
check('strikethrough', render('~~old~~').includes('<s>old</s>'));
check('unterminated fence still shows its body', render('```\nabc').includes('abc'));
check('plain text is a paragraph', render('hello').includes('<p class="md-p">'));

// --- the safety property ----------------------------------------------------
// Everything the model writes reaches the page as text. If any of it were
// inserted as markup, these would come back as real tags.
const hostile = '<img src=x onerror=alert(1)> **a** <script>alert(2)</script>';
const box = document.createElement('div');
box.appendChild(renderMarkdown(hostile));
const texts = [];
(function walk(n) {
  if (typeof n === 'string') { texts.push(n); return; }
  if (n._text) texts.push(n._text);
  n.children.forEach(walk);
})(box);
check('markup arrives as text, not as nodes', texts.join('').includes('<script>alert(2)</script>'));
// The serialisation above is this harness's own concatenation and does not
// escape, so it proves nothing. What matters is which elements were created:
// createElement is only ever called with tags this renderer chooses.
const tags = [];
(function collect(n) {
  if (typeof n === 'string') return;
  tags.push(n.tag);
  n.children.forEach(collect);
})(box);
check("only the renderer's own tags are created", !tags.some((t) => ['img', 'script', 'iframe', 'a'].includes(t)), tags.join(','));

console.log(failures ? `\n${failures} FAILED` : '\nrenderer: all checks passed');
process.exit(failures ? 1 : 0);
