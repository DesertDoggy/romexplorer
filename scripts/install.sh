#!/usr/bin/env bash
# Install the romexplorer server as a service that comes back on its own after a reboot.
#
# It is both the script inside a server package and the script you can pipe from the web:
#
#   # from an unpacked package (it finds the package around itself)
#   ./install.sh
#
#   # from a release, without downloading anything by hand first
#   #   ...| bash, not | sh: this script uses arrays and [[ ]], and dash rejects both.
#   curl -fsSL https://raw.githubusercontent.com/DesertDoggy/romexplorer/main/scripts/install.sh | bash -s -- --release latest
#
#   # from a file or URL you already have
#   ./install.sh --from ./romexplorer-server-0.1.0-linux-x64.tar.gz
#   ./install.sh --from https://example.invalid/romexplorer-server-0.1.0-linux-x64.tar.gz
#
#   ./install.sh --uninstall            # stop, disable and remove the service (keeps data)
#   ./install.sh --uninstall --purge    # ...and delete the install, database included
#
# As root it installs system-wide (/opt/romexplorer, a `romexplorer` system account, a
# systemd system unit or a launchd daemon). As an ordinary user it installs into
# ~/.local/opt/romexplorer with a systemd *user* unit or a launchd agent, and needs no
# privileges at all. Both survive a reboot; the user install needs lingering enabled, which
# this script turns on for you.
#
# What it does NOT do: create accounts itself. It writes the setup file the server reads on
# its first start (see romexplorer-server-start for why that is the service's job and
# not this script's) and then watches the service come up.
set -euo pipefail

REPO="DesertDoggy/romexplorer"
SERVICE_NAME="romexplorer"

FROM="" RELEASE="" PREFIX="" CONFIG="" SERVICE="auto" SVC_USER=""
PORT="" BIND="" START=1 UNINSTALL=0 PURGE=0 ASSUME_YES=0

die() { printf 'error: %s\n' "$*" >&2; exit 1; }
note() { printf '  %s\n' "$*"; }
step() { printf '\n==> %s\n' "$*"; }
warn() { printf 'warning: %s\n' "$*" >&2; }

usage() {
    # The header block is the long-form help, but it is only readable when this script is a
    # file: piped from curl, BASH_SOURCE[0] is "main" and there is nothing to read. And the
    # block is found by where the comments stop, not by a line range -- a range goes stale
    # the first time the header is edited, silently truncating the help.
    if [[ -n "${BASH_SOURCE[0]:-}" && -f "${BASH_SOURCE[0]}" ]]; then
        sed -n '2,/^[^#]/p' "${BASH_SOURCE[0]}" | sed -n 's/^# \{0,1\}//p'
    else
        printf 'Install the romexplorer server as a service that restarts after a reboot.\n'
        printf 'Run it from a checkout or an unpacked package for the full notes.\n'
    fi
    cat <<'EOF'

Options:
  --from SRC          a package: a URL, a .tar.gz/.zip, or an unpacked directory
  --release TAG       take the package from a GitHub release ("latest" for the newest)
  --prefix DIR        install here (default: /opt/romexplorer as root,
                      ~/.local/opt/romexplorer otherwise)
  --config FILE       settings to read (default: romexplorer.conf beside this script, then
                      the packaged romexplorer.conf.example; see that file for the keys)
  --service KIND      systemd | systemd-user | launchd | none   (default: pick for this host)
  --svc-user NAME     the account a system install runs as      (default: romexplorer)
  --port N            REST listener port                        (default: config, else 7337)
  --bind ADDR         REST listener address                      (default: config, else 127.0.0.1)
  --no-start          install the service but leave it stopped
  --uninstall         stop, disable and remove the service
  --purge             with --uninstall, also delete the install directory and its database
  --yes               do not ask before replacing an existing install
EOF
    exit "${1:-0}"
}

while [[ $# -gt 0 ]]; do
    case "$1" in
        --from) FROM="${2:-}"; shift 2 ;;
        --release) RELEASE="${2:-}"; shift 2 ;;
        --prefix) PREFIX="${2:-}"; shift 2 ;;
        --config) CONFIG="${2:-}"; shift 2 ;;
        --service) SERVICE="${2:-}"; shift 2 ;;
        --svc-user) SVC_USER="${2:-}"; shift 2 ;;
        --port) PORT="${2:-}"; shift 2 ;;
        --bind) BIND="${2:-}"; shift 2 ;;
        --no-start) START=0; shift ;;
        --uninstall) UNINSTALL=1; shift ;;
        --purge) PURGE=1; shift ;;
        --yes|-y) ASSUME_YES=1; shift ;;
        -h|--help) usage 0 ;;
        *) printf 'unknown option: %s\n' "$1" >&2; usage 2 ;;
    esac
done

