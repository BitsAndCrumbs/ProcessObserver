// ═══════════════════════════════════════════════════════════
// ProcessObserver – Main Application Logic
// Uses Tauri v2 IPC and Chart.js for real-time monitoring
// ═══════════════════════════════════════════════════════════

import { Chart, registerables } from 'chart.js';
import { invoke } from '@tauri-apps/api/core';
import { listen } from '@tauri-apps/api/event';
import { ask, message } from '@tauri-apps/plugin-dialog';

Chart.register(...registerables);

// ─── DOM References ───────────────────────────────────────
const $ = (sel) => document.querySelector(sel);
const $$ = (sel) => document.querySelectorAll(sel);

const el = {
  adminBadge: $('#admin-badge'),
  executableInput: $('#executable-input'),
  processSuggestions: $('#process-suggestions'),
  intervalSelect: $('#interval-select'),
  retentionSelect: $('#retention-select'),
  btnStart: $('#btn-start'),
  btnStop: $('#btn-stop'),
  networkToggle: $('#network-toggle'),
  networkStatus: $('#network-status'),
  tabBar: $('#tab-bar'),
  tabContent: $('#tab-content'),
};

// ─── Application State ────────────────────────────────────
const state = {
  activeSessionId: null,          // Currently viewed session
  sessions: new Map(),           // sessionId → { info, chartInstances, timer }
  isElevated: false,
};

// ─── Chart Configuration Factory ──────────────────────────
const METRIC_COLORS = {
  cpu: { line: '#58a6ff', fill: 'rgba(88,166,255,0.08)' },
  ram: { line: '#3fb950', fill: 'rgba(63,185,80,0.08)' },
  io:  { line: '#d29922', fill: 'rgba(210,153,34,0.08)' },
  net: { line: '#58a6ff', fill: 'rgba(88,166,255,0.08)' },
};

// Clearly distinguishable network line colors.
const NET_RECV_COLOR = '#58a6ff';
const NET_SENT_COLOR = '#f0883e';
const NET_CONN_COLOR = '#bc8cff';

function createChartConfig(metric, networkWfp = true) {
  const colors = METRIC_COLORS[metric];
  const datasets = [];

  if (metric === 'io') {
    datasets.push(
      {
        label: 'Read',
        borderColor: colors.line,
        backgroundColor: colors.fill,
        data: [],
        borderWidth: 1.5,
        pointRadius: 0,
        tension: 0.3,
        fill: true,
      },
      {
        label: 'Write',
        borderColor: '#39c5cf',
        backgroundColor: 'rgba(57,197,207,0.05)',
        data: [],
        borderWidth: 1.5,
        pointRadius: 0,
        tension: 0.3,
        fill: true,
      }
    );
  } else if (metric === 'net') {
    if (networkWfp) {
      datasets.push(
        {
          label: 'Received',
          borderColor: NET_RECV_COLOR,
          backgroundColor: 'rgba(88,166,255,0.08)',
          data: [],
          borderWidth: 1.5,
          pointRadius: 0,
          tension: 0.3,
          fill: true,
        },
        {
          label: 'Sent',
          borderColor: NET_SENT_COLOR,
          backgroundColor: 'rgba(240,136,62,0.06)',
          data: [],
          borderWidth: 1.5,
          pointRadius: 0,
          tension: 0.3,
          fill: true,
        }
      );
    } else {
      // Fallback: connection-count proxy metric.
      datasets.push({
        label: 'Connections',
        borderColor: NET_CONN_COLOR,
        backgroundColor: 'rgba(188,140,255,0.08)',
        data: [],
        borderWidth: 1.5,
        pointRadius: 0,
        tension: 0.3,
        fill: true,
      });
    }
  } else {
    datasets.push({
      label: metric.toUpperCase(),
      borderColor: colors.line,
      backgroundColor: colors.fill,
      data: [],
      borderWidth: 1.5,
      pointRadius: 0,
      tension: 0.3,
      fill: true,
    });
  }

  return {
    type: 'line',
    data: { labels: [], datasets },
    options: {
      responsive: true,
      maintainAspectRatio: false,
      animation: { duration: 200 },
      interaction: { intersect: false, mode: 'index' },
      plugins: {
        legend: { display: false },
        tooltip: {
          backgroundColor: '#21262d',
          titleColor: '#e6edf3',
          bodyColor: '#8b949e',
          borderColor: '#30363d',
          borderWidth: 1,
        },
      },
      scales: {
        x: {
          display: false,
        },
        y: {
          beginAtZero: true,
          grid: { color: 'rgba(48,54,61,0.5)' },
          ticks: {
            color: '#6e7681',
            font: { size: 10 },
            maxTicksLimit: 5,
            callback: (v) => {
              if (metric === 'net' && !networkWfp) return String(Math.round(v));
              return formatBytes(metric, v);
            },
          },
        },
      },
    },
  };
}

