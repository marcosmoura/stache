//! Window-level AX capabilities: typed geometry, flags, strings, membership,
//! settable checks, `CGWindowID` and tab extraction reads.
//!
//! [`WindowAx`] extends [`AxApi`] additively, so `MenuAnywhere`'s trait, fake and
//! reader are untouched. Like the base trait it is used on one AX owner thread
//! only; its elements are `!Send`.
//!
//! # Unknown is explicit
//!
//! A missing, unsupported or malformed attribute is never converted to a zero
//! frame, `false` flag or empty string. Raw reads return `Result<_, AxError>`;
//! [`AxReading`] classifies a result as a value, an explicitly unsupported
//! attribute or an unknown (transient or invalid) failure, and [`read_frame`]
//! only yields an [`AxFrame`] when both position and size were read.
//!
//! # Timeouts
//!
//! [`read_frame`], [`resizable`] and [`write_frame`] set the messaging timeout
//! on the exact element object first, reusing [`ELEMENT_TIMEOUT`] (0.5 s,
//! queries) and [`PRESS_TIMEOUT`] (1 s, setting and actions). Timeouts are per
//! element object (`AXUIElement.h:387-402`).

use core_foundation::array::CFArray;
use core_foundation::base::{CFType, CFTypeRef, TCFType};
use core_foundation::boolean::CFBoolean;
use core_foundation::number::CFNumber;
use core_foundation::string::CFString;

use super::api::{
    AXUIElementCopyAttributeValues, AXUIElementGetAttributeValueCount, AxApi, AxElement, AxError,
    StringAttribute, SystemAx,
};
use super::ffi::{AXUIElementIsAttributeSettable, AXUIElementSetAttributeValue};
use super::window_id::{WindowIdCapability, WindowIdError};
use super::{ELEMENT_TIMEOUT, PRESS_TIMEOUT, value};

/// The `AXRole` of a tab group container.
pub const TAB_GROUP_ROLE: &str = "AXTabGroup";

/// The most children scanned when looking for a tab group.
pub const TAB_GROUP_SCAN_LIMIT: usize = 64;

/// A point in the AX global space (origin top-left of the primary display,
/// y down), in points.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct AxPoint {
    /// Horizontal coordinate.
    pub x: f64,
    /// Vertical coordinate.
    pub y: f64,
}

/// A size in points.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct AxSize {
    /// Width.
    pub width: f64,
    /// Height.
    pub height: f64,
}

/// A complete window frame: both `AXPosition` and `AXSize` were read.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct AxFrame {
    /// `AXPosition`.
    pub origin: AxPoint,
    /// `AXSize`.
    pub size: AxSize,
}

/// A boolean AX attribute.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum BoolAttribute {
    /// `AXMinimized` (window).
    Minimized,
    /// `AXFullScreen` (window).
    FullScreen,
    /// `AXHidden` (application element).
    Hidden,
    /// `AXMain` (window).
    Main,
}

/// A string AX attribute of a window.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum WindowString {
    /// `AXTitle`.
    Title,
    /// `AXRole`.
    Role,
    /// `AXSubrole`.
    Subrole,
}

/// An attribute whose settability can be checked.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GeometryAttribute {
    /// `AXPosition`.
    Position,
    /// `AXSize`; settable means resizable.
    Size,
}

/// One attribute read, classified.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AxReading<T> {
    /// The attribute was read.
    Value(T),
    /// The element lacks the attribute (or has no value): explicit unsupported.
    Unsupported,
    /// The read failed or returned a malformed value: explicit unknown.
    Unknown(AxError),
}

impl<T> AxReading<T> {
    /// Classifies a raw result. Absent attributes are [`AxReading::Unsupported`];
    /// every other failure (timeout, invalid element, wrong type, ...) is
    /// [`AxReading::Unknown`].
    #[must_use]
    pub fn classify(result: Result<T, AxError>) -> Self {
        match result {
            Ok(value) => Self::Value(value),
            Err(error) if error.is_absent() => Self::Unsupported,
            Err(error) => Self::Unknown(error),
        }
    }

