#!/bin/sh
# The entry point of the chdlite and rommusic AppImages. scripts/appimage.sh fills in the
# @...@ values when it copies this into the AppDir.
#
# Unlike romexplorer's AppRun there are no writable paths to redirect: both tools keep their
# settings and webview data in the per-user config and cache directories whenever the folder
# beside the executable cannot be written to, which inside the read-only image it never can.
#
# What it adds:
#   - the window by default, and the console tools by name, so one image is the whole package:
#       ./@APP@.AppImage                       the window
#       ./@APP@.AppImage <tool> [args...]      a console tool (@TOOLS@)
#     A symlink named after a tool works too (ln -s @APP@.AppImage ~/bin/<tool>): the runtime
#     passes the name it was started under as ARGV0. Except one named @APP@ itself, which
#     still opens the window: renaming the download to plain "@APP@" is the likelier reason
#     for that name than wanting the console tool.
#   - --install / --uninstall, which put the window in the desktop menu and take it out again.
set -eu

HERE=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd -P)
APP=@APP@
GUI=@GUI@
TOOLS="@TOOLS@"
ICON_SIZE=@ICON_SIZE@

XDG_DATA_HOME=${XDG_DATA_HOME:-$HOME/.local/share}
# The AppImage runtime sets APPIMAGE to the .AppImage file itself; running the AppDir
# directly (which is how this gets tested) it is unset, so fall back to this script.
SELF=${APPIMAGE:-$HERE/AppRun}

is_tool() {
    for t in $TOOLS; do [ "$t" = "$1" ] && return 0; done
    return 1
}

# Started through a symlink named after a tool.
name=$(basename -- "${ARGV0:-$0}")
if [ "$name" != "$APP" ] && is_tool "$name"; then
    exec "$HERE/$name" "$@"
fi

case "${1:-}" in
    --install|--uninstall)
        apps="$XDG_DATA_HOME/applications"
        icons="$XDG_DATA_HOME/icons/hicolor/${ICON_SIZE}x${ICON_SIZE}/apps"
        entry="$apps/$APP.desktop"
        if [ "$1" = "--uninstall" ]; then
            rm -f "$entry" "$icons/$APP.png"
            command -v update-desktop-database >/dev/null 2>&1 && update-desktop-database "$apps" 2>/dev/null || true
            echo "removed the $APP menu entry (the AppImage itself is still at $SELF)"
            exit 0
        fi
        mkdir -p "$apps" "$icons"
        [ -f "$HERE/$APP.png" ] && cp -f "$HERE/$APP.png" "$icons/$APP.png"
        # Exec points at wherever the AppImage actually is: move the file and re-run
        # --install. Quoted, because "$SELF" may well be under ~/Downloads/My Stuff/.
        sed -e "s|^Exec=.*|Exec=\"$SELF\" %F|" -e "s|^TryExec=.*|TryExec=$SELF|" \
            "$HERE/$APP.desktop" > "$entry"
        chmod 644 "$entry"
        command -v update-desktop-database >/dev/null 2>&1 && update-desktop-database "$apps" 2>/dev/null || true
        echo "$APP is now in the desktop menu, pointing at $SELF"
        echo "(run it again with --uninstall to remove the entry)"
        exit 0
        ;;
esac

if [ $# -gt 0 ] && is_tool "$1"; then
    tool=$1
    shift
    exec "$HERE/$tool" "$@"
fi

exec "$HERE/$GUI" "$@"
