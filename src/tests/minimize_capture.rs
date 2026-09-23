//! Real capture protocol sessions keep their identity while hidden frames fail.
use std::ffi::OsStr;
use std::fs::File;
use std::os::fd::AsFd;
use std::path::Path;
use std::sync::{mpsc, Arc, Mutex};
use std::time::{Duration, Instant};

use niri_ipc::socket::Socket;
use niri_ipc::{Cast, CastTarget, Request, Response};
use smithay::reexports::rustix::fs::{memfd_create, MemfdFlags};
use smithay::reexports::wayland_protocols::ext::foreign_toplevel_list::v1::client::{
    ext_foreign_toplevel_handle_v1::ExtForeignToplevelHandleV1,
    ext_foreign_toplevel_list_v1::{self, ExtForeignToplevelListV1},
};
use smithay::reexports::wayland_protocols::ext::image_capture_source::v1::client::{
    ext_foreign_toplevel_image_capture_source_manager_v1::ExtForeignToplevelImageCaptureSourceManagerV1,
    ext_image_capture_source_v1::ExtImageCaptureSourceV1,
};
use smithay::reexports::wayland_protocols::ext::image_copy_capture::v1::client::{
    ext_image_copy_capture_cursor_session_v1::ExtImageCopyCaptureCursorSessionV1,
    ext_image_copy_capture_frame_v1::{self, ExtImageCopyCaptureFrameV1},
    ext_image_copy_capture_manager_v1::{self, ExtImageCopyCaptureManagerV1},
    ext_image_copy_capture_session_v1::{self, ExtImageCopyCaptureSessionV1},
};
use wayland_client::protocol::wl_pointer::WlPointer;
use wayland_client::protocol::wl_registry::{self, WlRegistry};
use wayland_client::protocol::wl_shm::{self, WlShm};
use wayland_client::protocol::wl_shm_pool::WlShmPool;
use wayland_client::protocol::wl_surface::WlSurface;
use wayland_client::{Connection, Dispatch, Proxy, QueueHandle, WEnum};

use super::client::{ClientId, State};
use super::Fixture;
use crate::ipc::server::IpcServer;

#[derive(Default)]
struct CaptureGlobals {
    source_manager: Option<ExtForeignToplevelImageCaptureSourceManagerV1>,
    copy_manager: Option<ExtImageCopyCaptureManagerV1>,
    shm: Option<WlShm>,
    toplevels: Vec<ExtForeignToplevelHandleV1>,
}

#[derive(Default)]
struct CaptureSession {
    size: Option<(u32, u32)>,
    stopped: bool,
    shm_formats: Vec<WEnum<wl_shm::Format>>,
    has_dmabuf: bool,
}

#[derive(Default)]
struct CapturedFrame {
    ready: bool,
    failed: Option<WEnum<ext_image_copy_capture_frame_v1::FailureReason>>,
}

impl Dispatch<WlRegistry, Arc<Mutex<CaptureGlobals>>> for State {
    fn event(
        _state: &mut Self,
        registry: &WlRegistry,
        event: wl_registry::Event,
        data: &Arc<Mutex<CaptureGlobals>>,
        _conn: &Connection,
        qh: &QueueHandle<Self>,
    ) {
        if let wl_registry::Event::Global {
            name, interface, ..
        } = event
        {
            let mut globals = data.lock().unwrap();
            if interface == ExtForeignToplevelListV1::interface().name {
                registry.bind::<ExtForeignToplevelListV1, _, _>(name, 1, qh, data.clone());
            } else if interface == ExtForeignToplevelImageCaptureSourceManagerV1::interface().name {
                globals.source_manager = Some(registry.bind(name, 1, qh, ()));
            } else if interface == ExtImageCopyCaptureManagerV1::interface().name {
                globals.copy_manager = Some(registry.bind(name, 1, qh, ()));
            } else if interface == WlShm::interface().name {
                globals.shm = Some(registry.bind(name, 1, qh, ()));
            }
        }
    }
}

impl Dispatch<ExtForeignToplevelListV1, Arc<Mutex<CaptureGlobals>>> for State {
    fn event(
        _state: &mut Self,
        _proxy: &ExtForeignToplevelListV1,
        event: ext_foreign_toplevel_list_v1::Event,
        data: &Arc<Mutex<CaptureGlobals>>,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
    ) {
        if let ext_foreign_toplevel_list_v1::Event::Toplevel { toplevel } = event {
            data.lock().unwrap().toplevels.push(toplevel);
        }
    }

    wayland_client::event_created_child!(State, ExtForeignToplevelListV1, [
        0 => (ExtForeignToplevelHandleV1, ())
    ]);
}

