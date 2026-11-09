//! Typed `AXValue` reads and writes for `CGPoint` and `CGSize`.
//!
//! Every value is type-checked (`CFGetTypeID` against `AXValueGetTypeID`, then
//! `AXValueGetType`) before it is interpreted, and non-finite numbers are
//! rejected as [`AxError::WrongType`]: a malformed value is never turned into a
//! zero or default frame.

use core_foundation::base::{CFGetTypeID, CFType, CFTypeRef, TCFType};
use core_graphics::geometry::{CGPoint, CGSize};

use super::api::AxError;
use super::ffi::{
    AXValueCreate, AXValueGetType, AXValueGetTypeID, AXValueGetValue, VALUE_TYPE_CG_POINT,
    VALUE_TYPE_CG_SIZE,
};
use super::window::{AxPoint, AxSize};

/// Reads an owned CF value as a point.
pub(super) fn read_point(value: &CFType) -> Result<AxPoint, AxError> {
    let mut point = CGPoint::new(0.0, 0.0);
    read(value, VALUE_TYPE_CG_POINT, (&raw mut point).cast())?;
    if point.x.is_finite() && point.y.is_finite() {
        Ok(AxPoint { x: point.x, y: point.y })
    } else {
        Err(AxError::WrongType)
    }
}

/// Reads an owned CF value as a size.
pub(super) fn read_size(value: &CFType) -> Result<AxSize, AxError> {
    let mut size = CGSize::new(0.0, 0.0);
    read(value, VALUE_TYPE_CG_SIZE, (&raw mut size).cast())?;
    if size.width.is_finite() && size.height.is_finite() {
        Ok(AxSize {
            width: size.width,
            height: size.height,
        })
    } else {
        Err(AxError::WrongType)
    }
}

fn read(value: &CFType, value_type: u32, out: *mut std::ffi::c_void) -> Result<(), AxError> {
    let raw: CFTypeRef = value.as_CFTypeRef();
    // SAFETY: `raw` is a live CF object owned by `value`; both calls only read it.
    let (type_id, ax_type) = unsafe { (CFGetTypeID(raw), AXValueGetTypeID()) };
    if type_id != ax_type {
        return Err(AxError::WrongType);
    }
    // SAFETY: `raw` was just checked to be an AXValue.
    if unsafe { AXValueGetType(raw) } != value_type {
        return Err(AxError::WrongType);
    }
    // SAFETY: `raw` is an AXValue of `value_type`; `out` points at a live,
    // properly sized and aligned `CGPoint`/`CGSize` the caller owns.
    if unsafe { AXValueGetValue(raw, value_type, out) } == 0 {
        return Err(AxError::WrongType);
    }
    Ok(())
}

/// Creates an owned `AXValue` holding `point`; `None` when AX rejects it.
pub(super) fn create_point(point: AxPoint) -> Option<CFType> {
    let native = CGPoint::new(point.x, point.y);
    create(VALUE_TYPE_CG_POINT, (&raw const native).cast())
}

/// Creates an owned `AXValue` holding `size`; `None` when AX rejects it.
pub(super) fn create_size(size: AxSize) -> Option<CFType> {
    let native = CGSize::new(size.width, size.height);
    create(VALUE_TYPE_CG_SIZE, (&raw const native).cast())
}

fn create(value_type: u32, value: *const std::ffi::c_void) -> Option<CFType> {
    // SAFETY: `value` points at a live `CGPoint`/`CGSize` matching `value_type`;
    // the callee copies it and returns a +1 reference (Create rule) or NULL.
    let raw = unsafe { AXValueCreate(value_type, value) };
    if raw.is_null() {
        return None;
    }
    // SAFETY: non-null Create-rule reference, owned by the wrapper exactly once.
    Some(unsafe { CFType::wrap_under_create_rule(raw) })
}

#[cfg(test)]
mod tests {
    use core_foundation::number::CFNumber;

    use super::*;

    #[test]
    fn point_and_size_round_trip_with_negative_origin() {
        let point = create_point(AxPoint { x: -1440.5, y: -20.0 }).expect("value");
        assert_eq!(read_point(&point), Ok(AxPoint { x: -1440.5, y: -20.0 }));
        let size = create_size(AxSize { width: 800.0, height: 600.25 }).expect("value");
        assert_eq!(read_size(&size), Ok(AxSize { width: 800.0, height: 600.25 }));
    }

    #[test]
    fn a_point_value_is_not_a_size() {
        let point = create_point(AxPoint { x: 1.0, y: 2.0 }).expect("value");
        assert_eq!(read_size(&point), Err(AxError::WrongType));
        let size = create_size(AxSize { width: 1.0, height: 2.0 }).expect("value");
        assert_eq!(read_point(&size), Err(AxError::WrongType));
    }

    #[test]
    fn a_non_ax_value_is_a_wrong_type_not_a_zero_frame() {
        let number = CFNumber::from(7_i64).as_CFType();
        assert_eq!(read_point(&number), Err(AxError::WrongType));
        assert_eq!(read_size(&number), Err(AxError::WrongType));
    }

    #[test]
    fn non_finite_numbers_are_malformed() {
        let point = create_point(AxPoint { x: f64::NAN, y: 0.0 }).expect("value");
        assert_eq!(read_point(&point), Err(AxError::WrongType));
        let size = create_size(AxSize {
            width: f64::INFINITY,
            height: 1.0,
        })
        .expect("value");
        assert_eq!(read_size(&size), Err(AxError::WrongType));
    }
}
