//! Sandboxed WASM community-labeler runtime.
//!
//! Loads and executes WASM labeler modules in a sandboxed wasmi environment
//! with fuel-based CPU limiting and configurable memory limits. Shared between
//! the nest publish-gate (compile-validation at `fauna.labelers.publish`) and
//! the FFI content-processor holder that runs `label()` over unsealed content
//! (`content-moderation-and-ranking.md` § Tier-3; design
//! `2026-07-07-labeler-registry-design.md` §6 — the execution boundary).
//!
//! The runtime uses an **empty `Linker`** (no WASI, no clock, no RNG imports),
//! which is the determinism + no-host-I/O property the tier requires.
//!
//! ## Module Protocol
//!
//! A WASM labeler module must export:
//! - `memory`                              — linear memory
//! - `alloc(size: i32) -> ptr: i32`        — allocate `size` bytes, return pointer
//! - `label(ptr: i32, len: i32) -> out_ptr: i32` — label input, return output pointer
//!
//! The host writes the BARE-encoded input into the module's memory via
//! `alloc`, then calls `label(ptr, len)`.  The output at `out_ptr` is a 4-byte
//! little-endian length followed by that many bytes of BARE-encoded `Vec<Label>`.
//!
//! **Which input a module reads is its own signed declaration**
//! (`fauna_core::scoring::LabelerInput`, `content-moderation-and-ranking.md`
//! § Tier-3 → *The attachment facet*): a module that did not declare
//! `needs_attachment_bytes` reads `LabelerPostInput` — every module before the
//! flag, unchanged; one that did reads `LabelerInputWithAttachments`, which is
//! the same bytes followed by the attachment facet. [`encode_labeler_input_for`]
//! is the one encoder every position uses, so no position can hand a module a
//! shape it did not declare. The host writes the whole input in one `alloc`, so
//! a module that asks for bytes sizes its linear memory for the facet ceiling
//! ([`LABELER_ATTACHMENT_FACET_MAX_BYTES`]) plus its own working set.
//!
//! **Which output a module emits is its own signed declaration too**
//! (`fauna_core::scoring::LabelerOutput`, `content-moderation-and-ranking.md`
//! § Tier-3 → *The output half of the `label()` ABI*): `label_abi` names the
//! revision of the `Label` record in the BARE `Vec<Label>` it returns —
//! absent, revision 1, the shape every module before the stamp emitted. The
//! output is positional, so a revision this build does not know cannot be
//! decoded at all: [`run_published_labeler_bare`] reads the stamp after the
//! signature verify and **before compiling the module**, refusing
//! `label_abi > LABEL_ABI_CURRENT` with the typed [`LabelerAbiNewer`] — a
//! newer module, never a decode error that reads as a broken one.

use anyhow::{Context, Result, bail};
use fauna_core::identity::ActorId;
use fauna_core::scoring::{
    AlgorithmLabeler, LABEL_ABI_CURRENT, Label, LabelerAttachmentInput, LabelerInput,
    LabelerPostInput, verify_labeler_metadata_for,
};
use wasmi::*;

pub mod region;

/// A verified module declares a `Label` record revision newer than this build
/// reads ([`fauna_core::scoring::LabelerOutput::label_abi`] above
/// [`LABEL_ABI_CURRENT`]): the runner refuses it before compiling, so the
/// refusal says "newer" rather than surfacing as a decode failure that reads
/// as a broken module (`content-moderation-and-ranking.md` § Tier-3 → *The
/// output half of the `label()` ABI*). Carried inside the `anyhow::Error`
/// [`run_published_labeler_bare`] returns; a caller tells it apart by
/// `downcast_ref`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error(
    "labeler emits Label record revision {declared}; this host reads up to revision {supported}"
)]
pub struct LabelerAbiNewer {
    pub declared: u16,
    pub supported: u16,
}

/// Host ceiling on a labeler run's linear memory (bytes) — the F4 clamp bound.
///
/// A labeler's `resource_limits` are **publisher-declared and self-signed**
/// (`AlgorithmLabeler.resource_limits`), so the adversary who wrote a malicious
/// module also picks its sandbox ceiling. The holder MUST NOT trust them as the
/// enforcement bound: it clamps each declared limit to the host ceiling
/// (`min(declared, ceiling)`) before constructing a runtime (
/// labeler-registry design § 6). A well-behaved publisher declaring *less* keeps
/// its lower limit; one declaring *more* is capped here. 64 MiB comfortably fits
/// a ≤ 1 MiB module's working set while bounding per-item host memory.
pub const LABELER_HOST_MAX_MEMORY_BYTES: u64 = 64 * 1024 * 1024;

/// Host ceiling on a labeler run's CPU budget (microseconds) — the F4 clamp
/// bound for the fuel limit (see [`LABELER_HOST_MAX_MEMORY_BYTES`]). Fuel is the
/// deterministic termination guarantee (`cpu_us × 1000` instructions), so this
/// ceiling — not a wall clock — is the CPU bound; 1 s per item bounds drain
/// latency while leaving ample headroom for a single-item labeling pass.
pub const LABELER_HOST_MAX_CPU_MICROSECONDS: u64 = 1_000_000;

/// Per-attachment ceiling on the bytes a host hands a module that declared
/// `needs_attachment_bytes` (`content-moderation-and-ranking.md` § Tier-3 →
/// *The attachment facet*). An attachment over it rides the facet with its
/// declared metadata and **empty bytes** — withheld, never truncated, so a
/// module never classifies half a picture.
///
/// A constant, never a knob (`principles.md` § One configuration surface):
/// the bound follows from [`LABELER_HOST_MAX_MEMORY_BYTES`] — the whole input
/// lands in the module's linear memory in one `alloc`, so the facet must leave
/// the module room to work — not from anything a deployment would choose.
/// 8 MiB is a comfortably large photo; a video is over it by construction and
/// reaches a module as metadata only.
pub const LABELER_ATTACHMENT_BYTES_MAX: u64 = 8 * 1024 * 1024;

/// Ceiling on the whole attachment facet of one item — the sum of the bytes
/// handed over across its attachments, in the author's order; once reached,
/// every later attachment is withheld ([`LABELER_ATTACHMENT_BYTES_MAX`]'s
/// rule). 24 MiB of a 64 MiB sandbox leaves a module reading the largest
/// facet at least 40 MiB to work in.
pub const LABELER_ATTACHMENT_FACET_MAX_BYTES: u64 = 24 * 1024 * 1024;

