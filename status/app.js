const storageKey = "r2_proxy_status_api_key";

const state = {
  apiKey: localStorage.getItem(storageKey) || "",
  range: "1h",
  bucket: "",
};
const errorMetricKeys = {
  totalRate: "errorRate",
  notFoundRate: "notFoundErrorRate",
  serverRate: "serverErrorRate",
};
const chartInstances = [];

const loginPanel = document.getElementById("login-panel");
const dashboard = document.getElementById("dashboard");
const loginForm = document.getElementById("login-form");
const loginError = document.getElementById("login-error");
const dashboardError = document.getElementById("dashboard-error");
const bucketFilter = document.getElementById("bucket-filter");
const logoutButton = document.getElementById("logout-button");
const metrics = document.getElementById("metrics");
const summaryStrip = document.getElementById("summary-strip");
const charts = document.getElementById("charts");
const hotFiles = document.getElementById("hot-files");
const missUrls = document.getElementById("miss-urls");
const notFoundUrls = document.getElementById("not-found-urls");
const serverErrorUrls = document.getElementById("server-error-urls");

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

function showDashboardError(message) {
  dashboardError.textContent = message;
  dashboardError.hidden = false;
}

function clearDashboardError() {
  dashboardError.textContent = "";
  dashboardError.hidden = true;
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
  document.body.dataset.view = "login";
  loginPanel.hidden = false;
  dashboard.hidden = true;
}

function showDashboard() {
  document.body.dataset.view = "dashboard";
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
    metricCard(
      "Errors",
      formatCompact(overview.totals.errors),
      `404 ${formatCompact(overview.totals.notFoundErrors)} · 5xx ${formatCompact(overview.totals.serverErrors)}`
    ),
    metricCard("Local Cache", formatBytes(overview.localCache.usedBytes), `${formatRate(overview.localCache.usageRate)} of ${formatBytes(overview.localCache.capacityBytes)}`),
  ].join("");
}

function renderSummary(summary) {
  summaryStrip.innerHTML = [
    ["Requests", formatCompact(summary.requests)],
    ["Traffic", formatBytes(summary.bytes)],
    ["Hit Rate", formatRate(summary.cacheHitRate)],
    ["Error Rate", formatRate(summary[errorMetricKeys.totalRate])],
    ["404 Rate", formatRate(summary[errorMetricKeys.notFoundRate])],
    ["5xx Rate", formatRate(summary[errorMetricKeys.serverRate])],
  ].map(([label, value]) => `
      <div class="summary-item">
        <span>${label}</span>
        <strong>${value}</strong>
      </div>
    `).join("");
}

function chartCardMarkup(id, title, points, formatter) {
  const values = points.map((point) => point.value);
  return `
    <article class="chart-card">
      <p class="metric-label">${title}</p>
      <div id="${id}" class="chart-canvas"></div>
      <footer>
        <span>min ${formatter(Math.min(...values, 0))}</span>
        <span>max ${formatter(Math.max(...values, 0))}</span>
      </footer>
    </article>
  `;
}

function renderCharts(series) {
  chartInstances.splice(0).forEach((chart) => chart.destroy());
  charts.innerHTML = [
    chartCardMarkup("chart-qps", "QPS", series.qps, (value) => value.toFixed(2)),
    chartCardMarkup("chart-throughput", "Throughput", series.throughputBytesPerSec, (value) => formatBytes(value)),
    chartCardMarkup("chart-hit-rate", "Cache Hit Rate", series.cacheHitRate, formatRate),
    chartCardMarkup("chart-404-rate", "404 Rate", series[errorMetricKeys.notFoundRate], formatRate),
    chartCardMarkup("chart-5xx-rate", "5xx Rate", series[errorMetricKeys.serverRate], formatRate),
  ].join("");

  [
    {
      id: "chart-qps",
      color: "#b46a1c",
      points: series.qps,
      formatter: (value) => value.toFixed(2),
    },
    {
      id: "chart-throughput",
      color: "#1f8f68",
      points: series.throughputBytesPerSec,
      formatter: (value) => formatBytes(value),
    },
    {
      id: "chart-hit-rate",
      color: "#182126",
      points: series.cacheHitRate,
      formatter: formatRate,
    },
    {
      id: "chart-404-rate",
      color: "#c0841a",
      points: series[errorMetricKeys.notFoundRate],
      formatter: formatRate,
    },
    {
      id: "chart-5xx-rate",
      color: "#b4432f",
      points: series[errorMetricKeys.serverRate],
      formatter: formatRate,
    },
  ].forEach(({ id, color, points, formatter }) => {
    if (!window.ApexCharts) {
      return;
    }

    const chart = new window.ApexCharts(document.getElementById(id), {
      chart: {
        type: "line",
        height: 240,
        toolbar: { show: false },
        zoom: { enabled: false },
        animations: { easing: "easeinout", speed: 260 },
        sparkline: { enabled: false },
        fontFamily: '"IBM Plex Mono", monospace',
      },
      series: [
        {
          name: id,
          data: points.map((point) => ({ x: point.ts * 1000, y: point.value })),
        },
      ],
      colors: [color],
      stroke: {
        curve: "smooth",
        width: 3,
      },
      grid: {
        borderColor: "rgba(24, 33, 38, 0.08)",
        strokeDashArray: 4,
      },
      markers: {
        size: 0,
        hover: { size: 4 },
      },
      tooltip: {
        theme: "light",
        x: {
          format: state.range === "1h" ? "HH:mm" : state.range === "24h" ? "MM-dd HH:mm" : "yyyy-MM-dd",
        },
        y: {
          formatter,
        },
      },
      xaxis: {
        type: "datetime",
        labels: {
          datetimeUTC: false,
          style: {
            colors: "#5a666f",
          },
        },
        axisBorder: { show: false },
        axisTicks: { show: false },
      },
      yaxis: {
        labels: {
          formatter,
          style: {
            colors: "#5a666f",
          },
        },
      },
      dataLabels: { enabled: false },
      legend: { show: false },
      theme: { mode: "light" },
    });

    chart.render();
    chartInstances.push(chart);
  });
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
  renderTable(notFoundUrls, top.notFoundUrls, "errors");
  renderTable(serverErrorUrls, top.serverErrorUrls, "errors");
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
    clearDashboardError();
    loginForm.reset();
    try {
      await loadFilters();
      await refresh();
    } catch (error) {
      showDashboardError("Dashboard unlocked, but the first data load failed. Try refreshing in a moment.");
      console.error(error);
    }
  } catch {
    loginError.hidden = false;
  }
});

bucketFilter.addEventListener("change", async () => {
  state.bucket = bucketFilter.value;
  try {
    clearDashboardError();
    await refresh();
  } catch (error) {
    showDashboardError("Failed to refresh bucket data.");
    console.error(error);
  }
});

document.querySelectorAll("#range-switcher button").forEach((button) => {
  button.addEventListener("click", async () => {
    document.querySelectorAll("#range-switcher button").forEach((candidate) => {
      candidate.classList.toggle("active", candidate === button);
    });
    state.range = button.dataset.range || "1h";
    try {
      clearDashboardError();
      await refresh();
    } catch (error) {
      showDashboardError("Failed to refresh the selected time range.");
      console.error(error);
    }
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
    clearDashboardError();
    showDashboard();
  } catch (error) {
    if (state.apiKey) {
      showDashboard();
      showDashboardError("Stored API key is valid, but the dashboard data is temporarily unavailable.");
      console.error(error);
    } else {
      showLogin();
    }
  }
})();
