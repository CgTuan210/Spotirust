#!/usr/bin/env python3
"""
Reproducible Desktop GUI & Audio Benchmark Harness for Spotirust.

IMPORTANT ENVIRONMENT CLASSIFICATION:
- Display: Virtual X11 framebuffer (Xvfb) — NOT a physical desktop compositor.
- Graphics: Mesa LLVMpipe software OpenGL (LIBGL_ALWAYS_SOFTWARE=1) — NOT hardware GPU.
- Audio: Timer-paced PulseAudio `module-null-sink` — NOT physical speakers/DAC.

Guarantees:
1. Measures strictly the exact `spotirust` child PID (verifies `/proc/<pid>/comm`).
2. Confirms Xvfb and PulseAudio are ready BEFORE starting application startup timers.
3. Never uses stale numbers; runs fresh isolated temp `HOME`/`XDG_*` directories every run.
4. Fails loudly if PulseAudio sink or real `rodio` local audio playback fails to start.
5. Cleans up all spawned processes (`spotirust`, `Xvfb`, `pulseaudio`) in `finally`.
6. Emits structured JSON + human-readable summary for apples-to-apples Before/After comparisons.
"""

import argparse
import json
import os
import platform
import re
import shutil
import subprocess
import sys
import tempfile
import time
from pathlib import Path

CLK_TCK = os.sysconf(os.sysconf_names["SC_CLK_TCK"])
ANSI_RE = re.compile(r"\x1b\[[0-9;]*m")


def strip_ansi(text: str) -> str:
    return ANSI_RE.sub("", text)


def collect_env_info(bin_path: Path) -> dict:
    cpu_model = "unknown"
    try:
        for line in Path("/proc/cpuinfo").read_text().splitlines():
            if line.startswith("model name"):
                cpu_model = line.split(":", 1)[1].strip()
                break
    except OSError:
        pass

    mem_total_mb = 0.0
    try:
        for line in Path("/proc/meminfo").read_text().splitlines():
            if line.startswith("MemTotal:"):
                mem_total_mb = int(line.split()[1]) / 1024.0
                break
    except OSError:
        pass

    rustc_ver = subprocess.run(
        ["rustc", "--version"], capture_output=True, text=True
    ).stdout.strip()

    return {
        "timestamp_utc": time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime()),
        "os": f"{platform.system()} {platform.release()} ({platform.machine()})",
        "cpu_model": cpu_model,
        "vcpus": os.cpu_count() or 1,
        "ram_total_mb": round(mem_total_mb, 1),
        "rustc": rustc_ver,
        "binary_path": str(bin_path),
        "binary_size_mb": round(bin_path.stat().st_size / (1024 * 1024), 2),
        "display_server": "Xvfb 1280x800x24 (virtual framebuffer, NOT physical desktop)",
        "gpu_renderer": "Mesa LLVMpipe software OpenGL (LIBGL_ALWAYS_SOFTWARE=1, NOT hardware GPU)",
        "audio_backend": "PulseAudio module-null-sink (timer-paced virtual sink, NOT physical speakers)",
    }


def verify_spotirust_pid(pid: int):
    comm = Path(f"/proc/{pid}/comm").read_text().strip()
    if comm != "spotirust":
        raise RuntimeError(f"PID {pid} is '{comm}', expected 'spotirust'")


def read_mem_kb(pid: int) -> tuple[int, int, int]:
    verify_spotirust_pid(pid)
    rss = hwm = threads = 0
    for line in Path(f"/proc/{pid}/status").read_text().splitlines():
        if line.startswith("VmRSS:"):
            rss = int(line.split()[1])
        elif line.startswith("VmHWM:"):
            hwm = int(line.split()[1])
        elif line.startswith("Threads:"):
            threads = int(line.split()[1])
    return rss, hwm, threads


def read_proc_ticks(pid: int) -> int:
    rest = Path(f"/proc/{pid}/stat").read_text().rsplit(")", 1)[1].split()
    return int(rest[11]) + int(rest[12])


def read_thread_ticks(pid: int) -> dict[str, tuple[str, int]]:
    res = {}
    task_dir = Path(f"/proc/{pid}/task")
    for entry in task_dir.iterdir():
        tid = entry.name
        try:
            name = (entry / "comm").read_text().strip()
            rest = (entry / "stat").read_text().rsplit(")", 1)[1].split()
            res[tid] = (name, int(rest[11]) + int(rest[12]))
        except OSError:
            pass
    return res