/// Ceiling on how many of one item's attachments the host **works on** —
/// fetches and opens — before the rest ride as their declared metadata with
/// empty bytes.
///
/// The two byte ceilings above bound what a module is *handed*; this one
/// bounds what the host *does to produce it*. The distinction matters because
/// the list the facet loop walks is the item's own **sealed** body — a room
/// message's `attachments`, a room post's media — which its author writes and
/// which no send-time check can bound, precisely because it is sealed. Without
/// this ceiling one message inside the envelope budget can name one 8 MiB blob
/// tens of thousands of times, and the home nest reads and AEAD-opens 8 MiB
/// per entry inline in the send path.
///
/// 64 is the nest's own per-record pin ceiling
/// (`conversations_handlers::MAX_ATTACHMENT_REFS_PER_RECORD`, pinned equal to
/// this constant by a static assertion beside it): a record cannot pin more
/// blobs than that, so an item naming more attachments than that is naming
/// blobs its own record never pinned. A module still **sees** every
/// attachment past the ceiling — the declared metadata rides in the author's
/// order, withheld rather than dropped — so a module that cares what it was
/// not shown can tell by `size_bytes`, exactly as for any other withheld
/// attachment.
pub const LABELER_ATTACHMENT_FACET_MAX_CANDIDATES: usize =
    fauna_core::attachment_limits::MAX_ATTACHMENTS_PER_RECORD;

/// The facet's admission rule as a pure value — the one place the two
/// ceilings are applied, so the nest's pass and any future position that
/// holds bytes admit identically.
///
/// Admission is by the **opened** length, never the author's declared size: a
/// member can declare anything, and the ceilings bound what the module is
/// handed. (A declared size over the per-attachment ceiling is a fine reason
/// not to fetch the blob at all — that is a cheap pre-filter, not the rule.)
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct AttachmentFacetBudget {
    handed_over: u64,
}

impl AttachmentFacetBudget {
    /// A fresh budget for one item.
    pub fn new() -> Self {
        Self::default()
    }

    /// Admit `len` bytes of one attachment into the facet — `true` and the
    /// budget spent when it fits under both ceilings, `false` and nothing
    /// spent when it does not (the caller hands the attachment over with
    /// empty bytes).
    pub fn admit(&mut self, len: u64) -> bool {
        if len > LABELER_ATTACHMENT_BYTES_MAX {
            return false;
        }
        match self.handed_over.checked_add(len) {
            Some(total) if total <= LABELER_ATTACHMENT_FACET_MAX_BYTES => {
                self.handed_over = total;
                true
            }
            _ => false,
        }
    }

    /// Bytes admitted so far.
    pub fn handed_over(&self) -> u64 {
        self.handed_over
    }

    /// What is left of the per-item ceiling — `0` once the facet is full.
    ///
    /// The spent-budget test is this, never a failed [`Self::admit`]: a full
    /// budget still admits a *zero*-length plaintext, and a budget with room
    /// left refuses an attachment too big for the remainder while admitting a
    /// smaller one after it. Only `remaining() == 0` means no later
    /// attachment can be handed any bytes at all — and then none of them is
    /// worth fetching or decrypting.
    pub fn remaining(&self) -> u64 {
        LABELER_ATTACHMENT_FACET_MAX_BYTES.saturating_sub(self.handed_over)
    }
}

/// Slack over [`LABELER_ATTACHMENT_BYTES_MAX`] allowed for a *sealed* blob
/// before the host declines to open it at all: a sealed blob is its plaintext
/// plus the AEAD framing, so anything past the ceiling by more than that is a
/// lying declared size and is not worth decrypting.
pub const SEALED_FRAMING_ALLOWANCE: u64 = 4096;

/// One attachment as the facet loop sees it — what its author declared, which
/// is exactly what rides the facet when the host withholds the bytes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FacetCandidate {
    /// Declared MIME type, handed over verbatim.
    pub mime_type: String,
    /// The author's declared plaintext size: a cheap reason not to fetch a
    /// blob at all, never the admission rule ([`AttachmentFacetBudget`]).
    pub size_bytes: u64,
    /// The content address this candidate's bytes live at — the same address
    /// the caller's `fetch` keys by. **`Some` is what makes the loop able to
    /// notice that two entries name one blob**; `None` disables that for this
    /// entry (a caller with no address to give), which costs work and changes
    /// nothing a module sees.
    ///
    /// The facet carries no identity, so this never reaches a module: it is
    /// the host's bookkeeping, and exists only so an author who names one
    /// upload sixty-four times is paid for once.
    pub address: Option<[u8; 32]>,
}

