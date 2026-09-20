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
  var activeRadius = null;
  var clusters = [];
  var byContinent = {};
  var wrapper = document.getElementById('map-plot-wrap');
  var canvas = document.getElementById('map-plot');
  var empty = document.getElementById('map-empty');
  var selectionRow = document.getElementById('map-selection-row');
  var breadcrumb = document.getElementById('map-breadcrumb');
  var radiusInput = document.getElementById('map-radius');
  var selectionStatus = document.getElementById('map-selection-status');
  if (!wrapper || !canvas || !empty || !selectionRow || !breadcrumb ||
      !radiusInput || !selectionStatus) return;

  var markerLayer = document.createElement('div');
  markerLayer.className = 'map-marker-layer';
  wrapper.appendChild(markerLayer);

  var controls = document.createElement('div');
  controls.className = 'map-controls';
  controls.innerHTML =
    '<button id="map-zoom-in" type="button" aria-label="Zoom in">+</button>' +
    '<button id="map-zoom-out" type="button" aria-label="Zoom out">&minus;</button>' +
    '<button id="map-clear" type="button" disabled>Clear</button>';
  wrapper.appendChild(controls);
  // Clear resets the selection and the view; it stays disabled until a cluster
  // is active, so it never refetches page 1 for a selection that was never made.
  var clearBtn = document.getElementById('map-clear');

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
    var padding = 80;
    if (position.x < -padding || position.x > wrapper.clientWidth + padding ||
        position.y < -padding || position.y > wrapper.clientHeight + padding) return;
    var button = document.createElement('button');
    button.type = 'button';
    button.className = 'map-marker' +
      (clusterId !== null && activeCluster && clusterId === activeCluster.cluster_id ?
        ' active' : '');
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
    drawRadiusRing(context);
  }

  function destinationPoint(cluster, radius, bearing) {
    var angular = radius / 6371;
    var latitude = cluster.centroid_lat * Math.PI / 180;
    var longitude = cluster.centroid_lon * Math.PI / 180;
    var nextLatitude = Math.asin(
      Math.sin(latitude) * Math.cos(angular) +
      Math.cos(latitude) * Math.sin(angular) * Math.cos(bearing)
    );
    var nextLongitude = longitude + Math.atan2(
      Math.sin(bearing) * Math.sin(angular) * Math.cos(latitude),
      Math.cos(angular) - Math.sin(latitude) * Math.sin(nextLatitude)
    );
    return {
      lat: nextLatitude * 180 / Math.PI,
      lon: ((nextLongitude * 180 / Math.PI + 540) % 360) - 180
    };
  }

  function drawRadiusRing(context) {
    if (!activeCluster || activeRadius === null) return;
    context.save();
    context.strokeStyle = '#60a5fa';
    context.lineWidth = 2;
    context.beginPath();
    var previous = null;
    for (var index = 0; index <= 64; index++) {
      var geographic = destinationPoint(
        activeCluster,
        activeRadius,
        index / 64 * Math.PI * 2
      );
      var point = toCanvas(project(geographic.lat, geographic.lon));
      if (!previous || Math.abs(point.x - previous.x) > wrapper.clientWidth) {
        context.moveTo(point.x, point.y);
      }
      else context.lineTo(point.x, point.y);
      previous = point;
    }
    context.stroke();
    context.restore();
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
          selectCluster(cluster, cluster.radius_km, 'push');
        }
      );
    });
  }

  var pendingRender = null;
  function scheduleRender() {
    if (pendingRender !== null) return;
    pendingRender = window.requestAnimationFrame(function () {
      pendingRender = null;
      render();
    });
  }

  function zoomAt(nextScale, x, y) {
    nextScale = Math.max(MIN_SCALE, Math.min(MAX_SCALE, nextScale));
    if (activeCluster && nextScale < CLUSTER_ZOOM) {
      clearSelection('push');
      return;
    }
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

  function locationPath(cluster, radius) {
    return '/map/location/' + encodeURIComponent(cluster.route_name) +
      '?radius=' + encodeURIComponent(radius);
  }

  function focusSelection(cluster, radius) {
    var width = wrapper.clientWidth;
    var height = wrapper.clientHeight;
    var base = Math.min(width / 1.5, height);
    var latitudeSpan = Math.max(radius / 111.32, 0.01) * 2;
    var cosine = Math.max(Math.cos(cluster.centroid_lat * Math.PI / 180), 0.01);
    var longitudeSpan = Math.min(360, radius / (111.32 * cosine) * 2);
    var projectedWidth = longitudeSpan / 360 * base * 1.5;
    var projectedHeight = latitudeSpan / 140 * base * 0.55;
    var fit = Math.min(
      width * 0.6 / Math.max(projectedWidth, 1),
      height * 0.6 / Math.max(projectedHeight, 1)
    );
    scale = Math.max(CLUSTER_ZOOM, Math.min(MAX_SCALE, fit));
    var center = project(cluster.centroid_lat, cluster.centroid_lon);
    ox = -(center.x - 0.5) * base * 1.5 * scale;
    oy = -(center.y - 0.5) * base * 0.55 * scale;
  }

  function showSelection(cluster, radius) {
    selectionStatus.hidden = true;
    selectionStatus.textContent = '';
    selectionRow.hidden = false;
    breadcrumb.innerHTML = '<a href="/map">Map</a> &gt; ' + escH(cluster.name);
    radiusInput.value = String(radius);
  }

  function selectCluster(cluster, radius, historyMode) {
    activeCluster = cluster;
    activeRadius = radius;
    wrapper.dataset.radius = String(radius);
    clearBtn.disabled = false;
    showSelection(cluster, radius);
    focusSelection(cluster, radius);
    window.setGalleryLocation(cluster.centroid_lat, cluster.centroid_lon, radius);
    if (historyMode === 'push') {
      window.history.pushState(null, '', locationPath(cluster, radius));
    }
    render();
  }

  function clearSelection(historyMode) {
    activeCluster = null;
    activeRadius = null;
    delete wrapper.dataset.radius;
    clearBtn.disabled = true;
    selectionRow.hidden = true;
    selectionStatus.hidden = true;
    selectionStatus.textContent = '';
    scale = 1;
    ox = 0;
    oy = 0;
    window.setGalleryLocation(null, null, null);
    if (historyMode === 'push') window.history.pushState(null, '', '/map');
    render();
  }

  function showUnknownLocation() {
    activeCluster = null;
    activeRadius = null;
    delete wrapper.dataset.radius;
    clearBtn.disabled = true;
    selectionRow.hidden = true;
    selectionStatus.textContent = 'Unknown location';
    selectionStatus.hidden = false;
    scale = 1;
    ox = 0;
    oy = 0;
    window.setGalleryLocation(null, null, null);
    render();
  }

  function locationStateFromUrl() {
    var match = /^\/map\/location\/([^/]+)$/.exec(window.location.pathname);
    if (!match) return null;
    var name;
    try { name = decodeURIComponent(match[1]); }
    catch (error) { return { kind: 'unknown' }; }
    var radius = Number(new URLSearchParams(window.location.search).get('radius'));
    return { kind: 'location', name: name, radius: radius };
  }

  function applyLocationState(state) {
    if (!state) {
      clearSelection('none');
      return;
    }
    if (state.kind !== 'location') {
      showUnknownLocation();
      return;
    }
    var cluster = clusters.find(function (candidate) {
      return candidate.route_name === state.name;
    });
    if (!cluster) {
      showUnknownLocation();
      return;
    }
    var radius = Number(state.radius);
    if (!Number.isFinite(radius) || radius <= 0) radius = cluster.radius_km;
    selectCluster(cluster, radius, 'none');
  }

  document.getElementById('map-zoom-in').addEventListener('click', function () {
    zoomAt(scale * 1.5, wrapper.clientWidth / 2, wrapper.clientHeight / 2);
  });
  document.getElementById('map-zoom-out').addEventListener('click', function () {
    zoomAt(scale / 1.5, wrapper.clientWidth / 2, wrapper.clientHeight / 2);
  });
  document.getElementById('map-clear').addEventListener('click', function () {
    clearSelection('push');
  });
  radiusInput.addEventListener('change', function () {
    if (!activeCluster || activeRadius === null) return;
    var radius = radiusInput.valueAsNumber;
    if (!Number.isFinite(radius) || radius < 1) {
      radiusInput.value = String(activeRadius);
      return;
    }
    activeRadius = radius;
    wrapper.dataset.radius = String(radius);
    focusSelection(activeCluster, radius);
    window.setGalleryLocation(activeCluster.centroid_lat, activeCluster.centroid_lon, radius);
    window.history.replaceState({}, '', locationPath(activeCluster, radius));
    render();
  });
  window.addEventListener('keydown', function (event) {
    if (event.key !== 'Escape' || !activeCluster) return;
    var lightbox = document.getElementById('lb');
    if (lightbox && lightbox.classList.contains('on')) return;
    clearSelection('push');
  }, true);
  window.addEventListener('popstate', function () {
    applyLocationState(locationStateFromUrl());
  });

  // Wheel and dblclick bind to the wrapper, not the canvas: the markers are DOM
  // buttons in a sibling layer above the canvas, so an event landing on a marker
  // never reaches a canvas listener, and the cursor sits on a marker exactly
  // when the user wants to zoom. Drag stays on the canvas so a press on a marker
  // is a click, not a pan.
  wrapper.addEventListener('wheel', function (event) {
    event.preventDefault();
    var bounds = wrapper.getBoundingClientRect();
    zoomAt(
      scale * (event.deltaY < 0 ? 1.2 : 1 / 1.2),
      event.clientX - bounds.left,
      event.clientY - bounds.top
    );
  }, { passive: false });
  wrapper.addEventListener('dblclick', function (event) {
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
    scheduleRender();
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
      applyLocationState(typeof GLOC === 'object' ? GLOC : locationStateFromUrl());
    })
    .catch(function () {
      empty.hidden = false;
      empty.textContent = 'Could not load locations.';
    });
})();
