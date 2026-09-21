// The Map view has two renderers over one interaction contract. The real
// browser path renders MapLibre GL JS over an offline PMTiles basemap; without
// WebGL (or the vendored libraries) it falls back to the self-drawn canvas
// plot, which is kept verbatim. Both drive the same DOM markers, selection,
// drill-down, radius, grid filtering, and history, so the page never regresses.
(function () {
  if (!LIVE_SERVER) return;

  if (shouldUseMapLibre()) {
    runMapLibrePlot();
    return;
  }
  runCanvasPlot();

  // A working WebGL context and the vendored libraries are both required; the
  // E2E suite forces the fallback with `__VIDERE_FORCE_CANVAS_MAP__` so the
  // interaction-contract specs test the canvas path deterministically, while a
  // dedicated spec exercises the real MapLibre path.
  function shouldUseMapLibre() {
    if (window.__VIDERE_FORCE_CANVAS_MAP__) return false;
    if (!window.maplibregl || !window.pmtiles || !window.BASEMAP_STYLE) return false;
    try {
      var probe = document.createElement('canvas');
      return !!(window.WebGLRenderingContext &&
        (probe.getContext('webgl2') || probe.getContext('webgl')));
    } catch (error) {
      return false;
    }
  }

  // ------------------------------------------------------------------
  // MapLibre renderer
  // ------------------------------------------------------------------
  function runMapLibrePlot() {
    var CLUSTER_ZOOM = 3;
    var clusters = [];
    var byContinent = {};
    var activeCluster = null;
    var activeRadius = null;
    var map = null;
    var ready = false;
    // Set while a programmatic fly/jump is animating, so the zoom-to-world
    // auto-clear does not fire on our own camera moves (a selection's flyTo
    // starts below the cluster threshold before climbing above it).
    var programmaticView = false;

    var wrapper = document.getElementById('map-plot-wrap');
    var canvas = document.getElementById('map-plot');
    var empty = document.getElementById('map-empty');
    var selectionRow = document.getElementById('map-selection-row');
    var breadcrumb = document.getElementById('map-breadcrumb');
    var radiusInput = document.getElementById('map-radius');
    var radiusGroup = document.getElementById('map-radius-group');
    var selectionStatus = document.getElementById('map-selection-status');
    if (!wrapper || !canvas || !empty || !selectionRow || !breadcrumb ||
        !radiusInput || !radiusGroup || !selectionStatus) return;

    // The canvas is the fallback's surface; MapLibre draws into its own child.
    canvas.style.display = 'none';
    var glContainer = document.createElement('div');
    glContainer.id = 'map-gl';
    wrapper.insertBefore(glContainer, wrapper.firstChild);

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
    var clearBtn = document.getElementById('map-clear');

    var attribution = document.createElement('div');
    attribution.id = 'map-attribution';
    attribution.className = 'map-attribution';
    attribution.textContent = '© OpenStreetMap contributors';
    wrapper.appendChild(attribution);

    function groupContinents() {
      byContinent = {};
      clusters.forEach(function (cluster) {
        var group = byContinent[cluster.continent];
        if (!group) {
          group = byContinent[cluster.continent] = {
            name: cluster.continent, lat: 0, lon: 0, count: 0, members: []
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

    function inView(point) {
      var padding = 80;
      return point.x >= -padding && point.x <= wrapper.clientWidth + padding &&
        point.y >= -padding && point.y <= wrapper.clientHeight + padding;
    }

    // Estimate a marker's on-screen box from its label, so overlap can be
    // tested without a reflow per tag on every move/zoom. The chip is centred
    // on its point (translate(-50%,-50%)) and sized like the CSS pill.
    function markerBox(point, label) {
      var width = Math.min(180, Math.max(88, label.length * 7.2 + 46));
      var height = 30;
      return {
        left: point.x - width / 2, right: point.x + width / 2,
        top: point.y - height / 2, bottom: point.y + height / 2
      };
    }

    function overlaps(a, b) {
      var gap = 4;
      return !(a.right + gap < b.left || a.left - gap > b.right ||
        a.bottom + gap < b.top || a.top - gap > b.bottom);
    }

    function addMarker(spec, placed) {
      var point = map.project([spec.lon, spec.lat]);
      if (!inView(point)) return;
      var box = markerBox(point, spec.label);
      // Declutter: a tag yields to an already-placed, higher-priority tag it
      // would overlap, so nearby markers thin out instead of piling into an
      // unreadable clump. Zooming in spreads the points and reveals more. The
      // active selection is always kept.
      if (!spec.active) {
        for (var i = 0; i < placed.length; i++) {
          if (overlaps(box, placed[i])) return;
        }
      }
      placed.push(box);
      var button = document.createElement('button');
      button.type = 'button';
      button.className = 'map-marker' + (spec.active ? ' active' : '');
      button.dataset.tier = spec.tier;
      button.dataset.name = spec.label;
      if (spec.clusterId !== null) button.dataset.cluster = String(spec.clusterId);
      button.style.left = point.x + 'px';
      button.style.top = point.y + 'px';
      button.innerHTML = escH(spec.label) +
        '<span class="map-marker-count">' + spec.count + '</span>';
      button.addEventListener('click', spec.click);
      markerLayer.appendChild(button);
    }

    function tier() {
      return map.getZoom() < CLUSTER_ZOOM ? 'world' : 'clusters';
    }

    // Marker specs for the current tier, ordered by priority so decluttering
    // keeps the most significant tags: the active selection first, then the
    // largest by photo count.
    function markerSpecs() {
      if (tier() === 'world') {
        return Object.keys(byContinent).map(function (name) {
          var group = byContinent[name];
          return {
            label: group.name, count: group.count, lat: group.lat, lon: group.lon,
            tier: 'continent', clusterId: null, active: false,
            click: function () { map.flyTo({ center: [group.lon, group.lat], zoom: CLUSTER_ZOOM + 1 }); }
          };
        }).sort(function (a, b) { return b.count - a.count; });
      }
      return clusters.map(function (cluster) {
        var isActive = !!(activeCluster && cluster.cluster_id === activeCluster.cluster_id);
        return {
          label: cluster.name, count: cluster.photo_count,
          lat: cluster.centroid_lat, lon: cluster.centroid_lon,
          tier: 'cluster', clusterId: cluster.cluster_id, active: isActive,
          click: function () { selectCluster(cluster, cluster.radius_km, 'push'); }
        };
      }).sort(function (a, b) {
        if (a.active !== b.active) return a.active ? -1 : 1;
        return b.count - a.count;
      });
    }

    function updateMarkers() {
      if (!map) return;
      markerLayer.innerHTML = '';
      var placed = [];
      markerSpecs().forEach(function (spec) { addMarker(spec, placed); });
    }

    function destinationPoint(cluster, radius, bearing) {
      var angular = radius / 6371;
      var latitude = cluster.centroid_lat * Math.PI / 180;
      var longitude = cluster.centroid_lon * Math.PI / 180;
      var nextLatitude = Math.asin(
        Math.sin(latitude) * Math.cos(angular) +
        Math.cos(latitude) * Math.sin(angular) * Math.cos(bearing));
      var nextLongitude = longitude + Math.atan2(
        Math.sin(bearing) * Math.sin(angular) * Math.cos(latitude),
        Math.cos(angular) - Math.sin(latitude) * Math.sin(nextLatitude));
      // Deliberately unwrapped: keep the ring's longitudes continuous around
      // the loop so a cluster near +/-180 does not jump from +179 to -179 and
      // draw one line across the whole map. MapLibre wraps out-of-range
      // longitudes for display, so the geometry stays a clean circle.
      return [nextLongitude * 180 / Math.PI, nextLatitude * 180 / Math.PI];
    }

    function ringGeoJson() {
      if (!activeCluster || activeRadius === null) {
        return { type: 'FeatureCollection', features: [] };
      }
      var coordinates = [];
      for (var index = 0; index <= 64; index++) {
        coordinates.push(destinationPoint(activeCluster, activeRadius, index / 64 * Math.PI * 2));
      }
      return {
        type: 'FeatureCollection',
        features: [{ type: 'Feature', geometry: { type: 'LineString', coordinates: coordinates } }]
      };
    }

    function updateRing() {
      if (!map || !map.getSource('radius-ring')) return;
      map.getSource('radius-ring').setData(ringGeoJson());
    }

    function locationPath(cluster, radius) {
      return '/map/location/' + encodeURIComponent(cluster.route_name) +
        '?radius=' + encodeURIComponent(radius);
    }

    function showSelection(cluster, radius) {
      selectionStatus.hidden = true;
      selectionStatus.textContent = '';
      selectionRow.hidden = false;
      radiusGroup.hidden = false;
      breadcrumb.textContent = cluster.name;
      radiusInput.value = String(radius);
    }

    function selectCluster(cluster, radius, historyMode) {
      activeCluster = cluster;
      activeRadius = radius;
      wrapper.dataset.radius = String(radius);
      clearBtn.disabled = false;
      showSelection(cluster, radius);
      window.setGalleryLocation(cluster.centroid_lat, cluster.centroid_lon, radius);
      if (historyMode === 'push') {
        window.history.pushState(null, '', locationPath(cluster, radius));
      }
      updateRing();
      if (ready) viewFlyTo({ center: [cluster.centroid_lon, cluster.centroid_lat], zoom: CLUSTER_ZOOM + 2 });
      updateMarkers();
    }

    function clearSelection(historyMode) {
      activeCluster = null;
      activeRadius = null;
      delete wrapper.dataset.radius;
      clearBtn.disabled = true;
      selectionRow.hidden = true;
      radiusGroup.hidden = true;
      selectionStatus.hidden = true;
      selectionStatus.textContent = '';
      window.setGalleryLocation(null, null, null);
      updateRing();
      if (historyMode === 'push') window.history.pushState(null, '', '/map');
      if (ready) viewFlyTo({ center: [10, 30], zoom: 1 });
      updateMarkers();
    }

    function showUnknownLocation() {
      activeCluster = null;
      activeRadius = null;
      delete wrapper.dataset.radius;
      clearBtn.disabled = true;
      selectionRow.hidden = true;
      radiusGroup.hidden = true;
      selectionStatus.textContent = 'Unknown location';
      selectionStatus.hidden = false;
      window.setGalleryLocation(null, null, null);
      updateRing();
      updateMarkers();
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
      if (!state) { clearSelection('none'); return; }
      if (state.kind !== 'location') { showUnknownLocation(); return; }
      var cluster = clusters.find(function (candidate) {
        return candidate.route_name === state.name;
      });
      if (!cluster) { showUnknownLocation(); return; }
      var radius = Number(state.radius);
      if (!Number.isFinite(radius) || radius <= 0) radius = cluster.radius_km;
      selectCluster(cluster, radius, 'none');
    }

    document.getElementById('map-zoom-in').addEventListener('click', function () {
      map.zoomIn();
    });
    document.getElementById('map-zoom-out').addEventListener('click', function () {
      map.zoomOut();
    });
    clearBtn.addEventListener('click', function () { clearSelection('push'); });
    radiusInput.addEventListener('change', function () {
      if (!activeCluster || activeRadius === null) return;
      var radius = radiusInput.valueAsNumber;
      if (!Number.isFinite(radius) || radius <= 0) {
        radiusInput.value = String(activeRadius);
        return;
      }
      activeRadius = radius;
      wrapper.dataset.radius = String(radius);
      window.setGalleryLocation(activeCluster.centroid_lat, activeCluster.centroid_lon, radius);
      window.history.replaceState({}, '', locationPath(activeCluster, radius));
      updateRing();
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

    // The map initializes on a background-only style so its `load` fires
    // whether or not the basemap archive is present: a machine without the
    // tiles yet still gets a working map and grid. The vector basemap is
    // attached later, only once the archive is ready.
    function baseStyle() {
      var full = window.BASEMAP_STYLE;
      var background = null;
      (full.layers || []).forEach(function (layer) {
        if (layer.type === 'background') background = layer;
      });
      return { version: 8, sources: {}, layers: background ? [background] : [] };
    }

    function addBasemapLayers() {
      if (!map || map.getSource('basemap')) return;
      var full = window.BASEMAP_STYLE;
      if (!full.sources || !full.sources.basemap) return;
      map.addSource('basemap', full.sources.basemap);
      var before = map.getLayer('radius-ring') ? 'radius-ring' : undefined;
      (full.layers || []).forEach(function (layer) {
        if (layer.source === 'basemap') map.addLayer(layer, before);
      });
    }

    // Poll the basemap status; attach the vector tiles once the archive is
    // ready, kicking the download once when it is not. Bounded and
    // fire-and-forget: view time on a seeded machine never touches the network.
    var basemapPolls = 0;
    function pollBasemap() {
      fetch('/api/basemap/status')
        .then(function (response) { return response.json(); })
        .then(function (status) {
          if (status.state === 'ready') { addBasemapLayers(); return; }
          // Kick the download for both absent and partial: a `.part` sibling
          // left by an interrupted run reports partial, and ensure resumes it
          // (the server guard collapses concurrent kicks). Without this a
          // partial archive would only be polled, never resumed.
          if (status.state !== 'ready' && basemapPolls === 0) {
            fetch('/api/basemap/ensure', { method: 'POST' }).catch(function () {});
          }
          if (++basemapPolls < 120) window.setTimeout(pollBasemap, 1500);
        })
        .catch(function () {});
    }

    function initMap() {
      maplibregl.addProtocol('pmtiles', new pmtiles.Protocol().tile);
      map = new maplibregl.Map({
        container: 'map-gl',
        style: baseStyle(),
        center: [10, 30],
        zoom: 1,
        attributionControl: false,
        dragRotate: false,
        pitchWithRotate: false
      });
      map.on('load', function () {
        map.addSource('radius-ring', { type: 'geojson', data: ringGeoJson() });
        map.addLayer({
          id: 'radius-ring',
          type: 'line',
          source: 'radius-ring',
          paint: { 'line-color': '#60a5fa', 'line-width': 2 }
        });
        ready = true;
        window.maplibreInitialized = true;
        // The grid and selection are already applied (bootstrap, below); the map
        // only needs to catch up to the current state now that it can render.
        syncMapToState();
        pollBasemap();
      });
      map.on('move', updateMarkers);
      map.on('moveend', function () { programmaticView = false; });
      map.on('zoom', function () {
        updateMarkers();
        // Zooming out to the world tier clears any active selection, matching
        // the canvas renderer. Skipped during our own fly/jump (which begins
        // below the threshold on the way to a selection).
        if (activeCluster && !programmaticView && map.getZoom() < CLUSTER_ZOOM) {
          clearSelection('push');
        }
      });
    }

    // A programmatic camera move that must not trip the zoom-to-world
    // auto-clear. The flag resets on moveend.
    function viewFlyTo(options) {
      programmaticView = true;
      map.flyTo(options);
    }

    // Reflect the current selection onto the map once it can render. Never
    // touches the grid or history: those are owned by the bootstrap and apply
    // whether or not MapLibre ever loads (a probe can pass on a machine whose
    // WebGL still cannot render, and the grid must work regardless).
    function syncMapToState() {
      if (!map || !ready) return;
      updateRing();
      if (activeCluster) {
        programmaticView = true;
        map.jumpTo({
          center: [activeCluster.centroid_lon, activeCluster.centroid_lat],
          zoom: CLUSTER_ZOOM + 2
        });
      }
      updateMarkers();
    }

    fetch('/api/location-clusters')
      .then(function (response) {
        if (!response.ok) throw new Error('locations request failed');
        return response.json();
      })
      .then(function (rows) {
        clusters = rows;
        if (!clusters.length) {
          empty.hidden = false;
          // No clusters to select, but the grid still owns the page: load all
          // files, since gallery.js defers the initial grid fetch to the map.
          window.setGalleryLocation(null, null, null);
          initMap();
          return;
        }
        empty.hidden = true;
        groupContinents();
        initMap();
        // Apply the grid, breadcrumb, and URL immediately, independent of the
        // map's load: gallery.js defers the initial grid fetch to us, so it must
        // not wait on WebGL. The map catches up in syncMapToState on load.
        applyLocationState(typeof GLOC === 'object' ? GLOC : locationStateFromUrl());
        if (activeCluster) {
          window.history.replaceState(null, '', locationPath(activeCluster, activeRadius));
        }
      })
      .catch(function () {
        empty.hidden = false;
        empty.textContent = 'Could not load locations.';
        window.setGalleryLocation(null, null, null);
      });
  }

  // ------------------------------------------------------------------
  // Canvas fallback (Part 1 renderer, retained verbatim)
  // ------------------------------------------------------------------
  function runCanvasPlot() {
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
    var radiusGroup = document.getElementById('map-radius-group');
    var selectionStatus = document.getElementById('map-selection-status');
    if (!wrapper || !canvas || !empty || !selectionRow || !breadcrumb ||
        !radiusInput || !radiusGroup || !selectionStatus) return;

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
      radiusGroup.hidden = false;
      breadcrumb.textContent = cluster.name;
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
      radiusGroup.hidden = true;
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
      radiusGroup.hidden = true;
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
      // Accept any positive radius, matching the server (files_location_filter),
      // so a sub-1 radius arriving by URL or a sub-1 cluster default can be nudged.
      if (!Number.isFinite(radius) || radius <= 0) {
        radiusInput.value = String(activeRadius);
        return;
      }
      activeRadius = radius;
      wrapper.dataset.radius = String(radius);
      // Only the ring resizes; the plot view holds still (spec asks for the ring
      // redraw, not a re-zoom on every radius change).
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
          // No clusters to select, but the grid still owns the page: load all
          // files, since gallery.js defers the initial grid fetch to the map here.
          window.setGalleryLocation(null, null, null);
          return;
        }
        empty.hidden = true;
        groupContinents();
        applyLocationState(typeof GLOC === 'object' ? GLOC : locationStateFromUrl());
        // Canonicalize the address bar to the resolved route name without adding a
        // history entry, so Back/Forward always see the normalized URL (a manually
        // typed /map/location/Berlin becomes .../berlin and resolves on popstate).
        if (activeCluster) {
          window.history.replaceState(null, '', locationPath(activeCluster, activeRadius));
        }
      })
      .catch(function () {
        empty.hidden = false;
        empty.textContent = 'Could not load locations.';
        // The plot failed, but the grid still owns the page: load all files, since
        // gallery.js defers the initial grid fetch to the map on this page.
        window.setGalleryLocation(null, null, null);
      });
  }
})();