/// Build one item's attachment facet under the ABI's ceilings
/// (`content-moderation-and-ranking.md` § Tier-3 → *The attachment facet*):
/// each candidate's **opened plaintext** in the author's order, or its
/// declared metadata with **empty bytes** where the host withholds it — over
/// a ceiling, not stored, or it did not open. Withheld, never truncated, so a
/// module never classifies half a picture.
///
/// **The one home for every bounding rule** — the per-item candidate ceiling
/// ([`LABELER_ATTACHMENT_FACET_MAX_CANDIDATES`]), the declared-size
/// pre-filter, the **sealed-length probe** (`sealed_size`, below), the
/// sealed-size allowance, the remaining-budget pre-check,
/// [`AttachmentFacetBudget`]'s admission by opened length, and **one fetch and
/// one open per distinct address** ([`FacetCandidate::address`]).
///
/// **`sealed_size` is the pre-read bound** — the host's own measurement of a
/// candidate's stored sealed length, or `None` for "no answer". Given one, the
/// loop decides both sealed-length refusals *before* the read, which is what
/// makes rule (4)'s "never fetched" true rather than aspirational; given
/// `None` it reads and decides after, exactly as it did before the probe
/// existed. A position with no probe to offer passes `|_| async { None }` and
/// is bounded as it always was.
///
/// The last of those is why the ceilings mean what they say. Every other rule
/// keys on *bytes handed over*, and an entry that never opens hands over
/// nothing — so before the dedupe an author who named one stored-but-
/// unopenable blob 64 times paid for 64 blob reads and 64 failed AEADs while
/// the budget sat untouched at its full ceiling. Now the second
/// and later entries naming an address the loop already resolved are served
/// from that one resolution: the same plaintext, admitted position by position
/// exactly as a wasteful host would admit it, or the same withholding.
///
/// The ceilings bound **the host's own work** as well as the module's input: a
/// candidate that cannot be admitted is never fetched and never decrypted, and
/// it rides the facet as its declared metadata in the author's order all the
/// same. Withheld, never dropped — so the facet a module reads is identical to
/// one built by a host that did all the wasted work, and only the work
/// differs. A caller supplies only what differs between
/// positions: `fetch` reads candidate `i`'s sealed blob (`None` = not stored,
/// unreadable, or no store at all), and `open` decrypts it under that
/// position's own key model (`None` = it did not open). A room message's
/// attachments open under the message's generation; a room-restricted post's
/// media open under its per-post key — two key models, one set of rules, so
/// the facets a module is handed cannot drift apart.
///
/// The opened bytes live only as long as the returned facet: no caller may
/// persist them (`community-rooms.md` § The three classes → *Forbidden*).
pub async fn build_attachment_facet<Size, SizeFut, Fetch, FetchFut, Open, OpenFut>(
    candidates: &[FacetCandidate],
    sealed_size: Size,
    fetch: Fetch,
    open: Open,
) -> Vec<LabelerAttachmentInput>
where
    Size: Fn(usize) -> SizeFut,
    SizeFut: core::future::Future<Output = Option<u64>>,
    Fetch: Fn(usize) -> FetchFut,
    FetchFut: core::future::Future<Output = Option<Vec<u8>>>,
    Open: Fn(usize, Vec<u8>) -> OpenFut,
    OpenFut: core::future::Future<Output = Option<Vec<u8>>>,
{
    let mut budget = AttachmentFacetBudget::new();
    let mut facet = Vec::with_capacity(candidates.len());
    // Which addresses this item names more than once — the only ones worth
    // remembering. An honest item names each of its attachments once, and
    // keeping a copy of every opened plaintext for a reuse that can never
    // come would add up to the whole byte budget in clones to the common
    // case. One pass over the declared list decides it, before any blob is
    // touched.
    let repeated: std::collections::BTreeSet<[u8; 32]> = {
        let mut seen = std::collections::BTreeSet::new();
        let mut twice = std::collections::BTreeSet::new();
        for address in candidates.iter().filter_map(|c| c.address) {
            if !seen.insert(address) {
                twice.insert(address);
            }
        }
        twice
    };
    // address -> what resolving it yielded: the opened plaintext, or `None`
    // for "this address does not resolve here" (not stored, over the sealed
    // ceiling, or it did not open). Both answers are worth remembering: the
    // second is the hostile case's whole cost. Bounded by the budget itself —
    // once `remaining()` is 0 nothing later is fetched, so nothing later is
    // cached.
    let mut resolved: std::collections::BTreeMap<[u8; 32], Option<Vec<u8>>> =
        std::collections::BTreeMap::new();
    for (i, candidate) in candidates.iter().enumerate() {
        let withheld = LabelerAttachmentInput {
            mime_type: candidate.mime_type.clone(),
            size_bytes: candidate.size_bytes,
            bytes: Vec::new(),
        };
        // Past the per-item work ceiling: the author can name any number of
        // attachments inside one sealed body, and every one past this point
        // rides as declared metadata — so none of them is fetched or opened.
        // Not a `break`: the metadata still rides, in the author's order.
        if i >= LABELER_ATTACHMENT_FACET_MAX_CANDIDATES {
            facet.push(withheld);
            continue;
        }
        // The facet is full. Nothing later can be handed any bytes, so
        // nothing later is worth a blob read or an AEAD.
        let remaining = budget.remaining();
        if remaining == 0 {
            facet.push(withheld);
            continue;
        }
        if candidate.size_bytes > LABELER_ATTACHMENT_BYTES_MAX {
            facet.push(withheld);
            continue;
        }
        // An address this loop already resolved is not fetched or opened
        // again. The cached plaintext still goes through `admit` below,
        // position by position, so the facet is byte-identical to one a host
        // that re-did all the work would build — only the work differs.
        if let Some(address) = candidate.address
            && repeated.contains(&address)
            && let Some(cached) = resolved.get(&address)
        {
            match cached {
                Some(plaintext) => {
                    let opened = plaintext.clone();
                    if budget.admit(opened.len() as u64) {
                        facet.push(LabelerAttachmentInput {
                            mime_type: candidate.mime_type.clone(),
                            size_bytes: candidate.size_bytes,
                            bytes: opened,
                        });
                    } else {
                        facet.push(withheld);
                    }
                }
                None => facet.push(withheld),
            }
            continue;
        }
        // **Ask how big it is before reading it.** Both sealed-length
        // refusals below are pure functions of one number, so a position that
        // can answer it without a read decides them without one — which is
        // the whole of rule (4)'s "a candidate that cannot be admitted is
        // never fetched and never decrypted". The number must be the host's
        // own measurement of what it stored, never `candidate.size_bytes`
        // above: that one is the author's declaration inside a sealed body.
        //
        // ⚠ **`None` is "no answer", never "refuse."** A position may have no
        // probe at all, and one that has may hold bytes with no metadata row
        // behind them; neither is evidence about the blob, so both fall
        // through to the read this loop did before the probe existed. Same
        // trap, same resolution, as the empty-refs rule
        // (`content-moderation-and-ranking.md` § Tier-3 → *The attachment
        // facet*, rule (4)'s ⚠).
        if let Some(sealed_len) = sealed_size(i).await {
            // The two refusals below, read from one number:
            //   `sealed > MAX + ALLOWANCE`         (the absolute ceiling)
            //   `sealed - ALLOWANCE > remaining`   (this item's remainder)
            // which together are `sealed > min(MAX, remaining) + ALLOWANCE`.
            // Keeping the absolute ceiling inside the `min` is what stops a
            // roomy budget from admitting a read past the per-attachment
            // ceiling.
            let cap = LABELER_ATTACHMENT_BYTES_MAX.min(remaining) + SEALED_FRAMING_ALLOWANCE;
            if sealed_len > cap {
                // Cached only when the refusal is about the BLOB — over the
                // absolute ceiling, so no position in any item could admit it
                // — and not when it is about this position's remainder, which
                // shrinks as the loop runs and says nothing about the address.
                // That is the same split the post-read checks make; this
                // mirrors it rather than inventing a second rule.
                if sealed_len > LABELER_ATTACHMENT_BYTES_MAX + SEALED_FRAMING_ALLOWANCE
                    && let Some(address) = candidate.address
                    && repeated.contains(&address)
                {
                    resolved.insert(address, None);
                }
                facet.push(withheld);
                continue;
            }
        }
        let Some(sealed) = fetch(i).await else {
            if let Some(address) = candidate.address
                && repeated.contains(&address)
            {
                resolved.insert(address, None);
            }
            facet.push(withheld);
            continue;
        };
        if sealed.len() as u64 > LABELER_ATTACHMENT_BYTES_MAX + SEALED_FRAMING_ALLOWANCE {
            if let Some(address) = candidate.address
                && repeated.contains(&address)
            {
                resolved.insert(address, None);
            }
            facet.push(withheld);
            continue;
        }
        // The plaintext cannot fit what is left of the facet, so `admit`
        // would refuse it: don't pay the AEAD to learn that. A seal is its
        // plaintext plus framing under every position's key model, so
        // `sealed.len() - SEALED_FRAMING_ALLOWANCE` is a floor on the opened
        // length — the same assumption the absolute check above already rests
        // on, read in the other direction.
        if (sealed.len() as u64).saturating_sub(SEALED_FRAMING_ALLOWANCE) > remaining {
            facet.push(withheld);
            continue;
        }
        let Some(opened) = open(i, sealed).await else {
            // A sealed blob that does not open under this item's key model
            // never will, however many entries name it.
            if let Some(address) = candidate.address
                && repeated.contains(&address)
            {
                resolved.insert(address, None);
            }
            facet.push(withheld);
            continue;
        };
        if let Some(address) = candidate.address
            && repeated.contains(&address)
        {
            resolved.insert(address, Some(opened.clone()));
        }
        if !budget.admit(opened.len() as u64) {
            facet.push(withheld);
            continue;
        }
        facet.push(LabelerAttachmentInput {
            mime_type: candidate.mime_type.clone(),
            size_bytes: candidate.size_bytes,
            bytes: opened,
        });
    }
    facet
}

