use niri_config::Config;
use smithay::reexports::wayland_protocols::xdg::shell::client::xdg_toplevel::WmCapabilities;
use smithay::reexports::wayland_protocols_wlr::foreign_toplevel::v1::client::zwlr_foreign_toplevel_handle_v1::State as ForeignState;
use smithay::wayland::xdg_activation::XdgActivationHandler;
use wayland_client::protocol::wl_surface::WlSurface;

use super::client::ClientId;
use super::Fixture;
use crate::window::mapped::MappedId;

fn map_window(
    f: &mut Fixture,
    client: ClientId,
    minimize_before_map: bool,
) -> (WlSurface, MappedId) {
    let window = f.client(client).create_window();
    let surface = window.surface.clone();
    window.set_title("minimize test");
    if minimize_before_map {
        window.xdg_toplevel.set_minimized();
    }
    window.commit();
    f.double_roundtrip(client);
    assert!(f
        .client(client)
        .window(&surface)
        .wm_capabilities
        .contains(&WmCapabilities::Minimize));
    let window = f.client(client).window(&surface);
    window.attach_new_buffer();
    window.set_size(300, 200);
    window.ack_last_and_commit();
    f.double_roundtrip(client);
    let id = f.niri().layout.managed_windows().last().unwrap().1.id();
    (surface, id)
}

#[test]
fn xdg_minimize_preserves_identity_and_handles_hidden_commits() {
    let mut f = Fixture::new();
    f.add_output(1, (1280, 720));
    let client = f.add_client();
    let (surface, id) = map_window(&mut f, client, false);
    let window = f.niri().find_window_by_id(id).unwrap();
    f.client(client)
        .window(&surface)
        .xdg_toplevel
        .set_minimized();
    f.double_roundtrip(client);
    assert_eq!(f.niri().layout.windows().count(), 0);
    assert_eq!(f.niri().layout.managed_windows().count(), 1);
    assert!(f.niri().layout.is_minimized(&window));
    assert!(!f.client(client).state.foreign_toplevels[0].closed);

    let hidden = f.client(client).window(&surface);
    hidden.set_title("changed while minimized");
    hidden.xdg_toplevel.set_app_id("minimized.app".into());
    hidden.attach_new_buffer();
    hidden.set_size(350, 220);
    hidden.ack_last_and_commit();
    f.double_roundtrip(client);
    let handle = &f.client(client).state.foreign_toplevels[0];
    assert_eq!(handle.title, "changed while minimized");
    assert_eq!(handle.app_id, "minimized.app");
    assert!(handle.states.contains(&ForeignState::Minimized));
    assert!(!handle.states.contains(&ForeignState::Activated));

    assert!(f.niri_state().restore_window(Some(id), None, true));
    f.double_roundtrip(client);
    assert_eq!(f.niri().layout.windows().count(), 1);
    assert_eq!(f.niri().find_window_by_id(id).unwrap(), window);
    assert_eq!(f.client(client).state.foreign_toplevels.len(), 1);
    assert!(!f.client(client).state.foreign_toplevels[0].closed);
    assert!(!f.client(client).state.foreign_toplevels[0]
        .states
        .contains(&ForeignState::Minimized));

    f.client(client).state.foreign_toplevels[0].handle.close();
    f.double_roundtrip(client);
    assert!(f.client(client).window(&surface).close_requested);
    f.client(client).window(&surface).xdg_toplevel.destroy();
    f.double_roundtrip(client);
    assert_eq!(f.niri().layout.managed_windows().count(), 0);
    assert!(f.client(client).state.foreign_toplevels[0].closed);
}

#[test]
fn minimize_before_initial_map_is_remembered() {
    let mut f = Fixture::new();
    f.add_output(1, (1280, 720));
    let client = f.add_client();
    let (_, id) = map_window(&mut f, client, true);
    assert_eq!(f.niri().layout.windows().count(), 0);
    assert_eq!(f.niri().layout.managed_windows().count(), 1);
    assert!(f.niri_state().restore_window(Some(id), None, true));
    assert_eq!(f.niri().layout.windows().count(), 1);
}

