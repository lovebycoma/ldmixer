//! Custom-drawn console widgets: a fader with built-in meters, and a knob.
//!
//! The fader redraws at meter rate, so it renders with GTK's GPU render
//! nodes ([`SnapArea`]): rectangles, gradients and rounded clips, plus a
//! texture of the static scale that is rasterized once per size. The knob
//! changes rarely and stays a Cairo `gtk::DrawingArea`. Both use gesture
//! controllers. Values are positions in `0.0..=1.0`; the owner maps them to
//! dB. `set_value` never fires the change callback, so the UI can follow the
//! device without feedback loops.

use std::cell::{Cell, RefCell};
use std::f64::consts::PI;
use std::rc::Rc;
use std::time::{Duration, Instant};

use gtk::cairo;
use gtk::glib;
use gtk::graphene::{Point, Rect};
use gtk::gsk::{ColorStop, RoundedRect};
use gtk::prelude::*;
use gtk::gdk;

use ldmixer::protocol;

/// An RGB color in 0..1.
pub type Rgb = (f64, f64, f64);

pub const fn hex(c: u32) -> Rgb {
    (
        ((c >> 16) & 0xff) as f64 / 255.0,
        ((c >> 8) & 0xff) as f64 / 255.0,
        (c & 0xff) as f64 / 255.0,
    )
}

const TRACK: Rgb = hex(0x0f1218);
const TICK: Rgb = hex(0x5b6474);
const TEXT: Rgb = hex(0x8b93a3);
const CAP: Rgb = hex(0xdfe4ec);
/// Fader cap body, a dark gray block like a console fader.
const CAP_TOP: Rgb = hex(0x5a5f69);
const CAP_MID: Rgb = hex(0x40444c);
const CAP_BOTTOM: Rgb = hex(0x2c2f35);
const METER_BG: Rgb = hex(0x161a22);
const METER_LOW: Rgb = hex(0x22c55e);
const METER_MID: Rgb = hex(0xeab308);
const METER_HIGH: Rgb = hex(0xef4444);

// ---------------------------------------------------------------------------
// Font-relative sizes
// ---------------------------------------------------------------------------

thread_local! {
    /// Pixels per em: the GTK UI font's size, text scaling included.
    static EM_PX: Cell<f64> = const { Cell::new(REF_EM_PX) };
}

/// 11 pt at 96 dpi, the size the layout was designed at.
const REF_EM_PX: f64 = 11.0 * 96.0 / 72.0;

/// Measure 1 em from the GTK font settings. Call once before building
/// widgets; sizes are taken when widgets are built.
pub fn init_em() {
    let Some(settings) = gtk::Settings::default() else { return };
    let font = settings.gtk_font_name().map(|f| gtk::pango::FontDescription::from_string(&f));
    let px = font.and_then(|d| {
        let size = d.size() as f64 / gtk::pango::SCALE as f64;
        if size <= 0.0 {
            None
        } else if d.is_size_absolute() {
            Some(size)
        } else {
            // gtk-xft-dpi is dpi * 1024 and carries the text scaling factor.
            let dpi = settings.gtk_xft_dpi();
            let dpi = if dpi > 0 { dpi as f64 / 1024.0 } else { 96.0 };
            Some(size * dpi / 72.0)
        }
    });
    if let Some(px) = px {
        EM_PX.with(|e| e.set(px.clamp(8.0, 48.0)));
    }
}

/// `x` em in pixels.
pub fn em(x: f64) -> f64 {
    EM_PX.with(|e| e.get()) * x
}

/// `x` em in whole pixels.
pub fn emi(x: f64) -> i32 {
    em(x).round() as i32
}

fn set(cr: &cairo::Context, c: Rgb) {
    cr.set_source_rgb(c.0, c.1, c.2);
}

fn rounded_rect(cr: &cairo::Context, x: f64, y: f64, w: f64, h: f64, r: f64) {
    let r = r.min(w / 2.0).min(h / 2.0);
    cr.new_sub_path();
    cr.arc(x + w - r, y + r, r, -PI / 2.0, 0.0);
    cr.arc(x + w - r, y + h - r, r, 0.0, PI / 2.0);
    cr.arc(x + r, y + h - r, r, PI / 2.0, PI);
    cr.arc(x + r, y + r, r, PI, 1.5 * PI);
    cr.close_path();
}

type Callback = RefCell<Option<Box<dyn Fn(f64)>>>;