/// Clamp publisher-declared `(memory_bytes, cpu_microseconds)` to the host
/// ceilings ([`LABELER_HOST_MAX_MEMORY_BYTES`] / [`LABELER_HOST_MAX_CPU_MICROSECONDS`]) —
/// the F4 guard a holder applies before building a [`LabelerRuntime`].
pub fn clamp_labeler_limits(memory_bytes: u64, cpu_microseconds: u64) -> (u64, u64) {
    (
        memory_bytes.min(LABELER_HOST_MAX_MEMORY_BYTES),
        cpu_microseconds.min(LABELER_HOST_MAX_CPU_MICROSECONDS),
    )
}

/// A reusable WASM labeler engine with shared configuration.
///
/// Holds a wasmi [`Engine`] configured with fuel-based CPU limiting.
/// Create one per process and use it to load multiple labeler modules.
pub struct LabelerRuntime {
    engine: Engine,
    memory_limit_bytes: u64,
    cpu_limit_fuel: u64,
}

/// A compiled WASM labeler module, ready to be instantiated.
pub struct LoadedLabeler {
    module: Module,
}

impl LabelerRuntime {
    /// Create a new labeler runtime.
    ///
    /// - `memory_limit_bytes`: maximum WASM linear memory allowed (bytes).
    /// - `cpu_limit_us`: CPU budget in microseconds; converted to fuel units
    ///   (multiplied by 1000).
    pub fn new(memory_limit_bytes: u64, cpu_limit_us: u64) -> Result<Self> {
        let mut config = Config::default();
        config.consume_fuel(true);
        let engine = Engine::new(&config);
        Ok(Self {
            engine,
            memory_limit_bytes,
            cpu_limit_fuel: cpu_limit_us.saturating_mul(1000),
        })
    }

    /// Compile a WASM labeler module from raw bytes.
    ///
    /// Returns an error if the bytes are not valid WASM.
    pub fn load_module(&self, wasm_bytes: &[u8]) -> Result<LoadedLabeler> {
        let module =
            Module::new(&self.engine, wasm_bytes).context("compile WASM labeler module")?;
        Ok(LoadedLabeler { module })
    }

    /// Execute a loaded labeler against BARE-encoded input bytes.
    ///
    /// Each call creates a fresh WASM instance with its own memory and fuel
    /// budget, so concurrent calls are safe.
    ///
    /// Returns the raw BARE-encoded `Vec<Label>` payload (without the 4-byte
    /// length prefix).
    pub fn execute(&self, labeler: &LoadedLabeler, input_bytes: &[u8]) -> Result<Vec<u8>> {
        // Create a store with the configured fuel budget AND a resource limiter
        // that caps linear-memory growth. `memory_size`
        // bounds EACH linear memory to `memory_limit_bytes` — enforced on the
        // initial allocation at instantiation and on every `memory.grow`, so a
        // module cannot OOM the holder by growing past the cap. Fuel bounds CPU,
        // not allocation: without this, wasmi permits `memory.grow` up to the
        // wasm32 4 GiB ceiling for ~1 fuel unit.
        let limits = StoreLimitsBuilder::new()
            .memory_size(self.memory_limit_bytes as usize)
            .build();
        let mut store = Store::new(&self.engine, limits);
        store.limiter(|limits| limits);
        store
            .set_fuel(self.cpu_limit_fuel)
            .context("set fuel budget")?;

        // Instantiate the module. A module whose declared initial memory exceeds
        // the cap fails here — the limiter denies the initial allocation.
        let linker = Linker::new(&self.engine);
        let instance = linker
            .instantiate_and_start(&mut store, &labeler.module)
            .context("instantiate WASM labeler module")?;

        // Obtain required exports.
        let memory = instance
            .get_memory(&store, "memory")
            .context("WASM labeler must export 'memory'")?;
        let alloc_fn = instance
            .get_typed_func::<i32, i32>(&store, "alloc")
            .context("WASM labeler must export 'alloc(i32) -> i32'")?;
        let label_fn = instance
            .get_typed_func::<(i32, i32), i32>(&store, "label")
            .context("WASM labeler must export 'label(i32, i32) -> i32'")?;

        // (The initial-memory-size check is now enforced by the store limiter
        // above, which also bounds growth — see F2 note at store creation.)

        // Allocate space in the module's memory for the input.
        let input_len = input_bytes.len() as i32;
        let input_ptr = alloc_fn
            .call(&mut store, input_len)
            .context("alloc call failed")?;
        if input_ptr < 0 {
            bail!("alloc returned negative pointer: {}", input_ptr);
        }

        // Write the input bytes into the module's memory.
        memory
            .write(&mut store, input_ptr as usize, input_bytes)
            .context("write input to WASM memory")?;

        // Call the label function.
        let output_ptr = label_fn
            .call(&mut store, (input_ptr, input_len))
            .context("label call failed")?;
        if output_ptr < 0 {
            bail!("label returned negative pointer: {}", output_ptr);
        }

        // Read the 4-byte little-endian length prefix at output_ptr.
        let mut len_buf = [0u8; 4];
        memory
            .read(&store, output_ptr as usize, &mut len_buf)
            .context("read output length from WASM memory")?;
        let payload_len = u32::from_le_bytes(len_buf) as usize;

        if payload_len == 0 {
            return Ok(vec![]);
        }

        // Bound the untrusted output length against the module's actual memory
        // BEFORE allocating. `payload_len` is an
        // attacker-controlled u32 (up to 4 GiB); a module with tiny memory
        // returning a `0xFFFFFFFF` prefix would otherwise force a ~4 GiB host
        // allocation (`vec![0u8; payload_len]`) even though the subsequent read
        // would fail. The payload must lie wholly inside the linear memory, so
        // reject anything that would run past it.
        let payload_offset = output_ptr as usize + 4;
        let mem_size = memory.data_size(&store);
        let fits = payload_offset
            .checked_add(payload_len)
            .is_some_and(|end| end <= mem_size);
        if !fits {
            bail!(
                "labeler output length {payload_len} at offset {payload_offset} \
                 exceeds WASM memory ({mem_size} bytes)"
            );
        }

        // Read the payload bytes that follow the length prefix.
        let mut payload = vec![0u8; payload_len];
        memory
            .read(&store, payload_offset, &mut payload)
            .context("read output payload from WASM memory")?;

        Ok(payload)
    }

    /// [`Self::execute`], then the BARE decode of the module's `Vec<Label>` —
    /// the two steps every caller that wants labels rather than bytes takes. An
    /// empty output is "detected nothing" and answers an empty vec (an empty
    /// slice is not a valid BARE `Vec<Label>`).
    pub fn execute_labels(
        &self,
        labeler: &LoadedLabeler,
        input_bytes: &[u8],
    ) -> Result<Vec<fauna_core::scoring::Label>> {
        let output = self.execute(labeler, input_bytes)?;
        if output.is_empty() {
            return Ok(Vec::new());
        }
        serde_bare::from_slice(&output).context("decode labeler output (BARE Vec<Label>)")
    }
}

/// BARE-encode one item's post-shaped input for the `label()` ABI (module doc
/// § Module Protocol) — the v1 shape every module reads. A module that declared
/// `needs_attachment_bytes` reads these bytes **followed by** the facet:
/// [`encode_labeler_input_for`] is the encoder that knows which.
pub fn encode_labeler_input(input: &LabelerPostInput) -> Result<Vec<u8>> {
    serde_bare::to_vec(input).context("labeler input BARE encode")
}