    /// The value, when read.
    #[must_use]
    pub fn value(self) -> Option<T> {
        match self {
            Self::Value(value) => Some(value),
            Self::Unsupported | Self::Unknown(_) => None,
        }
    }
}

/// The result of reading position and size separately.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct FrameRead {
    /// `AXPosition`.
    pub position: AxReading<AxPoint>,
    /// `AXSize`.
    pub size: AxReading<AxSize>,
}

impl FrameRead {
    /// The frame, only when both halves were read. Never a zero frame.
    #[must_use]
    pub const fn complete(&self) -> Option<AxFrame> {
        match (self.position, self.size) {
            (AxReading::Value(origin), AxReading::Value(size)) => Some(AxFrame { origin, size }),
            _ => None,
        }
    }
}

/// What happened to one requested half of a frame write.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WriteStatus {
    /// The half was not requested.
    NotRequested,
    /// `AXUIElementSetAttributeValue` returned success. Acceptance is not proof
    /// the window reached the requested geometry; read it back.
    Written,
    /// The call failed. [`AxError::CannotComplete`] may follow a write that
    /// already took effect, so a caller treats it as uncertain.
    Failed(AxError),
}

/// The result of [`write_frame`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FrameWriteOutcome {
    /// The messaging timeout could not be set, so nothing was sent.
    NotSent(AxError),
    /// Position and size were attempted separately, position first.
    Attempted {
        /// The position half.
        position: WriteStatus,
        /// The size half.
        size: WriteStatus,
    },
}

impl FrameWriteOutcome {
    /// Whether one requested half was written and the other failed.
    #[must_use]
    pub const fn is_partial(&self) -> bool {
        matches!(
            self,
            Self::Attempted { position, size }
                if matches!(
                    (position, size),
                    (WriteStatus::Written, WriteStatus::Failed(_))
                        | (WriteStatus::Failed(_), WriteStatus::Written)
                )
        )
    }

    /// Whether the call was sent and no requested half failed.
    #[must_use]
    pub const fn is_complete(&self) -> bool {
        matches!(
            self,
            Self::Attempted { position, size }
                if !matches!(position, WriteStatus::Failed(_))
                    && !matches!(size, WriteStatus::Failed(_))
        )
    }
}

/// Window-level operations on one AX owner thread.
pub trait WindowAx: AxApi {
    /// Copies `AXPosition`.
    ///
    /// # Errors
    ///
    /// The AX failure, or [`AxError::WrongType`] for a malformed value.
    fn position(&self, element: &Self::Element) -> Result<AxPoint, AxError>;

    /// Copies `AXSize`.
    ///
    /// # Errors
    ///
    /// The AX failure, or [`AxError::WrongType`] for a malformed value.
    fn size(&self, element: &Self::Element) -> Result<AxSize, AxError>;

    /// Sets `AXPosition` only.
    ///
    /// # Errors
    ///
    /// The AX failure. [`AxError::CannotComplete`] may follow an applied write.
    fn set_position(&self, element: &Self::Element, position: AxPoint) -> Result<(), AxError>;

    /// Sets `AXSize` only.
    ///
    /// # Errors
    ///
    /// The AX failure. [`AxError::CannotComplete`] may follow an applied write.
    fn set_size(&self, element: &Self::Element, size: AxSize) -> Result<(), AxError>;

    /// `AXUIElementIsAttributeSettable` for a geometry attribute.
    ///
    /// # Errors
    ///
    /// The AX failure.
    fn is_settable(
        &self,
        element: &Self::Element,
        attribute: GeometryAttribute,
    ) -> Result<bool, AxError>;

