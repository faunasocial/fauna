//! UniFFI face over the app side of the region content plane
//! (`libs/fauna-client-region`; `docs/goal/behavior/region-blocking.md`
//! § The content plane → *How an app obtains its region's policy*, *The
//! blocked render and the transparency surface*) — the one surface the four
//! UniFFI apps (macos, ios, windows, android) drive. tui and linux call the
//! crate directly; web reaches it through the wasm twin
//! (`libs/fauna-wasm/src/region.rs`). One shared plane, exposed once at each
//! boundary (priority #2).
//!
//! What stays per app is exactly what the design allows to diverge: the
//! one-function **leaf** naming the declared region (the storefront on a store
//! build, the OS's user-set region otherwise), handed to
//! [`FfiRegionPlane::open`]; the app's config dir, where this face keeps the
//! device record; and the paint. Everything that decides — verify, fold,
//! persist, the refresh cadence, the scorer join, the composed verdict, which
//! verbs get a placeholder and which language of the reason it shows — is
//! shared Rust behind this face.

use std::path::PathBuf;
use std::sync::{Arc, Mutex, MutexGuard};

use fauna_client_region::{
    DeclaredRegion, PolicyState, RegionPlane, RegionSource, RegionView, effective_registry,
};
use fauna_core::content_category::ContentLabelEntry;
use fauna_core::obligation::{ViewerThresholds, render_verdict_composed};

use crate::family::FfiContentPolicy;
use crate::nest_client::FfiNestClient;

/// Where a declared region came from — `fauna_client_region::RegionSource`.
#[derive(uniffi::Enum, Clone, Copy, Debug, PartialEq, Eq)]
pub enum FfiRegionSource {
    /// The app store's storefront (a store-distributed build).
    Storefront,
    /// The OS's user-set region setting.
    SystemRegion,
    /// The POSIX locale's territory.
    SystemLocale,
    /// The browser language's region subtag.
    BrowserLocale,
}

impl From<FfiRegionSource> for RegionSource {
    fn from(s: FfiRegionSource) -> Self {
        match s {
            FfiRegionSource::Storefront => Self::Storefront,
            FfiRegionSource::SystemRegion => Self::SystemRegion,
            FfiRegionSource::SystemLocale => Self::SystemLocale,
            FfiRegionSource::BrowserLocale => Self::BrowserLocale,
        }
    }
}

impl From<RegionSource> for FfiRegionSource {
    fn from(s: RegionSource) -> Self {
        match s {
            RegionSource::Storefront => Self::Storefront,
            RegionSource::SystemRegion => Self::SystemRegion,
            RegionSource::SystemLocale => Self::SystemLocale,
            RegionSource::BrowserLocale => Self::BrowserLocale,
        }
    }
}

/// The declared region as the settings surface shows it.
#[derive(uniffi::Record, Clone, Debug, PartialEq, Eq)]
pub struct FfiDeclaredRegion {
    pub code: String,
    pub source: FfiRegionSource,
    /// The i18n key naming the source and its change path
    /// (`region.source_*`).
    pub source_label_key: String,
}

/// One policy on the declared chain, as the settings surface lists it.
#[derive(uniffi::Record, Clone, Debug, PartialEq, Eq)]
pub struct FfiRegionPolicyRow {
    pub region: String,
    /// As the registry names the authority.
    pub authority_name: String,
    pub sequence: u64,
    /// Unix seconds.
    pub issued_at: u64,
    /// `applied` | `inert` | `malformed`.
    pub state: String,
    /// The unimplemented grammar version, for `inert` — what
    /// `region.inert_notice` names.
    pub inert_version: Option<u32>,
}

/// What the settings region surface paints — `fauna_client_region::RegionView`.
#[derive(uniffi::Record, Clone, Debug, PartialEq, Eq)]
pub struct FfiRegionView {
    /// `None`: the platform declares no region (`region.none_declared`).
    pub declared: Option<FfiDeclaredRegion>,
    /// Most specific first.
    pub policies: Vec<FfiRegionPolicyRow>,
    /// Unix seconds.
    pub last_checked_at: Option<u64>,
    pub stale: bool,
}