function formatBytes(metric, value) {
  if (value === undefined || value === null) return '';
  if (metric === 'cpu' || metric === 'ram') {
    return value.toFixed(1);
  }
  if (value >= 1e9) return (value / 1e9).toFixed(1) + ' GB';
  if (value >= 1e6) return (value / 1e6).toFixed(1) + ' MB';
  if (value >= 1e3) return (value / 1e3).toFixed(1) + ' KB';
  return value + ' B';
}

function createCharts(container, networkWfp = true) {
  const canvases = container.querySelectorAll('.graph-canvas');
  const instances = {};

  canvases.forEach((canvas) => {
    const metric = canvas.dataset.metric;
    const ctx = canvas.getContext('2d');
    const wfp = metric === 'net' ? networkWfp : true;
    instances[metric] = new Chart(ctx, createChartConfig(metric, wfp));
  });

  return instances;
}

// ─── Network Card / Legend Mode Helpers ─────────────────
function updateNetCardMode(view, networkEnabled, networkWfp) {
  const bytesDetails = view.querySelectorAll('.net-detail-bytes');
  const connDetail = view.querySelector('.net-detail-conn');
  const unit = view.querySelector('#stat-net-unit');

  const showBytes = networkEnabled && networkWfp;
  const showConn = networkEnabled && !networkWfp;

  bytesDetails.forEach((d) => { d.style.display = showBytes ? '' : 'none'; });
  if (connDetail) connDetail.style.display = showConn ? '' : 'none';
  if (unit) unit.textContent = showConn ? 'active connections' : 'received + sent';
}

function updateNetLegend(view, networkEnabled, networkWfp) {
  const legend = view.querySelector('.net-legend');
  if (!legend) return;
  if (networkEnabled && networkWfp) {
    legend.innerHTML =
      `<span style="color:${NET_RECV_COLOR};">● Received</span>` +
      ` <span style="color:${NET_SENT_COLOR};">● Sent</span>` +
      ` <span style="color:var(--text-secondary);">B/s</span>`;
  } else if (networkEnabled) {
    legend.innerHTML = `<span style="color:${NET_CONN_COLOR};">● Connections</span>`;
  } else {
    legend.innerHTML = `<span style="color:var(--text-tertiary);">No network data</span>`;
  }
}

function updateNetBanner(view, networkEnabled, networkWfp) {
  const banner = view.querySelector('.net-degraded-banner');
  if (banner) {
    banner.style.display = networkEnabled && !networkWfp ? '' : 'none';
  }
}