# --- host shape ----------------------------------------------------------------------------
case "$(uname -s)" in
    Linux) HOST_OS="linux" ;;
    Darwin) HOST_OS="macos" ;;
    *) die "this installer covers Linux and macOS; on Windows use scripts/install.ps1" ;;
esac
case "$(uname -m)" in
    x86_64|amd64) HOST_ARCH="x64" ;;
    arm64|aarch64) HOST_ARCH="arm64" ;;
    *) die "unsupported architecture $(uname -m)" ;;
esac

SYSTEM=0
[[ "$(id -u)" -eq 0 ]] && SYSTEM=1

if [[ -z "$PREFIX" ]]; then
    if [[ "$SYSTEM" -eq 1 ]]; then
        PREFIX="/opt/romexplorer"
    else
        PREFIX="$HOME/.local/opt/romexplorer"
    fi
fi
# The install is the app root, exactly as the package is: executables beside lib/, assets/
# and data/.
APP="$PREFIX"

if [[ "$SERVICE" == "auto" ]]; then
    if [[ "$HOST_OS" == "macos" ]]; then
        SERVICE="launchd"
    elif [[ "$SYSTEM" -eq 1 ]]; then
        SERVICE="systemd"
    else
        SERVICE="systemd-user"
    fi
fi
case "$SERVICE" in systemd|systemd-user|launchd|none) ;; *) die "--service must be systemd, systemd-user, launchd or none" ;; esac
[[ "$SERVICE" == "systemd" && "$SYSTEM" -eq 0 ]] && die "--service systemd needs root; use --service systemd-user for a per-user install"

[[ -z "$SVC_USER" && "$SYSTEM" -eq 1 ]] && SVC_USER="romexplorer"

# Where the unit/plist goes, and how we drive it.
if [[ "$SYSTEM" -eq 1 ]]; then
    UNIT_PATH="/etc/systemd/system/$SERVICE_NAME.service"
    PLIST_PATH="/Library/LaunchDaemons/com.romexplorer.server.plist"
else
    UNIT_PATH="${XDG_CONFIG_HOME:-$HOME/.config}/systemd/user/$SERVICE_NAME.service"
    PLIST_PATH="$HOME/Library/LaunchAgents/com.romexplorer.server.plist"
fi
SYSTEMCTL=(systemctl)
[[ "$SERVICE" == "systemd-user" ]] && SYSTEMCTL=(systemctl --user)

# --- small helpers -------------------------------------------------------------------------
have() { command -v "$1" >/dev/null 2>&1; }

# Replace or append one `KEY="value"` line. awk and not `sed -i`, because the value is a
# path or a password: with sed it would be part of the s/// expression, where a `|`, `&` or
# `\` in it changes the meaning rather than the text. Here it is only ever data.
set_kv() {
    local file="$1" key="$2" value="$3"
    [[ -f "$file" ]] || : > "$file"
    local tmp="$file.tmp.$$"
    awk -v key="$key" -v value="$value" '
        BEGIN { pattern = "^[[:space:]]*(export[[:space:]]+)?" key "[[:space:]]*=" }
        $0 ~ pattern { if (!done) { print key "=\"" value "\""; done = 1 } next }
        { print }
        END { if (!done) print key "=\"" value "\"" }
    ' "$file" > "$tmp"
    mv -f "$tmp" "$file"
}

# The config files here are `KEY=VALUE`, the same shape src/database/env_file.rs reads.
conf_get() {
    local key="$1" file="${2:-$CONFIG}"
    [[ -f "$file" ]] || return 0
    sed -n -E "s|^[[:space:]]*(export[[:space:]]+)?${key}[[:space:]]*=[[:space:]]*(.*)$|\2|p" "$file" \
        | tail -n1 | sed -E 's|^"(.*)"$|\1|; s|^'\''(.*)'\''$|\1|' | sed -E 's|[[:space:]]+$||'
}

# 24 alphanumerics. A fixed read of N random bytes is *usually* enough once non-alphanumerics
# are dropped (about 3 in 8 survive) but not reliably -- 128 bytes yielded 23 characters in
# testing -- so this accumulates until it has enough instead of hoping. dd, not `head -c`, so
# the input is finite and no reader exits early on a pipeline under `set -o pipefail`.
gen_password() {
    local out=""
    while [[ "${#out}" -lt 24 ]]; do
        out="$out$(LC_ALL=C dd if=/dev/urandom bs=256 count=1 2>/dev/null | LC_ALL=C tr -dc 'A-Za-z0-9')"
    done
    printf '%s' "${out:0:24}"
}

port_open() { (exec 3<>"/dev/tcp/${1}/${2}") >/dev/null 2>&1; }

confirm() {
    [[ "$ASSUME_YES" -eq 1 ]] && return 0
    [[ -t 0 ]] || die "$1 (re-run with --yes to do this non-interactively)"
    printf '%s [y/N] ' "$1"
    local answer; read -r answer
    [[ "$answer" == y* || "$answer" == Y* ]]
}

