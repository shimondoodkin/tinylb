use std::collections::HashMap;
use std::collections::VecDeque;
use std::sync::atomic::Ordering;
use std::sync::{Arc, RwLock};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use tracing::debug;

use crate::backends::BackendRegistry;

const MAX_SAMPLES: usize = 1440; // 24h at 60s intervals

#[derive(Clone)]
struct Sample {
    t: u64,
    c: usize,
    e: usize,
}

pub struct Stats {
    data: RwLock<HashMap<String, VecDeque<Sample>>>,
}

impl Stats {
    pub fn new() -> Self {
        Self {
            data: RwLock::new(HashMap::new()),
        }
    }

    fn record(&self, backend_url: &str, connections: usize, errors: usize) {
        let t = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs();

        let mut data = self.data.write().unwrap();
        let ring = data
            .entry(backend_url.to_string())
            .or_insert_with(|| VecDeque::with_capacity(MAX_SAMPLES));

        if ring.len() >= MAX_SAMPLES {
            ring.pop_front();
        }
        ring.push_back(Sample { t, c: connections, e: errors });
    }

    pub fn to_json(&self) -> String {
        let data = self.data.read().unwrap();
        let mut out = String::from("{");
        let mut first_backend = true;

        for (url, samples) in data.iter() {
            if !first_backend {
                out.push(',');
            }
            first_backend = false;

            // JSON-escape the URL
            out.push('"');
            for ch in url.chars() {
                match ch {
                    '"' => out.push_str("\\\""),
                    '\\' => out.push_str("\\\\"),
                    c => out.push(c),
                }
            }
            out.push_str("\":[");

            let mut first_sample = true;
            for s in samples.iter() {
                if !first_sample {
                    out.push(',');
                }
                first_sample = false;
                out.push_str(&format!("{{\"t\":{},\"c\":{},\"e\":{}}}", s.t, s.c, s.e));
            }
            out.push(']');
        }
        out.push('}');
        out
    }
}

pub async fn sample_loop(registry: Arc<BackendRegistry>, stats: Arc<Stats>) {
    let mut interval = tokio::time::interval(Duration::from_secs(60));
    loop {
        interval.tick().await;

        let groups = {
            let lock = registry.groups.read().unwrap();
            lock.clone()
        };

        for group in &groups {
            let backends = {
                let lock = group.backends.read().unwrap();
                lock.clone()
            };

            for backend in &backends {
                let c = backend.active_connections.load(Ordering::Relaxed);
                let e = backend.error_count.swap(0, Ordering::Relaxed);
                stats.record(&backend.url, c, e);
                debug!(url = %backend.url, connections = c, errors = e, "Sampled backend");
            }
        }
    }
}

pub fn dashboard_html() -> &'static str {
    r#"<!DOCTYPE html>
<html><head>
<meta charset="utf-8">
<title>LB Stats</title>
<script src="https://cdn.jsdelivr.net/npm/chart.js@4"></script>
<script src="https://cdn.jsdelivr.net/npm/chartjs-adapter-date-fns@3"></script>
<style>
  body { font-family: sans-serif; margin: 20px; background: #1a1a2e; color: #eee; }
  h1, h2 { color: #e94560; }
  canvas { max-width: 100%; background: #16213e; border-radius: 8px; padding: 10px; margin-bottom: 30px; }
</style>
</head><body>
<h1>LB Stats</h1>
<h2>Active Connections</h2>
<canvas id="connChart"></canvas>
<h2>Errors per Minute</h2>
<canvas id="errChart"></canvas>
<script>
const COLORS = ['#e94560','#0f3460','#53d8fb','#e7c664','#a3de83','#f67280','#c06c84','#6c5b7b'];
let connChart, errChart;

function buildDatasets(data, field) {
  const datasets = [];
  let i = 0;
  for (const [url, samples] of Object.entries(data)) {
    datasets.push({
      label: url,
      data: samples.map(s => ({ x: s.t * 1000, y: s[field] })),
      borderColor: COLORS[i % COLORS.length],
      backgroundColor: COLORS[i % COLORS.length] + '33',
      fill: true,
      tension: 0.3,
      pointRadius: 0,
    });
    i++;
  }
  return datasets;
}

function chartOpts(yLabel) {
  return {
    scales: {
      x: { type: 'time', time: { unit: 'hour' }, ticks: { color: '#aaa' }, grid: { color: '#333' } },
      y: { beginAtZero: true, title: { display: true, text: yLabel, color: '#aaa' }, ticks: { color: '#aaa', stepSize: 1 }, grid: { color: '#333' } }
    },
    plugins: { legend: { labels: { color: '#eee' } } },
    animation: false,
  };
}

async function load() {
  const resp = await fetch('/_lb/stats');
  const data = await resp.json();

  const connDs = buildDatasets(data, 'c');
  const errDs = buildDatasets(data, 'e');

  if (connChart) {
    connChart.data.datasets = connDs;
    connChart.update();
  } else {
    connChart = new Chart(document.getElementById('connChart'), {
      type: 'line', data: { datasets: connDs }, options: chartOpts('Active Connections')
    });
  }

  if (errChart) {
    errChart.data.datasets = errDs;
    errChart.update();
  } else {
    errChart = new Chart(document.getElementById('errChart'), {
      type: 'line', data: { datasets: errDs }, options: chartOpts('Errors / min')
    });
  }
}

load();
setInterval(load, 60000);
</script>
</body></html>"#
}
