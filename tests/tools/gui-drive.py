#!/usr/bin/env python3
"""RocksVision GUI driver: CGEvent mouse/keyboard + screencapture (python3 + pyobjc only).
Usage: gui-drive.py [--set K=V ...] [--check] <cmd...> | script FILE   (see tests/tools/README.md)
Ops: front winsize winfo click rclick dclick shift_click move|hover drag wheel(X Y DX DY [mods]) type key shot crop clip* note sleep.
Coordinates are SCREEN POINTS. Every command is appended to $SHOT_DIR/action.log.
No event is posted unless RocksVision is the frontmost app; held modifiers are always released."""
import sys, time, subprocess, os, re, atexit
import Quartz as Q

APP = os.environ.get("APP_NAME", "RocksVision")
SHOT_DIR = os.environ.get("SHOT_DIR", ".")
HERE = os.path.dirname(os.path.abspath(__file__))

KEYS = {"return": 36, "enter": 76, "esc": 53, "tab": 48, "space": 49, "delete": 51, "fwddelete": 117,
        "left": 123, "right": 124, "down": 125, "up": 126, "home": 115, "end": 119, "pgup": 116, "pgdn": 121,
        "f1": 122, "f2": 120, "f3": 99, "f4": 118, "f5": 96,
        "equal": 24, "minus": 27, "comma": 43, "period": 47, "slash": 44, "lbracket": 33, "rbracket": 30}
for _c, _k in zip("asdfhgzxcvbqweryt", [0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 11, 12, 13, 14, 15, 16, 17]): KEYS[_c] = _k
for _c, _k in zip("uiopljknm", [32, 34, 31, 35, 37, 38, 40, 45, 46]): KEYS[_c] = _k
for _c, _k in zip("1234567890", [18, 19, 20, 21, 23, 22, 26, 28, 25, 29]): KEYS[_c] = _k
MODS = {"cmd": Q.kCGEventFlagMaskCommand, "shift": Q.kCGEventFlagMaskShift, "alt": Q.kCGEventFlagMaskAlternate,
        "ctrl": Q.kCGEventFlagMaskControl}
MODKEY = {"cmd": 55, "shift": 56, "alt": 58, "ctrl": 59}
HELD = set()  # modifiers this script has pressed and not yet released

def log(msg):
    line = time.strftime("%H:%M:%S ") + msg
    print(line, flush=True)
    try:
        os.makedirs(SHOT_DIR, exist_ok=True)
        with open(os.path.join(SHOT_DIR, "action.log"), "a") as f: f.write(line + "\n")
    except OSError: pass

# --pid N / ROCKSVISION_DRIVER_PID: the guard compares the frontmost app's unix PID with N (exact
# instance); without it, the app NAME prefix is compared (fallback; cannot tell two instances apart).
PID = [int(os.environ["ROCKSVISION_DRIVER_PID"])] if os.environ.get("ROCKSVISION_DRIVER_PID") else [None]

def frontmost_app():
    from AppKit import NSWorkspace
    _pump()
    return NSWorkspace.sharedWorkspace().frontmostApplication()

def frontmost_name():
    app = frontmost_app()
    if not app: return ""
    return "%s (pid %d)" % (app.localizedName() or "", app.processIdentifier()) if PID[0] else (app.localizedName() or "")

def frontmost_ok():
    app = frontmost_app()
    if not app: return False
    if PID[0] is not None: return int(app.processIdentifier()) == PID[0]
    return (app.localizedName() or "").lower().startswith(APP.lower())

def target_app():
    """NSRunningApplication of --pid, or None without --pid. Aborts if that PID is not a running app."""
    if PID[0] is None: return None
    from AppKit import NSRunningApplication
    app = NSRunningApplication.runningApplicationWithProcessIdentifier_(PID[0])
    if app is None or app.isTerminated(): raise SystemExit("ABORT: no running application with pid %d" % PID[0])
    return app

def se_process():
    """System Events process specifier: by unix id with --pid, else by name."""
    return ("first process whose unix id is %d" % PID[0]) if PID[0] is not None else ('process "%s"' % APP)

