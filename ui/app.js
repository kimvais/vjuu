'use strict';
const $ = (s) => document.querySelector(s);
const content = $('#content');
const overall = $('#overall');
/** Content scrolls here, below the title bar, as in Windows Terminal. */
const scroller = $('#scroll');

/** {docs: [{name, kind, text}], autoclose, session} */
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
/** key -> {question, answer}: the ```choice blocks of the markdown documents, in order. Survives re-renders. */
const choices = new Map();

// The doc index keeps two documents with the same name apart, and part (the file's position in a diff) the same
// file shown twice in one diff, as in `git log -p`.
const keyOf = (a) => `${a.doc}\n${a.part ?? ''}\n${a.file}\n${a.start}-${a.end}${a.old ? 'o' : ''}`;
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
  $('#session').textContent = req.session ?? '';
  $('#name').textContent = req.docs.map((x) => x.name).join(', ');
  $('#view').hidden = !hasDiff();
  const renderers = new Set(req.docs.map((x) => ({ diff: RENDERERS[2], md: RENDERERS[1] })[x.kind]).filter(Boolean));
  await Promise.all([loadHighlight(), ...[...renderers].map(loadScript)]);
  render();
  requestAnimationFrame(() => post({ type: req.autoclose ? 'rendered' : 'ready' }));
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
    } else if (doc.kind === 'html') {
      renderHtml(section, doc, i);
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

/** querySelectorAll over the content and the HTML pages framed in it, in document order. */
const deep = (sel) => [...content.querySelectorAll(`${sel}, iframe`)]
  .flatMap((e) => (e.tagName === 'IFRAME' ? [...(e.contentDocument?.querySelectorAll(sel) ?? [])] : [e]));
const frameDocs = () => [...content.querySelectorAll('iframe')].map((f) => f.contentDocument).filter(Boolean);

/** Elements of an HTML page that take a comment; hovering picks the innermost. */
const HTML_BLOCKS = 'h1, h2, h3, h4, h5, h6, p, li, pre, blockquote, figcaption, dt, dd, th, td, img, summary, label, button';
/** vjuu's own styles inside a framed page; the theme's colours are copied in by syncFrameTheme(). */
const FRAME_CSS = `
[data-key].hovered { cursor: pointer; background: var(--focus); border-radius: 4px; }
[data-key].focused { background: var(--focus) !important; outline: 1px solid var(--accent); }
.vjuu-c.comment { box-sizing: border-box; margin: 6px 0; padding: 8px 10px; display: flex; gap: 8px; align-items: flex-start;
  flex-wrap: wrap; background: var(--comment-bg); color: var(--fg); border: 1px solid var(--border); border-radius: 6px;
  font: 14px/1.5 var(--sans); white-space: normal; text-align: left; }
.vjuu-c .text { flex: 1; white-space: pre-wrap; cursor: text; }
.vjuu-c textarea { box-sizing: border-box; width: 100%; font: inherit; color: inherit; background: var(--bg);
  border: 1px solid var(--border); border-radius: 4px; padding: 6px; }
.vjuu-c button { font: inherit; color: inherit; background: var(--bg); border: 1px solid var(--border); border-radius: 6px;
  padding: 2px 10px; cursor: pointer; }`;
const FRAME_VARS = ['--bg', '--fg', '--border', '--comment-bg', '--focus', '--accent', '--sans'];

/** Appended to a page with scripts: once the page settles, posts back a copy of it as rendered, without scripts. */
const SNAPSHOT_SCRIPT = `<script>(() => {
  let sent = false;
  let timer;
  const rules = (sheet) => [...sheet.cssRules].map((r) => r.cssText).join('\\n');
  const send = () => {
    if (sent) return;
    sent = true;
    for (const c of document.querySelectorAll('canvas')) {
      try {
        const img = document.createElement('img');
        img.src = c.toDataURL();
        img.alt = c.getAttribute('aria-label') || 'chart';
        img.className = c.className;
        img.style.cssText = c.style.cssText;
        img.width = c.clientWidth;
        img.height = c.clientHeight;
        c.replaceWith(img);
      } catch {} // a canvas with images from elsewhere cannot be read
    }
    // Rules inserted through the CSSOM (CSS-in-JS) are not in the markup. Only empty style tags are rebuilt: the
    // CSSOM loses shorthands that use var(), such as font: 500 56px var(--mono).
    for (const s of document.querySelectorAll('style')) {
      if (!s.textContent.trim()) s.textContent = rules(s.sheet);
    }
    for (const sheet of document.adoptedStyleSheets ?? []) {
      const s = document.createElement('style');
      s.textContent = rules(sheet);
      document.head.append(s);
    }
    // Values typed or set by script live in properties, not attributes.
    for (const el of document.querySelectorAll('input, textarea, select')) {
      if (el.type === 'checkbox' || el.type === 'radio') el.toggleAttribute('checked', el.checked);
      else if (el.tagName === 'TEXTAREA') el.textContent = el.value;
      else if (el.tagName === 'SELECT') for (const o of el.options) o.toggleAttribute('selected', o.selected);
      else el.setAttribute('value', el.value);
    }
    // The copy is shown with scripts off, where <noscript> would appear.
    document.querySelectorAll('script, noscript').forEach((e) => e.remove());
    const doctype = document.compatMode === 'BackCompat' ? '' : '<!doctype html>';
    parent.postMessage({ vjuuSnapshot: doctype + document.documentElement.outerHTML }, '*');
  };
  addEventListener('load', () => {
    // Settled: nothing changed for 300 ms. A clock never settles; it is taken as it is after 3 s.
    new MutationObserver(() => { clearTimeout(timer); timer = setTimeout(send, 300); })
      .observe(document, { subtree: true, childList: true, attributes: true, characterData: true });
    timer = setTimeout(send, 300);
    setTimeout(send, 3000);
  });
})();</script>`;

/**
 * Shows an HTML page. One with scripts (most claude.ai artifacts build their content in script) runs first in a frame
 * of its own origin, where it cannot reach vjuu's page or window.ipc; vjuu then shows the copy it sends back.
 */
function renderHtml(section, doc, i) {
  const header = document.createElement('div');
  header.className = 'doc-header';
  header.textContent = doc.name;
  section.append(header);
  targets.push({ el: section, doc: i, file: doc.name });
  // Relative stylesheets, scripts and images resolve against the file's folder, served as files/<i>/. After the
  // doctype, which must stay first or the page renders in quirks mode.
  const base = `<base href="${new URL(`files/${i}/`, location.href)}">`;
  const html = doc.text.replace(/^(\s*<!doctype[^>]*>)?/i, `$1${base}`);
  if (!/<script/i.test(doc.text)) {
    showPage(section, html, doc, i);
    return;
  }
  // Shown where the copy will go, not hidden: browsers throttle frames out of view, which stalls drawing.
  const runner = document.createElement('iframe');
  runner.className = 'html-page';
  runner.sandbox = 'allow-scripts';
  runner.srcdoc = html + SNAPSHOT_SCRIPT;
  let shown = false;
  const show = (page) => {
    if (shown) return;
    shown = true;
    removeEventListener('message', onMessage);
    runner.remove();
    showPage(section, page, doc, i);
  };
  const onMessage = (e) => {
    if (e.source === runner.contentWindow && typeof e.data?.vjuuSnapshot === 'string') show(e.data.vjuuSnapshot);
  };
  addEventListener('message', onMessage);
  // A page that never loads (a stalled CDN) or hangs is shown as written.
  setTimeout(() => show(html), 8000);
  section.append(runner);
}

function showPage(section, html, doc, i) {
  const frame = document.createElement('iframe');
  frame.className = 'html-page';
  // Same origin, so vjuu can add its comment layer; no allow-scripts, so nothing in the page (on* handlers, or
  // scripts if it is shown as written) runs where it could reach window.ipc. It also blocks forms and popups.
  frame.sandbox = 'allow-same-origin';
  frame.srcdoc = html;
  frame.onload = () => {
    wireFrame(frame.contentDocument, doc, i);
    renderComments();
  };
  section.append(frame);
}

function wireFrame(d, doc, i) {
  const style = d.createElement('style');
  style.textContent = FRAME_CSS;
  d.head.append(style);
  syncFrameTheme(d);
  const headings = [...d.querySelectorAll('h1, h2, h3, h4, h5, h6')];
  // A page has no line numbers: the agent finds the quote in the source, with an id or heading to tell repeats apart.
  const at = (el) => {
    const id = el.closest('[id]:not(html, body)')?.id;
    if (id) return `#${id}`;
    const h = headings.findLast((h) => h !== el && h.compareDocumentPosition(el) & Node.DOCUMENT_POSITION_FOLLOWING);
    return h ? `under "${h.textContent.trim()}"` : '';
  };
  d.querySelectorAll(HTML_BLOCKS).forEach((el, n) => {
    // A drawn chart is a data: URL hundreds of KB long.
    const src = el.getAttribute('src') ?? '';
    const img = `<img src="${src.startsWith('data:') ? 'data:…' : src}" alt="${el.alt}">`;
    const quote = el.tagName === 'IMG' ? img : el.innerText.trim();
    anchor(el, { doc: i, part: n, file: doc.name, start: 0, end: 0, old: false, quote, at: at(el) });
  });
  d.addEventListener('mouseover', onHover);
  d.documentElement.addEventListener('mouseleave', onLeave);
  d.addEventListener('click', onClick);
  d.addEventListener('keydown', onKey);
  d.addEventListener('mouseup', onMouseUp);
  d.addEventListener('selectionchange', onSelectionChange);
}

function syncFrameTheme(d) {
  const style = getComputedStyle(document.documentElement);
  for (const v of FRAME_VARS) d.documentElement.style.setProperty(v, style.getPropertyValue(v));
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
  const fence = md.renderer.rules.fence;
  md.renderer.rules.fence = (tokens, idx, ...rest) =>
    tokens[idx].info.trim() === 'choice' ? choiceHtml(tokens[idx]) : fence(tokens, idx, ...rest);
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
  for (const el of article.querySelectorAll('.choice')) {
    el.dataset.choice = `${i}\n${el.dataset.map}`;
    const c = choices.get(el.dataset.choice) ?? { question: el.querySelector('.question').textContent, answer: '' };
    choices.set(el.dataset.choice, c);
    showPick(el, c);
  }
  for (const el of article.querySelectorAll('[data-map]')) {
    const [from, to] = el.dataset.map.split(',').map(Number);
    let end = to;
    while (end > from + 1 && !lines[end - 1].trim()) end--;
    anchor(el, { doc: i, file: doc.name, start: from + 1, end, old: false, quote: lines.slice(from, end).join('\n') });
  }
}

const OPTION = /^(?:[-*]|\d+[.):])\s+(.*)$/;

