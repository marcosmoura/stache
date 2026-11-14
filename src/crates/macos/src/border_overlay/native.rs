//! The `AppKit` / Core Animation side of the overlay engine.
//!
//! Everything here runs on the main thread. Default tests never construct a
//! [`NativeHost`]; they use the fake host in `testing.rs`.

use std::cell::RefCell;
use std::sync::Arc;

use objc2::rc::Retained;
use objc2::runtime::AnyObject;
use objc2::{MainThreadMarker, MainThreadOnly, define_class, msg_send};
use objc2_app_kit::{
    NSBackingStoreType, NSColor, NSNormalWindowLevel, NSPanel, NSScreen, NSView,
    NSWindowAnimationBehavior, NSWindowCollectionBehavior, NSWindowOrderingMode,
    NSWindowSharingType, NSWindowStyleMask,
};
use objc2_core_foundation::{CGPoint, CGSize};
use objc2_core_graphics::{
    CGBitmapContextCreate, CGBitmapContextCreateImage, CGColor, CGColorSpace, CGContext, CGImage,
    CGImageAlphaInfo, CGMutablePath, kCGColorSpaceSRGB,
};
use objc2_foundation::{
    NSArray, NSNumber, NSPoint, NSProcessInfo, NSRect, NSSize, NSString, NSValue,
};
use objc2_quartz_core::{
    CAAnimation, CAAnimationGroup, CABasicAnimation, CACurrentMediaTime, CAGradientLayer,
    CAKeyframeAnimation, CALayer, CAMediaTiming, CAMediaTimingFunction, CAShapeLayer,
    CATransaction, kCAFillRuleEvenOdd,
};

pub use super::corner_radius::{
    CornerRadiusResolver, LazyCornerRadiusResolver, SystemCornerRadiusResolver,
};
use super::{
    AnimKind, AnimLayer, AnimProperty, AnimRepeat, AnimValue, ApplyReport, ApplySummary,
    BorderColor, BorderPlacement, FillSpec, GLOW_PASSES, GlowRender, LayerAnimation, LayerOverride,
    LayerSpec, OrderingMode, OverlayBackend, OverlayFailure, OverlayHost, OverlayOwners,
    OverlayPlan, OverlayRect, OverlayScreen, OverlayWork, Presented, RingShape,
    fallback_corner_radius,
};

/// The collection behavior every overlay is created with.
///
/// `transient` (hidden by Exposé) and `ignoresCycle` only. None of
/// `canJoinAllSpaces`, `moveToActiveSpace`, `stationary`, `managed` or
/// `fullScreenAuxiliary`, so an overlay belongs to one Space and never appears
/// on a fullscreen Space. Part 05 asserts this constant; the effect of each
/// flag on macOS 26 is verified by the native gate.
pub const COLLECTION_BEHAVIOR: NSWindowCollectionBehavior =
    NSWindowCollectionBehavior::Transient.union(NSWindowCollectionBehavior::IgnoresCycle);

define_class!(
    /// A panel that can never take key or main status.
    #[unsafe(super(NSPanel))]
    #[thread_kind = MainThreadOnly]
    #[name = "StacheBorderOverlayPanel"]
    struct OverlayWindow;

    impl OverlayWindow {
        #[unsafe(method(canBecomeKeyWindow))]
        fn can_become_key(&self) -> bool { false }

        #[unsafe(method(canBecomeMainWindow))]
        fn can_become_main(&self) -> bool { false }
    }
);

/// One overlay window and its layers.
///
/// All layers exist from creation; appearance changes only set properties and
/// toggle `hidden`, so the same panel is mutated in place.
pub struct NativePanel {
    window: Retained<OverlayWindow>,
    root: Retained<CALayer>,
    container: Retained<CALayer>,
    glow: Retained<CALayer>,
    /// What the glow layer's contents were last rendered from.
    glow_rendered: Option<GlowRender>,
    ring: Retained<CAShapeLayer>,
    gradient: Retained<CAGradientLayer>,
    gradient_mask: Retained<CAShapeLayer>,
    clip_mask: Retained<CAShapeLayer>,
}

/// The real host: `NSPanel` windows and `CAShapeLayer` strokes.
pub struct NativeHost {
    mtm: MainThreadMarker,
    resolver: Arc<dyn CornerRadiusResolver>,
    fallback_radius: f64,
}

