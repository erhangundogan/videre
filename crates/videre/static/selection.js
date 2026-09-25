// Shared multi-select, used by the People page (face ids) and the file grids
// (content hashes): a set of keys, an anchor for Shift-click ranges, and the
// selection bar along the bottom of the page ("N selected", actions, Clear).
//
// A page describes its items and actions; this owns the state and the bar.
//   items:  selector for every selectable element, in display order
//   keyOf:  element -> its key (string)
//   holder: element -> the element that shows the selected state (optional)
//   actions: api -> HTML for the page's buttons, between the count and Clear
//   hint:   optional text after Clear
function createSelection(opts) {
  var keys = new Set();
  var anchor = null;
  var api;

  function items() {
    return Array.prototype.slice.call(document.querySelectorAll(opts.items));
  }
  function holder(el) {
    return opts.holder ? opts.holder(el) : el;
  }
  function bar() {
    var el = document.getElementById(opts.bar);
    if (!el) {
      el = document.createElement('div');
      el.id = opts.bar;
      el.className = 'sel-bar';
      document.body.appendChild(el);
    }
    return el;
  }

  // Grids re-render wholesale (Show more, List/Tile, sort), so the selected
  // look is reapplied from the key set rather than kept on elements.
  function paint() {
    items().forEach(function (el) {
      holder(el).classList.toggle('selected', keys.has(opts.keyOf(el)));
    });
  }

  function renderBar() {
    var b = bar();
    var n = keys.size;
    b.classList.toggle('on', n > 0);
    if (!n) { b.innerHTML = ''; return; }
    b.innerHTML = '<span class="sel-count">' + n + ' selected</span>' +
      (opts.actions ? opts.actions(api) : '') +
      '<button type="button" data-sel-clear>Clear</button>' +
      (opts.hint ? '<span class="sel-hint">' + opts.hint + '</span>' : '');
  }

  function changed() {
    paint();
    renderBar();
    if (opts.onChange) opts.onChange(api);
  }

  api = {
    has: function (key) { return keys.has(key); },
    size: function () { return keys.size; },
    list: function () { return Array.from(keys); },
    // A plain click: toggle one item and make it the range anchor.
    toggle: function (key) {
      if (keys.has(key)) keys.delete(key); else keys.add(key);
      anchor = key;
      changed();
    },
    // A Shift-click: add every item from the anchor to this one, in display
    // order. Without an anchor still on the page it is a plain click.
    extend: function (key) {
      var order = items().map(opts.keyOf);
      var a = anchor === null ? -1 : order.indexOf(anchor);
      var b = order.indexOf(key);
      if (a < 0 || b < 0) { api.toggle(key); return; }
      for (var i = Math.min(a, b); i <= Math.max(a, b); i++) keys.add(order[i]);
      changed();
    },
    remove: function (list) {
      list.forEach(function (k) { keys.delete(k); });
      if (anchor !== null && !keys.has(anchor)) anchor = null;
      changed();
    },
    clear: function () {
      keys.clear();
      anchor = null;
      changed();
    },
    paint: paint,
    renderBar: renderBar,
    bar: bar
  };

  document.addEventListener('click', function (e) {
    if (e.target.closest('#' + opts.bar + ' [data-sel-clear]')) api.clear();
  });
  return api;
}
