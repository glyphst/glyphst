#!/usr/bin/env bash
# Run a command against a private headless sway compositor rendering on the GPU.
#
# Reader screenshot runs need a real Wayland surface. This compositor is
# independent of the desktop session, so its windows never appear on the
# desktop or take focus, and runs continue while the screen is locked or the
# displays are off. Sleep/idle are inhibited while it runs. The command sees
# GLYPHST_HEADLESS=1.
#
# Usage: script/headless-session.sh [--] command [args...]
# Environment:
#   GLYPHST_HEADLESS_MODE   output mode (default 3072x1920@120Hz)
#   GLYPHST_HEADLESS_SCALE  output scale (default 1.5, matching the desk display)
#   GLYPHST_RENDER_NODE     DRM render node (default: first /dev/dri/renderD*)
#   GLYPHST_SWAY            sway executable (default: the system's, then PATH)
#
# sway must come from the same nixpkgs as the system's Mesa driver
# (/run/opengl-driver); a dev-shell sway with an older glibc cannot load GBM.

set -euo pipefail
shopt -s nullglob

if [[ ${1:-} == "--" ]]; then
    shift
fi
if (($# == 0)); then
    printf 'usage: %s [--] command [args...]\n' "$0" >&2
    exit 2
fi

if [[ -z ${GLYPHST_HEADLESS_INHIBITED:-} ]] && command -v systemd-inhibit > /dev/null; then
    export GLYPHST_HEADLESS_INHIBITED=1
    exec systemd-inhibit --what=sleep:idle --who=glyphst-headless \
        --why="unattended Reader checks" --mode=block "$0" "$@"
fi

sway=${GLYPHST_SWAY:-}
if [[ -z ${sway} && -x /run/current-system/sw/bin/sway ]]; then
    sway=/run/current-system/sw/bin/sway
fi
sway=${sway:-$(command -v sway || true)}
[[ -n ${sway} ]] || {
    printf 'sway is not installed; add it to the system packages or set GLYPHST_SWAY, e.g.\n' >&2
    printf '  GLYPHST_SWAY=$(nix build --no-link --print-out-paths --inputs-from ~/nixos-config nixpkgs#sway)/bin/sway\n' >&2
    exit 1
}

render_nodes=(/dev/dri/renderD*)
render_node=${GLYPHST_RENDER_NODE:-${render_nodes[0]:-}}
[[ -n ${render_node} && -e ${render_node} ]] || {
    printf 'no DRM render node found\n' >&2
    exit 1
}

runtime=$(mktemp -d "${TMPDIR:-/tmp}/glyphst-headless.XXXXXX")
sway_pid=
cleanup() {
    if [[ -n ${sway_pid} ]]; then
        kill "${sway_pid}" 2> /dev/null || true
        wait "${sway_pid}" 2> /dev/null || true
    fi
    rm -rf "${runtime}"
}
trap cleanup EXIT INT TERM

cat > "${runtime}/sway.conf" << EOF
output HEADLESS-1 mode ${GLYPHST_HEADLESS_MODE:-3072x1920@120Hz} scale ${GLYPHST_HEADLESS_SCALE:-1.5}
default_border none
floating_maximum_size -1 x -1
default_floating_border none
# Keep the size each client requests instead of tiling it to the output.
for_window [app_id=".*"] floating enable
for_window [class=".*"] floating enable
EOF

env -u WAYLAND_DISPLAY -u DISPLAY -u SWAYSOCK \
    XDG_RUNTIME_DIR="${runtime}" \
    WLR_BACKENDS=headless \
    WLR_LIBINPUT_NO_DEVICES=1 \
    WLR_RENDERER=vulkan \
    WLR_RENDER_DRM_DEVICE="${render_node}" \
    "${sway}" --config "${runtime}/sway.conf" > "${runtime}/sway.log" 2>&1 &
sway_pid=$!

socket=
for _ in $(seq 1 100); do
    sockets=("${runtime}"/wayland-[0-9])
    socket=${sockets[0]:-}
    [[ -n ${socket} && -S ${socket} ]] && break
    if ! kill -0 "${sway_pid}" 2> /dev/null; then
        break
    fi
    sleep 0.1
done
if [[ -z ${socket} ]]; then
    printf 'headless sway did not start; log follows\n' >&2
    cat "${runtime}/sway.log" >&2
    exit 1
fi

printf 'headless sway on %s (%s, scale %s)\n' "${render_node}" \
    "${GLYPHST_HEADLESS_MODE:-3072x1920@120Hz}" "${GLYPHST_HEADLESS_SCALE:-1.5}" >&2

# An absolute WAYLAND_DISPLAY lets clients keep the caller's XDG_RUNTIME_DIR.
status=0
env -u DISPLAY WAYLAND_DISPLAY="${socket}" GLYPHST_HEADLESS=1 "$@" || status=$?
exit "${status}"
