# Windows architecture and event guide

This guide describes the native Windows app and its platform-independent simulation in `core/`. Read the current implementation when changing behavior: constants and availability rules can change.

## Where to look

| Area | Files and responsibilities |
| --- | --- |
| Startup and UI | `windows/src/instance.rs` holds the shared `Local\Snowhush` exclusion lock for the app and installer; `app.rs` owns the tray/settings UI and sends commands to the snow thread. |
| Orchestration | `windows/src/snow.rs` owns the app-wide clock, enabled screens, event lottery and traffic schedules. It checks monitor visibility and dispatches events. |
| Per-monitor state | `windows/src/screen.rs` owns a `SnowSimulation`, overlays, trees, sculptures, gifts, a cone, animal visitors and a snowflake game for a monitor. It handles input, persistence and drawing. |
| Shared physics | `core/src/simulation.rs`, `layer.rs`, `snowmen.rs` and `simulation/` implement falling weather, drifts, supports and Halloween ghosts. Positions are display-local logical coordinates with positive Y upward. |
| Drawing | `windows/src/render.rs` sets up Direct3D, resources and uniforms; `windows/src/shaders.hlsl` draws sprites, piles and lighting. Tree images and the lantern are in `windows/assets/`. |
| Settings and saves | `windows/src/prefs.rs` reads and writes `HKCU\Software\VelvetFrost`. `sculpt.rs` and `gifts.rs` encode versioned per-monitor data. Gift decoding supports the legacy `snowhush-boomboxes-1` format; keep migrations when changing it. |
| Installer | `windows/src/bin/setup.rs` embeds the app named by `SNOWHUSH_APP`; `install.rs` installs for the current user and registers the uninstaller. Building is separate from installing. |

## Spontaneous events

`windows/src/events.rs` is the single probability table and clock for Windows spontaneous events. Every 180 seconds while the app is active, the app consumes one draw. The schedule uses the monotonic performance clock, is not persisted across app restarts and does not replay missed draws after a pause. The table uses 10,000 slots, so one slot is 0.01%:

| Outcome | Base slots | Base chance |
| --- | ---: | ---: |
| Cone | 1,200 | 12% |
| Fox | 1,600 | 16% |
| Gift | 400 | 4% |
| Golden snowflake game | 1,500 | 15% |
| Sleigh | 1,500 | 15% |
| Halloween ghost | 1,600 | 16% |
| Bullfinch | 2,000 | 20% |
| Christmas truck | 120 | 1.2% |
| Articulated road train | 40 | 0.4% |
| No event | 40 | 0.4% |

The base chances are unconditional, not a weighted choice among currently eligible events. `events::draw` reserves each event's slot range and returns no event for unused or reduced slots; `snow.rs` also discards a selected event if it is unavailable. Nothing is rerolled or redistributed. Thus actual frequencies can be lower, and an active event may overlap a later event. An eligible golden snowflake game has a nominal mean wait of 20 minutes with no legendary trees and about 22.22 minutes with one; this is an average, not a deadline.

For cones, the chosen monitor and species are sampled independently of the draw. The effective cone slots are `base * max(0, 4 - same_species) * 10 / (4 * (10 + total_trees))`, using integer arithmetic. For gifts, they are `base * 10 / (10 + gift_count)` for the chosen eligible monitor. Young and adult trees count; unsprouted cones do not. A four-tree species cap can make a chosen cone a no-event draw. Probability calculations belong in `events.rs`, not in individual object implementations.

After the draw, `snow.rs` applies the rules specific to each outcome:

- Cones require snow, a visible monitor and no active cone or golden snowflake chain. A collected cone plants a tree; the tree has separate growth and save logic in `sculpt.rs`.
- Foxes require falling snow, a visible monitor, a suitable drift and no other animal, snowplow, truck or sleigh. A failed placement consumes the draw.
- Bullfinches require falling snow, a visible monitor, an unobstructed snowy patch and no other animal, snowplow, truck or sleigh. They fly in, hop, peck and watch the cursor; a nearby cursor, lost support or ground traffic makes them fly away.
- Gifts require an adult tree with a clear place and an available reward type on the selected visible monitor. `gifts.rs` manages gift behavior and save formats, not event timing.
- The snowflake game requires snow, a suitable monitor, no active cone or chain, and at least one unowned legendary design. The `celestial-owner`, `ruby-owner` and `rose-owner` registry records protect each design when its monitor is disconnected; stored and pending trees count as owned, and stale owners can be cleared. Each owned legendary tree reduces the base game chance by 10% of its original value, adds one snowflake to the initial ten, and increases the base fall speed by 10%. Available celestial, ruby and rose designs have relative weights of 80, 10 and 10.
- Sleighs require the setting to be enabled, Snow alone, an eligible monitor and no current flight. Enabling the setting does not launch a flight by itself; it can appear on a subsequent shared draw. The flight may overlap ground traffic.
- Ghosts require Halloween, a visible monitor and a suitable drift; the core limits ghosts per simulation. Windows calls `set_external_ghost_spawning(true)` and asks the core to spawn on the shared draw. Other core hosts retain the core's autonomous ghost scheduler by default. Even a ghost seeking revenge waits for a later draw on Windows.