def measure_window_cpu(pid: int, duration_s: float) -> dict:
    verify_spotirust_pid(pid)
    t0 = time.monotonic()
    p0 = read_proc_ticks(pid)
    th0 = read_thread_ticks(pid)
    time.sleep(duration_s)
    t1 = time.monotonic()
    p1 = read_proc_ticks(pid)
    th1 = read_thread_ticks(pid)

    dt = max(t1 - t0, 1e-6)
    total_pct = ((p1 - p0) / CLK_TCK) / dt * 100.0

    buckets = {
        "rust_ui": 0.0,
        "mesa_swgl": 0.0,
        "audio_rodio_cpal": 0.0,
        "tokio_other": 0.0,
    }
    for tid, (name, c1) in th1.items():
        c0 = th0.get(tid, (name, 0))[1]
        pct = ((c1 - c0) / CLK_TCK) / dt * 100.0
        if name == "spotirust":
            buckets["rust_ui"] += pct
        elif "llvmpipe" in name or "gallium" in name or "mesa" in name or "disk_cache" in name:
            buckets["mesa_swgl"] += pct
        elif "cpal" in name or "rodio" in name or "alsa" in name or "pulse" in name:
            buckets["audio_rodio_cpal"] += pct
        else:
            buckets["tokio_other"] += pct

    rss, hwm, threads = read_mem_kb(pid)
    return {
        "duration_s": round(dt, 2),
        "vm_rss_mb": round(rss / 1024.0, 2),
        "vm_hwm_mb": round(hwm / 1024.0, 2),
        "threads": threads,
        "total_cpu_pct": round(total_pct, 2),
        "rust_ui_cpu_pct": round(buckets["rust_ui"], 2),
        "audio_cpu_pct": round(buckets["audio_rodio_cpal"], 2),
        "tokio_other_cpu_pct": round(buckets["tokio_other"], 2),
        "mesa_swgl_cpu_pct": round(buckets["mesa_swgl"], 2),
    }


def terminate_proc(proc: subprocess.Popen | None):
    if proc is None or proc.poll() is not None:
        return
    proc.terminate()
    try:
        proc.wait(timeout=3)
    except subprocess.TimeoutExpired:
        proc.kill()
        proc.wait(timeout=3)


def wait_xvfb_ready(display: str, env: dict, timeout_s: float = 5.0):
    deadline = time.monotonic() + timeout_s
    sock = Path(f"/tmp/.X11-unix/X{display.lstrip(':')}")
    while time.monotonic() < deadline:
        if sock.exists():
            # Verify X11 accepts commands via xdotool
            r = subprocess.run(
                ["xdotool", "getdisplaygeometry"],
                capture_output=True, text=True, env=env
            )
            if r.returncode == 0:
                return
        time.sleep(0.05)
    raise RuntimeError(f"Xvfb on {display} did not become ready within {timeout_s}s")


def wait_pulse_ready(env: dict, timeout_s: float = 5.0):
    deadline = time.monotonic() + timeout_s
    while time.monotonic() < deadline:
        r = subprocess.run(["pactl", "info"], capture_output=True, text=True, env=env)
        if r.returncode == 0 and "spotirust_virtual" in subprocess.run(
            ["pactl", "list", "short", "sinks"], capture_output=True, text=True, env=env
        ).stdout:
            return
        time.sleep(0.05)
    raise RuntimeError("PulseAudio virtual null-sink did not become ready")


def get_window_geometry(env: dict) -> tuple[str | None, dict | None]:
    w = subprocess.run(
        ["xdotool", "search", "--name", "^Spotirust$"],
        capture_output=True, text=True, env=env
    )
    ids = [x.strip() for x in w.stdout.splitlines() if x.strip()]
    if not ids:
        return None, None
    wid = ids[-1]
    g = subprocess.run(
        ["xdotool", "getwindowgeometry", "--shell", wid],
        capture_output=True, text=True, env=env
    )
    geo = {}
    for line in g.stdout.splitlines():
        if "=" in line:
            k, v = line.split("=", 1)
            geo[k.strip()] = int(v.strip())
    return wid, geo


