'use strict';
const $ = (s) => document.querySelector(s);
const content = $('#content');
const addBtn = $('#add');
const overall = $('#overall');

let doc;
let sideBySide = false;
let focused = null;
/** key -> {el, a}: every commentable element of the current render. */
let anchors = new Map();
/** key -> {file, start, end, old, quote, text, editing}. Survives re-renders. */
const comments = new Map();

const keyOf = (a) => `${a.file}\n${a.start}-${a.end}${a.old ? 'o' : ''}`;
const post = (msg) => window.ipc.postMessage(JSON.stringify(msg));

function loadScript(src) {
  return new Promise((resolve, reject) => {
    const s = document.createElement('script');
    s.src = src;
    s.onload = resolve;
    s.onerror = reject;
    document.head.append(s);
  });
}

/** Loads the document; a pre-warmed spare window gets 404 until revu hands it one and calls this again. */
async function load() {
  if (doc) return;
  // Resident windows share one browser profile; never reuse an earlier document.
  const r = await fetch('doc.json', { cache: 'no-store' });
  if (!r.ok) return;
  const d = await r.json();
  if (doc) return;
  doc = d;
  $('#name').textContent = doc.name;
  if (doc.kind === 'diff') {
    $('#view').hidden = false;
    await loadScript('vendor/diff2html-ui.min.js');
  } else {
    await loadScript('vendor/markdown-it.min.js');
  }
  render();
  if (doc.autoclose) requestAnimationFrame(() => post({ type: 'rendered' }));
}

function render() {
  anchors = new Map();
  focused = null;
  if (doc.kind === 'diff') renderDiff(); else renderMarkdown();
  renderComments();
}

function anchor(el, a) {
  el.dataset.key = keyOf(a);
  anchors.set(el.dataset.key, { el, a });
}

function renderMarkdown() {
  // html off: the document is agent output and must not run script next to window.ipc.
  const md = window.markdownit({ linkify: true });
  const lines = doc.text.split(/\r?\n/);
  const tokens = md.parse(doc.text, {});
  for (const t of tokens) {
    if (t.map && t.nesting !== -1) t.attrSet('data-map', t.map.join(','));
  }
  content.innerHTML = `<article class="markdown">${md.renderer.render(tokens, md.options, {})}</article>`;
  for (const el of content.querySelectorAll('[data-map]')) {
    const [from, to] = el.dataset.map.split(',').map(Number);
    let end = to;
    while (end > from + 1 && !lines[end - 1].trim()) end--;
    anchor(el, { file: doc.name, start: from + 1, end, old: false, quote: lines.slice(from, end).join('\n') });
  }
}

function renderDiff() {
  const ui = new Diff2HtmlUI(content, doc.text, {
    outputFormat: sideBySide ? 'side-by-side' : 'line-by-line',
    drawFileList: false,
    matching: 'lines',
    highlight: true,
    fileContentToggle: false,
    colorScheme: 'auto',
  });
  ui.draw();
  // Highlighting a large diff takes hundreds of ms; paint first, colour after.
  requestAnimationFrame(() => setTimeout(() => ui.highlightCode()));
  for (const wrapper of content.querySelectorAll('.d2h-file-wrapper')) {
    const file = wrapper.querySelector('.d2h-file-name').textContent.split(' → ').pop().trim();
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
        anchor(tr, { file, start: line, end: line, old: !n2, quote: lineText(tr) });
      }
      continue;
    }
    wrapper.querySelectorAll('.d2h-file-side-diff').forEach((side, i) => {
      side.querySelectorAll('tbody tr').forEach((tr, idx) => {
        tr.dataset.idx = idx;
        const n = tr.querySelector('.d2h-code-side-linenumber')?.textContent.trim();
        // Left side: only deleted lines; unchanged lines are commented on the right.
        if (!n || (i === 0 && !tr.cells[1].classList.contains('d2h-del'))) return;
        anchor(tr, { file, start: Number(n), end: Number(n), old: i === 0, quote: lineText(tr) });
      });
    });
  }
}

function renderComments() {
  content.querySelectorAll('.revu-c').forEach((e) => e.remove());
  $('#orphans')?.remove();
  const orphans = [];
  for (const [key, c] of comments) {
    const box = commentBox(key, c);
    const entry = anchors.get(key);
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
  content.querySelector('.revu-c textarea, #orphans textarea')?.focus();
}

function place(el, box) {
  if (el.tagName !== 'TR') {
    box.classList.add('revu-c');
    if (el.tagName === 'LI') el.append(box); else el.after(box);
    return;
  }
  const row = document.createElement('tr');
  row.className = 'revu-c';
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
  spacer.className = 'revu-c spacer';
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
  const key = el.dataset.key;
  const c = comments.get(key) ?? { ...anchors.get(key).a, text: '' };
  c.editing = true;
  comments.set(key, c);
  setFocus(el);
  renderComments();
}

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
  const r = el.getBoundingClientRect();
  addBtn.hidden = false;
  addBtn.style.top = `${r.top + window.scrollY}px`;
  addBtn.style.left = `${Math.max(r.left - 26, 0)}px`;
  addBtn.onclick = () => startComment(el);
});

content.addEventListener('click', (e) => {
  if (e.target.closest('.revu-c')) return;
  const link = e.target.closest('a[href]');
  if (link) return;
  const el = e.target.closest('[data-key]');
  if (!el) return;
  // Clicking a diff line number comments right away, like GitLab and GitHub.
  if (e.target.closest('.d2h-code-linenumber, .d2h-code-side-linenumber')) startComment(el); else setFocus(el);
});

document.addEventListener('keydown', (e) => {
  if (e.key === 'Enter' && e.ctrlKey && e.shiftKey) { e.preventDefault(); done(); return; }
  if (e.target.closest('textarea') || e.ctrlKey || e.altKey || e.metaKey) return;
  const all = [...content.querySelectorAll('[data-key]')];
  const sections = [...content.querySelectorAll('.d2h-file-wrapper, .markdown > h1, .markdown > h2, .markdown > h3')];
  if (e.key === 'j' || e.key === 'k') {
    const el = move(all, e.key === 'j' ? 1 : -1);
    if (el) setFocus(el);
  } else if (e.key === 'n' || e.key === 'p') {
    const top = (el) => el.getBoundingClientRect().top;
    const target = e.key === 'n' ? sections.find((s) => top(s) > 2) : sections.findLast((s) => top(s) < -2);
    target?.scrollIntoView({ block: 'start' });
  } else if ((e.key === 'c' || e.key === 'Enter') && focused) {
    e.preventDefault();
    startComment(focused);
  } else if (e.key === 'v' && doc.kind === 'diff') {
    toggleView();
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

$('#view').onclick = toggleView;
$('#done').onclick = done;
overall.addEventListener('input', () => post({ type: 'state', review: review() }));

window.revuLoad = () => load().catch((err) => {
  content.textContent = `revu failed to render: ${err}`;
});
revuLoad();