def raw_post(e): Q.CGEventPost(Q.kCGHIDEventTap, e)

def _pump():
    """Let AppKit deliver pending activation notifications: NSWorkspace.frontmostApplication() is cached
    per process and stays stale in a script without a running run loop (the 0026 ChatGPT incident)."""
    from Foundation import NSRunLoop, NSDate
    NSRunLoop.currentRunLoop().runUntilDate_(NSDate.dateWithTimeIntervalSinceNow_(0.05))

def window_owner_at(x=None, y=None):
    """PID owning the topmost normal (layer 0, > 50x50, on-screen) window, or, with x/y, the topmost such
    window containing that screen point. Straight from the window server (front-to-back order), no caching."""
    infos = Q.CGWindowListCopyWindowInfo(Q.kCGWindowListOptionOnScreenOnly | Q.kCGWindowListExcludeDesktopElements, Q.kCGNullWindowID) or []
    for w in infos:
        if w.get("kCGWindowLayer", 1) != 0 or w.get("kCGWindowAlpha", 1) <= 0: continue
        b = w.get("kCGWindowBounds") or {}
        if b.get("Width", 0) < 50 or b.get("Height", 0) < 50: continue
        if x is not None and not (b["X"] <= x < b["X"] + b["Width"] and b["Y"] <= y < b["Y"] + b["Height"]): continue
        return int(w.get("kCGWindowOwnerPID", -1))
    return None

def guard_state(pid, x=None, y=None):
    """(ok, detail): fresh checks that `pid` is the active application AND owns the topmost window (at x,y for
    mouse events). Both must agree; nothing is cached between calls."""
    _pump()
    app = frontmost_app()
    active = int(app.processIdentifier()) if app else None
    # The menu bar (y < 24 pt) belongs to the ACTIVE app, not to a window below it.
    top = active if (y is not None and y < 24) else window_owner_at(x, y)
    ok = active == pid and top == pid
    return ok, "active pid=%s (%s) topmost window pid=%s%s, want pid=%s" % (
        active, (app.localizedName() if app else "?"), top, " at %.0f,%.0f" % (x, y) if x is not None else "", pid)

def activate_bound():
    app = target_app()
    if app is not None:
        from AppKit import NSApplicationActivateIgnoringOtherApps
        app.activateWithOptions_(NSApplicationActivateIgnoringOtherApps)
    time.sleep(0.6)

MOUSE_LIKE = (Q.kCGEventMouseMoved, Q.kCGEventLeftMouseDown, Q.kCGEventLeftMouseUp, Q.kCGEventLeftMouseDragged,
              Q.kCGEventRightMouseDown, Q.kCGEventRightMouseUp, Q.kCGEventScrollWheel)

BURST = [False]  # inside a multi-click: the guard ran once just before; no per-event delay (egui double-click window is 0.3 s)

def post(e):
    if BURST[0]: return raw_post(e)
    if PID[0] is None:  # name-prefix fallback (cannot tell instances apart); --pid is the supported mode
        if not frontmost_ok():
            raise SystemExit("ABORT: %s is not the frontmost app (front=%r); no event sent" % (APP, frontmost_name()))
        return raw_post(e)
    x = y = None
    if Q.CGEventGetType(e) in MOUSE_LIKE:
        p = Q.CGEventGetLocation(e); x, y = p.x, p.y
    ok, detail = guard_state(PID[0], x, y)
    if not ok:
        log("GUARD mismatch, event NOT posted: " + detail + "; trying `front` once")
        activate_bound()
        ok, detail = guard_state(PID[0], x, y)
        if not ok:
            log("GUARD ABORT: bound app is still not in front, no event sent: " + detail)
            release_mods()
            sys.exit(3)
    raw_post(e)

def flags_of(mods):
    f = 0
    for m in mods: f |= MODS[m]
    return f

def mod_down(m):
    e = Q.CGEventCreateKeyboardEvent(None, MODKEY[m], True)
    Q.CGEventSetFlags(e, flags_of(HELD | {m})); post(e); HELD.add(m); time.sleep(0.03)

