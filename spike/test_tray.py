#!/usr/bin/env python3
"""The app's tray icon and what its window does around it, on a real desktop.

Starts the app with a throwaway profile under XWayland (X11 lets a script
see windows and close one the way a title bar's button does; Wayland does
not), and talks to its tray item over D-Bus as the desktop's tray would.
Checks that

  - closing the window leaves the app running in the tray,
  - a click on the icon, and "Open" in its menu, bring the window back,
  - the menu's "Upscale video" switches upscaling, and "Quit" quits,
  - started at login with "Start minimized" there is the icon and no window,
  - without a tray (a private session bus nobody watches) closing the window
    quits, and "Start minimized" falls back to a minimised window.

Usage:  ~/vsr-venv/bin/python spike/test_tray.py [path/to/rtx-vsr]
  needs: a built app ((cd app && cargo build --release)), a desktop session
         with a tray (on GNOME: the AppIndicator extension) and XWayland,
         and xwininfo, xprop, gdbus and dbus-daemon.

Windows of the app appear on your screen for a moment while this runs.
"""
import ctypes
import json
import os
import re
import shutil
import subprocess
import sys
import tempfile
import time
import urllib.request

HERE = os.path.dirname(os.path.abspath(__file__))
REPO = os.path.dirname(HERE)
PORT = 8745            # not the app's usual port: yours may be running
MINIMIZED = {"start_minimized": True}

X = ctypes.CDLL("libX11.so.6")
X.XOpenDisplay.restype = ctypes.c_void_p
X.XInternAtom.argtypes = [ctypes.c_void_p, ctypes.c_char_p, ctypes.c_int]
X.XInternAtom.restype = ctypes.c_ulong


class ClientMessage(ctypes.Structure):
    _fields_ = [("type", ctypes.c_int), ("serial", ctypes.c_ulong), ("send_event", ctypes.c_int),
                ("display", ctypes.c_void_p), ("window", ctypes.c_ulong), ("message_type", ctypes.c_ulong),
                ("format", ctypes.c_int), ("data", ctypes.c_long * 5), ("pad", ctypes.c_long * 8)]


def find_app():
    if len(sys.argv) > 1:
        return sys.argv[1]
    for build in ("release", "debug"):
        p = os.path.join(REPO, "app", "target", build, "rtx-vsr")
        if os.path.exists(p):
            return p
    sys.exit("build the app first: (cd app && cargo build --release)")


def windows():
    out = subprocess.run(["xwininfo", "-root", "-tree"], capture_output=True, text=True).stdout
    return [int(m.group(1), 16)
            for m in re.finditer(r'^\s+(0x[0-9a-f]+) "RTX VSR": \("rtx-vsr"', out, re.M)]


def minimized(win):
    out = subprocess.run(["xprop", "-id", hex(win), "_NET_WM_STATE", "WM_STATE"],
                         capture_output=True, text=True).stdout
    return "_NET_WM_STATE_HIDDEN" in out or "Iconic" in out


def close(win):
    """What the window manager sends when the close button is pressed."""
    d = ctypes.c_void_p(X.XOpenDisplay(None))
    ev = ClientMessage()
    ev.type, ev.window, ev.format = 33, win, 32          # ClientMessage
    ev.message_type = X.XInternAtom(d, b"WM_PROTOCOLS", 0)
    ev.data[0] = X.XInternAtom(d, b"WM_DELETE_WINDOW", 0)
    X.XSendEvent(d, ctypes.c_ulong(win), 0, ctypes.c_long(0), ctypes.byref(ev))
    X.XFlush(d)
    X.XCloseDisplay(d)


def gdbus(dest, path, method, *args):
    # "--": a negative number among the arguments is not an option
    r = subprocess.run(["gdbus", "call", "--session", "--dest", dest, "--object-path", path,
                        "--method", method, "--", *args], capture_output=True, text=True)
    return r.stdout.strip()


def wait(cond, timeout=8):
    t0 = time.time()
    while time.time() - t0 < timeout:
        if cond():
            return True
        time.sleep(0.1)
    return False