/** Renders a ```choice block: its list items ("- ", "1. ", "2:") are the options, the other lines the question. */
function choiceHtml(t) {
  // Agent output, rendered with html off elsewhere: escape quotes too, as text goes into attributes.
  const esc = (s) => escapeHtml(s).replace(/"/g, '&quot;');
  const lines = t.content.split('\n').map((l) => l.trim()).filter(Boolean);
  const question = lines.filter((l) => !OPTION.test(l)).join(' ');
  const buttons = lines.map((l) => l.match(OPTION)?.[1]).filter(Boolean).map((option) => {
    // "Label: why"; the label is what goes back with the review.
    const [label, ...why] = option.split(/:\s+/);
    const detail = why.length ? `<span>${esc(why.join(': '))}</span>` : '';
    return `<button data-label="${esc(label)}"><b>${esc(label)}</b>${detail}</button>`;
  });
  return `<div class="choice" data-map="${t.map.join(',')}"><p class="question">${esc(question)}</p>${buttons.join('')}</div>`;
}

function showPick(el, c) {
  for (const b of el.querySelectorAll('button')) b.classList.toggle('picked', b.dataset.label === c.answer);
}

/** Picks an option of a choice block; picking it again takes the pick back. */
function pick(button) {
  const el = button.closest('.choice');
  const c = choices.get(el.dataset.choice);
  c.answer = c.answer === button.dataset.label ? '' : button.dataset.label;
  showPick(el, c);
  post({ type: 'state', review: review() });
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
  const wrap = document.createElement('div');
  wrap.className = 'code-scroll';
  wrap.append(table);
  section.append(header, wrap);
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
  for (const [part, wrapper] of section.querySelectorAll('.d2h-file-wrapper').entries()) {
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
        anchor(tr, { doc: i, part, file, start: line, end: line, old: !n2, quote: lineText(tr) });
      }
      continue;
    }
    wrapper.querySelectorAll('.d2h-file-side-diff').forEach((side, sideIdx) => {
      side.querySelectorAll('tbody tr').forEach((tr, idx) => {
        tr.dataset.idx = idx;
        const n = tr.querySelector('.d2h-code-side-linenumber')?.textContent.trim();
        // Left side: only deleted lines; unchanged lines are commented on the right.
        if (!n || (sideIdx === 0 && !tr.cells[1].classList.contains('d2h-del'))) return;
        anchor(tr, { doc: i, part, file, start: Number(n), end: Number(n), old: sideIdx === 0, quote: lineText(tr) });
      });
    });
  }
}

