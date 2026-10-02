//! A malicious paired peer on loopback, using only the disposable instance's identity.
//! Pairing/shares come from real CLI/HTTP flows; the probe never seeds federation rows.
use super::fed::{db, eventually, text};
use super::{Bus, Server};
use iroh::{endpoint::presets, Endpoint, EndpointAddr, RelayMode, SecretKey, TransportAddr};
use serde_json::{json, Value};
use std::net::SocketAddr;
use std::time::Duration;

pub struct Probe {
    runtime: tokio::runtime::Runtime,
    endpoint: Endpoint,
    connection: iroh::endpoint::Connection,
}

impl Probe {
    pub fn replace(bus: &Bus, server: &mut Server, peer_id: &str) -> Self {
        eventually(bus, "observed direct address for raw probe", || {
            db(bus)
                .query_row(
                    "SELECT count(*) FROM peer_addrs WHERE peer_id=?1",
                    [peer_id],
                    |r| r.get::<_, i64>(0),
                )
                .unwrap()
                == 1
        });
        let node = text(bus, "SELECT node_id FROM peers WHERE peer_id=?1", peer_id)
            .parse()
            .unwrap();
        let addrs: Vec<String> = serde_json::from_str(&text(
            bus,
            "SELECT addrs_json FROM peer_addrs WHERE peer_id=?1",
            peer_id,
        ))
        .unwrap();
        let addrs: Vec<SocketAddr> = addrs.iter().map(|a| a.parse().unwrap()).collect();
        assert!(!addrs.is_empty());
        assert!(
            addrs.iter().all(|a| a.ip().is_loopback()),
            "probe must stay on loopback"
        );
        let target = EndpointAddr::from_parts(node, addrs.into_iter().map(TransportAddr::Ip));
        let key: [u8; 32] = std::fs::read(bus.root.join("data/axon/fed/identity.key"))
            .unwrap()
            .try_into()
            .unwrap();
        server.process.0.kill().unwrap();
        server.process.0.wait().unwrap();
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .unwrap();
        let (endpoint, connection) = runtime.block_on(async {
            tokio::time::timeout(bus.remaining().min(Duration::from_secs(10)), async {
                let endpoint = Endpoint::builder(presets::Minimal)
                    .alpns(vec![b"axon/fed/1".to_vec()])
                    .secret_key(SecretKey::from_bytes(&key))
                    .relay_mode(RelayMode::Disabled)
                    .clear_ip_transports()
                    .bind_addr("127.0.0.1:0".parse::<SocketAddr>().unwrap())
                    .unwrap()
                    .bind()
                    .await
                    .unwrap();
                let connection = endpoint.connect(target, b"axon/fed/1").await.unwrap();
                (endpoint, connection)
            })
            .await
            .expect("loopback probe connection deadline")
        });
        Self {
            runtime,
            endpoint,
            connection,
        }
    }

    pub fn request(&self, frame: &Value) -> Value {
        self.runtime.block_on(async {
            tokio::time::timeout(Duration::from_secs(3), async {
                let (mut send, mut recv) = self.connection.open_bi().await.unwrap();
                let body = serde_json::to_vec(frame).unwrap();
                assert!(body.len() <= 8192);
                send.write_all(&(body.len() as u32).to_be_bytes())
                    .await
                    .unwrap();
                send.write_all(&body).await.unwrap();
                send.finish().unwrap();
                let mut prefix = [0; 4];
                recv.read_exact(&mut prefix).await.unwrap();
                let length = u32::from_be_bytes(prefix) as usize;
                assert!(length <= 8192);
                let mut reply = vec![0; length];
                recv.read_exact(&mut reply).await.unwrap();
                serde_json::from_slice(&reply).unwrap()
            })
            .await
            .expect("raw frame must receive a bounded response")
        })
    }
}

impl Drop for Probe {
    fn drop(&mut self) {
        self.runtime.block_on(self.endpoint.close());
    }
}

impl Probe {
    pub fn observe(&self, generation: i64) -> super::wire_observer::WireObserver {
        // Capture the real control response rather than inventing an undocumented hello DTO.
        let hello = self.request(&json!({"type":"hello","v":1,"generation":generation}));
        super::wire_observer::WireObserver::start(
            &self.runtime,
            self.endpoint.clone(),
            self.connection.clone(),
            hello,
        )
    }

    pub fn burst(&self, frame: &Value, count: usize) -> Vec<Value> {
        assert!(count <= 128);
        self.runtime.block_on(async {
            tokio::time::timeout(Duration::from_secs(3), async {
                let mut pending = tokio::task::JoinSet::new();
                for _ in 0..count {
                    let connection = self.connection.clone();
                    let body = serde_json::to_vec(frame).unwrap();
                    assert!(body.len() <= 8192);
                    pending.spawn(async move {
                        let (mut send, mut recv) = connection.open_bi().await.unwrap();
                        send.write_all(&(body.len() as u32).to_be_bytes())
                            .await
                            .unwrap();
                        send.write_all(&body).await.unwrap();
                        send.finish().unwrap();
                        let mut prefix = [0; 4];
                        recv.read_exact(&mut prefix).await.unwrap();
                        let length = u32::from_be_bytes(prefix) as usize;
                        assert!(length <= 8192);
                        let mut reply = vec![0; length];
                        recv.read_exact(&mut reply).await.unwrap();
                        serde_json::from_slice::<Value>(&reply).unwrap()
                    });
                }
                let mut replies = Vec::new();
                while let Some(reply) = pending.join_next().await {
                    replies.push(reply.expect("wire burst task"));
                }
                replies
            })
            .await
            .expect("all burst responses within three seconds")
        })
    }
}

pub fn frame(bus: &Bus, peer_id: &str, mut envelope: Value) -> Value {
    let generation: i64 = db(bus)
        .query_row(
            "SELECT generation FROM peers WHERE peer_id=?1",
            [peer_id],
            |r| r.get(0),
        )
        .unwrap();
    envelope["type"] = json!("msg");
    envelope["v"] = json!(1);
    envelope["generation"] = json!(generation);
    envelope
}
