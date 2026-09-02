//! Minimal std-only HTTP/1.1 server for the meter page. One thread per
//! connection; routes are fixed and tiny.

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::{Arc, Mutex};
use std::thread;

use crate::meter::{now_secs, Meter};

const PAGE: &str = include_str!("page.html");

pub fn serve(addr: &str, meter: Arc<Mutex<Meter>>) -> std::io::Result<()> {
    let listener = TcpListener::bind(addr)?;
    for stream in listener.incoming() {
        let stream = match stream {
            Ok(s) => s,
            Err(_) => continue,
        };
        let m = Arc::clone(&meter);
        thread::spawn(move || {
            let _ = handle(stream, m);
        });
    }
    Ok(())
}

fn handle(mut stream: TcpStream, meter: Arc<Mutex<Meter>>) -> std::io::Result<()> {
    let mut buf = [0u8; 2048];
    let n = stream.read(&mut buf)?;
    if n == 0 {
        return Ok(());
    }
    let req = String::from_utf8_lossy(&buf[..n]);
    let path = req
        .split_whitespace()
        .nth(1)
        .unwrap_or("/")
        .split('?')
        .next()
        .unwrap_or("/");

    if path == "/favicon.ico" {
        return send(&mut stream, "200 OK", "image/x-icon", FAVICON);
    }

    let (status, ctype, body) = match path {
        "/" | "/damage" | "/healing" | "/taken" => {
            ("200 OK", "text/html; charset=utf-8", PAGE.to_string())
        }
        "/data" => {
            let mut m = meter.lock().unwrap();
            let now = now_secs();
            m.tick(now);
            m.expire_notice(now, crate::sources::memory::client_pid);
            ("200 OK", "application/json", m.snapshot_json())
        }
        "/clear" => {
            let mut m = meter.lock().unwrap();
            m.clear();
            ("200 OK", "application/json", m.snapshot_json())
        }
        "/debug" => {
            let m = meter.lock().unwrap();
            ("200 OK", "text/plain; charset=utf-8", m.debug_text())
        }
        _ => ("404 Not Found", "text/plain", "not found".to_string()),
    };

    send(&mut stream, status, ctype, body.as_bytes())
}

/// Bundled favicon for the meter page (also the app's icon source).
const FAVICON: &[u8] = include_bytes!("../assets/favicon.ico");

fn send(stream: &mut TcpStream, status: &str, ctype: &str, body: &[u8]) -> std::io::Result<()> {
    let resp = format!(
        "HTTP/1.1 {}\r\nContent-Type: {}\r\nContent-Length: {}\r\n\
         Cache-Control: no-store\r\nConnection: close\r\n\r\n",
        status,
        ctype,
        body.len()
    );
    stream.write_all(resp.as_bytes())?;
    stream.write_all(body)?;
    stream.flush()?;
    Ok(())
}