// ─── Tab Management ──────────────────────────────────────
function createTab(sessionInfo) {
  // Build tab element
  const tab = document.createElement('div');
  tab.className = 'tab';
  tab.dataset.sessionId = sessionInfo.id;
  tab.innerHTML = `
    <span class="tab-status-dot" style="background:${sessionInfo.active ? 'var(--accent-green)' : 'var(--accent-red)'}"></span>
    <span class="tab-label">${escapeHtml(sessionInfo.label)}</span>
    <span class="tab-close" title="Remove session">✕</span>
  `;

  // Click to switch to this tab
  tab.addEventListener('click', (e) => {
    if (e.target.classList.contains('tab-close')) {
      e.stopPropagation();
      removeSession(sessionInfo.id);
      return;
    }
    switchToTab(sessionInfo.id);
  });

  el.tabBar.appendChild(tab);

  // Hide placeholder
  const placeholder = el.tabBar.querySelector('.tab-placeholder');
  if (placeholder) placeholder.style.display = 'none';

  // Build content panel
  const template = document.getElementById('session-tab-template');
  const clone = template.content.cloneNode(true);
  const view = clone.querySelector('.session-view');
  view.dataset.sessionId = sessionInfo.id;
  view.style.display = 'none';
  el.tabContent.appendChild(view);

  // Hide empty state
  const emptyState = el.tabContent.querySelector('.empty-state');
  if (emptyState) emptyState.style.display = 'none';

  // Determine network mode: WFP byte counters vs fallback connection counting.
  const networkWfp = sessionInfo.network_wfp !== undefined
    ? !!sessionInfo.network_wfp
    : !!(sessionInfo.network_enabled && state.isElevated);
  sessionInfo.network_wfp = networkWfp;

  const intervalMs = sessionInfo.interval_ms || 1000;
  const retentionSecs = sessionInfo.retention_secs || 300;
  const maxPoints = Math.max(10, Math.ceil((retentionSecs * 1000) / intervalMs));

  // Create charts
  const charts = createCharts(view, networkWfp);

  // Configure network card, legend, and degraded-mode banner.
  updateNetCardMode(view, sessionInfo.network_enabled, networkWfp);
  updateNetLegend(view, sessionInfo.network_enabled, networkWfp);
  updateNetBanner(view, sessionInfo.network_enabled, networkWfp);

  // Wire export button
  const exportBtn = view.querySelector('.btn-export');
  const formatSelect = view.querySelector('.export-format-select');
  exportBtn.addEventListener('click', () => {
    exportData(sessionInfo.id, formatSelect.value);
  });

  // Store session data
  state.sessions.set(sessionInfo.id, {
    info: sessionInfo,
    view,
    charts,
    maxPoints,
    networkWfp,
  });

  // Update PID display
  updateViewInfo(sessionInfo);

  return tab;
}

function switchToTab(sessionId) {
  // Deactivate all tabs and views
  $$('.tab').forEach((t) => t.classList.remove('active'));
  $$('.session-view').forEach((v) => (v.style.display = 'none'));

  // Activate the target
  const tab = document.querySelector(`.tab[data-session-id="${sessionId}"]`);
  const view = document.querySelector(`.session-view[data-session-id="${sessionId}"]`);

  if (tab) tab.classList.add('active');
  if (view) view.style.display = '';

  state.activeSessionId = sessionId;

  // Enable stop button only if session is active
  const session = state.sessions.get(sessionId);
  if (session) {
    el.btnStop.disabled = !session.info.active;
  }

  // Resize charts
  if (session?.charts) {
    Object.values(session.charts).forEach((c) => c.resize());
  }
}

function removeTab(sessionId) {
  const tab = document.querySelector(`.tab[data-session-id="${sessionId}"]`);
  const view = document.querySelector(`.session-view[data-session-id="${sessionId}"]`);

  if (tab) tab.remove();
  if (view) view.remove();

  // Destroy chart instances to prevent memory leaks
  const session = state.sessions.get(sessionId);
  if (session?.charts) {
    Object.values(session.charts).forEach((c) => c.destroy());
  }

  state.sessions.delete(sessionId);

  if (state.activeSessionId === sessionId) {
    state.activeSessionId = null;
    el.btnStop.disabled = true;

    // Switch to the first remaining tab or show empty state
    const remaining = state.sessions.keys().next().value;
    if (remaining) {
      switchToTab(remaining);
    } else {
      const emptyState = el.tabContent.querySelector('.empty-state');
      const placeholder = el.tabBar.querySelector('.tab-placeholder');
      if (emptyState) emptyState.style.display = '';
      if (placeholder) placeholder.style.display = '';
      el.btnStop.disabled = true;
    }
  }
}

async function removeSession(sessionId) {
  try {
    await invoke('remove_session', { sessionId });
  } catch (e) {
    console.error('Failed to remove session:', e);
  }
  removeTab(sessionId);
}