/// A region placeholder, ready to paint in place of the item — the app's frame
/// (`region.blocked_notice` / `region.collapsed_notice`) naming `region` and
/// `authority_name`, the authority's name, and `reason` verbatim; a `collapse`
/// adds the reveal.
#[derive(uniffi::Record, Clone, Debug, PartialEq, Eq)]
pub struct FfiRegionPlaceholder {
    /// `block` | `collapse` — also the value the placeholder's `verdict`
    /// attribute carries for the convention-17 invariant.
    pub verb: String,
    pub region: String,
    pub authority_name: String,
    pub reason: String,
}

/// One item's render decision with the region composed in.
#[derive(uniffi::Record, Clone, Debug, PartialEq, Eq)]
pub struct FfiRegionRender {
    /// `show` | `badge` | `collapse` | `block` — the same strictest-wins verdict
    /// `content_render_verdict` returns, with the region as its third source.
    pub verdict: String,
    /// `Some` exactly when the region drove a `block` or `collapse`: paint the
    /// region placeholder, ahead of the family arm (same verb, better
    /// attributed). `None` → the app's existing arms.
    pub placeholder: Option<FfiRegionPlaceholder>,
}

struct Inner {
    plane: RegionPlane,
    last_refresh: Option<u64>,
}

/// The device's region plane — one per app install, opened at launch **ahead
/// of** the first fetch (§ Fail posture) and kept across sign-out and identity
/// switch (a region is a fact about the device, not the account).
///
/// **Interior `Mutex`**, the same shape as `FfiNotifyAccumulator`: a UniFFI
/// object is shared across the binding's threads. The lock is never held
/// across the relay ask.
#[derive(uniffi::Object)]
pub struct FfiRegionPlane {
    inner: Mutex<Inner>,
    record: PathBuf,
}

fn now_secs() -> u64 {
    fauna_core::data::Timestamp::now_secs_or_zero().max(0) as u64
}

impl FfiRegionPlane {
    fn open_with(
        declared: Option<DeclaredRegion>,
        registry: fauna_core::region_authority::RegionRegistry,
        config_dir: PathBuf,
    ) -> Arc<Self> {
        let record = fauna_client_region::store::record_path(&config_dir);
        let mut plane = RegionPlane::new(declared, registry);
        plane.load(
            fauna_client_region::store::read_record(&record).as_deref(),
            now_secs(),
        );
        Arc::new(Self {
            inner: Mutex::new(Inner {
                plane,
                last_refresh: None,
            }),
            record,
        })
    }

    /// Install the leaf's answer (through the e2e override) and persist the
    /// record when the declaration changed.
    fn redeclare_with(&self, leaf: Option<DeclaredRegion>, source: RegionSource) -> bool {
        let declared = fauna_client_region::source::with_e2e_override(leaf, source);
        let bytes = {
            let mut inner = self.lock();
            if !inner.plane.redeclare(declared) {
                return false;
            }
            inner.last_refresh = None;
            inner.plane.to_bytes()
        };
        self.persist(&bytes);
        true
    }

    fn lock(&self) -> MutexGuard<'_, Inner> {
        // A poisoned lock still guards a consistent plane: every mutation is a
        // single fold call.
        self.inner.lock().unwrap_or_else(|p| p.into_inner())
    }

    fn persist(&self, bytes: &[u8]) {
        if let Err(e) = fauna_client_region::store::write_record(&self.record, bytes) {
            tracing::warn!("region: cannot persist the device record: {e}");
        }
    }
}

#[fauna_uniffi_async::export]
impl FfiRegionPlane {
    /// Open the plane: `declared_code` + `source` are the platform leaf's
    /// answer (`None` code — the platform reports no region, or one that is not
    /// a region code — declares nothing); `config_dir` is the app's
    /// install-scoped config directory, under which the device record lives
    /// (`fauna_client_region::store::record_path`). Restores the record now.
    ///
    /// In a test-capable build the e2e override
    /// (`fauna_client_region::source::E2E_DECLARED_ENV`) replaces the leaf's
    /// code, keeping its source — how a journey declares the synthetic region
    /// on a platform whose OS region a test cannot set.
    #[uniffi::constructor]
    pub fn open(
        declared_code: Option<String>,
        source: FfiRegionSource,
        config_dir: String,
    ) -> Arc<Self> {
        let source = RegionSource::from(source);
        let leaf = declared_code
            .as_deref()
            .and_then(|code| DeclaredRegion::from_os_code(code, source));
        let declared = fauna_client_region::source::with_e2e_override(leaf, source);
        Self::open_with(declared, effective_registry(), PathBuf::from(config_dir))
    }