/// BARE-encode the attachment facet alone — what follows the post-shaped
/// bytes for a module that declared `needs_attachment_bytes`.
pub fn encode_attachment_facet(attachments: &[LabelerAttachmentInput]) -> Result<Vec<u8>> {
    serde_bare::to_vec(attachments).context("labeler attachment facet BARE encode")
}

/// Encode the `label()` input **in the shape the module declared**: the v1
/// post-shaped bytes for a module without `needs_attachment_bytes`, whatever
/// `attachments` the caller holds; the same bytes followed by the facet for a
/// module with it — an empty facet when the position holds no bytes, so the
/// module always reads the shape it declared and never a silent v1.
///
/// The concatenation is `fauna_core::scoring::LabelerInputWithAttachments`'s
/// own BARE encoding (a struct is its fields in order, unframed); pinned by
/// `a_flagged_input_is_the_post_input_followed_by_the_facet`.
pub fn encode_labeler_input_for(
    schema: &LabelerInput,
    input: &LabelerPostInput,
    attachments: &[LabelerAttachmentInput],
) -> Result<Vec<u8>> {
    let mut bytes = encode_labeler_input(input)?;
    if schema.needs_attachment_bytes {
        bytes.extend(encode_attachment_facet(attachments)?);
    }
    Ok(bytes)
}

/// Run one **published** labeler over one item and return what it labelled —
/// the whole execution boundary, in the one copy every position that runs a
/// `wasm` labeler shares: the capability-holder content-processor (through
/// the FFI), and a community room's home nest as that room's
/// capability-holder (`content-scoring.md` § The placement matrix). One copy
/// is the point — no position can skip a guard, and none can drift on what a
/// label is (§ Don't do these: never fork a scorer by execution position).
///
/// In order, none of which a caller can reorder:
/// 1. Decode the signed [`AlgorithmLabeler`] from `metadata_blob`.
/// 2. **Re-verify** its signature, `wasm_hash` and `wasm_size` against
///    `wasm_bytes` before instantiating anything (security review B1) — a
///    compromised artifact store cannot swap the module its publisher signed
///    — **and** that its `algorithm_id` is `expected_id`, the labeler the
///    caller resolved from the factor, subscription or grant it is scoring
///    for ([`verify_labeler_metadata_for`]) — a store
///    cannot answer `inspect(A)` with publisher B's self-consistent module.
/// 3. **Clamp** the publisher-declared, self-signed resource limits to the
///    host ceiling ([`clamp_labeler_limits`]).
/// 4. Compile and run in the fuel/memory-bounded, empty-`Linker` sandbox.
/// 5. BARE-decode the module's `Vec<Label>`; an empty output is "detected
///    nothing", not an error.
///
/// What a caller *does* with the labels — the per-mille bus score
/// (`fauna_core::scoring::labels_to_score_entry`), the category plane — is its
/// own orchestration, not the scorer's.
pub fn run_published_labeler(
    metadata_blob: &[u8],
    wasm_bytes: &[u8],
    expected_id: &ActorId,
    input: &LabelerPostInput,
) -> Result<Vec<Label>> {
    run_published_labeler_with_attachments(metadata_blob, wasm_bytes, expected_id, input, &[])
}

/// [`run_published_labeler`] at a position that **holds the item's attachment
/// bytes** — a community room's home nest, inside its members' read. The
/// facet reaches the module only if it declared `needs_attachment_bytes`
/// ([`encode_labeler_input_for`]); a module that did not reads the v1 bytes
/// exactly as before. The facet is the caller's stack value: nothing here
/// keeps it (`conversation-rooms.md` § *Forbidden* — no derived view carries
/// bytes).
pub fn run_published_labeler_with_attachments(
    metadata_blob: &[u8],
    wasm_bytes: &[u8],
    expected_id: &ActorId,
    input: &LabelerPostInput,
    attachments: &[LabelerAttachmentInput],
) -> Result<Vec<Label>> {
    run_published_labeler_bare(
        metadata_blob,
        wasm_bytes,
        expected_id,
        &encode_labeler_input(input)?,
        attachments,
    )
}

/// [`run_published_labeler`] for a caller that already holds the BARE-encoded
/// **post-shaped** input — the FFI holder, whose content-kind mapping produced
/// those bytes on the far side of the UniFFI boundary — plus whatever
/// attachment bytes it holds (none, for the mail holder today). The facet is
/// appended here for a module that declared it, so the holder never has to
/// know the shape: the decision is the metadata's, read at the one boundary
/// that already decodes and verifies it.
pub fn run_published_labeler_bare(
    metadata_blob: &[u8],
    wasm_bytes: &[u8],
    expected_id: &ActorId,
    post_input_bare: &[u8],
    attachments: &[LabelerAttachmentInput],
) -> Result<Vec<Label>> {
    let metadata: AlgorithmLabeler = fauna_core::encoding::canonical_decode(metadata_blob)
        .map_err(|e| anyhow::anyhow!("labeler metadata decode: {e}"))?;
    verify_labeler_metadata_for(&metadata, wasm_bytes, expected_id)
        .map_err(|e| anyhow::anyhow!("labeler verify: {e}"))?;
    // The output stamp, read before the module is compiled and so before its
    // positional BARE output can be decoded (the ladder's "read before the
    // strict decode, or the refusal reads as corruption").
    if metadata.output_schema.label_abi > LABEL_ABI_CURRENT {
        return Err(LabelerAbiNewer {
            declared: metadata.output_schema.label_abi,
            supported: LABEL_ABI_CURRENT,
        }
        .into());
    }
    let (max_memory_bytes, max_cpu_microseconds) = clamp_labeler_limits(
        metadata.resource_limits.max_memory_bytes,
        metadata.resource_limits.max_cpu_microseconds,
    );
    let runtime = LabelerRuntime::new(max_memory_bytes, max_cpu_microseconds)
        .context("labeler runtime init")?;
    let module = runtime
        .load_module(wasm_bytes)
        .context("labeler module compile")?;
    let input_bare = if metadata.input_schema.needs_attachment_bytes {
        let mut bytes = post_input_bare.to_vec();
        bytes.extend(encode_attachment_facet(attachments)?);
        std::borrow::Cow::Owned(bytes)
    } else {
        std::borrow::Cow::Borrowed(post_input_bare)
    };
    runtime
        .execute_labels(&module, &input_bare)
        .context("labeler execute")
}

#[cfg(test)]
mod tests {
    use super::*;
    use core::cell::Cell;

    /// The async runtime the facet-loop tests block on — the loop is `async`
    /// because a position's `fetch` reads a blob store.
    fn block_on<F: core::future::Future>(f: F) -> F::Output {
        tokio::runtime::Builder::new_current_thread()
            .build()
            .expect("runtime")
            .block_on(f)
    }

