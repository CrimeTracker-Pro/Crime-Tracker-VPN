//! Minimal Docker Engine API client over `/var/run/docker.sock`.
//!
//! The `server_health` emitter needs to report CPU%, memory, and network
//! I/O for the *VPN host container* (the api-dev / api container that owns
//! the `wg0` interface) — not the worker's own cgroup, which is what
//! sysinfo would otherwise give us. The canonical way to read those is the
//! cgroup files Docker exposes via `GET /containers/{id}/stats`. We do not
//! pull `bollard` for this — a 50-line HTTP-over-Unix-socket request is
//! enough and keeps the dependency graph lean.
//!
//! Single-shot mode only: we issue the request with `stream=false` so the
//! engine returns one JSON object with `precpu_stats` populated, which is
//! what `docker stats` itself uses to compute the rolling CPU%.

use std::path::Path;

use serde::Deserialize;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::UnixStream;

const DEFAULT_SOCKET: &str = "/var/run/docker.sock";

/// Subset of `/containers/{id}/stats?stream=false` we read. Fields we don't
/// touch are skipped — `Deserialize` ignores unknowns by default.
#[derive(Debug, Deserialize)]
pub struct DockerStats {
    pub cpu_stats: CpuStats,
    pub precpu_stats: CpuStats,
    pub memory_stats: MemoryStats,
    /// Per-interface RX/TX counters. Keys are interface names visible in
    /// the container's netns (`eth0`, `wg0`, …). Cumulative since container
    /// start. Absent on stopped containers.
    #[serde(default)]
    pub networks: std::collections::HashMap<String, NetIo>,
}

#[derive(Debug, Default, Deserialize)]
pub struct CpuStats {
    #[serde(default)]
    pub cpu_usage: CpuUsage,
    /// Total host system CPU time across all cores (nanoseconds). Used in
    /// the docker-stats CPU% formula. May be missing on cgroup v2 — we
    /// gracefully treat 0 as "no rate available yet".
    #[serde(default)]
    pub system_cpu_usage: u64,
    #[serde(default)]
    pub online_cpus: u32,
}

#[derive(Debug, Default, Deserialize)]
pub struct CpuUsage {
    /// Cumulative nanoseconds of CPU time consumed by the container.
    #[serde(default)]
    pub total_usage: u64,
}

#[derive(Debug, Default, Deserialize)]
pub struct MemoryStats {
    /// Cumulative bytes of memory in use, including page cache.
    #[serde(default)]
    pub usage: u64,
    /// Memory cap from the container's cgroup. When uncapped, Docker
    /// reports the host's total memory here (matches `docker stats`).
    #[serde(default)]
    pub limit: u64,
    /// Granular cgroup memory counters. We subtract `inactive_file` (cgroup
    /// v2) or `cache` (cgroup v1) from `usage` to get the "real" RSS-style
    /// figure that `docker stats` shows.
    #[serde(default)]
    pub stats: MemoryDetail,
}

#[derive(Debug, Default, Deserialize)]
pub struct MemoryDetail {
    /// cgroup v1: file-backed page cache attributable to this container.
    #[serde(default)]
    pub cache: u64,
    /// cgroup v2: file-backed cache pages not yet active. `docker stats`
    /// subtracts this from `usage`.
    #[serde(default)]
    pub inactive_file: u64,
}

#[derive(Debug, Default, Deserialize, Clone, Copy)]
pub struct NetIo {
    pub rx_bytes: u64,
    pub tx_bytes: u64,
}

impl DockerStats {
    /// Apply the `docker stats` CPU% formula: ratio of container CPU time
    /// since the previous sample to host CPU time since the same instant,
    /// multiplied by the number of cores. Returns 0 when either delta is
    /// non-positive (the API was queried before precpu_stats populated, or
    /// the container is paused).
    pub fn cpu_pct(&self) -> f32 {
        let cpu_delta = self
            .cpu_stats
            .cpu_usage
            .total_usage
            .saturating_sub(self.precpu_stats.cpu_usage.total_usage);
        let sys_delta = self
            .cpu_stats
            .system_cpu_usage
            .saturating_sub(self.precpu_stats.system_cpu_usage);
        if cpu_delta == 0 || sys_delta == 0 {
            return 0.0;
        }
        let cores = self.cpu_stats.online_cpus.max(1) as f32;
        (cpu_delta as f32 / sys_delta as f32) * cores * 100.0
    }

    /// Memory in use minus reclaimable file-backed cache — matches the
    /// "Mem" column in `docker stats`. Saturates at zero if the cache
    /// counter ever exceeds usage (shouldn't, but defensive).
    pub fn mem_used_real(&self) -> u64 {
        let usage = self.memory_stats.usage;
        // cgroup v2 reports inactive_file; v1 reports cache. Prefer
        // inactive_file when present, otherwise fall back.
        let to_subtract = if self.memory_stats.stats.inactive_file > 0 {
            self.memory_stats.stats.inactive_file
        } else {
            self.memory_stats.stats.cache
        };
        usage.saturating_sub(to_subtract)
    }

