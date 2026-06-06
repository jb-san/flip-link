//! Shared daemon state: device-seq allocation, seq->client routing, cached CAPS.

use flip_core::transport::OwnedFrame;
use std::collections::HashMap;
use std::sync::mpsc::Sender;
use std::sync::Mutex;

#[derive(Default)]
struct Inner {
    next_seq: u16,
    routes: HashMap<u16, Sender<OwnedFrame>>,
    caps_payload: Vec<u8>, // cached CAPS body (empty until first connect)
}

// No `#[derive(Default)]`: a `Router::default()` would start `next_seq` at 0,
// which aliases the HELLO seq used during the CAPS handshake. Always use `new()`.
pub struct Router {
    inner: Mutex<Inner>,
}

impl Router {
    pub fn new() -> Self {
        Router {
            inner: Mutex::new(Inner {
                next_seq: 1,
                ..Default::default()
            }),
        }
    }

    /// Allocate a unique device-side seq and register where its reply should go.
    pub fn register(&self, reply_to: Sender<OwnedFrame>) -> u16 {
        let mut g = self.inner.lock().unwrap();
        let seq = g.next_seq;
        g.next_seq = g.next_seq.wrapping_add(1).max(1);
        g.routes.insert(seq, reply_to);
        seq
    }

    /// Drop a pending route (e.g. on client-side timeout) so a late device reply
    /// can't be delivered into a stale channel and contaminate a later request.
    pub fn unregister(&self, seq: u16) {
        self.inner.lock().unwrap().routes.remove(&seq);
    }

    /// Deliver an inbound device frame to the client that owns its seq (if any).
    pub fn deliver(&self, frame: OwnedFrame) {
        let sender = {
            let mut g = self.inner.lock().unwrap();
            g.routes.remove(&frame.seq)
        };
        if let Some(tx) = sender {
            let _ = tx.send(frame);
        }
    }

    pub fn set_caps(&self, payload: Vec<u8>) {
        self.inner.lock().unwrap().caps_payload = payload;
    }

    pub fn caps(&self) -> Vec<u8> {
        self.inner.lock().unwrap().caps_payload.clone()
    }
}
