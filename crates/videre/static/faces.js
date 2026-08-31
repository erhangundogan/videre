let facesData = { people: [], clusters: [], singletons: [] };

    // Sub-page prefix, from the server. `videre gallery` puts the labeling UI at
    // /people and its sub-pages beneath it; a labeling-only server serves it at
    // / and has no /people at all, so this cannot be hardcoded either way.
    function peopleRoot() {
      const r = (typeof PEOPLE_ROOT === 'string') ? PEOPLE_ROOT : '/';
      return r.endsWith('/') ? r : r + '/';
    }

    async function loadFaces() {
      try {
        const r = await fetch('/api/faces');
        if (!r.ok) throw new Error(`/api/faces returned ${r.status}`);
        facesData = await r.json();
        render();
        const total = facesData.people.length + facesData.clusters.length +
                      facesData.singletons.length;
        // Nothing detected is a state, not a failure, and it is the state every
        // library starts in. Saying "0 people, 0 clusters, 0 singletons" is
        // accurate and tells nobody what to do about it.
        if (total === 0) { showNothingDetected(); return; }
        setHeaderStats();
      } catch(e) {
        alert('Error loading faces: ' + e);
      }
    }

    function setHeaderStats() {
      const put = (id, n) => { const e = document.getElementById(id); if (e) e.textContent = n; };
      put('stat-people', facesData.people.length);
      put('stat-clusters', facesData.clusters.length);
      put('stat-singletons', facesData.singletons.length);
    }

    function showNothingDetected() {
      const host = document.querySelector('.people-section');
      if (!host || document.getElementById('faces-empty')) return;
      const box = document.createElement('div');
      box.id = 'faces-empty';
      box.className = 'empty-state';
      box.innerHTML =
        '<h2>No faces detected yet</h2>' +
        '<p>Face detection has not run against this library, so there is nobody ' +
        'to name here.</p>' +
        '<p class="hint">Run <code>videre faces</code> to detect and group them, ' +
        'then reload this page. It downloads the detection models on first use ' +
        'and takes a while on a large library, which is why it is a command you ' +
        'run rather than something a page starts for you.</p>';
      host.parentNode.insertBefore(box, host);
      ['.people-section', '.title-clusters', '#cluster-grid',
       '.title-singletons', '#singleton-grid'].forEach(function(sel) {
        const el = document.querySelector(sel);
        if (el) el.style.display = 'none';
      });
    }

    function faceImg(faceId, w, h) {
      return `<img class="face-img" src="/api/faces/${faceId}/image" width="${w}" height="${h}" title="#${faceId}" onerror="this.removeAttribute('src');this.style.background='#ddd'">`;
    }

    function thumbGrid(faceIds) {
      if (faceIds.length === 1) {
        return `<div style="margin-bottom:6px">${faceImg(faceIds[0], 140, 140)}</div>`;
      }
      const visible = faceIds.slice(0, 4);
      const extra = faceIds.length > 4
        ? `<div class="extra-count">+${faceIds.length - 4} more</div>` : '';
      return `
        <div style="display:grid;grid-template-columns:repeat(2,66px);gap:4px;margin-bottom:6px">
          ${visible.map(id => faceImg(id, 66, 66)).join('')}
        </div>${extra}`;
    }

    function renderPeople(people) {
      const grid = document.getElementById('people-grid');
      document.getElementById('people-count').textContent = people.length;
      // Sort by name (case-insensitive) so cards keep a stable position while
      // you drag clusters onto them, count-sort reshuffled them mid-assign.
      const sorted = [...people].sort((a, b) =>
        a.full_name.localeCompare(b.full_name, undefined, { sensitivity: 'base' }));
      grid.innerHTML = sorted.map(p => {
        const url = `${peopleRoot()}person/${encodeURIComponent(p.label)}`;
        const extra = p.face_ids.length > 1
          ? `<div class="extra-count">+${p.face_ids.length - 1} more</div>` : '';
        return `
        <div class="card person-card"
             data-label="${escHtml(p.label)}"
             ondragover="event.preventDefault(); this.classList.add('drag-over')"
             ondragleave="this.classList.remove('drag-over')"
             ondrop="onDropToPerson(event, this.dataset.label); this.classList.remove('drag-over')">
          <a href="${url}">
            <div style="margin-bottom:6px">${faceImg(p.representative_id, 140, 140)}</div>
          </a>
          <a class="cluster-link" href="${url}" title="${escHtml(p.full_name)}">${escHtml(p.full_name)}</a>
          ${extra}
        </div>
      `;
      }).join('');
    }

    const MAX_NAME_LEN = 60;

    // Trim, collapse internal whitespace, strip control/bidi-spoofing
    // characters, and cap length by code point (not UTF-16 code unit) so a
    // pasted wall of text or a spoofed name can't stretch card layout,
    // corrupt display order, or bloat the DB.
    // Mirror of `videre_core::person::normalize`, so the page can say "this
    // will add to an existing person" before posting - which needs the same
    // identity the server will compute. Kept small and labelled as a mirror:
    // if the two disagree the warning misfires, which is visible, rather than
    // the assignment landing somewhere unexpected, which is not.
    const TURKISH_FOLD = { 'ı':'i','İ':'i','ğ':'g','Ğ':'g','ş':'s','Ş':'s',
                           'ö':'o','Ö':'o','ü':'u','Ü':'u','ç':'c','Ç':'c' };
    function personIdentity(raw) {
      const folded = Array.from(String(raw).trim())
        .map(ch => TURKISH_FOLD[ch] || ch).join('')
        .toLowerCase().normalize('NFKD');
      let out = '', lastSep = true;
      for (const ch of folded) {
        if (/[\\s_]/.test(ch)) { if (!lastSep) { out += '_'; lastSep = true; } continue; }
        if (/[a-z0-9]/.test(ch)) { out += ch; lastSep = false; }
      }
      return out.replace(/_+$/, '');
    }

    // The person an entered name would land on, or null for a new one.
    function existingPersonFor(typed) {
      const id = personIdentity(typed);
      if (!id || !mainData || !mainData.people) return null;
      return mainData.people.find(p => p.label === id) || null;
    }

    function sanitizeName(raw) {
      const filtered = Array.from(raw).filter(function(ch) {
        const cp = ch.codePointAt(0);
        if (cp < 0x20 || (cp >= 0x7f && cp <= 0x9f)) return false;
        if (cp === 0x200B) return false;
        if (cp === 0x200E || cp === 0x200F) return false;
        // 0x200C (ZWNJ) and 0x200D (ZWJ) are intentionally allowed,
        // required for Persian/Indic text and emoji ZWJ sequences.
        if (cp >= 0x202A && cp <= 0x202E) return false;
        if (cp >= 0x2060 && cp <= 0x2069) return false;
        if (cp === 0xFEFF) return false;
        return true;
      }).join('');
      const collapsed = filtered.trim().replace(/\s+/g, ' ');
      return Array.from(collapsed).slice(0, MAX_NAME_LEN).join('');
    }

    function renderAssignableCard(faceIds, linkUrl, cardClass, selFaceId) {
      const faceIdsJson = JSON.stringify(faceIds);
      // Singletons carry a selection id: clicking the thumbnail toggles
      // multi-select. The click target is scoped to the thumbnail ("select
      // zone") rather than the whole card so the drag handle and the New
      // Person controls, which live outside it, never toggle selection
      // (they mutate the card, which would break a whole-card click guard).
      // Clusters (selFaceId undefined) are unaffected.
      const selectable = selFaceId != null;
      const inner = thumbGrid(faceIds);
      let thumb;
      if (selectable) {
        // The thumbnail click toggles multi-select and the drag handle assigns,
        // so opening the photo gets its own control: a corner link that stops
        // the click from bubbling to the select handler. A singleton has no
        // detail page, so this is its only route to the full image.
        thumb = `<div class="sel-zone" onclick="toggleSingleton(${selFaceId})" title="Click to select">`
          + `<div class="sel-check">&#10003;</div>`
          + `<a class="view-orig" href="/api/faces/${selFaceId}/original" target="_blank"`
          + ` title="Open original photo" onclick="event.stopPropagation()">&#128269;</a>`
          + `${inner}</div>`;
      } else if (linkUrl) {
        thumb = `<a href="${escHtml(linkUrl)}">${inner}</a>`;
      } else {
        thumb = inner;
      }
      const selAttr = selectable ? `data-sel-id="${selFaceId}"` : '';
      return `
        <div class="card ${cardClass}" ${selAttr}>
          <div class="drag-handle" draggable="true" ondragstart="onDragStart(event, ${faceIdsJson})" title="Drag to assign to a person">
            <span class="drag-dots">&#8942;&#8942;&#8942;</span>
            <span class="drag-hint">Drag on a person to assign</span>
          </div>
          ${thumb}
          <div class="new-person-area">
            <button class="new-person-btn" onclick="showNewPersonInput(this, ${faceIdsJson})">New Person</button>
          </div>
        </div>
      `;
    }

    function renderClusters(clusters) {
      const grid = document.getElementById('cluster-grid');
      document.getElementById('cluster-count').textContent = clusters.length;
      const sorted = [...clusters].sort((a, b) => b.face_ids.length - a.face_ids.length);
      grid.innerHTML = sorted.map(c =>
        renderAssignableCard(c.face_ids, `${peopleRoot()}cluster/${c.cluster_id}`, 'cluster-card')
      ).join('');
    }

    function renderSingletons(singletons) {
      const grid = document.getElementById('singleton-grid');
      document.getElementById('singleton-count').textContent = singletons.length;
      grid.innerHTML = singletons.map(s =>
        renderAssignableCard([s.face_id], null, 'singleton-card', s.face_id)
      ).join('');
    }

    function render() {
      renderPeople(facesData.people);
      renderClusters(facesData.clusters);
      renderSingletons(facesData.singletons);
      updateSelectionUI();
    }

    function escHtml(s) {
      return s.replace(/&/g,'&amp;').replace(/</g,'&lt;').replace(/>/g,'&gt;').replace(/"/g,'&quot;');
    }

    function onDragStart(event, faceIds) {
      if (!event.target.closest('.drag-handle')) {
        event.preventDefault();
        return;
      }
      // Dragging a selected singleton carries the whole current selection, so
      // one drop assigns every selected face at once.
      let ids = faceIds;
      const card = event.target.closest('.card');
      const selId = card && card.dataset.selId != null ? Number(card.dataset.selId) : null;
      if (selId != null && selectedSingletons.has(selId) && selectedSingletons.size > 1) {
        ids = Array.from(selectedSingletons);
      }
      event.dataTransfer.setData('application/json', JSON.stringify({ face_ids: ids }));
    }

    // ---- singleton multi-select ----
    let selectedSingletons = new Set();

    function toggleSingleton(faceId) {
      if (selectedSingletons.has(faceId)) selectedSingletons.delete(faceId);
      else selectedSingletons.add(faceId);
      updateSelectionUI();
    }

    function updateSelectionUI() {
      document.querySelectorAll('.singleton-card').forEach(c => {
        const id = c.dataset.selId != null ? Number(c.dataset.selId) : null;
        c.classList.toggle('selected', id != null && selectedSingletons.has(id));
      });
      rebuildSelBar();
    }

    function rebuildSelBar() {
      const bar = document.getElementById('sel-bar');
      const n = selectedSingletons.size;
      bar.classList.toggle('on', n > 0);
      if (n === 0) { bar.innerHTML = ''; return; }
      bar.innerHTML =
        `<span class="sel-count">${n} selected</span>` +
        `<button onclick="newPersonFromSelection()">New Person</button>` +
        `<button onclick="clearSelection()">Clear</button>` +
        `<span class="sel-hint">or drag any selected onto a person</span>`;
    }

    function clearSelection() {
      selectedSingletons.clear();
      updateSelectionUI();
    }

    function newPersonFromSelection() {
      if (selectedSingletons.size === 0) return;
      const bar = document.getElementById('sel-bar');
      bar.innerHTML =
        `<input type="text" id="sel-np-input" placeholder="Person name" maxlength="${MAX_NAME_LEN}" list="people-list">` +
        `<button id="sel-np-go" onclick="submitSelectionPerson()">Create</button>` +
        `<button onclick="rebuildSelBar()">Cancel</button>` +
        `<span id="sel-np-note" class="merge-note"></span>`;
      const inp = document.getElementById('sel-np-input');
      inp.focus();
      // Say what will happen before it happens. Typing a name that already
      // exists adds to that person rather than creating one - usually what is
      // meant, and previously indistinguishable from creating until afterwards.
      // The `list` attribute is the other half: this input was the only one of
      // the three without the autocomplete the others already had.
      inp.addEventListener('input', function() {
        const hit = existingPersonFor(inp.value);
        const note = document.getElementById('sel-np-note');
        const go = document.getElementById('sel-np-go');
        if (hit) {
          note.textContent = `adds to ${hit.full_name}, ${hit.face_ids.length} face(s)`;
          go.textContent = `Add to ${hit.full_name}`;
        } else {
          note.textContent = '';
          go.textContent = 'Create';
        }
      });
      inp.addEventListener('keydown', function(e) {
        if (e.key === 'Enter') { e.preventDefault(); submitSelectionPerson(); }
      });
    }

    async function submitSelectionPerson() {
      const input = document.getElementById('sel-np-input');
      if (!input) return;
      const label = sanitizeName(input.value);
      if (!label) return;
      const ids = Array.from(selectedSingletons);
      const r = await fetch('/api/people', {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({ face_ids: ids, name: label })
      });
      if (!r.ok) {
        alert('Create person failed');
        return;
      }
      clearSelection();
      await loadFaces();
    }

    // ---- People placement toggle (right sidebar vs top bar) ----
    //
    // :warning: **The default is 'right' because the top strip shares the top of
    // the viewport with the nav.** Both are `position: sticky; top: 0`, so the
    // element you drag a cluster onto and the element you navigate with occupy
    // the same space. The sidebar runs down the right edge, where it competes
    // with nothing.
    //
    // Only `toggleLayout` writes this key, so anyone who never chose a layout
    // has nothing stored and picks the new default up immediately. A stored
    // value is always a deliberate choice and is left alone.
    function applyLayout() {
      const mode = localStorage.getItem('videre_people_layout') || 'right';
      document.body.classList.toggle('sidebar-mode', mode === 'right');
      const btn = document.getElementById('layout-toggle');
      if (btn) btn.textContent = mode === 'right' ? 'People: Right' : 'People: Top';
      measureChrome();
    }

    // The top strip sticks *below* the nav rather than over it.
    //
    // :warning: Measured rather than written as a constant. The nav's height
    // comes from `chrome.css`, which `faces.css` does not own, so a hardcoded
    // offset here would be a number that goes stale the first time the nav's
    // padding or font changes and nobody would notice until the drop zone was
    // covered again.
    function measureChrome() {
      const nav = document.querySelector('.secnav');
      document.documentElement.style.setProperty(
        '--secnav-h', (nav ? nav.offsetHeight : 0) + 'px');
    }

    function toggleLayout() {
      const cur = localStorage.getItem('videre_people_layout') || 'right';
      localStorage.setItem('videre_people_layout', cur === 'right' ? 'top' : 'right');
      applyLayout();
    }

    window.addEventListener('resize', measureChrome);

    async function onDropToPerson(event, personLabel) {
      event.preventDefault();
      const data = JSON.parse(event.dataTransfer.getData('application/json'));
      const r = await fetch(`/api/people/${encodeURIComponent(personLabel)}/faces`, {
        method: 'PUT',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({ face_ids: data.face_ids })
      });
      if (!r.ok) {
        alert('Assign failed');
        return;
      }
      clearSelection();
      await loadFaces();
    }

    function showNewPersonInput(btn, faceIds) {
      const area = btn.parentElement;
      const faceIdsJson = JSON.stringify(faceIds);
      const inputId = `np-input-${faceIds[0]}`;
      area.innerHTML = `
        <input type="text" class="np-input" id="${inputId}" placeholder="Person name" maxlength="${MAX_NAME_LEN}" autofocus>
        <div class="np-btn-row">
          <button class="np-create-btn" onclick="submitNewPerson('${inputId}', ${faceIdsJson})">Create</button>
          <button class="new-person-btn" onclick="loadFaces()">Cancel</button>
        </div>
      `;
      document.getElementById(inputId).addEventListener('keydown', function(e) {
        if (e.key === 'Enter') { e.preventDefault(); submitNewPerson(inputId, faceIds); }
      });
    }

    async function submitNewPerson(inputId, faceIds) {
      const input = document.getElementById(inputId);
      const label = sanitizeName(input.value);
      if (!label) return;
      const r = await fetch('/api/people', {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({ face_ids: faceIds, name: label })
      });
      if (!r.ok) {
        alert('Create person failed');
        return;
      }
      await loadFaces();
    }

    applyLayout();
    loadFaces();