// ─── Data Update Handlers ────────────────────────────────
function updateViewInfo(sessionInfo) {
  const view = document.querySelector(`.session-view[data-session-id="${sessionInfo.id}"]`);
  if (!view) return;

  // Update status dots on stat cards
  const statusClass = sessionInfo.active ? 'active' : 'stopped';
  view.querySelectorAll('.status-dot').forEach((dot) => {
    dot.className = `status-dot ${statusClass}`;
  });

  // Network dot
  const netDot = view.querySelector('[data-metric="net"] .status-dot');
  if (netDot) {
    const wfp = sessionInfo.network_wfp !== undefined ? sessionInfo.network_wfp : state.isElevated;
    const netClass = sessionInfo.network_enabled ? (wfp ? 'active' : 'warning') : 'degraded';
    netDot.className = `status-dot ${netClass}`;
  }

  // Update tab dot
  const tab = document.querySelector(`.tab[data-session-id="${sessionInfo.id}"]`);
  if (tab) {
    const dot = tab.querySelector('.tab-status-dot');
    if (dot) {
      dot.style.background = sessionInfo.active
        ? 'var(--accent-green)'
        : 'var(--accent-red)';
    }
    tab.querySelector('.tab-label').textContent = sessionInfo.label;
  }
}

function applyDataPoint(sessionId, dataPoint) {
  const session = state.sessions.get(sessionId);
  if (!session) return;

  const { view, charts } = session;
  const ts = new Date(dataPoint.timestamp).toLocaleTimeString();

  // Update stat cards
  view.querySelector('#stat-cpu').textContent = dataPoint.cpu_percent.toFixed(1);
  view.querySelector('#stat-ram').textContent = dataPoint.ram_mb.toFixed(1);

  const ioTotal = dataPoint.io_read_bytes_per_sec + dataPoint.io_write_bytes_per_sec;
  view.querySelector('#stat-io').textContent = formatByteRate(ioTotal);

  // Total (cumulative) I/O bytes for this session
  const ioCard = view.querySelector('.stat-card[data-metric="io"]');
  if (ioCard) {
    ioCard.querySelector('.total-read').textContent = formatByteSize(dataPoint.io_read_bytes_total);
    ioCard.querySelector('.total-write').textContent = formatByteSize(dataPoint.io_write_bytes_total);
  }

  // Network card: byte rates + totals (WFP) or connection count (fallback).
  const netCard = view.querySelector('.stat-card[data-metric="net"]');
  if (session.info.network_enabled) {
    if (session.info.network_wfp) {
      const netTotal = dataPoint.net_recv_bytes_per_sec + dataPoint.net_sent_bytes_per_sec;
      view.querySelector('#stat-net').textContent = formatByteRate(netTotal);
      if (netCard) {
        netCard.querySelector('.total-recv').textContent = formatByteSize(dataPoint.net_recv_bytes_total ?? 0);
        netCard.querySelector('.total-sent').textContent = formatByteSize(dataPoint.net_sent_bytes_total ?? 0);
      }
    } else {
      const conn = dataPoint.net_connection_count ?? 0;
      view.querySelector('#stat-net').textContent = String(conn);
      if (netCard) netCard.querySelector('.total-conn').textContent = String(conn);
    }
  } else {
    view.querySelector('#stat-net').textContent = 'N/A';
  }

  // Update PID display
  view.querySelector('.pid-display').textContent =
    dataPoint.active_pids?.join(', ') || '—';

  // Push data to charts
  const maxPoints = session.maxPoints || 120;

  // CPU chart
  if (charts.cpu) {
    pushChartPoint(charts.cpu, ts, [dataPoint.cpu_percent], maxPoints);
  }

  // RAM chart
  if (charts.ram) {
    pushChartPoint(charts.ram, ts, [dataPoint.ram_mb], maxPoints);
  }

  // I/O chart (two lines: read + write)
  if (charts.io) {
    pushChartPoint(charts.io, ts, [
      dataPoint.io_read_bytes_per_sec,
      dataPoint.io_write_bytes_per_sec,
    ], maxPoints);
  }

  // Network chart (recv + sent in WFP mode, connection count in fallback)
  if (charts.net) {
    if (session.info.network_wfp) {
      pushChartPoint(charts.net, ts, [
        dataPoint.net_recv_bytes_per_sec,
        dataPoint.net_sent_bytes_per_sec,
      ], maxPoints);
    } else {
      pushChartPoint(charts.net, ts, [dataPoint.net_connection_count ?? 0], maxPoints);
    }
  }

  // Update max/avg periodically (every 10 points to avoid expensive recalc)
  updateMaxAvg(sessionId);
}

