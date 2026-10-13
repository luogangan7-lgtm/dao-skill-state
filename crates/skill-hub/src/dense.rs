//! dense — optional semantic recall (bge-m3 via a local Ollama) fused with BM25 by RRF.
//!
//! Depends on: std only (a minimal HTTP/1.0 client to localhost; no new crates).
//! Called by: `main` (`index --embed` writes the sidecar; `search`/`hook` fuse when it exists).
//! Invariants:
//! - Prompts are only ever sent to a loopback address (127.0.0.1 / ::1 / localhost); any other
//!   SKILL_HUB_OLLAMA is refused, so enabling dense recall never ships prompts off the machine.
//! - Dense recall is an addition, never a dependency: no sidecar, a sidecar that does not match
//!   the registry, Ollama down or slower than the timeout → plain BM25, same as before.
//! - The sidecar (`<registry>.emb`) is bound to the registry by a hash of the skill ids and
//!   entry hashes in registry order, so a re-index without `--embed` cannot pair stale vectors
//!   with different skills.

use sha2::{Digest, Sha256};
use std::io::{Read, Write};
use std::net::{TcpStream, ToSocketAddrs};
use std::time::{Duration, Instant};

pub const MAGIC: &str = "skill-hub-emb/1";

pub fn model() -> String {
    std::env::var("SKILL_HUB_EMBED_MODEL").unwrap_or_else(|_| "bge-m3".into())
}

fn endpoint() -> Result<(String, u16), String> {
    let url = std::env::var("SKILL_HUB_OLLAMA").unwrap_or_else(|_| "http://127.0.0.1:11434".into());
    let rest = url
        .strip_prefix("http://")
        .ok_or("SKILL_HUB_OLLAMA must be http://host:port")?;
    let hostport = rest.trim_end_matches('/');
    let (host, port) = hostport
        .rsplit_once(':')
        .ok_or("SKILL_HUB_OLLAMA needs a port")?;
    let host = host.trim_start_matches('[').trim_end_matches(']');
    if !matches!(host, "127.0.0.1" | "::1" | "localhost") {
        return Err(format!("refusing non-loopback embedding host {host}"));
    }
    Ok((host.to_string(), port.parse().map_err(|_| "bad port")?))
}

/// Embed `texts` with the local Ollama; unit-normalised vectors in input order.
pub fn embed(texts: &[String], timeout: Duration) -> Result<Vec<Vec<f32>>, String> {
    let (host, port) = endpoint()?;
    let deadline = Instant::now() + timeout;
    let left = || {
        deadline
            .checked_duration_since(Instant::now())
            .filter(|d| !d.is_zero())
    };
    // keep the model loaded between prompts so the hook does not pay a cold start
    let body =
        serde_json::json!({"model": model(), "input": texts, "keep_alive": "30m"}).to_string();
    // every address the name resolves to, loopback only (`localhost` may be ::1 first while
    // Ollama listens on 127.0.0.1); a name resolving off-machine is never contacted
    let addrs: Vec<_> = (host.as_str(), port)
        .to_socket_addrs()
        .map_err(|e| e.to_string())?
        .filter(|a| a.ip().is_loopback())
        .collect();
    let mut s = addrs
        .iter()
        .find_map(|a| TcpStream::connect_timeout(a, left()?).ok())
        .ok_or("ollama: no loopback address answered")?;
    s.set_write_timeout(left()).map_err(|e| e.to_string())?;
    // HTTP/1.0: the server closes after one response, so no chunked decoding is needed
    write!(
        s,
        "POST /api/embed HTTP/1.0\r\nHost: {host}\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}",
        body.len()
    )
    .map_err(|e| format!("ollama: {e}"))?;
    // one overall deadline and a size cap, however the server dribbles or floods
    let mut raw = Vec::new();
    let mut buf = [0u8; 64 * 1024];
    loop {
        let Some(rest) = left() else {
            return Err("ollama: deadline exceeded".into());
        };
        s.set_read_timeout(Some(rest)).map_err(|e| e.to_string())?;
        match s.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => {
                raw.extend_from_slice(&buf[..n]);
                if raw.len() > MAX_RESPONSE {
                    return Err("ollama: response too large".into());
                }
            }
            Err(e) => return Err(format!("ollama: {e}")),
        }
    }
    let text = String::from_utf8_lossy(&raw);
    let (head, payload) = text
        .split_once("\r\n\r\n")
        .ok_or("ollama: malformed response")?;
    if !head.starts_with("HTTP/1.") || !head.split_whitespace().nth(1).is_some_and(|c| c == "200") {
        return Err(format!("ollama: {}", head.lines().next().unwrap_or("")));
    }
    let v: serde_json::Value = serde_json::from_str(payload).map_err(|e| format!("ollama: {e}"))?;
    let rows = v["embeddings"].as_array().ok_or("ollama: no embeddings")?;
    if rows.len() != texts.len() {
        return Err("ollama: wrong number of embeddings".into());
    }
    rows.iter()
        .map(|r| {
            let mut x = Vec::new();
            for f in r.as_array().ok_or("ollama: bad vector")? {
                let v = f.as_f64().map(|v| v as f32).filter(|v| v.is_finite());
                x.push(v.ok_or("ollama: non-finite value in an embedding")?);
            }
            let n = x.iter().map(|a| a * a).sum::<f32>().sqrt();
            if !n.is_finite() || n == 0.0 {
                return Err("ollama: degenerate embedding".to_string());
            }
            x.iter_mut().for_each(|a| *a /= n);
            Ok(x)
        })
        .collect()
}