class App:
    """A copy of the app with its own settings, data and single-instance socket."""

    def __init__(self, exe, *args, settings=None):
        self.home = tempfile.mkdtemp(prefix="vsr-tray-")
        for d in ("run", "config/rtx-vsr"):
            os.makedirs(os.path.join(self.home, d))
        with open(os.path.join(self.home, "config", "rtx-vsr", "config.json"), "w") as f:
            json.dump(settings or {}, f)
        env = dict(os.environ, WAYLAND_DISPLAY="",
                   XDG_CONFIG_HOME=os.path.join(self.home, "config"),
                   XDG_DATA_HOME=os.path.join(self.home, "data"),
                   XDG_RUNTIME_DIR=os.path.join(self.home, "run"))
        self.proc = subprocess.Popen([exe, "--port", str(PORT), *args], env=env,
                                     stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
        self.item = f"org.kde.StatusNotifierItem-{self.proc.pid}-1"

    def __enter__(self):
        return self

    def __exit__(self, *exc):
        if self.proc.poll() is None:
            self.proc.kill()
        self.proc.wait()
        shutil.rmtree(self.home, ignore_errors=True)

    def exited(self):
        return self.proc.poll() is not None

    def get(self, path):
        with urllib.request.urlopen(f"http://127.0.0.1:{PORT}{path}", timeout=2) as r:
            return json.loads(r.read())

    def serving(self):
        try:
            return not self.exited() and self.get("/health")["app"] == "rtx-vsr"
        except Exception:
            return False

    def in_tray(self):
        items = gdbus("org.kde.StatusNotifierWatcher", "/StatusNotifierWatcher",
                      "org.freedesktop.DBus.Properties.Get", "org.kde.StatusNotifierWatcher",
                      "RegisteredStatusNotifierItems")
        return f"StatusNotifierItem-{self.proc.pid}-" in items

    def click_icon(self):
        gdbus(self.item, "/StatusNotifierItem", "org.kde.StatusNotifierItem.Activate", "0", "0")

    def menu(self):
        layout = gdbus(self.item, "/MenuBar", "com.canonical.dbusmenu.GetLayout", "0", "-1", "[]")
        return {label: id for id, label in re.findall(r"<\((\d+), \{'label': <'([^']*)'>", layout)}

    def click_menu(self, label):
        gdbus(self.item, "/MenuBar", "com.canonical.dbusmenu.Event", self.menu()[label],
              "clicked", "<0>", "0")


def main():
    exe = find_app()
    if not os.environ.get("DISPLAY"):
        sys.exit("needs an X display (XWayland)")
    if "true" not in gdbus("org.freedesktop.DBus", "/org/freedesktop/DBus",
                           "org.freedesktop.DBus.NameHasOwner", "org.kde.StatusNotifierWatcher"):
        sys.exit("this desktop has no tray (on GNOME: install the AppIndicator extension)")
    ok = True

    def check(label, good):
        nonlocal ok
        ok &= bool(good)
        print(f"{label}: {'ok' if good else 'FAILED'}")

    print("-- with a tray")
    with App(exe) as app:
        check("the window opens", wait(lambda: len(windows()) == 1))
        check("the icon is in the tray", wait(app.in_tray))
        check("its menu", list(app.menu()) == ["Open RTX VSR", "Upscale video", "Quit"])
        close(windows()[0])
        check("closing the window closes it", wait(lambda: not windows()))
        time.sleep(1.5)
        check("and leaves the app running in the tray", app.serving() and app.in_tray())
        app.click_icon()
        check("a click on the icon brings the window back", wait(lambda: len(windows()) == 1))
        close(windows()[0])
        wait(lambda: not windows())
        app.click_menu("Open RTX VSR")
        check("so does Open in the menu", wait(lambda: len(windows()) == 1))
        app.click_menu("Upscale video")
        check("the menu switches upscaling off", wait(lambda: not app.get("/config")["enabled"]))
        app.click_menu("Upscale video")
        check("and on", wait(lambda: app.get("/config")["enabled"]))
        app.click_menu("Quit")
        check("Quit ends the app", wait(app.exited))
        check("and takes the icon away", wait(lambda: not app.in_tray()))

    with App(exe, "--autostart", settings=MINIMIZED) as app:
        check("at login, minimized: the icon", wait(app.in_tray))
        time.sleep(4)
        check("and no window", not windows() and app.serving())
        app.click_icon()
        check("the icon opens the window", wait(lambda: len(windows()) == 1))
        time.sleep(1)
        check("which is not minimized", not minimized(windows()[0]))
    with App(exe, "--autostart") as app:
        check("at login, not minimized: a window", wait(lambda: len(windows()) == 1))
    with App(exe, settings=MINIMIZED) as app:
        check("started by hand, minimized or not: a window", wait(lambda: len(windows()) == 1))

    print("-- without a tray")
    bus = subprocess.Popen(["dbus-daemon", "--session", "--nofork", "--print-address"],
                           stdout=subprocess.PIPE, text=True)
    os.environ["DBUS_SESSION_BUS_ADDRESS"] = bus.stdout.readline().strip()
    try:
        with App(exe) as app:
            check("the window opens", wait(lambda: len(windows()) == 1))
            time.sleep(1)
            close(windows()[0])
            check("closing the window quits", wait(app.exited))
        with App(exe, "--autostart", settings=MINIMIZED) as app:
            time.sleep(6)
            check("at login, minimized: waits for a tray", not windows() and app.serving())
            check("then opens a window", wait(lambda: len(windows()) == 1))
            time.sleep(1.5)
            check("minimized", minimized(windows()[0]))
            close(windows()[0])
            check("closing it quits", wait(app.exited))
    finally:
        bus.kill()
    print("PASS" if ok else "FAIL")
    return 0 if ok else 1


if __name__ == "__main__":
    sys.exit(main())