function pushChartPoint(chart, label, values, maxPoints) {
  chart.data.labels.push(label);
  values.forEach((value, i) => {
    chart.data.datasets[i].data.push(value);
  });
  if (chart.data.labels.length > maxPoints) {
    chart.data.labels.shift();
    chart.data.datasets.forEach((ds) => ds.data.shift());
  }
  chart.update('none'); // 'none' for performance
}

function formatByteSize(bytes) {
  if (bytes >= 1e9) return (bytes / 1e9).toFixed(1) + ' GB';
  if (bytes >= 1e6) return (bytes / 1e6).toFixed(1) + ' MB';
  if (bytes >= 1e3) return (bytes / 1e3).toFixed(1) + ' KB';
  return bytes + ' B';
}

function formatByteRate(bytesPerSec) {
  return formatByteSize(bytesPerSec) + '/s';
}

let maxAvgUpdateCounter = 0;
function updateMaxAvg(sessionId) {
  maxAvgUpdateCounter++;
  if (maxAvgUpdateCounter % 10 !== 0) return;

  const session = state.sessions.get(sessionId);
  if (!session) return;
  const { view, charts } = session;

  // Get all current chart data to compute stats
  const datasets = {};
  for (const [metric, chart] of Object.entries(charts)) {
    const vals = chart.data.datasets.flatMap((ds) => ds.data).filter((v) => typeof v === 'number');
    if (vals.length > 0) {
      const max = Math.max(...vals);
      const avg = vals.reduce((a, b) => a + b, 0) / vals.length;
      datasets[metric] = { max, avg };
    }
  }

  // Update CPU
  if (datasets.cpu) {
    updateStatSub(view, 'cpu', datasets.cpu.max, datasets.cpu.avg);
  }
  // Update RAM
  if (datasets.ram) {
    updateStatSub(view, 'ram', datasets.ram.max, datasets.ram.avg);
  }
  // Update IO
  if (datasets.io) {
    updateStatSub(view, 'io', datasets.io.max, datasets.io.avg);
  }
  // Update NET
  if (datasets.net) {
    const isBytes = session.info.network_wfp;
    updateStatSub(view, 'net', datasets.net.max, datasets.net.avg, isBytes);
  }
}

function updateStatSub(view, metric, max, avg, isBytes = true) {
  const card = view.querySelector(`.stat-card[data-metric="${metric}"]`);
  if (!card) return;
  const maxEl = card.querySelector('.max-val');
  const avgEl = card.querySelector('.avg-val');
  if (maxEl) maxEl.textContent = formatSubValue(metric, max, isBytes);
  if (avgEl) avgEl.textContent = formatSubValue(metric, avg, isBytes);
}

function formatSubValue(metric, value, isBytes = true) {
  if (metric === 'cpu' || metric === 'ram') return value.toFixed(1);
  if (!isBytes) return String(Math.round(value));
  return formatByteRate(value);
}

// ─── Session Status Handler ──────────────────────────────
async function handleSessionStatus(event) {
  const { sessionId, status, message } = event.payload;
  const session = state.sessions.get(sessionId);
  if (!session) return;

  if (status === 'process_not_found' || status === 'process_terminated') {
    session.info.active = false;
    updateViewInfo(session.info);

    // Show status in the view
    const view = session.view;
    if (view) {
      const statCards = view.querySelectorAll('.stat-card');
      statCards.forEach((card) => {
        const dot = card.querySelector('.status-dot');
        if (dot) dot.className = 'status-dot stopped';
      });
    }
  }
}

