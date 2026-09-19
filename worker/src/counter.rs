//! Counter Durable Object: durable totals + a live in-memory event ring and
//! a WebSocket stream of new events.
//!
//! One global instance. Totals persist to storage on every add; the ring and
//! the socket set are in-memory (the DO stays alive while sockets are open).

use serde::{Deserialize, Serialize};
use serde_json::json;
use worker::*;

const RING_SIZE: usize = 30;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RequestEvent {
    pub id: String,
    pub ts: u64,
    pub method: String,
    pub path: String,
    pub kind: String,
    pub status: u16,
    pub ms: u64,
    pub client: String,
    pub tier: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct Totals {
    pub requests: u64,
    pub searches: u64,
    pub fetches: u64,
    pub errors: u64,
    #[serde(alias = "startedAt")]
    pub started_at: u64,
}

#[durable_object]
pub struct Counter {
    state: State,
    ring: std::cell::RefCell<Vec<RequestEvent>>,
    sockets: std::cell::RefCell<Vec<WebSocket>>,
    totals: std::cell::RefCell<Option<Totals>>,
}

impl DurableObject for Counter {
    fn new(state: State, _env: Env) -> Self {
        Self {
            state,
            ring: std::cell::RefCell::new(Vec::new()),
            sockets: std::cell::RefCell::new(Vec::new()),
            totals: std::cell::RefCell::new(None),
        }
    }

    async fn fetch(&self, mut req: Request) -> Result<Response> {
        let url = req.url()?;
        let path = url.path().to_string();
        match (req.method(), path.as_str()) {
            (Method::Post, "/add") => {
                let event: RequestEvent = serde_json::from_str(&req.text().await?)
                    .map_err(|e| Error::RustError(e.to_string()))?;
                self.add(event).await?;
                Response::from_json(&json!({ "ok": true }))
            }
            (Method::Get, "/state") => {
                let totals = self.load_totals().await?;
                let live = self.ring.borrow().clone();
                Response::from_json(&json!({ "totals": totals, "live": live }))
            }
            (Method::Get, "/stream") => {
                let pair = WebSocketPair::new()?;
                pair.server.accept()?;
                self.sockets.borrow_mut().push(pair.server.clone());
                Response::from_websocket(pair.client)
            }
            _ => Response::from_json(&json!({ "error": "not found" })).map(|r| r.with_status(404)),
        }
    }
}

impl Counter {
    async fn load_totals(&self) -> Result<Totals> {
        if let Some(totals) = self.totals.borrow().clone() {
            return Ok(totals);
        }
        let stored = self
            .state
            .storage()
            .get::<Totals>("totals")
            .await?
            .unwrap_or_default();
        *self.totals.borrow_mut() = Some(stored.clone());
        Ok(stored)
    }

    async fn add(&self, event: RequestEvent) -> Result<()> {
        let mut totals = self.load_totals().await?;
        totals.requests += 1;
        if event.kind == "search" {
            totals.searches += 1;
        }
        if event.kind == "fetch" {
            totals.fetches += 1;
        }
        if event.status >= 400 {
            totals.errors += 1;
        }
        if totals.started_at == 0 {
            totals.started_at = event.ts;
        }
        self.state.storage().put("totals", &totals).await?;
        *self.totals.borrow_mut() = Some(totals);

        {
            let mut ring = self.ring.borrow_mut();
            ring.push(event.clone());
            let overflow = ring.len().saturating_sub(RING_SIZE);
            if overflow > 0 {
                ring.drain(0..overflow);
            }
        }

        let payload = json!({ "type": "event", "event": event }).to_string();
        self.sockets
            .borrow_mut()
            .retain(|socket| socket.send_with_str(&payload).is_ok());
        Ok(())
    }
}