function renderComments() {
  deep('.vjuu-c').forEach((e) => e.remove());
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
  deep('.vjuu-c textarea, #orphans textarea')[0]?.focus();
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
  const pad = (el, depth) => { el.style.paddingLeft = `${12 + depth * 14}px`; };
  // A directory tree, like GitLab's: a folder row wherever a file's directory parts differ from the previous
  // file's. Files keep document order, which git already sorts by path.
  const rows = [];
  let dirs = [];
  let depth = 0;
  for (const t of targets) {
    const a = document.createElement('a');
    a.onclick = () => t.el.scrollIntoView({ block: 'start' });
    if (t.heading) {
      // Markdown: a table of contents, indented by heading level, under its file when there are several.
      a.className = 'toc';
      a.title = t.heading;
      pad(a, t.level - 1 + (req.docs.length > 1 ? depth + 1 : 0));
      a.append(span('heading', t.heading));
      rows.push(a);
      continue;
    }
    const parts = t.file.split(/[\\/]/);
    const base = parts.pop();
    let same = 0;
    while (same < parts.length && parts[same] === dirs[same]) same++;
    for (let k = same; k < parts.length; k++) {
      const folder = document.createElement('div');
      folder.className = 'folder';
      folder.title = parts.slice(0, k + 1).join('/');
      pad(folder, k);
      folder.append(span('name', parts[k]));
      rows.push(folder);
    }
    dirs = parts;
    depth = parts.length;
    a.title = t.file;
    pad(a, depth);
    a.append(span('base', base));
    if (t.add !== undefined) a.append(span('add', `+${t.add}`), span('del', `−${t.del}`));
    const n = perFile.get(`${t.doc}\n${t.file}`);
    if (n) a.append(span('badge', n));
    rows.push(a);
  }
  nav.replaceChildren(...rows);
  markActiveFile();
}

