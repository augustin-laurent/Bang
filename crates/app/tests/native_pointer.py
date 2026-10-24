"""Inject a real pointer click into an isolated Xvfb display via XTEST.

For native Wayland tests, DISPLAY is Weston's outer Xvfb server. The fixture
uses an undecorated, origin-aligned Weston window and a fullscreen client.
No GTK/Slint event handler is called directly by this helper.
"""
import ctypes
import sys

x11 = ctypes.CDLL("libX11.so.6")
xtst = ctypes.CDLL("libXtst.so.6")
x11.XOpenDisplay.argtypes = [ctypes.c_char_p]
x11.XOpenDisplay.restype = ctypes.c_void_p
x11.XSync.argtypes = [ctypes.c_void_p, ctypes.c_int]
x11.XCloseDisplay.argtypes = [ctypes.c_void_p]
xtst.XTestFakeMotionEvent.argtypes = [ctypes.c_void_p, ctypes.c_int, ctypes.c_int, ctypes.c_int, ctypes.c_ulong]
xtst.XTestFakeButtonEvent.argtypes = [ctypes.c_void_p, ctypes.c_uint, ctypes.c_int, ctypes.c_ulong]
display = x11.XOpenDisplay(None)
assert display, "XTEST needs the isolated test DISPLAY"
try:
    assert xtst.XTestFakeMotionEvent(display, -1, int(sys.argv[1]), int(sys.argv[2]), 0)
    assert xtst.XTestFakeButtonEvent(display, 1, True, 0)
    assert xtst.XTestFakeButtonEvent(display, 1, False, 0)
    x11.XSync(display, False)
finally:
    x11.XCloseDisplay(display)
