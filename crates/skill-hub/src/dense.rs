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
use std::time::Instant;

pub const MAGIC: &str = "skill-hub-emb/1";

pub fn model() -> String {
    std::env::var("SKILL_HUB_EMBED_MODEL").unwrap_or_else(|_| "bge-m3".into())
}

fn endpoint() -> Result<(String, u16), String> {
    let url = std::env::var("SKILL_HUB_OLLAMA").unwrap_or_else(|_| "http://127.0.0.1:11434".into());
    loopback("SKILL_HUB_OLLAMA", &url)
}

/// `http://host:port` from `var`, refused unless the host is loopback.
fn loopback(var: &str, url: &str) -> Result<(String, u16), String> {
    let rest = url
        .strip_prefix("http://")
        .ok_or(format!("{var} must be http://host:port"))?;
    let hostport = rest.trim_end_matches('/');
    let (host, port) = hostport
        .rsplit_once(':')
        .ok_or(format!("{var} needs a port"))?;
    let host = host.trim_start_matches('[').trim_end_matches(']');
    if !matches!(host, "127.0.0.1" | "::1" | "localhost") {
        return Err(format!("refusing non-loopback host {host} in {var}"));
    }
    Ok((host.to_string(), port.parse().map_err(|_| "bad port")?))
}

/// One HTTP/1.0 exchange with the local Ollama, bounded by `deadline`; the JSON body.
fn call(
    method: &str,
    path: &str,
    body: &str,
    deadline: Instant,
) -> Result<serde_json::Value, String> {
    call_at("ollama", endpoint()?, method, path, body, deadline)
}

/// Relevance of each `doc` to `query` from a local cross-encoder server (`SKILL_HUB_RERANK`,
/// loopback only), in input order. The API is text-embeddings-inference's `/rerank`:
/// `{"query", "texts"}` -> `[{"index", "score"}]`; `skill-hub/tools/rerank_server.py` serves it.
pub fn rerank(query: &str, docs: &[String], deadline: Instant) -> Result<Vec<f32>, String> {
    let url = std::env::var("SKILL_HUB_RERANK").map_err(|_| "SKILL_HUB_RERANK is not set")?;
    let at = loopback("SKILL_HUB_RERANK", &url)?;
    let body = serde_json::json!({"query": query, "texts": docs}).to_string();
    let v = call_at("rerank", at, "POST", "/rerank", &body, deadline)?;
    let mut out = vec![f32::NAN; docs.len()];
    for r in v.as_array().ok_or("rerank: expected a list")? {
        let i = r["index"].as_u64().ok_or("rerank: hit without index")? as usize;
        let s = r["score"]
            .as_f64()
            .map(|s| s as f32)
            // tau is a probability: a raw logit or anything outside [0,1] is a bad answer
            .filter(|s| s.is_finite() && (0.0..=1.0).contains(s));
        let slot = out.get_mut(i).ok_or("rerank: index out of range")?;
        if !slot.is_nan() {
            return Err("rerank: duplicate index".into());
        }
        *slot = s.ok_or("rerank: bad score")?;
    }
    if out.iter().any(|s| s.is_nan()) {
        return Err("rerank: not every text was scored".into());
    }
    Ok(out)
}

