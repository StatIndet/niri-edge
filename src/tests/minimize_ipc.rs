//! End-to-end CLI validation against an isolated real Wayland client/server fixture.
use std::ffi::OsStr;
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use niri_ipc::state::{EventStreamState, EventStreamStatePart};
use niri_ipc::{Event, Reply, Response};

use super::Fixture;
use crate::ipc::server::IpcServer;

fn cli(f: &mut Fixture, binary: &Path, socket: &Path, args: &[&str]) -> serde_json::Value {
    let mut child = Command::new(binary)
        .env_remove("WAYLAND_DISPLAY")
        .env_remove("DISPLAY")
        .env("NIRI_SOCKET", socket)
        .args(["msg", "--json"])
        .args(args)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        f.state.server.dispatch();
        if child.try_wait().unwrap().is_some() {
            break;
        }
        if Instant::now() >= deadline {
            child.kill().unwrap();
            panic!("isolated CLI request timed out: {args:?}");
        }
        std::thread::sleep(Duration::from_millis(1));
    }
    let output = child.wait_with_output().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    if output.stdout.is_empty() {
        serde_json::Value::Null
    } else {
        serde_json::from_slice(&output.stdout).unwrap()
    }
}

#[test]
#[ignore = "requires cargo build and NIRI_TEST_BINARY pointing to that built niri"]
fn built_cli_minimize_restore_close() {
    let binary = PathBuf::from(std::env::var_os("NIRI_TEST_BINARY").expect("set NIRI_TEST_BINARY"));
    assert!(
        binary.is_absolute(),
        "use an explicit absolute test binary path"
    );
    let mut f = Fixture::new();
    f.add_output(1, (1280, 720));
    f.add_output(2, (900, 600));
    let name = format!("minimize-test-{}", std::process::id());
    let server = IpcServer::start(&f.niri().event_loop, Some(OsStr::new(&name))).unwrap();
    let socket = server.socket_path.clone().unwrap();
    assert_ne!(
        Some(socket.as_os_str()),
        std::env::var_os("NIRI_SOCKET").as_deref()
    );
    f.niri().ipc_server = Some(server);
    f.niri_state().ipc_keyboard_layouts_changed();

    let client = f.add_client();
    let window = f.client(client).create_window();
    let surface = window.surface.clone();
    window.set_title("minimize-cli-test");
    window.commit();
    f.roundtrip(client);
    let window = f.client(client).window(&surface);
    window.set_size(320, 240);
    window.attach_new_buffer();
    window.ack_last_and_commit();
    f.double_roundtrip(client);

    let capabilities = cli(&mut f, &binary, &socket, &["capabilities"]);
    assert_eq!(capabilities["window_minimization"], true);
    let original = cli(&mut f, &binary, &socket, &["windows"]);
    let id = original[0]["id"].as_u64().unwrap();
    let id_arg = id.to_string();
    assert_eq!(original[0]["is_minimized"], false);

    // Follow the actual socket event stream, not a reconstruction from internal objects.
    let mut stream = UnixStream::connect(&socket).unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(10)))
        .unwrap();
    stream.write_all(b"\"EventStream\"\n").unwrap();
    let (tx, rx) = mpsc::channel();
    let reader = std::thread::spawn(move || {
        let mut lines = BufReader::new(stream).lines();
        let reply: Reply = serde_json::from_str(&lines.next().unwrap().unwrap()).unwrap();
        assert!(matches!(reply, Ok(Response::Handled)));
        for line in lines {
            let Ok(line) = line else {
                break;
            };
            let event: Event = serde_json::from_str(&line).unwrap();
            let done = matches!(event, Event::WindowClosed { .. });
            tx.send(event).unwrap();
            if done {
                break;
            }
        }
    });
    // A read-only CLI roundtrip also ensures the stream subscription was dispatched.
    cli(&mut f, &binary, &socket, &["windows"]);
    cli(
        &mut f,
        &binary,
        &socket,
        &["action", "minimize-window", "--id", &id_arg],
    );
    f.double_roundtrip(client);
    let hidden = cli(&mut f, &binary, &socket, &["windows"]);
    assert_eq!(hidden[0]["id"], id);
    assert_eq!(hidden[0]["is_minimized"], true);
    assert_eq!(hidden[0]["is_focused"], false);
    assert!(hidden[0]["workspace_id"].is_null());
    assert!(hidden[0]["layout"]["pos_in_scrolling_layout"].is_null());
    assert!(hidden[0]["layout"]["tile_pos_in_workspace_view"].is_null());
    cli(
        &mut f,
        &binary,
        &socket,
        &[
            "action",
            "restore-window",
            "--output",
            "missing-test-output",
        ],
    );
    assert_eq!(
        cli(&mut f, &binary, &socket, &["windows"])[0]["is_minimized"],
        true
    );

    f.client(client)
        .window(&surface)
        .set_title("updated-while-hidden");
    f.client(client).window(&surface).ack_last_and_commit();
    f.double_roundtrip(client);
    cli(
        &mut f,
        &binary,
        &socket,
        &[
            "action",
            "restore-window",
            "--id",
            &id_arg,
            "--output",
            "headless-2",
        ],
    );
    f.double_roundtrip(client);
    let restored = cli(&mut f, &binary, &socket, &["windows"]);
    assert_eq!(restored[0]["id"], id);
    assert_eq!(restored[0]["title"], "updated-while-hidden");
    assert_eq!(restored[0]["is_minimized"], false);
    assert_eq!(restored[0]["is_focused"], true);
    assert_eq!(
        f.niri().layout.active_output().unwrap().name(),
        "headless-2"
    );

    let deadline = Instant::now() + Duration::from_secs(5);
    let mut folded = EventStreamState::default();
    loop {
        let event = rx
            .recv_timeout(deadline.saturating_duration_since(Instant::now()))
            .unwrap();
        assert!(
            !matches!(event, Event::WindowClosed { .. }),
            "minimize/restore must preserve identity"
        );
        folded.apply(event);
        if folded
            .windows
            .windows
            .get(&id)
            .is_some_and(|w| !w.is_minimized && w.title.as_deref() == Some("updated-while-hidden"))
        {
            break;
        }
    }
    let snapshot: Vec<niri_ipc::Window> = serde_json::from_value(restored).unwrap();
    assert_eq!(
        serde_json::to_value(folded.windows.windows.get(&id)).unwrap(),
        serde_json::to_value(&snapshot[0]).unwrap()
    );

    // A hidden window must also remain closeable through the same ID.
    cli(&mut f, &binary, &socket, &["action", "minimize-window"]);
    cli(
        &mut f,
        &binary,
        &socket,
        &["action", "close-window", "--id", &id_arg],
    );
    f.double_roundtrip(client);
    assert!(f.client(client).window(&surface).close_requested);
    let window = f.client(client).window(&surface);
    window.surface.attach(None, 0, 0);
    window.commit();
    f.double_roundtrip(client);
    assert!(cli(&mut f, &binary, &socket, &["windows"])
        .as_array()
        .unwrap()
        .is_empty());
    cli(&mut f, &binary, &socket, &["action", "restore-window"]);
    assert!(cli(&mut f, &binary, &socket, &["windows"])
        .as_array()
        .unwrap()
        .is_empty());
    reader.join().unwrap();
}
