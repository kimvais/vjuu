'use strict';
const $ = (s) => document.querySelector(s);
const content = $('#content');
const addBtn = $('#add');
const overall = $('#overall');

/** {docs: [{name, kind, text}], autoclose} */
let req;
let sideBySide = false;
let focused = null;
/** key -> {el, a}: every commentable element of the current render. */
let anchors = new Map();
/** [{el, doc, file, add, del}]: one per file, for the file list. Rebuilt by render(). */
let targets = [];
let filesHidden = false;
/** key -> {doc, file, start, end, old, quote, text, editing}. Survives re-renders. */
const comments = new Map();

// The doc index keeps two documents with the same name apart.
const keyOf = (a) => `${a.doc}\n${a.file}\n${a.start}-${a.end}${a.old ? 'o' : ''}`;
const post = (msg) => window.ipc.postMessage(JSON.stringify(msg));

const scripts = new Map();

function loadScript(src) {
  if (!scripts.has(src)) {
    scripts.set(src, new Promise((resolve, reject) => {
      const s = document.createElement('script');
      s.src = src;
      s.onload = resolve;
      s.onerror = reject;
      document.head.append(s);
    }));
  }
  return scripts.get(src);
}

const RENDERERS = ['vendor/highlight.min.js', 'vendor/markdown-it.min.js', 'vendor/diff2html-ui-base.min.js'];
/** highlight.js plus grammars its common bundle lacks; they register on the global hljs. */
const loadHighlight = () => loadScript(RENDERERS[0]).then(() => loadScript('vendor/hljs-languages.min.js'));

/** Loads the documents; a pre-warmed spare window gets 404 until vjuu hands it some and calls this again. */
async function load() {
  if (req) return;
  // Resident windows share one browser profile; never reuse an earlier document.
  const r = await fetch('doc.json', { cache: 'no-store' });
  if (!r.ok) {
    // Spare window: compile every renderer now so the first real document doesn't wait for it.
    loadHighlight();
    RENDERERS.slice(1).forEach(loadScript);
    return;
  }
  const d = await r.json();
  if (req) return;
  req = d;
  $('#name').textContent = req.docs.map((x) => x.name).join(', ');
  $('#view').hidden = !hasDiff();
  const renderers = new Set(req.docs.map((x) => ({ diff: RENDERERS[2], md: RENDERERS[1] })[x.kind]).filter(Boolean));
  await Promise.all([loadHighlight(), ...[...renderers].map(loadScript)]);
  render();
  if (req.autoclose) requestAnimationFrame(() => post({ type: 'rendered' }));
}

const hasDiff = () => req.docs.some((x) => x.kind === 'diff');

function render() {
  anchors = new Map();
  targets = [];
  focused = null;
  content.replaceChildren();
  req.docs.forEach((doc, i) => {
    const section = document.createElement('section');
    section.className = 'doc';
    content.append(section);
    if (doc.kind === 'diff') {
      renderDiff(section, doc, i);
    } else if (doc.kind === 'code') {
      renderCode(section, doc, i);
    } else {
      // Diffs and code carry their own headers; markdown gets one when there is more than one document.
      if (req.docs.length > 1) section.innerHTML = `<div class="doc-header"></div>`;
      section.querySelector('.doc-header')?.append(doc.name);
      if (req.docs.length > 1) targets.push({ el: section, doc: i, file: doc.name });
      renderMarkdown(section, doc, i);
    }
  });
  renderComments();
}

function anchor(el, a) {
  el.dataset.key = keyOf(a);
  anchors.set(el.dataset.key, { el, a });
}

// SQL and INI are left out: they claim plain prose and command lines.
const AUTO_LANGUAGES = ['bash', 'powershell', 'python', 'json', 'yaml', 'rust', 'typescript', 'javascript', 'dockerfile', 'xml', 'diff'];
const SHELLS = ['bash', 'powershell'];
/** Lines starting like source code, not a command. */
const CODE_KEYWORDS = /^(def|class|import|from|return|if|for|while|with|const|let|var|function|fn|use|pub|impl|struct|SELECT|INSERT|UPDATE)\b/;

