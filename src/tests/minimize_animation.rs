//! Real client buffers, compositor rendering and IPC. No running desktop is touched.
use std::ffi::OsStr;
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::rc::Rc;
use std::time::{Duration, Instant};

use niri_config::Config;
use niri_ipc::{DockEdge, Reply};
use smithay::backend::allocator::Fourcc;
use smithay::backend::renderer::element::Element;
use smithay::output::Output;
use smithay::utils::{Logical, Rectangle, Scale, Transform};
use wayland_client::protocol::wl_surface::WlSurface;

use super::animations::set_time;
use super::client::ClientId;
use super::Fixture;
use crate::ipc::server::IpcServer;
use crate::layout::minimize_animation::AnimationTarget;
use crate::render_helpers::{render_to_vec, RenderCtx, RenderTarget};
use crate::utils::output_size;
use crate::window::mapped::MappedId;

fn setup(extra: &str) -> Fixture {
    let config = Config::parse_mem(&format!(r#"
        layout {{ gaps 0; border {{ off; }}; focus-ring {{ off; }}; shadow {{ off; }}; }}
        hotkey-overlay {{ skip-at-startup; }}
        animations {{ window-open {{ off; }}; window-minimize {{ duration-ms 1000; curve "linear"; }}; }}
        {extra}
    "#)).unwrap();
    let mut f = Fixture::with_config(config);
    f.niri_state().backend.headless().add_renderer().unwrap();
    f.add_output(1, (800, 600));
    f.add_output(2, (1200, 900));
    f
}

fn window(f: &mut Fixture, client: ClientId, color: [u32; 3]) -> (WlSurface, MappedId) {
    let w = f.client(client).create_window();
    let surface = w.surface.clone();
    w.set_title("same-title");
    w.xdg_toplevel.set_app_id("same-app".into());
    w.commit();
    f.double_roundtrip(client);
    let w = f.client(client).window(&surface);
    let buffer = w
        .spbm
        .create_u32_rgba_buffer(color[0], color[1], color[2], u32::MAX, &w.qh, ());
    w.surface.attach(Some(&buffer), 0, 0);
    w.set_size(240, 180);
    w.ack_last_and_commit();
    f.double_roundtrip(client);
    let id = f.niri().layout.focus().unwrap().id();
    (surface, id)
}

fn elements(f: &mut Fixture, output: &Output) -> Vec<Rectangle<f64, Logical>> {
    let scale = Scale::from(output.current_scale().fractional_scale());
    let mut result = Vec::new();
    f.niri()
        .layout
        .render_minimize_animations(output, RenderTarget::Output, &mut |element| {
            result.push(element.geometry(scale).to_f64().to_logical(scale));
        });
    result
}

fn red_pixels(f: &mut Fixture, output: &Output, target: RenderTarget) -> usize {
    let state = f.niri_state();
    state.niri.update_render_elements(Some(output));
    state
        .backend
        .with_primary_renderer(|renderer| {
            let elements = state.niri.render_to_vec(
                RenderCtx {
                    renderer,
                    target,
                    xray: None,
                },
                output,
                false,
            );
            let scale = Scale::from(output.current_scale().fractional_scale());
            let pixels = render_to_vec(
                renderer,
                output_size(output).to_physical_precise_round(scale),
                scale,
                Transform::Normal,
                Fourcc::Abgr8888,
                elements.into_iter().rev(),
            )
            .unwrap();
            pixels
                .chunks_exact(4)
                .filter(|p| p[0] > 180 && p[1] < 80 && p[2] < 80)
                .count()
        })
        .unwrap()
}

#[test]
fn egl_scale_endpoints_all_edges_and_live_handoff() {
    for edge in [
        DockEdge::Left,
        DockEdge::Right,
        DockEdge::Top,
        DockEdge::Bottom,
    ] {
        let mut f = setup("");
        let client = f.add_client();
        let (_, id) = window(&mut f, client, [u32::MAX, 0, 0]);
        f.niri_complete_animations();
        set_time(f.niri(), Duration::ZERO);
        let output = f.niri_output(1);
        let win = f.niri().find_window_by_id(id).unwrap();
        let target = match edge {
            DockEdge::Left => Rectangle::new((8., 240.).into(), (40., 40.).into()),
            DockEdge::Right => Rectangle::new((752., 240.).into(), (40., 40.).into()),
            DockEdge::Top => Rectangle::new((380., 8.).into(), (40., 40.).into()),
            DockEdge::Bottom => Rectangle::new((380., 552.).into(), (40., 40.).into()),
        };
        let owner = Rc::new(());
        f.niri().layout.set_window_animation_targets(
            &owner,
            vec![AnimationTarget {
                id: win.clone(),
                output: output.clone(),
                rect: target,
                edge,
                layer: None,
            }],
        );
        assert!(red_pixels(&mut f, &output, RenderTarget::Output) > 10000);
        assert!(f.niri_state().minimize_window(Some(id)));
        assert!(f.niri().layout.is_minimized(&win));
        let source = elements(&mut f, &output)[0];
        assert!(source.size.w >= 200. && source.size.h >= 150.);
        // Publishing a new endpoint while running cannot redirect this transition.
        f.niri().layout.set_window_animation_targets(
            &owner,
            vec![AnimationTarget {
                id: win.clone(),
                output: output.clone(),
                rect: Rectangle::from_size((1., 1.).into()),
                edge,
                layer: None,
            }],
        );
        set_time(f.niri(), Duration::from_millis(500));
        f.niri().advance_animations();
        let middle = elements(&mut f, &output)[0];
        assert!((middle.loc.x - (source.loc.x + target.loc.x) / 2.).abs() <= 1.);
        assert!((middle.loc.y - (source.loc.y + target.loc.y) / 2.).abs() <= 1.);
        assert!((middle.size.w - (source.size.w + target.size.w) / 2.).abs() <= 1.);
        let red = red_pixels(&mut f, &output, RenderTarget::Output);
        assert!(
            red > 1000,
            "{edge:?}: source={source:?}, middle={middle:?}, red={red}"
        );
        set_time(f.niri(), Duration::from_millis(1001));
        f.niri().advance_animations();
        assert!(elements(&mut f, &output).is_empty());
        assert_eq!(red_pixels(&mut f, &output, RenderTarget::Output), 0);
        f.niri().layout.set_window_animation_targets(
            &owner,
            vec![AnimationTarget {
                id: win.clone(),
                output: output.clone(),
                rect: target,
                edge,
                layer: None,
            }],
        );
        assert!(f.niri_state().restore_window(Some(id), Some(&output), true));
        // Exactly one snapshot; the real tile is hidden, including when its client stalls.
        assert_eq!(elements(&mut f, &output).len(), 1);
        assert_eq!(red_pixels(&mut f, &output, RenderTarget::Output), 0);
        set_time(f.niri(), Duration::from_millis(1501));
        f.niri().advance_animations();
        assert!(red_pixels(&mut f, &output, RenderTarget::Output) > 1000);
        set_time(f.niri(), Duration::from_millis(2102));
        f.niri().advance_animations();
        assert!(elements(&mut f, &output).is_empty());
        assert!(red_pixels(&mut f, &output, RenderTarget::Output) > 10000);
        assert_eq!(f.niri().find_window_by_id(id).unwrap(), win);
    }
}

#[test]
fn egl_cross_output_fractional_scale_restore_uses_new_layout() {
    let mut f = setup(r#"output "headless-2" { scale 1.5; transform "90"; }"#);
    let client = f.add_client();
    let (_, first) = window(&mut f, client, [u32::MAX, 0, 0]);
    let (_, second) = window(&mut f, client, [0, 0, u32::MAX]);
    f.niri_complete_animations();
    set_time(f.niri(), Duration::ZERO);
    let a = f.niri_output(1);
    let b = f.niri_output(2);
    let win = f.niri().find_window_by_id(first).unwrap();
    let owner = Rc::new(());
    let target = Rectangle::new((8., 120.).into(), (40., 40.).into());
    f.niri().layout.set_window_animation_targets(
        &owner,
        vec![AnimationTarget {
            id: win.clone(),
            output: b.clone(),
            rect: target,
            edge: DockEdge::Left,
            layer: None,
        }],
    );
    assert!(f.niri_state().minimize_window(Some(first)));
    assert_eq!(f.niri().layout.windows().count(), 1);
    assert!(f.niri().find_window_by_id(second).is_some());
    assert!(f.niri_state().restore_window(Some(first), Some(&b), true));
    assert!(elements(&mut f, &a).is_empty());
    assert_eq!(elements(&mut f, &b), vec![target]);
    assert!(f.niri_state().minimize_window(Some(first)));
    assert!(f.niri_state().restore_window(Some(first), Some(&b), true));
    assert_eq!(elements(&mut f, &b).len(), 1);
    f.niri_complete_animations();
    assert!(elements(&mut f, &b).is_empty());
    assert!(red_pixels(&mut f, &b, RenderTarget::Output) > 10000);
    let (_, output) = f
        .niri()
        .layout
        .find_window_and_output(win.toplevel().unwrap().wl_surface())
        .unwrap();
    assert_eq!(output, Some(&b));
    f.niri_state().minimize_window(Some(first));
    f.niri().remove_output(&b);
    assert!(elements(&mut f, &b).is_empty());
    assert!(f.niri_state().restore_window(Some(first), Some(&a), true));
    f.niri_complete_animations();
    assert!(red_pixels(&mut f, &a, RenderTarget::Output) > 10000);
}

#[test]
fn egl_capture_privacy_and_disabled_animation_fallback() {
    let mut f = setup(r#"window-rule { block-out-from "screen-capture"; }"#);
    let client = f.add_client();
    let (_, id) = window(&mut f, client, [u32::MAX, 0, 0]);
    f.niri_complete_animations();
    set_time(f.niri(), Duration::ZERO);
    let output = f.niri_output(1);
    f.niri_state().minimize_window(Some(id));
    set_time(f.niri(), Duration::from_millis(500));
    f.niri().advance_animations();
    assert!(red_pixels(&mut f, &output, RenderTarget::Output) > 1000);
    assert_eq!(red_pixels(&mut f, &output, RenderTarget::ScreenCapture), 0);
    f.niri().clock.set_complete_instantly(true);
    f.niri().advance_animations();
    assert!(f.niri_state().restore_window(Some(id), Some(&output), true));
    assert!(elements(&mut f, &output).is_empty());
    assert!(red_pixels(&mut f, &output, RenderTarget::Output) > 10000);
}

#[test]
fn egl_ipc_target_owner_disconnect_clears_hint() {
    let mut f = setup("");
    let client = f.add_client();
    let (_, id) = window(&mut f, client, [u32::MAX, 0, 0]);
    f.niri_complete_animations();
    set_time(f.niri(), Duration::ZERO);
    let name = format!("scale-test-{}-{}", std::process::id(), id.get());
    let server = IpcServer::start(&f.niri().event_loop, Some(OsStr::new(&name))).unwrap();
    let path = server.socket_path.clone().unwrap();
    f.niri().ipc_server = Some(server);
    f.niri_state().ipc_keyboard_layouts_changed();
    let mut stream = UnixStream::connect(path).unwrap();
    stream.set_nonblocking(true).unwrap();
    let request = serde_json::json!({"SetWindowAnimationTargets":{"targets":[{"id":id.get(),"output":"headless-1","rect":[8,240,40,40],"edge":"left"}]}});
    writeln!(stream, "{request}").unwrap();
    let mut reader = BufReader::new(stream);
    let deadline = Instant::now() + Duration::from_secs(5);
    let mut line = String::new();
    loop {
        f.state.server.dispatch();
        if reader.read_line(&mut line).is_ok() && line.ends_with('\n') {
            break;
        }
        assert!(Instant::now() < deadline);
    }
    let reply: Reply = serde_json::from_str(&line).unwrap();
    assert!(reply.is_ok());
    let output = f.niri_output(1);
    assert!(f.niri_state().minimize_window(Some(id)));
    set_time(f.niri(), Duration::from_millis(500));
    f.niri().advance_animations();
    let midpoint = elements(&mut f, &output)[0];
    assert!((midpoint.loc.x - 4.).abs() < 1.);
    assert!((midpoint.loc.y - 120.).abs() < 1.);
    f.niri_complete_animations();
    assert!(f.niri_state().restore_window(Some(id), Some(&output), true));
    f.niri_complete_animations();
    set_time(f.niri(), Duration::ZERO);
    drop(reader);
    // Dispatch the socket EOF and the cleanup idle callback.
    for _ in 0..10 {
        f.state.server.dispatch();
    }
    let output = f.niri_output(1);
    assert!(f.niri_state().minimize_window(Some(id)));
    set_time(f.niri(), Duration::from_millis(500));
    f.niri().advance_animations();
    let midpoint = elements(&mut f, &output)[0];
    assert!(
        midpoint.loc.y > 280.,
        "uses bottom fallback after publisher disconnect: {midpoint:?}"
    );
}

fn tile_rect(f: &mut Fixture, id: MappedId) -> Rectangle<f64, Logical> {
    f.niri()
        .layout
        .active_workspace()
        .unwrap()
        .tiles_with_render_positions()
        .find(|(tile, _, _)| tile.window().id() == id)
        .map(|(tile, pos, _)| Rectangle::new(pos, tile.animated_tile_size()))
        .unwrap()
}

#[test]
fn egl_tiled_gap_closes_and_restore_inserts_at_new_position() {
    let mut f = setup("");
    let client = f.add_client();
    let (_, first) = window(&mut f, client, [u32::MAX, 0, 0]);
    let (_, second) = window(&mut f, client, [0, 0, u32::MAX]);
    f.niri_complete_animations();
    let first_before = tile_rect(&mut f, first);
    let second_before = tile_rect(&mut f, second);
    assert!(first_before.loc.x < second_before.loc.x);
    f.niri_state().minimize_window(Some(first));
    f.niri_complete_animations();
    // Niri may keep the focused column stationary by adjusting the view offset.
    // Its logical position must still move into the removed column, with no placeholder.
    let position = f
        .niri()
        .layout
        .active_workspace()
        .unwrap()
        .tiles_with_ipc_layouts()
        .find(|(tile, _)| tile.window().id() == second)
        .unwrap()
        .1
        .pos_in_scrolling_layout;
    assert_eq!(position, Some((1, 1)));
    assert!(f.niri_state().restore_window(Some(first), None, true));
    f.niri_complete_animations();
    let restored = tile_rect(&mut f, first);
    assert!(restored.loc.x > tile_rect(&mut f, second).loc.x);
    assert_ne!(restored.loc, first_before.loc);
    let output = f.niri_output(1);
    assert!(red_pixels(&mut f, &output, RenderTarget::Output) > 10000);
}

#[test]
fn egl_floating_restore_and_output_change_reveal_live_window() {
    let mut f = setup("window-rule { open-floating true; }");
    let client = f.add_client();
    let (surface, id) = window(&mut f, client, [u32::MAX, 0, 0]);
    f.niri_complete_animations();
    let before = tile_rect(&mut f, id);
    let output = f.niri_output(1);
    f.niri_state().minimize_window(Some(id));
    f.niri_complete_animations();
    assert!(f.niri_state().restore_window(Some(id), None, true));
    assert_eq!(elements(&mut f, &output).len(), 1);
    output.change_current_state(None, Some(Transform::_180), None, None);
    f.niri().advance_animations();
    assert!(elements(&mut f, &output).is_empty());
    assert_eq!(tile_rect(&mut f, id), before);
    assert!(red_pixels(&mut f, &output, RenderTarget::Output) > 10000);
    f.niri_state().minimize_window(Some(id));
    assert_eq!(elements(&mut f, &output).len(), 1);
    // Destroy the actual client toplevel during a transition.
    f.client(client).window(&surface).xdg_toplevel.destroy();
    f.double_roundtrip(client);
    assert!(elements(&mut f, &output).is_empty());
    assert!(f.niri().find_window_by_id(id).is_none());
}

#[test]
fn target_validation_and_surface_lifetime() {
    use smithay::desktop::layer_map_for_output;
    use smithay::reexports::wayland_protocols_wlr::layer_shell::v1::client::zwlr_layer_shell_v1::Layer;
    use smithay::reexports::wayland_protocols_wlr::layer_shell::v1::client::zwlr_layer_surface_v1::Anchor;

    use super::client::{LayerConfigureProps, LayerMargin};
    use crate::layout::minimize_animation::target_rect;

    let mut f = setup("");
    let client = f.add_client();
    let layer = f.client(client).create_layer(None, Layer::Top, "test-dock");
    let surface = layer.surface.clone();
    layer.set_configure_props(LayerConfigureProps {
        anchor: Some(Anchor::Left | Anchor::Bottom),
        size: Some((200, 60)),
        margin: Some(LayerMargin {
            left: 40,
            bottom: 20,
            ..Default::default()
        }),
        ..Default::default()
    });
    layer.commit();
    f.roundtrip(client);
    let layer = f.client(client).layer(&surface);
    layer.attach_new_buffer();
    layer.set_size(200, 60);
    layer.ack_last_and_commit();
    f.double_roundtrip(client);
    let (_, id) = window(&mut f, client, [u32::MAX, 0, 0]);
    f.niri_complete_animations();
    set_time(f.niri(), Duration::ZERO);
    let output = f.niri_output(1);
    for rect in [
        [f64::NAN, 0., 1., 1.],
        [0., 0., 0., 1.],
        [0., 0., -1., 1.],
        [1e20, 0., 1., 1.],
    ] {
        assert!(target_rect(rect, &output, DockEdge::Left).is_none());
    }
    assert_eq!(
        target_rect([-50., 200., 40., 40.], &output, DockEdge::Left)
            .unwrap()
            .loc
            .x,
        0.
    );
    let layer = layer_map_for_output(&output)
        .layers()
        .find(|l| l.namespace() == "test-dock")
        .unwrap()
        .clone();
    let win = f.niri().find_window_by_id(id).unwrap();
    let owner = Rc::new(());
    f.niri().layout.set_window_animation_targets(
        &owner,
        vec![AnimationTarget {
            id: win,
            output: output.clone(),
            rect: Rectangle::new((8., 12.).into(), (40., 40.).into()),
            edge: DockEdge::Bottom,
            layer: Some(layer),
        }],
    );
    f.niri_state().minimize_window(Some(id));
    set_time(f.niri(), Duration::from_millis(500));
    f.niri().advance_animations();
    let midpoint = elements(&mut f, &output)[0];
    assert_eq!(midpoint.loc, (24., 266.).into());
    f.niri_complete_animations();
    // Unmapping the panel invalidates even a still-connected publisher's old hint.
    f.client(client).layer(&surface).surface.attach(None, 0, 0);
    f.client(client).layer(&surface).commit();
    f.double_roundtrip(client);
    assert!(f.niri_state().restore_window(Some(id), None, true));
    let fallback = elements(&mut f, &output)[0];
    assert!(
        fallback.loc.x > 350. && fallback.loc.y > 550.,
        "{fallback:?}"
    );
}

#[test]
fn egl_lock_cancels_snapshot_without_revealing_window() {
    use crate::niri::LockState;
    let mut f = setup("");
    let client = f.add_client();
    let (_, id) = window(&mut f, client, [u32::MAX, 0, 0]);
    f.niri_complete_animations();
    let output = f.niri_output(1);
    f.niri_state().minimize_window(Some(id));
    assert_eq!(elements(&mut f, &output).len(), 1);
    let c = f.client(client);
    let _lock = c
        .state
        .session_lock_manager
        .as_ref()
        .unwrap()
        .lock(&c.qh, ());
    f.double_roundtrip(client);
    if let LockState::WaitingForSurfaces {
        confirmation,
        deadline_token,
    } = std::mem::take(&mut f.niri().lock_state)
    {
        f.niri().event_loop.remove(deadline_token);
        f.niri().lock_state = LockState::Locking(confirmation);
    } else {
        panic!("expected a lock request");
    }
    f.niri().advance_animations();
    assert!(elements(&mut f, &output).is_empty());
    assert_eq!(red_pixels(&mut f, &output, RenderTarget::Output), 0);
    assert!(!f.niri_state().restore_window(Some(id), None, true));
}