# --- uninstall -----------------------------------------------------------------------------
if [[ "$UNINSTALL" -eq 1 ]]; then
    step "removing the $SERVICE_NAME service"
    case "$SERVICE" in
        systemd|systemd-user)
            if have systemctl; then
                "${SYSTEMCTL[@]}" stop "$SERVICE_NAME" 2>/dev/null || true
                "${SYSTEMCTL[@]}" disable "$SERVICE_NAME" 2>/dev/null || true
            fi
            [[ -f "$UNIT_PATH" ]] && rm -f "$UNIT_PATH" && note "removed $UNIT_PATH"
            have systemctl && "${SYSTEMCTL[@]}" daemon-reload 2>/dev/null || true
            ;;
        launchd)
            launchctl unload "$PLIST_PATH" 2>/dev/null || true
            [[ -f "$PLIST_PATH" ]] && rm -f "$PLIST_PATH" && note "removed $PLIST_PATH"
            ;;
    esac
    # A mariadbd that outlived its server (a broken install, a killed service) keeps the
    # database port. Left alone it survives the purge with its files deleted under it, and the
    # next install's first-run setup connects to it instead of a fresh database and fails with
    # "Access denied for user 'root'". SIGTERM is a clean shutdown for mariadbd.
    if have pgrep; then
        for pid in $(pgrep -x mariadbd 2>/dev/null || true); do
            cmd="$(ps -o command= -p "$pid" 2>/dev/null || true)"
            [[ "$cmd" == "$PREFIX/"* ]] || continue
            kill "$pid" 2>/dev/null || { warn "a database server from this install is still running (pid $pid) and could not be stopped"; continue; }
            note "stopping the database server left running from this install (pid $pid)"
            for _ in $(seq 1 60); do kill -0 "$pid" 2>/dev/null || break; sleep 1; done
        done
    fi
    if [[ "$PURGE" -eq 1 ]]; then
        if [[ -d "$PREFIX" ]]; then
            confirm "delete $PREFIX and everything in it, database included?" || die "left $PREFIX alone"
            # Contents first, then the directory if it will go: a prefix that is a mount point
            # (or sits in a parent we cannot write to) empties fine but cannot be removed.
            find "$PREFIX" -mindepth 1 -delete
            if rmdir "$PREFIX" 2>/dev/null; then
                note "removed $PREFIX"
            else
                note "emptied $PREFIX (the directory itself could not be removed; a mount point?)"
            fi
        fi
    else
        note "kept $PREFIX (pass --purge to delete it, database included)"
    fi
    printf '\nuninstalled.\n'
    exit 0
fi

# --- find the package ----------------------------------------------------------------------
SELF_DIR=""
if [[ -n "${BASH_SOURCE[0]:-}" && -f "${BASH_SOURCE[0]}" ]]; then
    SELF_DIR="$(CDPATH= cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd -P)"
fi

TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT

fetch() {
    local url="$1" dest="$2"
    if have curl; then
        curl -fsSL --retry 4 --retry-delay 2 --retry-all-errors -o "$dest" "$url"
    elif have wget; then
        wget -q -O "$dest" "$url"
    else
        die "need curl or wget to download $url"
    fi
}

# The release's asset for this host, by the name scripts/package.sh gives it.
resolve_release() {
    local tag="$1" api
    if [[ "$tag" == "latest" ]]; then
        api="https://api.github.com/repos/$REPO/releases/latest"
    else
        api="https://api.github.com/repos/$REPO/releases/tags/$tag"
    fi
    local json="$TMP/release.json"
    fetch "$api" "$json" || die "could not read the release list for $REPO ($tag)"
    # Deliberately grep and not a JSON parser: the target machine is not guaranteed python3
    # or jq, and the only thing wanted here is one browser_download_url.
    local want="romexplorer-server-.*-${HOST_OS}-${HOST_ARCH}\.\(tar\.gz\|zip\)"
    local url
    url="$(grep -o "\"browser_download_url\"[[:space:]]*:[[:space:]]*\"[^\"]*\"" "$json" \
        | sed -E 's|.*"(https[^"]*)"$|\1|' | grep -E "$(printf '%s' "$want" | sed 's|\\||g')" | head -n1 || true)"
    [[ -n "$url" ]] || die "release $tag has no server package for $HOST_OS-$HOST_ARCH"
    printf '%s' "$url"
}

unpack_to() {
    local archive="$1" dir="$2"
    mkdir -p "$dir"
    case "$archive" in
        *.tar.gz|*.tgz) tar xzf "$archive" -C "$dir" ;;
        *.zip) have unzip || die "need unzip to open $archive"; unzip -q "$archive" -d "$dir" ;;
        *) die "don't know how to unpack $archive" ;;
    esac
}