fn rgba(c: Rgb, a: f32) -> gdk::RGBA {
    gdk::RGBA::new(c.0 as f32, c.1 as f32, c.2 as f32, a)
}

fn rect(x: f64, y: f64, w: f64, h: f64) -> Rect {
    Rect::new(x as f32, y as f32, w.max(0.0) as f32, h.max(0.0) as f32)
}

fn rounded(x: f64, y: f64, w: f64, h: f64, r: f64) -> RoundedRect {
    RoundedRect::from_rect(rect(x, y, w, h), r as f32)
}

// ---------------------------------------------------------------------------
// SnapArea: a DrawingArea-like widget that draws with render nodes
// ---------------------------------------------------------------------------

mod imp {
    use std::cell::{Cell, RefCell};

    use gtk::glib;
    use gtk::prelude::*;
    use gtk::subclass::prelude::*;

    type DrawFn = Box<dyn Fn(&gtk::Snapshot, f64, f64)>;
    type ResizeFn = Box<dyn Fn(i32, i32)>;

    #[derive(Default)]
    pub struct SnapArea {
        pub draw: RefCell<Option<DrawFn>>,
        pub resized: RefCell<Vec<ResizeFn>>,
        pub content: Cell<(i32, i32)>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for SnapArea {
        const NAME: &'static str = "LdSnapArea";
        type Type = super::SnapArea;
        type ParentType = gtk::Widget;
    }

    impl ObjectImpl for SnapArea {}

    impl WidgetImpl for SnapArea {
        fn measure(&self, orientation: gtk::Orientation, _for_size: i32) -> (i32, i32, i32, i32) {
            let (w, h) = self.content.get();
            let v = if orientation == gtk::Orientation::Horizontal { w } else { h };
            (v, v, -1, -1)
        }

        fn size_allocate(&self, width: i32, height: i32, baseline: i32) {
            self.parent_size_allocate(width, height, baseline);
            for cb in self.resized.borrow().iter() {
                cb(width, height);
            }
        }

        fn snapshot(&self, snapshot: &gtk::Snapshot) {
            let obj = self.obj();
            if let Some(draw) = self.draw.borrow().as_ref() {
                draw(snapshot, obj.width() as f64, obj.height() as f64);
            }
        }
    }
}

glib::wrapper! {
    /// A widget that renders through a snapshot callback (GPU render nodes)
    /// instead of Cairo, for content that redraws often.
    pub struct SnapArea(ObjectSubclass<imp::SnapArea>)
        @extends gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget;
}

impl SnapArea {
    pub fn new(content_width: i32, content_height: i32) -> SnapArea {
        let a: SnapArea = glib::Object::new();
        use gtk::subclass::prelude::ObjectSubclassIsExt;
        a.imp().content.set((content_width, content_height));
        a
    }

    pub fn set_draw_func(&self, f: impl Fn(&gtk::Snapshot, f64, f64) + 'static) {
        use gtk::subclass::prelude::ObjectSubclassIsExt;
        *self.imp().draw.borrow_mut() = Some(Box::new(f));
    }