    /// Open the plane for a leaf that answers **asynchronously** (whether this
    /// is a store build, and its storefront): until [`Self::redeclare`] or
    /// [`Self::redeclare_storefront`] lands, it stands on the declaration the
    /// device record carries (`RegionPlane::new_pending`), so a held document
    /// binds from the first paint.
    #[uniffi::constructor]
    pub fn open_pending(config_dir: String) -> Arc<Self> {
        let record = fauna_client_region::store::record_path(&PathBuf::from(config_dir));
        let mut plane = RegionPlane::new_pending(effective_registry());
        plane.load(
            fauna_client_region::store::read_record(&record).as_deref(),
            now_secs(),
        );
        Arc::new(Self {
            inner: Mutex::new(Inner {
                plane,
                last_refresh: None,
            }),
            record,
        })
    }

    /// The asynchronous leaf answered with an OS region (a self-built or
    /// sideloaded build) — `declared_code` + `source` exactly as
    /// [`Self::open`] takes them. Returns whether the declaration changed: the
    /// render must re-read and the next refresh asks for the new chain.
    pub fn redeclare(&self, declared_code: Option<String>, source: FfiRegionSource) -> bool {
        let source = RegionSource::from(source);
        let leaf = declared_code
            .as_deref()
            .and_then(|code| DeclaredRegion::from_os_code(code, source));
        self.redeclare_with(leaf, source)
    }

    /// The asynchronous leaf answered as a **store build**: its storefront's
    /// ISO 3166-1 alpha-3 code (`None`: no storefront — nothing is declared;
    /// a store build never falls back to the OS region).
    pub fn redeclare_storefront(&self, storefront_alpha3: Option<String>) -> bool {
        let leaf = DeclaredRegion::from_storefront_alpha3(storefront_alpha3.as_deref());
        self.redeclare_with(leaf, RegionSource::Storefront)
    }

    /// Ask the session's nest (the relay) for every policy on the declared
    /// chain, verify and fold the answers, and persist the device record when
    /// it changed. Call at login. Returns whether the render must re-read —
    /// repaint the surfaces that composed a verdict.
    ///
    /// A failed ask writes nothing; nothing declared (or nothing enrolled for
    /// it) asks nothing.
    pub async fn refresh(&self, nest: Arc<FfiNestClient>) -> bool {
        let chain = {
            let mut inner = self.lock();
            let chain = inner.plane.chain();
            if chain.is_empty() {
                return false;
            }
            inner.last_refresh = Some(now_secs());
            chain
        };
        let nest = nest.nest_arc();
        let replies = fauna_client_region::fetch_chain(nest.as_ref(), &chain).await;
        let bytes = {
            let mut inner = self.lock();
            if !inner.plane.apply_replies(replies, now_secs()) {
                return false;
            }
            inner.plane.to_bytes()
        };
        self.persist(&bytes);
        true
    }

    /// [`Self::refresh`] when the shared cadence
    /// (`fauna_core::region_authority::REFRESH_INTERVAL_SECS`) is due — the
    /// app's periodic tick calls this; `false` when not due.
    pub async fn refresh_if_due(&self, nest: Arc<FfiNestClient>) -> bool {
        let due = fauna_client_region::refresh_due(self.lock().last_refresh, now_secs());
        if !due {
            return false;
        }
        self.refresh(nest).await
    }

    /// Forget the refresh clock on an identity change, so the next login asks
    /// at once. The plane itself is the device's and stays.
    pub fn clear_session(&self) {
        self.lock().last_refresh = None;
    }