    /// Copies a boolean attribute.
    ///
    /// # Errors
    ///
    /// The AX failure or [`AxError::WrongType`].
    fn bool_attribute(
        &self,
        element: &Self::Element,
        attribute: BoolAttribute,
    ) -> Result<bool, AxError>;

    /// Copies a string attribute of a window.
    ///
    /// # Errors
    ///
    /// The AX failure or [`AxError::WrongType`].
    fn window_string(
        &self,
        element: &Self::Element,
        attribute: WindowString,
    ) -> Result<String, AxError>;

    /// The `AXWindows` count, without copying the array.
    ///
    /// # Errors
    ///
    /// The AX failure.
    fn window_count(&self, application: &Self::Element) -> Result<usize, AxError>;

    /// Copies at most `limit` `AXWindows` starting at `index`, type-checked.
    ///
    /// # Errors
    ///
    /// The AX failure or [`AxError::WrongType`].
    fn windows(
        &self,
        application: &Self::Element,
        index: usize,
        limit: usize,
    ) -> Result<Vec<Self::Element>, AxError>;

    /// Copies `AXFocusedWindow`.
    ///
    /// # Errors
    ///
    /// The AX failure or [`AxError::WrongType`].
    fn focused_window(&self, application: &Self::Element) -> Result<Self::Element, AxError>;

    /// Copies `AXMainWindow`.
    ///
    /// # Errors
    ///
    /// The AX failure or [`AxError::WrongType`].
    fn main_window(&self, application: &Self::Element) -> Result<Self::Element, AxError>;

    /// Whether `_AXUIElementGetWindow` resolved on this system.
    fn window_id_capability(&self) -> WindowIdCapability;

    /// The `CGWindowID` of a window element: query/diagnostic metadata only.
    ///
    /// # Errors
    ///
    /// [`WindowIdError::Unsupported`] when the private symbol is missing.
    fn window_id(&self, element: &Self::Element) -> Result<u32, WindowIdError>;

    /// Copies at most `limit` elements of `AXTabs` from a tab group.
    ///
    /// # Errors
    ///
    /// The AX failure or [`AxError::WrongType`].
    fn tab_elements(
        &self,
        group: &Self::Element,
        limit: usize,
    ) -> Result<Vec<Self::Element>, AxError>;

    /// Whether a tab element is the selected tab: its `AXValue`, a number or
    /// boolean that is non-zero for the selected tab.
    ///
    /// # Errors
    ///
    /// The AX failure or [`AxError::WrongType`].
    fn tab_selected(&self, tab: &Self::Element) -> Result<bool, AxError>;

    /// Performs `AXRaise` on a window element. The caller sets
    /// [`PRESS_TIMEOUT`] on the exact element first. Raising does not activate
    /// the application: callers combine it with the applications adapter's
    /// `activate_exact`.
    ///
    /// # Errors
    ///
    /// The AX failure ([`AxError::ActionUnsupported`] when the window cannot be
    /// raised, [`AxError::CannotComplete`] when the call may have run).
    fn raise(&self, element: &Self::Element) -> Result<(), AxError>;

    /// Sets `AXHidden` on an **application** element: the fallback when the
    /// public `NSRunningApplication` hide or unhide returned `false`. The
    /// caller sets [`PRESS_TIMEOUT`] on the exact element first.
    ///
    /// # Errors
    ///
    /// The AX failure. [`AxError::CannotComplete`] may follow an applied write.
    fn set_hidden(&self, application: &Self::Element, hidden: bool) -> Result<(), AxError>;
}

/// Reads position and size separately after setting [`ELEMENT_TIMEOUT`].
///
/// A failure to set the timeout leaves both halves unknown.
pub fn read_frame<A: WindowAx>(api: &A, element: &A::Element) -> FrameRead {
    if let Err(error) = api.set_messaging_timeout(element, ELEMENT_TIMEOUT) {
        return FrameRead {
            position: AxReading::Unknown(error),
            size: AxReading::Unknown(error),
        };
    }
    FrameRead {
        position: AxReading::classify(api.position(element)),
        size: AxReading::classify(api.size(element)),
    }
}