    /// Called with the new width and height after every allocation.
    pub fn connect_resize(&self, f: impl Fn(i32, i32) + 'static) {
        use gtk::subclass::prelude::ObjectSubclassIsExt;
        self.imp().resized.borrow_mut().push(Box::new(f));
    }
}

// ---------------------------------------------------------------------------
// Fader
// ---------------------------------------------------------------------------

struct FaderInner {
    value: Cell<f64>,
    default: Cell<f64>,
    accent: Cell<Rgb>,
    meters: RefCell<Vec<f32>>,
    peaks: RefCell<Vec<(f32, Instant)>>,
    drag_origin: Cell<f64>,
    on_change: Callback,
    /// The parts that only change with the widget's size (meter wells,
    /// scale, slot), rasterized once per size into a GPU texture.
    static_layer: RefCell<Option<(i32, i32, i32, gdk::Texture)>>,
}

/// A vertical fader with a dB scale and one meter per channel.
#[derive(Clone)]
pub struct Fader {
    pub area: SnapArea,
    inner: Rc<FaderInner>,
}

// Fader geometry in em, left to right: meters, scale labels, slot with cap.
fn fader_top() -> f64 { (cap_h() / 2.0 + em(0.1)).round() }
fn fader_bottom() -> f64 { (cap_h() / 2.0 + em(0.1)).round() }
fn meter_w() -> f64 { em(1.2).round() }
fn meter_gap() -> f64 { em(0.25).round() }
fn label_w() -> f64 { em(1.0) }
fn cap_w() -> f64 { em(2.0).round() }
fn cap_h() -> f64 { em(2.4).round() }
/// Gap between the groups (meters, labels, slot).
fn group_gap() -> f64 { em(0.4).round() }
/// Width of the slot the cap runs in.
fn slot_w() -> f64 { em(0.27).round().max(2.0) }
const SCALE_DB: [f32; 8] = [0.0, -6.0, -12.0, -18.0, -24.0, -36.0, -48.0, -60.0];

impl Fader {
    /// `channels` meters are drawn left of the slot (0 for none).
    pub fn new(channels: usize, accent: Rgb) -> Fader {
        let area = SnapArea::new(group_width(channels).ceil() as i32 + 4, emi(8.0));
        area.set_vexpand(true);
        area.set_focusable(true);
        area.add_css_class("fader");
        let inner = Rc::new(FaderInner {
            value: Cell::new(0.0),
            default: Cell::new(1.0),
            accent: Cell::new(accent),
            meters: RefCell::new(vec![0.0; channels]),
            peaks: RefCell::new(vec![(0.0, Instant::now()); channels]),
            drag_origin: Cell::new(0.0),
            on_change: RefCell::new(None),
            static_layer: RefCell::new(None),
        });
        let f = Fader { area, inner };
        f.install();
        f
    }

    pub fn connect_changed(&self, cb: impl Fn(f64) + 'static) {
        *self.inner.on_change.borrow_mut() = Some(Box::new(cb));
    }

    pub fn value(&self) -> f64 {
        self.inner.value.get()
    }

    /// Set without notifying.
    pub fn set_value(&self, v: f64) {
        let v = v.clamp(0.0, 1.0);
        if (v - self.inner.value.get()).abs() > f64::EPSILON {
            self.inner.value.set(v);
            self.area.queue_draw();
        }
    }

    pub fn set_accent(&self, c: Rgb) {
        self.inner.accent.set(c);
        self.area.queue_draw();
    }

    /// Meter levels, 0..1 per channel.
    pub fn set_meters(&self, levels: &[f32]) {
        let mut m = self.inner.meters.borrow_mut();
        let mut peaks = self.inner.peaks.borrow_mut();
        let now = Instant::now();
        let mut changed = false;
        for (i, slot) in m.iter_mut().enumerate() {
            let v = levels.get(i).copied().unwrap_or(0.0).clamp(0.0, 1.0);
            // Fast attack, gentle release, so the bar does not flicker.
            let mut next = if v > *slot { v } else { (*slot * 0.82).max(v) };
            if next < 0.002 {
                next = 0.0;
            }
            changed |= (next - *slot).abs() > 0.001;
            *slot = next;
            let p = &mut peaks[i];
            if v >= p.0 || now.duration_since(p.1) > Duration::from_millis(1500) {
                changed |= (p.0 - v).abs() > 0.001;
                *p = (v, now);
            }
        }
        drop((m, peaks));
        // Silent or steady meters cost nothing.
        if changed {
            self.area.queue_draw();
        }
    }

    fn change(&self, v: f64) {
        let v = v.clamp(0.0, 1.0);
        if (v - self.inner.value.get()).abs() <= f64::EPSILON {
            return;
        }
        self.inner.value.set(v);
        self.area.queue_draw();
        if let Some(cb) = self.inner.on_change.borrow().as_ref() {
            cb(v);
        }
    }

    /// Horizontal center of the slot for a widget `width` wide, so labels
    /// above the fader can line up with it.
    pub fn slot_center(&self, width: f64) -> f64 {
        layout(width, self.inner.meters.borrow().len()).2
    }

    fn track_len(&self) -> f64 {
        (self.area.height() as f64 - fader_top() - fader_bottom()).max(1.0)
    }

