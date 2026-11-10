//! External file export: native Wayland drag-and-drop, or Xdnd on X11.
//! Only copy is offered: the destination owns the transfer, cancellation never
//! removes a source, and browser uploads receive real file:// URIs.
use std::{os::unix::ffi::OsStrExt, path::PathBuf, time::{Duration, Instant}};
use raw_window_handle::{HasDisplayHandle, HasWindowHandle, RawDisplayHandle, RawWindowHandle};
use x11rb::{connection::Connection, protocol::{Event, xproto::*}, wrapper::ConnectionExt as _, CURRENT_TIME, NONE, COPY_FROM_PARENT};

pub fn handoff(app: &mut crate::app::App, ctx: &egui::Context, frame: &eframe::Frame) {
    let wayland = match frame.display_handle().map(|d| d.as_raw()) {
        Ok(RawDisplayHandle::Wayland(display)) => Some(display.display),
        _ => None,
    };
    if let Some(display) = wayland {
        // Bind before the first press: start_drag needs that press's serial.
        if let Err(e) = wayland::init(display, ctx) { log_once(e); }
        if let Some(ok) = wayland::take_outcome() {
            if ok { app.acts.push(crate::app::Act::Refresh); }
        }
    }
    // Whether the window held the keyboard at some point of the drag in progress.
    static HAD_FOCUS: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
    use std::sync::atomic::Ordering::Relaxed;
    let Some(paths) = egui::DragAndDrop::payload::<crate::app::DragPaths>(ctx) else {
        HAD_FOCUS.store(false, Relaxed);
        return;
    };
    // Keep checking even when winit stops delivering motion at the surface edge.
    ctx.request_repaint_after(Duration::from_millis(10));
    let (rect, pos, delta, down, focused) = ctx.input(|i| (i.content_rect(), i.pointer.hover_pos(),
        i.pointer.delta(), i.pointer.primary_down(), i.focused));
    // Only a focus lost during the drag counts: a window dragged from without
    // the keyboard must still move files between its own panels.
    let lost_focus = !focused && HAD_FOCUS.load(Relaxed);
    if focused { HAD_FOCUS.store(true, Relaxed); }
    let outward = if wayland.is_some() { leaves_wayland_window(rect, pos, down, !lost_focus) }
        else { should_handoff(rect, pos, delta, down) };
    if !outward { return; }
    egui::DragAndDrop::clear_payload(ctx);
    HAD_FOCUS.store(false, Relaxed);
    let result = match wayland {
        Some(_) => match frame.window_handle().map(|w| w.as_raw()) {
            Ok(RawWindowHandle::Wayland(window)) => wayland::start(window.surface, &paths.0),
            _ => Err("Wayland surface unavailable".into()),
        }.map(|()| false),
        None => (|| {
            let display = frame.display_handle().map_err(|e| e.to_string())?;
            let RawDisplayHandle::Xlib(display) = display.as_raw() else {
                return Err("External file dragging requires Wayland or X11".into());
            };
            let xlib = x11_dl::xlib::Xlib::open().map_err(|e| e.to_string())?;
            let display = display.display.ok_or("X11 display unavailable")?;
            // Winit owns the implicit press grab. Release it on the SAME connection
            // before our short-lived Xdnd connection takes over the drag.
            unsafe {
                (xlib.XUngrabPointer)(display.as_ptr().cast(), CURRENT_TIME as _);
                (xlib.XSync)(display.as_ptr().cast(), 0);
            }
            drag(&paths.0).map_err(|e| e.to_string())
        })(),
    };
    match result {
        Ok(true) => app.acts.push(crate::app::Act::Refresh),
        Ok(false) => {},
        Err(e) => app.toast(crate::app::ToastKind::Error, e),
    }
    ctx.stop_dragging();
    ctx.input_mut(|i| i.pointer = Default::default());
    ctx.request_repaint();
}

fn log_once(e: String) {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| eprintln!("Wayland file drag unavailable: {e}"));
}

