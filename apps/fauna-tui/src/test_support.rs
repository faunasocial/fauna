//! Cross-module test fixtures — helpers three or more `#[cfg(test)] mod tests`
//! blocks in different files independently hand-rolled byte-identically before
//! this lift (found by the same-crate arm of the dev-fleet near-duplicate-
//! function scanner, which the workspace's usual `--cross-crate-only` sweeps
//! never run, so a same-crate copy could hide indefinitely).

/// Assert every action `wire_kind` yields is registered in
/// `fauna_protocol::offline_class` — the shared body of each module's own
/// `every_declared_<X>_kind_is_registered` test (`admin`, `backups`,
/// `conversations`, `family`, `media`, `settings`), each of which otherwise
/// hand-copied this exact loop over its own action enum's own enumerator.
#[cfg(test)]
pub fn assert_every_wire_kind_is_registered<A: std::fmt::Debug>(
    actions: impl IntoIterator<Item = A>,
    wire_kind: impl Fn(&A) -> Option<&'static str>,
) {
    for action in actions {
        let Some(kind) = wire_kind(&action) else {
            continue;
        };
        assert!(
            fauna_protocol::offline_class::offline_class(kind).is_some(),
            "{action:?} declares {kind}, which is not registered in \
             `fauna_protocol::offline_class` — an unregistered kind reads \
             as Available, so this affordance would be silently ungated"
        );
    }
}

/// A one-cell rasterized thumbnail standing in for the real rasterizer's
/// output, in **both** arms' representations exactly as it always produces
/// them — plaintext half-block art (a single `▀`) and the pixel buffer.
/// Shared by `document`, `media`, and `feed`'s test modules.
#[cfg(test)]
pub fn art() -> crate::thumbnail::Thumbnail {
    crate::thumbnail::Thumbnail {
        art: crate::thumbnail::HalfBlockArt {
            rows: vec![vec![crate::thumbnail::HalfBlockCell {
                top: [1, 2, 3],
                bottom: [4, 5, 6],
            }]],
        },
        pixels: std::sync::Arc::new(image::RgbImage::from_pixel(1, 2, image::Rgb([1, 2, 3]))),
    }
}
