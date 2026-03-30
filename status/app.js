const storageKey = "r2_proxy_status_api_key";

const state = {
  apiKey: localStorage.getItem(storageKey) || "",
  range: "1h",
  bucket: "",
};

const loginPanel = document.getElementById("login-panel");
const dashboard = document.getElementById("dashboard");
const loginForm = document.getElementById("login-form");
const loginError = document.getElementById("login-error");
const bucketFilter = document.getElementById("bucket-filter");
const logoutButton = document.getElementById("logout-button");
const metrics = document.getElementById("metrics");
const summaryStrip = document.getElementById("summary-strip");
const charts = document.getElementById("charts");
const hotFiles = document.getElementById("hot-files");
const missUrls = document.getElementById("miss-urls");
const errorUrls = document.getElementById("error-urls");

function formatBytes(bytes) {
  if (!bytes) return "0 B";
  const units = ["B", "KB", "MB", "GB", "TB"];
  let value = bytes;
  let index = 0;
  while (value >= 1024 && index < units.length - 1) {
    value /= 1024;
    index += 1;
  }
  return `${value.toFixed(value >= 10 || index === 0 ? 0 : 1)} ${units[index]}`;
}

function formatRate(rate) {
  return `${(rate * 100).toFixed(2)}%`;
}

function formatCompact(value) {
  return new Intl.NumberFormat("en", { notation: "compact", maximumFractionDigits: 1 }).format(value);
}

function withApiKey(headers = {}) {
  return { ...headers, "X-Status-API-Key": state.apiKey };
}

async function request(path) {
  const response = await fetch(path, { headers: withApiKey() });
  if (response.status === 401) {
    localStorage.removeItem(storageKey);
    state.apiKey = "";
    showLogin();
    throw new Error("unauthorized");
  }
  if (!response.ok) {
    throw new Error(`request failed: ${response.status}`);
  }
  return response.json();
}

async function login(apiKey) {
  const response = await fetch("/api/login", {
    method: "POST",
    headers: { "content-type": "application/json" },
    body: JSON.stringify({ apiKey }),
  });
  if (!response.ok) {
    throw new Error("invalid");
  }
}

function showLogin() {
  loginPanel.hidden = false;
  dashboard.hidden = true;
}

function showDashboard() {
  loginPanel.hidden = true;
  dashboard.hidden = false;
}

function metricCard(label, value, subtext) {
  return `
    <article class="metric-card">
      <p class="metric-label">${label}</p>
      <h2 class="metric-value">${value}</h2>
      <p class="metric-subtext">${subtext}</p>
    </article>
  `;
}

function renderMetrics(overview) {
  metrics.innerHTML = [
    metricCard("Total Requests", formatCompact(overview.totals.requests), "All successful and failed requests"),
    metricCard("Total Throughput", formatBytes(overview.totals.bytes), "Served successfully to clients"),
    metricCard("Cache Hit Rate", formatRate(overview.totals.cacheHitRate), "Hit / (Hit + Miss)"),
    metricCard("Errors", formatCompact(overview.totals.errors), `Error rate ${formatRate(overview.totals.errorRate)}`),
    metricCard("Local Cache", formatBytes(overview.localCache.usedBytes), `${formatRate(overview.localCache.usageRate)} of ${formatBytes(overview.localCache.capacityBytes)}`),
  ].join("");
}

function renderSummary(summary) {
  summaryStrip.innerHTML = [
    ["Requests", formatCompact(summary.requests)],
    ["Traffic", formatBytes(summary.bytes)],
    ["Hit Rate", formatRate(summary.cacheHitRate)],
    ["Error Rate", formatRate(summary.errorRate)],
  ].map(([label, value]) => `
      <div class="summary-item">
        <span>${label}</span>
        <strong>${value}</strong>
      </div>
    `).join("");
}

function linePath(points, width, height) {
  const max = Math.max(...points.map((point) => point.value), 1);
  const min = Math.min(...points.map((point) => point.value), 0);
  const xStep = points.length > 1 ? width / (points.length - 1) : width;
  return points.map((point, index) => {
    const x = index * xStep;
    const normalized = max === min ? 0.5 : (point.value - min) / (max - min);
    const y = height - normalized * height;
    return `${index === 0 ? "M" : "L"} ${x.toFixed(2)} ${y.toFixed(2)}`;
  }).join(" ");
}