impl Dispatch<ExtImageCopyCaptureSessionV1, Arc<Mutex<CaptureSession>>> for State {
    fn event(
        _state: &mut Self,
        _proxy: &ExtImageCopyCaptureSessionV1,
        event: ext_image_copy_capture_session_v1::Event,
        data: &Arc<Mutex<CaptureSession>>,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
    ) {
        let mut data = data.lock().unwrap();
        match event {
            ext_image_copy_capture_session_v1::Event::BufferSize { width, height } => {
                data.size = Some((width, height));
            }
            ext_image_copy_capture_session_v1::Event::Stopped => data.stopped = true,
            ext_image_copy_capture_session_v1::Event::ShmFormat { format } => {
                data.shm_formats.push(format);
            }
            ext_image_copy_capture_session_v1::Event::DmabufFormat { .. } => data.has_dmabuf = true,
            _ => (),
        }
    }
}

impl Dispatch<ExtImageCopyCaptureFrameV1, Arc<Mutex<CapturedFrame>>> for State {
    fn event(
        _state: &mut Self,
        _proxy: &ExtImageCopyCaptureFrameV1,
        event: ext_image_copy_capture_frame_v1::Event,
        data: &Arc<Mutex<CapturedFrame>>,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
    ) {
        let mut data = data.lock().unwrap();
        match event {
            ext_image_copy_capture_frame_v1::Event::Ready => data.ready = true,
            ext_image_copy_capture_frame_v1::Event::Failed { reason } => data.failed = Some(reason),
            _ => (),
        }
    }
}

wayland_client::delegate_noop!(State: ignore ExtForeignToplevelHandleV1);
wayland_client::delegate_noop!(State: ExtForeignToplevelImageCaptureSourceManagerV1);
wayland_client::delegate_noop!(State: ExtImageCaptureSourceV1);
wayland_client::delegate_noop!(State: ExtImageCopyCaptureManagerV1);
wayland_client::delegate_noop!(State: ignore WlShm);
wayland_client::delegate_noop!(State: WlShmPool);
wayland_client::delegate_noop!(State: ignore WlPointer);
wayland_client::delegate_noop!(State: ignore ExtImageCopyCaptureCursorSessionV1);

fn set_up() -> (Fixture, ClientId, WlSurface, Arc<Mutex<CaptureGlobals>>) {
    let mut f = Fixture::new();
    f.niri_state().backend.headless().add_renderer().unwrap();
    f.add_output(1, (800, 600));
    let client = f.add_client();
    let window = f.client(client).create_window();
    let surface = window.surface.clone();
    window.commit();
    f.roundtrip(client);
    let window = f.client(client).window(&surface);
    window.set_size(320, 240);
    window.attach_new_buffer();
    window.ack_last_and_commit();
    f.double_roundtrip(client);

    let globals = Arc::new(Mutex::new(CaptureGlobals::default()));
    let c = f.client(client);
    c.display.get_registry(&c.qh, globals.clone());
    f.double_roundtrip(client);
    assert_eq!(globals.lock().unwrap().toplevels.len(), 1);
    (f, client, surface, globals)
}

fn capture_frame(
    f: &mut Fixture,
    client: ClientId,
    session: &ExtImageCopyCaptureSessionV1,
    shm: &WlShm,
    size: (u32, u32),
    format: wl_shm::Format,
) -> (ExtImageCopyCaptureFrameV1, Arc<Mutex<CapturedFrame>>) {
    let (width, height) = size;
    let file = File::from(memfd_create("niri-capture-test", MemfdFlags::CLOEXEC).unwrap());
    file.set_len(u64::from(width) * u64::from(height) * 4)
        .unwrap();
    let frame_data = Arc::new(Mutex::new(CapturedFrame::default()));
    let c = f.client(client);
    let pool = shm.create_pool(file.as_fd(), (width * height * 4) as i32, &c.qh, ());
    let buffer = pool.create_buffer(
        0,
        width as i32,
        height as i32,
        (width * 4) as i32,
        format,
        &c.qh,
        (),
    );
    let frame = session.create_frame(&c.qh, frame_data.clone());
    frame.attach_buffer(&buffer);
    frame.capture();
    f.double_roundtrip(client);
    (frame, frame_data)
}

fn assert_hidden_frame(data: &Arc<Mutex<CapturedFrame>>) {
    let data = data.lock().unwrap();
    assert!(!data.ready);
    assert_eq!(
        data.failed,
        Some(WEnum::Value(
            ext_image_copy_capture_frame_v1::FailureReason::Unknown
        ))
    );
}

