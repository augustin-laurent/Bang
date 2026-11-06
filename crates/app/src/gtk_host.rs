//! GTK owns the native window; Slint draws the chrome and WebKit draws mail.
//! Both widgets share GTK's event loop on X11 and Wayland.
use gtk::{cairo, gdk, glib, prelude::*};
use slint::platform::{
    software_renderer::{RepaintBufferType, SoftwareRenderer},
    Clipboard, EventLoopProxy, Key, Platform, PlatformError, PointerEventButton, WindowAdapter,
    WindowEvent, WindowProperties,
};
use slint::{ComponentHandle, LogicalPosition, LogicalSize, PhysicalSize, Rgb8Pixel};
use std::{
    cell::{Cell, RefCell},
    rc::{Rc, Weak},
    time::Duration,
};

thread_local! { static HOST: RefCell<Weak<Host>> = RefCell::new(Weak::new()); }

/// Install before creating the main Slint component. Headless fixtures use their own backend.
pub fn init() -> Result<(), PlatformError> {
    gtk::init().map_err(|e| PlatformError::Other(e.to_string()))?;
    slint::platform::set_platform(Box::new(GtkPlatform))
        .map_err(|e| PlatformError::Other(e.to_string()))
}

/// Attach a native reader to the existing main window.
pub fn attach(
    ui: &crate::BangApp,
    store: Option<std::sync::Arc<bang_core::Store>>,
    bridge: Option<crate::bridge::Bridge>,
) {
    HOST.with(|h| {
        if let Some(host) = h.borrow().upgrade() {
            *host.ui.borrow_mut() = Some(ui.as_weak());
            crate::reader::attach(ui, &host.fixed, store, bridge);
        }
    });
}

/// Apply and follow the Linux desktop's reduced-animation preference.
pub(crate) fn watch_welcome_motion(ui: &crate::BangApp) {
    if let Some(settings) = gtk::Settings::default() {
        ui.set_welcome_motion(settings.is_gtk_enable_animations());
        let weak = ui.as_weak();
        settings.connect_gtk_enable_animations_notify(move |settings| {
            if let Some(ui) = weak.upgrade() {
                ui.set_welcome_motion(settings.is_gtk_enable_animations());
            }
        });
    }
}

struct GtkPlatform;
struct Proxy;
impl EventLoopProxy for Proxy {
    fn quit_event_loop(&self) -> Result<(), slint::EventLoopError> {
        glib::idle_add_once(|| {
            if gtk::main_level() > 0 {
                gtk::main_quit();
            }
        });
        Ok(())
    }
    fn invoke_from_event_loop(
        &self,
        event: Box<dyn FnOnce() + Send>,
    ) -> Result<(), slint::EventLoopError> {
        // MainContext::invoke may run inline on a worker before main() starts.
        // An idle source always delivers on GTK's thread instead.
        glib::idle_add_once(event);
        Ok(())
    }
}
impl Platform for GtkPlatform {
    fn create_window_adapter(&self) -> Result<Rc<dyn WindowAdapter>, PlatformError> {
        let host = Host::new();
        HOST.with(|h| *h.borrow_mut() = Rc::downgrade(&host));
        Ok(host)
    }
    fn run_event_loop(&self) -> Result<(), PlatformError> {
        let timer = glib::timeout_add_local(Duration::from_millis(16), || {
            slint::platform::update_timers_and_animations();
            HOST.with(|h| {
                if let Some(h) = h.borrow().upgrade() {
                    if h.slint.has_active_animations() {
                        h.area.queue_draw();
                    }
                }
            });
            glib::ControlFlow::Continue
        });
        gtk::main();
        timer.remove();
        Ok(())
    }
    fn new_event_loop_proxy(&self) -> Option<Box<dyn EventLoopProxy>> {
        Some(Box::new(Proxy))
    }
    fn set_clipboard_text(&self, text: &str, clipboard: Clipboard) {
        native_clipboard(clipboard).set_text(text);
    }
    fn clipboard_text(&self, clipboard: Clipboard) -> Option<String> {
        native_clipboard(clipboard)
            .wait_for_text()
            .map(|s| s.to_string())
    }
    fn open_url(&self, url: &str) -> Result<(), PlatformError> {
        webbrowser::open(url)
            .map(|_| ())
            .map_err(|e| PlatformError::Other(e.to_string()))
    }
}
fn native_clipboard(clipboard: Clipboard) -> gtk::Clipboard {
    gtk::Clipboard::get(&if matches!(clipboard, Clipboard::SelectionClipboard) {
        gdk::SELECTION_PRIMARY
    } else {
        gdk::SELECTION_CLIPBOARD
    })
}
struct FrameBuffer {
    width: i32,
    height: i32,
    scale: i32,
    pixels: Vec<Rgb8Pixel>,
    surface: cairo::ImageSurface,
}