def click_rel(geo: dict, rx: int, ry: int, env: dict):
    x = geo.get("X", 0) + int(rx)
    y = geo.get("Y", 0) + int(ry)
    subprocess.run(["xdotool", "mousemove", "--sync", str(x), str(y)], env=env, check=True)
    time.sleep(0.08)
    subprocess.run(["xdotool", "click", "1"], env=env, check=True)


def run_cold_startup_bench(bin_path: Path, runs: int, display: str) -> dict:
    ui_init_ms = []
    mapped_ms_list = []

    for _ in range(runs):
        with tempfile.TemporaryDirectory(prefix="spotirust_bench_boot_") as tmp:
            env = os.environ.copy()
            env["HOME"] = tmp
            env["XDG_DATA_HOME"] = os.path.join(tmp, "data")
            env["XDG_CACHE_HOME"] = os.path.join(tmp, "cache")
            env["DISPLAY"] = display
            env["LIBGL_ALWAYS_SOFTWARE"] = "1"
            env["RUST_LOG"] = "spotirust=info"

            xvfb = subprocess.Popen(
                ["Xvfb", display, "-screen", "0", "1280x800x24", "-nolisten", "tcp"],
                stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL
            )
            app = None
            try:
                # Guarantee Xvfb is completely up BEFORE starting Spotirust timer
                wait_xvfb_ready(display, env)
                log_path = Path(tmp) / "boot.log"
                with log_path.open("w") as lf:
                    t0 = time.monotonic()
                    app = subprocess.Popen([str(bin_path)], stdout=lf, stderr=subprocess.STDOUT, env=env)
                    mapped = None
                    for _ in range(160):
                        if app.poll() is not None:
                            raise RuntimeError(f"Spotirust exited prematurely during startup: {log_path.read_text()}")
                        wid, _ = get_window_geometry(env)
                        if wid:
                            mapped = (time.monotonic() - t0) * 1000.0
                            break
                        time.sleep(0.025)
                    if mapped is None:
                        raise RuntimeError("Spotirust X11 window did not map within 4s")
                    time.sleep(0.35)
                    terminate_proc(app)
                log_text = strip_ansi(log_path.read_text(errors="replace"))
                m = re.search(r"startup_ms=(\d+)", log_text)
                if not m:
                    raise RuntimeError(f"Could not find startup_ms in log:\n{log_text}")
                ui_init_ms.append(int(m.group(1)))
                mapped_ms_list.append(round(mapped, 1))
            finally:
                terminate_proc(app)
                terminate_proc(xvfb)

    return {
        "runs": runs,
        "ui_init_ms": ui_init_ms,
        "ui_init_avg_ms": round(sum(ui_init_ms) / len(ui_init_ms), 1),
        "x11_mapped_ms": mapped_ms_list,
        "x11_mapped_avg_ms": round(sum(mapped_ms_list) / len(mapped_ms_list), 1),
    }


