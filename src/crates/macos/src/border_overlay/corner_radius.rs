//! The target window's real corner radius (spec decision 7 / OQ-4).
//!
//! # Inventory (private API)
//!
//! - Symbols, all exported by the `SkyLight` image and declared in no public
//!   header: `SLSMainConnectionID`, `SLSWindowQueryWindows`,
//!   `SLSWindowQueryResultCopyWindows`, `SLSWindowIteratorAdvance`,
//!   `SLSWindowIteratorGetWindowID`, `SLSWindowIteratorGetCornerRadii`.
//! - Missing public equivalent: no public API reports another application's
//!   window corner radius, and on macOS 26 it differs per application.
//! - Minimum need: the border's inner edge follows the target's corner.
//!
//! The functions are never linked: they are looked up with `dlopen`/`dlsym`
//! (the policy of `private_skylight.rs` and `ax/window_id.rs`). Any missing
//! symbol makes the whole capability unavailable: every query answers `None`
//! and the per-OS fallback radius is used, with the one `FallbackOnly`
//! capability diagnostic.
//!
//! # Signatures: assumptions (unverified until a native check)
//!
//! A symbol lookup reveals no types. The shapes below are the commonly
//! described C shapes of this window-query protocol, written independently of
//! any border tool or yabai header:
//!
//! - `int SLSMainConnectionID(void)`;
//! - `CFTypeRef SLSWindowQueryWindows(int connection, CFArrayRef window_ids,
//!   uint32_t options)`: a query over the windows whose ids (`CFNumber`s) are
//!   in the array; options `0`. Returned at +1 (released here);
//! - `CFTypeRef SLSWindowQueryResultCopyWindows(CFTypeRef query)`: the
//!   iterator over the query's result. Returned at +1 (released here);
//! - `bool SLSWindowIteratorAdvance(CFTypeRef iterator)`: moves to the next
//!   window; `false` when there is none;
//! - `uint32_t SLSWindowIteratorGetWindowID(CFTypeRef iterator)`;
//! - `CFArrayRef SLSWindowIteratorGetCornerRadii(CFTypeRef iterator)`: the
//!   current window's corner radii as `CFNumber`s, under the Get rule (not
//!   owned, not released).
//!
//! Defensive checks around the guesses: both returned objects must be
//! non-null, the iterator's window id must equal the queried id, the radii
//! object must be a `CFArray` (checked with `CFGetTypeID`) whose first element
//! is a `CFNumber`, and the value (read as a double) must be finite and within
//! zero to [`MAX_SANE_RADIUS`] inclusive. Anything else is `None`, so the fallback is used.
//! Set `STACHE_BORDER_RADIUS_DEBUG=1` to print each raw value to standard
//! error so a native run can confirm the reading.
//!
//! # Threading and caching
//!
//! The overlay owner calls [`CornerRadiusResolver::corner_radius`] on the main
//! thread on `Show` and `Order`. A sane answer is cached per window id (a
//! failed query is not cached, so it is retried). `Order` calls
//! [`CornerRadiusResolver::forget`] first, so it always re-queries: a radius
//! can change on fullscreen or zoom, and one query is a few window-server
//! round trips. `Release` forgets the target too. The cache is bounded.

use std::collections::HashMap;
use std::ffi::{CStr, c_int, c_void};
use std::sync::{Mutex, OnceLock, PoisonError};

use super::RadiusCapability;
use super::private_skylight::system_lookup;

/// The largest radius accepted as sane, in points.
pub const MAX_SANE_RADIUS: f64 = 64.0;
/// The cache is cleared when it holds this many windows.
pub const RADIUS_CACHE_LIMIT: usize = 512;

/// Answers a target window's corner radius.
///
/// Implementations are read on the main thread. A missing capability returns
/// `None`, which is a capability fact, never an error.
pub trait CornerRadiusResolver: Send + Sync {
    /// The queried radius in points, or `None` when it cannot be queried.
    fn corner_radius(&self, target_cg_id: u32) -> Option<f64>;
    /// Drops any cached radius of `target_cg_id` (the next query re-reads it).
    fn forget(&self, _target_cg_id: u32) {}
}