const fn ns_rect(rect: OverlayRect) -> NSRect {
    NSRect::new(
        NSPoint::new(rect.x, rect.y),
        NSSize::new(rect.width, rect.height),
    )
}

impl OverlayHost for NativeHost {
    type Panel = NativePanel;

    fn create_panel(&mut self) -> Result<NativePanel, OverlayFailure> {
        let mtm = self.mtm;
        let frame = NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(1.0, 1.0));
        let allocated = mtm.alloc::<OverlayWindow>().set_ivars(());
        // SAFETY: `initWithContentRect:styleMask:backing:defer:` is the
        // designated NSPanel initializer; the arguments have the documented
        // types and the allocation comes from the superclass chain.
        let window: Retained<OverlayWindow> = unsafe {
            msg_send![
                super(allocated),
                initWithContentRect: frame,
                styleMask: NSWindowStyleMask::Borderless | NSWindowStyleMask::NonactivatingPanel,
                backing: NSBackingStoreType::Buffered,
                defer: true
            ]
        };
        // SAFETY: this module closes a window only through `close` in
        // `release_all`, and the panel is owned solely by the owner map, so
        // disabling release-on-close cannot over-release the one strong reference.
        unsafe { window.setReleasedWhenClosed(false) };
        window.setIgnoresMouseEvents(true);
        window.setOpaque(false);
        window.setBackgroundColor(Some(&NSColor::clearColor()));
        window.setHasShadow(false);
        window.setHidesOnDeactivate(false);
        window.setBecomesKeyOnlyIfNeeded(true);
        window.setSharingType(NSWindowSharingType::None);
        window.setAnimationBehavior(NSWindowAnimationBehavior::None);
        window.setCollectionBehavior(COLLECTION_BEHAVIOR);