/// Native Wayland export. Shares winit's wl_display so start_drag can use the
/// app's own surface and the serial of the button press that began the drag.
mod wayland {
    use std::{ffi::c_void, io::Write, ptr::NonNull, sync::{Arc, Mutex, OnceLock}};
    use wayland_client::{
        backend::{Backend, ObjectId}, event_created_child, globals::{registry_queue_init, GlobalListContents},
        protocol::{wl_data_device::{self, WlDataDevice}, wl_data_device_manager::{DndAction, WlDataDeviceManager},
            wl_data_offer::WlDataOffer, wl_data_source::{self, WlDataSource}, wl_pointer::{self, WlPointer},
            wl_registry::WlRegistry, wl_seat::WlSeat, wl_surface::WlSurface},
        Connection, Dispatch, Proxy, QueueHandle, WEnum,
    };

    const BTN_LEFT: u32 = 0x110;

    struct Native { conn: Connection, qh: QueueHandle<State>, manager: WlDataDeviceManager, device: WlDataDevice }
    #[derive(Default)]
    struct Shared { press: Option<u32>, outcome: Option<bool>, ctx: Option<egui::Context> }
    struct State;

    static NATIVE: OnceLock<Result<Native, String>> = OnceLock::new();
    static SHARED: Mutex<Shared> = Mutex::new(Shared { press: None, outcome: None, ctx: None });