def mod_up(m):
    HELD.discard(m)
    e = Q.CGEventCreateKeyboardEvent(None, MODKEY[m], False)
    Q.CGEventSetFlags(e, flags_of(HELD)); raw_post(e); time.sleep(0.03)  # releasing is always safe

def release_mods():
    """Release every modifier (also ones we did not track), flags 0. Bypasses the guard on purpose."""
    HELD.clear()
    for m in MODKEY:
        e = Q.CGEventCreateKeyboardEvent(None, MODKEY[m], False); Q.CGEventSetFlags(e, 0); raw_post(e); time.sleep(0.01)
    log("release_mods done")

atexit.register(lambda: release_mods() if HELD else None)

def mouse(typ, x, y, btn=Q.kCGMouseButtonLeft, flags=0, state=None):
    e = Q.CGEventCreateMouseEvent(None, typ, (x, y), btn)
    Q.CGEventSetFlags(e, flags)
    if state is not None: Q.CGEventSetIntegerValueField(e, Q.kCGMouseEventClickState, state)
    post(e)

def move(x, y, flags=0):
    mouse(Q.kCGEventMouseMoved, x, y, flags=flags); time.sleep(0.15)

def click(x, y, button="left", n=1, mods=()):
    for m in mods: mod_down(m)
    try:
        fl = flags_of(mods)  # zero when no modifiers: stale shift/cmd never leaks into a plain click
        move(x, y, fl)
        if button == "left": d, u, b = Q.kCGEventLeftMouseDown, Q.kCGEventLeftMouseUp, Q.kCGMouseButtonLeft
        else: d, u, b = Q.kCGEventRightMouseDown, Q.kCGEventRightMouseUp, Q.kCGMouseButtonRight
        if n > 1:
            # fresh guard once (aborts like post() does), then post the burst without per-event guard delay:
            # per-event guard checks (about 50 ms each) stretched a double-click past egui's 0.3 s window in the 0026 D2 run.
            probe = Q.CGEventCreateMouseEvent(None, Q.kCGEventMouseMoved, (x, y), Q.kCGMouseButtonLeft)
            post(probe)
            BURST[0] = True
        for i in range(1, n + 1):
            mouse(d, x, y, b, fl, i); time.sleep(0.03)
            mouse(u, x, y, b, fl, i); time.sleep(0.08)
    finally:
        BURST[0] = False
        for m in mods: mod_up(m)

def drag(x1, y1, x2, y2):
    move(x1, y1)
    mouse(Q.kCGEventLeftMouseDown, x1, y1, state=1); time.sleep(0.1)
    steps = 12
    for i in range(1, steps + 1):
        mouse(Q.kCGEventLeftMouseDragged, x1 + (x2 - x1) * i / steps, y1 + (y2 - y1) * i / steps); time.sleep(0.03)
    mouse(Q.kCGEventLeftMouseUp, x2, y2, state=1); time.sleep(0.1)

def wheel(x, y, dx, dy, mods=()):
    """Scroll wheel at screen point (x, y): pixel units, dy>0 scrolls up / content down, dx>0 scrolls left (CGEvent
    convention). Sent in 6 steps of dx/6, dy/6 so egui sees a smooth gesture. Pinch has no CGEvent equivalent here."""
    for m in mods: mod_down(m)
    try:
        move(x, y, flags_of(mods))
        n = 6
        for _ in range(n):
            e = Q.CGEventCreateScrollWheelEvent(None, Q.kCGScrollEventUnitPixel, 2, int(round(dy / n)), int(round(dx / n)))
            Q.CGEventSetFlags(e, flags_of(mods))
            post(e); time.sleep(0.03)
        time.sleep(0.1)
    finally:
        for m in mods: mod_up(m)

def key(name, mods=()):
    code = KEYS[name]
    for m in mods: mod_down(m)
    try:
        fl = flags_of(mods)
        for down in (True, False):
            e = Q.CGEventCreateKeyboardEvent(None, code, down); Q.CGEventSetFlags(e, fl); post(e); time.sleep(0.03)
    finally:
        for m in reversed(list(mods)): mod_up(m)