// ─── Elapsed Timer ───────────────────────────────────────
function startElapsedTimer(sessionId) {
  const session = state.sessions.get(sessionId);
  if (!session) return;

  const update = () => {
    const view = session.view;
    if (!view) return;
    const elapsed = view.querySelector('.elapsed-display');
    if (!elapsed) return;

    const start = new Date(session.info.started_at);
    const now = new Date();
    const diff = Math.floor((now - start) / 1000);
    const h = String(Math.floor(diff / 3600)).padStart(2, '0');
    const m = String(Math.floor((diff % 3600) / 60)).padStart(2, '0');
    const s = String(diff % 60).padStart(2, '0');
    elapsed.textContent = `${h}:${m}:${s}`;
  };

  update();
  const timer = setInterval(update, 1000);
  session.timer = timer;
}

// ─── Export ──────────────────────────────────────────────
async function exportData(sessionId, format) {
  try {
    // Show a native save dialog and write the file from the backend.
    const savedPath = await invoke('export_session_to_file', { sessionId, format });
    if (savedPath) {
      console.log('Exported session data to', savedPath);
    }
    // savedPath is null when the user cancelled the dialog
  } catch (e) {
    console.error('Export failed:', e);
    await message(`Export failed: ${e}`, {
      title: 'ProcessObserver',
      kind: 'error',
    });
  }
}

// ─── Tauri Event Listeners ───────────────────────────────
async function setupEventListeners() {
  // Real-time metric updates
  await listen('metrics-update', (event) => {
    const { sessionId, dataPoint } = event.payload;
    applyDataPoint(sessionId, dataPoint);
  });

  // Session status changes
  await listen('session-status', handleSessionStatus);
}

// ─── Actions ─────────────────────────────────────────────
async function startMonitoring() {
  const executable = el.executableInput.value.trim();
  if (!executable) {
    await message('Please enter an executable name (e.g., firefox.exe).', {
      title: 'ProcessObserver',
      kind: 'warning',
    });
    return;
  }

  const intervalMs = parseInt(el.intervalSelect.value, 10);
  const retentionSecs = parseInt(el.retentionSelect.value, 10) || 300;
  const enableNetwork = el.networkToggle.checked;
  const networkWfp = enableNetwork && state.isElevated;

  // Check if network requested but not elevated
  if (enableNetwork && !state.isElevated) {
    const confirmed = await ask(
      'Network monitoring without Administrator privileges can only count ' +
      'active TCP connections (approximate — no byte-level accuracy).\n\n' +
      'Restart as Administrator for accurate per-connection byte counters?',
      { title: 'ProcessObserver', kind: 'warning' }
    );
    if (confirmed) {
      try {
        const restarted = await invoke('request_elevation', {
          executable,
          intervalMs,
          enableNetwork,
          retentionSecs,
        });
        // The app will restart — we won't reach here if successful.
        if (restarted) return;
      } catch (e) {
        console.error('Elevation request failed:', e);
      }
      // Elevation failed (e.g. UAC cancelled) — continue in fallback mode.
    }
    // Otherwise continue in fallback (degraded) mode.
  }

  try {
    const sessionId = await invoke('start_monitoring', {
      executable,
      intervalMs,
      enableNetwork,
      retentionSecs,
    });

    // Create local session tracking
    const now = new Date().toISOString();
    const sessionInfo = {
      id: sessionId,
      label: `Session – ${executable}`,
      executable,
      interval_ms: intervalMs,
      retention_secs: retentionSecs,
      active: true,
      network_enabled: enableNetwork,
      network_wfp: networkWfp,
      data_point_count: 0,
      started_at: now,
      ended_at: null,
    };

    createTab(sessionInfo);
    switchToTab(sessionId);
    startElapsedTimer(sessionId);

    // Update tab label with proper session naming
    await refreshSessions();

    el.btnStart.disabled = false;
    el.btnStop.disabled = false;
  } catch (e) {
    console.error('Failed to start monitoring:', e);
    await message(`Failed to start monitoring: ${e}`, {
      title: 'ProcessObserver',
      kind: 'error',
    });
  }
}

async function stopMonitoring() {
  if (!state.activeSessionId) return;

  try {
    await invoke('stop_monitoring', { sessionId: state.activeSessionId });
    const session = state.sessions.get(state.activeSessionId);
    if (session) {
      session.info.active = false;
      session.info.ended_at = new Date().toISOString();
      updateViewInfo(session.info);
      if (session.timer) {
        clearInterval(session.timer);
        session.timer = null;
      }
    }
    el.btnStop.disabled = true;
  } catch (e) {
    console.error('Failed to stop monitoring:', e);
  }
}