fn call_at(
    who: &str,
    (host, port): (String, u16),
    method: &str,
    path: &str,
    body: &str,
    deadline: Instant,
) -> Result<serde_json::Value, String> {
    let left = || {
        deadline
            .checked_duration_since(Instant::now())
            .filter(|d| !d.is_zero())
    };
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
        .ok_or(format!("{who}: no loopback address answered"))?;
    s.set_write_timeout(left()).map_err(|e| e.to_string())?;
    // a first bound on reads too, so a read can never outlive the deadline by more than this
    s.set_read_timeout(left()).map_err(|e| e.to_string())?;
    // HTTP/1.0: the server closes after one response, so no chunked decoding is needed
    // one buffer, one write: the request is never split across a partial write
    let req = format!(
        "{method} {path} HTTP/1.0\r\nHost: {host}\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}",
        body.len()
    );
    // the deadline bounds the write too: a server draining slowly can't stretch it chunk by chunk
    let mut sent = req.as_bytes();
    while !sent.is_empty() {
        let Some(rest) = left() else {
            return Err(format!("{who}: deadline exceeded"));
        };
        let _ = s.set_write_timeout(Some(rest));
        match s.write(sent) {
            Ok(0) => return Err(format!("{who}: connection closed")),
            Ok(n) => sent = &sent[n..],
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
            Err(e) => return Err(format!("{who}: {e}")),
        }
    }
    // one overall deadline and a size cap, however the server dribbles or floods
    let mut raw = Vec::new();
    let mut buf = [0u8; 64 * 1024];
    loop {
        let Some(rest) = left() else {
            return Err(format!("{who}: deadline exceeded"));
        };
        // macOS refuses SO_RCVTIMEO (EINVAL) once the peer has closed; the read then returns
        // at once, so the error is harmless and must not fail an answer already received
        let _ = s.set_read_timeout(Some(rest));
        match s.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => {
                raw.extend_from_slice(&buf[..n]);
                if raw.len() > MAX_RESPONSE {
                    return Err(format!("{who}: response too large"));
                }
            }
            Err(e)
                if matches!(
                    e.kind(),
                    std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                ) =>
            {
                return Err(format!("{who}: deadline exceeded"))
            }
            Err(e) => return Err(format!("{who}: {e}")),
        }
    }
    let text = String::from_utf8_lossy(&raw);
    let (head, payload) = text
        .split_once("\r\n\r\n")
        .ok_or(format!("{who}: malformed response"))?;
    if !head.starts_with("HTTP/1.") || !head.split_whitespace().nth(1).is_some_and(|c| c == "200") {
        return Err(format!("{who}: {}", head.lines().next().unwrap_or("")));
    }
    serde_json::from_str(payload).map_err(|e| format!("{who}: {e}"))
}

/// The content digest of the configured model as Ollama has it installed, so a sidecar can be
/// bound to the exact weights and not only to a re-pullable tag.
pub fn model_digest(deadline: Instant) -> Result<String, String> {
    let v = call("GET", "/api/tags", "", deadline)?;
    let want = model();
    v["models"]
        .as_array()
        .into_iter()
        .flatten()
        .find(|m| m["name"].as_str().is_some_and(|n| same_model(n, &want)))
        .and_then(|m| m["digest"].as_str())
        .map(str::to_string)
        .ok_or_else(|| format!("ollama: model {want} is not installed"))
}