    /// The render decision for one item — the region, the guardian floor and
    /// the viewer's own thresholds composed strictest-wins, with the chain's
    /// scorer factors joined to `labels` first. The same arguments
    /// `content_render_verdict` takes, plus the scorer input a bundled scorer
    /// reads: `content_id_hex` (the item's 32-byte id; `None` for an item
    /// without one), and the item's author (hex actor id; `None` → the zero
    /// id), text, hashtags and whether it carries media.
    #[allow(clippy::too_many_arguments)]
    pub fn render(
        &self,
        labels: Vec<ContentLabelEntry>,
        content_policy: Option<FfiContentPolicy>,
        own_spam_permille: Option<u16>,
        own_phishing_permille: Option<u16>,
        content_id_hex: Option<String>,
        author_hex: Option<String>,
        text: String,
        hashtags: Vec<String>,
        has_media: bool,
        lang: String,
    ) -> FfiRegionRender {
        let inner = self.lock();
        let id = content_id_hex
            .as_deref()
            .and_then(|h| fauna_core::hex32::decode(h).ok());
        let joined = inner.plane.join_labels(id.as_ref(), &labels, || {
            let author = author_hex
                .as_deref()
                .and_then(|h| fauna_core::hex32::decode(h).ok())
                .map(fauna_core::identity::ActorId)
                .unwrap_or(fauna_core::identity::ActorId([0; 32]));
            fauna_client_region::scorer_input(author, &text, &hashtags, has_media)
        });
        let policy = crate::family::render_guardian_policy(content_policy);
        let own = own_spam_permille
            .zip(own_phishing_permille)
            .map(|(s, p)| ViewerThresholds {
                spam_permille: s,
                phishing_permille: p,
            });
        let composed =
            render_verdict_composed(&joined, policy.as_ref(), own, &inner.plane.rule_sets());
        FfiRegionRender {
            verdict: composed.verdict.as_str().to_string(),
            placeholder: fauna_client_region::placeholder_for(&composed, &lang).map(|p| {
                FfiRegionPlaceholder {
                    verb: p.verb.as_str().to_string(),
                    region: p.region.as_str().to_string(),
                    authority_name: p.authority_name,
                    reason: p.reason,
                }
            }),
        }
    }

    /// What the settings region surface paints.
    pub fn view(&self) -> FfiRegionView {
        view_to_ffi(self.lock().plane.view(now_secs()))
    }
}