def type_text(text):
    for ch in text:
        for down in (True, False):
            e = Q.CGEventCreateKeyboardEvent(None, 0, down)
            Q.CGEventKeyboardSetUnicodeString(e, len(ch), ch)
            Q.CGEventSetFlags(e, 0)  # no modifier may leak into typed characters
            post(e); time.sleep(0.004)

def osa(script):
    r = subprocess.run(["osascript", "-e", script], capture_output=True, text=True)
    if r.returncode != 0: raise RuntimeError(r.stderr.strip())
    return r.stdout.strip()

def winfo():
    return osa('tell application "System Events" to tell %s to get {position, size} of window 1' % se_process())

def screen_pts():
    from AppKit import NSScreen
    s = NSScreen.mainScreen().frame().size
    return int(s.width), int(s.height)

def img_size(p):
    out = subprocess.run(["sips", "-g", "pixelWidth", "-g", "pixelHeight", p], capture_output=True, text=True).stdout
    return int(re.search(r"pixelWidth: (\d+)", out).group(1)), int(re.search(r"pixelHeight: (\d+)", out).group(1))

def shot(name):
    os.makedirs(SHOT_DIR, exist_ok=True)
    p = os.path.join(SHOT_DIR, name + ".png"); s = os.path.join(SHOT_DIR, name + "_s.png")
    subprocess.run(["screencapture", "-x", "-m", p], check=True)  # main display only
    subprocess.run(["sips", "-Z", "1600", p, "--out", s], check=True, capture_output=True)
    try: w = winfo()
    except Exception: w = "n/a"
    LAST_SHOT[0] = p
    sw, sh = screen_pts(); iw, ih = img_size(s)
    log("shot %s: _s=%dx%d screen_pts=%dx%d factor=%.4f (screen_pt = _s_pixel * factor) window{pos,size}=%s" % (name, iw, ih, sw, sh, sw / iw, w))

def winshot(name):
    """Window-only capture (no shadow, `screencapture -o -l <windowid>`) of the largest on-screen layer-0 window of
    --pid; full pixels (2x on Retina), NAME is a base name under SHOT_DIR or an absolute path (without .png)."""
    if PID[0] is None: raise SystemExit("ABORT: winshot needs --pid")
    infos = Q.CGWindowListCopyWindowInfo(Q.kCGWindowListOptionOnScreenOnly | Q.kCGWindowListExcludeDesktopElements, Q.kCGNullWindowID) or []
    best = None
    for w in infos:
        if int(w.get("kCGWindowOwnerPID", -1)) != PID[0] or w.get("kCGWindowLayer", 1) != 0: continue
        b = w.get("kCGWindowBounds") or {}
        area = b.get("Width", 0) * b.get("Height", 0)
        if best is None or area > best[0]: best = (area, int(w["kCGWindowNumber"]), b)
    if best is None: raise SystemExit("ABORT: winshot found no window of pid %d" % PID[0])
    p = name if os.path.isabs(name) else os.path.join(SHOT_DIR, name)
    if not p.endswith(".png"): p += ".png"
    os.makedirs(os.path.dirname(p), exist_ok=True)
    subprocess.run(["screencapture", "-x", "-o", "-l", str(best[1]), p], check=True)
    iw, ih = img_size(p)
    log("winshot %s: window id %d bounds(pts) %gx%g at %g,%g -> %dx%d px, %d bytes" % (p, best[1], best[2]["Width"], best[2]["Height"], best[2]["X"], best[2]["Y"], iw, ih, os.path.getsize(p)))

LAST_SHOT = [None]  # full-resolution PNG of the most recent `shot` in this run