struct Host {
    slint: slint::Window,
    window: gtk::Window,
    area: gtk::DrawingArea,
    fixed: gtk::Fixed,
    renderer: SoftwareRenderer,
    frame: RefCell<Option<FrameBuffer>>,
    modifiers: Cell<gdk::ModifierType>,
    im: gtk::IMMulticontext,
    ui: RefCell<Option<slint::Weak<crate::BangApp>>>,
}
impl Host {
    fn new() -> Rc<Self> {
        let window = gtk::Window::new(gtk::WindowType::Toplevel);
        window.set_icon_name(Some("io.github.augustin_laurent.Bang"));
        window.set_default_size(1440, 900);
        let overlay = gtk::Overlay::new();
        let area = gtk::DrawingArea::new();
        area.set_can_focus(true);
        area.add_events(gdk::EventMask::ALL_EVENTS_MASK);
        overlay.add(&area);
        let fixed = gtk::Fixed::new();
        overlay.add_overlay(&fixed);
        // GtkOverlay creates an input window even for a no-window child.
        // Pass its empty area through to Slint; WebKit's own child window
        // remains interactive inside the reader rectangle.
        overlay.set_overlay_pass_through(&fixed, true);
        window.add(&overlay);
        let host = Rc::new_cyclic(|weak: &Weak<Self>| Self {
            slint: slint::Window::new(weak.clone()),
            window,
            area,
            fixed,
            renderer: SoftwareRenderer::new_with_repaint_buffer_type(RepaintBufferType::NewBuffer),
            frame: RefCell::new(None),
            modifiers: Cell::new(gdk::ModifierType::empty()),
            im: gtk::IMMulticontext::new(),
            ui: RefCell::new(None),
        });
        host.wire();
        host
    }
    fn wire(self: &Rc<Self>) {
        let weak = Rc::downgrade(self);
        self.area.connect_draw(move |area, context| {
            let Some(h) = weak.upgrade() else {
                return glib::Propagation::Proceed;
            };
            let scale = area.scale_factor().max(1);
            let width = area.allocated_width().max(1) * scale;
            let height = area.allocated_height().max(1) * scale;
            let mut frame = h.frame.borrow_mut();
            let resized = frame
                .as_ref()
                .is_none_or(|f| f.width != width || f.height != height || f.scale != scale);
            if resized {
                let Ok(surface) = cairo::ImageSurface::create(cairo::Format::Rgb24, width, height)
                else {
                    return glib::Propagation::Proceed;
                };
                surface.set_device_scale(scale as f64, scale as f64);
                *frame = Some(FrameBuffer {
                    width,
                    height,
                    scale,
                    pixels: vec![Rgb8Pixel::default(); (width * height) as usize],
                    surface,
                });
                h.renderer
                    .set_repaint_buffer_type(RepaintBufferType::NewBuffer);
            }
            let frame = frame.as_mut().unwrap();
            let dirty = h.renderer.render(&mut frame.pixels, width as usize);
            // Cairo owns a separate, persistent buffer. Convert only the pixels
            // Slint repainted, keeping the rest for expose events and scrolling.
            let stride = frame.surface.stride() as usize;
            let Ok(mut data) = frame.surface.data() else {
                h.renderer
                    .set_repaint_buffer_type(RepaintBufferType::NewBuffer);
                h.area.queue_draw();
                return glib::Propagation::Stop;
            };
            for (origin, size) in dirty.iter() {
                let x0 = origin.x.max(0) as usize;
                let y0 = origin.y.max(0) as usize;
                let x1 = (x0 + size.width as usize).min(width as usize);
                let y1 = (y0 + size.height as usize).min(height as usize);
                for y in y0..y1 {
                    for x in x0..x1 {
                        let pixel = frame.pixels[y * width as usize + x];
                        let offset = y * stride + x * 4;
                        data[offset..offset + 4].copy_from_slice(
                            &u32::from_be_bytes([0, pixel.r, pixel.g, pixel.b]).to_ne_bytes(),
                        );
                    }
                }
            }
            drop(data);
            frame.surface.mark_dirty();
            h.renderer
                .set_repaint_buffer_type(RepaintBufferType::ReusedBuffer);
            if context.set_source_surface(&frame.surface, 0., 0.).is_ok() {
                let _ = context.paint();
            }
            glib::Propagation::Stop
        });
        let weak = Rc::downgrade(self);
        self.area.connect_size_allocate(move |area, allocation| {
            if let Some(h) = weak.upgrade() {
                h.slint.dispatch_event(WindowEvent::ScaleFactorChanged {
                    scale_factor: area.scale_factor() as f32,
                });
                h.slint.dispatch_event(WindowEvent::Resized {
                    size: LogicalSize::new(allocation.width() as f32, allocation.height() as f32),
                });
                h.area.queue_draw();
            }
        });
        let weak = Rc::downgrade(self);
        self.area.connect_scale_factor_notify(move |area| {
            if let Some(h) = weak.upgrade() {
                h.slint.dispatch_event(WindowEvent::ScaleFactorChanged {
                    scale_factor: area.scale_factor() as f32,
                });
                h.area.queue_draw();
            }
        });
        let weak = Rc::downgrade(self);
        self.window.connect_delete_event(move |_, _| {
            if let Some(h) = weak.upgrade() {
                h.slint.dispatch_event(WindowEvent::CloseRequested);
            }
            glib::Propagation::Stop
        });
        let weak = Rc::downgrade(self);
        self.window.connect_is_active_notify(move |window| {
            if let Some(h) = weak.upgrade() {
                h.slint
                    .dispatch_event(WindowEvent::WindowActiveChanged(window.is_active()));
            }
        });
        let weak = Rc::downgrade(self);
        self.area.connect_button_press_event(move |area, e| {
            area.grab_focus();
            if let Some(h) = weak.upgrade() {
                // GDK sends an extra 2BUTTON_PRESS after the ordinary press.
                if e.event_type() == gdk::EventType::ButtonPress {
                    h.sync_modifiers(e.state());
                    h.slint.dispatch_event(WindowEvent::PointerPressed {
                        position: point(e.position()),
                        button: button(e.button()),
                    });
                }
            }
            glib::Propagation::Stop
        });
        let weak = Rc::downgrade(self);
        self.area.connect_button_release_event(move |_, e| {
            // GDK sends an extra 2BUTTON_RELEASE after the ordinary release.
            if e.event_type() == gdk::EventType::ButtonRelease {
                if let Some(h) = weak.upgrade() {
                    h.slint.dispatch_event(WindowEvent::PointerReleased {
                        position: point(e.position()),
                        button: button(e.button()),
                    });
                }
            }
            glib::Propagation::Stop
        });
        let weak = Rc::downgrade(self);
        self.area.connect_motion_notify_event(move |_, e| {
            if let Some(h) = weak.upgrade() {
                h.slint.dispatch_event(WindowEvent::PointerMoved {
                    position: point(e.position()),
                });
            }
            glib::Propagation::Stop
        });
        let weak = Rc::downgrade(self);
        self.area.connect_leave_notify_event(move |_, _| {
            if let Some(h) = weak.upgrade() {
                h.slint.dispatch_event(WindowEvent::PointerExited);
            }
            glib::Propagation::Proceed
        });
        let weak = Rc::downgrade(self);
        self.area.connect_scroll_event(move |_, e| {
            let (dx, dy) = match e.direction() {
                gdk::ScrollDirection::Up => (0., 48.),
                gdk::ScrollDirection::Down => (0., -48.),
                gdk::ScrollDirection::Left => (48., 0.),
                gdk::ScrollDirection::Right => (-48., 0.),
                _ => {
                    let (x, y) = e.delta();
                    (-x * 32., -y * 32.)
                }
            };
            if let Some(h) = weak.upgrade() {
                h.slint.dispatch_event(WindowEvent::PointerScrolled {
                    position: point(e.position()),
                    delta_x: dx as f32,
                    delta_y: dy as f32,
                });
            }
            glib::Propagation::Stop
        });
        let weak = Rc::downgrade(self);
        self.im.connect_commit(move |_, text| {
            if let Some(h) = weak.upgrade() {
                h.slint
                    .dispatch_event(WindowEvent::KeyPressed { text: text.into() });
                h.slint
                    .dispatch_event(WindowEvent::KeyReleased { text: text.into() });
            }
        });
        let weak = Rc::downgrade(self);
        self.area.connect_realize(move |area| {
            if let Some(h) = weak.upgrade() {
                h.im.set_client_window(area.window().as_ref());
            }
        });
        let weak = Rc::downgrade(self);
        self.area.connect_focus_in_event(move |_, _| {
            if let Some(h) = weak.upgrade() {
                h.im.focus_in();
            }
            glib::Propagation::Proceed
        });
        let weak = Rc::downgrade(self);
        self.area.connect_focus_out_event(move |_, _| {
            if let Some(h) = weak.upgrade() {
                h.im.focus_out();
                h.sync_modifiers(gdk::ModifierType::empty());
            }
            glib::Propagation::Proceed
        });
        let weak = Rc::downgrade(self);
        self.area.connect_key_press_event(move |_, e| {
            if let Some(h) = weak.upgrade() {
                h.key(e, true);
            }
            glib::Propagation::Stop
        });
        let weak = Rc::downgrade(self);
        self.area.connect_key_release_event(move |_, e| {
            if let Some(h) = weak.upgrade() {
                h.key(e, false);
            }
            glib::Propagation::Stop
        });
        // Global app shortcuts must still work when WebKit owns keyboard focus.
        for pressed in [true, false] {
            let weak = Rc::downgrade(self);
            let handler = move |_: &gtk::Window, e: &gdk::EventKey| {
                let Some(h) = weak.upgrade() else {
                    return glib::Propagation::Proceed;
                };
                let global = e.keyval() == gdk::keys::constants::Escape
                    || (e.state().contains(gdk::ModifierType::CONTROL_MASK)
                        && e.keyval()
                            .to_unicode()
                            .is_some_and(|c| "nNfFqQ".contains(c)));
                if global {
                    h.area.grab_focus();
                    if pressed {
                        // A hidden dialog button can retain Slint focus, and
                        // WebKit has its own focus tree. Start global shortcuts
                        // at the app scope; their handlers may focus a field.
                        if let Some(ui) = h.ui.borrow().as_ref().and_then(|ui| ui.upgrade()) {
                            ui.invoke_focus_chrome();
                        }
                    }
                    h.key(e, pressed);
                    return glib::Propagation::Stop;
                }
                glib::Propagation::Proceed
            };
            if pressed {
                self.window.connect_key_press_event(handler);
            } else {
                self.window.connect_key_release_event(handler);
            }
        }
    }
    fn sync_modifiers(&self, state: gdk::ModifierType) {
        let old = self.modifiers.replace(state);
        for (mask, key) in [
            (gdk::ModifierType::CONTROL_MASK, Key::Control),
            (gdk::ModifierType::SHIFT_MASK, Key::Shift),
            (gdk::ModifierType::MOD1_MASK, Key::Alt),
            (gdk::ModifierType::SUPER_MASK, Key::Meta),
        ] {
            if old.contains(mask) != state.contains(mask) {
                let text = key.into();
                self.slint.dispatch_event(if state.contains(mask) {
                    WindowEvent::KeyPressed { text }
                } else {
                    WindowEvent::KeyReleased { text }
                });
            }
        }
    }
    fn key(&self, event: &gdk::EventKey, pressed: bool) {
        self.sync_modifiers(event.state());
        let Some(text) = key_text(event.keyval()) else {
            if !event
                .state()
                .intersects(gdk::ModifierType::CONTROL_MASK | gdk::ModifierType::MOD1_MASK)
                && self.im.filter_keypress(event)
            {
                return;
            }
            if let Some(c) = event.keyval().to_unicode() {
                let text = c.to_string().into();
                self.slint.dispatch_event(if pressed {
                    WindowEvent::KeyPressed { text }
                } else {
                    WindowEvent::KeyReleased { text }
                });
            }
            return;
        };
        self.slint.dispatch_event(if pressed {
            WindowEvent::KeyPressed { text }
        } else {
            WindowEvent::KeyReleased { text }
        });
    }
}
#[allow(non_upper_case_globals)] // GDK names its key symbols this way.
fn key_text(key: gdk::keys::Key) -> Option<slint::SharedString> {
    use gdk::keys::constants::*;
    Some(
        match key {
            Return | KP_Enter => Key::Return,
            Escape => Key::Escape,
            BackSpace => Key::Backspace,
            Delete | KP_Delete => Key::Delete,
            Tab | ISO_Left_Tab => Key::Tab,
            Left | KP_Left => Key::LeftArrow,
            Right | KP_Right => Key::RightArrow,
            Up | KP_Up => Key::UpArrow,
            Down | KP_Down => Key::DownArrow,
            Home | KP_Home => Key::Home,
            End | KP_End => Key::End,
            Page_Up | KP_Page_Up => Key::PageUp,
            Page_Down | KP_Page_Down => Key::PageDown,
            Control_L | Control_R => Key::Control,
            Shift_L | Shift_R => Key::Shift,
            Alt_L | Alt_R => Key::Alt,
            Super_L | Super_R | Meta_L | Meta_R => Key::Meta,
            _ => return None,
        }
        .into(),
    )
}
fn point((x, y): (f64, f64)) -> LogicalPosition {
    LogicalPosition::new(x as f32, y as f32)
}
fn button(button: u32) -> PointerEventButton {
    match button {
        1 => PointerEventButton::Left,
        2 => PointerEventButton::Middle,
        3 => PointerEventButton::Right,
        _ => PointerEventButton::Other,
    }
}
impl WindowAdapter for Host {
    fn window(&self) -> &slint::Window {
        &self.slint
    }
    fn renderer(&self) -> &dyn slint::platform::Renderer {
        &self.renderer
    }
    fn size(&self) -> PhysicalSize {
        PhysicalSize::new(
            (self.area.allocated_width().max(1) * self.area.scale_factor()) as u32,
            (self.area.allocated_height().max(1) * self.area.scale_factor()) as u32,
        )
    }
    fn request_redraw(&self) {
        self.area.queue_draw();
    }
    fn set_visible(&self, visible: bool) -> Result<(), PlatformError> {
        if visible {
            self.window.show_all();
            self.area.grab_focus();
        } else {
            self.window.hide();
            if gtk::main_level() > 0 {
                gtk::main_quit();
            }
        }
        Ok(())
    }
    fn set_size(&self, size: slint::WindowSize) {
        let size = size.to_logical(self.area.scale_factor() as f32);
        self.window.resize(size.width as i32, size.height as i32);
    }
    fn update_window_properties(&self, properties: WindowProperties<'_>) {
        self.window.set_title(&properties.title());
        let constraints = properties.layout_constraints();
        if let Some(min) = constraints.min {
            self.area
                .set_size_request(min.width as i32, min.height as i32);
        }
    }
}