        let view = NSView::initWithFrame(mtm.alloc::<NSView>(), frame);
        view.setWantsLayer(true);
        let Some(root) = view.layer() else {
            return Err(OverlayFailure::PanelCreation("NSView.layer"));
        };
        let container = CALayer::layer();
        let glow = CALayer::layer();
        let ring = CAShapeLayer::layer();
        let gradient = CAGradientLayer::layer();
        let gradient_mask = CAShapeLayer::layer();
        let clip_mask = CAShapeLayer::layer();
        // SAFETY: the mask layer is owned by the panel for the gradient
        // layer's whole life; `setMask` retains it.
        unsafe { gradient.setMask(Some(&gradient_mask)) };
        glow.setHidden(true);
        ring.setHidden(true);
        gradient.setHidden(true);
        container.addSublayer(&glow);
        container.addSublayer(&ring);
        container.addSublayer(&gradient);
        root.addSublayer(&container);
        window.setContentView(Some(&view));
        Ok(NativePanel {
            window,
            root,
            container,
            glow,
            glow_rendered: None,
            ring,
            gradient,
            gradient_mask,
            clip_mask,
        })
    }

    fn begin_transaction(&mut self) {
        CATransaction::begin();
        CATransaction::setDisableActions(true);
    }

    fn commit_transaction(&mut self) { CATransaction::commit(); }

    fn screens(&self) -> Vec<OverlayScreen> {
        NSScreen::screens(self.mtm)
            .to_vec()
            .iter()
            .map(|screen| {
                let frame = screen.frame();
                OverlayScreen {
                    frame: OverlayRect::new(
                        frame.origin.x,
                        frame.origin.y,
                        frame.size.width,
                        frame.size.height,
                    ),
                    scale: screen.backingScaleFactor(),
                }
            })
            .collect()
    }

    fn resolved_corner_radius(&self, target_cg_id: u32) -> Option<f64> {
        self.resolver.corner_radius(target_cg_id)
    }

    fn forget_corner_radius(&self, target_cg_id: u32) { self.resolver.forget(target_cg_id); }

    fn fallback_corner_radius(&self) -> f64 { self.fallback_radius }

    fn window_level(&self, _target_cg_id: u32) -> isize {
        // Normal unless the target reports otherwise; no level is read yet.
        NSNormalWindowLevel
    }

    fn configure(&mut self, panel: &mut NativePanel, plan: &OverlayPlan) {
        panel.window.setFrame_display(ns_rect(plan.panel_frame), false);
        apply_layers(panel, &plan.layer_spec());
    }

    fn translate(&mut self, panel: &mut NativePanel, plan: &OverlayPlan) {
        panel
            .window
            .setFrameOrigin(NSPoint::new(plan.panel_frame.x, plan.panel_frame.y));
    }

    fn order_relative(
        &mut self,
        panel: &mut NativePanel,
        target_cg_id: u32,
        level: isize,
        mode: OrderingMode,
        placement: BorderPlacement,
    ) {
        // An unchanged level is not set again (a level change re-stacks).
        if panel.window.level() != level {
            panel.window.setLevel(level);
        }
        let relative_to = match mode {
            OrderingMode::RelativeToTarget => isize::try_from(target_cg_id).unwrap_or(0),
            // 0 orders relative to the panel's own level: to the front of it
            // (`Above`) or the back of it (`Below`). Without relative ordering a
            // below-placed border may be covered by other windows of the level.
            OrderingMode::RaiseFallback => 0,
        };
        let ordering = match placement {
            BorderPlacement::Below => NSWindowOrderingMode::Below,
            // `Top` differs only by the level the caller passes.
            BorderPlacement::Above | BorderPlacement::Top => NSWindowOrderingMode::Above,
        };
        let number = window_number(panel);
        // Ineligible while AppKit re-stacks it; eligible again only when it is
        // now visible directly above its own target.
        if let Some(number) = number {
            super::panel_registry::mark_ineligible(number);
        }
        panel.window.orderWindow_relativeTo(ordering, relative_to);
        if let Some(number) = number
            && placement == BorderPlacement::Above
            && mode == OrderingMode::RelativeToTarget
            && panel.window.isVisible()
        {
            super::panel_registry::mark_eligible(number, target_cg_id);
        }
    }

    fn order_out(&mut self, panel: &mut NativePanel) {
        // Before the order out: no fast order may re-stack (order in) it.
        if let Some(number) = window_number(panel) {
            super::panel_registry::mark_ineligible(number);
        }
        let none: Option<&AnyObject> = None;
        panel.window.orderOut(none);
    }

    fn window_number(&self, panel: &NativePanel) -> Option<u32> { window_number(panel) }

    fn media_time(&self) -> f64 { CACurrentMediaTime() }

    fn presented(&self, panel: &NativePanel) -> Option<Presented> { read_presented(panel) }

    fn add_animation(&mut self, panel: &mut NativePanel, animation: &LayerAnimation) {
        let built = build_animation(animation);
        let key = NSString::from_str(animation.key);
        layer_for(panel, animation.layer).addAnimation_forKey(&built, Some(&key));
    }

    fn remove_animation(&mut self, panel: &mut NativePanel, layer: AnimLayer, key: &'static str) {
        layer_for(panel, layer).removeAnimationForKey(&NSString::from_str(key));
    }

    fn set_ring_override(&mut self, panel: &mut NativePanel, shape: RingShape) {
        let path = ring_path(shape);
        panel.ring.setPath(Some(&path));
        panel.gradient_mask.setPath(Some(&path));
    }

    fn set_layer_override(&mut self, panel: &mut NativePanel, layer_override: LayerOverride) {
        let layer = layer_for(panel, layer_override.layer);
        layer.setHidden(layer_override.hidden);
        #[allow(clippy::cast_possible_truncation)]
        layer.setOpacity(layer_override.opacity as f32);
    }

    fn close(&mut self, panel: NativePanel) {
        if let Some(number) = window_number(&panel) {
            super::panel_registry::mark_ineligible(number);
        }
        panel.ring.removeAllAnimations();
        panel.gradient.removeAllAnimations();
        panel.glow.removeAllAnimations();
        panel.gradient_mask.removeAllAnimations();
        panel.window.close();
        panel.container.removeFromSuperlayer();
    }
}

/// The panel's window server number (positive once it has a window device;
/// `None` before).
fn window_number(panel: &NativePanel) -> Option<u32> {
    u32::try_from(panel.window.windowNumber()).ok().filter(|n| *n > 0)
}

fn layer_for(panel: &NativePanel, layer: AnimLayer) -> &CALayer {
    match layer {
        AnimLayer::Ring => &panel.ring,
        AnimLayer::Gradient => &panel.gradient,
        AnimLayer::Glow => &panel.glow,
        AnimLayer::GradientMask => &panel.gradient_mask,
    }
}

