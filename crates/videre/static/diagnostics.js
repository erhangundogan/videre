// The Diagnostics page: what `videre status`, `videre stats` and the command
// logs say, each read from its own endpoint below. Read-only. Status
// refreshes every 10 s while watch runs; Logs keeps its filters in the URL so
// a link reproduces the view.
(function () {
  'use strict';
  var root = document.getElementById('diag');
  if (!root) return;
  var tab = root.getAttribute('data-tab');
  var REFRESH_MS = 10000;

  function esc(s) {
    return String(s == null ? '' : s).replace(/[&<>"']/g, function (c) {
      return { '&': '&amp;', '<': '&lt;', '>': '&gt;', '"': '&quot;', "'": '&#39;' }[c];
    });
  }
  function num(n) { return Number(n || 0).toLocaleString(); }
  function bytes(n) {
    n = Number(n || 0);
    var units = ['B', 'KB', 'MB', 'GB', 'TB'];
    var i = 0;
    while (n >= 1000 && i < units.length - 1) { n /= 1000; i++; }
    return (i === 0 ? n : n.toFixed(1)) + ' ' + units[i];
  }
  function duration(ms) {
    if (ms == null) return '';
    var s = Math.round(ms / 1000);
    if (s < 60) return s + 's';
    var m = Math.floor(s / 60);
    if (m < 60) return m + 'm ' + String(s % 60).padStart(2, '0') + 's';
    return Math.floor(m / 60) + 'h ' + String(m % 60).padStart(2, '0') + 'm';
  }
  // Run times are stored as SQLite's UTC `YYYY-MM-DD HH:MM:SS`; log lines are
  // RFC 3339. Both are shown in local time.
  function when(ts) {
    if (!ts) return '';
    var d = new Date(/T/.test(ts) ? ts : ts.replace(' ', 'T') + 'Z');
    if (isNaN(d)) return ts;
    var opts = { day: 'numeric', month: 'short', hour: '2-digit', minute: '2-digit' };
    if (d.getFullYear() !== new Date().getFullYear()) opts.year = 'numeric';
    return d.toLocaleString(undefined, opts);
  }
  function pill(text, cls) { return '<span class="diag-pill diag-' + cls + '">' + esc(text) + '</span>'; }
  function section(title, html) {
    return '<section class="diag-section"><h2>' + esc(title) + '</h2>' + html + '</section>';
  }
  function bar(part, whole) {
    var pct = whole > 0 ? Math.round(part * 100 / whole) : 0;
    return '<div class="diag-bar"><i style="width:' + pct + '%"></i></div>';
  }
  function fail(e) {
    root.innerHTML = '<p class="diag-error">Could not read this: ' + esc(e && e.message || e) + '</p>';
  }
  function getJson(url) {
    return fetch(url).then(function (r) {
      if (!r.ok) return r.text().then(function (t) { throw new Error(t || r.status); });
      return r.json();
    });
  }

  // ---------------------------------------------------------------- Status

  function step(command, note) {
    return '<div class="diag-step"><code class="diag-code">' + esc(command) + '</code><span class="diag-note">' + esc(note) + '</span></div>';
  }
  function statusPill(p) {
    var s = p.currently_running ? 'running' : (p.status || 'never run');
    var cls = { running: 'run', success: 'ok', failed: 'err', crashed: 'err', interrupted: 'warn' }[s] || 'idle';
    return pill(s, cls);
  }
  function skippedNote(summary) {
    return summary && /skipped$/.test(summary) ? summary : '';
  }

  function renderStatus(doc) {
    var r = doc.report;
    var w = r.watch;
    var problems = r.logs.filter(function (l) { return l.errors + l.warnings > 0; });
    var watchText = w.running
      ? (w.last_cycle_at ? 'last cycle ' + when(w.last_cycle_at) : 'first cycle in progress')
      : (w.last_cycle_at ? 'last cycle ' + when(w.last_cycle_at) : 'never run');
    var html = '<div class="diag-head">' +
      pill(w.running ? 'watch running' : 'watch not running', w.running ? 'run' : 'idle') +
      '<span class="diag-note">' + esc(watchText) + '</span>' +
      (problems.length ? pill(problems.length + ' with problems', 'warn') : pill('no problems', 'ok')) +
      '</div>';

    var rows = r.coverage.map(function (c) {
      var done = c.total - c.outstanding - c.skipped;
      var state;
      if (c.stale) state = pill('clusters stale', 'warn');
      else if (c.outstanding > 0) state = pill(num(c.outstanding) + ' outstanding', c.heavy ? 'idle' : 'warn');
      else state = pill('up to date', 'ok');
      var skipped = c.skipped ? ' ' + pill(num(c.skipped) + ' skipped', 'warn') : '';
      return '<tr><td>' + esc(c.stage) + '</td><td>' + num(done) + ' of ' + num(c.total) + bar(done, c.total) +
        '</td><td>' + state + skipped + '</td></tr>';
    }).join('');
    html += section('Coverage (model ' + r.embed_model + ')',
      '<table class="diag-table"><colgroup><col style="width:22%"><col><col style="width:38%"></colgroup>' + rows + '</table>');

    var d = r.dates;
    var dated = [['EXIF', d.exif], ['video', d.video], ['sidecar', d.sidecar], ['mvhd', d.mvhd], ['file time', d.mtime], ['unresolved', d.unresolved]]
      .filter(function (x) { return x[1] > 0; })
      .map(function (x) { return x[0] + ' ' + num(x[1]); }).join(', ');
    if (dated) html += section('Dates', '<p class="diag-foot" style="margin:0">' + esc(dated) + '</p>');

    var runs = r.pipelines.map(function (p) {
      var head = '<tr><td>' + esc(p.command) + '</td><td class="diag-num">' + esc(p.last_run_at ? when(p.last_run_at) : 'never run') +
        '</td><td class="diag-num">' + esc(p.currently_running ? '' : duration(p.duration_ms)) + '</td><td>' + statusPill(p) +
        '</td><td>' + esc(skippedNote(p.summary)) + '</td></tr>';
      return head + (p.history || []).map(function (h) {
        return '<tr class="diag-earlier"><td>earlier</td><td class="diag-num">' + esc(when(h.started_at)) + '</td><td class="diag-num">' +
          esc(duration(h.duration_ms)) + '</td><td>' + esc(h.status) + '</td><td>' + esc(skippedNote(h.summary)) + '</td></tr>';
      }).join('');
    }).join('');
    html += section('Runs', '<table class="diag-table"><colgroup><col style="width:20%"><col style="width:24%"><col style="width:12%"><col style="width:14%"><col></colgroup>' +
      '<tr><th>Command</th><th>Started</th><th>Took</th><th>Result</th><th>Note</th></tr>' + runs + '</table>');

    if (problems.length) {
      var prows = problems.map(function (l) {
        var last = l.last_error;
        var msg = last ? (last.kind ? last.kind + ': ' : '') + String(last.message).split('\n')[0] : '';
        var href = '/diagnostics/logs?command=' + encodeURIComponent(l.command) + '&since=' + encodeURIComponent(l.started) + '&range=all';
        return '<tr><td>' + esc(l.command) + '</td><td class="diag-num">' + esc(when(l.started)) + '</td><td>' +
          num(l.errors) + ' error(s), ' + num(l.warnings) + ' warning(s)' + (msg ? '<div class="diag-msg">' + esc(msg) + '</div>' : '') +
          '</td><td><a href="' + esc(href) + '">Logs</a></td></tr>';
      }).join('');
      html += section('Recent problems', '<table class="diag-table"><colgroup><col style="width:20%"><col style="width:24%"><col><col style="width:10%"></colgroup>' + prows + '</table>');
    }

    // The same next steps `videre status` prints.
    var next = [];
    r.costs.forEach(function (pair) {
      var c = r.coverage.filter(function (x) { return x.stage === pair[0]; })[0];
      if (c && c.next_command) next.push(step(c.next_command, num(c.outstanding) + ' item(s)' + (c.heavy ? ', can take hours' : '')));
    });
    if (d.unresolved > 0) next.push(step('videre scan', num(d.unresolved) + ' file(s) still need their capture date resolved'));
    html += section('Next', next.length ? next.join('') : '<p class="diag-foot" style="margin:0">Nothing outstanding; the library is up to date.</p>');
    root.innerHTML = html;
  }

  var timer = null;
  function loadStatus() {
    getJson('/api/diagnostics/status').then(function (doc) {
      renderStatus(doc);
      clearTimeout(timer);
      if (doc.report.watch.running) timer = setTimeout(loadStatus, REFRESH_MS);
    }).catch(fail);
  }
  window.addEventListener('pagehide', function () { clearTimeout(timer); });

  // ----------------------------------------------------------------- Stats

  function renderStats(s) {
    var l = s.library;
    var html = '<div class="diag-cards">' +
      '<div class="diag-card"><p>Files</p><div>' + num(l.total_files) + '</div><p>' + num(l.total_photos) + ' photos, ' + num(l.total_videos) + ' videos</p></div>' +
      '<div class="diag-card"><p>Size</p><div>' + bytes(l.total_size_bytes) + '</div></div>' +
      '<div class="diag-card"><p>Faces / people</p><div>' + num(l.faces_detected) + ' / ' + num(l.people_named) + '</div></div>' +
      '<div class="diag-card"><p>Duplicates</p><div>' + num(l.duplicate_group_count) + ' groups</div><p>' + bytes(l.wasted_bytes) + ' wasted</p></div>' +
      '</div>';
    var m = l.marks || {};
    html += section('Marks', '<p class="diag-foot" style="margin:0">' + num(m.rated) + ' rated, ' + num(m.picked) + ' picked, ' + num(m.labelled) + ' labelled, ' + num(m.liked) + ' liked</p>');
    if ((l.embeddings || []).length) {
      html += section('Embeddings', '<table class="diag-table"><colgroup><col><col style="width:16%"><col style="width:14%"><col style="width:16%"></colgroup>' +
        l.embeddings.map(function (e) {
          return '<tr><td class="diag-mono">' + esc(e.model_id) + '</td><td class="diag-num">' + num(e.count) + '</td><td class="diag-num">' + num(e.dims) + '-dim</td><td class="diag-num">' + bytes(e.size_bytes) + '</td></tr>';
        }).join('') + '</table>');
    }
    var biggest = s.by_type.length ? s.by_type[0].bytes : 0;
    html += section('By type', '<table class="diag-table"><colgroup><col style="width:12%"><col style="width:26%"><col style="width:14%"><col style="width:14%"><col></colgroup>' +
      s.by_type.map(function (t) {
        return '<tr><td>' + esc(t.ext) + '</td><td>' + esc(t.mime) + '</td><td class="diag-num">' + num(t.files) + '</td><td class="diag-num">' + bytes(t.bytes) + '</td><td>' + bar(t.bytes, biggest) + '</td></tr>';
      }).join('') + '</table>');
    html += section('Disk used by videre', '<table class="diag-table"><colgroup><col style="width:34%"><col style="width:16%"><col style="width:16%"><col></colgroup>' +
      s.disk_use.map(function (u) {
        return '<tr><td>' + esc(u.label) + '</td><td class="diag-num">' + bytes(u.bytes) + '</td><td class="diag-num">' + num(u.files) + ' files</td><td class="diag-mono" title="' + esc(u.path) + '">' + esc(u.rebuildable ? 'rebuildable' : 'not rebuildable') + '</td></tr>';
      }).join('') + '</table>');
    var mm = s.mismatches;
    if (mm.count) {
      html += section('Name and content disagree (' + num(mm.count) + ')', '<table class="diag-table"><colgroup><col><col style="width:12%"><col style="width:24%"></colgroup>' +
        mm.files.map(function (f) {
          return '<tr><td class="diag-mono" title="' + esc(f.path) + '">' + esc(f.path) + '</td><td>' + esc(f.ext) + '</td><td>' + esc(f.mime) + '</td></tr>';
        }).join('') + '</table>' + (mm.truncated ? '<p class="diag-foot"><code>videre stats --mismatched</code> lists them all.</p>' : ''));
    }
    root.innerHTML = html;
  }

  // ------------------------------------------------------------------ Logs

  var RANGES = [['1', 'Last day'], ['7', 'Last 7 days'], ['30', 'Last 30 days'], ['all', 'All time']];
  var LEVELS = [['warn', 'Warnings and errors'], ['error', 'Errors'], ['info', 'Info and up']];

  function logState() {
    var p = new URLSearchParams(location.search);
    return {
      command: p.get('command') || '',
      level: p.get('level') || 'warn',
      range: p.get('range') || '7',
      since: p.get('since') || '',
      q: p.get('q') || ''
    };
  }
  function saveState(st) {
    var p = new URLSearchParams();
    if (st.command) p.set('command', st.command);
    if (st.level !== 'warn') p.set('level', st.level);
    if (st.range !== '7') p.set('range', st.range);
    if (st.since) p.set('since', st.since);
    if (st.q) p.set('q', st.q);
    var qs = p.toString();
    history.replaceState(null, '', location.pathname + (qs ? '?' + qs : ''));
  }
  function apiUrl(st, before) {
    var p = new URLSearchParams();
    if (st.command) p.set('command', st.command);
    p.set('level', st.level);
    var since = st.since;
    if (!since && st.range !== 'all') since = new Date(Date.now() - Number(st.range) * 86400000).toISOString();
    if (since) p.set('since', since);
    if (st.q) p.set('q', st.q);
    if (before) p.set('before', before);
    return '/api/diagnostics/logs?' + p.toString();
  }
  function options(list, value) {
    return list.map(function (o) {
      return '<option value="' + esc(o[0]) + '"' + (o[0] === value ? ' selected' : '') + '>' + esc(o[1]) + '</option>';
    }).join('');
  }
  function levelPill(level) { return pill(level, { error: 'err', warn: 'warn' }[level] || 'idle'); }
  function lineRow(l) {
    var path = l.path ? '<div><a class="diag-mono" href="/?q=' + encodeURIComponent('path:"' + l.path + '"') + '">' + esc(l.path) + '</a></div>' : '';
    return '<tr><td class="diag-num">' + esc(when(l.ts)) + '</td><td>' + esc(l.command || '') + (l.stage ? '<div class="diag-note">' + esc(l.stage) + '</div>' : '') +
      '</td><td>' + levelPill(l.level) + '</td><td class="diag-mono">' + esc(l.kind || '') + '</td><td><div class="diag-msg">' + esc(l.message) + '</div>' + path + '</td></tr>';
  }

  function renderLogs() {
    var st = logState();
    root.innerHTML = '<div class="diag-controls">' +
      '<select id="diag-command" aria-label="Command"><option value="">All commands</option></select>' +
      '<select id="diag-level" aria-label="Level">' + options(LEVELS, st.level) + '</select>' +
      '<select id="diag-range" aria-label="Range">' + options(RANGES, st.since ? 'all' : st.range) + '</select>' +
      '<input id="diag-q" type="search" placeholder="IMG_4411" aria-label="Search messages and paths" value="' + esc(st.q) + '">' +
      '</div><div id="diag-logs"><p class="diag-empty">Loading&hellip;</p></div>';
    var out = document.getElementById('diag-logs');
    var last = null;

    function read(before) {
      return getJson(apiUrl(st, before)).then(function (page) {
        var sel = document.getElementById('diag-command');
        if (sel.options.length === 1) {
          sel.innerHTML += options(page.commands.map(function (c) { return [c, c]; }), st.command);
        }
        return page;
      });
    }
    function notes(page) {
      var n = [];
      if (page.no_trace.length) n.push('No info lines for ' + page.no_trace.join(', ') + ': its log-level was below info.');
      if (page.unreadable) n.push(num(page.unreadable) + ' line(s) could not be read.');
      return n.map(function (t) { return '<p class="diag-foot">' + esc(t) + '</p>'; }).join('');
    }
    function more(page) {
      return page.more ? '<button type="button" class="diag-more" id="diag-more">Show more</button>' : '';
    }
    function bindMore() {
      var b = document.getElementById('diag-more');
      if (!b) return;
      b.addEventListener('click', function () {
        b.disabled = true;
        read(last).then(function (page) {
          b.remove();
          document.getElementById('diag-rows').insertAdjacentHTML('beforeend', page.lines.map(lineRow).join(''));
          if (page.lines.length) last = page.lines[page.lines.length - 1].ts;
          out.insertAdjacentHTML('beforeend', more(page));
          bindMore();
        }).catch(fail);
      });
    }
    function load() {
      saveState(st);
      read(null).then(function (page) {
        last = page.lines.length ? page.lines[page.lines.length - 1].ts : null;
        out.innerHTML = page.lines.length
          ? '<table class="diag-table" style="margin-top:10px"><colgroup><col style="width:18%"><col style="width:13%"><col style="width:10%"><col style="width:17%"><col></colgroup>' +
            '<thead><tr><th>Time</th><th>Command</th><th>Level</th><th>Kind</th><th>Message</th></tr></thead><tbody id="diag-rows">' +
            page.lines.map(lineRow).join('') + '</tbody></table>' + more(page) + notes(page)
          : '<p class="diag-empty">No log lines match.</p>' + notes(page);
        bindMore();
      }).catch(function (e) { out.innerHTML = '<p class="diag-error">Could not read the logs: ' + esc(e.message) + '</p>'; });
    }
    function on(id, ev, fn) { document.getElementById(id).addEventListener(ev, fn); }
    on('diag-command', 'change', function (e) { st.command = e.target.value; load(); });
    on('diag-level', 'change', function (e) { st.level = e.target.value; load(); });
    on('diag-range', 'change', function (e) { st.range = e.target.value; st.since = ''; load(); });
    var typing = null;
    on('diag-q', 'input', function (e) {
      clearTimeout(typing);
      typing = setTimeout(function () { st.q = e.target.value.trim(); load(); }, 250);
    });
    load();
  }

  if (tab === 'status') loadStatus();
  else if (tab === 'stats') getJson('/api/diagnostics/stats').then(renderStats).catch(fail);
  else renderLogs();
})();
