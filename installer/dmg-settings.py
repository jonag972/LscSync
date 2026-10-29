# Layout of the macOS disk image, read by dmgbuild (tools/bundle-macos.sh).
# The window shows the app, a shortcut to Applications and, between them, the
# drag path drawn in assets/installer/dmg-background.svg.
import os

app = defines["app"]  # noqa: F821 (provided by dmgbuild)

files = [app]
symlinks = {"Applications": "/Applications"}
icon = defines["icon"]  # noqa: F821
background = defines["background"]  # noqa: F821
format = "UDZO"

# The window frame includes the title bar, whose height varies across macOS
# versions: the bottom of the 660x400 background may be cut, never its top.
window_rect = ((200, 120), (660, 400))
default_view = "icon-view"
show_status_bar = False
show_tab_view = False
show_toolbar = False
show_pathbar = False
show_sidebar = False

icon_size = 112
text_size = 13
# Centers, in background coordinates.
icon_locations = {
    os.path.basename(app): (170, 170),
    "Applications": (490, 170),
}
hide_extensions = [os.path.basename(app)]