    fn install(&self) {
        let me = self.clone();
        self.area.set_draw_func(move |snap, w, h| me.draw(snap, w, h));

        let drag = gtk::GestureDrag::new();
        let me = self.clone();
        drag.connect_drag_begin(move |_, _, y| {
            me.area.grab_focus();
            // Clicking the slot away from the cap jumps there; grabbing the
            // cap keeps its position, so it never leaps under the pointer.
            let cap_y = fader_top() + (1.0 - me.value()) * me.track_len();
            if (y - cap_y).abs() > cap_h() * 0.5 {
                me.change(1.0 - (y - fader_top()) / me.track_len());
            }
            me.inner.drag_origin.set(me.value());
        });
        let me = self.clone();
        drag.connect_drag_update(move |g, _, dy| {
            let fine = g
                .current_event_state()
                .contains(gtk::gdk::ModifierType::SHIFT_MASK);
            let scale = if fine { 0.2 } else { 1.0 };
            me.change(me.inner.drag_origin.get() - dy * scale / me.track_len());
        });
        self.area.add_controller(drag);

        let click = gtk::GestureClick::new();
        let me = self.clone();
        click.connect_pressed(move |_, n, _, _| {
            if n == 2 {
                me.change(me.inner.default.get());
            }
        });
        self.area.add_controller(click);

        let scroll = gtk::EventControllerScroll::new(gtk::EventControllerScrollFlags::VERTICAL);
        let me = self.clone();
        scroll.connect_scroll(move |_, _, dy| {
            me.change(me.value() - dy * 0.015);
            glib::Propagation::Stop
        });
        self.area.add_controller(scroll);

        let keys = gtk::EventControllerKey::new();
        let me = self.clone();
        keys.connect_key_pressed(move |_, key, _, _| {
            use gtk::gdk::Key;
            let step = match key {
                Key::Up => 0.01,
                Key::Down => -0.01,
                Key::Page_Up => 0.1,
                Key::Page_Down => -0.1,
                Key::Home => 1.0,
                Key::End => -1.0,
                _ => return glib::Propagation::Proceed,
            };
            me.change(me.value() + step);
            glib::Propagation::Stop
        });
        self.area.add_controller(keys);
    }

    /// Draw the size-dependent background (meter wells, scale, slot).
    fn draw_static(&self, cr: &cairo::Context, w: f64, h: f64) {
        let len = (h - fader_top() - fader_bottom()).max(1.0);
        let ypos = |p: f64| fader_top() + (1.0 - p) * len;
        let channels = self.inner.meters.borrow().len();
        let (meter_x0, labels_right, slot_x) = layout(w, channels);
        for i in 0..channels {
            let x = meter_x0 + i as f64 * (meter_w() + meter_gap());
            set(cr, METER_BG);
            rounded_rect(cr, x, fader_top(), meter_w(), len, 1.5);
            let _ = cr.fill();
        }
        cr.select_font_face("Sans", cairo::FontSlant::Normal, cairo::FontWeight::Normal);
        cr.set_font_size(em(0.58));
        for db in SCALE_DB {
            let y = ypos(protocol::db_to_fader(Some(db))).round() + 0.5;
            set(cr, TICK);
            cr.set_line_width(1.0);
            cr.move_to(labels_right + group_gap() * 0.33, y);
            cr.line_to(labels_right + group_gap() * 0.83, y);
            let _ = cr.stroke();
            let text = format!("{}", db.abs() as i32);
            if let Ok(ext) = cr.text_extents(&text) {
                set(cr, TEXT);
                cr.move_to(labels_right - ext.width() - ext.x_bearing(), y + ext.height() / 2.0);
                let _ = cr.show_text(&text);
            }
        }
        set(cr, TRACK);
        let sw = slot_w();
        rounded_rect(cr, slot_x - sw / 2.0, fader_top() - sw, sw, len + 2.0 * sw, sw / 2.0);
        let _ = cr.fill();
    }

    /// The static layer as a texture, re-rasterized only when the size or
    /// scale changes; the renderer keeps it on the GPU between frames.
    fn static_texture(&self, w: f64, h: f64) -> Option<gdk::Texture> {
        let scale = self.area.scale_factor().max(1);
        let (wi, hi) = (w.ceil() as i32, h.ceil() as i32);
        let mut cache = self.inner.static_layer.borrow_mut();
        let fresh = !matches!(&*cache, Some((cw, ch, cs, _)) if *cw == wi && *ch == hi && *cs == scale);
        if fresh {
            *cache = (|| {
                let mut surf = cairo::ImageSurface::create(cairo::Format::ARgb32, wi * scale, hi * scale).ok()?;
                surf.set_device_scale(scale as f64, scale as f64);
                {
                    let c = cairo::Context::new(&surf).ok()?;
                    self.draw_static(&c, w, h);
                }
                surf.flush();
                let stride = surf.stride() as usize;
                let data = surf.data().ok()?.to_vec();
                // Cairo ARGB32 is premultiplied BGRA in memory on little endian.
                let tex = gdk::MemoryTexture::new(
                    wi * scale,
                    hi * scale,
                    gdk::MemoryFormat::B8g8r8a8Premultiplied,
                    &glib::Bytes::from_owned(data),
                    stride,
                );
                Some((wi, hi, scale, tex.upcast::<gdk::Texture>()))
            })();
        }
        cache.as_ref().map(|(_, _, _, t)| t.clone())
    }

