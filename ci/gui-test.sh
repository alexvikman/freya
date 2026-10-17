#!/bin/sh
# Runs Freya on a virtual X11 screen (Xvfb) and in a headless Wayland compositor (Sway),
# with the native spectrum on and off, and writes screenshots, logs and CPU use to /out.
# Run by `ci/check.sh gui`. Without a sound device Freya plays silently in real time.
set -eu

if [ "$(id -u)" = 0 ]; then
    cargo build --release -p freya
    chmod 777 /out
    exec su tester -s /bin/sh -c "sh $0"
fi

FREYA=/target/release/freya
export XDG_RUNTIME_DIR=/tmp/runtime-tester RUST_LOG=freya=debug
mkdir -p "$XDG_RUNTIME_DIR" "$HOME/.config/freya" && chmod 700 "$XDG_RUNTIME_DIR"
printf 'volume = 0.0\nwindow_size = [560.0, 680.0]\n' > "$HOME/.config/freya/settings.toml"

# Ten seconds of noise (22,050 Hz, mono, 16-bit): a spectrum that changes every frame. The
# header is written byte by byte; sizes are little-endian: 441,036 and 441,000 bytes.
noise=/tmp/noise.wav
{
    printf 'RIFF\314\272\006\000WAVEfmt \020\000\000\000\001\000\001\000'
    printf '\042\126\000\000\104\254\000\000\002\000\020\000data\250\272\006\000'
    head -c 441000 /dev/urandom
} > $noise

cpu() { # CPU time used by process $1, in hundredths of a second (user + system)
    awk '{ print $14 + $15 }' "/proc/$1/stat"
}

# Plays the noise with the native spectrum on or off ($2), measures CPU over 5 seconds after
# 2 seconds of warm-up, and takes a screenshot with $3.
run() {
    name="$1-native-$2"
    # RENDERER is an option and its value: split into words on purpose.
    # shellcheck disable=SC2086
    FREYA_NATIVE_SPECTRUM=$2 $FREYA ${RENDERER:-} $noise >"/out/$name.log" 2>&1 &
    pid=$!
    sleep 2
    before=$(cpu $pid); sleep 5; after=$(cpu $pid)
    $3 "/out/$name.png"
    kill $pid; wait $pid 2>/dev/null || true
    used=$((after - before))
    echo "$name: $((used / 5)).$((used % 5 * 2)) % CPU" | tee -a /out/results.txt
}

: > /out/results.txt

echo "== X11 (Xvfb, OpenGL in software)"
Xvfb :99 -screen 0 800x720x24 >/dev/null 2>&1 &
xvfb=$!
sleep 1
export DISPLAY=:99
x11_shot() { xwd -root -silent | convert xwd:- "$1"; }
run x11 off x11_shot
run x11 on x11_shot
kill $xvfb
unset DISPLAY

echo "== Wayland (headless Sway, software renderer)"
printf 'output HEADLESS-1 resolution 800x720\ndefault_border none\n' > /tmp/sway.conf
WLR_BACKENDS=headless WLR_RENDERER=pixman WLR_LIBINPUT_NO_DEVICES=1 \
    sway -c /tmp/sway.conf >/out/sway.log 2>&1 &
sway=$!
sleep 2
for socket in "$XDG_RUNTIME_DIR"/wayland-[0-9]*; do
    case "$socket" in *.lock) continue ;; esac
    WAYLAND_DISPLAY=$(basename "$socket")
    break
done
export WAYLAND_DISPLAY RENDERER="--renderer software"
run wayland off grim
run wayland on grim
kill $sway

grep -h "drawn by Freya\|no native spectrum\|panicked\|Broken pipe" /out/*.log || true
