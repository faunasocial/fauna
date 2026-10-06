//! The JS binding — the SPA's `SharedAccountPort` as a [`PortTransport`], and
//! the tagged object a [`PortFault`] crosses JS as. The shape mirrors
//! `fauna_rpc_wasm::shared_port`: a TypeScript interface emitted into every
//! chunk that links this crate, a duck-typed extern view of it, a
//! required-method check at construction that refuses by name, and a
//! rejection both ends decode — this crate is compiled into the core chunk
//! (which rejects with [`PortFault::to_js`]) and into every consumer chunk
//! (which reads it back with [`PortFault::from_js`]), so both sides of every
//! crossing are one definition.

use wasm_bindgen::prelude::*;
use wasm_bindgen_futures::JsFuture;

use crate::{PortFault, PortTransport};

/// The TypeScript declaration of the port, emitted into every consuming
/// chunk's `.d.ts` so the SPA implements ONE typed interface and each chunk
/// machine's `setAccountPort` is typed `port: SharedAccountPort` (never
/// `any`). [`JsAccountPort`] below is its mirror; the two are kept in step by
/// being in one file.
#[wasm_bindgen(typescript_custom_section)]
const SHARED_ACCOUNT_PORT_TS: &'static str = r#"
/**
 * The tab's account runtime, lent to a wasm chunk other than the core chunk
 * as pure data in / pure data out. Minted per account by `$lib/account-runtime`
 * (`sharedAccountPort`) over the core chunk's `accountPortCall`; consumed by
 * every chunk machine that needs the store.
 */
export interface SharedAccountPort {
  /**
   * Run one door of a consumer seam. `payload` and the resolved reply are the
   * call's arguments / return as canonical DAG-CBOR bytes. Rejects with the
   * tagged object `PortFault::to_js` writes (`{ arm, message }`).
   */
  call(door: string, payload: Uint8Array): Promise<Uint8Array>;
}
"#;

#[wasm_bindgen]
extern "C" {
    /// The Rust view of a `SharedAccountPort` (see the TypeScript section
    /// above): duck-typed, so the object the SPA hands over needs only `call`.
    #[wasm_bindgen(typescript_type = "SharedAccountPort")]
    pub type JsAccountPort;

    #[wasm_bindgen(method, catch)]
    fn call(this: &JsAccountPort, door: &str, payload: Vec<u8>)
    -> Result<js_sys::Promise, JsValue>;
}

const ARM_KEY: &str = "arm";
const MESSAGE_KEY: &str = "message";

impl PortFault {
    fn arm_name(&self) -> &'static str {
        match self {
            Self::NoRuntime(_) => "no_runtime",
            Self::UnknownDoor(_) => "unknown_door",
            Self::Codec(_) => "codec",
            Self::Glue(_) => "glue",
        }
    }

    /// The tagged object this fault crosses JS as — what the core chunk's
    /// `accountPortCall` rejects with.
    pub fn to_js(&self) -> JsValue {
        let obj = js_sys::Object::new();
        let message = match self {
            Self::NoRuntime(m) | Self::UnknownDoor(m) | Self::Codec(m) | Self::Glue(m) => m,
        };
        // `Reflect::set` on a fresh plain object cannot fail.
        let _ = js_sys::Reflect::set(&obj, &ARM_KEY.into(), &self.arm_name().into());
        let _ = js_sys::Reflect::set(&obj, &MESSAGE_KEY.into(), &message.as_str().into());
        obj.into()
    }

    /// Decode a port's rejection — the inverse of [`Self::to_js`]. A value
    /// this crate did not write (the SPA's glue threw, or its promise
    /// rejected with an `Error`) is a [`Self::Glue`] fault carrying the
    /// value's string form, never mistaken for one of ours.
    pub fn from_js(value: JsValue) -> Self {
        let field = |key: &str| {
            js_sys::Reflect::get(&value, &JsValue::from_str(key))
                .ok()
                .and_then(|v| v.as_string())
        };
        let message = || field(MESSAGE_KEY).unwrap_or_default();
        match field(ARM_KEY).as_deref() {
            Some("no_runtime") => Self::NoRuntime(message()),
            Some("unknown_door") => Self::UnknownDoor(message()),
            Some("codec") => Self::Codec(message()),
            Some("glue") => Self::Glue(message()),
            _ => Self::Glue(format!(
                "shared account port failed: {}",
                js_sys::JSON::stringify(&value)
                    .ok()
                    .and_then(|s| s.as_string())
                    .filter(|s| !s.is_empty() && s != "{}")
                    .unwrap_or_else(|| format!("{value:?}"))
            )),
        }
    }
}

