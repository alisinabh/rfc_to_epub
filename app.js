// Client-side search over every RFC. No backend, no dependencies.
//
// rfcs.json is an array of rows, newest RFC first:
//     [ number, title, status, date, hasEpub, hasTextSource ]
// meta.json carries the run summary: generated, total, withEpub, latest,
// added (numbers converted in the most recent run), epubBase, zipUrl.

'use strict';

const N = 0, TITLE = 1, STATUS = 2, DATE = 3, HAS_EPUB = 4, HAS_TEXT = 5;

// A 9,800-row DOM is the one thing that would make this page feel slow.
const MAX_ROWS = 200;

let rows = [];
let lowerTitles = [];
let meta = {};
let added = new Set();

const $ = (id) => document.getElementById(id);

// One release per 500 RFCs. Must stay in lockstep with BUCKET_SIZE in
// scripts/site.py — the script names the release, this rebuilds the URL.
const BUCKET_SIZE = 500;

function epubUrl(number) {
  const bucket = String(Math.floor(number / BUCKET_SIZE) * BUCKET_SIZE).padStart(5, '0');
  return `${meta.epubBase || 'epub/'}${bucket}/rfc${number}.epub`;
}

function escapeHtml(s) {
  return s.replace(/[&<>"]/g, (c) => ({ '&': '&amp;', '<': '&lt;', '>': '&gt;', '"': '&quot;' })[c]);
}

function matches(i, terms) {
  const title = lowerTitles[i];
  const number = String(rows[i][N]);
  for (const term of terms) {
    // A run of digits matches the RFC number by prefix as well as the title, so
    // "911" finds RFC 9110 and "http" finds it by name.
    const hit = title.includes(term) || (/^\d+$/.test(term) && number.startsWith(term));
    if (!hit) return false;
  }
  return true;
}

function rank(number, digits) {
  const s = String(number);
  if (s === digits) return 0;
  if (s.startsWith(digits)) return 1;
  return 2;
}

function render() {
  const query = $('q').value.trim().toLowerCase();
  const terms = query ? query.split(/\s+/) : [];

  const hits = [];
  for (let i = 0; i < rows.length; i++) {
    if (!terms.length || matches(i, terms)) {
      hits.push(rows[i]);
      // Stop early only when nothing is filtered: an unfiltered view is already
      // sorted newest-first, so the first MAX_ROWS are the ones we want.
      if (!terms.length && hits.length >= MAX_ROWS) break;
    }
  }

  // A bare number should surface that RFC itself, not an RFC whose title
  // happens to cite it: "1149" must lead with RFC 1149, not RFC 6214.
  if (terms.length === 1 && /^\d+$/.test(terms[0])) {
    const q = terms[0];
    hits.sort((a, b) => rank(a[N], q) - rank(b[N], q));
  }

  const shown = hits.slice(0, MAX_ROWS);
  $('results').innerHTML = shown.map(rowHtml).join('');

  const total = terms.length ? hits.length : rows.length;
  if (!total) {
    $('count').textContent = `No RFC matches “${query}”.`;
  } else if (total > shown.length) {
    $('count').textContent =
      `Showing ${shown.length} of ${total.toLocaleString()}` +
      (terms.length ? ' matches — keep typing to narrow it down.'
                    : ' RFCs, newest first — search to find older ones.');
  } else {
    $('count').textContent = `${total.toLocaleString()} ${total === 1 ? 'match' : 'matches'}.`;
  }
}

function rowHtml(row) {
  const n = row[N];
  const badge = added.has(n) ? '<span class="badge">new</span>' : '';
  const meta_ = [row[STATUS], row[DATE]].filter(Boolean).map(escapeHtml).join(' · ');
  let link;
  if (row[HAS_EPUB]) {
    link = `<a class="dl" href="${epubUrl(n)}" download>EPUB</a>`;
  } else if (row[HAS_TEXT]) {
    link = '<span class="none">not converted yet</span>';
  } else {
    link = '<span class="none">PDF only</span>';
  }
  return (
    '<li><span>' +
    `<span class="num">RFC ${n}</span>${badge} ${escapeHtml(row[TITLE])}` +
    `<br><span class="meta">${meta_}</span>` +
    `</span>${link}</li>`
  );
}

function renderHeader() {
  const parts = [
    `${(meta.withEpub || 0).toLocaleString()} of ${(meta.total || 0).toLocaleString()} RFCs available`,
    `updated ${(meta.generated || '').slice(0, 10)}`,
  ];
  if (added.size) {
    parts.push(`${added.size.toLocaleString()} added in the latest run`);
  }
  $('stats').textContent = parts.join(' · ');

  if (meta.zipUrl) {
    const zip = $('zip');
    zip.href = meta.zipUrl;
    zip.hidden = false;
  }
}

async function boot() {
  try {
    const [r, m] = await Promise.all([
      fetch('rfcs.json').then((res) => res.json()),
      fetch('meta.json').then((res) => res.json()),
    ]);
    rows = r;
    meta = m;
    added = new Set(m.added || []);
    lowerTitles = rows.map((row) => row[TITLE].toLowerCase());
  } catch (err) {
    $('stats').textContent = 'Could not load the RFC index.';
    $('count').textContent = String(err);
    return;
  }

  renderHeader();
  render();
  $('q').addEventListener('input', render);
}

boot();