// ─── Session Refresh ─────────────────────────────────────
async function refreshSessions() {
  try {
    const sessions = await invoke('get_sessions');
    for (const info of sessions) {
      if (!state.sessions.has(info.id)) {
        createTab(info);
        startElapsedTimer(info.id);
      } else {
        const existing = state.sessions.get(info.id);
        info.network_wfp = existing.info.network_wfp !== undefined
          ? existing.info.network_wfp
          : !!(info.network_enabled && state.isElevated);
        existing.info = info;
        updateViewInfo(info);
      }
    }
  } catch (e) {
    console.error('Failed to refresh sessions:', e);
  }
}

// ─── Admin Status ────────────────────────────────────────
async function checkAdminStatus() {
  try {
    state.isElevated = await invoke('is_elevated');
    if (state.isElevated) {
      // In admin mode network monitoring is enabled by default.
      el.networkToggle.checked = true;
    }
    updateAdminBadge();
  } catch (e) {
    console.error('Failed to check admin status:', e);
  }
}

function updateAdminBadge() {
  if (state.isElevated) {
    el.adminBadge.textContent = '🛡️ Administrator';
    el.adminBadge.classList.add('elevated');
  } else {
    el.adminBadge.textContent = '🔒 Standard';
    el.adminBadge.classList.remove('elevated');
  }
}

// ─── Network Toggle ──────────────────────────────────────
function updateNetworkStatus() {
  const enabled = el.networkToggle.checked;
  if (enabled && !state.isElevated) {
    el.networkStatus.textContent = '⚠ Degraded (connection count)';
    el.networkStatus.className = 'network-status-indicator disabled';
  } else if (enabled && state.isElevated) {
    el.networkStatus.textContent = '✓ Active (byte counters)';
    el.networkStatus.className = 'network-status-indicator enabled';
  } else {
    el.networkStatus.textContent = 'Off';
    el.networkStatus.className = 'network-status-indicator disabled';
  }
}

// ─── Pending Elevation Params ───────────────────────────
async function applyPendingParams() {
  try {
    const params = await invoke('get_restart_params');
    if (!params) return;
    if (params.executable) el.executableInput.value = params.executable;
    if (params.interval_ms) el.intervalSelect.value = String(params.interval_ms);
    if (params.retention_secs) el.retentionSelect.value = String(params.retention_secs);
    el.networkToggle.checked = !!params.enable_network;
    updateNetworkStatus();
  } catch (e) {
    console.error('Failed to restore pending monitoring params:', e);
  }
}

// ─── Process Autocomplete ────────────────────────────────
async function loadProcessList() {
  try {
    const processes = await invoke('get_running_processes');
    el.processSuggestions.innerHTML = '';
    for (const name of processes) {
      const option = document.createElement('option');
      option.value = name;
      el.processSuggestions.appendChild(option);
    }
  } catch (e) {
    console.error('Failed to load process list:', e);
  }
}

// ─── Escape HTML ─────────────────────────────────────────
function escapeHtml(str) {
  const div = document.createElement('div');
  div.textContent = str;
  return div.innerHTML;
}

// ─── Initialization ──────────────────────────────────────
async function init() {
  await checkAdminStatus();
  await applyPendingParams();
  await loadProcessList();
  await setupEventListeners();
  await refreshSessions();

  // Event bindings
  el.btnStart.addEventListener('click', startMonitoring);
  el.btnStop.addEventListener('click', stopMonitoring);

  el.networkToggle.addEventListener('change', updateNetworkStatus);
  updateNetworkStatus();

  // Refresh process list when input is focused
  el.executableInput.addEventListener('focus', () => {
    loadProcessList();
  });

  // Allow Enter key to start monitoring
  el.executableInput.addEventListener('keydown', (e) => {
    if (e.key === 'Enter') startMonitoring();
  });

  console.log('ProcessObserver initialized');
}

// Initialize the application. Tauri v2 exposes its IPC bridge through the
// @tauri-apps/api package rather than the `window.__TAURI__` global.
init();