Trucks have an on/off setting and use the shared lottery: the ordinary truck has 120 slots and the articulated road train 40 out of 10,000 per draw. Unavailable draws are skipped. Snowplows retain their user-selected schedule; ground vehicles do not overlap, and a different type waits 90 seconds after the previous one exits. Selecting a snowplow interval can request a pass immediately. The old truck interval is migrated to the corresponding on/off choice.

To add an event, add its explicit slots to `events.rs` (keeping the total at or below 10,000), implement eligibility and one dispatch in `snow.rs`, and keep object behavior in its own module. Test empty and unavailable outcomes, competing events, monitor restrictions and pause/resume; do not add a second independent spawn clock.

## Rendering and interaction notes

- Tree physical bases follow the live snow surface in the shared snowman support physics; `sculpt.rs` rounds the render base to the pixel grid. Adult and young tree texture sampling crops transparent space at the bottom in `shaders.hlsl`. Keep lights, ornaments, hit testing and the physical base aligned if changing tree dimensions.
- A revealed lantern is a gift sprite (`mood` 18). The shader narrows it and draws a dim ember when disabled. Enabled lanterns supply warm light through the renderer's light slots and melt nearby visible snow via `screen.rs` and the radius/rate constants in `gifts.rs`; disabled or hidden lanterns do not melt snow. Do not confuse them with Halloween pumpkins or the cursor's candlelight.
- Cursor sweeping is implemented in `SnowSimulation::track_cursor`, `Air::carve` and `Air::emit`. Swept snow mass is either deposited or emitted as grains; additional cursor dust motes are cosmetic, capped by `MAX_MOTES` and remain visible when the falling-weather opacity is low. Lantern heat checks individual visible snow columns, including ledges beside the source, with a falloff toward the radius edge.
- `Screen` retains persistent objects per monitor. Legendary owners may refer to disconnected displays, so lack of a current screen is not proof that a tree was deleted. Saves can also refer to windows or tree slots that need to be resolved later.
- Screenshot-excluded desktop effects use non-topmost, click-through top-level overlays immediately above the desktop icons' host, because Windows cannot apply display affinity to the wallpaper layer's child windows. When screenshots are enabled, desktop effects return to the original child layer under the icons. Recreating the surfaces must preserve the existing simulation and object state. Windows display affinity applies to supported capture APIs and is not a guarantee against all capture methods.
- The tray Collection item opens `inventory.rs`, which displays snapshots from the snow thread and sends it physical drop coordinates. The screen converts those to local coordinates and chooses the first supporting window ledge or ground below; decorations can use a nearby tree slot. A tree is restored only if a free position is close to the drop. Objects can currently be dropped only on their original monitor. Stored trees and rewards remain in per-monitor saves (`snowhush-sculptures-7`, `-8` or `-9` depending on variants, and `snowhush-gifts-3`); older records still load. Store attached rewards before the tree, preserving each separately even if a later write fails. A stored reward still blocks that reward kind from spawning on its monitor. Disabled or disconnected monitors retain their saves but are not listed until re-enabled. The `inventory-preview` feature runs an isolated in-memory UI preview without loading real saves.

## Validation and preview builds

Start the checks from the repository root. Run Windows tests from `windows/` so Cargo loads that directory's configuration:

```powershell
cargo fmt --check --manifest-path core/Cargo.toml
cargo fmt --check --manifest-path windows/Cargo.toml
cargo test --locked --manifest-path core/Cargo.toml
Push-Location windows
cargo test --locked
Pop-Location
```

GPU and interactive-desktop tests in `screen.rs` and `render.rs` are ignored by default. Run a specific one from `windows/` on an interactive Windows desktop, for example `cargo test --locked gift_texture_renders_with_transparency_on_gpu -- --ignored`. Older registry-write tests are also ignored; do not run them without accounting for the user's settings. The installer isolation test runs in a child process with an isolated temporary registry subtree and folder.

The optional `cone-test-20s`, `fox-test-20s`, `bullfinch-test-20s`, `gift-test-20s` and `star-test-20s` features force the named outcome every 20 seconds for development while retaining eligibility checks. Use one feature at a time; if several are enabled, their order in `events::Schedule::poll` decides which wins. Never enable them for a release. See [RELEASING.md](../RELEASING.md) for Windows release builds.
