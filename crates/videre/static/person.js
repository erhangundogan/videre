const personName = decodeURIComponent(window.location.pathname.split('/').pop());
    // Set by the page before this script runs; see person.html.
    const FACES_UI_ENABLED = window.FACES_UI_ENABLED === true;
    const MAX_NAME_LEN = 60;
    let facesData = [];

    (function() {
      const params = new URLSearchParams(location.search);
      if (params.get('from') === 'lightbox') {
        const link = document.getElementById('backLink');
        link.textContent = '← Back';
        link.href = '#';
        link.onclick = function(e) { e.preventDefault(); history.back(); };
      }
    })();

    if (FACES_UI_ENABLED) {
      document.getElementById('removeBtn').style.display = 'inline-block';
      document.getElementById('renameArea').style.display = 'inline-flex';
    }

    // Trim, collapse internal whitespace, strip control/bidi-spoofing
    // characters, and cap length by code point, mirrors the sanitization in
    // FACES_HTML/CLUSTER_HTML.
    function sanitizeName(raw) {
      const filtered = Array.from(raw).filter(function(ch) {
        const cp = ch.codePointAt(0);
        if (cp < 0x20 || (cp >= 0x7f && cp <= 0x9f)) return false;
        if (cp === 0x200B) return false;
        if (cp === 0x200E || cp === 0x200F) return false;
        if (cp >= 0x202A && cp <= 0x202E) return false;
        if (cp >= 0x2060 && cp <= 0x2069) return false;
        if (cp === 0xFEFF) return false;
        return true;
      }).join('');
      const collapsed = filtered.trim().replace(/\s+/g, ' ');
      return Array.from(collapsed).slice(0, MAX_NAME_LEN).join('');
    }

    async function load() {
      try {
        document.title = personName;
        const r = await fetch(`/api/person/${encodeURIComponent(personName)}`);
        if (!r.ok) throw new Error('person fetch failed');
        const data = await r.json();
        // After the fetch, not before: `data` is a `const` declared here, and
        // reading it earlier threw a ReferenceError that aborted the whole
        // function, so the page showed an error and no photos at all.
        //
        // The heading and the rename box show the display name; the URL and
        // every request keep using the identity.
        const shown = data.full_name || personName;
        document.getElementById('person-title').textContent = shown;
        document.title = shown;
        const ri = document.getElementById('renameInput');
        if (ri) ri.value = shown;
        facesData = data.faces;
        document.getElementById('face-count').textContent = `${facesData.length} face(s)`;
        render();
      } catch(e) {
        document.getElementById('status').textContent = 'Error: ' + e;
      }
    }

    function render() {
      const grid = document.getElementById('faces-grid');
      grid.innerHTML = facesData.map(f => `
        <div class="card${f.is_primary ? ' is-default' : ''}" id="card-${f.face_id}">
          ${f.is_primary ? '<span class="default-badge">&#9733; Default</span>' : ''}
          <a href="/api/original-image/${f.face_id}" target="_blank" title="Open original image">
            <img class="face-img" src="/api/face-image/${f.face_id}" width="180" height="180"
                 onerror="this.removeAttribute('src');this.style.background='#ddd'">
          </a>
          <div class="path" title="${escHtml(f.path)}">${escHtml(basename(f.path))}</div>
          <div class="face-id">#${f.face_id}</div>
          <div class="btns">
            <button class="danger" onclick="removeFace(${f.face_id})">Remove</button>
            <button onclick="setDefault(${f.face_id})" ${f.is_primary ? 'disabled title="Already the default photo"' : 'title="Show this photo for this person on the labeling page"'}>Set Default</button>
          </div>
        </div>
      `).join('');
    }

    function basename(p) { return p.split('/').pop() || p; }

    function escHtml(s) {
      return String(s).replace(/&/g,'&amp;').replace(/</g,'&lt;').replace(/>/g,'&gt;').replace(/"/g,'&quot;');
    }

    async function removeFace(faceId) {
      const r = await fetch('/api/remove-face', {
        method: 'POST', headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({ face_id: faceId })
      });
      if (!r.ok) { document.getElementById('status').textContent = 'Error: remove failed'; return; }
      document.getElementById(`card-${faceId}`)?.remove();
      facesData = facesData.filter(f => f.face_id !== faceId);
      document.getElementById('face-count').textContent = `${facesData.length} face(s)`;
    }

    async function setDefault(faceId) {
      const r = await fetch('/api/set-primary', {
        method: 'POST', headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({ face_id: faceId, person_label: personName })
      });
      if (!r.ok) { document.getElementById('status').textContent = 'Error: set default failed'; return; }
      // Move the flag locally and re-render so the badge and disabled state
      // follow, without a full round-trip; the labeling page picks up the new
      // primary on its next load.
      facesData.forEach(f => { f.is_primary = (f.face_id === faceId); });
      render();
      document.getElementById('status').textContent = 'Default photo updated';
    }

    async function removePerson() {
      if (!confirm('Remove ' + personName + '? Their ' + facesData.length + ' photo(s) will become unassigned.')) return;
      const r = await fetch('/api/delete-person', {
        method: 'POST', headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({ label: personName })
      });
      if (!r.ok) { alert('Failed to remove person.'); return; }
      window.location.href = '/';
    }

    // Edits how the person is shown - adding a surname, fixing a spelling -
    // without touching their identity, so the URL and every face row are
    // untouched and no link breaks. Changing the identity is a different
    // operation and deliberately not offered here.
    async function submitRename() {
      const newName = sanitizeName(document.getElementById('renameInput').value);
      if (!newName) return;
      const r = await fetch('/api/set-full-name', {
        method: 'POST', headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({ name: personName, full_name: newName })
      });
      if (!r.ok) { alert('Could not save the name.'); return; }
      document.getElementById('person-title').textContent = newName;
      document.getElementById('status').textContent = 'Saved';
    }

    load();