/// A consumer chunk's [`PortTransport`]: the SPA's `SharedAccountPort`.
pub struct JsAccountTransport {
    port: JsAccountPort,
}

impl JsAccountTransport {
    /// Wrap the SPA's port object, refusing one that is not an object or
    /// lacks `call` — by name, so the mistake surfaces where the port was
    /// wired rather than at a page's first gesture.
    pub fn new(port: JsValue) -> Result<Self, PortFault> {
        if !port.is_object() {
            return Err(PortFault::Glue(
                "shared account port is not an object".into(),
            ));
        }
        let has_call = js_sys::Reflect::get(&port, &JsValue::from_str("call"))
            .map(|m| m.is_function())
            .unwrap_or(false);
        if !has_call {
            return Err(PortFault::Glue(
                "shared account port lacks the `call` method".into(),
            ));
        }
        Ok(Self {
            port: port.unchecked_into(),
        })
    }
}

#[async_trait::async_trait(?Send)]
impl PortTransport for JsAccountTransport {
    async fn call(&self, door: &'static str, payload: Vec<u8>) -> Result<Vec<u8>, PortFault> {
        let promise = self.port.call(door, payload).map_err(PortFault::from_js)?;
        let reply = JsFuture::from(promise).await.map_err(PortFault::from_js)?;
        if !reply.is_instance_of::<js_sys::Uint8Array>() {
            return Err(PortFault::Codec(format!(
                "shared account port resolved `{door}` with a non-bytes reply"
            )));
        }
        Ok(js_sys::Uint8Array::new(&reply).to_vec())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use wasm_bindgen_test::{wasm_bindgen_test, wasm_bindgen_test_configure};

    wasm_bindgen_test_configure!(run_in_browser);

    #[wasm_bindgen(inline_js = r#"
        export function echo_port(reject_with) {
            return {
                call(door, payload) {
                    if (reject_with !== undefined && reject_with !== null) {
                        return Promise.reject(reject_with);
                    }
                    return Promise.resolve(new Uint8Array(payload));
                },
            };
        }
    "#)]
    extern "C" {
        fn echo_port(reject_with: JsValue) -> JsValue;
    }

    #[wasm_bindgen_test]
    async fn bytes_cross_and_every_fault_arm_survives() {
        let t = JsAccountTransport::new(echo_port(JsValue::NULL)).unwrap();
        assert_eq!(t.call("d", vec![1, 2, 3]).await.unwrap(), vec![1, 2, 3]);
        for fault in [
            PortFault::NoRuntime("none".into()),
            PortFault::UnknownDoor("x.y".into()),
            PortFault::Codec("bad".into()),
            PortFault::Glue("boom".into()),
        ] {
            let t = JsAccountTransport::new(echo_port(fault.to_js())).unwrap();
            assert_eq!(t.call("d", vec![]).await, Err(fault));
        }
    }

    #[wasm_bindgen_test]
    async fn a_foreign_rejection_is_a_glue_fault() {
        let t = JsAccountTransport::new(echo_port(JsValue::from_str("TypeError: boom"))).unwrap();
        let err = t.call("d", vec![]).await.unwrap_err();
        assert!(
            matches!(&err, PortFault::Glue(m) if m.contains("boom")),
            "{err:?}"
        );
    }

    #[wasm_bindgen_test]
    fn a_port_without_call_is_refused_by_name() {
        let err = JsAccountTransport::new(js_sys::Object::new().into())
            .err()
            .expect("refused");
        assert!(err.to_string().contains("`call`"), "{err}");
    }
}
