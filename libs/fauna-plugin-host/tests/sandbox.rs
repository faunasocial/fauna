//! The sandbox's contract, driven by the in-repo hello plugin against a
//! recording embedder: every import reaches the embedder with the lowered
//! arguments, an undeclared host is refused before any dial, fuel exhaustion
//! ends the call and poisons the instance while the host survives, memory
//! growth over the cap is refused, the identity assertion reaches the plugin.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use fauna_plugin_host::fixture::{HELLO_PLUGIN_LOG_LINE, hello_plugin};
use fauna_plugin_host::{
    BoxFut, HolderKey, HostServices, HttpRequest, HttpResponse, IngressRequest, LogLevel,
    OutboundPolicy, PluginEngine, PluginFault, PluginInstance, RpcRefusal,
};

const ACCOUNT: [u8; 32] = [0xA1; 32];
const NOW: u64 = 1_760_000_000_123;

/// One recorded `nest-api.call`: the account, the kind, the payload.
type RecordedCall = (Option<[u8; 32]>, String, Vec<u8>);

/// An embedder that records what the plugin asked for. Inside the ceiling:
/// `fauna.capabilities.fetch`; everything else refuses with the kind's family
/// code, as the nest's chokepoint would.
#[derive(Default)]
struct Recorder {
    bound: Vec<[u8; 32]>,
    calls: Mutex<Vec<RecordedCall>>,
    state: Mutex<BTreeMap<String, Vec<u8>>>,
    fetches: Mutex<Vec<HttpRequest>>,
    logs: Mutex<Vec<(LogLevel, String)>>,
    key: Option<HolderKey>,
}

impl Recorder {
    fn new(bound: Vec<[u8; 32]>) -> Arc<Self> {
        Arc::new(Self {
            bound,
            key: Some(HolderKey::mint()),
            ..Default::default()
        })
    }

    fn state(&self, key: &str) -> Option<Vec<u8>> {
        self.state.lock().unwrap().get(key).cloned()
    }
}

impl HostServices for Recorder {
    fn nest_call(
        &self,
        account: Option<[u8; 32]>,
        kind: String,
        payload: Vec<u8>,
    ) -> BoxFut<'_, Result<Vec<u8>, RpcRefusal>> {
        self.calls
            .lock()
            .unwrap()
            .push((account, kind.clone(), payload));
        Box::pin(async move {
            if kind == "fauna.capabilities.fetch" {
                Ok(b"ok".to_vec())
            } else {
                Err(RpcRefusal {
                    code: "fauna.feed.permission_denied".into(),
                    message: format!("{kind}: not covered by the principal's scopes"),
                })
            }
        })
    }

    fn bindings(&self) -> BoxFut<'_, Vec<[u8; 32]>> {
        Box::pin(async move { self.bound.clone() })
    }

    fn state_get(&self, key: String) -> BoxFut<'_, anyhow::Result<Option<Vec<u8>>>> {
        Box::pin(async move { Ok(self.state(&key)) })
    }

    fn state_put(&self, key: String, value: Vec<u8>) -> BoxFut<'_, anyhow::Result<()>> {
        self.state.lock().unwrap().insert(key, value);
        Box::pin(async { Ok(()) })
    }

    fn state_delete(&self, key: String) -> BoxFut<'_, anyhow::Result<()>> {
        self.state.lock().unwrap().remove(&key);
        Box::pin(async { Ok(()) })
    }

    fn holder_key(&self) -> &HolderKey {
        self.key.as_ref().unwrap()
    }

    fn http_fetch(&self, req: HttpRequest) -> BoxFut<'_, Result<HttpResponse, String>> {
        self.fetches.lock().unwrap().push(req);
        Box::pin(async {
            Ok(HttpResponse {
                status: 204,
                headers: vec![],
                body: vec![],
            })
        })
    }

    fn now_millis(&self) -> u64 {
        NOW
    }

    fn log(&self, level: LogLevel, message: String) {
        self.logs.lock().unwrap().push((level, message));
    }
}

async fn instance(services: Arc<Recorder>, policy: OutboundPolicy) -> PluginInstance {
    let engine = PluginEngine::new().unwrap();
    let compiled = engine.compile(&hello_plugin().unwrap()).unwrap();
    PluginInstance::instantiate(&engine, &compiled, services, policy)
        .await
        .unwrap()
}