# A staged package is any directory with the server binary in it.
looks_like_package() { [[ -f "$1/romexplorer_server" && -d "$1/lib" ]]; }

SRC=""
step "locating the package"
if [[ -n "$FROM" ]]; then
    case "$FROM" in
        http://*|https://*)
            name="$(basename "${FROM%%\?*}")"
            note "downloading $FROM"
            fetch "$FROM" "$TMP/$name"
            # Check the checksum if the publisher put one next to the archive.
            if fetch "$FROM.sha256" "$TMP/$name.sha256" 2>/dev/null; then
                if have sha256sum; then
                    (cd "$TMP" && sha256sum -c "$name.sha256" >/dev/null 2>&1) \
                        && note "sha256 verified" || die "sha256 mismatch on $name -- refusing to install it"
                elif have shasum; then
                    (cd "$TMP" && shasum -a 256 -c "$name.sha256" >/dev/null 2>&1) \
                        && note "sha256 verified" || die "sha256 mismatch on $name -- refusing to install it"
                fi
            else
                note "no .sha256 published next to the archive; skipping the checksum"
            fi
            unpack_to "$TMP/$name" "$TMP/unpacked"
            ;;
        *)
            [[ -e "$FROM" ]] || die "no such file or directory: $FROM"
            if [[ -d "$FROM" ]]; then
                SRC="$(CDPATH= cd -- "$FROM" && pwd -P)"
            else
                unpack_to "$FROM" "$TMP/unpacked"
            fi
            ;;
    esac
elif [[ -n "$SELF_DIR" ]] && looks_like_package "$SELF_DIR"; then
    SRC="$SELF_DIR"
    note "installing the package this script sits in"
else
    RELEASE="${RELEASE:-latest}"
    url="$(resolve_release "$RELEASE")"
    note "release $RELEASE: $(basename "$url")"
    fetch "$url" "$TMP/$(basename "$url")"
    if fetch "$url.sha256" "$TMP/$(basename "$url").sha256" 2>/dev/null; then
        name="$(basename "$url")"
        if have sha256sum; then
            (cd "$TMP" && sha256sum -c "$name.sha256" >/dev/null 2>&1) && note "sha256 verified" || die "sha256 mismatch on $name"
        elif have shasum; then
            (cd "$TMP" && shasum -a 256 -c "$name.sha256" >/dev/null 2>&1) && note "sha256 verified" || die "sha256 mismatch on $name"
        fi
    fi
    unpack_to "$TMP/$(basename "$url")" "$TMP/unpacked"
fi