def run_live_session_bench(bin_path: Path, sample_s: float, display: str) -> dict:
    with tempfile.TemporaryDirectory(prefix="spotirust_bench_live_") as tmp:
        runtime_dir = os.path.join(tmp, "run")
        os.makedirs(runtime_dir, exist_ok=True)
        os.chmod(runtime_dir, 0o700)

        with open(os.path.join(tmp, ".asoundrc"), "w") as f:
            f.write("pcm.!default { type pulse }\nctl.!default { type pulse }\n")

        env = os.environ.copy()
        env["HOME"] = tmp
        env["XDG_RUNTIME_DIR"] = runtime_dir
        env["XDG_DATA_HOME"] = os.path.join(tmp, "data")
        env["XDG_CACHE_HOME"] = os.path.join(tmp, "cache")
        env["DISPLAY"] = display
        env["LIBGL_ALWAYS_SOFTWARE"] = "1"
        env["RUST_LOG"] = "spotirust=debug"

        pa = subprocess.Popen(
            [
                "pulseaudio", "--disallow-exit", "--exit-idle-time=-1",
                "--disable-shm=yes", "-n",
                "-L", "module-null-sink sink_name=spotirust_virtual",
                "-L", "module-native-protocol-unix",
            ],
            env=env, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL
        )
        xvfb = subprocess.Popen(
            ["Xvfb", display, "-screen", "0", "1280x800x24", "-nolisten", "tcp"],
            stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL
        )
        app = None
        log_path = Path(tmp) / "live.log"

        try:
            wait_pulse_ready(env)
            wait_xvfb_ready(display, env)

            with log_path.open("w") as lf:
                app = subprocess.Popen([str(bin_path)], stdout=lf, stderr=subprocess.STDOUT, env=env)
                wid = geo = None
                for _ in range(160):
                    if app.poll() is not None:
                        raise RuntimeError(f"Spotirust crashed on launch:\n{log_path.read_text()}")
                    wid, geo = get_window_geometry(env)
                    if wid:
                        break
                    time.sleep(0.05)
                if not wid or not geo:
                    raise RuntimeError("Spotirust window did not appear")

                # Wait for initial Soundcheck WAV generation & initial paint to settle
                time.sleep(2.0)

                # 1. Idle GUI (Home)
                idle = measure_window_cpu(app.pid, sample_s)

                # 2. Start real Local WAV playback (Midnight Compiler, 22.0s) via Home card
                click_rel(geo, 330, 445, env)
                time.sleep(1.0)
                log_now = strip_ansi(log_path.read_text(errors="replace"))
                if "Midnight Compiler" not in log_now:
                    click_rel(geo, 330, 500, env)
                    time.sleep(1.0)
                    log_now = strip_ansi(log_path.read_text(errors="replace"))
                if "Midnight Compiler" not in log_now:
                    raise RuntimeError(
                        f"Audio playback did not start! Log:\n{log_now}"
                    )

                # Verify PulseAudio actually has an active sink-input stream from Spotirust!
                sink_inputs = subprocess.run(
                    ["pactl", "list", "short", "sink-inputs"],
                    capture_output=True, text=True, env=env
                ).stdout.strip()
                if not sink_inputs:
                    raise RuntimeError(
                        "PulseAudio reported 0 active sink-inputs — rodio did not stream real audio!"
                    )

                # Measure Playing Local WAV (Home screen, bottom progress bar only)
                playing_bar = measure_window_cpu(app.pid, sample_s)

                # 3. Open Fullscreen Now Playing + Word-by-Word Karaoke
                w_px = geo.get("WIDTH", 1240)
                click_rel(geo, w_px - 68, 24, env)
                time.sleep(0.8)
                # Ensure track is still actively playing (if near end, restart Midnight Compiler)
                sink_inputs_kar = subprocess.run(
                    ["pactl", "list", "short", "sink-inputs"],
                    capture_output=True, text=True, env=env
                ).stdout.strip()
                if not sink_inputs_kar:
                    # Press Previous or Space if needed
                    subprocess.run(["xdotool", "key", "space"], env=env, check=False)
                    time.sleep(0.5)

                playing_karaoke = measure_window_cpu(app.pid, sample_s)

                # 4. Continuous RAM stability across auto-advance & playback finish (30s)
                ram_timeline = []
                for step in range(7):
                    rss, hwm, thr = read_mem_kb(app.pid)
                    ram_timeline.append({
                        "t_s": step * 5,
                        "vm_rss_mb": round(rss / 1024.0, 2),
                        "vm_hwm_mb": round(hwm / 1024.0, 2),
                        "threads": thr,
                    })
                    if step < 6:
                        time.sleep(5.0)

                # 5. Post-stop idle verification (wait briefly if track 2 still finishing)
                for _ in range(12):
                    si = subprocess.run(
                        ["pactl", "list", "short", "sink-inputs"],
                        capture_output=True, text=True, env=env
                    ).stdout.strip()
                    if not si:
                        break
                    time.sleep(1.0)
                time.sleep(1.0)
                post_stop_idle = measure_window_cpu(app.pid, 5.0)

                terminate_proc(app)

            final_log = strip_ansi(log_path.read_text(errors="replace"))
            auto_advanced = "Velocity Pulse" in final_log
            return {
                "window_geometry": geo,
                "pulse_sink_input_verified": True,
                "auto_advance_verified": auto_advanced,
                "idle_home": idle,
                "playing_local_wav_bar": playing_bar,
                "playing_fullscreen_karaoke": playing_karaoke,
                "post_stop_idle": post_stop_idle,
                "ram_timeline_30s": ram_timeline,
                "ram_delta_30s_mb": round(
                    ram_timeline[-1]["vm_rss_mb"] - ram_timeline[0]["vm_rss_mb"], 2
                ),
            }
        finally:
            terminate_proc(app)
            terminate_proc(xvfb)
            terminate_proc(pa)