    pub fn mem_limit(&self) -> u64 {
        self.memory_stats.limit
    }

}

/// Connect to the Docker engine over the local socket and fetch one stats
/// sample. Returns `None` when the socket isn't reachable (not running in
/// Docker, or the socket wasn't mounted into this container) so the caller
/// can gracefully fall back to sysinfo without spamming errors every tick.
pub async fn fetch(container_name: &str) -> anyhow::Result<Option<DockerStats>> {
    let socket = std::env::var("DOCKER_HOST")
        .ok()
        .and_then(|s| s.strip_prefix("unix://").map(str::to_string))
        .unwrap_or_else(|| DEFAULT_SOCKET.to_string());
    if !Path::new(&socket).exists() {
        return Ok(None);
    }
    let mut stream = UnixStream::connect(&socket).await?;
    // HTTP/1.1 + Connection: close: HTTP/1.0 makes the Docker engine
    // return an empty 500 from its router for /stats (it specifically
    // serves stats over HTTP/1.1). The Connection: close header keeps us
    // off keep-alive and chunked-streaming so we can just read-to-EOF.
    let req = format!(
        "GET /containers/{container_name}/stats?stream=false HTTP/1.1\r\n\
         Host: localhost\r\n\
         Accept: application/json\r\n\
         Connection: close\r\n\
         \r\n"
    );
    stream.write_all(req.as_bytes()).await?;

    let mut buf = Vec::with_capacity(8192);
    stream.read_to_end(&mut buf).await?;
    let header_end = find_header_end(&buf)
        .ok_or_else(|| anyhow::anyhow!("malformed http response from docker"))?;
    let status_line = std::str::from_utf8(&buf[..find_crlf(&buf).unwrap_or(buf.len())])
        .unwrap_or_default();
    if !status_line.contains(" 200 ") {
        // Include up to 200 chars of body for diagnosis (typically a tiny
        // JSON `{"message":"..."}` from the engine on 404 / 500).
        let body_preview = std::str::from_utf8(
            &buf[header_end..header_end.saturating_add(200).min(buf.len())],
        )
        .unwrap_or("");
        anyhow::bail!("docker stats: {status_line} body={body_preview}");
    }
    // HTTP/1.1 with Connection: close on /stats?stream=false: docker
    // sends the JSON either with Content-Length or with chunked transfer
    // encoding. Strip chunk framing if present.
    let body_bytes = &buf[header_end..];
    let body_owned: Vec<u8>;
    let body = if is_chunked(&buf[..header_end]) {
        body_owned = decode_chunked(body_bytes);
        body_owned.as_slice()
    } else {
        body_bytes
    };
    let stats: DockerStats = serde_json::from_slice(body)?;
    Ok(Some(stats))
}

fn is_chunked(headers: &[u8]) -> bool {
    let s = std::str::from_utf8(headers).unwrap_or("").to_ascii_lowercase();
    s.contains("transfer-encoding: chunked")
}

/// Decode HTTP/1.1 chunked transfer-encoded body to raw bytes. Each chunk
/// is `<hex-size>\r\n<bytes>\r\n`; a `0\r\n\r\n` chunk ends the stream.
/// Returns whatever decoded successfully — defensive against malformed
/// input (we'd rather return a partial JSON that fails to parse cleanly
/// than panic mid-tick).
fn decode_chunked(input: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(input.len());
    let mut i = 0;
    while i < input.len() {
        let line_end = match input[i..].windows(2).position(|w| w == b"\r\n") {
            Some(p) => i + p,
            None => break,
        };
        let size_str = std::str::from_utf8(&input[i..line_end]).unwrap_or("");
        // Chunk-extensions come after `;` — ignore them.
        let size_hex = size_str.split(';').next().unwrap_or("").trim();
        let chunk_size = match usize::from_str_radix(size_hex, 16) {
            Ok(n) => n,
            Err(_) => break,
        };
        i = line_end + 2;
        if chunk_size == 0 {
            break;
        }
        let chunk_end = i + chunk_size;
        if chunk_end > input.len() {
            break;
        }
        out.extend_from_slice(&input[i..chunk_end]);
        i = chunk_end + 2; // skip trailing CRLF
    }
    out
}

fn find_crlf(buf: &[u8]) -> Option<usize> {
    buf.windows(2).position(|w| w == b"\r\n")
}

fn find_header_end(buf: &[u8]) -> Option<usize> {
    buf.windows(4).position(|w| w == b"\r\n\r\n").map(|i| i + 4)
}