#[test]
fn foreign_toplevel_unminimize_does_not_activate_and_activate_restores() {
    let mut f = Fixture::new();
    f.add_output(1, (1280, 720));
    let client = f.add_client();
    let (_, id) = map_window(&mut f, client, false);
    let first = f.niri().find_window_by_id(id).unwrap();
    let (_, second_id) = map_window(&mut f, client, false);
    let handle = f.client(client).state.foreign_toplevels[0].handle.clone();
    handle.set_minimized();
    f.double_roundtrip(client);
    assert!(f.niri().layout.is_minimized(&first));
    handle.unset_minimized();
    f.double_roundtrip(client);
    assert!(!f.niri().layout.is_minimized(&first));
    assert_eq!(f.niri().layout.focus().unwrap().id(), second_id);
    handle.set_minimized();
    f.double_roundtrip(client);
    let seat = f.client(client).state.seat.clone().unwrap();
    handle.activate(&seat);
    f.double_roundtrip(client);
    assert!(!f.niri().layout.is_minimized(&first));
    assert_eq!(f.niri().layout.focus().unwrap().id(), id);
    assert_eq!(f.client(client).state.foreign_toplevels.len(), 2);
}

#[test]
fn close_and_unmap_while_minimized_remove_live_records() {
    for unmap in [false, true] {
        let mut f = Fixture::new();
        f.add_output(1, (1280, 720));
        let client = f.add_client();
        let (surface, id) = map_window(&mut f, client, true);
        f.client(client).state.foreign_toplevels[0].handle.close();
        f.double_roundtrip(client);
        assert!(f.client(client).window(&surface).close_requested);
        let window = f.client(client).window(&surface);
        if unmap {
            window.attach_null();
            window.commit();
        } else {
            window.xdg_toplevel.destroy();
        }
        f.double_roundtrip(client);
        assert!(f.niri().find_window_by_id(id).is_none());
        assert!(!f.niri_state().restore_window(Some(id), None, true));
        assert!(f.client(client).state.foreign_toplevels[0].closed);
        assert_eq!(f.niri().unmapped_windows.len(), usize::from(unmap));
    }
}

#[test]
fn xdg_activation_policy_does_not_restore_ignored_or_urgent_windows() {
    for policy in ["ignore", "set-urgent", "focus"] {
        let config =
            Config::parse_mem(&format!("window-rule {{ on-xdg-activate \"{policy}\"; }}")).unwrap();
        let mut f = Fixture::with_config(config);
        f.add_output(1, (1280, 720));
        let client = f.add_client();
        let (_, id) = map_window(&mut f, client, true);
        let window = f.niri().find_window_by_id(id).unwrap();
        let surface = window.toplevel().unwrap().wl_surface().clone();
        let (token, data) = f.niri().activation_state.create_external_token(None);
        let (token, data) = (token.clone(), data.clone());
        f.niri_state().request_activation(token, data, surface);
        f.double_roundtrip(client);
        let mapped = f.niri().layout.managed_windows().next().unwrap().1;
        assert_eq!(mapped.is_minimized(), policy != "focus");
        assert_eq!(mapped.is_urgent(), policy == "set-urgent");
    }
}

#[test]
fn minimize_request_targets_its_surface_not_the_focused_window() {
    let mut f = Fixture::new();
    f.add_output(1, (1280, 720));
    let client = f.add_client();
    let (first_surface, first_id) = map_window(&mut f, client, false);
    let (_, focused_id) = map_window(&mut f, client, false);
    f.client(client)
        .window(&first_surface)
        .xdg_toplevel
        .set_minimized();
    f.double_roundtrip(client);
    assert_eq!(f.niri().layout.focus().unwrap().id(), focused_id);
    let window = f.niri().find_window_by_id(first_id).unwrap();
    assert!(f.niri().layout.is_minimized(&window));
}