fn view_to_ffi(view: RegionView) -> FfiRegionView {
    FfiRegionView {
        declared: view.declared.map(|d| FfiDeclaredRegion {
            code: d.code.as_str().to_string(),
            source: d.source.into(),
            source_label_key: d.source.label_key().to_string(),
        }),
        policies: view
            .policies
            .into_iter()
            .map(|p| {
                let (state, inert_version) = match p.state {
                    PolicyState::Applied => ("applied", None),
                    PolicyState::Inert { version } => ("inert", Some(version)),
                    PolicyState::Malformed(_) => ("malformed", None),
                };
                FfiRegionPolicyRow {
                    region: p.region.as_str().to_string(),
                    authority_name: p.authority_name,
                    sequence: p.sequence,
                    issued_at: p.issued_at,
                    state: state.to_string(),
                    inert_version,
                }
            })
            .collect(),
        last_checked_at: view.last_checked_at,
        stale: view.stale,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fauna_client_region::fixtures;
    use fauna_core::region_policy::ContentVerdict;

    fn plane_in(
        dir: &std::path::Path,
        doc: &fauna_core::region_policy::ContentPolicyDocument,
    ) -> Arc<FfiRegionPlane> {
        let now = now_secs();
        let held = fixtures::plane_holding(doc, now);
        fauna_client_region::store::write_record(
            &fauna_client_region::store::record_path(dir),
            &held.to_bytes(),
        )
        .unwrap();
        FfiRegionPlane::open_with(
            DeclaredRegion::from_os_code(fixtures::region().as_str(), RegionSource::SystemRegion),
            fixtures::registry(),
            dir.to_path_buf(),
        )
    }

    fn tmp(name: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("fauna-ffi-region-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        dir
    }

    fn nsfw() -> Vec<ContentLabelEntry> {
        vec![ContentLabelEntry {
            category: "nsfw".into(),
            confidence_per_mille: 900,
        }]
    }

    #[test]
    fn a_stored_policy_binds_at_open_and_paints_its_placeholder() {
        let dir = tmp("binds");
        let reason = "Withheld under the Synthetic Act, section 7.";
        let doc = fixtures::document(
            vec![fixtures::rule("nsfw", ContentVerdict::Block, reason)],
            Vec::new(),
        );
        let plane = plane_in(&dir, &doc);
        let render = plane.render(
            nsfw(),
            None,
            None,
            None,
            None,
            None,
            String::new(),
            Vec::new(),
            false,
            "en".into(),
        );
        assert_eq!(render.verdict, "block");
        let p = render.placeholder.expect("the region drove the block");
        assert_eq!(p.verb, "block");
        assert_eq!(p.region, fixtures::region().as_str());
        assert_eq!(p.reason, reason);

        let view = plane.view();
        let declared = view.declared.expect("declared");
        assert_eq!(declared.source, FfiRegionSource::SystemRegion);
        assert_eq!(declared.source_label_key, "region.source_system_region");
        assert_eq!(view.policies.len(), 1);
        assert_eq!(view.policies[0].state, "applied");
        let _ = std::fs::remove_dir_all(&dir);
    }

    // An nsfw label above the trigger renders `block` under the kids floor, so
    // this pins the unfloored build (`family.rs` `kids_floor_tests` the other).
    #[cfg(not(feature = "kids-floor"))]
    #[test]
    fn a_pending_plane_binds_the_recorded_declaration_until_the_store_leaf_answers() {
        let dir = tmp("pending");
        let reason = "Withheld under the Synthetic Act, section 7.";
        let doc = fixtures::document(
            vec![fixtures::rule("nsfw", ContentVerdict::Block, reason)],
            Vec::new(),
        );
        // A previous run declared the fixture region and held its document.
        let record = fauna_client_region::store::record_path(&dir);
        let mut held = fixtures::plane_holding(&doc, now_secs());
        held.redeclare(DeclaredRegion::from_os_code(
            fixtures::region().as_str(),
            RegionSource::SystemRegion,
        ));
        fauna_client_region::store::write_record(&record, &held.to_bytes()).unwrap();

        let plane = FfiRegionPlane {
            inner: Mutex::new(Inner {
                plane: {
                    let mut p = RegionPlane::new_pending(fixtures::registry());
                    p.load(
                        fauna_client_region::store::read_record(&record).as_deref(),
                        now_secs(),
                    );
                    p
                },
                last_refresh: None,
            }),
            record: record.clone(),
        };
        let render = |p: &FfiRegionPlane| {
            p.render(
                nsfw(),
                None,
                None,
                None,
                None,
                None,
                String::new(),
                Vec::new(),
                false,
                "en".into(),
            )
        };
        assert_eq!(
            render(&plane).verdict,
            "block",
            "bound from the first paint"
        );

        // The store leaf answers with a storefront outside the fixture region.
        assert!(plane.redeclare_storefront(Some("NOR".into())));
        let view = plane.view();
        let declared = view.declared.expect("the storefront declares");
        assert_eq!(declared.code, "NO");
        assert_eq!(declared.source, FfiRegionSource::Storefront);
        assert_eq!(render(&plane).verdict, "badge");
        // …and the new declaration is what the record now carries.
        let mut reread = RegionPlane::new_pending(fixtures::registry());
        reread.load(
            fauna_client_region::store::read_record(&record).as_deref(),
            now_secs(),
        );
        assert_eq!(reread.declared().map(|d| d.code.as_str()), Some("NO"));

        // A store build with no storefront declares nothing.
        assert!(plane.redeclare_storefront(None));
        assert!(plane.view().declared.is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }

    // An nsfw label above the trigger renders `block` under the kids floor, so
    // this pins the unfloored build (`family.rs` `kids_floor_tests` the other).
    #[cfg(not(feature = "kids-floor"))]
    #[test]
    fn an_undeclared_device_renders_the_family_arms_unchanged() {
        let dir = tmp("undeclared");
        let plane = FfiRegionPlane::open_with(None, fixtures::registry(), dir.clone());
        let render = plane.render(
            nsfw(),
            Some(FfiContentPolicy {
                nsfw: "collapse".into(),
                spam: "inherit".into(),
                phishing: "inherit".into(),
                commercial: "inherit".into(),
            }),
            None,
            None,
            None,
            None,
            String::new(),
            Vec::new(),
            false,
            "en".into(),
        );
        assert_eq!(render.verdict, "collapse");
        assert!(render.placeholder.is_none(), "the family floor drove it");
        assert!(plane.view().declared.is_none());
    }
}