/// A `CGColor` as the Objective-C object Core Animation values hold
/// (toll-free bridged).
fn color_object(color: BorderColor) -> Retained<AnyObject> {
    let color = cg_color(color);
    let pointer = objc2_core_foundation::CFRetained::as_ptr(&color).as_ptr().cast::<AnyObject>();
    // SAFETY: a CGColor is toll-free bridged to an Objective-C object; `retain`
    // adds the strong reference that the returned `Retained` owns, so the
    // object outlives the `CFRetained` released at the end of this function.
    unsafe { Retained::retain(pointer) }.unwrap_or_else(|| {
        // SAFETY: `cg_color` never returns null, so this fallback is unreachable;
        // an NSNumber is an Objective-C object and the cast only widens the type.
        unsafe { Retained::cast_unchecked::<AnyObject>(NSNumber::new_f64(0.0)) }
    })
}

fn point_object(x: f64, y: f64) -> Retained<AnyObject> {
    let value = NSValue::new(CGPoint::new(x, y));
    // SAFETY: an NSValue is an Objective-C object; the cast only widens the type.
    unsafe { Retained::cast_unchecked::<AnyObject>(value) }
}

fn value_object(value: &AnimValue, which: Option<bool>) -> Retained<AnyObject> {
    match value {
        AnimValue::Number(number) => {
            // SAFETY: an NSNumber is an Objective-C object; the cast only widens the type.
            unsafe { Retained::cast_unchecked::<AnyObject>(NSNumber::new_f64(*number)) }
        }
        AnimValue::Color(color) => color_object(*color),
        AnimValue::Colors(from, to) => {
            let array = NSArray::from_retained_slice(&[color_object(*from), color_object(*to)]);
            // SAFETY: an NSArray is an Objective-C object; the cast only widens the type.
            unsafe { Retained::cast_unchecked::<AnyObject>(array) }
        }
        AnimValue::Direction(start, end) => {
            let point = if which == Some(false) { end } else { start };
            point_object(point.x, point.y)
        }
        AnimValue::Ring(shape) => {
            let path = ring_path(*shape);
            let pointer =
                objc2_core_foundation::CFRetained::as_ptr(&path).as_ptr().cast::<AnyObject>();
            // SAFETY: a CGPath is a CoreFoundation object Core Animation accepts
            // as an `id` path value; `retain` adds the strong reference the
            // returned `Retained` owns, so it outlives the `CFRetained`.
            unsafe { Retained::retain(pointer) }.unwrap_or_else(|| {
                // SAFETY: `CGMutablePath::new` never returns null, so this is
                // unreachable; the cast only widens the type.
                unsafe { Retained::cast_unchecked::<AnyObject>(NSNumber::new_f64(0.0)) }
            })
        }
    }
}

/// Builds one `CAAnimation` for `description`. Plain properties: no delegate,
/// no completion block; `removedOnCompletion` stays default so a finished
/// animation falls back to the model value, which was already set.
fn build_animation(description: &LayerAnimation) -> Retained<CAAnimation> {
    if description.property == AnimProperty::Direction {
        // One group: start and end point tracks share timing and phase.
        let children: Vec<Retained<CAAnimation>> =
            [("startPoint", Some(true)), ("endPoint", Some(false))]
                .iter()
                .map(|(path, which)| build_track(description, path, *which, false))
                .collect();
        let group = CAAnimationGroup::animation();
        group.setAnimations(Some(&NSArray::from_retained_slice(&children)));
        apply_timing(&group, description);
        // SAFETY: a group is a CAAnimation.
        return unsafe { Retained::cast_unchecked::<CAAnimation>(group) };
    }
    build_track(description, description.property.key_path(), None, true)
}

fn apply_timing(animation: &CAAnimation, description: &LayerAnimation) {
    animation.setDuration(description.duration);
    match description.repeat {
        // `timeOffset` wraps a single play (offset → end, then 0 → offset), so a
        // phase-preserving rebuild of a single play starts it in the past instead.
        AnimRepeat::Once => {
            animation.setRepeatCount(1.0);
            if description.time_offset > 0.0 {
                animation.setBeginTime(CACurrentMediaTime() - description.time_offset);
            }
        }
        AnimRepeat::Forever => {
            animation.setRepeatCount(f32::INFINITY);
            animation.setTimeOffset(description.time_offset);
        }
    }
}