def crop(name, x, y, w, h):
    """Save a FULL-RESOLUTION region (screen points x,y,w,h, origin top-left of the main display) of the last
    `shot`'s full PNG as NAME.png. Pixel = point * (png_pixel_width / screen_points_width) (2.0 on Retina)."""
    src = LAST_SHOT[0]
    if not src or not os.path.exists(src): raise SystemExit("ABORT: crop needs an earlier `shot` in the same run")
    sw, sh = screen_pts(); iw, ih = img_size(src); f = iw / sw
    px, py, pw, ph = int(round(x * f)), int(round(y * f)), int(round(w * f)), int(round(h * f))
    if px < 0 or py < 0 or pw <= 0 or ph <= 0 or px + pw > iw or py + ph > ih:
        raise SystemExit("ABORT: crop %s rect %s outside the %dx%d screenshot" % (name, (px, py, pw, ph), iw, ih))
    out = os.path.join(SHOT_DIR, name + ".png")
    subprocess.run(["sips", "--cropToHeightWidth", str(ph), str(pw), "--cropOffset", str(py), str(px), src, "--out", out],
                   check=True, capture_output=True)
    log("crop %s: points(%g,%g %gx%g) -> pixels(%d,%d %dx%d) of %s factor=%.4f -> %s" % (name, x, y, w, h, px, py, pw, ph, os.path.basename(src), f, out))

def pbpaste(): return subprocess.run(["pbpaste"], capture_output=True).stdout

def run(cmd):
    c = cmd.split(" ", 1); op = c[0]; rest = c[1] if len(c) > 1 else ""
    if op in ("#", ""): return
    log("> " + cmd + ("   [front=%s]" % frontmost_name() if op not in ("sleep", "clip", "clipsave", "clipclear", "note", "crop") else ""))
    f = lambda: list(map(float, rest.split()))
    if op == "click": a = rest.split(); click(float(a[0]), float(a[1]), mods=a[2:])
    elif op == "rclick": a = rest.split(); click(float(a[0]), float(a[1]), "right", mods=a[2:])
    elif op == "dclick": x, y = f(); click(x, y, "left", 2)
    elif op == "shift_click": x, y = f(); click(x, y, mods=("shift",))
    elif op in ("move", "hover"): x, y = f(); move(x, y)
    elif op == "drag": drag(*f())
    elif op == "wheel": a = rest.split(); wheel(*map(float, a[:4]), mods=a[4:])  # wheel X Y DX DY [shift|cmd|alt|ctrl ...]
    elif op == "type": type_text(rest)
    elif op == "key": p = rest.split(); key(p[0], p[1:])
    elif op == "release_mods": release_mods()
    elif op == "shot": shot(rest.strip())
    elif op == "winshot": winshot(rest.strip())
    elif op == "crop":
        a = rest.split(); crop(a[0], *map(float, a[1:5]))
    elif op == "note": log("NOTE " + rest)  # free text into action.log (step markers, expected values)
    elif op == "sleep": time.sleep(float(rest))
    elif op == "front":
        app = target_app()
        if app is not None:
            from AppKit import NSApplicationActivateIgnoringOtherApps
            app.activateWithOptions_(NSApplicationActivateIgnoringOtherApps)
        else:
            osa('tell application "%s" to activate' % APP)
        time.sleep(0.6)
        if not frontmost_ok(): raise SystemExit("ABORT: front failed, front=%r" % frontmost_name())
    elif op == "winfo": log("window {position,size} = " + winfo() + "  screen_pts=%dx%d" % screen_pts())
    elif op == "winsize":
        w, h = map(int, rest.split())
        try:
            osa('tell application "System Events" to tell %s to set size of window 1 to {%d, %d}' % (se_process(), w, h))
        except RuntimeError as e:
            raise SystemExit("ABORT: winsize needs Accessibility for the terminal (%s). Fallback: `winfo`, then "
                             "`drag` the bottom-right window corner by the size delta." % e)
        time.sleep(0.8); log("window now " + winfo())
    elif op == "winpos":
        x, y = map(int, rest.split())
        try:
            osa('tell application "System Events" to tell %s to set position of window 1 to {%d, %d}' % (se_process(), x, y))
        except RuntimeError as e:
            raise SystemExit("ABORT: winpos needs Accessibility for the terminal (%s)" % e)
        time.sleep(0.8); log("window now " + winfo())
    elif op == "clip": t = pbpaste(); log("clipboard %d bytes:" % len(t)); print(repr(t.decode("utf-8", "replace")), flush=True)
    elif op == "clipclear": subprocess.run(["pbcopy"], input=b""); log("clipboard cleared")
    elif op == "clipsave":
        os.makedirs(SHOT_DIR, exist_ok=True); t = pbpaste()
        path = os.path.join(SHOT_DIR, rest.strip() + ".txt"); open(path, "wb").write(t)
        log("clipsave %s: %d bytes -> %s" % (rest.strip(), len(t), path))
    else: raise SystemExit("unknown command: " + cmd)