#[test]
fn minimizing_dismisses_popups_and_rejects_new_hidden_popups() {
    let mut f = Fixture::new();
    f.add_output(1, (1280, 720));
    let client = f.add_client();
    let (surface, id) = map_window(&mut f, client, false);
    f.client(client).create_popup(&surface).surface.commit();
    f.double_roundtrip(client);
    assert!(!f.client(client).state.popups[0].done);
    assert!(f.niri_state().minimize_window(Some(id)));
    f.double_roundtrip(client);
    assert!(f.client(client).state.popups[0].done);
    f.client(client).state.popups[0].popup.destroy();
    f.client(client).create_popup(&surface).surface.commit();
    f.double_roundtrip(client);
    assert!(f.client(client).state.popups[1].done);
    assert!(f.niri().layout.focus().is_none());
    assert!(f.niri().popup_grab.is_none());
}

#[test]
fn locked_session_rejects_restore_and_activation() {
    use crate::niri::LockState;

    let mut f = Fixture::new();
    f.add_output(1, (1280, 720));
    let client = f.add_client();
    let (_, id) = map_window(&mut f, client, true);
    let window = f.niri().find_window_by_id(id).unwrap();
    let c = f.client(client);
    let _lock = c
        .state
        .session_lock_manager
        .as_ref()
        .unwrap()
        .lock(&c.qh, ());
    f.double_roundtrip(client);
    // A headless test has no scanout completion; move to the lock-rendering stage explicitly.
    if let LockState::WaitingForSurfaces {
        confirmation,
        deadline_token,
    } = std::mem::take(&mut f.niri().lock_state)
    {
        f.niri().event_loop.remove(deadline_token);
        f.niri().lock_state = LockState::Locking(confirmation);
    } else {
        panic!("expected a lock request waiting for headless surfaces");
    }
    assert!(f.niri().is_locked());
    assert!(!f.niri_state().restore_window(Some(id), None, true));
    f.niri_state().focus_window(&window);
    assert!(f.niri().layout.is_minimized(&window));
    let c = f.client(client);
    c.state.foreign_toplevels[0]
        .handle
        .activate(c.state.seat.as_ref().unwrap());
    f.double_roundtrip(client);
    assert!(f.niri().layout.is_minimized(&window));
    assert!(f.niri().layout.focus().is_none());
}

#[test]
fn transient_created_by_minimized_parent_stays_hidden() {
    let mut f = Fixture::new();
    f.add_output(1, (1280, 720));
    let client = f.add_client();
    let (parent_surface, _) = map_window(&mut f, client, true);
    let parent = f
        .client(client)
        .window(&parent_surface)
        .xdg_toplevel
        .clone();
    let child = f.client(client).create_window();
    let child_surface = child.surface.clone();
    child.set_parent(Some(&parent));
    child.commit();
    f.double_roundtrip(client);
    let child = f.client(client).window(&child_surface);
    child.attach_new_buffer();
    child.set_size(100, 100);
    child.ack_last_and_commit();
    f.double_roundtrip(client);
    assert_eq!(f.niri().layout.managed_windows().count(), 2);
    assert_eq!(f.niri().layout.windows().count(), 0);
    assert!(f.niri().layout.focus().is_none());
    assert!(f
        .niri()
        .layout
        .managed_windows()
        .all(|(_, mapped)| mapped.is_minimized()));
    assert!(f.niri_state().restore_window(None, None, true));
    assert_eq!(f.niri().layout.windows().count(), 1);
}

#[test]
fn expired_activation_token_does_not_restore_window() {
    let mut f = Fixture::new();
    f.add_output(1, (1280, 720));
    let client = f.add_client();
    let (_, id) = map_window(&mut f, client, true);
    let window = f.niri().find_window_by_id(id).unwrap();
    let surface = window.toplevel().unwrap().wl_surface().clone();
    let (token, data) = f.niri().activation_state.create_external_token(None);
    let (token, mut data) = (token.clone(), data.clone());
    data.timestamp -= std::time::Duration::from_secs(120);
    f.niri_state().request_activation(token, data, surface);
    f.double_roundtrip(client);
    assert!(f.niri().layout.is_minimized(&window));
}