    /// What a seal adds to its plaintext under either position's key model —
    /// a 12-byte nonce and a 16-byte Poly1305 tag
    /// (`fauna_core::group_content::seal_group_content` for a room message's
    /// generation, `fauna_core::subscription::crypto::encrypt_content` for a
    /// post's per-post key). Comfortably under [`SEALED_FRAMING_ALLOWANCE`],
    /// which is what lets the loop read a floor on the opened length off the
    /// sealed one.
    const SEAL_FRAMING: usize = 12 + 16;

    /// Candidates declaring `sizes`, each with its index in its MIME type so
    /// a test can assert the author's order survived.
    fn candidates_of(sizes: &[u64]) -> Vec<FacetCandidate> {
        sizes
            .iter()
            .enumerate()
            .map(|(i, n)| FacetCandidate {
                mime_type: format!("image/test-{i}"),
                size_bytes: *n,
                // A DISTINCT address per entry: the dedupe must not change
                // what any of these tests measure.
                address: Some(address_of(i as u8)),
            })
            .collect()
    }

    /// A content address that differs only in its first byte.
    fn address_of(tag: u8) -> [u8; 32] {
        let mut a = [0u8; 32];
        a[0] = tag;
        a
    }

    /// `n` candidates all naming ONE address — an author who uploaded once
    /// and named the upload `n` times inside the sealed body.
    fn candidates_naming_one_address(n: usize, size: u64) -> Vec<FacetCandidate> {
        (0..n)
            .map(|i| FacetCandidate {
                mime_type: format!("image/test-{i}"),
                size_bytes: size,
                address: Some(address_of(7)),
            })
            .collect()
    }

    /// One counted pass of the loop over `sizes`, with an honest position
    /// behind it: every blob is stored, every seal opens, and each opens to
    /// exactly its declared size. Returns the facet and the (fetches, opens)
    /// the host paid to build it.
    fn honest_pass(sizes: &[u64]) -> (Vec<LabelerAttachmentInput>, usize, usize) {
        let candidates = candidates_of(sizes);
        let fetches = Cell::new(0usize);
        let opens = Cell::new(0usize);
        let facet = block_on(build_attachment_facet(
            &candidates,
            // No probe: this pass measures the loop's own post-read
            // bounds, the shape every position had before the probe.
            |_| async { None },
            |i| {
                fetches.set(fetches.get() + 1);
                let sealed_len = sizes[i] as usize + SEAL_FRAMING;
                async move { Some(vec![0u8; sealed_len]) }
            },
            |_, sealed| {
                opens.set(opens.get() + 1);
                async move { Some(vec![0u8; sealed.len() - SEAL_FRAMING]) }
            },
        ));
        (facet, fetches.get(), opens.get())
    }

    /// The same honest position, now able to answer how big a stored seal is
    /// before it is read — what a nest holding blob metadata can do
    /// (`db::blobs::get_blob_sizes`). Everything else is `honest_pass`:
    /// same candidates, same bytes, same opens. The only thing that may
    /// differ is what the host PAID, which is the point.
    fn probed_pass(sizes: &[u64]) -> (Vec<LabelerAttachmentInput>, usize, usize) {
        let candidates = candidates_of(sizes);
        let fetches = Cell::new(0usize);
        let opens = Cell::new(0usize);
        let facet = block_on(build_attachment_facet(
            &candidates,
            |i| async move { Some(sizes[i] + SEAL_FRAMING as u64) },
            |i| {
                fetches.set(fetches.get() + 1);
                let sealed_len = sizes[i] as usize + SEAL_FRAMING;
                async move { Some(vec![0u8; sealed_len]) }
            },
            |_, sealed| {
                opens.set(opens.get() + 1);
                async move { Some(vec![0u8; sealed.len() - SEAL_FRAMING]) }
            },
        ));
        (facet, fetches.get(), opens.get())
    }

    /// Asserts one entry rode as withheld metadata — declared fields intact,
    /// no bytes — which is what every bound the loop applies produces.
    fn assert_withheld_at(entry: &LabelerAttachmentInput, i: usize, declared: u64) {
        assert!(
            entry.bytes.is_empty(),
            "entry {i} should have been withheld, got {} bytes",
            entry.bytes.len()
        );
        assert_eq!(
            entry.size_bytes, declared,
            "entry {i} lost its declared size"
        );
        assert_eq!(
            entry.mime_type,
            format!("image/test-{i}"),
            "entry {i} is out of the author's order"
        );
    }

    /// The finding: the
    /// ceilings used to bound what the module was handed while the host
    /// fetched and AEAD-opened every candidate anyway. Three 8 MiB
    /// attachments fill the 24 MiB facet exactly, so the fourth and fifth can
    /// be handed nothing — and must therefore cost nothing.
    #[test]
    fn the_facet_stops_working_once_its_byte_budget_is_spent() {
        let big = LABELER_ATTACHMENT_BYTES_MAX;
        let (facet, fetches, opens) = honest_pass(&[big; 5]);

        assert_eq!(
            opens, 3,
            "the facet admits exactly three 8 MiB attachments, so only three \
             should ever be decrypted"
        );
        assert_eq!(
            fetches, 3,
            "a candidate that cannot be admitted should not even be read from \
             the blob store"
        );

        // The facet itself is unchanged by the bound: every attachment still
        // rides, in order, the last two withheld.
        assert_eq!(facet.len(), 5, "every attachment must ride the facet");
        for (i, entry) in facet.iter().enumerate().take(3) {
            assert_eq!(
                entry.bytes.len() as u64,
                big,
                "entry {i} is inside the budget and should carry its bytes"
            );
        }
        assert_withheld_at(&facet[3], 3, big);
        assert_withheld_at(&facet[4], 4, big);
    }

    /// The residual the bound left: every refusal keys on BYTES HANDED OVER, and an
    /// entry that never opens hands over nothing — so the budget sat at its
    /// full 24 MiB while an author who named one stored-but-unopenable
    /// upload sixty-four times was charged 64 blob reads and 64 failed AEADs.
    /// One address is resolved once.
    #[test]
    fn an_address_that_does_not_open_is_paid_for_once_however_often_it_is_named() {
        let candidates = candidates_naming_one_address(
            LABELER_ATTACHMENT_FACET_MAX_CANDIDATES,
            LABELER_ATTACHMENT_BYTES_MAX,
        );
        let fetches = Cell::new(0usize);
        let opens = Cell::new(0usize);
        let facet = block_on(build_attachment_facet(
            &candidates,
            // No probe: this pass measures the loop's own post-read
            // bounds, the shape every position had before the probe.
            |_| async { None },
            |_| {
                fetches.set(fetches.get() + 1);
                async move { Some(vec![0u8; LABELER_ATTACHMENT_BYTES_MAX as usize]) }
            },
            |_, _sealed| {
                opens.set(opens.get() + 1);
                async move { None } // stored, but it does not open under this item's key
            },
        ));
        assert_eq!(fetches.get(), 1, "one address is read once");
        assert_eq!(opens.get(), 1, "and decrypted once");
        assert_eq!(facet.len(), LABELER_ATTACHMENT_FACET_MAX_CANDIDATES);
        for (i, entry) in facet.iter().enumerate() {
            assert_withheld_at(entry, i, LABELER_ATTACHMENT_BYTES_MAX);
        }
    }