#[allow(clippy::cast_possible_truncation)]
fn timing_function(points: [f64; 4]) -> Retained<CAMediaTimingFunction> {
    CAMediaTimingFunction::functionWithControlPoints(
        points[0] as f32,
        points[1] as f32,
        points[2] as f32,
        points[3] as f32,
    )
}

fn build_track(
    description: &LayerAnimation,
    key_path: &str,
    which: Option<bool>,
    timed: bool,
) -> Retained<CAAnimation> {
    let path = NSString::from_str(key_path);
    let function = timing_function(description.timing);
    match &description.kind {
        AnimKind::Basic { from, to } => {
            let animation = CABasicAnimation::animationWithKeyPath(Some(&path));
            // SAFETY: the values have the types the key path animates.
            unsafe {
                animation.setFromValue(Some(&value_object(from, which)));
                animation.setToValue(Some(&value_object(to, which)));
            }
            animation.setTimingFunction(Some(&function));
            let animation = animation.into_super().into_super();
            if timed {
                apply_timing(&animation, description);
            } else {
                animation.setDuration(description.duration);
            }
            animation
        }
        AnimKind::Keyframes { values, key_times } => {
            let animation = CAKeyframeAnimation::animationWithKeyPath(Some(&path));
            let objects: Vec<Retained<AnyObject>> =
                values.iter().map(|value| value_object(value, which)).collect();
            let key_time_numbers: Vec<Retained<NSNumber>> =
                key_times.iter().map(|time| NSNumber::new_f64(*time)).collect();
            // SAFETY: one value per key time, of the type the key path animates.
            unsafe { animation.setValues(Some(&NSArray::from_retained_slice(&objects))) };
            animation.setKeyTimes(Some(&NSArray::from_retained_slice(&key_time_numbers)));
            let segments: Vec<Retained<CAMediaTimingFunction>> =
                (1..values.len()).map(|_| function.clone()).collect();
            animation.setTimingFunctions(Some(&NSArray::from_retained_slice(&segments)));
            let animation = animation.into_super().into_super();
            if timed {
                apply_timing(&animation, description);
            } else {
                animation.setDuration(description.duration);
            }
            animation
        }
    }
}

/// Reads a `CGColor` object as sRGB components (alpha last), when it has four.
fn color_of(object: &AnyObject) -> Option<BorderColor> {
    let color = std::ptr::from_ref(object).cast::<CGColor>();
    // SAFETY: the object is a CGColor held by a Core Animation color property.
    let color = unsafe { &*color };
    if CGColor::number_of_components(Some(color)) != 4 {
        return None;
    }
    let components = CGColor::components(Some(color));
    if components.is_null() {
        return None;
    }
    // SAFETY: the pointer addresses `number_of_components` (4) values that live
    // as long as the color, read at once.
    let slice = unsafe { std::slice::from_raw_parts(components, 4) };
    Some(BorderColor {
        r: slice[0],
        g: slice[1],
        b: slice[2],
        a: slice[3],
    })
}