#[test]
fn minimized_window_survives_output_removal_and_failed_restore() {
    let mut f = Fixture::new();
    f.add_output(1, (1280, 720));
    let output = f.niri_output(1);
    let client = f.add_client();
    let (_, id) = map_window(&mut f, client, true);
    f.niri().remove_output(&output);
    f.double_roundtrip(client);
    assert!(!f.niri_state().restore_window(None, None, true));
    assert!(!f.client(client).state.foreign_toplevels[0].closed);
    assert_eq!(f.niri().layout.managed_windows().next().unwrap().1.id(), id);
    f.add_output(2, (800, 600));
    assert!(f.niri_state().restore_window(None, None, true));
    f.double_roundtrip(client);
    assert_eq!(f.niri().layout.windows().next().unwrap().1.id(), id);
    assert_eq!(f.client(client).state.foreign_toplevels.len(), 1);
}

#[test]
fn hidden_new_subsurface_commits_update_the_live_window() {
    let mut f = Fixture::new();
    f.add_output(1, (1280, 720));
    let client = f.add_client();
    let (surface, id) = map_window(&mut f, client, true);
    let window = f.niri().find_window_by_id(id).unwrap();
    let initial_bbox = window.bbox();
    let c = f.client(client);
    let child = c
        .state
        .compositor
        .as_ref()
        .unwrap()
        .create_surface(&c.qh, ());
    let subsurface =
        c.state
            .subcompositor
            .as_ref()
            .unwrap()
            .get_subsurface(&child, &surface, &c.qh, ());
    subsurface.set_desync();
    subsurface.set_position(initial_bbox.size.w + 100, 0);
    let buffer = c
        .state
        .spbm
        .as_ref()
        .unwrap()
        .create_u32_rgba_buffer(0, 0, 0, 0, &c.qh, ());
    child.attach(Some(&buffer), 0, 0);
    child.commit();
    c.window(&surface).commit();
    f.double_roundtrip(client);
    assert!(window.bbox().size.w > initial_bbox.size.w);
    assert!(f.niri().layout.is_minimized(&window));
    assert!(f.niri_state().restore_window(Some(id), None, true));
    f.double_roundtrip(client);
    assert_eq!(f.niri().find_window_by_id(id).unwrap(), window);
    assert!(window.bbox().size.w > initial_bbox.size.w);
    subsurface.destroy();
    child.destroy();
    f.client(client).window(&surface).ack_last_and_commit();
    f.double_roundtrip(client);
    assert_eq!(window.bbox().size.w, initial_bbox.size.w);
}

#[test]
fn minimized_window_stops_inhibiting_idle_until_visible_again() {
    use smithay::backend::renderer::element::{
        default_primary_scanout_output_compare, RenderElementPresentationState, RenderElementState,
        RenderElementStates,
    };
    use smithay::desktop::utils::update_surface_primary_scanout_output;
    use smithay::wayland::compositor::with_states;

    let mut f = Fixture::new();
    f.add_output(1, (1280, 720));
    let output = f.niri_output(1);
    let client = f.add_client();
    let (surface, id) = map_window(&mut f, client, false);
    let c = f.client(client);
    let _inhibitor = c
        .state
        .idle_inhibit_manager
        .as_ref()
        .unwrap()
        .create_inhibitor(&surface, &c.qh, ());
    f.double_roundtrip(client);
    let window = f.niri().find_window_by_id(id).unwrap();
    let server_surface = window.toplevel().unwrap().wl_surface();
    // Headless outputs do not scan out. Supply the same visibility report a renderer provides.
    let mut report = RenderElementStates::default();
    report.states.insert(
        server_surface.into(),
        RenderElementState {
            visible_area: 1,
            presentation_state: RenderElementPresentationState::Rendering { reason: None },
            needs_capture: false,
        },
    );
    let report_visible = || {
        with_states(server_surface, |states| {
            update_surface_primary_scanout_output(
                server_surface,
                &output,
                states,
                None,
                &report,
                default_primary_scanout_output_compare,
            );
        });
    };
    report_visible();
    f.niri().refresh_idle_inhibit();
    assert!(f.niri().idle_notifier_state.is_inhibited());
    assert!(f.niri_state().minimize_window(Some(id)));
    f.niri().refresh_idle_inhibit();
    assert!(!f.niri().idle_notifier_state.is_inhibited());
    assert!(f.niri_state().restore_window(Some(id), None, true));
    report_visible();
    f.niri().refresh_idle_inhibit();
    assert!(f.niri().idle_notifier_state.is_inhibited());
}