function renderChart(title, color, points, formatter) {
  const path = linePath(points, 520, 180);
  const values = points.map((point) => point.value);
  return `
    <article class="chart-card">
      <p class="metric-label">${title}</p>
      <svg viewBox="0 0 520 180" preserveAspectRatio="none" aria-hidden="true">
        <path d="${path}" fill="none" stroke="${color}" stroke-width="4" stroke-linecap="round" stroke-linejoin="round"></path>
      </svg>
      <footer>
        <span>min ${formatter(Math.min(...values, 0))}</span>
        <span>max ${formatter(Math.max(...values, 0))}</span>
      </footer>
    </article>
  `;
}

function renderCharts(series) {
  charts.innerHTML = [
    renderChart("QPS", "#b46a1c", series.qps, (value) => value.toFixed(2)),
    renderChart("Throughput", "#1f8f68", series.throughputBytesPerSec, (value) => formatBytes(value)),
    renderChart("Cache Hit Rate", "#182126", series.cacheHitRate, formatRate),
    renderChart("Error Rate", "#b4432f", series.errorRate, formatRate),
  ].join("");
}

function renderTable(target, rows, valueKey) {
  if (!rows.length) {
    target.innerHTML = '<p class="empty">No data in the last 7 days.</p>';
    return;
  }

  target.innerHTML = `<div class="table">${rows.map((row) => `
    <div class="table-row">
      <strong>${row.objectKey || row.url}</strong>
      <span>${row.bucket} · ${formatCompact(row[valueKey] || 0)}</span>
    </div>
  `).join("")}</div>`;
}

async function loadFilters() {
  const filters = await request("/api/filters");
  const options = ['<option value="">All Buckets</option>']
    .concat(filters.buckets.map((bucket) => `<option value="${bucket}">${bucket}</option>`));
  bucketFilter.innerHTML = options.join("");
  bucketFilter.value = state.bucket;
}

async function refresh() {
  const query = state.bucket ? `?bucket=${encodeURIComponent(state.bucket)}` : "";
  const overview = await request(`/api/overview${query}`);
  const seriesQuery = new URLSearchParams({ range: state.range });
  if (state.bucket) seriesQuery.set("bucket", state.bucket);
  const timeseries = await request(`/api/timeseries?${seriesQuery.toString()}`);
  const top = await request(`/api/top${query}`);

  renderMetrics(overview);
  renderSummary(timeseries.summary);
  renderCharts(timeseries.series);
  renderTable(hotFiles, top.hotCacheFiles, "hits");
  renderTable(missUrls, top.missUrls, "misses");
  renderTable(errorUrls, top.errorUrls, "errors");
}

loginForm.addEventListener("submit", async (event) => {
  event.preventDefault();
  const formData = new FormData(loginForm);
  const apiKey = String(formData.get("api-key") || "");
  loginError.hidden = true;
  try {
    await login(apiKey);
    state.apiKey = apiKey;
    localStorage.setItem(storageKey, apiKey);
    showDashboard();
    await loadFilters();
    await refresh();
  } catch {
    loginError.hidden = false;
  }
});

bucketFilter.addEventListener("change", async () => {
  state.bucket = bucketFilter.value;
  await refresh();
});

document.querySelectorAll("#range-switcher button").forEach((button) => {
  button.addEventListener("click", async () => {
    document.querySelectorAll("#range-switcher button").forEach((candidate) => {
      candidate.classList.toggle("active", candidate === button);
    });
    state.range = button.dataset.range || "1h";
    await refresh();
  });
});

logoutButton.addEventListener("click", () => {
  localStorage.removeItem(storageKey);
  state.apiKey = "";
  showLogin();
});

(async function boot() {
  if (!state.apiKey) {
    showLogin();
    return;
  }

  try {
    await loadFilters();
    await refresh();
    showDashboard();
  } catch {
    showLogin();
  }
})();