/// Whether the window is resizable: `AXSize` is settable.
///
/// Unsupported or failing checks are explicit, never `false`.
pub fn resizable<A: WindowAx>(api: &A, element: &A::Element) -> AxReading<bool> {
    if let Err(error) = api.set_messaging_timeout(element, ELEMENT_TIMEOUT) {
        return AxReading::Unknown(error);
    }
    AxReading::classify(api.is_settable(element, GeometryAttribute::Size))
}

/// Writes position and size as two separate calls, position first, after
/// setting [`PRESS_TIMEOUT`] on the exact element object.
///
/// AX has no atomic frame write. The outcome reports each half, so a partial
/// write (one half applied, the other failed) is explicit and the caller can
/// read the actual geometry back. Nothing is retried.
pub fn write_frame<A: WindowAx>(
    api: &A,
    element: &A::Element,
    position: Option<AxPoint>,
    size: Option<AxSize>,
) -> FrameWriteOutcome {
    if let Err(error) = api.set_messaging_timeout(element, PRESS_TIMEOUT) {
        return FrameWriteOutcome::NotSent(error);
    }
    write_halves(api, element, position, size)
}

/// [`write_frame`] with a commit step that runs after the timeout is set and
/// immediately before the first native write.
///
/// `commit` returns whether the send may proceed. When it declines nothing was
/// sent and `None` is returned. A timeout failure is
/// [`FrameWriteOutcome::NotSent`] and `commit` is not called.
pub fn write_frame_gated<A: WindowAx>(
    api: &A,
    element: &A::Element,
    position: Option<AxPoint>,
    size: Option<AxSize>,
    commit: impl FnOnce() -> bool,
) -> Option<FrameWriteOutcome> {
    if let Err(error) = api.set_messaging_timeout(element, PRESS_TIMEOUT) {
        return Some(FrameWriteOutcome::NotSent(error));
    }
    if !commit() {
        return None;
    }
    Some(write_halves(api, element, position, size))
}

fn write_halves<A: WindowAx>(
    api: &A,
    element: &A::Element,
    position: Option<AxPoint>,
    size: Option<AxSize>,
) -> FrameWriteOutcome {
    let position = position.map_or(WriteStatus::NotRequested, |position| {
        match api.set_position(element, position) {
            Ok(()) => WriteStatus::Written,
            Err(error) => WriteStatus::Failed(error),
        }
    });
    let size = size.map_or(WriteStatus::NotRequested, |size| {
        match api.set_size(element, size) {
            Ok(()) => WriteStatus::Written,
            Err(error) => WriteStatus::Failed(error),
        }
    });
    FrameWriteOutcome::Attempted { position, size }
}

/// The result of a tab extraction read.
///
/// This is read-only proof that the attributes can be read with correct CF type
/// checks and retain/release. It never claims tabs are supported: part 08 owns
/// the runtime tab policy.
#[derive(Debug)]
pub enum TabExtraction<E> {
    /// The window has no `AXTabGroup` child.
    NoTabGroup,
    /// The tab group's `AXTabs` elements (possibly empty).
    Tabs(Vec<E>),
    /// A read failed or returned a malformed value: unknown, not "no tabs".
    Unknown(AxError),
}

/// Finds an `AXTabGroup` among the window's children and reads its `AXTabs`.
pub fn extract_tabs<A: WindowAx>(
    api: &A,
    window: &A::Element,
    limit: usize,
) -> TabExtraction<A::Element> {
    let children = match api.children(window, TAB_GROUP_SCAN_LIMIT) {
        Ok(children) => children,
        Err(error) if error.is_absent() => return TabExtraction::NoTabGroup,
        Err(error) => return TabExtraction::Unknown(error),
    };
    for child in &children {
        if api
            .string(child, StringAttribute::Role)
            .is_ok_and(|role| role == TAB_GROUP_ROLE)
        {
            return match api.tab_elements(child, limit) {
                Ok(tabs) => TabExtraction::Tabs(tabs),
                Err(error) if error.is_absent() => TabExtraction::Tabs(Vec::new()),
                Err(error) => TabExtraction::Unknown(error),
            };
        }
    }
    TabExtraction::NoTabGroup
}