fn ipc_casts(f: &mut Fixture, path: &Path) -> Vec<Cast> {
    let path = path.to_owned();
    let (tx, rx) = mpsc::channel();
    let thread = std::thread::spawn(move || {
        let reply = Socket::connect_to(path)
            .unwrap()
            .send(Request::Casts)
            .unwrap()
            .unwrap();
        tx.send(reply).unwrap();
    });
    let deadline = Instant::now() + Duration::from_secs(10);
    let reply = loop {
        f.state.server.dispatch();
        if let Ok(reply) = rx.try_recv() {
            break reply;
        }
        assert!(
            Instant::now() < deadline,
            "isolated capture IPC request timed out"
        );
        std::thread::sleep(Duration::from_millis(1));
    };
    thread.join().unwrap();
    let Response::Casts(casts) = reply else {
        panic!("unexpected IPC response");
    };
    casts
}

#[test]
fn minimizing_pauses_capture_without_closing_the_session() {
    let (mut f, client, surface, globals) = set_up();
    let name = format!("minimize-capture-test-{}", std::process::id());
    let server = IpcServer::start(&f.niri().event_loop, Some(OsStr::new(&name))).unwrap();
    let socket = server.socket_path.clone().unwrap();
    f.niri().ipc_server = Some(server);
    f.niri_state().ipc_keyboard_layouts_changed();
    let window_id = f.niri().layout.focus().unwrap().id();
    let session_data = Arc::new(Mutex::new(CaptureSession::default()));
    let (session, shm) = {
        let globals = globals.lock().unwrap();
        assert_eq!(globals.toplevels.len(), 1);
        let c = f.client(client);
        let source = globals.source_manager.as_ref().unwrap().create_source(
            &globals.toplevels[0],
            &c.qh,
            (),
        );
        let session = globals.copy_manager.as_ref().unwrap().create_session(
            &source,
            ext_image_copy_capture_manager_v1::Options::empty(),
            &c.qh,
            session_data.clone(),
        );
        (session, globals.shm.clone().unwrap())
    };
    f.double_roundtrip(client);
    assert!(!session_data.lock().unwrap().stopped);
    let (width, height) = session_data.lock().unwrap().size.unwrap();
    let before = ipc_casts(&mut f, &socket);
    assert_eq!(before.len(), 1);
    assert_eq!(
        before[0].target,
        CastTarget::Window {
            id: window_id.get()
        }
    );
    assert!(before[0].is_active);

    let (frame, frame_data) = capture_frame(
        &mut f,
        client,
        &session,
        &shm,
        (width, height),
        wl_shm::Format::Xrgb8888,
    );
    assert!(frame_data.lock().unwrap().ready);
    frame.destroy();
    // With no new window damage, the next frame waits until minimization fails it.
    let (frame, frame_data) = capture_frame(
        &mut f,
        client,
        &session,
        &shm,
        (width, height),
        wl_shm::Format::Xrgb8888,
    );
    assert!(f.niri().image_copy_sessions[0].pending_frame.is_some());
    assert!(frame_data.lock().unwrap().failed.is_none());

    f.client(client)
        .window(&surface)
        .xdg_toplevel
        .set_minimized();
    f.double_roundtrip(client);
    assert!(!session_data.lock().unwrap().stopped);
    assert_eq!(f.niri().image_copy_sessions.len(), 1);
    assert!(f.niri().image_copy_sessions[0].pending_frame.is_none());
    assert_hidden_frame(&frame_data);
    let hidden = ipc_casts(&mut f, &socket);
    let mut expected = before[0].clone();
    expected.is_active = false;
    assert_eq!(hidden, vec![expected]);

    assert!(f.niri_state().restore_window(Some(window_id), None, true));
    f.double_roundtrip(client);
    assert!(!session_data.lock().unwrap().stopped);
    assert_eq!(ipc_casts(&mut f, &socket), before);
    frame.destroy();
    let size = session_data.lock().unwrap().size.unwrap();
    let (restored_frame, restored_data) = capture_frame(
        &mut f,
        client,
        &session,
        &shm,
        size,
        wl_shm::Format::Xrgb8888,
    );
    // The existing session retains damage history; a fresh commit resumes delivery.
    let window = f.client(client).window(&surface);
    window.attach_new_buffer();
    window.surface.damage_buffer(0, 0, 1, 1);
    window.ack_last_and_commit();
    f.double_roundtrip(client);
    assert!(restored_data.lock().unwrap().ready);
    assert!(restored_data.lock().unwrap().failed.is_none());
    restored_frame.destroy();

    f.client(client)
        .window(&surface)
        .xdg_toplevel
        .set_minimized();
    f.double_roundtrip(client);
    let window = f.client(client).window(&surface);
    window.surface.attach(None, 0, 0);
    window.commit();
    f.double_roundtrip(client);
    assert!(session_data.lock().unwrap().stopped);
    assert!(ipc_casts(&mut f, &socket).is_empty());
    session.destroy();
}