fn read_presented(panel: &NativePanel) -> Option<Presented> {
    // SAFETY: presentationLayer is read on the main thread, right after commit.
    let gradient = unsafe { panel.gradient.presentationLayer() };
    // SAFETY: as above.
    let ring = unsafe { panel.ring.presentationLayer() };
    let shown = |layer: &CALayer, model: &CALayer| !model.isHidden() && layer.opacity() > 0.0;
    let gradient_opacity = gradient
        .as_ref()
        .filter(|layer| shown(layer, &panel.gradient))
        .map(|layer| layer.opacity());
    let ring_opacity = ring
        .as_ref()
        .filter(|layer| shown(layer, &panel.ring))
        .map(|layer| layer.opacity());
    // SAFETY: presentationLayer is read on the main thread, right after commit.
    let glow = unsafe { panel.glow.presentationLayer() };
    let glow_opacity = glow
        .as_ref()
        .filter(|layer| shown(layer, &panel.glow))
        .map(|layer| layer.opacity());
    // The glow has no ring: its color is the one its bitmap was rendered in.
    if let (Some(level), Some(render)) = (glow_opacity, panel.glow_rendered)
        && level > gradient_opacity.unwrap_or(0.0).max(ring_opacity.unwrap_or(0.0))
    {
        return Some(Presented::Glow(render.color));
    }
    let gradient_wins = match (gradient_opacity, ring_opacity) {
        (Some(g), Some(r)) => g >= r,
        (Some(_), None) => true,
        _ => false,
    };
    if gradient_wins {
        // SAFETY: the presentation layer of a CAGradientLayer is a CAGradientLayer.
        let layer = unsafe { Retained::cast_unchecked::<CAGradientLayer>(gradient?) };
        let colors = layer.colors()?;
        if colors.count() < 2 {
            return None;
        }
        let first = color_of(&colors.objectAtIndex(0))?;
        let second = color_of(&colors.objectAtIndex(1))?;
        return Some(Presented::Gradient(first, second));
    }
    ring_opacity?;
    // SAFETY: the presentation layer of a CAShapeLayer is a CAShapeLayer.
    let layer = unsafe { Retained::cast_unchecked::<CAShapeLayer>(ring?) };
    let fill = layer.fillColor()?;
    let components = CGColor::components(Some(&fill));
    if components.is_null() || CGColor::number_of_components(Some(&fill)) != 4 {
        return None;
    }
    // SAFETY: four components, read at once while `fill` is alive.
    let slice = unsafe { std::slice::from_raw_parts(components, 4) };
    let color = BorderColor {
        r: slice[0],
        g: slice[1],
        b: slice[2],
        a: slice[3],
    };
    Some(Presented::Solid(color))
}

fn cg_color(color: BorderColor) -> objc2_core_foundation::CFRetained<CGColor> {
    CGColor::new_srgb(
        color.r.clamp(0.0, 1.0),
        color.g.clamp(0.0, 1.0),
        color.b.clamp(0.0, 1.0),
        color.a.clamp(0.0, 1.0),
    )
}

fn rounded_rects(
    first: (OverlayRect, f64),
    second: (OverlayRect, f64),
) -> objc2_core_foundation::CFRetained<CGMutablePath> {
    let path = CGMutablePath::new();
    for (rect, radius) in [first, second] {
        // SAFETY: a null transform pointer is documented as the identity; the
        // radii are clamped to half the shorter side by `plan_overlay`.
        unsafe {
            CGMutablePath::add_rounded_rect(
                Some(&path),
                std::ptr::null(),
                ns_rect(rect),
                radius,
                radius,
            );
        }
    }
    path
}

/// The even-odd ring path of `shape`.
fn ring_path(shape: RingShape) -> objc2_core_foundation::CFRetained<CGMutablePath> {
    rounded_rects(
        (shape.outer, shape.outer_radius),
        (shape.inner, shape.inner_radius),
    )
}

