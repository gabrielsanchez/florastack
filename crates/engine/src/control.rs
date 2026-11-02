//! Authenticated newline-delimited JSON on loopback TCP. The network worker queues requests; only the host thread
//! touches the engine. One request per connection, bounded clients, frame size and deadlines, no HTTP or CORS.

use std::net::{IpAddr, Ipv4Addr, SocketAddr, TcpListener};
use std::path::Path;
use std::sync::{Arc, mpsc};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::sync::{Semaphore, oneshot};

use crate::{ErrorInfo, Value};

/// Maximum wire frame (including base64 thumbnails). Larger requests are closed before allocation grows further.
pub const MAX_FRAME: u64 = 24 << 20;
const DEADLINE: Duration = Duration::from_secs(30);

#[derive(Debug, thiserror::Error)]
pub enum ControlError {
    #[error("control channel I/O failed: {0}")]
    Io(#[from] std::io::Error),
    #[error("invalid control JSON: {0}")]
    Json(#[from] serde_json::Error),
    #[error("control address must be a numeric loopback address")]
    Address,
    #[error("control token must be 32 to 256 printable ASCII characters")]
    Token,
    #[error("control request timed out")]
    Timeout,
    #[error("control peer closed the connection or exceeded the frame limit")]
    Frame,
    #[error("control host stopped")]
    Stopped,
}

/// A token is supplied in a file, never a command-line argument or startup log.
pub fn read_token(path: &Path) -> Result<String, ControlError> {
    use std::io::Read;
    let mut text = String::new();
    std::fs::File::open(path)?.take(259).read_to_string(&mut text)?;
    if text.len() > 258 {
        return Err(ControlError::Token);
    }
    let token = text.trim_end_matches(['\r', '\n']);
    validate_token(token)?;
    Ok(token.to_owned())
}

fn validate_token(token: &str) -> Result<(), ControlError> {
    if !(32..=256).contains(&token.len()) || !token.bytes().all(|b| b.is_ascii_graphic()) {
        return Err(ControlError::Token);
    }
    Ok(())
}

/// Numeric addresses avoid DNS rebinding, proxies and accidental remote connections.
pub fn address(text: &str) -> Result<SocketAddr, ControlError> {
    text.parse::<SocketAddr>().ok().filter(|a| a.ip().is_loopback()).ok_or(ControlError::Address)
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Request {
    id: u64,
    token: String,
    method: String,
    #[serde(default)]
    params: Value,
}

/// The token is deliberately omitted from the request delivered to the UI thread.
pub struct Pending {
    pub method: String,
    pub params: Value,
    reply: oneshot::Sender<Result<Value, ErrorInfo>>,
}

impl Pending {
    /// Timed-out requests must not execute later from a stale UI queue.
    pub fn is_closed(&self) -> bool {
        self.reply.is_closed()
    }

    pub fn respond(self, result: Result<Value, ErrorInfo>) {
        let _ = self.reply.send(result);
    }
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Response {
    id: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    result: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<ErrorInfo>,
}

/// Keeps the listener alive. Dropping it shuts down the listener and all outstanding sockets.
pub struct Server {
    pub address: SocketAddr,
    requests: mpsc::Receiver<Pending>,
    stop: Option<oneshot::Sender<()>>,
    worker: Option<std::thread::JoinHandle<()>>,
}

impl Server {
    pub fn start(port: u16, token: String, wake: crate::Waker) -> Result<Self, ControlError> {
        validate_token(&token)?;
        let listener = TcpListener::bind(SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), port))?;
        listener.set_nonblocking(true)?;
        let address = listener.local_addr()?;
        let (tx, requests) = mpsc::sync_channel(16);
        let (stop, stopped) = oneshot::channel();
        let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build()?;
        // A dedicated I/O thread, not a CPU worker: it never runs image processing or touches project state.
        let worker = std::thread::Builder::new().name("control-io".into()).spawn(move || {
            runtime.block_on(async move {
                let Ok(listener) = tokio::net::TcpListener::from_std(listener) else { return };
                let token = Arc::new(token);
                let limit = Arc::new(Semaphore::new(16));
                tokio::pin!(stopped);
                loop {
                    tokio::select! {
                        _ = &mut stopped => break,
                        accepted = listener.accept() => {
                            let Ok((stream, peer)) = accepted else { break };
                            if !peer.ip().is_loopback() { continue; }
                            let Ok(permit) = limit.clone().try_acquire_owned() else { continue };
                            let (tx, token, wake) = (tx.clone(), token.clone(), wake.clone());
                            tokio::spawn(async move {
                                let _permit = permit;
                                let _ = tokio::time::timeout(DEADLINE, serve(stream, &token, &tx, &wake)).await;
                            });
                        }
                    }
                }
            });
        })?;
        Ok(Self { address, requests, stop: Some(stop), worker: Some(worker) })
    }

    pub fn try_recv(&self) -> Option<Pending> {
        self.requests.try_recv().ok()
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        if let Some(stop) = self.stop.take() {
            let _ = stop.send(());
        }
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

async fn line(reader: impl AsyncRead + Unpin) -> Result<Vec<u8>, ControlError> {
    let mut bytes = Vec::new();
    BufReader::new(reader.take(MAX_FRAME + 1)).read_until(b'\n', &mut bytes).await?;
    if bytes.len() as u64 > MAX_FRAME || bytes.last() != Some(&b'\n') {
        return Err(ControlError::Frame);
    }
    Ok(bytes)
}

// Fixed-work comparison for equal-size tokens; malformed lengths reveal no token bytes.
fn authenticated(actual: &str, expected: &str) -> bool {
    actual.len() == expected.len() && actual.bytes().zip(expected.bytes()).fold(0_u8, |diff, (a, b)| diff | (a ^ b)) == 0
}

async fn serve(mut stream: tokio::net::TcpStream, token: &str, tx: &mpsc::SyncSender<Pending>, wake: &crate::Waker) -> Result<(), ControlError> {
    let request: Request = serde_json::from_slice(&line(&mut stream).await?)?;
    let result = if authenticated(&request.token, token) {
        let (reply, response) = oneshot::channel();
        let pending = Pending { method: request.method, params: request.params, reply };
        if tx.try_send(pending).is_err() {
            Err(error("busy", "control request queue is full"))
        } else {
            wake();
            response.await.map_err(|_| ControlError::Stopped)?
        }
    } else {
        Err(error("unauthorized", "control authentication failed"))
    };
    let response = match result {
        Ok(value) => Response { id: request.id, result: Some(value), error: None },
        Err(error) => Response { id: request.id, result: None, error: Some(error) },
    };
    let mut bytes = serde_json::to_vec(&response)?;
    if bytes.len() as u64 >= MAX_FRAME {
        return Err(ControlError::Frame);
    }
    bytes.push(b'\n');
    stream.write_all(&bytes).await?;
    Ok(())
}

pub fn error(code: &str, message: &str) -> ErrorInfo {
    ErrorInfo { code: code.into(), command: None, message: message.into() }
}

/// A client stores no socket; simultaneous calls (including cancellation) use independent connections.
#[derive(Clone)]
pub struct Client {
    address: SocketAddr,
    token: String,
}

impl Client {
    pub fn new(address: SocketAddr, token: String) -> Result<Self, ControlError> {
        if !address.ip().is_loopback() {
            return Err(ControlError::Address);
        }
        validate_token(&token)?;
        Ok(Self { address, token })
    }

    pub async fn request(&self, method: &str, params: Value) -> Result<Result<Value, ErrorInfo>, ControlError> {
        tokio::time::timeout(DEADLINE, async {
            let mut stream = tokio::net::TcpStream::connect(self.address).await?;
            let mut bytes = serde_json::to_vec(&Request { id: 1, token: self.token.clone(), method: method.into(), params })?;
            if bytes.len() as u64 >= MAX_FRAME {
                return Err(ControlError::Frame);
            }
            bytes.push(b'\n');
            stream.write_all(&bytes).await?;
            let response: Response = serde_json::from_slice(&line(&mut stream).await?)?;
            if response.id != 1 {
                return Err(ControlError::Frame);
            }
            match (response.result, response.error) {
                (Some(value), None) => Ok(Ok(value)),
                (None, Some(error)) => Ok(Err(error)),
                _ => Err(ControlError::Frame),
            }
        })
        .await
        .map_err(|_| ControlError::Timeout)?
    }
}
