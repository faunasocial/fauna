//! The loopback transport — a seam's forwarder wired straight into its
//! `serve`, so the pair is proved natively with no browser (decision (i): per
//! seam, the round trip over the loopback).

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use crate::{PortFault, PortTransport};

#[cfg(not(target_arch = "wasm32"))]
type ServeFuture = Pin<Box<dyn Future<Output = Result<Vec<u8>, PortFault>> + Send>>;
#[cfg(target_arch = "wasm32")]
type ServeFuture = Pin<Box<dyn Future<Output = Result<Vec<u8>, PortFault>>>>;

#[cfg(not(target_arch = "wasm32"))]
type ServeFn = dyn Fn(&'static str, Vec<u8>) -> ServeFuture + Send + Sync;
#[cfg(target_arch = "wasm32")]
type ServeFn = dyn Fn(&'static str, Vec<u8>) -> ServeFuture;

/// A [`PortTransport`] whose `call` runs `serve` in-process — the core
/// chunk's half without the chunk boundary. The bytes still cross encoded, so
/// the codec is exercised exactly as on web.
pub struct Loopback {
    serve: Arc<ServeFn>,
}

impl Loopback {
    /// A loopback into `serve`, which answers one door from its payload.
    #[cfg(not(target_arch = "wasm32"))]
    pub fn new<F, Fut>(serve: F) -> Self
    where
        F: Fn(&'static str, Vec<u8>) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Result<Vec<u8>, PortFault>> + Send + 'static,
    {
        Self {
            serve: Arc::new(move |door, payload| Box::pin(serve(door, payload))),
        }
    }

    /// A loopback into `serve`, which answers one door from its payload.
    #[cfg(target_arch = "wasm32")]
    pub fn new<F, Fut>(serve: F) -> Self
    where
        F: Fn(&'static str, Vec<u8>) -> Fut + 'static,
        Fut: Future<Output = Result<Vec<u8>, PortFault>> + 'static,
    {
        Self {
            serve: Arc::new(move |door, payload| Box::pin(serve(door, payload))),
        }
    }

    /// A transport on which every call faults with `fault` — the refusal
    /// every forwarder method must answer with.
    pub fn faulting(fault: PortFault) -> Self {
        Self::new(move |_, _| {
            let fault = fault.clone();
            async move { Err(fault) }
        })
    }
}

#[cfg_attr(not(target_arch = "wasm32"), async_trait::async_trait)]
#[cfg_attr(target_arch = "wasm32", async_trait::async_trait(?Send))]
impl PortTransport for Loopback {
    async fn call(&self, door: &'static str, payload: Vec<u8>) -> Result<Vec<u8>, PortFault> {
        (self.serve)(door, payload).await
    }
}