/// Largest Ollama response read (an index batch of 64 × 1024 floats is ~1.5 MB).
const MAX_RESPONSE: usize = 32 << 20;

/// `bge-m3` and `bge-m3:latest` name the same Ollama model.
pub fn same_model(a: &str, b: &str) -> bool {
    a.trim_end_matches(":latest") == b.trim_end_matches(":latest")
}

/// Binds a sidecar to the registry it was built for.
pub fn registry_key(ids_and_hashes: &[(String, String)]) -> String {
    let mut h = Sha256::new();
    for (id, e) in ids_and_hashes {
        h.update(format!("{id}\0{e}\n"));
    }
    h.finalize().iter().map(|b| format!("{b:02x}")).collect()
}

/// `MAGIC\n{json header}\n` then n × dim little-endian f32.
pub fn write_sidecar(path: &str, key: &str, vecs: &[Vec<f32>]) -> Result<(), String> {
    let dim = vecs.first().map_or(0, Vec::len);
    let header =
        serde_json::json!({"model": model(), "dim": dim, "n": vecs.len(), "registry": key});
    let mut out = format!("{MAGIC}\n{header}\n").into_bytes();
    for v in vecs {
        if v.len() != dim {
            return Err("embeddings of different sizes".into());
        }
        v.iter()
            .for_each(|f| out.extend_from_slice(&f.to_le_bytes()));
    }
    let tmp = format!("{path}.tmp.{}", std::process::id());
    std::fs::write(&tmp, out)
        .and_then(|_| std::fs::rename(&tmp, path))
        .map_err(|e| {
            let _ = std::fs::remove_file(&tmp);
            format!("{path}: {e}")
        })
}

pub struct Sidecar {
    pub model: String,
    pub dim: usize,
    pub vecs: Vec<f32>,
}

/// Load a sidecar if it exists and belongs to this registry (by key and count).
pub fn read_sidecar(path: &str, key: &str, n: usize) -> Option<Sidecar> {
    let bytes = std::fs::read(path).ok()?;
    let rest = bytes.strip_prefix(format!("{MAGIC}\n").as_bytes())?;
    let nl = rest.iter().position(|b| *b == b'\n')?;
    let h: serde_json::Value = serde_json::from_slice(&rest[..nl]).ok()?;
    let (dim, count) = (h["dim"].as_u64()? as usize, h["n"].as_u64()? as usize);
    let data = &rest[nl + 1..];
    if h["registry"].as_str()? != key || count != n || data.len() != n * dim * 4 {
        return None;
    }
    let vecs: Vec<f32> = data
        .chunks_exact(4)
        .map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]]))
        .collect();
    if vecs.iter().any(|v| !v.is_finite()) {
        return None; // a corrupt sidecar must not rank anything
    }
    Some(Sidecar {
        model: h["model"].as_str()?.to_string(),
        dim,
        vecs,
    })
}

