//! RR-1 receiver: records real message frames and can hold their acknowledgements.
use iroh::{endpoint::Connection, Endpoint};
use serde_json::{json, Value};
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc, Mutex,
};
use std::time::{Duration, Instant};
use tokio::{
    runtime::{Handle, Runtime},
    task::{JoinHandle, JoinSet},
};

#[derive(Default)]
struct Traffic {
    messages: Vec<(Instant, Value)>,
    errors: Vec<String>,
}

pub struct WireObserver {
    traffic: Arc<Mutex<Traffic>>,
    release: Arc<AtomicBool>,
    tasks: Vec<JoinHandle<()>>,
    runtime: Handle,
}

impl WireObserver {
    pub fn start(
        runtime: &Runtime,
        endpoint: Endpoint,
        connection: Connection,
        hello: Value,
    ) -> Self {
        let traffic = Arc::new(Mutex::new(Traffic::default()));
        let release = Arc::new(AtomicBool::new(false));
        let initial = runtime.spawn(serve(
            connection,
            traffic.clone(),
            release.clone(),
            hello.clone(),
        ));
        let log = traffic.clone();
        let gate = release.clone();
        let reconnects = runtime.spawn(async move {
            let mut connections = JoinSet::new();
            loop {
                tokio::select! {
                    incoming = endpoint.accept() => {
                        let Some(incoming) = incoming else { break };
                        let log = log.clone();
                        let gate = gate.clone();
                        let hello = hello.clone();
                        connections.spawn(async move {
                            if let Ok(connection) = incoming.await {
                                serve(connection, log, gate, hello).await;
                            }
                        });
                    }
                    result = connections.join_next(), if !connections.is_empty() => {
                        if let Some(Err(error)) = result {
                            log.lock().unwrap().errors.push(error.to_string());
                        }
                    }
                }
            }
        });
        Self {
            traffic,
            release,
            tasks: vec![initial, reconnects],
            runtime: runtime.handle().clone(),
        }
    }

    pub fn messages(&self) -> Vec<(Instant, Value)> {
        assert!(
            !self.tasks[1].is_finished(),
            "wire reconnect observer stopped"
        );
        let traffic = self.traffic.lock().unwrap();
        assert!(
            traffic.errors.is_empty(),
            "wire observer failed: {:?}",
            traffic.errors
        );
        traffic.messages.clone()
    }

    pub fn release_acks(&self) {
        self.release.store(true, Ordering::SeqCst);
    }
}

async fn serve(
    connection: Connection,
    traffic: Arc<Mutex<Traffic>>,
    release: Arc<AtomicBool>,
    hello: Value,
) {
    let mut streams = JoinSet::new();
    loop {
        tokio::select! {
            incoming = connection.accept_bi() => {
                let Ok((mut send, mut recv)) = incoming else { break };
                let traffic = traffic.clone();
                let release = release.clone();
                let hello = hello.clone();
                streams.spawn(async move {
                    let frame = tokio::time::timeout(Duration::from_secs(3), async {
                        let mut prefix = [0; 4];
                        recv.read_exact(&mut prefix).await.ok()?;
                        let length = u32::from_be_bytes(prefix) as usize;
                        assert!(length <= 8192, "bounded wire frame");
                        let mut bytes = vec![0; length];
                        recv.read_exact(&mut bytes).await.ok()?;
                        Some(serde_json::from_slice::<Value>(&bytes).expect("wire JSON"))
                    }).await;
                    let Ok(Some(frame)) = frame else { return };
                    let reply = match frame["type"].as_str() {
                        Some("msg") => {
                            traffic.lock().unwrap().messages.push((Instant::now(), frame.clone()));
                            while !release.load(Ordering::SeqCst) {
                                tokio::time::sleep(Duration::from_millis(5)).await;
                            }
                            json!({"type":"ack","status":"accepted"})
                        }
                        Some("ping") => json!({"type":"pong","t":frame["t"]}),
                        Some("hello") => hello,
                        _ => json!({"type":"ack","status":"accepted"}),
                    };
                    let bytes = serde_json::to_vec(&reply).unwrap();
                    // Pause/remove may close this already-received request before its ack.
                    if send.write_all(&(bytes.len() as u32).to_be_bytes()).await.is_ok()
                        && send.write_all(&bytes).await.is_ok() {
                        let _ = send.finish();
                    }
                });
            }
            result = streams.join_next(), if !streams.is_empty() => {
                if let Some(Err(error)) = result {
                    traffic.lock().unwrap().errors.push(error.to_string());
                }
            }
        }
    }
}

impl Drop for WireObserver {
    fn drop(&mut self) {
        for task in self.tasks.drain(..) {
            task.abort();
            let _ = self.runtime.block_on(task);
        }
    }
}