    /// The facet a module reads must be identical to one built by a host that
    /// re-did all the work — so a repeat is served THE SAME BYTES, never
    /// withheld. The facet carries no identity, so a module cannot tell the
    /// difference, which is precisely why withholding would be a change to
    /// what it sees and the same-bytes branch is not.
    #[test]
    fn repeats_of_one_address_are_served_the_same_bytes_never_withheld() {
        let candidates = candidates_naming_one_address(3, 8);
        let fetches = Cell::new(0usize);
        let opens = Cell::new(0usize);
        let facet = block_on(build_attachment_facet(
            &candidates,
            // No probe: this pass measures the loop's own post-read
            // bounds, the shape every position had before the probe.
            |_| async { None },
            |_| {
                fetches.set(fetches.get() + 1);
                async move { Some(vec![0u8; 8 + SEAL_FRAMING]) }
            },
            |_, sealed| {
                opens.set(opens.get() + 1);
                async move { Some(vec![0xABu8; sealed.len() - SEAL_FRAMING]) }
            },
        ));
        assert_eq!((fetches.get(), opens.get()), (1, 1));
        assert_eq!(facet.len(), 3);
        for (i, entry) in facet.iter().enumerate() {
            assert_eq!(
                entry.bytes,
                vec![0xABu8; 8],
                "entry {i} must carry the same bytes a wasteful host would have opened for it"
            );
        }
    }

    /// The budget is charged POSITION BY POSITION, exactly as it would be if
    /// each repeat had been opened: four 8 MiB entries naming one address
    /// fill the 24 MiB facet after three, and the fourth is withheld — the
    /// dedupe saves work, never budget.
    #[test]
    fn a_deduped_repeat_is_still_admitted_position_by_position() {
        let big = LABELER_ATTACHMENT_BYTES_MAX;
        let candidates = candidates_naming_one_address(4, big);
        let facet = block_on(build_attachment_facet(
            &candidates,
            // No probe: this pass measures the loop's own post-read
            // bounds, the shape every position had before the probe.
            |_| async { None },
            |_| async move { Some(vec![0u8; big as usize + SEAL_FRAMING]) },
            |_, sealed| async move { Some(vec![0u8; sealed.len() - SEAL_FRAMING]) },
        ));
        for (i, entry) in facet.iter().enumerate().take(3) {
            assert_eq!(
                entry.bytes.len() as u64,
                big,
                "entry {i} is inside the budget"
            );
        }
        assert_withheld_at(&facet[3], 3, big);
    }

    /// A caller with no address to give loses the dedupe and nothing else —
    /// the work is what it always was, and the facet is unchanged.
    #[test]
    fn a_candidate_without_an_address_is_simply_not_deduped() {
        let candidates: Vec<FacetCandidate> = (0..3)
            .map(|i| FacetCandidate {
                mime_type: format!("image/test-{i}"),
                size_bytes: 8,
                address: None,
            })
            .collect();
        let opens = Cell::new(0usize);
        let facet = block_on(build_attachment_facet(
            &candidates,
            // No probe: this pass measures the loop's own post-read
            // bounds, the shape every position had before the probe.
            |_| async { None },
            |_| async move { Some(vec![0u8; 8 + SEAL_FRAMING]) },
            |_, sealed| {
                opens.set(opens.get() + 1);
                async move { Some(vec![1u8; sealed.len() - SEAL_FRAMING]) }
            },
        ));
        assert_eq!(opens.get(), 3, "no address, no dedupe");
        assert!(facet.iter().all(|e| e.bytes.len() == 8));
    }

    /// The unbounded half of the finding: the list the loop walks is the
    /// item's own sealed body, which no send-time check can bound. Past the
    /// per-item candidate ceiling nothing is fetched or opened, and the
    /// metadata still rides in the author's order.
    #[test]
    fn the_facet_works_on_at_most_the_per_item_candidate_ceiling() {
        // Four-byte attachments, so the byte budget never spends and the
        // candidate ceiling is provably the only thing that stopped the loop.
        let n = LABELER_ATTACHMENT_FACET_MAX_CANDIDATES + 200;
        let (facet, fetches, opens) = honest_pass(&vec![4u64; n]);

        assert_eq!(fetches, LABELER_ATTACHMENT_FACET_MAX_CANDIDATES);
        assert_eq!(opens, LABELER_ATTACHMENT_FACET_MAX_CANDIDATES);
        assert_eq!(facet.len(), n, "every attachment must ride the facet");
        for (i, entry) in facet
            .iter()
            .enumerate()
            .take(LABELER_ATTACHMENT_FACET_MAX_CANDIDATES)
        {
            assert_eq!(entry.bytes.len(), 4, "entry {i} is under the ceiling");
        }
        // `facet.len() == n` is asserted above, so skipping the candidates that
        // were admitted walks exactly the withheld tail.
        for (i, entry) in facet
            .iter()
            .enumerate()
            .skip(LABELER_ATTACHMENT_FACET_MAX_CANDIDATES)
        {
            assert_withheld_at(entry, i, 4);
        }
    }

    /// A seal whose plaintext cannot fit what is left of the facet is never
    /// decrypted — `admit` would refuse the result, and the AEAD is the
    /// expensive half. **With no probe behind it, it is still fetched:** the
    /// author's declared size is not evidence, so reading the seal is the only
    /// way this position can learn its real length. The probe is what removes
    /// that read, and its twin below is the same case measured with one.
    #[test]
    fn a_seal_too_large_for_the_remainder_is_fetched_but_never_decrypted() {
        let big = LABELER_ATTACHMENT_BYTES_MAX;
        // 8 + 8 + (8 - 512) hands over all but 512 bytes of the 24 MiB facet.
        let (facet, fetches, opens) = honest_pass(&[big, big, big - 512, big]);

        assert_eq!(
            opens, 3,
            "the fourth candidate's 8 MiB plaintext cannot fit the 512 bytes \
             left, so it must not be decrypted"
        );
        assert_eq!(
            fetches, 4,
            "it is still read — a declared size is not evidence"
        );
        assert_withheld_at(&facet[3], 3, big);
    }

    /// The same case as above with a probe behind it: the read goes away too.
    /// This is rule (4)'s "never fetched" — the host asks how big the stored
    /// seal is, learns the plaintext cannot fit the remainder, and pays
    /// neither the read nor the AEAD.
    #[test]
    fn a_probe_removes_the_read_a_seal_too_large_for_the_remainder_used_to_cost() {
        let big = LABELER_ATTACHMENT_BYTES_MAX;
        let sizes = [big, big, big - 512, big];
        let (unprobed, unprobed_fetches, unprobed_opens) = honest_pass(&sizes);
        let (facet, fetches, opens) = probed_pass(&sizes);

        assert_eq!(
            (unprobed_fetches, unprobed_opens),
            (4, 3),
            "fixture: without a probe the fourth seal is read to learn its length"
        );
        assert_eq!(
            (fetches, opens),
            (3, 3),
            "with one, it is neither read nor decrypted"
        );
        assert_eq!(
            facet, unprobed,
            "and the module reads the SAME facet either way — only the host's \
             work differs, which is rule (4)'s whole shape"
        );
        assert_withheld_at(&facet[3], 3, big);
    }