if [[ -z "$SRC" ]]; then
    # An archive made by package.sh has exactly one top-level directory.
    if looks_like_package "$TMP/unpacked"; then
        SRC="$TMP/unpacked"
    else
        for d in "$TMP/unpacked"/*; do
            [[ -d "$d" ]] && looks_like_package "$d" && SRC="$d" && break
        done
    fi
fi
[[ -n "$SRC" ]] || die "that package has no romexplorer_server and lib/ at its top level"
looks_like_package "$SRC" || die "that package has no romexplorer_server and lib/ at its top level"
note "package: $SRC"
# An install has the same shape as a package, so the copy of this script inside one would
# otherwise "upgrade" the install from itself -- deleting lib/ and then copying it from nowhere.
[[ -d "$PREFIX" && "$SRC" -ef "$PREFIX" ]] && die "$PREFIX is the install itself, not a package to install from; pass --from or --release"

# Refuse a package built for another host rather than installing something that cannot run.
if [[ ! -d "$SRC/lib/$HOST_OS/$HOST_ARCH" ]]; then
    have_dirs="$(find "$SRC/lib" -mindepth 2 -maxdepth 2 -type d 2>/dev/null | sed "s|$SRC/lib/||" | tr '\n' ' ' || true)"
    die "that package carries libraries for [$have_dirs], not $HOST_OS/$HOST_ARCH"
fi

# A package with its MariaDB removed (to run against a database you already have) cannot do
# first-run setup: that creates the database by starting the bundled mariadbd. The database
# has to exist already and database.env has to say where it is.
BUNDLED_DB=1
[[ -x "$SRC/lib/$HOST_OS/$HOST_ARCH/mariadb/bin/mariadbd" ]] || BUNDLED_DB=0
if [[ "$BUNDLED_DB" -eq 0 && ! -f "$APP/data/database.env" ]]; then
    die "this package has no bundled MariaDB, so it cannot run first-run setup.
  Point it at a database you already have: create $APP/data/database.env (mode 600) with
    db_host=\"...\"  db_port=\"3306\"  db_user=\"...\"  db_password=\"...\"  db_database=\"romexplorer\"
  one per line, then re-run this installer. Or install the server package, which bundles one."
fi

# --- settings ------------------------------------------------------------------------------
if [[ -z "$CONFIG" ]]; then
    for candidate in "$SRC/romexplorer.conf" "${SELF_DIR:-}/romexplorer.conf" "$SRC/romexplorer.conf.example"; do
        [[ -n "$candidate" && -f "$candidate" ]] && CONFIG="$candidate" && break
    done
fi
[[ -n "$CONFIG" && -f "$CONFIG" ]] && note "settings from $CONFIG" || note "no settings file; using defaults for everything"

ADMIN_USER="$(conf_get admin_username)";      ADMIN_USER="${ADMIN_USER:-admin}"
ADMIN_PASS="$(conf_get admin_password)"
DB_NAME="$(conf_get database)";               DB_NAME="${DB_NAME:-romexplorer}"
DB_ROOT_PASS="$(conf_get db_root_password)"
DB_ADMIN_USER="$(conf_get db_admin_user)";    DB_ADMIN_USER="${DB_ADMIN_USER:-rx_admin}"
DB_ADMIN_PASS="$(conf_get db_admin_password)"
DB_APP_USER="$(conf_get db_app_user)"
DB_APP_PASS="$(conf_get db_app_password)"
DB_PORT="$(conf_get db_port)";                DB_PORT="${DB_PORT:-3307}"
DB_DATADIR="$(conf_get db_datadir)"
SHARE_ENABLED="$(conf_get share_enabled)"
SHARE_ADVERTISE="$(conf_get share_advertise)"
[[ -z "$PORT" ]] && PORT="$(conf_get server_port)"; PORT="${PORT:-7337}"
[[ -z "$BIND" ]] && BIND="$(conf_get server_bind)"; BIND="${BIND:-127.0.0.1}"

GENERATED_ADMIN_PASS=0
[[ -z "$ADMIN_PASS" ]] && { ADMIN_PASS="$(gen_password)"; GENERATED_ADMIN_PASS=1; }
[[ -z "$DB_ROOT_PASS" ]] && DB_ROOT_PASS="$(gen_password)"
[[ -z "$DB_ADMIN_PASS" ]] && DB_ADMIN_PASS="$(gen_password)"
# app_user and app_password go together or not at all (api::setup::SetupFile).
if [[ -n "$DB_APP_USER" && -z "$DB_APP_PASS" ]]; then
    DB_APP_PASS="$(gen_password)"
elif [[ -z "$DB_APP_USER" ]]; then
    DB_APP_PASS=""
fi

# --- the service account -------------------------------------------------------------------
if [[ "$SYSTEM" -eq 1 ]]; then
    if id -u "$SVC_USER" >/dev/null 2>&1; then
        note "service account $SVC_USER exists"
    else
        step "creating the $SVC_USER system account"
        if [[ "$HOST_OS" == "linux" ]]; then
            if have useradd; then
                useradd --system --home-dir "$PREFIX" --shell /usr/sbin/nologin "$SVC_USER" 2>/dev/null \
                    || useradd --system --home-dir "$PREFIX" --shell /bin/false "$SVC_USER"
            elif have adduser; then
                adduser --system --home "$PREFIX" --no-create-home "$SVC_USER"
            else
                die "no useradd/adduser here; make the $SVC_USER account yourself, or pass --svc-user with an existing one"
            fi
        else
            die "creating a daemon account on macOS is not automated; pass --svc-user with an existing account, or install as that user without --service launchd"
        fi
        note "created $SVC_USER"
    fi
fi

# --- lay the files down --------------------------------------------------------------------
# An upgrade must never touch the database, the keys, or anything the user put in assets/.
# Only the parts a release actually replaces are replaced.
UPGRADE=0
[[ -d "$APP/data" ]] && UPGRADE=1

if [[ "$UPGRADE" -eq 1 ]]; then
    step "upgrading the install at $PREFIX"
    if [[ "$SERVICE" != "none" ]]; then
        case "$SERVICE" in
            systemd|systemd-user) have systemctl && "${SYSTEMCTL[@]}" stop "$SERVICE_NAME" 2>/dev/null || true ;;
            launchd) launchctl unload "$PLIST_PATH" 2>/dev/null || true ;;
        esac
        note "stopped the running service"
    fi
    rm -rf "$APP/lib"
    mkdir -p "$APP/lib"
    cp -a "$SRC/lib/." "$APP/lib/"
    for f in romexplorer_server romexplorer_worker romexplorer_dbtool romexplorer-server-start LICENSE; do
        if [[ -f "$SRC/$f" ]]; then cp -af "$SRC/$f" "$PREFIX/$f"; fi
    done
    for shared in icons gui_web; do
        if [[ -d "$SRC/assets/$shared" ]]; then
            rm -rf "$APP/assets/$shared"
            mkdir -p "$APP/assets"
            cp -a "$SRC/assets/$shared" "$APP/assets/"
        fi
    done
    note "replaced the executables, lib/ and the shipped assets; kept data/ and everything else in assets/"
else
    step "installing into $PREFIX"
    if [[ -d "$PREFIX" ]] && [[ -n "$(ls -A "$PREFIX" 2>/dev/null)" ]]; then
        confirm "$PREFIX is not empty and has no database in it; replace its contents?" || die "left $PREFIX alone"
        # The contents, not the directory: a prefix someone else created for us (a mount
        # point, a root-made directory we were given write access to) cannot be removed.
        find "$PREFIX" -mindepth 1 -delete
    fi
    mkdir -p "$PREFIX"
    # Entry by entry rather than `cp -a "$SRC/." "$PREFIX/"`, which also sets the times on
    # $PREFIX itself and fails with "Operation not permitted" when we can write to it but do
    # not own it.
    (
        shopt -s dotglob nullglob
        cp -a "$SRC"/* "$PREFIX"/
    )
    note "copied the package"
fi

# The installer and its example config are useful next to the install (upgrades, uninstall),
# but the settings file itself must not stay: it holds passwords.
for extra in install.sh romexplorer.conf.example; do
    [[ -f "$SRC/$extra" ]] && cp -f "$SRC/$extra" "$PREFIX/$extra" 2>/dev/null || true
done
chmod +x "$PREFIX/install.sh" 2>/dev/null || true
[[ -f "$PREFIX/romexplorer-server-start" ]] && chmod +x "$PREFIX/romexplorer-server-start"

mkdir -p "$APP/data" "$APP/assets/media" "$APP/assets/cache" "$APP/assets/download/dats"

# --- the files the first start consumes ----------------------------------------------------
# Both hold plaintext passwords and both are deleted by the server once applied, so they are
# written owner-only and never logged.
if [[ "$UPGRADE" -eq 1 || "$BUNDLED_DB" -eq 0 ]] && [[ -f "$APP/data/database.env" ]]; then
    note "setup already ran here, or the database is external; leaving the existing accounts and database alone"
else
    step "writing the first-start setup files"
    setup_file="$APP/data/setup.env"
    umask 077
    {
        printf '# Written by install.sh. The server applies this on its first start and then\n'
        printf '# deletes it -- it holds passwords. See src/api/setup.rs.\n'
        printf 'database=%s\n' "$DB_NAME"
        printf 'root_password=%s\n' "$DB_ROOT_PASS"
        printf 'admin_user=%s\n' "$DB_ADMIN_USER"
        printf 'admin_password=%s\n' "$DB_ADMIN_PASS"
        [[ -n "$DB_APP_USER" ]] && printf 'app_user=%s\napp_password=%s\n' "$DB_APP_USER" "$DB_APP_PASS"
        printf 'db_port=%s\n' "$DB_PORT"
        printf 'server_port=%s\n' "$PORT"
    } > "$setup_file"
    chmod 600 "$setup_file"
    note "data/setup.env (database accounts)"

    auth_file="$APP/data/auth.env"
    {
        printf '# Written by install.sh. Applied once, while no admin can sign in yet, then\n'
        printf '# deleted. See src/api/auth.rs::SeedFile.\n'
        printf 'admin_username=%s\n' "$ADMIN_USER"
        printf 'admin_password=%s\n' "$ADMIN_PASS"
        # The extra accounts, copied across verbatim. `#` as the delimiter, because `|` is
        # the alternation operator inside the expression.
        if [[ -n "$CONFIG" && -f "$CONFIG" ]]; then
            sed -n -E 's#^[[:space:]]*((user|upload)\.[A-Za-z0-9._@-]+)[[:space:]]*=[[:space:]]*(.*)$#\1=\3#p' "$CONFIG"
        fi
    } > "$auth_file"
    chmod 600 "$auth_file"
    note "data/auth.env (the console login)"
    umask 022

    # StoredConfig carries these, and setup preserves whatever is already in database.env --
    # so a pre-seeded file is how sharing gets turned on from the config.
    case "$(printf '%s' "$SHARE_ENABLED" | tr 'A-Z' 'a-z')" in
        1|true|yes|on)
            set_kv "$APP/data/database.env" share_enabled "true"
            case "$(printf '%s' "$SHARE_ADVERTISE" | tr 'A-Z' 'a-z')" in
                1|true|yes|on) set_kv "$APP/data/database.env" share_advertise "true" ;;
            esac
            [[ -n "$DB_DATADIR" ]] && set_kv "$APP/data/database.env" db_datadir "$DB_DATADIR"
            chmod 600 "$APP/data/database.env"
            note "data/database.env pre-seeded with the share settings"
            ;;
        *)
            if [[ -n "$DB_DATADIR" ]]; then
                set_kv "$APP/data/database.env" db_datadir "$DB_DATADIR"
                chmod 600 "$APP/data/database.env"
                note "data/database.env pre-seeded with db_datadir=$DB_DATADIR"
            fi
            ;;
    esac
fi

if [[ -n "$DB_DATADIR" ]]; then
    mkdir -p "$DB_DATADIR"
    [[ "$SYSTEM" -eq 1 ]] && chown -R "$SVC_USER" "$DB_DATADIR"
fi

# Everything under the prefix, because the bundled database writes inside it and the app
# writes media, caches and downloads there too.
if [[ "$SYSTEM" -eq 1 ]]; then
    chown -R "$SVC_USER" "$PREFIX"
    note "chown -R $SVC_USER $PREFIX"
fi

# --- the service ---------------------------------------------------------------------------
EXEC="$PREFIX/romexplorer-server-start"
[[ -x "$EXEC" ]] || EXEC="$PREFIX/romexplorer_server"

write_systemd_unit() {
    mkdir -p "$(dirname "$UNIT_PATH")"
    {
        cat <<EOF
[Unit]
Description=romexplorer server
Documentation=https://github.com/$REPO
After=network-online.target
Wants=network-online.target

[Service]
Type=simple
EOF
        [[ "$SYSTEM" -eq 1 ]] && printf 'User=%s\n' "$SVC_USER"
        cat <<EOF
WorkingDirectory=$PREFIX
ExecStart=$EXEC --bind $BIND --port $PORT
Environment=ROMEXPLORER_SERVICE_MANAGED=1
Environment=ROMEXPLORER_APP_ROOT=$APP

# The console's "restart" asks to be started again by exiting 75 (EX_TEMPFAIL,
# lifecycle::RESTART_EXIT_CODE) rather than forking a second copy that would fight this one
# for the port. Both lines are needed: SuccessExitStatus so the restart is not logged as a
# failure, RestartForceExitStatus so it happens even under Restart=on-failure semantics.
# on-failure, not always: the console's "shutdown" exits 0, and under always systemd started
# the server again two seconds later -- a shutdown that never shut anything down. A crash
# still restarts it, and so does a reboot (the unit is enabled).
Restart=on-failure
RestartSec=2
SuccessExitStatus=75
RestartForceExitStatus=75

# Long enough for the bundled MariaDB to close its tablespaces. Cut it short and InnoDB
# crash-recovers on the next boot, every boot.
TimeoutStopSec=120
KillSignal=SIGTERM
KillMode=mixed

NoNewPrivileges=yes
PrivateTmp=yes
ProtectSystem=full
# Deliberately not ProtectHome: a media_dir is very often under /home, and the server has
# to be able to read it. Add ReadWritePaths= lines here for media kept outside $PREFIX.
ReadWritePaths=$PREFIX
EOF
        [[ -n "$DB_DATADIR" ]] && printf 'ReadWritePaths=%s\n' "$DB_DATADIR"
        printf '\n[Install]\n'
        if [[ "$SYSTEM" -eq 1 ]]; then printf 'WantedBy=multi-user.target\n'; else printf 'WantedBy=default.target\n'; fi
    } > "$UNIT_PATH"
    note "wrote $UNIT_PATH"
}

write_launchd_plist() {
    mkdir -p "$(dirname "$PLIST_PATH")"
    cat > "$PLIST_PATH" <<EOF
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>Label</key><string>com.romexplorer.server</string>
    <key>ProgramArguments</key>
    <array>
        <string>$EXEC</string>
        <string>--bind</string><string>$BIND</string>
        <string>--port</string><string>$PORT</string>
    </array>
    <key>EnvironmentVariables</key>
    <dict>
        <key>ROMEXPLORER_SERVICE_MANAGED</key><string>1</string>
        <key>ROMEXPLORER_APP_ROOT</key><string>$APP</string>
    </dict>
    <key>WorkingDirectory</key><string>$PREFIX</string>
    <!-- RunAtLoad is the "comes back after a reboot" half; KeepAlive/SuccessfulExit=false is
         the "comes back after a crash, and after the console's exit-75 restart" half, while
         still letting a deliberate clean exit stay stopped. -->
    <key>RunAtLoad</key><true/>
    <key>KeepAlive</key>
    <dict><key>SuccessfulExit</key><false/></dict>
    <key>ExitTimeOut</key><integer>120</integer>
    <key>StandardOutPath</key><string>$APP/data/server.log</string>
    <key>StandardErrorPath</key><string>$APP/data/server.log</string>
EOF
    [[ "$SYSTEM" -eq 1 ]] && printf '    <key>UserName</key><string>%s</string>\n' "$SVC_USER" >> "$PLIST_PATH"
    cat >> "$PLIST_PATH" <<'EOF'
</dict>
</plist>
EOF
    note "wrote $PLIST_PATH"
}

case "$SERVICE" in
    systemd|systemd-user)
        have systemctl || die "no systemctl here; re-run with --service none and start $EXEC yourself"
        step "installing the systemd $([[ "$SYSTEM" -eq 1 ]] && echo system || echo user) unit"
        write_systemd_unit
        "${SYSTEMCTL[@]}" daemon-reload
        "${SYSTEMCTL[@]}" enable "$SERVICE_NAME" >/dev/null
        note "enabled at boot"
        if [[ "$SERVICE" == "systemd-user" ]]; then
            # Without lingering a user unit stops at logout and does not come back until the
            # next login -- which is not "starts after a reboot".
            if have loginctl; then
                loginctl enable-linger "$(id -un)" 2>/dev/null \
                    && note "lingering enabled for $(id -un), so the unit starts at boot without a login" \
                    || warn "could not enable lingering; the service will only run while you are logged in (sudo loginctl enable-linger $(id -un))"
            else
                warn "no loginctl; the service will only run while you are logged in"
            fi
        fi
        ;;
    launchd)
        step "installing the launchd $([[ "$SYSTEM" -eq 1 ]] && echo daemon || echo agent)"
        write_launchd_plist
        ;;
    none)
        note "no service installed (--service none); start it with: $EXEC --bind $BIND --port $PORT"
        ;;
esac

# --- start it and watch it come up ---------------------------------------------------------
service_logs() {
    case "$SERVICE" in
        systemd|systemd-user) have journalctl && journalctl $([[ "$SERVICE" == "systemd-user" ]] && echo --user) -u "$SERVICE_NAME" -n 40 --no-pager 2>/dev/null || true ;;
        launchd) [[ -f "$APP/data/server.log" ]] && tail -n 40 "$APP/data/server.log" || true ;;
    esac
}

if [[ "$START" -eq 1 && "$SERVICE" != "none" ]]; then
    step "starting the service"
    case "$SERVICE" in
        systemd|systemd-user) "${SYSTEMCTL[@]}" restart "$SERVICE_NAME" ;;
        launchd) launchctl load -w "$PLIST_PATH" ;;
    esac

    # First start does setup and the first migration, so give it real time. The listener
    # answering is the signal that all of it finished.
    waited=0
    ready=0
    while [[ "$waited" -lt 300 ]]; do
        if port_open "$([[ "$BIND" == "0.0.0.0" ]] && echo 127.0.0.1 || echo "$BIND")" "$PORT"; then ready=1; break; fi
        sleep 2; waited=$((waited + 2))
        [[ $((waited % 30)) -eq 0 ]] && note "still starting (${waited}s)..."
    done

    if [[ "$ready" -eq 1 ]]; then
        note "listening on $BIND:$PORT after ${waited}s"
    else
        printf '\n' >&2
        warn "the service did not answer on $BIND:$PORT within ${waited}s. Its log:"
        service_logs >&2
        die "install finished but the server is not up; fix what the log says and run: $(printf '%s' "${SYSTEMCTL[*]}") restart $SERVICE_NAME"
    fi
fi

# --- what the operator needs to know -------------------------------------------------------
scheme="https"; [[ "$BIND" == "127.0.0.1" || "$BIND" == "localhost" ]] && scheme="http"
printf '\n'
printf 'romexplorer is installed.\n\n'
printf '  console    %s://%s:%s/\n' "$scheme" "$([[ "$BIND" == "0.0.0.0" ]] && hostname || printf '%s' "$BIND")" "$PORT"
printf '  install    %s\n' "$PREFIX"
case "$SERVICE" in
    systemd)      printf '  service    systemctl {status,restart,stop} %s\n' "$SERVICE_NAME" ;;
    systemd-user) printf '  service    systemctl --user {status,restart,stop} %s\n' "$SERVICE_NAME" ;;
    launchd)      printf '  service    launchctl {unload,load -w} %s\n' "$PLIST_PATH" ;;
esac
printf '  check      %s/romexplorer_dbtool check\n' "$PREFIX"

if [[ "$UPGRADE" -eq 0 ]]; then
    printf '\n'
    if [[ "$GENERATED_ADMIN_PASS" -eq 1 ]]; then
        printf 'The console login, generated just now and shown only here:\n\n'
        printf '    username  %s\n' "$ADMIN_USER"
        printf '    password  %s\n\n' "$ADMIN_PASS"
        printf 'Write it down before closing this window. To change it later:\n'
        printf '    %s/romexplorer_server reset-admin --username %s\n' "$PREFIX" "$ADMIN_USER"
    else
        printf 'Sign in as "%s" with the password from %s.\n' "$ADMIN_USER" "$(basename "$CONFIG")"
    fi
    if [[ "$BIND" == "127.0.0.1" ]]; then
        # %s and not a bare format string: a format beginning with "--" is parsed as printf's
        # own options.
        printf '\n%s\n' 'This server answers only on this machine. To serve the network, re-run'
        printf '%s\n' 'with --bind 0.0.0.0 (it serves TLS with a certificate made on first start).'
    fi
fi
printf '\n'