function renderMarkdown(section, doc, i) {
  // html off: the document is agent output and must not run script next to window.ipc.
  const highlight = (code, lang) => {
    if (lang) return hljs.getLanguage(lang) ? hljs.highlight(code, { language: lang, ignoreIllegals: true }).value : '';
    // Unlabelled fences: highlight.js guesses short snippets badly (`cargo install .` scores as SQL), and in
    // agent-written docs they are mostly shell. A confident non-shell guess wins; command-like lines are shell;
    // anything else (example output) stays plain.
    // Command lines are checked first: `# comments` alone make highlight.js call them Python.
    const lines = code.split('\n').filter((l) => l.trim());
    const isCommand = (l) => /^(\$ )?[\w.\\/:~-]+(\s|$)/.test(l) && !/[:{;]\s*$/.test(l) && !CODE_KEYWORDS.test(l);
    if (!lines.length || !lines.every(isCommand)) {
      const guess = hljs.highlightAuto(code, AUTO_LANGUAGES);
      return guess.relevance >= 3 && !SHELLS.includes(guess.language) ? guess.value : '';
    }
    if (/\b[A-Z][a-z]+-[A-Z][A-Za-z]+\b/.test(code)) return hljs.highlight(code, { language: 'powershell', ignoreIllegals: true }).value;
    // The bash grammar only knows builtins; colour each line's command and its flags like a shell does.
    return hljs.highlight(code, { language: 'bash', ignoreIllegals: true }).value.split('\n').map((line) => line
      .replace(/^(\s*(?:\$ )?)([\w.\\/:~-]+)/, '$1<span class="hljs-title">$2</span>')
      .replace(/(^|>)([^<]*)/g, (_, tag, text) => tag + text.replace(/(\s)(--?\w[\w-]*)/g, '$1<span class="hljs-attr">$2</span>')))
      .join('\n');
  };
  const md = window.markdownit({ linkify: true, highlight });
  const lines = doc.text.split(/\r?\n/);
  const tokens = md.parse(doc.text, {});
  for (const t of tokens) {
    if (t.map && t.nesting !== -1) t.attrSet('data-map', t.map.join(','));
  }
  const article = document.createElement('article');
  article.className = 'markdown';
  article.innerHTML = md.renderer.render(tokens, md.options, {});
  section.append(article);
  // Relative images resolve against each document's own folder, served by vjuu as files/<i>/.
  for (const img of article.querySelectorAll('img[src]')) {
    const src = img.getAttribute('src');
    if (!/^([a-z]+:|\/)/i.test(src)) img.src = `files/${i}/${src}`;
  }
  for (const h of article.querySelectorAll('h1, h2, h3')) {
    targets.push({ el: h, doc: i, file: doc.name, heading: h.textContent, level: Number(h.tagName[1]) });
  }
  for (const el of article.querySelectorAll('[data-map]')) {
    const [from, to] = el.dataset.map.split(',').map(Number);
    let end = to;
    while (end > from + 1 && !lines[end - 1].trim()) end--;
    anchor(el, { doc: i, file: doc.name, start: from + 1, end, old: false, quote: lines.slice(from, end).join('\n') });
  }
}

function renderCode(section, doc, i) {
  const text = doc.text.replace(/\r\n/g, '\n').replace(/\n$/, '');
  const lang = doc.name.split('.').pop().toLowerCase();
  const html = hljs.getLanguage(lang) ? hljs.highlight(text, { language: lang, ignoreIllegals: true }).value : escapeHtml(text);
  const raw = text.split('\n');
  const header = document.createElement('div');
  header.className = 'doc-header';
  header.textContent = doc.name;
  const table = document.createElement('table');
  table.className = 'code';
  splitHighlighted(html).forEach((line, idx) => {
    const tr = table.insertRow();
    const num = tr.insertCell();
    num.className = 'code-ln';
    num.textContent = idx + 1;
    tr.insertCell().innerHTML = line;
    anchor(tr, { doc: i, file: doc.name, start: idx + 1, end: idx + 1, old: false, quote: raw[idx] });
  });
  const scroller = document.createElement('div');
  scroller.className = 'code-scroll';
  scroller.append(table);
  section.append(header, scroller);
  targets.push({ el: section, doc: i, file: doc.name });
}

/** Splits highlighted HTML into lines, closing and reopening spans (comments, strings) that cross a line break. */
function splitHighlighted(html) {
  const open = [];
  return html.split('\n').map((line) => {
    const prefix = open.join('');
    for (const [tag] of line.matchAll(/<span[^>]*>|<\/span>/g)) {
      if (tag === '</span>') open.pop(); else open.push(tag);
    }
    return prefix + line + '</span>'.repeat(open.length);
  });
}

function escapeHtml(text) {
  const div = document.createElement('div');
  div.textContent = text;
  return div.innerHTML;
}

function renderDiff(section, doc, i) {
  const ui = new Diff2HtmlUI(section, doc.text, {
    outputFormat: sideBySide ? 'side-by-side' : 'line-by-line',
    drawFileList: false,
    matching: 'lines',
    // draw() would highlight synchronously; it is done after paint below.
    highlight: false,
    fileContentToggle: false,
    // Themes recolour the light scheme through its CSS variables.
    colorScheme: 'light',
  }, hljs);
  ui.draw();
  // Highlighting a large diff takes hundreds of ms; paint first, colour after.
  requestAnimationFrame(() => setTimeout(() => ui.highlightCode()));
  for (const wrapper of section.querySelectorAll('.d2h-file-wrapper')) {
    const file = wrapper.querySelector('.d2h-file-name').textContent.split(' → ').pop().trim();
    // Side-by-side shows added lines on the right only and deleted ones on the left only.
    const count = (cls) => wrapper.querySelectorAll(`.d2h-code-linenumber.${cls}, .d2h-code-side-linenumber.${cls}`).length;
    targets.push({ el: wrapper, doc: i, file, add: count('d2h-ins'), del: count('d2h-del') });
    const lineText = (tr) =>
      (tr.querySelector('.d2h-code-line-prefix')?.textContent.trim() ?? '') +
      (tr.querySelector('.d2h-code-line-ctn')?.textContent.trimEnd() ?? '');
    if (!sideBySide) {
      for (const tr of wrapper.querySelectorAll('tr')) {
        const num = tr.querySelector('.d2h-code-linenumber');
        if (!num) continue;
        const n2 = num.querySelector('.line-num2')?.textContent.trim();
        const n1 = num.querySelector('.line-num1')?.textContent.trim();
        if (!n1 && !n2) continue;
        const line = Number(n2 || n1);
        anchor(tr, { doc: i, file, start: line, end: line, old: !n2, quote: lineText(tr) });
      }
      continue;
    }
    wrapper.querySelectorAll('.d2h-file-side-diff').forEach((side, sideIdx) => {
      side.querySelectorAll('tbody tr').forEach((tr, idx) => {
        tr.dataset.idx = idx;
        const n = tr.querySelector('.d2h-code-side-linenumber')?.textContent.trim();
        // Left side: only deleted lines; unchanged lines are commented on the right.
        if (!n || (sideIdx === 0 && !tr.cells[1].classList.contains('d2h-del'))) return;
        anchor(tr, { doc: i, file, start: Number(n), end: Number(n), old: sideIdx === 0, quote: lineText(tr) });
      });
    });
  }
}

function renderComments() {
  content.querySelectorAll('.vjuu-c').forEach((e) => e.remove());
  $('#orphans')?.remove();
  const orphans = [];
  for (const [key, c] of comments) {
    const box = commentBox(key, c);
    const entry = anchors.get(key) ?? anchors.get(c.anchorKey);
    if (entry) place(entry.el, box); else orphans.push(box);
  }
  if (orphans.length) {
    const div = document.createElement('div');
    div.id = 'orphans';
    div.innerHTML = '<h3>Comments on lines not shown in this view</h3>';
    div.append(...orphans);
    content.append(div);
  }
  const n = [...comments.values()].filter((c) => c.text.trim()).length;
  $('#count').textContent = n ? `${n} comment${n > 1 ? 's' : ''}` : '';
  renderFileList();
  content.querySelector('.vjuu-c textarea, #orphans textarea')?.focus();
}

function renderFileList() {
  const nav = $('#files');
  const show = targets.length > 1 && !filesHidden;
  nav.hidden = !show;
  document.body.classList.toggle('with-files', show);
  if (!show) return;
  const perFile = new Map();
  for (const c of comments.values()) {
    if (c.text.trim()) perFile.set(`${c.doc}\n${c.file}`, (perFile.get(`${c.doc}\n${c.file}`) ?? 0) + 1);
  }
  const span = (cls, text) => {
    const s = document.createElement('span');
    s.className = cls;
    s.textContent = text;
    return s;
  };
  const indent = req.docs.length > 1 ? 1 : 0;
  nav.replaceChildren(...targets.map((t) => {
    const a = document.createElement('a');
    a.onclick = () => t.el.scrollIntoView({ block: 'start' });
    if (t.heading) {
      // Markdown: a table of contents, indented by heading level.
      a.className = 'toc';
      a.title = t.heading;
      a.style.paddingLeft = `${12 + (t.level - 1 + indent) * 14}px`;
      a.append(span('heading', t.heading));
      return a;
    }
    a.title = t.file;
    const parts = t.file.split(/[\\/]/);
    a.append(span('base', parts.pop()), span('dir', parts.join('/')));
    if (t.add !== undefined) a.append(span('add', `+${t.add}`), span('del', `−${t.del}`));
    const n = perFile.get(`${t.doc}\n${t.file}`);
    if (n) a.append(span('badge', n));
    return a;
  }));
  markActiveFile();
}

/** Highlights the file at the top of the view. */
function markActiveFile() {
  document.documentElement.style.setProperty('--header-h', `${$('header').offsetHeight}px`);
  const nav = $('#files');
  if (nav.hidden) return;
  const top = $('header').offsetHeight + 8;
  let active = 0;
  targets.forEach((t, i) => { if (t.el.getBoundingClientRect().top <= top) active = i; });
  // A short last file never reaches the top; at the bottom of the page it is the current one.
  if (window.innerHeight + window.scrollY >= document.documentElement.scrollHeight - 2) active = targets.length - 1;
  [...nav.children].forEach((a, i) => a.classList.toggle('active', i === active));
  nav.children[active]?.scrollIntoView({ block: 'nearest' });
}

let scrollQueued = false;
window.addEventListener('scroll', () => {
  if (scrollQueued) return;
  scrollQueued = true;
  requestAnimationFrame(() => { scrollQueued = false; markActiveFile(); });
});

function place(el, box) {
  if (el.tagName !== 'TR') {
    box.classList.add('vjuu-c');
    if (el.tagName === 'LI') el.append(box); else el.after(box);
    return;
  }
  const row = document.createElement('tr');
  row.className = 'vjuu-c';
  const td = row.insertCell();
  td.colSpan = el.cells.length;
  td.append(box);
  el.after(row);
  const sides = el.closest('.d2h-files-diff')?.querySelectorAll('.d2h-file-side-diff');
  if (!sides) return;
  // Side-by-side: pad the other table so the rows stay aligned.
  const other = sides[0].contains(el) ? sides[1] : sides[0];
  const twin = other.querySelector(`tr[data-idx="${el.dataset.idx}"]`);
  if (!twin) return;
  const spacer = document.createElement('tr');
  spacer.className = 'vjuu-c spacer';
  spacer.insertCell().colSpan = twin.cells.length;
  twin.after(spacer);
  new ResizeObserver(() => { spacer.style.height = `${row.offsetHeight}px`; }).observe(row);
}

function commentBox(key, c) {
  const box = document.createElement('div');
  box.className = 'comment';
  if (!c.editing) {
    const text = document.createElement('div');
    text.className = 'text';
    text.textContent = c.text;
    text.title = 'Click to edit';
    text.onclick = () => { c.editing = true; renderComments(); };
    const del = document.createElement('button');
    del.textContent = '×';
    del.title = 'Delete';
    del.onclick = () => { comments.delete(key); changed(); };
    box.append(text, del);
    return box;
  }
  const ta = document.createElement('textarea');
  ta.value = c.text;
  ta.rows = 3;
  ta.placeholder = `Comment on ${c.file}:${c.start}${c.end !== c.start ? `-${c.end}` : ''} (Ctrl+Enter to save, Esc to cancel)`;
  const save = () => {
    c.text = ta.value.trim();
    c.editing = false;
    if (!c.text) comments.delete(key);
    changed();
  };
  const cancel = () => {
    if (!c.text) comments.delete(key); else c.editing = false;
    renderComments();
  };
  ta.onkeydown = (e) => {
    if (e.key === 'Enter' && e.ctrlKey && !e.shiftKey) { e.preventDefault(); save(); }
    if (e.key === 'Escape') { e.preventDefault(); cancel(); }
  };
  const saveBtn = document.createElement('button');
  saveBtn.textContent = 'Save';
  saveBtn.onclick = save;
  const cancelBtn = document.createElement('button');
  cancelBtn.textContent = 'Cancel';
  cancelBtn.onclick = cancel;
  box.append(ta, saveBtn, cancelBtn);
  return box;
}

function review() {
  return { comments: [...comments.values()].filter((c) => c.text.trim()), overall: overall.value };
}

function changed() {
  renderComments();
  post({ type: 'state', review: review() });
}

function startComment(el) {
  commentOn(el, anchors.get(el.dataset.key).a);
}

/** Opens the comment box for anchor `a`, shown under `el`. */
function commentOn(el, a) {
  const key = keyOf(a);
  const c = comments.get(key) ?? { ...a, text: '', anchorKey: el.dataset.key };
  c.editing = true;
  comments.set(key, c);
  getSelection().removeAllRanges();
  selBtn.hidden = true;
  setFocus(el);
  renderComments();
}

/** Returns {el, a} for the lines the selection covers in one file, or null. */
function selectionAnchor() {
  const sel = getSelection();
  if (sel.isCollapsed || !sel.rangeCount) return null;
  const range = sel.getRangeAt(0);
  const keyed = (node) => (node.nodeType === 1 ? node : node.parentElement)?.closest('#content [data-key]');
  const first = keyed(range.startContainer);
  const last = keyed(range.endContainer);
  if (!first || !last || first.closest('.vjuu-c') || last.closest('.vjuu-c')) return null;
  const all = [...content.querySelectorAll('[data-key]')];
  const head = anchors.get(first.dataset.key).a;
  const span = all.slice(all.indexOf(first), all.indexOf(last) + 1)
    .map((el) => ({ el, a: anchors.get(el.dataset.key).a }))
    .filter((x) => x.a.doc === head.doc && x.a.file === head.file);
  if (!span.length) return null;
  const doc = req.docs[head.doc];
  if (doc.kind !== 'diff') {
    const start = Math.min(...span.map((x) => x.a.start));
    const end = Math.max(...span.map((x) => x.a.end));
    const quote = doc.text.split(/\r?\n/).slice(start - 1, end).join('\n');
    return { el: span.at(-1).el, a: { ...head, start, end, quote } };
  }
  // Diff: number by the new file unless only deleted lines are selected.
  const added = span.filter((x) => !x.a.old);
  const numbered = added.length ? added : span;
  const lines = numbered.map((x) => x.a.start);
  const a = { ...head, start: Math.min(...lines), end: Math.max(...lines), old: !added.length, quote: span.map((x) => x.a.quote).join('\n') };
  return { el: span.at(-1).el, a };
}

const selBtn = $('#sel-comment');
selBtn.addEventListener('mousedown', (e) => e.preventDefault()); // keep the selection
selBtn.onclick = () => {
  const s = selectionAnchor();
  if (s) commentOn(s.el, s.a);
};
document.addEventListener('mouseup', () => setTimeout(() => {
  const s = selectionAnchor();
  selBtn.hidden = !s;
  if (!s) return;
  const r = getSelection().getRangeAt(0).getBoundingClientRect();
  selBtn.style.top = `${r.bottom + window.scrollY + 6}px`;
  selBtn.style.left = `${Math.max(r.left + window.scrollX, 8)}px`;
}));
document.addEventListener('selectionchange', () => { if (getSelection().isCollapsed) selBtn.hidden = true; });

function setFocus(el) {
  focused?.classList.remove('focused');
  focused = el;
  el.classList.add('focused');
  el.scrollIntoView({ block: 'nearest' });
}

function move(list, delta) {
  const cur = list.indexOf(focused);
  // Nothing focused yet: start at the first element on screen.
  if (cur < 0) return list.find((el) => el.getBoundingClientRect().top >= 0) ?? list[0];
  return list[Math.min(Math.max(cur + delta, 0), list.length - 1)];
}

function done() {
  post({ type: 'state', review: review() });
  post({ type: 'done' });
}

content.addEventListener('mouseover', (e) => {
  const el = e.target.closest('[data-key]');
  if (!el) return;
  const num = el.querySelector('.d2h-code-linenumber, .d2h-code-side-linenumber, .code-ln');
  const r = el.getBoundingClientRect();
  addBtn.hidden = false;
  if (num) {
    const n = num.getBoundingClientRect();
    addBtn.style.top = `${n.top + window.scrollY + (n.height - 22) / 2}px`;
    addBtn.style.left = `${n.right - 11}px`;
  } else {
    addBtn.style.top = `${r.top + window.scrollY + 2}px`;
    addBtn.style.left = `${Math.max(r.left - 28, 0)}px`;
  }
  addBtn.onclick = () => startComment(el);
});

content.addEventListener('click', (e) => {
  if (e.target.closest('.vjuu-c')) return;
  const link = e.target.closest('a[href]');
  if (link) return;
  const el = e.target.closest('[data-key]');
  if (!el) return;
  // Clicking a diff line number comments right away, like GitLab and GitHub.
  if (e.target.closest('.d2h-code-linenumber, .d2h-code-side-linenumber, .code-ln')) startComment(el); else setFocus(el);
});

document.addEventListener('keydown', (e) => {
  if (e.key === 'Enter' && e.ctrlKey && e.shiftKey) { e.preventDefault(); done(); return; }
  if (e.target.closest('textarea') || e.ctrlKey || e.altKey || e.metaKey) return;
  const all = [...content.querySelectorAll('[data-key]')];
  const sections = [...content.querySelectorAll('.d2h-file-wrapper, .doc-header, .markdown > h1, .markdown > h2, .markdown > h3')];
  if (e.key === 'j' || e.key === 'k') {
    const el = move(all, e.key === 'j' ? 1 : -1);
    if (el) setFocus(el);
  } else if (e.key === 'n' || e.key === 'p') {
    const top = (el) => el.getBoundingClientRect().top;
    const target = e.key === 'n' ? sections.find((s) => top(s) > 2) : sections.findLast((s) => top(s) < -2);
    target?.scrollIntoView({ block: 'start' });
  } else if (e.key === 'c' && selectionAnchor()) {
    const s = selectionAnchor();
    commentOn(s.el, s.a);
  } else if ((e.key === 'c' || e.key === 'Enter') && focused) {
    startComment(focused);
  } else if (e.key === 'v' && hasDiff()) {
    toggleView();
  } else if (e.key === 'f') {
    filesHidden = !filesHidden;
    renderFileList();
  } else if (e.key === 't') {
    const names = [...$('#theme').options].map((o) => o.value);
    setTheme(names[(names.indexOf(theme) + 1) % names.length]);
  } else {
    return;
  }
  e.preventDefault();
});

function toggleView() {
  sideBySide = !sideBySide;
  $('#view').textContent = sideBySide ? 'Unified' : 'Side-by-side';
  render();
}

const osDark = matchMedia('(prefers-color-scheme: dark)');
let theme = 'auto';

function setTheme(name) {
  theme = name;
  document.documentElement.dataset.theme = name === 'auto' ? (osDark.matches ? 'monokai' : 'light') : name;
  $('#theme').value = name;
  // Every window shares one WebView2 profile, so the choice carries over; storage may be unavailable.
  try { localStorage.setItem('vjuu-theme', name); } catch {}
}

try { theme = localStorage.getItem('vjuu-theme') ?? 'auto'; } catch {}
setTheme(theme);
osDark.addEventListener('change', () => { if (theme === 'auto') setTheme('auto'); });
$('#theme').onchange = (e) => { setTheme(e.target.value); e.target.blur(); };
$('#view').onclick = toggleView;
$('#done').onclick = done;
overall.addEventListener('input', () => post({ type: 'state', review: review() }));

window.vjuuLoad = () => load().catch((err) => {
  content.textContent = `vjuu failed to render: ${err}`;
});
vjuuLoad();