    /// The amplification rule (4) exists against, counted: an author names 64
    /// DISTINCT addresses, each declaring a few bytes, each actually holding a
    /// blob at the upload ceiling. Dedupe cannot help (the addresses differ)
    /// and the declared sizes pass the cheap pre-filter, so before the probe
    /// every one of them was read in full. With the probe not one is.
    #[test]
    fn sixty_four_lying_candidates_are_never_read() {
        const BLOB_BODY_LIMIT: u64 = 10 * 1024 * 1024;
        let n = LABELER_ATTACHMENT_FACET_MAX_CANDIDATES;
        // Declared: 4 bytes. Stored: 10 MiB. The declaration is the author's,
        // the stored length is the nest's own measurement.
        let candidates = candidates_of(&vec![4u64; n]);
        let fetches = Cell::new(0usize);
        let opens = Cell::new(0usize);
        let facet = block_on(build_attachment_facet(
            &candidates,
            |_| async { Some(BLOB_BODY_LIMIT) },
            |_| {
                fetches.set(fetches.get() + 1);
                async move { Some(vec![0u8; BLOB_BODY_LIMIT as usize]) }
            },
            |_, sealed| {
                opens.set(opens.get() + 1);
                async move { Some(vec![0u8; sealed.len() - SEAL_FRAMING]) }
            },
        ));

        assert_eq!(
            (fetches.get(), opens.get()),
            (0, 0),
            "a candidate whose stored seal is past the per-attachment ceiling is \
             never fetched and never decrypted"
        );
        assert_eq!(
            facet.len(),
            n,
            "every entry still rides, in the author's order"
        );
        for (i, entry) in facet.iter().enumerate() {
            assert_withheld_at(entry, i, 4);
        }
    }

    /// An absent probe answer must mean "no answer", never "refuse" — a blob
    /// can be stored with no metadata row behind it (the relay and backup
    /// write paths, a fixture that puts bytes directly), and the honest facet
    /// must survive that. The mirror of the empty-refs rule.
    #[test]
    fn a_probe_with_no_answer_falls_through_to_the_read() {
        let candidates = candidates_of(&[4, 4, 4]);
        let fetches = Cell::new(0usize);
        let facet = block_on(build_attachment_facet(
            &candidates,
            // Answers for the middle one only: the outer two have no
            // metadata row, which says nothing about their blobs.
            |i| async move { (i == 1).then_some(4 + SEAL_FRAMING as u64) },
            |_| {
                fetches.set(fetches.get() + 1);
                async move { Some(vec![0u8; 4 + SEAL_FRAMING]) }
            },
            |_, sealed| async move { Some(vec![0u8; sealed.len() - SEAL_FRAMING]) },
        ));

        assert_eq!(
            fetches.get(),
            3,
            "all three are read — absence is not a refusal"
        );
        for (i, entry) in facet.iter().enumerate() {
            assert_eq!(entry.bytes.len(), 4, "entry {i} rode with its bytes");
        }
    }

    /// The spent-budget test is `remaining() == 0`, never a failed `admit`:
    /// a budget with room left refuses an attachment too big for the
    /// remainder and must go on to admit a smaller one after it.
    #[test]
    fn the_loop_keeps_going_past_a_refusal_and_admits_a_later_smaller_attachment() {
        let big = LABELER_ATTACHMENT_BYTES_MAX;
        let (facet, _, opens) = honest_pass(&[big, big, big - 512, big, 256]);

        assert_eq!(
            opens, 4,
            "candidates 0,1,2 and 4 are admissible; only the 8 MiB candidate 3 \
             is not"
        );
        assert_withheld_at(&facet[3], 3, big);
        assert_eq!(
            facet[4].bytes.len(),
            256,
            "256 bytes still fit the 512 left of the facet, so a refusal \
             before it must not have ended the loop"
        );
    }

    /// Why [`AttachmentFacetBudget::remaining`] exists as its own test rather
    /// than `!admit(..)`: a full budget still admits a zero-length plaintext,
    /// so a loop keyed on a failed `admit` would never learn it was full.
    #[test]
    fn a_full_budget_reports_no_remainder_though_it_still_admits_zero_bytes() {
        let mut budget = AttachmentFacetBudget::new();
        for _ in 0..3 {
            assert!(budget.admit(LABELER_ATTACHMENT_BYTES_MAX));
        }
        assert_eq!(budget.handed_over(), LABELER_ATTACHMENT_FACET_MAX_BYTES);
        assert_eq!(budget.remaining(), 0);
        assert!(
            budget.admit(0),
            "a zero-length plaintext admits on a full budget, which is exactly \
             why `remaining()` is the spent-budget test"
        );
    }

    /// An empty item is the ABI-legal empty facet, and costs nothing.
    #[test]
    fn an_item_with_no_attachments_builds_an_empty_facet() {
        let (facet, fetches, opens) = honest_pass(&[]);
        assert!(facet.is_empty());
        assert_eq!((fetches, opens), (0, 0));
    }

    #[test]
    fn runtime_creates_successfully() {
        let rt = LabelerRuntime::new(16 * 1024 * 1024, 100_000);
        assert!(rt.is_ok());
    }

    #[test]
    fn rejects_invalid_wasm_bytes() {
        let rt = LabelerRuntime::new(16 * 1024 * 1024, 100_000).unwrap();
        let result = rt.load_module(b"not valid wasm");
        assert!(result.is_err());
    }

    #[test]
    fn clamp_caps_a_declared_memory_above_the_host_ceiling() {
        let (memory_bytes, cpu_microseconds) =
            clamp_labeler_limits(LABELER_HOST_MAX_MEMORY_BYTES + 1, 1);
        assert_eq!(memory_bytes, LABELER_HOST_MAX_MEMORY_BYTES);
        assert_eq!(
            cpu_microseconds, 1,
            "cpu axis must not be affected by memory clamping"
        );
    }

    #[test]
    fn clamp_caps_a_declared_cpu_budget_above_the_host_ceiling() {
        let (memory_bytes, cpu_microseconds) =
            clamp_labeler_limits(1, LABELER_HOST_MAX_CPU_MICROSECONDS + 1);
        assert_eq!(
            memory_bytes, 1,
            "memory axis must not be affected by cpu clamping"
        );
        assert_eq!(cpu_microseconds, LABELER_HOST_MAX_CPU_MICROSECONDS);
    }

    #[test]
    fn clamp_passes_through_declared_limits_below_the_host_ceiling_unchanged() {
        let (memory_bytes, cpu_microseconds) = clamp_labeler_limits(
            LABELER_HOST_MAX_MEMORY_BYTES - 1,
            LABELER_HOST_MAX_CPU_MICROSECONDS - 1,
        );
        assert_eq!(memory_bytes, LABELER_HOST_MAX_MEMORY_BYTES - 1);
        assert_eq!(cpu_microseconds, LABELER_HOST_MAX_CPU_MICROSECONDS - 1);
    }
}