#[cfg(test)]
pub fn snapshot(path: &std::path::Path) {
    HOST.with(|h| {
        let h = h.borrow().upgrade().unwrap();
        let window = h.window.window().unwrap();
        let pixbuf = window
            .pixbuf(0, 0, h.area.allocated_width(), h.area.allocated_height())
            .expect("native GTK screenshot");
        pixbuf.savev(path, "png", &[]).unwrap();
    });
}

#[cfg(test)]
pub fn check_keyboard_and_clipboard(ui: &crate::BangApp) {
    use glib::translate::*;
    HOST.with(|h| {
        let h = h.borrow().upgrade().unwrap();
        let event = |key: gdk::keys::Key, state: gdk::ModifierType, pressed: bool| {
            let mut event = gdk::Event::new(if pressed {
                gdk::EventType::KeyPress
            } else {
                gdk::EventType::KeyRelease
            })
            .downcast::<gdk::EventKey>()
            .unwrap();
            event.as_mut().keyval = key.into_glib();
            event.as_mut().state = state.bits();
            event.as_mut().window = h.area.window().unwrap().to_glib_full();
            event
        };
        // Window-level Ctrl+F is used even when the HTML widget has focus.
        let e = event(
            gdk::keys::constants::f,
            gdk::ModifierType::CONTROL_MASK,
            true,
        );
        h.window.emit_by_name::<bool>("key-press-event", &[&*e]);
        h.key(
            &event(
                gdk::keys::constants::f,
                gdk::ModifierType::CONTROL_MASK,
                false,
            ),
            false,
        );
        for c in "café".chars() {
            let key = gdk::keys::Key::from_unicode(c);
            h.key(&event(key, gdk::ModifierType::empty(), true), true);
            h.key(&event(key, gdk::ModifierType::empty(), false), false);
        }
        assert_eq!(ui.get_search_text(), "café");
        GtkPlatform.set_clipboard_text("Été déjà prêt", Clipboard::DefaultClipboard);
        assert_eq!(
            GtkPlatform
                .clipboard_text(Clipboard::DefaultClipboard)
                .as_deref(),
            Some("Été déjà prêt")
        );
    });
}

/// Inject through the display server, including GTK hit-testing and overlays.
/// Only used by the explicitly enabled, isolated native pointer fixture.
#[cfg(test)]
pub fn click_native(position: LogicalPosition) {
    HOST.with(|h| {
        let h = h.borrow().upgrade().unwrap();
        let scale = h.area.scale_factor() as f32;
        let (x, y) = if std::env::var("GDK_BACKEND").as_deref() == Ok("x11") {
            let (_, x, y) = h.window.window().unwrap().origin();
            (x, y)
        } else {
            (0, 0)
        };
        let status = std::process::Command::new("python3")
            .arg("-c")
            .arg(include_str!("../tests/native_pointer.py"))
            .arg((x + (position.x * scale).round() as i32).to_string())
            .arg((y + (position.y * scale).round() as i32).to_string())
            .status()
            .expect("run native pointer fixture");
        assert!(status.success());
    });
}