/// Embed `texts` with the local Ollama; unit-normalised vectors in input order.
pub fn embed(texts: &[String], deadline: Instant) -> Result<Vec<Vec<f32>>, String> {
    // keep the model loaded between prompts so the hook does not pay a cold start
    let body =
        serde_json::json!({"model": model(), "input": texts, "keep_alive": "30m"}).to_string();
    let v = call("POST", "/api/embed", &body, deadline)?;
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
pub fn write_sidecar(path: &str, key: &str, digest: &str, vecs: &[Vec<f32>]) -> Result<(), String> {
    let dim = vecs.first().map_or(0, Vec::len);
    let header = serde_json::json!({"model": model(), "digest": digest, "dim": dim,
                                    "n": vecs.len(), "registry": key});
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
    pub digest: String,
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
    if h["registry"].as_str()? != key
        || count != n
        || Some(data.len()) != n.checked_mul(dim).and_then(|x| x.checked_mul(4))
    {
        return None;
    }
    let vecs: Vec<f32> = data
        .chunks_exact(4)
        .map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]]))
        .collect();
    // a corrupt sidecar must not rank anything: every value finite, every row unit length
    let unit = |r: &[f32]| (r.iter().map(|a| a * a).sum::<f32>().sqrt() - 1.0).abs() < 1e-3;
    if dim == 0 || vecs.iter().any(|v| !v.is_finite()) || !vecs.chunks_exact(dim).all(unit) {
        return None;
    }
    Some(Sidecar {
        model: h["model"].as_str()?.to_string(),
        digest: h["digest"].as_str()?.to_string(),
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
/// score = 1/(K + rank_lex) + 1/(K + rank_dense), K = 60. Returns (skill index, fused score),
/// best first.
pub fn rrf(lexical: &[usize], dense: &[usize]) -> Vec<(usize, f64)> {
    const K: f64 = 60.0;
    let mut score: std::collections::HashMap<usize, f64> = std::collections::HashMap::new();
    for list in [lexical, dense] {
        for (r, &i) in list.iter().enumerate() {
            *score.entry(i).or_default() += 1.0 / (K + r as f64 + 1.0);
        }
    }
    let mut out: Vec<usize> = score.keys().copied().collect();
    out.sort_by(|a, b| score[b].total_cmp(&score[a]).then(a.cmp(b)));
    out.into_iter().map(|i| (i, score[&i])).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn rrf_rewards_agreement_and_sidecar_is_bound() {
        let order: Vec<usize> = rrf(&[1, 2, 3], &[2, 4]).iter().map(|h| h.0).collect();
        assert_eq!(order, vec![2, 1, 4, 3]);
        let dir = std::env::temp_dir().join(format!("skill-hub-emb-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("r.emb").to_string_lossy().into_owned();
        write_sidecar(&p, "k1", "sha256:d", &[vec![1.0, 0.0], vec![0.0, 1.0]]).unwrap();
        let s = read_sidecar(&p, "k1", 2).unwrap();
        assert_eq!(s.cosines(&[1.0, 0.0]), vec![1.0, 0.0]);
        assert_eq!(s.digest, "sha256:d");
        write_sidecar(&p, "k1", "sha256:d", &[vec![2.0, 0.0], vec![0.0, 1.0]]).unwrap();
        assert!(
            read_sidecar(&p, "k1", 2).is_none(),
            "a row that is not unit length"
        );
        write_sidecar(&p, "k1", "sha256:d", &[vec![1.0, 0.0], vec![0.0, 1.0]]).unwrap();
        assert!(read_sidecar(&p, "k2", 2).is_none(), "another registry");
        assert!(read_sidecar(&p, "k1", 3).is_none(), "another size");
        let huge = format!("{MAGIC}\n{{\"model\":\"m\",\"digest\":\"d\",\"dim\":{},\"n\":2,\"registry\":\"k1\"}}\n", 1u64 << 62);
        std::fs::write(&p, huge).unwrap();
        assert!(
            read_sidecar(&p, "k1", 2).is_none(),
            "a header whose size overflows"
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn rerank_client_reads_tei_scores_and_rejects_bad_answers() {
        // one process-wide env var: every case runs in this single test
        let serve = |body: &'static str| {
            let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
            let port = l.local_addr().unwrap().port();
            std::thread::spawn(move || {
                if let Ok((mut c, _)) = l.accept() {
                    let mut req = Vec::new();
                    let mut b = [0u8; 4096];
                    // headers, then the JSON body (ends with '}')
                    while !(req.windows(4).any(|w| w == b"\r\n\r\n") && req.ends_with(b"}")) {
                        match c.read(&mut b) {
                            Ok(0) | Err(_) => break,
                            Ok(n) => req.extend_from_slice(&b[..n]),
                        }
                    }
                    let _ = write!(c, "HTTP/1.0 200 OK\r\n\r\n{body}");
                }
            });
            std::env::set_var("SKILL_HUB_RERANK", format!("http://127.0.0.1:{port}"));
        };
        let docs = vec!["a: x".to_string(), "b: y".to_string()];
        let soon = || Instant::now() + Duration::from_secs(5);
        serve(r#"[{"index":1,"score":0.9},{"index":0,"score":0.1}]"#);
        assert_eq!(rerank("q", &docs, soon()).unwrap(), vec![0.1, 0.9]);
        serve(r#"[{"index":0,"score":0.1}]"#);
        assert!(rerank("q", &docs, soon())
            .unwrap_err()
            .contains("not every text"));
        serve(r#"[{"index":0,"score":-5.0},{"index":1,"score":5.0}]"#);
        assert!(
            rerank("q", &docs, soon())
                .unwrap_err()
                .contains("bad score"),
            "raw logits"
        );
        serve(r#"[{"index":0,"score":0.1},{"index":0,"score":0.2}]"#);
        assert!(rerank("q", &docs, soon())
            .unwrap_err()
            .contains("duplicate"));
        serve(r#"[{"index":5,"score":0.1},{"index":0,"score":0.1}]"#);
        assert!(rerank("q", &docs, soon())
            .unwrap_err()
            .contains("out of range"));
        serve(r#"[{"index":0,"score":"high"},{"index":1,"score":0.1}]"#);
        assert!(rerank("q", &docs, soon())
            .unwrap_err()
            .contains("bad score"));
        std::env::set_var("SKILL_HUB_RERANK", "http://example.com:8121");
        assert!(rerank("q", &docs, soon())
            .unwrap_err()
            .contains("non-loopback"));
        std::env::remove_var("SKILL_HUB_RERANK");
        assert!(rerank("q", &docs, soon()).is_err());
    }

    #[test]
    fn prompts_never_leave_the_machine() {
        std::env::set_var("SKILL_HUB_OLLAMA", "http://example.com:11434");
        assert!(
            embed(&["x".into()], Instant::now() + Duration::from_millis(50))
                .unwrap_err()
                .contains("non-loopback")
        );
        std::env::set_var("SKILL_HUB_OLLAMA", "http://127.0.0.1:1");
        assert!(
            embed(&["x".into()], Instant::now() + Duration::from_millis(50)).is_err(),
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
        let e = embed(&["x".into()], Instant::now() + Duration::from_millis(600)).unwrap_err();
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
        assert!(
            embed(&["x".into()], Instant::now() + Duration::from_secs(20))
                .unwrap_err()
                .contains("too large")
        );
        // the digest check: tag aliases match, other tags and malformed answers do not
        // (same test as above: the env var is process-wide, so these must not run in parallel)
        let serve = |body: &'static str| {
            let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
            let port = l.local_addr().unwrap().port();
            std::thread::spawn(move || {
                if let Ok((mut c, _)) = l.accept() {
                    // read the whole request (GET, no body): closing with unread bytes is a reset on Linux
                    let mut req = Vec::new();
                    let mut b = [0u8; 1024];
                    while !req.windows(4).any(|w| w == b"\r\n\r\n") {
                        match c.read(&mut b) {
                            Ok(0) | Err(_) => break,
                            Ok(n) => req.extend_from_slice(&b[..n]),
                        }
                    }
                    let _ = write!(c, "HTTP/1.0 200 OK\r\n\r\n{body}");
                }
            });
            std::env::set_var("SKILL_HUB_OLLAMA", format!("http://127.0.0.1:{port}"));
        };
        let soon = || Instant::now() + Duration::from_secs(5);
        std::env::remove_var("SKILL_HUB_EMBED_MODEL");
        serve(
            r#"{"models":[{"name":"x:1","digest":"no"},{"name":"bge-m3:latest","digest":"d1"}]}"#,
        );
        assert_eq!(model_digest(soon()).unwrap(), "d1");
        serve(r#"{"models":[{"name":"bge-m3:567m","digest":"d2"}]}"#);
        assert!(
            model_digest(soon()).is_err(),
            "another tag is another model"
        );
        serve(r#"{"models":{"name":"bge-m3"}}"#);
        assert!(model_digest(soon()).is_err());
        serve("not json");
        assert!(model_digest(soon()).is_err());
        std::env::remove_var("SKILL_HUB_OLLAMA");
    }
}
