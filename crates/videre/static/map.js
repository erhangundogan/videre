// Fetch location clusters for the map page. Plot interaction is layered onto
// this data source separately from the shared gallery grid.
(function () {
  if (!LIVE_SERVER) return;
  fetch('/api/location-clusters')
    .then(function (response) { return response.json(); })
    .then(function (clusters) {
      var empty = document.getElementById('map-empty');
      if (!empty) return;
      if (!clusters.length) {
        empty.hidden = false;
        return;
      }
      empty.hidden = true;
      window.__mapClusters = clusters;
    })
    .catch(function () {
      var empty = document.getElementById('map-empty');
      if (empty) empty.textContent = 'Could not load locations.';
    });
})();