/// One member of a tab group, read with explicit unknowns.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TabMemberRead {
    /// The member's `CGWindowID`, metadata only; `None` when the private
    /// capability is missing or the read failed.
    pub cg_window_id: Option<u32>,
    /// Whether it is the selected tab.
    pub selected: AxReading<bool>,
}

/// The result of reading a window's tab group with its members.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TabGroupRead {
    /// The window has no `AXTabGroup` child.
    NoTabGroup,
    /// The members in `AXTabs` order (possibly empty).
    Group(Vec<TabMemberRead>),
    /// A read failed or returned a malformed value: unknown, not "no tabs".
    Unknown(AxError),
}

/// Reads the tab group of `window` with each member's `CGWindowID` and
/// selected flag. At most `limit` members are read; every read is bounded by
/// the messaging timeout the caller set on the window.
pub fn read_tab_group<A: WindowAx>(api: &A, window: &A::Element, limit: usize) -> TabGroupRead {
    match extract_tabs(api, window, limit) {
        TabExtraction::NoTabGroup => TabGroupRead::NoTabGroup,
        TabExtraction::Unknown(error) => TabGroupRead::Unknown(error),
        TabExtraction::Tabs(tabs) => TabGroupRead::Group(
            tabs.iter()
                .map(|tab| {
                    let _ = api.set_messaging_timeout(tab, ELEMENT_TIMEOUT);
                    TabMemberRead {
                        cg_window_id: api.window_id(tab).ok(),
                        selected: AxReading::classify(api.tab_selected(tab)),
                    }
                })
                .collect(),
        ),
    }
}

/// The deepest level (window children are level 1) searched for a tab group.
pub const TAB_BAR_MAX_DEPTH: usize = 7;

/// The most elements whose role is read while searching for a tab group.
pub const TAB_BAR_NODE_BUDGET: usize = 96;

/// The result of searching a window's subtree for its `AXTabGroup`.
#[derive(Debug)]
pub enum TabGroupSearch<E> {
    /// The tab group element (a retained, owner-confined handle).
    Found(E),
    /// The bounded search finished without a tab group.
    Absent,
    /// The window's own children could not be read: unknown, not "no tab
    /// group". This is what a window in transition answers.
    Unknown(AxError),
}

/// Searches the subtree of `window` breadth first for an `AXTabGroup`.
///
/// Bounded by [`TAB_BAR_MAX_DEPTH`] levels, [`TAB_GROUP_SCAN_LIMIT`] children per
/// element and [`TAB_BAR_NODE_BUDGET`] role reads; every element read is bounded
/// by [`ELEMENT_TIMEOUT`]. A failing read below the window only prunes that
/// branch; a failing read of the window's own children is
/// [`TabGroupSearch::Unknown`].
pub fn find_tab_group<A: WindowAx>(api: &A, window: &A::Element) -> TabGroupSearch<A::Element> {
    let mut frontier = match api.children(window, TAB_GROUP_SCAN_LIMIT) {
        Ok(children) => children,
        Err(error) if error.is_absent() => return TabGroupSearch::Absent,
        Err(error) => return TabGroupSearch::Unknown(error),
    };
    let mut visited = 0;
    for depth in 1..=TAB_BAR_MAX_DEPTH {
        let mut next = Vec::new();
        for child in frontier {
            if visited >= TAB_BAR_NODE_BUDGET {
                return TabGroupSearch::Absent;
            }
            visited += 1;
            let _ = api.set_messaging_timeout(&child, ELEMENT_TIMEOUT);
            if api
                .string(&child, StringAttribute::Role)
                .is_ok_and(|role| role == TAB_GROUP_ROLE)
            {
                return TabGroupSearch::Found(child);
            }
            if depth < TAB_BAR_MAX_DEPTH
                && let Ok(children) = api.children(&child, TAB_GROUP_SCAN_LIMIT)
            {
                next.extend(children);
            }
        }
        if next.is_empty() {
            break;
        }
        frontier = next;
    }
    TabGroupSearch::Absent
}