/// Sets every layer property for `spec` on the panel's existing layers.
fn apply_layers(panel: &mut NativePanel, spec: &LayerSpec) {
    let bounds = ns_rect(spec.bounds);
    let ring_frame = ns_rect(spec.ring_frame);
    let ring_bounds = NSRect::new(NSPoint::new(0.0, 0.0), ring_frame.size);
    panel.root.setContentsScale(spec.scale);
    panel.container.setContentsScale(spec.scale);
    panel.container.setFrame(bounds);
    panel.ring.setContentsScale(spec.scale);
    panel.ring.setFrame(ring_frame);
    panel.gradient.setContentsScale(spec.scale);
    panel.gradient.setFrame(ring_frame);
    panel.gradient_mask.setContentsScale(spec.scale);
    panel.gradient_mask.setFrame(ring_bounds);
    panel.clip_mask.setContentsScale(spec.scale);
    panel.clip_mask.setFrame(bounds);
    panel.glow.setContentsScale(spec.scale);
    panel.glow.setFrame(bounds);

    // Defaults for "not this fill": hidden layers, no shadow, no clip. Opacity
    // returns to 1 so a fade's override never outlives the layer write.
    panel.ring.setHidden(true);
    panel.gradient.setHidden(true);
    panel.glow.setHidden(true);
    panel.ring.setOpacity(1.0);
    panel.gradient.setOpacity(1.0);
    panel.glow.setOpacity(1.0);
    // SAFETY: clearing the mask with `None` is always valid.
    unsafe { panel.container.setMask(None) };

    let ring_path = rounded_rects((spec.outer, spec.outer_radius), (spec.inner, spec.inner_radius));
    // Even-odd: the ring between the outer and the inner edge only, so the
    // stroke lies outside the target and its inner edge follows the corner.
    // SAFETY: `kCAFillRuleEvenOdd` is an immutable framework constant.
    let even_odd = unsafe { kCAFillRuleEvenOdd };
    match spec.fill {
        FillSpec::None => {}
        FillSpec::Solid(color) => {
            panel.ring.setFillRule(even_odd);
            panel.ring.setFillColor(Some(&cg_color(color)));
            panel.ring.setStrokeColor(None);
            panel.ring.setPath(Some(&ring_path));
            panel.ring.setHidden(false);
        }
        FillSpec::Gradient { from, to, start, end } => {
            panel.gradient_mask.setFillRule(even_odd);
            panel.gradient_mask.setFillColor(Some(&CGColor::new_srgb(0.0, 0.0, 0.0, 1.0)));
            panel.gradient_mask.setStrokeColor(None);
            panel.gradient_mask.setPath(Some(&ring_path));
            let (from, to) = (cg_color(from), cg_color(to));
            // SAFETY: a CGColor is toll-free bridged to an Objective-C object,
            // which is what `CAGradientLayer.colors` expects; the array retains
            // both colors and the references do not outlive this statement.
            let colors = unsafe {
                let objects: [&AnyObject; 2] = [
                    &*std::ptr::from_ref::<CGColor>(&from).cast::<AnyObject>(),
                    &*std::ptr::from_ref::<CGColor>(&to).cast::<AnyObject>(),
                ];
                NSArray::from_slice(&objects)
            };
            // SAFETY: `colors` holds two CGColor references as documented.
            unsafe { panel.gradient.setColors(Some(&colors)) };
            panel.gradient.setLocations(None);
            panel.gradient.setStartPoint(CGPoint::new(start.x, start.y));
            panel.gradient.setEndPoint(CGPoint::new(end.x, end.y));
            panel.gradient.setHidden(false);
        }
        FillSpec::Glow { .. } => {
            // No crisp ring: the ring stays hidden. Its path follows the spec
            // (its fill color is left alone) so a fade from solid shows the
            // leaving ring at the right size.
            panel.ring.setFillRule(even_odd);
            panel.ring.setPath(Some(&ring_path));
            let render = spec.glow_render();
            if let Some(render) = render {
                // The layer covers the panel and stretches the tile: corners
                // keep their pixels, edges stretch along, the clear interior
                // stretches over the window (see `GlowRender`).
                panel.glow.setContentsCenter(ns_rect(render.center));
            }
            if let Some(render) = render
                && panel.glow_rendered != Some(render)
            {
                let image = render_glow(&render);
                // SAFETY: a CGImage is toll-free bridged to the Objective-C
                // object `CALayer.contents` expects; the layer retains it.
                unsafe {
                    panel.glow.setContents(image.as_ref().map(|image| {
                        &*objc2_core_foundation::CFRetained::as_ptr(image)
                            .as_ptr()
                            .cast::<AnyObject>()
                    }));
                }
                panel.glow_rendered = image.is_some().then_some(render);
            }
            panel.glow.setHidden(false);
        }
    }
    if let Some(clip) = spec.clip {
        // Everything except the target frame: nothing draws inside the inner edge.
        let hole = rounded_rects((spec.bounds, 0.0), (clip.hole, clip.radius));
        panel.clip_mask.setFillRule(even_odd);
        panel.clip_mask.setFillColor(Some(&CGColor::new_srgb(0.0, 0.0, 0.0, 1.0)));
        panel.clip_mask.setPath(Some(&hole));
        // SAFETY: the mask is owned by the panel and retained by the container.
        unsafe { panel.container.setMask(Some(&panel.clip_mask)) };
    }
}