    fn draw(&self, snap: &gtk::Snapshot, w: f64, h: f64) {
        let len = (h - fader_top() - fader_bottom()).max(1.0);
        let ypos = |p: f64| fader_top() + (1.0 - p) * len;
        let channels = self.inner.meters.borrow().len();
        let (meter_x0, _, slot_x) = layout(w, channels);
        let acc = self.inner.accent.get();
        let v = self.value();

        if let Some(tex) = self.static_texture(w, h) {
            snap.append_texture(&tex, &rect(0.0, 0.0, w.ceil(), h.ceil()));
        }

        // Meter levels: one gradient spanning the full height, clipped to
        // the lit part so the colors stay tied to the dB scale.
        let stops = [
            ColorStop::new(0.0, rgba(METER_LOW, 1.0)),
            ColorStop::new(0.7, rgba(METER_LOW, 1.0)),
            ColorStop::new(0.86, rgba(METER_MID, 1.0)),
            ColorStop::new(1.0, rgba(METER_HIGH, 1.0)),
        ];
        let meters = self.inner.meters.borrow();
        let peaks = self.inner.peaks.borrow();
        for (i, &m) in meters.iter().enumerate() {
            let x = meter_x0 + i as f64 * (meter_w() + meter_gap());
            if m > 0.002 {
                let top = fader_top() + (1.0 - m as f64) * len;
                snap.push_rounded_clip(&rounded(x, top, meter_w(), fader_top() + len - top, 1.5));
                snap.append_linear_gradient(
                    &rect(x, top, meter_w(), fader_top() + len - top),
                    &Point::new(x as f32, (fader_top() + len) as f32),
                    &Point::new(x as f32, fader_top() as f32),
                    &stops,
                );
                snap.pop();
            }
            let p = peaks[i].0;
            if p > 0.01 {
                let y = (fader_top() + (1.0 - p as f64) * len).round();
                snap.append_color(&rgba(if p > 0.95 { METER_HIGH } else { CAP }, 1.0), &rect(x, y, meter_w(), 1.0));
            }
        }
        drop((meters, peaks));

        // Slot fill below the cap, in the mix's accent color.
        let sw = slot_w();
        let fill = fader_top() + len - ypos(v) + sw;
        snap.push_rounded_clip(&rounded(slot_x - sw / 2.0, ypos(v), sw, fill, sw / 2.0));
        snap.append_color(&rgba(acc, 0.85), &rect(slot_x - sw / 2.0, ypos(v), sw, fill));
        snap.pop();

        // Cap: shadow, body, accent line.
        let cy = ypos(v);
        let (cx, top) = (slot_x - cap_w() / 2.0, cy - cap_h() / 2.0);
        let (radius, shadow, inset) = (em(0.27), em(0.14).round(), em(0.27).round());
        snap.push_rounded_clip(&rounded(cx, top + shadow, cap_w(), cap_h(), radius));
        snap.append_color(&gdk::RGBA::new(0.0, 0.0, 0.0, 0.45), &rect(cx, top + shadow, cap_w(), cap_h()));
        snap.pop();
        snap.push_rounded_clip(&rounded(cx, top, cap_w(), cap_h(), radius));
        snap.append_linear_gradient(
            &rect(cx, top, cap_w(), cap_h()),
            &Point::new(cx as f32, top as f32),
            &Point::new(cx as f32, (top + cap_h()) as f32),
            &[
                ColorStop::new(0.0, rgba(CAP_TOP, 1.0)),
                ColorStop::new(0.5, rgba(CAP_MID, 1.0)),
                ColorStop::new(1.0, rgba(CAP_BOTTOM, 1.0)),
            ],
        );
        snap.pop();
        snap.append_color(&rgba(acc, 1.0), &rect(cx + inset, cy - 1.0, cap_w() - 2.0 * inset, 2.0));

        if self.area.has_focus() {
            let ring = rounded(cx - 1.5, top - 1.5, cap_w() + 3.0, cap_h() + 3.0, radius + 1.5);
            let c = rgba(acc, 0.6);
            snap.append_border(&ring, &[1.0; 4], &[c, c, c, c]);
        }
    }
}

/// Width of the meters + scale + slot group.
fn group_width(channels: usize) -> f64 {
    let n = channels as f64;
    let meters = if channels > 0 { n * meter_w() + (n - 1.0) * meter_gap() + group_gap() } else { 0.0 };
    meters + label_w() + group_gap() + cap_w()
}

/// `(first meter x, right edge of the scale labels, slot center)`, with
/// the group centered in the widget.
fn layout(w: f64, channels: usize) -> (f64, f64, f64) {
    let x0 = ((w - group_width(channels)) / 2.0).max(2.0).round();
    let n = channels as f64;
    let meters = if channels > 0 { n * meter_w() + (n - 1.0) * meter_gap() + group_gap() } else { 0.0 };
    let labels_right = x0 + meters + label_w();
    let slot_x = (labels_right + group_gap() + cap_w() / 2.0).round();
    (x0, labels_right, slot_x)
}

// ---------------------------------------------------------------------------
// Knob
// ---------------------------------------------------------------------------

struct KnobInner {
    value: Cell<f64>,
    default: f64,
    bipolar: bool,
    accent: Cell<Rgb>,
    drag_origin: Cell<f64>,
    on_change: Callback,
}

/// A rotary control. Bipolar knobs (pan) fill from the center.
#[derive(Clone)]
pub struct Knob {
    pub area: gtk::DrawingArea,
    inner: Rc<KnobInner>,
}

const ARC_START: f64 = 0.75 * PI;
const ARC_SWEEP: f64 = 1.5 * PI;

impl Knob {
    pub fn new(size: i32, bipolar: bool, default: f64, accent: Rgb) -> Knob {
        let area = gtk::DrawingArea::builder()
            .content_width(size)
            .content_height(size)
            .halign(gtk::Align::Center)
            .focusable(true)
            .build();
        area.add_css_class("knob");
        let inner = Rc::new(KnobInner {
            value: Cell::new(default),
            default,
            bipolar,
            accent: Cell::new(accent),
            drag_origin: Cell::new(0.0),
            on_change: RefCell::new(None),
        });
        let k = Knob { area, inner };
        k.install();
        k
    }