impl Sidecar {
    /// Cosine similarity of `q` with every skill (vectors are unit-normalised).
    pub fn cosines(&self, q: &[f32]) -> Vec<f32> {
        self.vecs
            .chunks_exact(self.dim)
            .map(|v| v.iter().zip(q).map(|(a, b)| a * b).sum())
            .collect()
    }
}

/// Reciprocal-rank fusion of a lexical ranking (best first) and a dense ranking (best first):
/// score = 1/(K + rank_lex) + 1/(K + rank_dense), K = 60. Returns skill indexes, best first.
pub fn rrf(lexical: &[usize], dense: &[usize]) -> Vec<usize> {
    const K: f64 = 60.0;
    let mut score: std::collections::HashMap<usize, f64> = std::collections::HashMap::new();
    for list in [lexical, dense] {
        for (r, &i) in list.iter().enumerate() {
            *score.entry(i).or_default() += 1.0 / (K + r as f64 + 1.0);
        }
    }
    let mut out: Vec<usize> = score.keys().copied().collect();
    out.sort_by(|a, b| score[b].total_cmp(&score[a]).then(a.cmp(b)));
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rrf_rewards_agreement_and_sidecar_is_bound() {
        assert_eq!(rrf(&[1, 2, 3], &[2, 4]), vec![2, 1, 4, 3]);
        let dir = std::env::temp_dir().join(format!("skill-hub-emb-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("r.emb").to_string_lossy().into_owned();
        write_sidecar(&p, "k1", &[vec![1.0, 0.0], vec![0.0, 1.0]]).unwrap();
        let s = read_sidecar(&p, "k1", 2).unwrap();
        assert_eq!(s.cosines(&[1.0, 0.0]), vec![1.0, 0.0]);
        assert!(read_sidecar(&p, "k2", 2).is_none(), "another registry");
        assert!(read_sidecar(&p, "k1", 3).is_none(), "another size");
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn prompts_never_leave_the_machine() {
        std::env::set_var("SKILL_HUB_OLLAMA", "http://example.com:11434");
        assert!(embed(&["x".into()], Duration::from_millis(50))
            .unwrap_err()
            .contains("non-loopback"));
        std::env::set_var("SKILL_HUB_OLLAMA", "http://127.0.0.1:1");
        assert!(
            embed(&["x".into()], Duration::from_millis(50)).is_err(),
            "down → error, caller falls back"
        );
        // a server that dribbles bytes is cut off at the overall deadline, not per read
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = l.local_addr().unwrap().port();
        std::thread::spawn(move || {
            if let Ok((mut c, _)) = l.accept() {
                for _ in 0..100 {
                    if c.write_all(b"x").is_err() {
                        break;
                    }
                    std::thread::sleep(Duration::from_millis(100));
                }
            }
        });
        std::env::set_var("SKILL_HUB_OLLAMA", format!("http://localhost:{port}"));
        let t0 = Instant::now();
        let e = embed(&["x".into()], Duration::from_millis(600)).unwrap_err();
        assert!(
            t0.elapsed() < Duration::from_secs(3),
            "{e} after {:?}",
            t0.elapsed()
        );
        // a flood is capped
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = l.local_addr().unwrap().port();
        std::thread::spawn(move || {
            if let Ok((mut c, _)) = l.accept() {
                let chunk = vec![b'x'; 1 << 20];
                for _ in 0..64 {
                    if c.write_all(&chunk).is_err() {
                        break;
                    }
                }
            }
        });
        std::env::set_var("SKILL_HUB_OLLAMA", format!("http://127.0.0.1:{port}"));
        assert!(embed(&["x".into()], Duration::from_secs(20))
            .unwrap_err()
            .contains("too large"));
        std::env::remove_var("SKILL_HUB_OLLAMA");
    }
}
