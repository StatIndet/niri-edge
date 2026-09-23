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
{"window_minimization":true,"window_minimization_animation":true,"window_minimization_effects":["scale","genie"]}
```

The socket request is `"Capabilities"`; its successful reply is
`{"Ok":{"Capabilities":{"window_minimization":true,"window_minimization_animation":true,"window_minimization_effects":["scale","genie"]}}}`. This read-only query
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

## Scale and Genie transitions

Minimization snapshots the visible tile before detaching it. The tiled layout
closes the gap using its normal movement rules. Restoration inserts the real
window first, then expands a snapshot toward that tile's current position; it does
not animate to the old column or old output. The live tile is hidden only while
its restore snapshot is running. Completion reveals the current client buffer
without waiting indefinitely for a configure acknowledgement. A late client may
therefore visibly resize after handoff. Rapid actions replace the previous
transition; they do not duplicate window ownership or snapshot overlays.

Both directions share the following configuration (280 ms, ease-out-cubic by
default). Global animation disabling and slowdown apply too:

```kdl
animations {
    window-minimize-effect "genie" // "scale" is the default
    window-minimize {
        duration-ms 280
        curve "ease-out-cubic"
        // off
    }
}
```

Scale keeps a rectangular snapshot; Genie pulls the near edge toward the icon,
then follows with the far edge while narrowing and bending individual sections
of the image. Left, right, top, and bottom share one direction-normalized shader;
restoration traverses the same shape in reverse. Shadows, borders, and the
snapshot's orientation travel with the sheet. A missing shader or an unusual
hint behind the source window falls back to Scale. Changing the setting affects
new operations; an active operation keeps its selected effect.

A shell checks `window_minimization_animation` separately from basic minimization,
and checks `window_minimization_effects` before offering effect selection. An
absent boolean means unavailable; an absent effects list means the shell cannot
edit the effect through this extension. Timing, slowdown and animation disabling
are independent of `window-minimize-effect`. A style-only include preserves them.

The shell maintains a dedicated IPC connection and publishes the complete set of
targets it owns as newline-delimited requests:

```json
{"SetWindowAnimationTargets":{"targets":[{"id":42,"output":"DP-1","rect":[24.5,8,48,48],"edge":"bottom","layer_namespace":"clavis-shell-dock"}]}}
```

The reply is `{"Ok":"Handled"}`. Keep the connection open; its hints are removed
on disconnect. An empty array clears them. Each request replaces this connection's
previous set, up to 1024 targets. Window IDs are exact IPC identities, including
minimized windows. Missing windows or outputs are ignored. Invalid rectangles
(nonfinite, nonpositive size, or any component beyond ±32768) reject the request
without replacing the previous set. Different publishers can own independent
sets; the most recently published matching target wins.

Without `layer_namespace`, the rectangle is **output-local logical** `[x,y,w,h]`.
With it, the rectangle is local to the unique mapped layer surface of that name
on that output. Niri resolves its actual origin, so the shell need not guess a
Wayland panel's global position. The surface identity is retained; unmapping or
destroying it invalidates the hint. The origin is resolved again when starting an
operation. Ambiguous or absent namespaces are ignored at publication. Do not
multiply coordinates by the output scale. Negative desktop output positions and
output rotation do not alter this local coordinate contract.

`edge` accepts `left`, `right`, `top`, or `bottom`. Partially visible rectangles
are clipped to the output; fully offscreen targets fall back to their indicated
edge. A missing valid target uses the bottom center of the operation's output.
A minimize uses its source output; a restore uses its destination output. Niri
never interpolates between two desktop output coordinate spaces. Shells should
publish current targets for every window represented by each Dock, independently
of clicks, including auto-hide/magnification changes. Each animation freezes its
resolved Dock endpoint. Later updates or disconnection affect only new operations.

Snapshots render in output space, outside the old tile/workspace clipping bounds,
while retaining capture block-out rules. They are released at completion,
window closure, output removal/reconfiguration, overview entry, or session lock.
The real window remains usable if animation preparation fails or animation is
disabled. No GPU snapshot cache is retained for the duration of minimization;
Dock thumbnail caching remains the shell's responsibility. The shared lifecycle
is shared by rectangle interpolation and Genie deformation.

## Clavis Settings integration

Clavis exposes **Genie / Scale** under **Settings → Dock → Behavior** when the
compositor advertises both effects. First-time **Set up** connects a managed
`clavis/minimize-animation.kdl` include through the existing configuration editor.
The fragment contains only the selected style:

```kdl
animations {
    window-minimize-effect "genie"
}
```

The niri configuration is the sole persistent source; Clavis does not duplicate
the preference in `dock.json` or rewrite window-open/window-close settings. Edits
use staged validation, atomic publication and stale-revision checks. A conflicting
later include produces an error instead of silently claiming success. Niri's
normal configuration watcher applies a successful edit. Setup/editing happen
only when requested in Settings; building this feature does not change the active
desktop's configuration.

## Scope and validation

The fork supports Scale and Genie for minimization and restoration.
Minimized-window overview previews, old-workspace restoration, and persistence of
minimized windows across compositor restarts are not implemented. Advertising protocol
support does not guarantee identical title-bar buttons in every application.

Headless GLES tests use actual Wayland client buffers and full-output pixel
readback to verify both effects in all four directions, snapshot/live handoff
(including a stalled client), tiled insertion, floating restoration, fractional scale, rotated and
multiple outputs, rapid actions, publisher/surface lifetime, cancellation, and
capture privacy. Asymmetric colored subsurfaces verify texture orientation; image
cross-sections distinguish Genie deformation from rigid scaling. Set
`NIRI_TEST_ANIMATION_FRAMES` to an existing directory when running
`tests::minimize_animation` to export a deterministic frame sequence for inspection.
Physical GPU and multi-monitor timing still require validation
in a separately launched session.

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