    pub fn connect_changed(&self, cb: impl Fn(f64) + 'static) {
        *self.inner.on_change.borrow_mut() = Some(Box::new(cb));
    }

    pub fn value(&self) -> f64 {
        self.inner.value.get()
    }

    pub fn set_value(&self, v: f64) {
        let v = v.clamp(0.0, 1.0);
        if (v - self.inner.value.get()).abs() > f64::EPSILON {
            self.inner.value.set(v);
            self.area.queue_draw();
        }
    }

    pub fn set_accent(&self, c: Rgb) {
        self.inner.accent.set(c);
        self.area.queue_draw();
    }

    fn change(&self, v: f64) {
        let v = v.clamp(0.0, 1.0);
        if (v - self.inner.value.get()).abs() <= f64::EPSILON {
            return;
        }
        self.inner.value.set(v);
        self.area.queue_draw();
        if let Some(cb) = self.inner.on_change.borrow().as_ref() {
            cb(v);
        }
    }

    fn install(&self) {
        let me = self.clone();
        self.area.set_draw_func(move |_, cr, w, h| me.draw(cr, w as f64, h as f64));

        let drag = gtk::GestureDrag::new();
        let me = self.clone();
        drag.connect_drag_begin(move |_, _, _| {
            me.area.grab_focus();
            me.inner.drag_origin.set(me.value());
        });
        let me = self.clone();
        drag.connect_drag_update(move |g, dx, dy| {
            let fine = g
                .current_event_state()
                .contains(gtk::gdk::ModifierType::SHIFT_MASK);
            let travel = if fine { em(60.0) } else { em(12.0) };
            me.change(me.inner.drag_origin.get() + (dx - dy) / travel);
        });
        self.area.add_controller(drag);

        let click = gtk::GestureClick::new();
        let me = self.clone();
        click.connect_pressed(move |_, n, _, _| {
            if n == 2 {
                me.change(me.inner.default);
            }
        });
        self.area.add_controller(click);

        let scroll = gtk::EventControllerScroll::new(gtk::EventControllerScrollFlags::VERTICAL);
        let me = self.clone();
        scroll.connect_scroll(move |_, _, dy| {
            me.change(me.value() - dy * 0.02);
            glib::Propagation::Stop
        });
        self.area.add_controller(scroll);

        let keys = gtk::EventControllerKey::new();
        let me = self.clone();
        keys.connect_key_pressed(move |_, key, _, _| {
            use gtk::gdk::Key;
            let step = match key {
                Key::Up | Key::Right => 0.01,
                Key::Down | Key::Left => -0.01,
                _ => return glib::Propagation::Proceed,
            };
            me.change(me.value() + step);
            glib::Propagation::Stop
        });
        self.area.add_controller(keys);
    }