#[test]
fn minimizing_cancels_only_its_pointer_move_and_resize_grabs() {
    use smithay::backend::input::InputTime;
    use smithay::input::pointer::{Focus, GrabStartData, MotionEvent};
    use smithay::utils::SERIAL_COUNTER;

    use crate::input::move_grab::MoveGrab;
    use crate::input::resize_grab::ResizeGrab;
    use crate::input::AnyStartData;
    use crate::utils::ResizeEdge;

    for resize in [false, true] {
        for minimize_target in [false, true] {
            let mut f = Fixture::new();
            f.add_output(1, (1280, 720));
            let client = f.add_client();
            let (_, first_id) = map_window(&mut f, client, false);
            let (_, second_id) = map_window(&mut f, client, false);
            let window = f.niri().find_window_by_id(first_id).unwrap();
            // Real compositor move/resize entry points activate the grabbed window first.
            f.niri().layout.activate_window(&window);
            f.niri_state().update_keyboard_focus();
            let pointer = f.niri().seat.get_pointer().unwrap();
            pointer.motion(
                f.niri_state(),
                None,
                &MotionEvent {
                    location: (100., 100.).into(),
                    serial: SERIAL_COUNTER.next_serial(),
                    time: InputTime::now(),
                },
            );
            let start = AnyStartData::Pointer(GrabStartData {
                focus: None,
                button: 0x110,
                location: (100., 100.).into(),
            });
            if resize {
                assert!(f
                    .niri()
                    .layout
                    .interactive_resize_begin(window.clone(), ResizeEdge::RIGHT));
                let grab = ResizeGrab::new(start, window);
                pointer.set_grab(
                    f.niri_state(),
                    grab,
                    SERIAL_COUNTER.next_serial(),
                    Focus::Clear,
                );
            } else {
                let grab = MoveGrab::new(f.niri_state(), start, window, true, None).unwrap();
                pointer.set_grab(
                    f.niri_state(),
                    grab,
                    SERIAL_COUNTER.next_serial(),
                    Focus::Clear,
                );
            }
            assert!(pointer.is_grabbed());
            let target = if minimize_target { first_id } else { second_id };
            assert!(f.niri_state().minimize_window(Some(target)));
            assert_eq!(
                pointer.is_grabbed(),
                !minimize_target,
                "resize={resize}, minimize_target={minimize_target}"
            );
            pointer.unset_grab(
                f.niri_state(),
                SERIAL_COUNTER.next_serial(),
                InputTime::now(),
            );
        }
    }
}

