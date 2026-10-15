//! The web paywall's one servability check — "is `path` a paywall-servable
//! resource of this site, and which tier gates it?" — shared by every door
//! that mints a paywall token: the creator comp-link
//! (`fauna.web.paywall.mint_token`) and the visitor redeem door
//! (`monetization.md` § Pillar 2 → *Visitor entitlement doors* ruling (4)).
//!
//! Two resource halves, one answer shape:
//!
//! - **a post** — a gated, web-published post's sealed full page: a
//!   `web_rendered_sealed` row at `post/<slug>.html`, whose `tier` column names
//!   the gating tier;
//! - **a folder file** — a sealed file synced into a `web`-mode set whose
//!   `folders.web_paywall_tier` is set.
//!
//! Anything else (a public file, an unpublished or ungated post, a sealed file
//! in an unpaywalled set) is not paywall-servable: a token for it would only
//! ever yield the teaser or nothing, so every door refuses it up front.

use anyhow::Result;

use crate::db::CacheDb;

/// Which half a [`PaywallResource`] is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PaywallResourceKind {
    /// A gated, published post's sealed rendered page.
    Post,
    /// A sealed file in a paywalled `web` folder set.
    FolderFile,
}

/// A paywall-servable resource of one site.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PaywallResource {
    /// The path exactly as a paywall token scopes it (no leading `/`).
    pub path: String,
    /// The tier that gates it: the sealed row's `tier`, or the set's
    /// `web_paywall_tier`.
    pub tier: String,
    pub kind: PaywallResourceKind,
}

/// Resolve `path` on `owner`'s site to a paywall-servable resource, or `None`.
/// A leading `/` is ignored, as on the serving path.
pub async fn paywall_resource(
    db: &CacheDb,
    owner: &[u8; 32],
    path: &str,
) -> Result<Option<PaywallResource>> {
    let path = path.trim_start_matches('/');
    if path.is_empty() {
        return Ok(None);
    }
    if let Some(sealed) = db.get_web_rendered_sealed(owner, path).await? {
        return Ok(Some(PaywallResource {
            path: path.to_string(),
            tier: sealed.tier,
            kind: PaywallResourceKind::Post,
        }));
    }
    let Some(row) = db.get_web_file(owner, path).await? else {
        return Ok(None);
    };
    if !row.is_sealed() {
        return Ok(None);
    }
    let Some(folder_id) = row.folder_id else {
        return Ok(None);
    };
    Ok(db
        .get_folder_by_id(folder_id)
        .await?
        .and_then(|set| set.web_paywall_tier)
        .map(|tier| PaywallResource {
            path: path.to_string(),
            tier,
            kind: PaywallResourceKind::FolderFile,
        }))
}