/// `Some(raw)` when `raw` is a plausible corner radius, else `None`.
#[must_use]
pub fn sane_radius(raw: f64) -> Option<f64> {
    (raw.is_finite() && (0.0..=MAX_SANE_RADIUS).contains(&raw)).then_some(raw)
}

const SYM_MAIN_CONNECTION: &CStr = c"SLSMainConnectionID";
const SYM_QUERY_WINDOWS: &CStr = c"SLSWindowQueryWindows";
const SYM_COPY_WINDOWS: &CStr = c"SLSWindowQueryResultCopyWindows";
const SYM_ADVANCE: &CStr = c"SLSWindowIteratorAdvance";
const SYM_WINDOW_ID: &CStr = c"SLSWindowIteratorGetWindowID";
const SYM_CORNER_RADII: &CStr = c"SLSWindowIteratorGetCornerRadii";

/// Every symbol the query needs, in lookup order.
pub const RADIUS_SYMBOLS: [&CStr; 6] = [
    SYM_MAIN_CONNECTION,
    SYM_QUERY_WINDOWS,
    SYM_COPY_WINDOWS,
    SYM_ADVANCE,
    SYM_WINDOW_ID,
    SYM_CORNER_RADII,
];

type CFTypeRef = *const c_void;
type MainConnectionFn = unsafe extern "C" fn() -> c_int;
type QueryWindowsFn = unsafe extern "C" fn(c_int, CFTypeRef, u32) -> CFTypeRef;
type CopyWindowsFn = unsafe extern "C" fn(CFTypeRef) -> CFTypeRef;
type AdvanceFn = unsafe extern "C" fn(CFTypeRef) -> bool;
type WindowIdFn = unsafe extern "C" fn(CFTypeRef) -> u32;
type CornerRadiiFn = unsafe extern "C" fn(CFTypeRef) -> CFTypeRef;

const K_CF_NUMBER_SINT32_TYPE: isize = 3;
const K_CF_NUMBER_DOUBLE_TYPE: isize = 13;

#[link(name = "CoreFoundation", kind = "framework")]
unsafe extern "C" {
    static kCFTypeArrayCallBacks: c_void;
    fn CFArrayCreate(
        allocator: CFTypeRef,
        values: *const CFTypeRef,
        count: isize,
        callbacks: *const c_void,
    ) -> CFTypeRef;
    fn CFArrayGetCount(array: CFTypeRef) -> isize;
    fn CFArrayGetValueAtIndex(array: CFTypeRef, index: isize) -> CFTypeRef;
    fn CFArrayGetTypeID() -> usize;
    fn CFNumberCreate(allocator: CFTypeRef, kind: isize, value: *const c_void) -> CFTypeRef;
    fn CFNumberGetValue(number: CFTypeRef, kind: isize, value: *mut c_void) -> bool;
    fn CFNumberGetTypeID() -> usize;
    fn CFGetTypeID(object: CFTypeRef) -> usize;
    fn CFRelease(object: CFTypeRef);
}

/// The resolved private functions (all present).
#[derive(Clone, Copy)]
struct RadiusSymbols {
    main_connection: MainConnectionFn,
    query_windows: QueryWindowsFn,
    copy_windows: CopyWindowsFn,
    advance: AdvanceFn,
    window_id: WindowIdFn,
    corner_radii: CornerRadiiFn,
}

/// Releases a +1 Core Foundation object on drop.
struct Owned(CFTypeRef);

impl Drop for Owned {
    fn drop(&mut self) {
        if !self.0.is_null() {
            // SAFETY: the object was returned at +1 to this owner.
            unsafe { CFRelease(self.0) };
        }
    }
}

fn debug_enabled() -> bool {
    static ENABLED: OnceLock<bool> = OnceLock::new();
    *ENABLED.get_or_init(|| std::env::var_os("STACHE_BORDER_RADIUS_DEBUG").is_some())
}