    fn shared() -> std::sync::MutexGuard<'static, Shared> {
        SHARED.lock().unwrap_or_else(|e| e.into_inner())
    }

    pub fn init(display: NonNull<c_void>, ctx: &egui::Context) -> Result<(), String> {
        NATIVE.get_or_init(|| {
            shared().ctx = Some(ctx.clone());
            // Safety: winit keeps this wl_display alive for the whole app.
            let conn = Connection::from_backend(unsafe { Backend::from_foreign_display(display.as_ptr().cast()) });
            let (globals, mut queue) = registry_queue_init::<State>(&conn).map_err(|e| e.to_string())?;
            let qh = queue.handle();
            let seat: WlSeat = globals.bind(&qh, 1..=5, ()).map_err(|e| e.to_string())?;
            let manager: WlDataDeviceManager = globals.bind(&qh, 3..=3, ()).map_err(|e| e.to_string())?;
            // A second wl_pointer receives the same button serials as winit's.
            seat.get_pointer(&qh, ());
            let device = manager.get_data_device(&seat, &qh, ());
            queue.roundtrip(&mut State).map_err(|e| e.to_string())?;
            std::thread::Builder::new().name("wayland-dnd".into()).spawn(move || {
                while queue.blocking_dispatch(&mut State).is_ok() {}
            }).map_err(|e| e.to_string())?;
            Ok(Native { conn, qh, manager, device })
        }).as_ref().map(|_| ()).map_err(Clone::clone)
    }

    pub fn take_outcome() -> Option<bool> { shared().outcome.take() }

    pub fn start(surface: NonNull<c_void>, paths: &[std::path::PathBuf]) -> Result<(), String> {
        let Some(Ok(n)) = NATIVE.get() else { return Err("Wayland file drag is unavailable".into()) };
        let bytes = super::uri_list(paths).map_err(|e| e.to_string())?;
        let serial = shared().press.ok_or("Could not start file drag; release the mouse and try again")?;
        // Safety: the pointer is winit's live wl_surface for this window.
        let id = unsafe { ObjectId::from_ptr(WlSurface::interface(), surface.as_ptr().cast()) }.map_err(|e| e.to_string())?;
        let origin = WlSurface::from_id(&n.conn, id).map_err(|e| e.to_string())?;
        let source = n.manager.create_data_source(&n.qh, Arc::new(bytes));
        source.offer(super::URI_LIST.into());
        source.set_actions(DndAction::Copy);
        n.device.start_drag(Some(&source), &origin, None, serial);
        n.conn.flush().map_err(|e| e.to_string())
    }

    fn finish(source: &WlDataSource, ok: bool) {
        source.destroy();
        let mut s = shared();
        s.outcome = Some(ok);
        if let Some(ctx) = &s.ctx { ctx.request_repaint(); }
    }

    impl Dispatch<WlDataSource, Arc<Vec<u8>>> for State {
        fn event(_: &mut Self, source: &WlDataSource, event: wl_data_source::Event, bytes: &Arc<Vec<u8>>, _: &Connection, _: &QueueHandle<Self>) {
            match event {
                wl_data_source::Event::Send { mime_type, fd } if mime_type == super::URI_LIST => {
                    // Never block the dispatch thread on a slow reader.
                    let bytes = bytes.clone();
                    std::thread::spawn(move || { let _ = std::fs::File::from(fd).write_all(&bytes); });
                }
                wl_data_source::Event::Cancelled => finish(source, false),
                wl_data_source::Event::DndFinished => finish(source, true),
                _ => {}
            }
        }
    }

    impl Dispatch<WlPointer, ()> for State {
        fn event(_: &mut Self, _: &WlPointer, event: wl_pointer::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {
            if let wl_pointer::Event::Button { serial, button: BTN_LEFT, state: WEnum::Value(state), .. } = event {
                shared().press = (state == wl_pointer::ButtonState::Pressed).then_some(serial);
            }
        }
    }

    impl Dispatch<WlDataDevice, ()> for State {
        fn event(_: &mut Self, _: &WlDataDevice, event: wl_data_device::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {
            // Offers on this device are only clipboard/drag announcements we never read.
            match event {
                wl_data_device::Event::Enter { id: Some(offer), .. }
                | wl_data_device::Event::Selection { id: Some(offer) } => offer.destroy(),
                _ => {}
            }
        }
        event_created_child!(State, WlDataDevice, [wl_data_device::EVT_DATA_OFFER_OPCODE => (WlDataOffer, ())]);
    }

    impl Dispatch<WlRegistry, GlobalListContents> for State {
        fn event(_: &mut Self, _: &WlRegistry, _: wayland_client::protocol::wl_registry::Event, _: &GlobalListContents, _: &Connection, _: &QueueHandle<Self>) {}
    }
    macro_rules! ignore { ($($t:ty),*) => {$(
        impl Dispatch<$t, ()> for State {
            fn event(_: &mut Self, _: &$t, _: <$t as Proxy>::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {}
        }
    )*} }
    ignore!(WlSeat, WlDataDeviceManager, WlDataOffer);
}

const URI_LIST: &str = "text/uri-list";

/// Wayland keeps reporting motion outside the surface during the implicit grab, so
/// internal drags stay local right up to the window border. The same grab hides a
/// window that comes up over this one: a shortcut that shows another workspace or a
/// scratchpad mid-drag leaves the pointer inside this window's rectangle. What does
/// change then is the keyboard focus, so losing it hands the drag to the compositor.
fn leaves_wayland_window(rect: egui::Rect, pos: Option<egui::Pos2>, down: bool, focused: bool) -> bool {
    down && (!focused || pos.is_none_or(|p| !rect.contains(p)))
}

fn should_handoff(rect: egui::Rect, pos: Option<egui::Pos2>, delta: egui::Vec2, down: bool) -> bool {
    if !down { return false; }
    let Some(p) = pos else { return true; };
    if !rect.contains(p) { return true; }
    let edge = rect.shrink(16.0);
    (p.x < edge.left() && delta.x < 0.0)
        || (p.x > edge.right() && delta.x > 0.0)
        || (p.y < edge.top() && delta.y < 0.0)
        || (p.y > edge.bottom() && delta.y > 0.0)
}

fn uri_list(paths: &[PathBuf]) -> Result<Vec<u8>> {
    if paths.is_empty() { return Err("No files selected".into()); }
    let mut out = Vec::new();
    for path in paths {
        let absolute = std::path::absolute(path)?;
        // Check availability without canonicalizing symlinks or losing filename bytes.
        std::fs::symlink_metadata(&absolute)?;
        out.extend_from_slice(b"file://");
        for &byte in absolute.as_os_str().as_bytes() {
            if byte.is_ascii_alphanumeric() || b"/-._~".contains(&byte) { out.push(byte); }
            else { out.extend_from_slice(format!("%{byte:02X}").as_bytes()); }
        }
        out.extend_from_slice(b"\r\n");
    }
    if out.len() > 1024 * 1024 { return Err("Too many filenames for one drag; select fewer files".into()); }
    Ok(out)
}

x11rb::atom_manager! {
    Atoms: AtomCookies {
        XdndAware, XdndEnter, XdndLeave, XdndPosition, XdndStatus,
        XdndDrop, XdndFinished, XdndSelection, XdndActionCopy,
        XdndTypeList, XdndActionList, TARGETS,
        uri: b"text/uri-list",
    }
}

type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;
fn message(c: &impl Connection, window: Window, kind: Atom, data: [u32; 5]) -> Result<()> {
    c.send_event(false, window, EventMask::NO_EVENT, ClientMessageEvent::new(32, window, kind, data))?;
    c.flush()?;
    Ok(())
}

/// Find the deepest Xdnd-aware ancestor beneath the pointer, including clients
/// nested under a window-manager frame. Never depend on window titles/classes.
fn destination(c: &impl Connection, root: Window, aware: Atom) -> Result<(Window, u32, u32)> {
    let pointer = c.query_pointer(root)?.reply()?;
    let packed = ((pointer.root_x as u16 as u32) << 16) | pointer.root_y as u16 as u32;
    let mut current = pointer.child;
    let mut found = (NONE, 0);
    for _ in 0..64 {
        if current == NONE { break; }
        let property = c.get_property(false, current, aware, AtomEnum::ATOM, 0, 1)?.reply();
        if let Ok(p) = property {
            if let Some(version) = p.value32().and_then(|mut v| v.next()) {
                if version >= 3 { found = (current, version.min(5)); }
            }
        }
        let Ok(p) = c.query_pointer(current)?.reply() else { break };
        current = p.child;
    }
    Ok((found.0, found.1, packed))
}

fn drag(paths: &[PathBuf]) -> Result<bool> {
    let bytes = uri_list(paths)?;
    let (c, screen) = x11rb::connect(None)?;
    let root = c.setup().roots[screen].root;
    let a = Atoms::new(&c)?.reply()?;
    let source = c.generate_id()?;
    let pointer = c.query_pointer(root)?.reply()?;
    // An InputOnly window has no Wayland surface. XWayland cannot bridge a
    // grab on it into a Wayland data-device drag. Use a mapped drawable source
    // on-screen, just behind the pointer, while the original surface owns input.
    c.create_window(COPY_FROM_PARENT as u8, source, root,
        pointer.root_x.saturating_sub(2).max(0), pointer.root_y.saturating_sub(2).max(0), 1, 1, 0,
        WindowClass::INPUT_OUTPUT, 0, &CreateWindowAux::new().override_redirect(1)
            .event_mask(EventMask::PROPERTY_CHANGE | EventMask::KEY_PRESS))?;
    c.map_window(source)?;
    c.change_property32(PropMode::REPLACE, source, a.XdndTypeList, AtomEnum::ATOM, &[a.uri])?;
    c.change_property32(PropMode::REPLACE, source, a.XdndActionList, AtomEnum::ATOM, &[a.XdndActionCopy])?;
    c.flush()?;
    let timestamp = loop {
        if let Event::PropertyNotify(p) = c.wait_for_event()? { break p.time; }
    };
    c.set_selection_owner(source, a.XdndSelection, timestamp)?;
    let grabbed = c.grab_pointer(false, source, EventMask::BUTTON_RELEASE,
        GrabMode::ASYNC, GrabMode::ASYNC, NONE, NONE, CURRENT_TIME)?.reply()?;
    if grabbed.status != GrabStatus::SUCCESS { return Err("Could not start file drag; release the mouse and try again".into()); }
    // Connection teardown releases pointer/keyboard/selection even on error.
    c.grab_keyboard(false, source, CURRENT_TIME, GrabMode::ASYNC, GrabMode::ASYNC)?.reply()?;
    c.flush()?;
    let mut target = NONE;
    let mut version = 0;
    let mut accepted = false;
    let mut pending = false;
    let mut released = None;
    let mut dropped = false;
    let mut supplied = false;
    let mut last_position = None;
    let mut final_position = None;
    let started = Instant::now();
    loop {
        while let Some(event) = c.poll_for_event()? {
            match event {
                Event::KeyPress(key) => {
                    let map = c.get_keyboard_mapping(key.detail, 1)?.reply()?;
                    if map.keysyms.contains(&0xff1b) {
                        if target != NONE { message(&c, target, a.XdndLeave, [source, 0, 0, 0, 0])?; }
                        return Ok(false);
                    }
                }
                Event::ButtonRelease(button) if button.detail == 1 && released.is_none() => {
                    released = Some((button.time, Instant::now()));
                    c.ungrab_pointer(button.time)?;
                    c.ungrab_keyboard(button.time)?;
                }
                Event::ClientMessage(msg) if msg.type_ == a.XdndStatus && msg.data.as_data32()[0] == target && !dropped => {
                    let d = msg.data.as_data32();
                    accepted = d[1] & 1 != 0 && d[4] == a.XdndActionCopy;
                    pending = false;
                }
                Event::ClientMessage(msg) if msg.type_ == a.XdndFinished && msg.data.as_data32()[0] == target && dropped => {
                    let d = msg.data.as_data32();
                    return Ok(supplied && (version < 5 || (d[1] & 1 != 0 && d[2] == a.XdndActionCopy)));
                }
                Event::SelectionRequest(request) if request.selection == a.XdndSelection => {
                    let property = if request.property == NONE { request.target } else { request.property };
                    let mut reply_property = NONE;
                    if request.target == a.TARGETS {
                        c.change_property32(PropMode::REPLACE, request.requestor, property, AtomEnum::ATOM, &[a.TARGETS, a.uri])?;
                        reply_property = property;
                    } else if request.target == a.uri {
                        // Chunk requests below X11's maximum request size. This
                        // is a single property; INCR isn't needed for our 1 MiB cap.
                        c.change_property8(PropMode::REPLACE, request.requestor, property, a.uri, &[])?;
                        for chunk in bytes.chunks(32 * 1024) {
                            c.change_property8(PropMode::APPEND, request.requestor, property, a.uri, chunk)?;
                        }
                        supplied = true;
                        reply_property = property;
                    }
                    c.send_event(false, request.requestor, EventMask::NO_EVENT, SelectionNotifyEvent {
                        response_type: SELECTION_NOTIFY_EVENT, sequence: 0, time: request.time,
                        requestor: request.requestor, selection: request.selection,
                        target: request.target, property: reply_property,
                    })?;
                    c.flush()?;
                }
                _ => {}
            }
        }
        if !dropped {
            // Poll coordinates without subscribing to every motion event.
            // Resolve once more after ButtonRelease: a fast motion and release
            // may both arrive between polls. Freeze that destination afterwards.
            let (next, next_version, packed) = match final_position {
                Some(position) => position,
                None => destination(&c, root, a.XdndAware)?,
            };
            if released.is_some() { final_position = Some((next, next_version, packed)); }
            if next != target {
                if target != NONE { message(&c, target, a.XdndLeave, [source, 0, 0, 0, 0])?; }
                target = next;
                version = next_version;
                accepted = false;
                pending = false;
                last_position = None;
                if target != NONE { message(&c, target, a.XdndEnter, [source, version << 24, a.uri, 0, 0])?; }
            }
            if target != NONE && !pending && last_position != Some(packed) {
                message(&c, target, a.XdndPosition, [source, 0, packed, timestamp, a.XdndActionCopy])?;
                pending = true;
                last_position = Some(packed);
            }
            // Covers release racing the grab at the window boundary.
            if released.is_none() && !c.query_pointer(root)?.reply()?.mask.contains(KeyButMask::BUTTON1) {
                released = Some((timestamp, Instant::now()));
                final_position = Some((next, next_version, packed));
                c.ungrab_pointer(CURRENT_TIME)?;
                c.ungrab_keyboard(CURRENT_TIME)?;
            }
        }
        if let Some((time, at)) = released {
            if !dropped && !pending {
                if target == NONE || !accepted {
                    if target != NONE { message(&c, target, a.XdndLeave, [source, 0, 0, 0, 0])?; }
                    return Ok(false);
                }
                message(&c, target, a.XdndDrop, [source, 0, time, 0, 0])?;
                dropped = true;
            }
            if at.elapsed() > Duration::from_secs(10) {
                if !dropped && target != NONE { message(&c, target, a.XdndLeave, [source, 0, 0, 0, 0])?; }
                return Ok(false);
            }
        }
        if started.elapsed() > Duration::from_secs(300) { return Ok(false); }
        c.flush()?;
        std::thread::sleep(Duration::from_millis(10));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn handoff_precedes_surface_exit_but_preserves_internal_drags() {
        use egui::{Rect, pos2, vec2};
        let rect = Rect::from_min_max(pos2(0.0, 0.0), pos2(1200.0, 750.0));
        for (p, outward) in [(pos2(4.0, 300.0), vec2(-2.0, 0.0)),
            (pos2(1196.0, 300.0), vec2(2.0, 0.0)),
            (pos2(600.0, 4.0), vec2(0.0, -2.0)),
            (pos2(600.0, 746.0), vec2(0.0, 2.0))] {
            assert!(should_handoff(rect, Some(p), outward, true));
            assert!(!should_handoff(rect, Some(p), -outward, true));
            assert!(!should_handoff(rect, Some(p), outward, false));
        }
        assert!(!should_handoff(rect, Some(pos2(600.0, 300.0)), vec2(20.0, 0.0), true));
        assert!(should_handoff(rect, None, egui::Vec2::ZERO, true));
        assert!(!should_handoff(rect, None, egui::Vec2::ZERO, false));
    }
    #[test]
    fn a_wayland_drag_leaves_at_the_border_or_when_another_window_takes_the_keyboard() {
        use egui::{Rect, pos2};
        let rect = Rect::from_min_max(pos2(0.0, 0.0), pos2(1200.0, 750.0));
        let inside = Some(pos2(600.0, 300.0));
        assert!(!leaves_wayland_window(rect, inside, true, true), "a drag inside the window stays local");
        assert!(leaves_wayland_window(rect, Some(pos2(1300.0, 300.0)), true, true));
        assert!(leaves_wayland_window(rect, None, true, true));
        assert!(leaves_wayland_window(rect, inside, true, false), "a scratchpad came up over the window");
        assert!(!leaves_wayland_window(rect, inside, false, false), "nothing is held");
    }
    #[test]
    fn uri_payload_preserves_filename_bytes_and_sources() {
        use std::os::unix::ffi::OsStringExt;
        let root = std::env::temp_dir().join(format!("fc_uri_{}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();
        let names = ["Zażółć #1%?.txt".into(), std::ffi::OsString::from_vec(b"bad\xff\nname".to_vec())];
        let paths: Vec<_> = names.into_iter().map(|n| root.join(n)).collect();
        for p in &paths { std::fs::write(p, b"unchanged").unwrap(); }
        let text = String::from_utf8(uri_list(&paths).unwrap()).unwrap();
        assert!(text.contains("Za%C5%BC%C3%B3%C5%82%C4%87%20%231%25%3F.txt\r\n"));
        assert!(text.contains("bad%FF%0Aname\r\n"));
        assert_eq!(text.matches("file:///").count(), 2);
        assert!(uri_list(&[]).is_err());
        assert!(uri_list(&[root.join("missing")]).is_err());
        for p in &paths { assert_eq!(std::fs::read(p).unwrap(), b"unchanged"); }
        std::fs::remove_dir_all(root).unwrap();
    }
    #[test]
    #[ignore = "Moves the test display pointer; run explicitly on a disposable X11 display"]
    fn xdnd_receiver_reads_selection_and_cancellation_keeps_sources() {
        use x11rb::protocol::xtest::ConnectionExt as _;
        assert_eq!(std::env::var("FERROUS_DND_TEST").as_deref(), Ok("1"));
        let root_dir = std::env::temp_dir().join(format!("fc_xdnd_{}", std::process::id()));
        std::fs::create_dir_all(root_dir.join("folder Łódź")).unwrap();
        let file = root_dir.join("upload #1.txt");
        std::fs::write(&file, b"original").unwrap();
        let paths = vec![file.clone(), root_dir.join("folder Łódź")];
        let expected = uri_list(&paths).unwrap();
        for accept in [true, false] {
            let (c, screen) = x11rb::connect(None).unwrap();
            let root = c.setup().roots[screen].root;
            let a = Atoms::new(&c).unwrap().reply().unwrap();
            let original = c.query_pointer(root).unwrap().reply().unwrap();
            assert!(!original.mask.contains(KeyButMask::BUTTON1), "Do not run while dragging");
            let window = c.generate_id().unwrap();
            c.create_window(COPY_FROM_PARENT as u8, window, root, 0, 0, 400, 400, 0,
                WindowClass::INPUT_OUTPUT, 0, &CreateWindowAux::new().override_redirect(1)).unwrap();
            c.change_property32(PropMode::REPLACE, window, a.XdndAware, AtomEnum::ATOM, &[5]).unwrap();
            c.map_window(window).unwrap();
            c.warp_pointer(NONE, window, 0, 0, 0, 0, 100, 100).unwrap();
            c.xtest_fake_input(BUTTON_PRESS_EVENT, 1, CURRENT_TIME, root, 0, 0, 0).unwrap();
            c.flush().unwrap();
            c.get_input_focus().unwrap().reply().unwrap();
            let receiver = std::thread::spawn(move || {
                let deadline = Instant::now() + Duration::from_secs(5);
                let mut received = None;
                loop {
                    assert!(Instant::now() < deadline, "Xdnd receiver timed out");
                    if let Some(event) = c.poll_for_event().unwrap() {
                        match event {
                            Event::ClientMessage(m) if m.type_ == a.XdndPosition => {
                                let source = m.data.as_data32()[0];
                                message(&c, source, a.XdndStatus,
                                    [window, if accept { 3 } else { 2 }, 0, 0, if accept { a.XdndActionCopy } else { NONE }]).unwrap();
                                c.xtest_fake_input(BUTTON_RELEASE_EVENT, 1, CURRENT_TIME, root, 0, 0, 0).unwrap();
                                c.flush().unwrap();
                            }
                            Event::ClientMessage(m) if m.type_ == a.XdndDrop => {
                                assert!(accept);
                                c.convert_selection(window, a.XdndSelection, a.uri, a.uri, CURRENT_TIME).unwrap();
                                c.flush().unwrap();
                            }
                            Event::SelectionNotify(n) => {
                                assert_ne!(n.property, NONE);
                                received = Some(c.get_property(false, window, n.property, a.uri, 0, u32::MAX).unwrap().reply().unwrap().value);
                                let source = c.get_selection_owner(a.XdndSelection).unwrap().reply().unwrap().owner;
                                message(&c, source, a.XdndFinished, [window, 1, a.XdndActionCopy, 0, 0]).unwrap();
                                break;
                            }
                            Event::ClientMessage(m) if m.type_ == a.XdndLeave && !accept => break,
                            _ => {}
                        }
                    }
                    std::thread::sleep(Duration::from_millis(5));
                }
                c.destroy_window(window).unwrap();
                c.warp_pointer(NONE, root, 0, 0, 0, 0, original.root_x, original.root_y).unwrap();
                c.flush().unwrap();
                received
            });
            assert_eq!(drag(&paths).unwrap(), accept);
            let received = receiver.join().unwrap();
            assert_eq!(received, accept.then(|| expected.clone()));
            assert_eq!(std::fs::read(&file).unwrap(), b"original");
            assert!(paths[1].is_dir());
        }
        std::fs::remove_dir_all(root_dir).unwrap();
    }

}