#[test]
fn capture_session_created_while_minimized_survives_and_refreshes_constraints() {
    let (mut f, client, surface, globals) = set_up();
    let window_id = f.niri().layout.focus().unwrap().id();
    f.client(client)
        .window(&surface)
        .xdg_toplevel
        .set_minimized();
    f.double_roundtrip(client);

    let session_data = Arc::new(Mutex::new(CaptureSession::default()));
    let (session, shm) = {
        let globals = globals.lock().unwrap();
        let c = f.client(client);
        let source = globals.source_manager.as_ref().unwrap().create_source(
            &globals.toplevels[0],
            &c.qh,
            (),
        );
        let session = globals.copy_manager.as_ref().unwrap().create_session(
            &source,
            ext_image_copy_capture_manager_v1::Options::empty(),
            &c.qh,
            session_data.clone(),
        );
        (session, globals.shm.clone().unwrap())
    };
    f.double_roundtrip(client);
    let size = {
        let data = session_data.lock().unwrap();
        assert!(!data.stopped);
        assert!(!data.has_dmabuf);
        assert!(data
            .shm_formats
            .contains(&WEnum::Value(wl_shm::Format::Xrgb8888)));
        data.size.unwrap()
    };
    let (frame, data) = capture_frame(
        &mut f,
        client,
        &session,
        &shm,
        size,
        wl_shm::Format::Xrgb8888,
    );
    assert_hidden_frame(&data);
    frame.destroy();

    // Restoration renegotiates dimensions from the new output scale, rather than
    // treating the output-independent hidden constraints as the visible buffer size.
    f.niri_output(1).change_current_state(
        None,
        None,
        Some(smithay::output::Scale::Fractional(1.5)),
        None,
    );
    assert!(f.niri_state().restore_window(Some(window_id), None, true));
    f.double_roundtrip(client);
    let restored_size = session_data.lock().unwrap().size.unwrap();
    assert_eq!(restored_size, (480, 360));
    assert!(!session_data.lock().unwrap().stopped);
    let (frame, data) = capture_frame(
        &mut f,
        client,
        &session,
        &shm,
        restored_size,
        wl_shm::Format::Xrgb8888,
    );
    assert!(data.lock().unwrap().ready);
    assert!(data.lock().unwrap().failed.is_none());
    frame.destroy();
    session.destroy();
}

#[test]
fn cursor_capture_sessions_survive_minimization() {
    for initially_minimized in [false, true] {
        let (mut f, client, surface, globals) = set_up();
        let window_id = f.niri().layout.focus().unwrap().id();
        if initially_minimized {
            f.client(client)
                .window(&surface)
                .xdg_toplevel
                .set_minimized();
            f.double_roundtrip(client);
        }
        let session_data = Arc::new(Mutex::new(CaptureSession::default()));
        let (cursor_session, session, shm) = {
            let globals = globals.lock().unwrap();
            let c = f.client(client);
            let source = globals.source_manager.as_ref().unwrap().create_source(
                &globals.toplevels[0],
                &c.qh,
                (),
            );
            let pointer = c.state.seat.as_ref().unwrap().get_pointer(&c.qh, ());
            let cursor_session = globals
                .copy_manager
                .as_ref()
                .unwrap()
                .create_pointer_cursor_session(&source, &pointer, &c.qh, ());
            let session = cursor_session.get_capture_session(&c.qh, session_data.clone());
            (cursor_session, session, globals.shm.clone().unwrap())
        };
        f.double_roundtrip(client);
        assert!(!session_data.lock().unwrap().stopped);
        let size = session_data.lock().unwrap().size.unwrap();
        if initially_minimized {
            assert_eq!(size, (1, 1));
        }
        let (frame, data) = capture_frame(
            &mut f,
            client,
            &session,
            &shm,
            size,
            wl_shm::Format::Argb8888,
        );
        let (frame, data) = if initially_minimized {
            (frame, data)
        } else {
            assert!(data.lock().unwrap().ready);
            frame.destroy();
            let (frame, data) = capture_frame(
                &mut f,
                client,
                &session,
                &shm,
                size,
                wl_shm::Format::Argb8888,
            );
            assert!(!data.lock().unwrap().ready);
            assert!(data.lock().unwrap().failed.is_none());
            f.client(client)
                .window(&surface)
                .xdg_toplevel
                .set_minimized();
            f.double_roundtrip(client);
            (frame, data)
        };
        assert_hidden_frame(&data);
        assert!(!session_data.lock().unwrap().stopped);
        frame.destroy();
        assert!(f.niri_state().restore_window(Some(window_id), None, true));
        f.double_roundtrip(client);
        assert!(!session_data.lock().unwrap().stopped);
        let size = session_data.lock().unwrap().size.unwrap();
        assert!(size.0 > 1 && size.1 > 1);

        f.client(client).window(&surface).attach_null();
        f.client(client).window(&surface).commit();
        f.double_roundtrip(client);
        assert!(session_data.lock().unwrap().stopped);
        session.destroy();
        cursor_session.destroy();
    }
}