impl RadiusSymbols {
    /// Runs the window query for one id; the raw first radius when every
    /// defensive check passes.
    fn raw_radius(self, target_cg_id: u32) -> Option<f64> {
        let id = i32::try_from(target_cg_id).ok()?;
        // SAFETY (whole block): public Core Foundation calls with valid
        // arguments, and the private calls with the shapes documented at the
        // module top (unverified; see there). Every +1 object is released by
        // `Owned`; the radii array is borrowed from the live iterator.
        unsafe {
            let number = Owned(CFNumberCreate(
                std::ptr::null(),
                K_CF_NUMBER_SINT32_TYPE,
                std::ptr::from_ref(&id).cast(),
            ));
            if number.0.is_null() {
                return None;
            }
            let ids = Owned(CFArrayCreate(
                std::ptr::null(),
                &raw const number.0,
                1,
                &raw const kCFTypeArrayCallBacks,
            ));
            if ids.0.is_null() {
                return None;
            }
            let connection = (self.main_connection)();
            let query = Owned((self.query_windows)(connection, ids.0, 0));
            if query.0.is_null() {
                return None;
            }
            let iterator = Owned((self.copy_windows)(query.0));
            if iterator.0.is_null() || !(self.advance)(iterator.0) {
                return None;
            }
            if (self.window_id)(iterator.0) != target_cg_id {
                return None;
            }
            let radii = (self.corner_radii)(iterator.0);
            if radii.is_null()
                || CFGetTypeID(radii) != CFArrayGetTypeID()
                || CFArrayGetCount(radii) < 1
            {
                return None;
            }
            let first = CFArrayGetValueAtIndex(radii, 0);
            if first.is_null() || CFGetTypeID(first) != CFNumberGetTypeID() {
                return None;
            }
            let mut value = f64::NAN;
            CFNumberGetValue(first, K_CF_NUMBER_DOUBLE_TYPE, (&raw mut value).cast());
            Some(value)
        }
    }
}

/// The raw per-window query: the unvalidated radius, or `None`.
pub type RawRadiusQuery = Box<dyn Fn(u32) -> Option<f64> + Send + Sync>;

/// The private window-query resolver, with a per-window cache.
pub struct SystemCornerRadiusResolver {
    query: Option<RawRadiusQuery>,
    missing_symbol: Option<&'static CStr>,
    cache: Mutex<HashMap<u32, f64>>,
}

impl std::fmt::Debug for SystemCornerRadiusResolver {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SystemCornerRadiusResolver")
            .field("available", &self.query.is_some())
            .field("missing_symbol", &self.missing_symbol)
            .finish_non_exhaustive()
    }
}

impl SystemCornerRadiusResolver {
    /// Looks the symbols up on the `SkyLight` image. Creates no connection and
    /// calls nothing.
    #[must_use]
    pub fn system() -> Self {
        // SAFETY: `system_lookup` returns only the `dlsym` address of the
        // exact named symbol, or null.
        unsafe { Self::from_lookup(system_lookup) }
    }

    /// Resolves through an injected lookup.
    ///
    /// # Safety
    ///
    /// A non-null address returned for a name must be the address of a
    /// function with the signature documented for that name at the module top,
    /// because [`CornerRadiusResolver::corner_radius`] calls it. Tests that
    /// return a dummy address must never query.
    pub unsafe fn from_lookup(lookup: impl Fn(&CStr) -> *mut c_void) -> Self {
        let mut addresses = [std::ptr::null_mut(); 6];
        for (slot, name) in addresses.iter_mut().zip(RADIUS_SYMBOLS) {
            *slot = lookup(name);
            if slot.is_null() {
                return Self::unavailable(name);
            }
        }
        let [main, query, copy, advance, window_id, radii] = addresses;
        // SAFETY: every address is non-null and, by the function contract, a
        // function of the documented shape; data and function pointers have
        // the same size on every supported target.
        let symbols = unsafe {
            RadiusSymbols {
                main_connection: std::mem::transmute::<*mut c_void, MainConnectionFn>(main),
                query_windows: std::mem::transmute::<*mut c_void, QueryWindowsFn>(query),
                copy_windows: std::mem::transmute::<*mut c_void, CopyWindowsFn>(copy),
                advance: std::mem::transmute::<*mut c_void, AdvanceFn>(advance),
                window_id: std::mem::transmute::<*mut c_void, WindowIdFn>(window_id),
                corner_radii: std::mem::transmute::<*mut c_void, CornerRadiiFn>(radii),
            }
        };
        Self::with_query(move |id| symbols.raw_radius(id))
    }

    /// A resolver over an injected raw query (tests).
    #[must_use]
    pub fn with_query(query: impl Fn(u32) -> Option<f64> + Send + Sync + 'static) -> Self {
        Self {
            query: Some(Box::new(query)),
            missing_symbol: None,
            cache: Mutex::new(HashMap::new()),
        }
    }

