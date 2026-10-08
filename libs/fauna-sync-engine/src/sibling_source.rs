//! The engine's seam for **sibling-first chunk fetches** — the pull half of the
//! same-account peer data plane (`docs/goal/behavior/p2p.md` § Goal: two of
//! your own devices move a file straight between themselves; a transfer never
//! fails for want of a direct path).
//!
//! A download asks a [`SiblingChunkSource`] for the chunk bodies it needs
//! before the nest, and fetches from the nest only what no sibling served
//! ([`crate::engine::SyncEngine`]'s blob fetcher). The source is infallible by
//! contract: a sibling that is off, unreachable, refusing, misbehaving or
//! reachable only over a relay while the nest answers simply serves nothing,
//! and the nest path carries the remainder — the bodies a sibling did serve are
//! kept, never fetched twice.
//!
//! Not feature-gated: the engine holds the seam in every build. The one
//! implementation, the peer leg's admitted-sibling registry
//! (`crate::sibling_chunks::SiblingChannels`), lives behind `account-runtime`
//! with the rest of the leg; an engine built without one fetches from the nest
//! exactly as before.

use std::collections::HashMap;

use fauna_core::data::ContentHash;

/// Where a download looks for chunk bodies before the nest.
#[async_trait::async_trait]
pub trait SiblingChunkSource: Send + Sync {
    /// The bodies a sibling device served of `store_keys` in `folder` (its
    /// `FolderRef` wire string), each already checked against its store key.
    /// Any key absent from the map is the nest's to serve. `relative_path` is
    /// log context only — never a lookup key.
    async fn fetch(
        &self,
        folder: &str,
        store_keys: &[ContentHash],
        relative_path: &str,
    ) -> HashMap<ContentHash, Vec<u8>>;
}

/// Fetch `store_keys` sibling first, `nest` for the rest: the bodies `source`
/// served are kept and only the remaining distinct keys are handed to `nest`
/// — a cut part-way costs the remainder, never a re-fetch. The result is
/// parallel to `store_keys` (the `BlobFetcher` contract), a repeated key
/// repeated.
pub async fn sibling_first<F, Fut>(
    source: &dyn SiblingChunkSource,
    folder: &str,
    store_keys: &[ContentHash],
    relative_path: &str,
    nest: F,
) -> anyhow::Result<Vec<Vec<u8>>>
where
    F: FnOnce(Vec<ContentHash>) -> Fut,
    Fut: std::future::Future<Output = anyhow::Result<Vec<Vec<u8>>>>,
{
    let mut have = source.fetch(folder, store_keys, relative_path).await;
    let mut rest: Vec<ContentHash> = Vec::new();
    for key in store_keys {
        if !have.contains_key(key) && !rest.contains(key) {
            rest.push(*key);
        }
    }
    if !rest.is_empty() {
        let from_nest = nest(rest.clone()).await?;
        anyhow::ensure!(
            from_nest.len() == rest.len(),
            "the nest path returned {} bodies for {} chunks",
            from_nest.len(),
            rest.len()
        );
        have.extend(rest.into_iter().zip(from_nest));
    }
    Ok(store_keys.iter().map(|key| have[key].clone()).collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    struct Holds(HashMap<ContentHash, Vec<u8>>);

    #[async_trait::async_trait]
    impl SiblingChunkSource for Holds {
        async fn fetch(
            &self,
            _: &str,
            keys: &[ContentHash],
            _: &str,
        ) -> HashMap<ContentHash, Vec<u8>> {
            keys.iter()
                .filter_map(|k| self.0.get(k).map(|b| (*k, b.clone())))
                .collect()
        }
    }

    fn k(n: u8) -> ContentHash {
        ContentHash::of_raw(&[n])
    }

    /// The nest is asked for exactly what no sibling served, once per key, and
    /// the result keeps the want order with a repeated key repeated.
    #[tokio::test]
    async fn the_nest_gets_only_the_remainder_in_want_order() {
        let sibling = Holds(HashMap::from([(k(1), b"one".to_vec())]));
        let asked = Mutex::new(Vec::new());
        let out = sibling_first(&sibling, "7", &[k(2), k(1), k(2), k(3)], "f", |rest| {
            *asked.lock().unwrap() = rest.clone();
            async move { Ok(rest.iter().map(|r| format!("{r:?}").into_bytes()).collect()) }
        })
        .await
        .unwrap();
        assert_eq!(*asked.lock().unwrap(), vec![k(2), k(3)]);
        assert_eq!(out[1], b"one");
        assert_eq!(out[0], out[2]);
        assert_eq!(out.len(), 4);
    }

    /// A sibling that serves everything leaves the nest unasked; one that
    /// serves nothing leaves the transfer to the nest whole.
    #[tokio::test]
    async fn all_or_nothing_from_a_sibling() {
        let sibling = Holds(HashMap::from([(k(1), b"one".to_vec())]));
        let out = sibling_first(&sibling, "7", &[k(1)], "f", |_| async {
            anyhow::bail!("the nest must not be asked")
        })
        .await
        .unwrap();
        assert_eq!(out, vec![b"one".to_vec()]);

        let none = Holds(HashMap::new());
        let out = sibling_first(&none, "7", &[k(1)], "f", |rest| async move {
            Ok(rest.iter().map(|_| b"nest".to_vec()).collect())
        })
        .await
        .unwrap();
        assert_eq!(out, vec![b"nest".to_vec()]);
    }
}