#[tokio::test]
async fn start_drives_every_import_through_the_embedder() {
    let rec = Recorder::new(vec![ACCOUNT, [0xB2; 32]]);
    let mut plugin = instance(rec.clone(), OutboundPolicy::none()).await;

    assert_eq!(plugin.start().await.unwrap(), Ok(()));

    // The holder key reached the plugin as its PUBLIC half only.
    assert_eq!(
        rec.state("pub").as_deref(),
        Some(&rec.holder_key().public()[..])
    );

    // Both calls went to the chokepoint for the first bound account, with the
    // lowered kind and payload; the ceiling's answer came back as the
    // result's discriminant.
    let calls = rec.calls.lock().unwrap().clone();
    assert_eq!(
        calls,
        vec![
            (
                Some(ACCOUNT),
                "fauna.capabilities.fetch".to_string(),
                vec![0xA0]
            ),
            (Some(ACCOUNT), "fauna.feed.posts".to_string(), vec![0xA0]),
        ]
    );
    assert_eq!(rec.state("fetch"), Some(vec![0]), "inside the ceiling: ok");
    assert_eq!(rec.state("refused"), Some(vec![1]), "outside: refused");

    // The undeclared host was refused BEFORE any dial.
    assert_eq!(rec.state("http"), Some(vec![1]));
    assert!(rec.fetches.lock().unwrap().is_empty());
    assert!(
        rec.logs
            .lock()
            .unwrap()
            .iter()
            .any(|(l, m)| *l == LogLevel::Warn && m.contains("undeclared.example"))
    );

    assert_eq!(rec.state("now"), Some(NOW.to_le_bytes().to_vec()));
    assert!(
        rec.logs
            .lock()
            .unwrap()
            .contains(&(LogLevel::Info, HELLO_PLUGIN_LOG_LINE.to_string()))
    );
    assert!(!plugin.is_poisoned());
    assert_eq!(plugin.stop().await.unwrap(), ());
}

#[tokio::test]
async fn a_declared_host_reaches_the_embedders_fetcher() {
    let rec = Recorder::new(vec![]);
    let mut plugin = instance(rec.clone(), OutboundPolicy::new(["undeclared.example"])).await;
    assert_eq!(plugin.start().await.unwrap(), Ok(()));
    assert_eq!(rec.state("http"), Some(vec![0]));
    let fetches = rec.fetches.lock().unwrap().clone();
    assert_eq!(fetches.len(), 1);
    assert_eq!(fetches[0].method, "GET");
    assert_eq!(fetches[0].url, "https://undeclared.example/");
    // No binding: the calls were for the install row.
    assert!(
        rec.calls
            .lock()
            .unwrap()
            .iter()
            .all(|(account, _, _)| account.is_none())
    );
}

#[tokio::test]
async fn fuel_exhaustion_ends_the_call_and_the_host_survives() {
    let rec = Recorder::new(vec![]);
    let mut plugin = instance(rec.clone(), OutboundPolicy::none()).await;
    let spin = IngressRequest {
        method: "GET".into(),
        path: "/spin".into(),
        ..Default::default()
    };
    let fault = plugin.handle_ingress(spin.clone()).await.unwrap_err();
    assert!(matches!(fault, PluginFault::OutOfFuel), "{fault}");
    assert!(plugin.is_poisoned());
    assert!(matches!(
        plugin.handle_ingress(spin).await.unwrap_err(),
        PluginFault::Poisoned
    ));

    // The same compiled plugin, a fresh instance, serves again.
    let mut again = instance(rec, OutboundPolicy::none()).await;
    let reply = again
        .handle_ingress(IngressRequest {
            method: "GET".into(),
            path: "/".into(),
            ..Default::default()
        })
        .await
        .unwrap();
    assert_eq!(reply.status, 404);
    assert!(again.fuel_remaining() > 0);
}

#[tokio::test]
async fn memory_growth_over_the_cap_is_refused() {
    let rec = Recorder::new(vec![]);
    let mut plugin = instance(rec, OutboundPolicy::none()).await;
    let reply = plugin
        .handle_ingress(IngressRequest {
            method: "GET".into(),
            path: "/grow".into(),
            ..Default::default()
        })
        .await
        .unwrap();
    assert_eq!(reply.status, 507, "the grow was refused, not trapped");
    assert!(!plugin.is_poisoned());
}

#[tokio::test]
async fn the_identity_assertion_reaches_the_plugin() {
    let rec = Recorder::new(vec![]);
    let mut plugin = instance(rec, OutboundPolicy::none()).await;
    let who = |actor| IngressRequest {
        method: "GET".into(),
        path: "/whoami".into(),
        actor,
        ..Default::default()
    };
    let reply = plugin.handle_ingress(who(Some(ACCOUNT))).await.unwrap();
    assert_eq!(reply.status, 200);
    assert_eq!(reply.body, ACCOUNT.to_vec());
    let reply = plugin.handle_ingress(who(None)).await.unwrap();
    assert_eq!(reply.status, 401);
    assert!(reply.body.is_empty());
}

#[test]
fn a_component_over_the_size_cap_is_refused_before_compiling() {
    let engine = PluginEngine::new().unwrap();
    let too_big = vec![0u8; fauna_plugin_host::PLUGIN_MODULE_MAX_BYTES + 1];
    assert!(engine.compile(&too_big).is_err());
}