    /// A resolver whose `symbol` is missing: every query answers `None`.
    #[must_use]
    pub fn unavailable(symbol: &'static CStr) -> Self {
        Self {
            query: None,
            missing_symbol: Some(symbol),
            cache: Mutex::new(HashMap::new()),
        }
    }

    /// Whether every private symbol exists on this system.
    #[must_use]
    pub const fn symbol_present(&self) -> bool { self.query.is_some() }

    /// The first missing symbol, when the capability is unavailable.
    #[must_use]
    pub const fn missing_symbol(&self) -> Option<&'static CStr> { self.missing_symbol }

    /// The capability this resolver provides.
    #[must_use]
    pub const fn capability(&self) -> RadiusCapability {
        if self.query.is_some() {
            RadiusCapability::Available
        } else {
            RadiusCapability::FallbackOnly
        }
    }

    /// How many windows are cached.
    #[must_use]
    pub fn cached(&self) -> usize { self.lock().len() }

    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<u32, f64>> {
        self.cache.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

impl CornerRadiusResolver for SystemCornerRadiusResolver {
    fn corner_radius(&self, target_cg_id: u32) -> Option<f64> {
        let query = self.query.as_ref()?;
        if let Some(radius) = self.lock().get(&target_cg_id) {
            return Some(*radius);
        }
        let raw = query(target_cg_id);
        if debug_enabled() {
            #[allow(clippy::print_stderr)]
            {
                eprintln!("stache borders: window {target_cg_id} raw corner radius {raw:?}");
            }
        }
        let radius = sane_radius(raw?)?;
        {
            let mut cache = self.lock();
            if cache.len() >= RADIUS_CACHE_LIMIT {
                cache.clear();
            }
            cache.insert(target_cg_id, radius);
        }
        Some(radius)
    }

    fn forget(&self, target_cg_id: u32) { self.lock().remove(&target_cg_id); }
}

type CapabilityObserver = Box<dyn Fn(RadiusCapability) + Send + Sync>;

/// A [`CornerRadiusResolver`] that resolves its inner resolver on first use.
///
/// Constructing it (host start, borders disabled) does nothing native: the
/// `dlopen`/`dlsym` lookup of [`SystemCornerRadiusResolver::system`] runs only
/// when an overlay first asks for a radius. The optional observer learns the
/// resulting capability once, at that moment.
pub struct LazyCornerRadiusResolver {
    resolve: Box<dyn Fn() -> SystemCornerRadiusResolver + Send + Sync>,
    resolved: OnceLock<SystemCornerRadiusResolver>,
    observer: Option<CapabilityObserver>,
}

impl LazyCornerRadiusResolver {
    /// The system resolver, looked up on first use.
    #[must_use]
    pub fn system() -> Self { Self::with(SystemCornerRadiusResolver::system) }

    /// Resolves through `resolve` on first use (tests count the calls).
    #[must_use]
    pub fn with(resolve: impl Fn() -> SystemCornerRadiusResolver + Send + Sync + 'static) -> Self {
        Self {
            resolve: Box::new(resolve),
            resolved: OnceLock::new(),
            observer: None,
        }
    }

    /// Calls `observer` with the capability once the inner resolver resolves.
    #[must_use]
    pub fn observed(mut self, observer: impl Fn(RadiusCapability) + Send + Sync + 'static) -> Self {
        self.observer = Some(Box::new(observer));
        self
    }

    /// Whether the inner resolver was resolved yet.
    #[must_use]
    pub fn is_resolved(&self) -> bool { self.resolved.get().is_some() }

    fn inner(&self) -> &SystemCornerRadiusResolver {
        self.resolved.get_or_init(|| {
            let resolver = (self.resolve)();
            if let Some(observer) = &self.observer {
                observer(resolver.capability());
            }
            resolver
        })
    }
}

impl CornerRadiusResolver for LazyCornerRadiusResolver {
    fn corner_radius(&self, target_cg_id: u32) -> Option<f64> {
        self.inner().corner_radius(target_cg_id)
    }

    fn forget(&self, target_cg_id: u32) {
        if let Some(resolver) = self.resolved.get() {
            resolver.forget(target_cg_id);
        }
    }
}