/// How many tab buttons a tab group has and which one is selected.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TabBarShape {
    /// `AXTabs` elements read (at most the limit).
    pub count: usize,
    /// The index of the first button whose `AXValue` is non-zero.
    pub selected: Option<usize>,
}

/// Reads the shape of a tab group.
///
/// # Errors
///
/// A failure other than an absent `AXTabs` (which is an empty bar).
pub fn read_tab_bar_shape<A: WindowAx>(
    api: &A,
    group: &A::Element,
    limit: usize,
) -> Result<TabBarShape, AxError> {
    let _ = api.set_messaging_timeout(group, ELEMENT_TIMEOUT);
    let tabs = match api.tab_elements(group, limit) {
        Ok(tabs) => tabs,
        Err(error) if error.is_absent() => Vec::new(),
        Err(error) => return Err(error),
    };
    let selected = tabs.iter().position(|tab| {
        let _ = api.set_messaging_timeout(tab, ELEMENT_TIMEOUT);
        api.tab_selected(tab).unwrap_or(false)
    });
    Ok(TabBarShape { count: tabs.len(), selected })
}

/// `kAXErrorIllegalArgument`.
const ILLEGAL_ARGUMENT: i32 = -25201;

/// The `(index, max)` AX range for one page of a list holding `count` values,
/// or `None` when the page is empty: `limit` is zero or `index` is at or past
/// the end. The page is clamped to the values that exist.
fn page_bounds(count: usize, index: usize, limit: usize) -> Option<(isize, isize)> {
    if index >= count || limit == 0 {
        return None;
    }
    let max = isize::try_from(limit.min(count - index)).ok()?;
    Some((isize::try_from(index).ok()?, max))
}

/// Whether a re-read `count` shows `index` is now at or past the end.
const fn index_now_past_end(count: usize, index: usize) -> bool { index >= count }

impl SystemAx {
    fn value_count(element: &AxElement, attribute: &CFString) -> Result<usize, AxError> {
        let mut count: isize = 0;
        // SAFETY: live element and attribute; valid out pointer; no ownership.
        let code = unsafe {
            AXUIElementGetAttributeValueCount(
                element.as_ref(),
                attribute.as_concrete_TypeRef(),
                &raw mut count,
            )
        };
        if code != 0 {
            return Err(AxError::from_code(code));
        }
        usize::try_from(count).map_err(|_| AxError::WrongType)
    }