    fn draw(&self, cr: &cairo::Context, w: f64, h: f64) {
        let (cx, cy) = (w / 2.0, h / 2.0);
        let outer = w.min(h) / 2.0 - 1.0;
        // Tick ring outside the arc, like a hardware knob's scale.
        let tick = (outer * 0.13).max(2.0);
        let r = outer - tick - (outer * 0.06).max(1.5);
        let ring = (r * 0.16).clamp(2.5, 7.0);
        let acc = self.inner.accent.get();
        let v = self.value();
        let angle = |p: f64| ARC_START + p * ARC_SWEEP;

        cr.set_line_cap(cairo::LineCap::Round);
        set(cr, TICK);
        cr.set_line_width(1.0);
        for i in 0..=10 {
            let a = angle(i as f64 / 10.0);
            cr.move_to(cx + a.cos() * (outer - tick), cy + a.sin() * (outer - tick));
            cr.line_to(cx + a.cos() * outer, cy + a.sin() * outer);
            let _ = cr.stroke();
        }
        // End labels below the arc: L / R for pan, -inf / 0 for levels.
        let (left, right) = if self.inner.bipolar { ("L", "R") } else { ("−∞", "0") };
        cr.select_font_face("Sans", cairo::FontSlant::Normal, cairo::FontWeight::Normal);
        cr.set_font_size((outer * 0.3).clamp(em(0.55), em(0.75)));
        set(cr, TEXT);
        for (text, a) in [(left, angle(0.0)), (right, angle(1.0))] {
            if let Ok(ext) = cr.text_extents(text) {
                let (x, y) = (cx + a.cos() * outer, cy + a.sin() * outer);
                cr.move_to(x - ext.width() / 2.0 - ext.x_bearing(), (y + ext.height()).min(h - 1.0));
                let _ = cr.show_text(text);
            }
        }
        // show_text leaves a current point; the arc below must not join it.
        cr.new_path();

        cr.set_line_width(ring);
        set(cr, TRACK);
        cr.arc(cx, cy, r - ring / 2.0, angle(0.0), angle(1.0));
        let _ = cr.stroke();

        let (a0, a1) = if self.inner.bipolar {
            let (a, b) = (angle(0.5), angle(v));
            if a < b { (a, b) } else { (b, a) }
        } else {
            (angle(0.0), angle(v))
        };
        if a1 - a0 > 0.001 {
            set(cr, acc);
            cr.arc(cx, cy, r - ring / 2.0, a0, a1);
            let _ = cr.stroke();
        }

        // Body.
        let body = r - ring - 3.0;
        let grad = cairo::LinearGradient::new(0.0, cy - body, 0.0, cy + body);
        grad.add_color_stop_rgb(0.0, 0.25, 0.27, 0.32);
        grad.add_color_stop_rgb(1.0, 0.13, 0.14, 0.17);
        let _ = cr.set_source(&grad);
        cr.arc(cx, cy, body, 0.0, 2.0 * PI);
        let _ = cr.fill();

        // Pointer.
        let a = angle(v);
        set(cr, CAP);
        cr.set_line_width((body * 0.12).clamp(1.5, 3.5));
        cr.move_to(cx + a.cos() * body * 0.35, cy + a.sin() * body * 0.35);
        cr.line_to(cx + a.cos() * body * 0.85, cy + a.sin() * body * 0.85);
        let _ = cr.stroke();

        if self.area.has_focus() {
            cr.set_source_rgba(acc.0, acc.1, acc.2, 0.5);
            cr.set_line_width(1.0);
            cr.arc(cx, cy, r + 1.5, 0.0, 2.0 * PI);
            let _ = cr.stroke();
        }
    }
}
