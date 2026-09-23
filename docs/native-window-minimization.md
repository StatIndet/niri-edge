# Native window minimization (fork extension)

This feature belongs to **StatIndet/niri-edge**. It is not an upstream niri feature.
It works without Quickshell, a Dock, or another desktop shell.

## Behavior

Minimization hides a window without closing it or disconnecting its client. The
same managed window and Window ID survive. Applications can continue playing
music, downloading, and updating their state while hidden.

Minimized windows are kept outside the ordinary tiled and floating layouts. They
do not reserve a column, tab, or workspace, participate in directional focus, or
receive normal pointer, keyboard, or touch input. Empty columns and workspaces
are cleaned up normally.

Restoration targets the active output's current workspace at the time of the
operation. An optional output selects that output's current workspace. It does
not remember the old workspace or reapply initial `open-on-workspace` or
`open-on-output` rules. A tiled window uses the normal insertion position, not
its old column or neighbors.

Floating windows retain their floating mode and normal logical size. Their
center's relative position within the available work area is transferred to the
target output, then constrained to keep the window usable. This uses logical
coordinates, including work-area offsets and fractional output scale. A smaller
target may constrain the size; a client's minimum size still takes precedence.
Fullscreen and maximized state use the existing window state and sizing rules
for the destination output.

Without an ID, restoration picks the most recently successfully minimized live
window. Repeated minimization does not change that order. Repeated restoration
does not insert a second copy. An unavailable destination or lack of outputs
leaves the window minimized so that it can be restored later. A minimized window
can still be closed by ID. Explicit `focus-window --id` restores it before
focusing it; normal activation policy and session-lock input protection still
apply. Restore and activation requests are ignored while the session is locked.

Popups are dismissed when their window is minimized, and new popups from hidden
windows are rejected. A new transient toplevel whose parent is minimized starts
minimized too, with its own ID and independent restore order. Existing transient
toplevels remain independent; this extension does not implement window groups.

Existing single-window capture sessions retain their identity while hidden but
receive no new window images; pending image-copy capture frames fail until the
window becomes visible again. Output captures keep their existing privacy and
lock-screen behavior.

## Commands and bindings

Run these with the new fork build after switching to it:

```sh
niri msg action minimize-window
niri msg action minimize-window --id 42
niri msg action restore-window
niri msg action restore-window --id 42
niri msg action restore-window --id 42 --output DP-1
niri msg --json windows
niri msg --json capabilities
```

Suggested optional KDL bindings, with no change to the default bindings:

```kdl
binds {
    Mod+M { minimize-window; }
    Mod+Shift+M { restore-window; }
    Mod+Ctrl+M { restore-window output="DP-1"; }
}
```

Both actions also accept an `id=42` KDL property. `restore-window` focuses the
restored window. These commands and bindings provide recovery even when no
desktop shell is running.

## IPC contract

`niri msg --json capabilities` returns:

```json
{"window_minimization":true}
```

The socket request is `"Capabilities"`; its successful reply is
`{"Ok":{"Capabilities":{"window_minimization":true}}}`. This read-only query
does not change focus or window state. Older compositors may reject the query;
clients should treat that as an unavailable extension. The existing `Version`
reply is unchanged.

Action requests use the usual IPC envelope:

```json
{"Action":{"MinimizeWindow":{"id":42}}}
{"Action":{"RestoreWindow":{"id":42,"output":"DP-1"}}}
```

Omit an optional field or set it to `null` to use its default. The normal action
reply is `{"Ok":"Handled"}`; it acknowledges dispatch, as for other niri
actions, rather than promising that a matching window or destination existed.

Window queries and the event stream include minimized windows with their
unchanged ID, title, app ID, and other metadata. `is_minimized` explicitly
identifies them and defaults to `false` when decoding older snapshots. A
minimized window has `is_focused: false`, `workspace_id: null`,
`layout.pos_in_scrolling_layout: null`, and
`layout.tile_pos_in_workspace_view: null`. Required size fields remain present.
Do not infer minimization from a missing workspace alone.

State changes use existing `WindowOpenedOrChanged` events. Reconnecting yields
the same window state through `WindowsChanged`; minimization does not emit a
fake `WindowClosed`, and restoration does not create a new window identity.

The compositor advertises xdg-shell minimization and supports foreign-toplevel
minimize, unminimize, activate, and close requests. Foreign-toplevel handles stay
alive while minimized. Unminimizing alone does not request activation; explicit
activation and the native restore action do.

## Scope and validation

Phase one provides immediate hiding and restoration. It does not include Dock
integration, Genie animation, minimized-window overview previews, old-workspace
restoration, or persistence across compositor restarts. Advertising protocol
support does not guarantee identical title-bar buttons in every application.

Contract tests cover CLI and KDL parsing, IPC wire formats, compatibility with
older window snapshots, and equivalence between incremental window events and
reconnected snapshots. Layout and real Wayland client/server tests cover the
compositor behavior. The CI test job also builds the CLI and runs it against a
separate headless Wayland server, including the actual IPC event stream and a
create/minimize/query/restore/close cycle. To run that check locally:

```sh
cargo build
NIRI_TEST_BINARY="$PWD/target/debug/niri" cargo test --lib \
    tests::minimize_ipc::built_cli_minimize_restore_close -- --ignored --exact
```

Local nested-instance smoke tests covered native Wayland Zenity and Xwayland
xmessage, each using the test instance's own `NIRI_SOCKET`, temporary config,
and display. These checks do not replace the running desktop compositor.
Physical multi-monitor hardware, touch/tablet devices, and application-specific
behavior beyond those clients remain unverified. The pull request records the
complete checks and results for the delivered commit.

Protocol integration was reviewed alongside
[LengineerC/niri's minimization implementation](https://github.com/LengineerC/niri/commit/1b1e5c994ce3b820b339d88ffbf9939f42960ffc).
This fork uses a separate ownership collection rather than preserving old column
placeholders.