    fn copy_elements(
        &self,
        element: &AxElement,
        attribute: &CFString,
        index: usize,
        limit: usize,
    ) -> Result<Vec<AxElement>, AxError> {
        // `AXUIElementCopyAttributeValues` answers kAXErrorIllegalArgument (not
        // an empty array) when `index` is at or past the end, so a paged reader
        // would see "no more values" as a failure. Read the live count first
        // and answer an empty page past the end; clamp the page to it.
        let count = Self::value_count(element, attribute)?;
        let Some((first, max)) = page_bounds(count, index, limit) else {
            return Ok(Vec::new());
        };
        let mut array: CFTypeRef = std::ptr::null();
        // SAFETY: live element and attribute; valid out pointer. On success the
        // callee returns a +1 CFArray (Copy rule) wrapped below exactly once.
        let code = unsafe {
            AXUIElementCopyAttributeValues(
                element.as_ref(),
                attribute.as_concrete_TypeRef(),
                first,
                max,
                &raw mut array,
            )
        };
        if code != 0 {
            // The list can shrink between the count read and the copy, which
            // answers illegal argument. Re-read the count: when `index` is now
            // at or past the end it is the end of the list, not a failure.
            if code == ILLEGAL_ARGUMENT
                && Self::value_count(element, attribute)
                    .is_ok_and(|now| index_now_past_end(now, index))
            {
                return Ok(Vec::new());
            }
            return Err(AxError::from_code(code));
        }
        if array.is_null() {
            return Err(AxError::NoValue);
        }
        // SAFETY: non-null +1 reference owned by the wrapper.
        let array = unsafe { CFType::wrap_under_create_rule(array) };
        let array = array.downcast_into::<CFArray>().ok_or(AxError::WrongType)?;
        let mut out = Vec::with_capacity(usize::try_from(array.len()).unwrap_or(0));
        for position in 0..array.len() {
            let item = array.get(position).ok_or(AxError::NoValue)?;
            out.push(self.element_from(*item)?);
        }
        Ok(out)
    }

    fn copy_element(
        &self,
        element: &AxElement,
        attribute: &CFString,
    ) -> Result<AxElement, AxError> {
        let value = Self::copy_value(element, attribute)?;
        self.element_from(value.as_CFTypeRef())
    }

    fn set_value(element: &AxElement, attribute: &CFString, value: &CFType) -> Result<(), AxError> {
        // SAFETY: live element, attribute and value; the callee neither retains
        // nor consumes them (AXUIElement.h `AXUIElementSetAttributeValue`).
        let code = unsafe {
            AXUIElementSetAttributeValue(
                element.as_ref(),
                attribute.as_concrete_TypeRef(),
                value.as_CFTypeRef(),
            )
        };
        if code == 0 {
            Ok(())
        } else {
            Err(AxError::from_code(code))
        }
    }

    const fn geometry_name(&self, attribute: GeometryAttribute) -> &CFString {
        match attribute {
            GeometryAttribute::Position => &self.names.position,
            GeometryAttribute::Size => &self.names.size,
        }
    }
}

impl WindowAx for SystemAx {
    fn position(&self, element: &AxElement) -> Result<AxPoint, AxError> {
        value::read_point(&Self::copy_value(element, &self.names.position)?)
    }

    fn size(&self, element: &AxElement) -> Result<AxSize, AxError> {
        value::read_size(&Self::copy_value(element, &self.names.size)?)
    }

    fn set_position(&self, element: &AxElement, position: AxPoint) -> Result<(), AxError> {
        let value = value::create_point(position).ok_or(AxError::WrongType)?;
        Self::set_value(element, &self.names.position, &value)
    }

    fn set_size(&self, element: &AxElement, size: AxSize) -> Result<(), AxError> {
        let value = value::create_size(size).ok_or(AxError::WrongType)?;
        Self::set_value(element, &self.names.size, &value)
    }

    fn is_settable(
        &self,
        element: &AxElement,
        attribute: GeometryAttribute,
    ) -> Result<bool, AxError> {
        let mut settable: u8 = 0;
        // SAFETY: live element and attribute; valid out pointer; no ownership.
        let code = unsafe {
            AXUIElementIsAttributeSettable(
                element.as_ref(),
                self.geometry_name(attribute).as_concrete_TypeRef(),
                &raw mut settable,
            )
        };
        if code == 0 {
            Ok(settable != 0)
        } else {
            Err(AxError::from_code(code))
        }
    }

    fn bool_attribute(
        &self,
        element: &AxElement,
        attribute: BoolAttribute,
    ) -> Result<bool, AxError> {
        let value = Self::copy_value(element, self.names.bool_name(attribute))?;
        value.downcast_into::<CFBoolean>().map(bool::from).ok_or(AxError::WrongType)
    }