/** Highlights the file at the top of the view. */
/** Returns the index in `list` of the file or heading at the top of the view. */
function activeTarget(list) {
  const top = scroller.getBoundingClientRect().top + 8;
  let active = 0;
  list.forEach((t, i) => { if (t.el.getBoundingClientRect().top <= top) active = i; });
  // A short last file never reaches the top; at the bottom of the page it is the current one.
  if (scroller.scrollTop + scroller.clientHeight >= scroller.scrollHeight - 2) active = list.length - 1;
  return active;
}

function markActiveFile() {
  document.documentElement.style.setProperty('--header-h', `${$('header').offsetHeight}px`);
  const nav = $('#files');
  if (nav.hidden) return;
  const active = activeTarget(targets);
  // One link per target; folder rows are not links.
  const links = nav.querySelectorAll('a');
  links.forEach((a, i) => a.classList.toggle('active', i === active));
  links[active]?.scrollIntoView({ block: 'nearest' });
}

let scrollQueued = false;
scroller.addEventListener('scroll', () => {
  if (scrollQueued) return;
  scrollQueued = true;
  requestAnimationFrame(() => { scrollQueued = false; markActiveFile(); });
});

function place(el, box) {
  if (el.tagName !== 'TR') {
    box.classList.add('vjuu-c');
    // A div between list items or table cells breaks the list or table.
    if (['LI', 'TD', 'TH'].includes(el.tagName)) el.append(box); else el.after(box);
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
  ta.value = c.draft ?? c.text;
  ta.rows = 3;
  ta.placeholder = `Comment on ${c.file}:${c.start}${c.end !== c.start ? `-${c.end}` : ''} (Ctrl+Enter to save, Esc to cancel)`;
  // Text still being typed counts too: Done or closing the window must not drop it.
  ta.oninput = () => {
    c.draft = ta.value;
    post({ type: 'state', review: review() });
  };
  const save = () => {
    c.text = ta.value.trim();
    c.editing = false;
    delete c.draft;
    if (!c.text) comments.delete(key);
    changed();
  };
  const cancel = () => {
    delete c.draft;
    if (!c.text) comments.delete(key); else c.editing = false;
    changed();
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

/** Returns the review as sent to vjuu: saved comments, plus what is typed in any comment still open. */
function review() {
  const sent = [...comments.values()].map(({ draft, ...c }) => ({ ...c, text: (c.editing ? draft ?? c.text : c.text).trim() }));
  const r = { comments: sent.filter((c) => c.text), overall: overall.value };
  if (choices.size) r.choices = [...choices.values()];
  return r;
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
  frameDocs().forEach((d) => d.getSelection()?.removeAllRanges());
  selBtn.hidden = true;
  setFocus(el);
  renderComments();
}

/** Returns {el, a} for the lines the selection covers in one file, or for text selected on an HTML page; or null. */
function selectionAnchor() {
  for (const d of frameDocs()) {
    const sel = d.getSelection();
    if (!sel || sel.isCollapsed || !sel.rangeCount) continue;
    const node = sel.getRangeAt(0).endContainer;
    const el = (node.nodeType === 1 ? node : node.parentElement)?.closest('[data-key]');
    if (!el || el.closest('.vjuu-c')) return null;
    const head = anchors.get(el.dataset.key).a;
    const quote = sel.toString().trim();
    // The quoted text, not the block, is what the comment is on: a separate comment from one on the whole block.
    return { el, a: { ...head, part: `${head.part} ${quote}`, quote } };
  }
  const sel = getSelection();
  if (sel.isCollapsed || !sel.rangeCount) return null;
  const range = sel.getRangeAt(0);
  const keyed = (node) => (node.nodeType === 1 ? node : node.parentElement)?.closest('#content [data-key]');
  const first = keyed(range.startContainer);
  const last = keyed(range.endContainer);
  if (!first || !last || first.closest('.vjuu-c') || last.closest('.vjuu-c')) return null;
  const all = deep('[data-key]');
  const head = anchors.get(first.dataset.key).a;
  const span = all.slice(all.indexOf(first), all.indexOf(last) + 1)
    .map((el) => ({ el, a: anchors.get(el.dataset.key).a }))
    .filter((x) => x.a.doc === head.doc && x.a.part === head.part && x.a.file === head.file);
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
function onMouseUp(e) {
  const d = e.target.ownerDocument ?? e.target;
  setTimeout(() => {
    const s = selectionAnchor();
    selBtn.hidden = !s;
    if (!s) return;
    const r = d.getSelection().getRangeAt(0).getBoundingClientRect();
    // Text on a framed page is measured in the frame; the button lives in the scrolling area, so position it in that
    // area's coordinates.
    const frame = d.defaultView.frameElement?.getBoundingClientRect() ?? { left: 0, top: 0 };
    const area = scroller.getBoundingClientRect();
    selBtn.style.top = `${r.bottom + frame.top - area.top + scroller.scrollTop + 6}px`;
    selBtn.style.left = `${Math.max(r.left + frame.left - area.left + scroller.scrollLeft, 8)}px`;
  });
}
function onSelectionChange(e) {
  // Fired on the document, and on a textarea for selections within it.
  if ((e.target.ownerDocument ?? e.target).getSelection().isCollapsed) selBtn.hidden = true;
}
document.addEventListener('mouseup', onMouseUp);
document.addEventListener('selectionchange', onSelectionChange);

function setFocus(el) {
  focused?.classList.remove('focused');
  focused = el;
  el.classList.add('focused');
  el.scrollIntoView({ block: 'nearest' });
}

function move(list, delta) {
  const cur = list.indexOf(focused);
  // Nothing focused yet: start at the first element on screen.
  if (cur < 0) return list.find((el) => el.getBoundingClientRect().top >= scroller.getBoundingClientRect().top) ?? list[0];
  return list[Math.min(Math.max(cur + delta, 0), list.length - 1)];
}

function done() {
  post({ type: 'state', review: review() });
  post({ type: 'done' });
}

// Highlight only the innermost line or block: that is what a click comments on.
let hovered = null;
function onHover(e) {
  const el = e.target.closest('.vjuu-c') ? null : e.target.closest('[data-key]');
  if (el === hovered) return;
  hovered?.classList.remove('hovered');
  hovered = el;
  el?.classList.add('hovered');
}
function onLeave() {
  hovered?.classList.remove('hovered');
  hovered = null;
}
content.addEventListener('mouseover', onHover);
content.addEventListener('mouseleave', onLeave);

// Clicking content has no other purpose than commenting, so any click on a line or block starts one.
function onClick(e) {
  const option = e.target.closest('.choice button');
  if (option) {
    pick(option);
    return;
  }
  const link = e.target.closest('a[href]');
  // A link on a framed page would load in the frame and lose the comment layer; in-page ones still scroll.
  if (link && link.ownerDocument !== document) {
    e.preventDefault();
    const href = link.getAttribute('href');
    if (href.startsWith('#')) link.ownerDocument.getElementById(decodeURIComponent(href.slice(1)))?.scrollIntoView();
  }
  if (e.target.closest('.vjuu-c, a[href]')) return;
  // A drag that selected text is a range comment: the Comment button handles it.
  if (!e.target.ownerDocument.getSelection().isCollapsed) return;
  const el = e.target.closest('[data-key]');
  if (el) startComment(el);
}
content.addEventListener('click', onClick);

/** Moves to the previous or next file, or between headings when there is only one file; lands on its first line. */
function jumpFile(delta) {
  const files = targets.filter((t) => !t.heading);
  const list = files.length > 1 ? files : targets.filter((t) => t.heading);
  if (!list.length) return;
  // Start from the cursor when there is one, so the keys follow it rather than the scroll position.
  // On a framed page, the frame stands in for the cursor: positions in two documents do not compare.
  const at = focused?.ownerDocument.defaultView.frameElement ?? focused;
  const atOrBefore = (t) => t.el === at
    || t.el.compareDocumentPosition(at) & (Node.DOCUMENT_POSITION_FOLLOWING | Node.DOCUMENT_POSITION_CONTAINED_BY);
  const cur = focused ? list.findLastIndex(atOrBefore) : activeTarget(list);
  const next = list[Math.min(Math.max(cur + delta, 0), list.length - 1)];
  next.el.scrollIntoView({ block: 'start' });
  const first = next.el.matches('[data-key]')
    ? next.el
    : next.el.querySelector('[data-key]') ?? next.el.querySelector('iframe')?.contentDocument?.querySelector('[data-key]');
  if (first) setFocus(first);
}

function onKey(e) {
  const inField = e.target.closest('textarea, input, select');
  // Ctrl+Enter saves the comment being written (its own handler); anywhere else it finishes the review.
  if (e.key === 'Enter' && e.ctrlKey && (e.shiftKey || !inField || e.target.id === 'overall')) {
    e.preventDefault();
    done();
    return;
  }
  if (inField || e.ctrlKey || e.altKey || e.metaKey) return;
  const all = deep('[data-key]');
  // Arrows and the vim-style letters do the same thing.
  const key = { ArrowDown: 'j', ArrowUp: 'k', ArrowRight: 'n', ArrowLeft: 'p' }[e.key] ?? e.key;
  if (key === 'j' || key === 'k') {
    const el = move(all, key === 'j' ? 1 : -1);
    if (el) setFocus(el);
  } else if (key === 'n' || key === 'p') {
    jumpFile(key === 'n' ? 1 : -1);
  } else if (key === 'c' && selectionAnchor()) {
    const s = selectionAnchor();
    commentOn(s.el, s.a);
  } else if (key === 'c' || key === 'Enter') {
    const el = focused ?? move(all, 0);
    if (el) startComment(el);
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
}
document.addEventListener('keydown', onKey);

function toggleView() {
  sideBySide = !sideBySide;
  $('#view').textContent = sideBySide ? 'Unified' : 'Side-by-side';
  render();
}

const osDark = matchMedia('(prefers-color-scheme: dark)');
let theme = 'auto';

function setTheme(name) {
  theme = name;
  document.documentElement.dataset.theme = name === 'auto' ? (osDark.matches ? 'graphite' : 'light') : name;
  $('#theme').value = name;
  frameDocs().forEach(syncFrameTheme);
  // Every window shares one WebView2 profile, so the choice carries over; storage may be unavailable.
  try { localStorage.setItem('vjuu-theme', name); } catch {}
}

try { theme = localStorage.getItem('vjuu-theme') ?? 'auto'; } catch {}
// Monokai was folded into Graphite; anything unknown falls back to Auto.
if (theme === 'monokai') theme = 'graphite';
if (![...$('#theme').options].some((o) => o.value === theme)) theme = 'auto';
setTheme(theme);
osDark.addEventListener('change', () => { if (theme === 'auto') setTheme('auto'); });
$('#theme').onchange = (e) => { setTheme(e.target.value); e.target.blur(); };
$('#view').onclick = toggleView;
$('#done').onclick = done;
overall.addEventListener('input', () => post({ type: 'state', review: review() }));

// Window chrome. On Windows the window is frameless, so the title bar and edges ask vjuu to move, resize or close it;
// macOS and Linux keep the native frame, and these controls are hidden.
if (!window.VJUU_FRAMELESS) document.body.classList.add('native-frame');
const titlebar = $('#titlebar');
const isMaximized = () => window.outerWidth >= screen.availWidth && window.outerHeight >= screen.availHeight;
titlebar.addEventListener('mousedown', (e) => {
  if (e.button !== 0 || e.target.closest('button, select, input')) return;
  if (e.detail === 2) post({ type: 'maximize' }); else post({ type: 'drag' });
});
for (const btn of document.querySelectorAll('#window-controls button')) {
  btn.onclick = () => (btn.dataset.cmd === 'close' ? done() : post({ type: btn.dataset.cmd }));
}
for (const edge of document.querySelectorAll('.edge')) {
  edge.addEventListener('mousedown', (e) => {
    if (e.button === 0) post({ type: 'resize', dir: edge.dataset.dir });
  });
}
function syncMaximized() {
  const max = isMaximized();
  document.body.classList.toggle('maximized', max);
  $('#max').innerHTML = max ? '&#xE923;' : '&#xE922;'; // Segoe Fluent: restore / maximize
  $('#max').title = max ? 'Restore' : 'Maximize';
}
window.addEventListener('resize', syncMaximized);
syncMaximized();

window.vjuuLoad = () => load().catch((err) => {
  content.textContent = `vjuu failed to render: ${err}`;
});
vjuuLoad();