def run_headless_diagnostics(bin_path: Path) -> str:
    r = subprocess.run([str(bin_path), "--diagnostics"], capture_output=True, text=True, check=True)
    return strip_ansi(r.stdout).strip()


def main():
    parser = argparse.ArgumentParser(description="Spotirust reproducible GUI & audio benchmark")
    parser.add_argument(
        "--binary",
        default="/home/antigravity/CgTuan210/Spotirust/target/release/spotirust",
        help="Path to release spotirust binary",
    )
    parser.add_argument("--startup-runs", type=int, default=3)
    parser.add_argument("--sample-secs", type=float, default=6.0)
    parser.add_argument("--display", default=":95")
    parser.add_argument("--output-json", default=None, help="Optional path to save JSON report")
    args = parser.parse_args()

    for tool in ("Xvfb", "xdotool", "pulseaudio", "pactl"):
        if not shutil.which(tool):
            print(f"ERROR: required benchmark tool '{tool}' not found on PATH", file=sys.stderr)
            sys.exit(2)

    bin_path = Path(args.binary).resolve()
    if not bin_path.is_file():
        print(f"ERROR: binary not found at {bin_path}", file=sys.stderr)
        sys.exit(2)

    report = {
        "environment": collect_env_info(bin_path),
        "cold_startup": run_cold_startup_bench(bin_path, args.startup_runs, args.display),
        "live_gui_session": run_live_session_bench(bin_path, args.sample_secs, args.display),
        "headless_diagnostics": run_headless_diagnostics(bin_path),
    }

    if args.output_json:
        out = Path(args.output_json)
        out.parent.mkdir(parents=True, exist_ok=True)
        out.write_text(json.dumps(report, indent=2))

    # Print clean summary
    env = report["environment"]
    cs = report["cold_startup"]
    lg = report["live_gui_session"]
    print(f"Spotirust Benchmark ({env['timestamp_utc']})")
    print(f"  Host:       {env['os']} | {env['cpu_model']} ({env['vcpus']} vCPU, {env['ram_total_mb']} MB RAM)")
    print(f"  Display:    {env['display_server']}")
    print(f"  Graphics:   {env['gpu_renderer']}")
    print(f"  Audio:      {env['audio_backend']}")
    print(f"  Binary:     {env['binary_size_mb']} MB ({env['rustc']})")
    print(
        f"  Startup:    UI init avg={cs['ui_init_avg_ms']} ms {cs['ui_init_ms']} | "
        f"X11 mapped avg={cs['x11_mapped_avg_ms']} ms {cs['x11_mapped_ms']}"
    )
    print(
        f"  Audio OK:   PulseAudio sink-input={lg['pulse_sink_input_verified']}, "
        f"auto-advance={lg['auto_advance_verified']}"
    )
    print()
    print(f"{'Scenario':<28} {'VmRSS':>8} {'VmHWM':>8} {'Total%':>8} {'RustUI%':>8} {'Audio%':>8} {'Tokio%':>8} {'MesaGL%':>8}")
    print("-" * 92)
    for label, key in [
        ("Idle Home", "idle_home"),
        ("Playing Local WAV (Bar)", "playing_local_wav_bar"),
        ("Playing + Fullscreen Karaoke", "playing_fullscreen_karaoke"),
        ("Post-Stop Idle", "post_stop_idle"),
    ]:
        s = lg[key]
        print(
            f"{label:<28} {s['vm_rss_mb']:>6.2f}MB {s['vm_hwm_mb']:>6.2f}MB "
            f"{s['total_cpu_pct']:>7.2f}% {s['rust_ui_cpu_pct']:>7.2f}% "
            f"{s['audio_cpu_pct']:>7.2f}% {s['tokio_other_cpu_pct']:>7.2f}% {s['mesa_swgl_cpu_pct']:>7.2f}%"
        )
    print(f"\n  30s RAM delta: {lg['ram_delta_30s_mb']:+.2f} MB")


if __name__ == "__main__":
    main()