    fn window_string(
        &self,
        element: &AxElement,
        attribute: WindowString,
    ) -> Result<String, AxError> {
        let value = Self::copy_value(element, self.names.string_name(attribute))?;
        value
            .downcast_into::<CFString>()
            .map(|s| s.to_string())
            .ok_or(AxError::WrongType)
    }

    fn window_count(&self, application: &AxElement) -> Result<usize, AxError> {
        let mut count: isize = 0;
        // SAFETY: live element and attribute; valid out pointer; no ownership.
        let code = unsafe {
            AXUIElementGetAttributeValueCount(
                application.as_ref(),
                self.names.windows.as_concrete_TypeRef(),
                &raw mut count,
            )
        };
        if code != 0 {
            return Err(AxError::from_code(code));
        }
        usize::try_from(count).map_err(|_| AxError::WrongType)
    }

    fn windows(
        &self,
        application: &AxElement,
        index: usize,
        limit: usize,
    ) -> Result<Vec<AxElement>, AxError> {
        self.copy_elements(application, &self.names.windows, index, limit)
    }

    fn focused_window(&self, application: &AxElement) -> Result<AxElement, AxError> {
        self.copy_element(application, &self.names.focused_window)
    }

    fn main_window(&self, application: &AxElement) -> Result<AxElement, AxError> {
        self.copy_element(application, &self.names.main_window)
    }

    fn window_id_capability(&self) -> WindowIdCapability { self.window_id.capability() }

    fn window_id(&self, element: &AxElement) -> Result<u32, WindowIdError> {
        self.window_id.window_id(element)
    }

    fn tab_elements(&self, group: &AxElement, limit: usize) -> Result<Vec<AxElement>, AxError> {
        self.copy_elements(group, &self.names.tabs, 0, limit)
    }

    fn tab_selected(&self, tab: &AxElement) -> Result<bool, AxError> {
        let value = Self::copy_value(tab, &self.names.value)?;
        if let Some(flag) = value.downcast::<CFBoolean>() {
            return Ok(bool::from(flag));
        }
        value
            .downcast_into::<CFNumber>()
            .and_then(|number| number.to_i64())
            .map(|number| number != 0)
            .ok_or(AxError::WrongType)
    }

    fn set_hidden(&self, application: &AxElement, hidden: bool) -> Result<(), AxError> {
        let value = CFBoolean::from(hidden).as_CFType();
        Self::set_value(application, &self.names.hidden, &value)
    }

    fn raise(&self, element: &AxElement) -> Result<(), AxError> {
        // SAFETY: `element` is a live AXUIElement and `raise_action` a live
        // CFString (`kAXRaiseAction`); the call neither retains nor consumes
        // either. The owner thread is the only caller and has set the action
        // timeout on this element object.
        let code = unsafe {
            super::api::AXUIElementPerformAction(
                element.as_ref(),
                self.names.raise_action.as_concrete_TypeRef(),
            )
        };
        if code == 0 {
            Ok(())
        } else {
            Err(AxError::from_code(code))
        }
    }
}

#[cfg(test)]
mod page_tests {
    use super::{index_now_past_end, page_bounds};

    #[test]
    fn page_bounds_table() {
        assert_eq!(page_bounds(0, 0, 8), None, "empty list");
        assert_eq!(page_bounds(3, 3, 8), None, "index == count");
        assert_eq!(page_bounds(3, 9, 8), None, "index > count");
        assert_eq!(page_bounds(3, 0, 0), None, "limit 0");
        assert_eq!(page_bounds(10, 0, 4), Some((0, 4)), "full page");
        assert_eq!(page_bounds(10, 8, 4), Some((8, 2)), "partial last page");
        assert_eq!(page_bounds(10, 9, 4), Some((9, 1)));
    }

    #[test]
    fn a_shrunk_list_is_the_end_of_the_list() {
        assert!(index_now_past_end(2, 2));
        assert!(index_now_past_end(0, 1));
        assert!(!index_now_past_end(5, 2));
    }
}
