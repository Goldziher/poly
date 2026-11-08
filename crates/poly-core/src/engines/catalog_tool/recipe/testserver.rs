//! A scripted HTTP server for provisioning tests: per-path queues of canned
//! replies (the last repeats), a record of every request, and optional delays,
//! so a test can say "503, then 200" and count what was asked.

use std::collections::HashMap;
use std::io::{Read, Write};
use std::net::TcpListener;
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// One canned answer.
#[derive(Clone)]
pub(super) struct Canned {
    pub(super) status: u16,
    pub(super) headers: Vec<(String, String)>,
    pub(super) body: Vec<u8>,
    /// Wait before answering, to provoke a client timeout.
    pub(super) delay: Duration,
    /// Promise more bytes than are sent, then hang up.
    pub(super) truncate: bool,
}

impl Canned {
    pub(super) fn new(status: u16, body: impl Into<Vec<u8>>) -> Self {
        Canned {
            status,
            headers: Vec::new(),
            body: body.into(),
            delay: Duration::ZERO,
            truncate: false,
        }
    }

    pub(super) fn ok(body: impl Into<Vec<u8>>) -> Self {
        Canned::new(200, body)
    }

    pub(super) fn header(mut self, name: &str, value: &str) -> Self {
        self.headers.push((name.to_owned(), value.to_owned()));
        self
    }

    pub(super) fn delayed(mut self, delay: Duration) -> Self {
        self.delay = delay;
        self
    }

    pub(super) fn truncated(mut self) -> Self {
        self.truncate = true;
        self
    }
}

/// What the server saw.
#[derive(Clone, Debug)]
pub(super) struct Seen {
    pub(super) path: String,
    /// Header names lower-cased.
    pub(super) headers: HashMap<String, String>,
}

pub(super) struct Server {
    pub(super) base: String,
    seen: Arc<Mutex<Vec<Seen>>>,
}

impl Server {
    /// Start serving `routes` (path with query, replies in order) on a free port.
    pub(super) fn start(routes: Vec<(&str, Vec<Canned>)>) -> Server {
        Server::start_with(|_| {
            routes
                .into_iter()
                .map(|(path, replies)| (path.to_owned(), replies))
                .collect()
        })
    }

    /// [`Server::start`] for routes whose bodies name the server itself: `build`
    /// is given the base URL (`http://127.0.0.1:<port>`).
    pub(super) fn start_with(build: impl FnOnce(&str) -> Vec<(String, Vec<Canned>)>) -> Server {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
        let base = format!("http://{}", listener.local_addr().expect("addr"));
        let table: Arc<Mutex<HashMap<String, Vec<Canned>>>> = Arc::new(Mutex::new(build(&base).into_iter().collect()));
        let seen: Arc<Mutex<Vec<Seen>>> = Arc::default();
        let log = Arc::clone(&seen);
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(stream) = stream else { return };
                let (table, log) = (Arc::clone(&table), Arc::clone(&log));
                std::thread::spawn(move || answer(stream, &table, &log));
            }
        });
        Server { base, seen }
    }

    pub(super) fn url(&self, path: &str) -> String {
        format!("{}{path}", self.base)
    }

    /// How many requests `path` has received.
    pub(super) fn hits(&self, path: &str) -> usize {
        self.requests(path).len()
    }

    /// Every request received, whatever its path.
    pub(super) fn total(&self) -> usize {
        self.seen.lock().expect("log").len()
    }

    pub(super) fn requests(&self, path: &str) -> Vec<Seen> {
        self.seen
            .lock()
            .expect("log")
            .iter()
            .filter(|seen| seen.path == path)
            .cloned()
            .collect()
    }
}

fn answer(mut stream: std::net::TcpStream, table: &Mutex<HashMap<String, Vec<Canned>>>, log: &Mutex<Vec<Seen>>) {
    let mut request = Vec::new();
    let mut chunk = [0u8; 2048];
    while !request.windows(4).any(|w| w == b"\r\n\r\n") {
        match stream.read(&mut chunk) {
            Ok(0) | Err(_) => return,
            Ok(n) => request.extend_from_slice(&chunk[..n]),
        }
    }
    let text = String::from_utf8_lossy(&request).into_owned();
    let mut lines = text.lines();
    let path = lines
        .next()
        .and_then(|l| l.split_whitespace().nth(1))
        .unwrap_or("/")
        .to_owned();
    let headers = lines
        .take_while(|line| !line.is_empty())
        .filter_map(|line| line.split_once(':'))
        .map(|(name, value)| (name.trim().to_ascii_lowercase(), value.trim().to_owned()))
        .collect();
    log.lock().expect("log").push(Seen {
        path: path.clone(),
        headers,
    });
    let reply = {
        let mut table = table.lock().expect("table");
        match table.get_mut(&path) {
            Some(replies) if replies.len() > 1 => replies.remove(0),
            Some(replies) => replies[0].clone(),
            None => Canned::new(404, "not found"),
        }
    };
    std::thread::sleep(reply.delay);
    let promised = if reply.truncate {
        reply.body.len() + 1024
    } else {
        reply.body.len()
    };
    let mut head = format!(
        "HTTP/1.1 {} X\r\nContent-Length: {promised}\r\nConnection: close\r\n",
        reply.status
    );
    for (name, value) in &reply.headers {
        head.push_str(&format!("{name}: {value}\r\n"));
    }
    head.push_str("\r\n");
    let _ = stream.write_all(head.as_bytes());
    let _ = stream.write_all(&reply.body);
}