/// Renders the glow band as shadow only, into a premultiplied RGBA bitmap of
/// `bounds × scale` pixels (the 9-slice tile, or the panel for a tiny
/// target) clipped to the outside of the hole.
///
/// The filled `core` rounded rect is translated far left of the bitmap and
/// drawn [`GLOW_PASSES`] times under a same-colored shadow (blur
/// `blur × scale`) whose offset (in device pixels, which the CTM does not
/// scale) brings only the shadow back onto the core. So no crisp edge is
/// drawn: the window edge lies inside the core and the band is brightest
/// there, fading outward. `None` when the bitmap cannot be created.
fn render_glow(render: &GlowRender) -> Option<objc2_core_foundation::CFRetained<CGImage>> {
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let pixels = |points: f64| (points * render.scale).ceil().max(1.0) as usize;
    let (width, height) = (pixels(render.bounds.width), pixels(render.bounds.height));
    // SAFETY: `kCGColorSpaceSRGB` is an immutable framework constant.
    let space = CGColorSpace::with_name(Some(unsafe { kCGColorSpaceSRGB }))?;
    // SAFETY: a null data pointer lets Core Graphics allocate and own the
    // buffer; 8 bits per component and bytes-per-row 0 (computed) are valid
    // for premultiplied RGBA.
    let context = unsafe {
        CGBitmapContextCreate(
            std::ptr::null_mut(),
            width,
            height,
            8,
            0,
            Some(&space),
            CGImageAlphaInfo::PremultipliedLast.0,
        )
    }?;
    let context = Some(&*context);
    CGContext::scale_ctm(context, render.scale, render.scale);
    // Clip: everything except the target, so nothing draws over the window.
    CGContext::add_path(
        context,
        Some(&rounded_rects(
            (render.bounds, 0.0),
            (render.clip.hole, render.clip.radius),
        )),
    );
    CGContext::eo_clip(context);
    // Far enough that the core and its blur lie wholly left of the bitmap.
    let away = 4.0f64.mul_add(render.blur, render.core.x + render.core.width) + 1.0;
    let color = cg_color(render.color);
    CGContext::set_fill_color_with_color(context, Some(&color));
    CGContext::set_shadow_with_color(
        context,
        CGSize::new(away * render.scale, 0.0),
        render.blur * render.scale,
        Some(&color),
    );
    let core = CGMutablePath::new();
    let shifted = OverlayRect {
        x: render.core.x - away,
        ..render.core
    };
    // SAFETY: a null transform pointer is documented as the identity; the
    // radius is at most half the shorter side (hole radius plus the outset).
    unsafe {
        CGMutablePath::add_rounded_rect(
            Some(&core),
            std::ptr::null(),
            ns_rect(shifted),
            render.core_radius,
            render.core_radius,
        );
    }
    for _ in 0..GLOW_PASSES {
        CGContext::add_path(context, Some(&core));
        CGContext::fill_path(context);
    }
    CGBitmapContextCreateImage(context)
}

thread_local! {
    static OWNERS: RefCell<Option<OverlayOwners<NativeHost>>> = const { RefCell::new(None) };
}

/// The real backend; stateless, the owner map is main-thread-local.
pub struct SystemOverlayBackend {
    resolver: Arc<dyn CornerRadiusResolver>,
}

impl SystemOverlayBackend {
    /// Creates a backend. Constructing it creates no panel.
    #[must_use]
    pub fn new(resolver: Arc<dyn CornerRadiusResolver>) -> Self { Self { resolver } }

    fn with_owners<R>(&self, f: impl FnOnce(&mut OverlayOwners<NativeHost>) -> R) -> Option<R> {
        let mtm = MainThreadMarker::new()?;
        OWNERS.with(|cell| {
            let mut slot = cell.borrow_mut();
            let owners = slot.get_or_insert_with(|| {
                let major = NSProcessInfo::processInfo().operatingSystemVersion().majorVersion;
                OverlayOwners::new(NativeHost {
                    mtm,
                    resolver: Arc::clone(&self.resolver),
                    fallback_radius: fallback_corner_radius(u32::try_from(major).unwrap_or(0)),
                })
            });
            Some(f(owners))
        })
    }
}

impl OverlayBackend for SystemOverlayBackend {
    fn apply(&self, work: &OverlayWork) -> ApplyReport {
        // Off the main thread nothing native runs; the executor contract makes
        // this unreachable, and the summary reports the failure.
        self.with_owners(|owners| owners.apply(work)).unwrap_or_else(|| ApplyReport {
            summary: ApplySummary {
                failed: u32::try_from(work.commands.len()).unwrap_or(u32::MAX),
                ..ApplySummary::default()
            },
            dropped: Vec::new(),
            windows: Vec::new(),
        })
    }

    fn release_all(&self) -> usize { self.with_owners(OverlayOwners::release_all).unwrap_or(0) }
}
