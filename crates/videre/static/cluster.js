
// After an action that removes what this page was showing, go back to the
// labeling UI rather than to `/`.
//
// :warning: `/` is the **Files** view under `videre gallery`. These three
// redirects were missed when the back links were fixed in 0.20.5: the link at
// the top of the page went to the right place while every action that finished
// the page still left labeling.
function peopleHome() {
  return (typeof PEOPLE_ROOT === 'string' && PEOPLE_ROOT) ? PEOPLE_ROOT : '/';
}
// Set by the page before this script runs; see cluster.html.
const clusterId = window.CLUSTER_ID;
    let facesData = [];
    let mainData = { people: [] };
    const MAX_NAME_LEN = 60;

    // Trim, collapse internal whitespace, strip control/bidi-spoofing
    // characters, and cap length by code point (not UTF-16 code unit) so a
    // pasted wall of text or a spoofed name can't stretch card layout,
    // corrupt display order, or bloat the DB.
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

    document.getElementById('person-input').addEventListener('keydown', function(e) {
      if (e.key === 'Enter') { e.preventDefault(); assignAll(); }
    });

    async function load() {
      try {
        const [clusterRes, mainRes] = await Promise.all([
          fetch(`/api/cluster/${clusterId}`),
          fetch('/api/faces')
        ]);
        if (!clusterRes.ok) throw new Error('cluster fetch failed');
        const clusterData = await clusterRes.json();
        mainData = mainRes.ok ? await mainRes.json() : { people: [] };
        facesData = clusterData.faces;
        const dl = document.getElementById('people-list');
        dl.innerHTML = mainData.people.map(p => `<option value="${escHtml(p.full_name)}">`).join('');
        document.getElementById('face-count').textContent = `${facesData.length} face(s)`;
        render();
      } catch(e) {
        document.getElementById('status').textContent = 'Error: ' + e;
      }
    }

    function render() {
      const grid = document.getElementById('faces-grid');
      grid.innerHTML = facesData.map(f => `
        <div class="card" id="card-${f.face_id}">
          <img class="face-img" src="/api/face-image/${f.face_id}" width="180" height="180"
               onerror="this.removeAttribute('src');this.style.background='#ddd'">
          <div class="path" title="${escHtml(f.path)}">${escHtml(basename(f.path))}</div>
          <div class="face-id">#${f.face_id}</div>
          <div class="btns">
            <button class="danger" onclick="removeFace(${f.face_id})">Remove</button>
            <button onclick="assignOne(${f.face_id})">Assign</button>
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

    async function assignAll() {
      const label = sanitizeName(document.getElementById('person-input').value);
      if (!label) return;
      const faceIds = facesData.map(f => f.face_id);
      const r = await fetch('/api/new-person', {
        method: 'POST', headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({ face_ids: faceIds, label })
      });
      if (!r.ok) { document.getElementById('status').textContent = 'Error: assign failed'; return; }
      document.getElementById('status').textContent = `Assigned ${faceIds.length} face(s) to "${label}"`;
      setTimeout(() => { window.location.href = peopleHome(); }, 800);
    }

    let assignModalFaceId = null;

    function openAssignModal(faceId) {
      assignModalFaceId = faceId;
      document.getElementById('assign-people-list').innerHTML =
        mainData.people.map(p => `<option value="${escHtml(p.full_name)}">`).join('');
      document.getElementById('assignModal').classList.add('on');
      document.getElementById('assignInput').value = '';
      document.getElementById('assignInput').focus();
    }

    function closeAssignModal() {
      document.getElementById('assignModal').classList.remove('on');
      assignModalFaceId = null;
    }

    async function submitAssignModal() {
      const label = sanitizeName(document.getElementById('assignInput').value);
      if (!label) return;
      const faceId = assignModalFaceId;
      const r = await fetch('/api/assign', {
        method: 'POST', headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({ face_ids: [faceId], person_label: label })
      });
      if (!r.ok) { document.getElementById('status').textContent = 'Error: assign failed'; return; }
      closeAssignModal();
      document.getElementById(`card-${faceId}`)?.remove();
      facesData = facesData.filter(f => f.face_id !== faceId);
      document.getElementById('face-count').textContent = `${facesData.length} face(s)`;
    }

    document.getElementById('assignInput').addEventListener('keydown', function(e) {
      if (e.key === 'Enter') { e.preventDefault(); submitAssignModal(); }
    });

    document.addEventListener('keydown', function(e) {
      if (e.key === 'Escape') closeAssignModal();
    });
    document.getElementById('assignModal').addEventListener('click', function(e) {
      if (e.target === this) closeAssignModal();
    });

    async function dissolveCluster() {
      if (!confirm(`Dissolve cluster ${clusterId}? Its ${facesData.length} face(s) will become unassigned singletons (not deleted).`)) return;
      const r = await fetch('/api/dissolve-cluster', {
        method: 'POST', headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({ cluster_id: clusterId })
      });
      if (!r.ok) { document.getElementById('status').textContent = 'Error: dissolve failed'; return; }
      document.getElementById('status').textContent = 'Cluster dissolved';
      setTimeout(() => { window.location.href = peopleHome(); }, 500);
    }

    function assignOne(faceId) {
      openAssignModal(faceId);
    }

    load();
