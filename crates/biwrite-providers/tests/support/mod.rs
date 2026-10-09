//! A tiny scripted HTTP/1.1 server for provider tests (no external deps).

#![allow(dead_code, clippy::unwrap_used, clippy::expect_used)]

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

#[derive(Clone, Debug)]
pub struct Recorded {
    pub method: String,
    pub path: String,
    pub headers: Vec<(String, String)>,
    pub body: String,
}

impl Recorded {
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
    }

    pub fn json(&self) -> serde_json::Value {
        serde_json::from_str(&self.body).unwrap()
    }
}

#[derive(Clone, Debug)]
pub struct Canned {
    pub status: u16,
    pub headers: Vec<(String, String)>,
    pub chunks: Vec<String>,
    pub delay: Duration,
}

impl Canned {
    pub fn sse(events: &[&str]) -> Self {
        Self {
            status: 200,
            headers: vec![("Content-Type".into(), "text/event-stream".into())],
            chunks: events.iter().map(|e| format!("{e}\n\n")).collect(),
            delay: Duration::from_millis(5),
        }
    }

    pub fn json(status: u16, body: &str) -> Self {
        Self {
            status,
            headers: vec![("Content-Type".into(), "application/json".into())],
            chunks: vec![body.to_owned()],
            delay: Duration::ZERO,
        }
    }

    pub fn with_header(mut self, k: &str, v: &str) -> Self {
        self.headers.push((k.into(), v.into()));
        self
    }
}

#[derive(Clone)]
pub struct MockServer {
    pub base: String,
    requests: Arc<Mutex<Vec<Recorded>>>,
    script: Arc<Mutex<VecDeque<Canned>>>,
}

impl MockServer {
    pub async fn start() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let server = Self {
            base,
            requests: Arc::default(),
            script: Arc::default(),
        };
        let s = server.clone();
        tokio::spawn(async move {
            loop {
                let Ok((mut socket, _)) = listener.accept().await else {
                    return;
                };
                let s = s.clone();
                tokio::spawn(async move {
                    let Some(req) = read_request(&mut socket).await else {
                        return;
                    };
                    s.requests.lock().unwrap().push(req);
                    let canned = s.script.lock().unwrap().pop_front().unwrap_or_else(|| {
                        Canned::json(500, r#"{"error":{"message":"no script"}}"#)
                    });
                    let mut head = format!("HTTP/1.1 {} X\r\n", canned.status);
                    for (k, v) in &canned.headers {
                        head.push_str(&format!("{k}: {v}\r\n"));
                    }
                    head.push_str("Connection: close\r\n\r\n");
                    if socket.write_all(head.as_bytes()).await.is_err() {
                        return;
                    }
                    for chunk in canned.chunks {
                        tokio::time::sleep(canned.delay).await;
                        if socket.write_all(chunk.as_bytes()).await.is_err() {
                            return;
                        }
                        let _ = socket.flush().await;
                    }
                    let _ = socket.shutdown().await;
                });
            }
        });
        server
    }

    pub fn push(&self, canned: Canned) {
        self.script.lock().unwrap().push_back(canned);
    }

    pub fn requests(&self) -> Vec<Recorded> {
        self.requests.lock().unwrap().clone()
    }
}

async fn read_request(socket: &mut tokio::net::TcpStream) -> Option<Recorded> {
    let mut buf = Vec::new();
    let mut tmp = [0u8; 4096];
    let head_end = loop {
        let n = socket.read(&mut tmp).await.ok()?;
        if n == 0 {
            return None;
        }
        buf.extend_from_slice(&tmp[..n]);
        if let Some(p) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
            break p;
        }
    };
    let head = String::from_utf8_lossy(&buf[..head_end]).to_string();
    let mut lines = head.split("\r\n");
    let mut first = lines.next()?.split(' ');
    let method = first.next()?.to_owned();
    let path = first.next()?.to_owned();
    let headers: Vec<(String, String)> = lines
        .filter_map(|l| {
            l.split_once(':')
                .map(|(k, v)| (k.trim().to_owned(), v.trim().to_owned()))
        })
        .collect();
    let len: usize = headers
        .iter()
        .find(|(k, _)| k.eq_ignore_ascii_case("content-length"))
        .and_then(|(_, v)| v.parse().ok())
        .unwrap_or(0);
    let mut body = buf[head_end + 4..].to_vec();
    while body.len() < len {
        let n = socket.read(&mut tmp).await.ok()?;
        if n == 0 {
            break;
        }
        body.extend_from_slice(&tmp[..n]);
    }
    Some(Recorded {
        method,
        path,
        headers,
        body: String::from_utf8_lossy(&body).to_string(),
    })
}