def load_script(path, vars_):
    lines = [l.rstrip("\n") for l in open(path)]
    for l in lines:  # `set NAME VALUE` lines are collected first; --set overrides them
        m = re.match(r"set (\w+) (.*)$", l)
        if m: vars_.setdefault(m.group(1), m.group(2))
    out = []
    for l in lines:
        if not l.strip() or l.startswith("#") or l.startswith("set "): continue
        l = re.sub(r"\{\{(\w+)\}\}", lambda m: vars_.get(m.group(1), m.group(0)), l)
        out.append(l)
    bad = [l for l in out if "{{" in l]
    if bad: raise SystemExit("ABORT: unresolved placeholders (use --set NAME=VALUE), nothing executed:\n  " + "\n  ".join(bad))
    return out

def selftest_guard():
    """Proves the guard comparison without posting any event: the fresh front pid passes, any other pid fails,
    a point outside every window yields no owner. Exit 0 when all hold."""
    ok_fail = True
    front = int(frontmost_app().processIdentifier()) if (_pump() or True) and frontmost_app() else None
    top = window_owner_at()
    print("fresh: active pid=%s, topmost window pid=%s" % (front, top))
    if front is None or top is None:
        print("SELFTEST SKIP: no frontmost application or no on-screen window"); return
    good, d1 = guard_state(top) if front == top else (False, "")
    bad, d2 = guard_state(top + 1 if top < 2**30 else 1)
    print("bound=topmost pid -> ok=%s (%s)" % (good if front == top else "n/a (active != topmost window owner)", d1))
    print("bound=other pid   -> ok=%s (%s)" % (bad, d2))
    if bad: print("SELFTEST FAIL: a non-front pid passed the guard"); ok_fail = False
    if front == top and not good: print("SELFTEST FAIL: the front pid did not pass"); ok_fail = False
    if window_owner_at(-5000, -5000) is not None: print("SELFTEST FAIL: owner found at an off-screen point"); ok_fail = False
    print("SELFTEST %s" % ("OK" if ok_fail else "FAILED")); sys.exit(0 if ok_fail else 1)

def main(argv):
    vars_ = {"FIXTURES": os.path.normpath(os.path.join(HERE, "..", "fixtures")),
             "EXAMPLES": os.path.normpath(os.path.join(HERE, "..", "..", "examples")), "SHOT_DIR": SHOT_DIR}
    if argv and argv[0] == "--selftest-guard": return selftest_guard()
    check = False
    while argv and argv[0] in ("--set", "--check", "--pid"):
        if argv[0] == "--check": check = True; argv = argv[1:]
        elif argv[0] == "--pid": PID[0] = int(argv[1]); argv = argv[2:]
        else: k, v = argv[1].split("=", 1); vars_[k] = v; argv = argv[2:]
    cmds = load_script(argv[1], vars_) if argv[0] == "script" else [" ".join(argv)]
    if check: print("\n".join(cmds)); print("OK: %d commands, no unresolved placeholders" % len(cmds)); return
    target_app()  # with --pid: abort now if that process is not a running application
    NOEVENT = ("front", "winfo", "winsize", "clip", "clipsave", "clipclear", "release_mods", "sleep", "note", "crop")
    if not frontmost_ok() and cmds[0].split(" ")[0] not in NOEVENT:
        raise SystemExit("ABORT: %s is not frontmost (front=%r); nothing executed. Run `front` first." % (APP if PID[0] is None else "pid %d" % PID[0], frontmost_name()))
    log("=== run %s" % " ".join(argv))
    try:
        for c in cmds:
            try: run(c)
            except RuntimeError as e: raise SystemExit("ABORT: %s (osascript/System Events needs Accessibility for the terminal; app must be running)" % e)
    except BaseException as e:
        log("ABORTED: %s" % e); raise
    finally:
        release_mods()

if __name__ == "__main__":
    main(sys.argv[1:])
