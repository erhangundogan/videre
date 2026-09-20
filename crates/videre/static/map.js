// The local map plot uses only persisted location clusters. The canvas draws
// the frame; DOM buttons carry marker labels, clicks, and accessibility.
(function () {
  if (!LIVE_SERVER) return;

  var WORLD = { south: -60, west: -180, north: 80, east: 180 };
  var CLUSTER_ZOOM = 2;
  var MIN_SCALE = 1;
  var MAX_SCALE = 12;
  var scale = 1;
  var ox = 0;
  var oy = 0;
  var activeCluster = null;
  var clusters = [];
  var byContinent = {};
  var wrapper = document.getElementById('map-plot-wrap');
  var canvas = document.getElementById('map-plot');
  var empty = document.getElementById('map-empty');
  if (!wrapper || !canvas || !empty) return;

  var markerLayer = document.createElement('div');
  markerLayer.className = 'map-marker-layer';
  wrapper.appendChild(markerLayer);

  var controls = document.createElement('div');
  controls.className = 'map-controls';
  controls.innerHTML =
    '<button id="map-zoom-in" type="button" aria-label="Zoom in">+</button>' +
    '<button id="map-zoom-out" type="button" aria-label="Zoom out">&minus;</button>' +
    '<button id="map-clear" type="button">Clear</button>';
  wrapper.appendChild(controls);

  function project(lat, lon) {
    return {
      x: (lon - WORLD.west) / (WORLD.east - WORLD.west),
      y: (WORLD.north - lat) / (WORLD.north - WORLD.south)
    };
  }

  // Equirectangular world coordinates mapped through the current zoom and pan.
  // The world is compressed vertically to match its 360 by 140 degree bounds.
  function toCanvas(point) {
    var width = wrapper.clientWidth;
    var height = wrapper.clientHeight;
    var base = Math.min(width / 1.5, height);
    return {
      x: width / 2 + (point.x - 0.5) * base * 1.5 * scale + ox,
      y: height / 2 + (point.y - 0.5) * base * 0.55 * scale + oy
    };
  }

  function tier() {
    return scale < CLUSTER_ZOOM ? 'world' : 'clusters';
  }

  function groupContinents() {
    byContinent = {};
    clusters.forEach(function (cluster) {
      var group = byContinent[cluster.continent];
      if (!group) {
        group = byContinent[cluster.continent] = {
          name: cluster.continent,
          lat: 0,
          lon: 0,
          count: 0,
          members: []
        };
      }
      group.lat += cluster.centroid_lat;
      group.lon += cluster.centroid_lon;
      group.count += cluster.photo_count;
      group.members.push(cluster);
    });
    Object.keys(byContinent).forEach(function (name) {
      var group = byContinent[name];
      group.lat /= group.members.length;
      group.lon /= group.members.length;
    });
  }

  function marker(label, count, lat, lon, markerTier, clusterId, click) {
    var position = toCanvas(project(lat, lon));
    var button = document.createElement('button');
    button.type = 'button';
    button.className = 'map-marker' +
      (clusterId !== null && clusterId === activeCluster ? ' active' : '');
    button.dataset.tier = markerTier;
    button.dataset.name = label;
    if (clusterId !== null) button.dataset.cluster = String(clusterId);
    button.style.left = position.x + 'px';
    button.style.top = position.y + 'px';
    button.innerHTML = escH(label) + '<span class="map-marker-count">' + count + '</span>';
    button.addEventListener('click', click);
    markerLayer.appendChild(button);
  }

  function drawFrame() {
    var ratio = window.devicePixelRatio || 1;
    var width = wrapper.clientWidth;
    var height = wrapper.clientHeight;
    canvas.width = Math.round(width * ratio);
    canvas.height = Math.round(height * ratio);
    var context = canvas.getContext('2d');
    context.setTransform(ratio, 0, 0, ratio, 0, 0);
    context.clearRect(0, 0, width, height);
    context.strokeStyle = 'rgba(148,163,184,.16)';
    context.lineWidth = 1;
    for (var lon = -180; lon <= 180; lon += 45) {
      var top = toCanvas(project(WORLD.north, lon));
      var bottom = toCanvas(project(WORLD.south, lon));
      context.beginPath();
      context.moveTo(top.x, top.y);
      context.lineTo(bottom.x, bottom.y);
      context.stroke();
    }
    for (var lat = -60; lat <= 80; lat += 20) {
      var left = toCanvas(project(lat, WORLD.west));
      var right = toCanvas(project(lat, WORLD.east));
      context.beginPath();
      context.moveTo(left.x, left.y);
      context.lineTo(right.x, right.y);
      context.stroke();
    }
  }

  function render() {
    drawFrame();
    markerLayer.innerHTML = '';
    if (tier() === 'world') {
      Object.keys(byContinent).sort().forEach(function (name) {
        var group = byContinent[name];
        marker(group.name, group.count, group.lat, group.lon, 'continent', null, function () {
          zoomToContinent(group);
        });
      });
      return;
    }
    clusters.forEach(function (cluster) {
      marker(
        cluster.name,
        cluster.photo_count,
        cluster.centroid_lat,
        cluster.centroid_lon,
        'cluster',
        cluster.cluster_id,
        function () {
          activeCluster = cluster.cluster_id;
          window.setGalleryCluster(cluster.cluster_id);
          render();
        }
      );
    });
  }

  function zoomAt(nextScale, x, y) {
    nextScale = Math.max(MIN_SCALE, Math.min(MAX_SCALE, nextScale));
    var factor = nextScale / scale;
    ox = x - wrapper.clientWidth / 2 - (x - wrapper.clientWidth / 2 - ox) * factor;
    oy = y - wrapper.clientHeight / 2 - (y - wrapper.clientHeight / 2 - oy) * factor;
    scale = nextScale;
    render();
  }

  function zoomToContinent(group) {
    var center = project(group.lat, group.lon);
    var base = Math.min(wrapper.clientWidth / 1.5, wrapper.clientHeight);
    scale = 2.25;
    ox = -(center.x - 0.5) * base * 1.5 * scale;
    oy = -(center.y - 0.5) * base * 0.55 * scale;
    render();
  }

  function clearSelection() {
    activeCluster = null;
    scale = 1;
    ox = 0;
    oy = 0;
    window.setGalleryCluster(null);
    render();
  }

  document.getElementById('map-zoom-in').addEventListener('click', function () {
    zoomAt(scale * 1.5, wrapper.clientWidth / 2, wrapper.clientHeight / 2);
  });
  document.getElementById('map-zoom-out').addEventListener('click', function () {
    zoomAt(scale / 1.5, wrapper.clientWidth / 2, wrapper.clientHeight / 2);
  });
  document.getElementById('map-clear').addEventListener('click', clearSelection);

  canvas.addEventListener('wheel', function (event) {
    event.preventDefault();
    var bounds = wrapper.getBoundingClientRect();
    zoomAt(
      scale * (event.deltaY < 0 ? 1.2 : 1 / 1.2),
      event.clientX - bounds.left,
      event.clientY - bounds.top
    );
  }, { passive: false });
  canvas.addEventListener('dblclick', function (event) {
    var bounds = wrapper.getBoundingClientRect();
    zoomAt(scale * 1.5, event.clientX - bounds.left, event.clientY - bounds.top);
  });

  var dragging = false;
  var dragX = 0;
  var dragY = 0;
  canvas.addEventListener('pointerdown', function (event) {
    dragging = true;
    dragX = event.clientX;
    dragY = event.clientY;
    canvas.setPointerCapture(event.pointerId);
  });
  canvas.addEventListener('pointermove', function (event) {
    if (!dragging) return;
    ox += event.clientX - dragX;
    oy += event.clientY - dragY;
    dragX = event.clientX;
    dragY = event.clientY;
    render();
  });
  canvas.addEventListener('pointerup', function () { dragging = false; });
  canvas.addEventListener('pointercancel', function () { dragging = false; });
  window.addEventListener('resize', render);

  fetch('/api/location-clusters')
    .then(function (response) {
      if (!response.ok) throw new Error('locations request failed');
      return response.json();
    })
    .then(function (rows) {
      clusters = rows;
      if (!clusters.length) {
        empty.hidden = false;
        render();
        return;
      }
      empty.hidden = true;
      groupContinents();
      render();
    })
    .catch(function () {
      empty.hidden = false;
      empty.textContent = 'Could not load locations.';
    });
})();
