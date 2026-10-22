# tests/tools - RocksVision GUI driver
`gui-drive.py` drives the real app with CGEvents (python3 + pyobjc Quartz/AppKit; no other deps) and captures screens.
Prereqs: pyobjc present (checked: `import Quartz, AppKit` works); RocksVision running AND frontmost; the terminal needs
Screen Recording (screencapture) and, for `winsize`/`winfo`/shot geometry, Accessibility (System Events).
## Run
`SHOT_DIR=<evidence dir> python3 -I tests/tools/gui-drive.py [--set K=V]... <cmd>` or `... script tests/tools/scripts/NAME.txt`
`--check` prints the expanded script and verifies placeholders only (no events). `{{NAME}}` placeholders (screen
coordinates etc.) must be given with `--set NAME=VALUE`, or `set NAME VALUE` lines; unresolved ones abort before any event.
Built-ins: `{{FIXTURES}}` (tests/fixtures), `{{EXAMPLES}}` (examples/), `{{SHOT_DIR}}`. Options (`--set`, `--check`) go BEFORE `script FILE`.
Scripts (scripts/*.txt; every coordinate is a placeholder resolved from a prior screenshot): clipboard, clipboard_binary, shift_click, reset_view, windows,
themes_capture, harness_slowjob; Phase B acceptance (v3): resize_sequence, collapse_restore, reset_view_contract, search_enter, derived_space, diagnosis,
hex_sweep, themes_capture_v2. Each header lists its placeholders and the expected result; each ends with a `note PASS: ...` line.
Check all placeholders without sending events: `--set` every name to 1 and add `--check`.
## Commands (coordinates = screen points, origin top-left of main display)
click/rclick/dclick X Y [mods] | shift_click X Y | move|hover X Y | drag X1 Y1 X2 Y2 | type TEXT | key NAME [cmd shift alt ctrl]
shot NAME | crop NAME X Y W H | note TEXT | sleep S | front | winfo | winsize W H | clip | clipclear | clipsave NAME | release_mods
- `crop NAME X Y W H` (NEW) saves a FULL-RESOLUTION region of the last `shot` in the same run as `NAME.png`; X Y W H are screen POINTS (same
  coordinates as click), converted with pixel = point * (full_png_width / screen_points_width) (2.0 on Retina); aborts if there is no earlier shot or
  the rect leaves the image. Verified with a synthetic image (a 20x20 red square at pixel 50,40 cropped from points 25,20 10x10 -> all red).
- `note TEXT` (NEW) writes `NOTE TEXT` to action.log (step markers, expected values); sends no event.
- Minimum window size: `winsize 200 200` is clamped by the app; `winfo` afterwards logs the size actually reached.
- `winsize` uses System Events; if refused it aborts - fallback: `winfo`, then `drag` the bottom-right corner by the delta.
- `front` activates RocksVision and aborts if it is still not frontmost.
## Safety
- `--pid N` (before `script`; or env `ROCKSVISION_DRIVER_PID=N`): the guard compares the frontmost app's unix PID (AppKit) with N before every event, `front` activates that PID and `winfo`/`winsize` address it; without it the guard falls back to the app NAME.
- EXACTLY ONE RocksVision instance may be driven; without `--pid` the frontmost guard only checks the app NAME, so a second instance (e.g. the user's) can receive events.
  Start your own build, record its PID, drive only it, and NEVER kill/signal any process you did not start (report other PIDs to the coordinator).
- Frontmost guard: before every posted event the frontmost app must be RocksVision, else abort (no event sent).
- Modifiers: explicit key down/up around modified keys/clicks; flags are 0 on every typed char, move and plain click;
  `release_mods` (and automatic release at script end, on error, on abort, at exit) sends key-up for cmd/shift/alt/ctrl.
## Evidence conventions
- `SHOT_DIR` receives `NAME.png` (full) and `NAME_s.png` (<=1600 px wide), `NAME.txt` (clipsave) and `action.log`
  (every command with timestamp and frontmost app; each `shot` logs window pos/size and the scale factor).
- Two pixel scales: `_s.png` -> points: `screen_pt = _s_pixel * factor` (factor = points/_s width, in action.log, e.g. 1.08); full `NAME.png` -> points: `screen_pt = pixel / (png_width / screen_points_width)` (2.0 on Retina; crop does this for you).
- Screenshot pixel -> screen points (`_s` image): `screen_pt = _s_pixel * (screen_points_width / _s_image_width)`; the factor is in
  the `shot` line of action.log (e.g. 1728 pt / 1600 px = 1.08). Window origin: `osascript -e 'tell application
  "System Events" to tell process "RocksVision" to get {position, size} of window 1'` (= `winfo`).
- Use `clipclear` before each copy so a stale clipboard cannot pass; compare `clipsave` files to `expected_clipboard.md` (v3: byte-level hex/SHA-256 for NUL keys, non-UTF-8 values and the 64 KiB cap; check with `xxd` and `shasum -a 256`).
- RULE: a screenshot never proves an interaction without the matching action.log lines (command, timestamp, front app).