#[test]
fn minimizing_cancels_touch_overview_grab_without_client_focus() {
    use smithay::input::touch::GrabStartData;
    use smithay::utils::SERIAL_COUNTER;

    use crate::input::touch_overview_grab::TouchOverviewGrab;
    use crate::input::AnyStartData;

    let mut f = Fixture::new();
    f.add_output(1, (1280, 720));
    let client = f.add_client();
    let (_, first_id) = map_window(&mut f, client, false);
    let (_, second_id) = map_window(&mut f, client, false);
    let window = f.niri().find_window_by_id(first_id).unwrap();
    let output = f.niri_output(1);
    let workspace = f.niri().layout.active_workspace().unwrap().id();
    let touch = f.niri().seat.add_touch();
    let start = AnyStartData::Touch(GrabStartData {
        focus: None,
        slot: Some(0).into(),
        location: (100., 100.).into(),
    });
    let grab = TouchOverviewGrab::new(
        start,
        std::time::Duration::ZERO,
        output,
        (100., 100.).into(),
        Some(workspace),
        true,
        Some(window),
    );
    touch.set_grab(f.niri_state(), grab, SERIAL_COUNTER.next_serial());
    assert!(f.niri_state().minimize_window(Some(second_id)));
    assert!(touch.is_grabbed());
    assert!(f.niri_state().minimize_window(Some(first_id)));
    assert!(!touch.is_grabbed());
}

#[test]
fn minimizing_releases_tablet_grabs_and_refreshes_hover_focus() {
    use smithay::backend::input::{
        InputTime, TabletToolCapabilities, TabletToolDescriptor, TabletToolType,
    };
    use smithay::input::pointer::Focus;
    use smithay::input::tablet::{tool, TabletDescriptor, TabletSeatTrait};
    use smithay::utils::SERIAL_COUNTER;

    use crate::input::move_grab::MoveGrab;
    use crate::input::AnyStartData;

    // Hover, an implicit pen-down grab, and a compositor move with no client focus.
    for mode in 0..3 {
        let mut f = Fixture::new();
        f.add_output(1, (1280, 720));
        let client = f.add_client();
        let (_, id) = map_window(&mut f, client, false);
        f.niri_complete_animations();
        let window = f.niri().find_window_by_id(id).unwrap();
        let surface = window.toplevel().unwrap().wl_surface().clone();
        let origin = f.niri().layout.window_render_location(&surface).unwrap();
        let location = origin + smithay::utils::Point::from((20., 20.));
        assert_eq!(
            f.niri().contents_under(location).surface.unwrap().0,
            surface
        );
        let tablet_seat = f.niri().seat.tablet_seat();
        let tablet = tablet_seat.add_tablet(&TabletDescriptor {
            name: "Minimization test tablet".into(),
            usb_id: None,
            syspath: None,
        });
        let tool = tablet_seat.add_tool(&TabletToolDescriptor {
            tool_type: TabletToolType::Pen,
            hardware_serial: 1,
            hardware_id_wacom: 0,
            capabilities: TabletToolCapabilities::empty(),
        });
        let time = InputTime::now();
        tool.proximity_in(
            f.niri_state(),
            Some((surface.clone(), origin)),
            tablet,
            &tool::ProximityInEvent {
                location,
                axis: None,
                serial: SERIAL_COUNTER.next_serial(),
                time,
            },
        );
        if mode == 1 {
            tool.down(
                f.niri_state(),
                &tool::DownEvent {
                    serial: SERIAL_COUNTER.next_serial(),
                    time,
                },
            );
        } else if mode == 2 {
            let start = AnyStartData::TabletTool(tool::GrabStartData {
                focus: None,
                trigger: tool::GrabTrigger::Tip,
                location,
            });
            let grab = MoveGrab::new(f.niri_state(), start, window, true, None).unwrap();
            tool.set_grab(
                f.niri_state(),
                grab,
                time,
                SERIAL_COUNTER.next_serial(),
                Focus::Clear,
            );
        }
        assert_eq!(tool.is_grabbed(), mode != 0);
        assert!(f.niri_state().minimize_window(Some(id)));
        assert!(!tool.is_grabbed());
        // No motion event is injected between hiding and the next input event. The implicit
        // grab captures the tool's current focus, which must already exclude the hidden client.
        tool.down(
            f.niri_state(),
            &tool::DownEvent {
                serial: SERIAL_COUNTER.next_serial(),
                time,
            },
        );
        assert!(tool.grab_start_data().unwrap().focus.is_none());
    }
}
