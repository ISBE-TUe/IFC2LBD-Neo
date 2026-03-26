#!/usr/bin/env python3
"""
Benchmark: ifc2lbd-neo (Rust) vs IFCtoLBD CLI (Java)
------------------------------------------------------
Measures wall time, peak RAM, CPU%, and thread count for both backends
converting Infra-Bridge.ifc, then produces scientific matplotlib plots.

Usage:
    python3 benchmark.py
"""

import os, subprocess, tempfile, time, threading, pathlib, sys
import psutil
import matplotlib
matplotlib.use("Agg")
import matplotlib.pyplot as plt
import matplotlib.gridspec as gridspec
import matplotlib.patches as mpatches
import numpy as np

ROOT      = pathlib.Path(__file__).parent
IFC       = ROOT / "IFC_SKW_Modell_07052019.ifc"
JAR       = ROOT.parent / "singleIngestConverter/services/module-ifc2lbd/IFCtoLBD_CLI_2_44_4.jar"
RUST_BIN  = ROOT / "target/release/ifc2lbd-neo"
BASE_URI  = "https://benchmark.test/bridge/"
POLL_HZ   = 10          # samples per second
OUT_DIR   = ROOT / "benchmark_results"
OUT_DIR.mkdir(exist_ok=True)

RUST_COLOR = "#2196F3"  # blue
JAVA_COLOR = "#FF6F00"  # amber

# ── monitoring ────────────────────────────────────────────────────────────────

def monitor(pid: int, samples: list, stop_event: threading.Event):
    """Poll RSS, CPU%, and thread count of process tree every 1/POLL_HZ s."""
    interval = 1.0 / POLL_HZ
    t0 = time.monotonic()
    try:
        root = psutil.Process(pid)
    except psutil.NoSuchProcess:
        return

    # Prime CPU counters (first call returns 0)
    try:
        procs = [root] + root.children(recursive=True)
        for p in procs:
            try: p.cpu_percent(interval=None)
            except (psutil.NoSuchProcess, psutil.AccessDenied): pass
    except (psutil.NoSuchProcess, psutil.AccessDenied):
        pass

    time.sleep(interval)

    while not stop_event.is_set():
        elapsed = time.monotonic() - t0
        rss_bytes = 0
        cpu_pct   = 0.0
        threads   = 0
        try:
            procs = [root] + root.children(recursive=True)
            for p in procs:
                try:
                    mi = p.memory_info()
                    rss_bytes += mi.rss
                    cpu_pct   += p.cpu_percent(interval=None)
                    threads   += p.num_threads()
                except (psutil.NoSuchProcess, psutil.AccessDenied):
                    pass
        except (psutil.NoSuchProcess, psutil.AccessDenied):
            pass
        samples.append((elapsed, rss_bytes / 1024**2, cpu_pct, threads))
        time.sleep(interval)


# ── run one backend ───────────────────────────────────────────────────────────

def run_backend(label: str, cmd: list[str], tmpdir: pathlib.Path) -> dict:
    print(f"\n{'='*60}")
    print(f"  Running: {label}")
    print(f"  CMD: {' '.join(str(c) for c in cmd)}")
    print(f"{'='*60}")

    samples: list = []
    stop   = threading.Event()

    t_start = time.monotonic()
    proc    = subprocess.Popen(
        [str(c) for c in cmd],
        cwd=str(tmpdir),
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
    )

    mon_thread = threading.Thread(target=monitor, args=(proc.pid, samples, stop), daemon=True)
    mon_thread.start()

    stdout, stderr = proc.communicate()
    wall_time = time.monotonic() - t_start

    stop.set()
    mon_thread.join(timeout=2)

    if proc.returncode != 0:
        print(f"  [ERROR] exit={proc.returncode}")
        print(stderr.decode(errors="replace")[-1000:])

    # collect output sizes
    outputs = {p.name: p.stat().st_size for p in tmpdir.glob("*") if p.is_file()}

    print(f"  Done in {wall_time:.1f}s  |  outputs: {outputs}")

    return {
        "label":      label,
        "wall_time":  wall_time,
        "returncode": proc.returncode,
        "samples":    samples,
        "outputs":    outputs,
        "stdout":     stdout.decode(errors="replace"),
        "stderr":     stderr.decode(errors="replace"),
        "tmpdir":     tmpdir,
    }


def peak(samples, idx):
    return max((s[idx] for s in samples), default=0)

def series(samples, idx):
    if not samples:
        return np.array([]), np.array([])
    t = np.array([s[0] for s in samples])
    v = np.array([s[idx] for s in samples])
    return t, v


# ── count triples ─────────────────────────────────────────────────────────────

def count_triples(path: pathlib.Path) -> int:
    """Parse the Turtle file with rdflib for an accurate triple count."""
    if not path.exists():
        return 0
    try:
        from rdflib import ConjunctiveGraph
        g = ConjunctiveGraph()
        g.parse(str(path), format="turtle")
        return len(g)
    except Exception as e:
        print(f"  [warn] rdflib parse failed for {path.name}: {e}")
        return -1


# ── diff via compare_turtle ───────────────────────────────────────────────────

def run_compare(rust_lbd: pathlib.Path, java_lbd: pathlib.Path,
                rust_owl: pathlib.Path, java_owl: pathlib.Path) -> dict:
    compare_bin = ROOT / "target/release/compare-turtle"
    results = {}
    if not compare_bin.exists():
        print("  compare_turtle binary not found — skipping diff")
        return results

    if rust_lbd.exists() and java_lbd.exists():
        r = subprocess.run(
            [str(compare_bin), str(java_lbd), str(rust_lbd), "--normalize-lbd-opm"],
            capture_output=True, text=True, timeout=120,
        )
        results["lbd"] = r.stdout + r.stderr
        print(f"  compare_turtle lbd: {r.stdout[:300]}")

    for tag, path in [("ifcowl_rust", rust_owl), ("ifcowl_java", java_owl)]:
        if path.exists():
            print(f"  counting triples in {path.name} via rdflib (may take a moment)...")
            n = count_triples(path)
            results[tag] = f"triples={n}"
            print(f"  {tag}: {results[tag]}")
    return results


# ── plot ──────────────────────────────────────────────────────────────────────

def make_plots(rust: dict, java: dict, compare: dict):
    fig = plt.figure(figsize=(16, 14))
    fig.suptitle(
        "ifc2lbd-neo (Rust) vs IFCtoLBD CLI (Java)\n"
        f"Input: Infra-Bridge.ifc  ({IFC.stat().st_size/1024**2:.1f} MB)  |  "
        f"Host: macOS, arm64 (Apple M1)",
        fontsize=13, fontweight="bold", y=0.98,
    )

    gs = gridspec.GridSpec(3, 2, figure=fig, hspace=0.45, wspace=0.35)

    ax_ram   = fig.add_subplot(gs[0, :])   # full-width RAM
    ax_cpu   = fig.add_subplot(gs[1, 0])
    ax_thr   = fig.add_subplot(gs[1, 1])
    ax_bar   = fig.add_subplot(gs[2, 0])
    ax_sizes = fig.add_subplot(gs[2, 1])

    # ── RAM over time ──────────────────────────────────────────────────────────
    rt, rram = series(rust["samples"], 1)
    jt, jram = series(java["samples"], 1)

    ax_ram.plot(rt, rram, color=RUST_COLOR, lw=1.8, label=f"Rust  (peak {peak(rust['samples'],1):.0f} MB)")
    ax_ram.fill_between(rt, rram, alpha=0.18, color=RUST_COLOR)
    ax_ram.plot(jt, jram, color=JAVA_COLOR, lw=1.8, label=f"Java  (peak {peak(java['samples'],1):.0f} MB)")
    ax_ram.fill_between(jt, jram, alpha=0.18, color=JAVA_COLOR)
    # annotate finish lines with wall time
    ax_ram.axvline(rust["wall_time"], color=RUST_COLOR, lw=1.2, ls="--", alpha=0.7)
    ax_ram.axvline(java["wall_time"], color=JAVA_COLOR, lw=1.2, ls="--", alpha=0.7)
    ax_ram.text(rust["wall_time"] + 0.05, ax_ram.get_ylim()[1] * 0.95 if ax_ram.get_ylim()[1] else 100,
                f"Rust done\n{rust['wall_time']:.2f}s", color=RUST_COLOR, fontsize=8, va="top")
    ax_ram.text(java["wall_time"] - 0.05, ax_ram.get_ylim()[1] * 0.95 if ax_ram.get_ylim()[1] else 100,
                f"Java done\n{java['wall_time']:.2f}s", color=JAVA_COLOR, fontsize=8, va="top", ha="right")
    ax_ram.set_xlabel("Wall time (s)")
    ax_ram.set_ylabel("RSS (MB)")
    ax_ram.set_title("Resident Set Size (process tree) over time")
    ax_ram.legend(fontsize=9)
    ax_ram.grid(True, alpha=0.3)

    # ── CPU% over time ─────────────────────────────────────────────────────────
    rt, rcpu = series(rust["samples"], 2)
    jt, jcpu = series(java["samples"], 2)

    ax_cpu.plot(rt, rcpu, color=RUST_COLOR, lw=1.5, label=f"Rust  avg {np.mean(rcpu):.0f}%")
    ax_cpu.plot(jt, jcpu, color=JAVA_COLOR, lw=1.5, label=f"Java  avg {np.mean(jcpu):.0f}%")
    ax_cpu.set_xlabel("Wall time (s)")
    ax_cpu.set_ylabel("CPU % (all cores, process tree)")
    ax_cpu.set_title("CPU utilisation")
    ax_cpu.legend(fontsize=9)
    ax_cpu.grid(True, alpha=0.3)

    # ── Effective cores in use (CPU% / 100) ───────────────────────────────────
    rt, rcpu2 = series(rust["samples"], 2)
    jt, jcpu2 = series(java["samples"], 2)
    r_cores = rcpu2 / 100.0
    j_cores = jcpu2 / 100.0

    ax_thr.plot(rt, r_cores, color=RUST_COLOR, lw=1.5,
                label=f"Rust  avg {np.mean(r_cores):.1f} cores")
    ax_thr.plot(jt, j_cores, color=JAVA_COLOR, lw=1.5,
                label=f"Java  avg {np.mean(j_cores):.1f} cores")
    ax_thr.axhline(y=8, color="#999", lw=0.8, ls="--", alpha=0.6, label="M1 P-cores (8)")
    ax_thr.set_xlabel("Wall time (s)")
    ax_thr.set_ylabel("Effective cores in use (CPU% ÷ 100)")
    ax_thr.set_title("Effective CPU core utilisation\n(physical work only, not OS thread count)")
    ax_thr.legend(fontsize=9)
    ax_thr.grid(True, alpha=0.3)

    # ── Wall time & peak RAM bar chart ─────────────────────────────────────────
    metrics   = ["Wall time (s)", "Peak RAM (MB)"]
    rust_vals = [rust["wall_time"], peak(rust["samples"], 1)]
    java_vals = [java["wall_time"], peak(java["samples"], 1)]

    x = np.arange(len(metrics))
    w = 0.35
    b1 = ax_bar.bar(x - w/2, rust_vals, w, label="Rust", color=RUST_COLOR, alpha=0.85)
    b2 = ax_bar.bar(x + w/2, java_vals, w, label="Java", color=JAVA_COLOR, alpha=0.85)

    for bar in list(b1) + list(b2):
        h = bar.get_height()
        ax_bar.text(bar.get_x() + bar.get_width()/2, h + 0.5,
                    f"{h:.1f}", ha="center", va="bottom", fontsize=8)

    ax_bar.set_xticks(x)
    ax_bar.set_xticklabels(metrics)
    ax_bar.set_title("Wall time & peak RAM comparison")
    ax_bar.legend(fontsize=9)
    ax_bar.grid(True, alpha=0.3, axis="y")

    # speedup annotation
    speedup = java["wall_time"] / rust["wall_time"] if rust["wall_time"] > 0 else 0
    ram_ratio = peak(java["samples"], 1) / peak(rust["samples"], 1) if peak(rust["samples"], 1) > 0 else 0
    ax_bar.text(0.5, 0.97,
        f"Rust is {speedup:.1f}× faster  |  Java uses {ram_ratio:.1f}× more RAM",
        transform=ax_bar.transAxes, ha="center", va="top",
        fontsize=9, color="#333333",
        bbox=dict(boxstyle="round,pad=0.3", fc="white", ec="#cccccc"),
    )

    # ── Output file sizes ──────────────────────────────────────────────────────
    rust_lbd_size = rust["outputs"].get("out.ttl", 0) / 1024**2
    java_lbd_size = sum(v for k,v in java["outputs"].items()
                        if "owl" not in k.lower() and k.endswith(".ttl")) / 1024**2
    rust_owl_size = rust["outputs"].get("out_ifcowl.ttl", 0) / 1024**2
    java_owl_size = sum(v for k,v in java["outputs"].items()
                        if "owl" in k.lower() and k.endswith(".ttl")) / 1024**2

    labels = ["LBD TTL", "IfcOWL TTL"]
    r_sizes = [rust_lbd_size, rust_owl_size]
    j_sizes = [java_lbd_size, java_owl_size]

    x2 = np.arange(len(labels))
    b3 = ax_sizes.bar(x2 - w/2, r_sizes, w, label="Rust", color=RUST_COLOR, alpha=0.85)
    b4 = ax_sizes.bar(x2 + w/2, j_sizes, w, label="Java", color=JAVA_COLOR, alpha=0.85)

    for bar in list(b3) + list(b4):
        h = bar.get_height()
        if h > 0:
            ax_sizes.text(bar.get_x() + bar.get_width()/2, h + 0.002,
                          f"{h:.2f} MB", ha="center", va="bottom", fontsize=8)

    ax_sizes.set_xticks(x2)
    ax_sizes.set_xticklabels(labels)
    ax_sizes.set_ylabel("File size (MB)")
    ax_sizes.set_title("Output file sizes")
    ax_sizes.legend(fontsize=9)
    ax_sizes.grid(True, alpha=0.3, axis="y")

    # ── diff summary as text in a box ──────────────────────────────────────────
    if compare:
        diff_lines = []
        for tag, text in compare.items():
            for line in text.splitlines():
                if any(k in line for k in ("result=", "rust_only=", "java_only=", "identical=", "total=", "triples=")):
                    diff_lines.append(f"[{tag.upper()}] {line.strip()}")
        if diff_lines:
            fig.text(0.5, 0.01, "\n".join(diff_lines),
                     ha="center", va="bottom", fontsize=8,
                     family="monospace",
                     bbox=dict(boxstyle="round,pad=0.4", fc="#f5f5f5", ec="#aaaaaa"))

    out_path = OUT_DIR / "benchmark.png"
    plt.savefig(out_path, dpi=150, bbox_inches="tight")
    print(f"\n  Plot saved → {out_path}")

    # also save a high-res PDF
    pdf_path = OUT_DIR / "benchmark.pdf"
    plt.savefig(pdf_path, bbox_inches="tight")
    print(f"  PDF  saved → {pdf_path}")

    plt.close(fig)


# ── main ──────────────────────────────────────────────────────────────────────

def main():
    if not IFC.exists():
        sys.exit(f"IFC file not found: {IFC}")
    if not JAR.exists():
        sys.exit(f"JAR not found: {JAR}")
    if not RUST_BIN.exists():
        sys.exit(f"Rust binary not found: {RUST_BIN}")

    print(f"IFC:      {IFC}  ({IFC.stat().st_size/1024:.0f} KB)")
    print(f"JAR:      {JAR}")
    print(f"Rust bin: {RUST_BIN}")

    with tempfile.TemporaryDirectory() as r_tmp, tempfile.TemporaryDirectory() as j_tmp:
        r_dir = pathlib.Path(r_tmp)
        j_dir = pathlib.Path(j_tmp)

        rust_cmd = [
            RUST_BIN, str(IFC),
            "-u", BASE_URI,
            "-t", str(r_dir / "out.ttl"),
            "--ifcowl-file", str(r_dir / "out_ifcowl.ttl"),
            "--topology", "false",   # disabled for fair comparison — Java has no topology
        ]
        java_cmd = [
            "java", "-Xms256m", "-Xmx4G", "-jar", str(JAR),
            str(IFC),
            "--url", BASE_URI,
            "--level", "3",
            "--target_file", str(j_dir / "out.ttl"),
            "--hasBuildingElements",
            "--hasBuildingElementProperties",
            "--hasUnits",
            "--hasGeolocation",
            "--hasIfc_based_elements",
            "--ifcOWL",
        ]

        rust_result = run_backend("Rust (ifc2lbd-neo)", rust_cmd, r_dir)
        java_result = run_backend("Java (IFCtoLBD CLI)", java_cmd, j_dir)

        # diff
        compare = run_compare(
            r_dir / "out.ttl",
            next((j_dir / k for k in java_result["outputs"]
                  if "owl" not in k.lower() and k.endswith(".ttl")), j_dir / "out.ttl"),
            r_dir / "out_ifcowl.ttl",
            next((j_dir / k for k in java_result["outputs"]
                  if "owl" in k.lower() and k.endswith(".ttl")), j_dir / "out_ifcowl.ttl"),
        )

        make_plots(rust_result, java_result, compare)

        # ── print summary table ────────────────────────────────────────────────
        print("\n" + "="*60)
        print("  SUMMARY")
        print("="*60)
        fmt = "{:<22} {:>12} {:>12}"
        print(fmt.format("Metric", "Rust", "Java"))
        print("-"*48)
        print(fmt.format("Wall time (s)",
              f"{rust_result['wall_time']:.2f}",
              f"{java_result['wall_time']:.2f}"))
        print(fmt.format("Peak RSS (MB)",
              f"{peak(rust_result['samples'],1):.0f}",
              f"{peak(java_result['samples'],1):.0f}"))
        print(fmt.format("Peak CPU %",
              f"{peak(rust_result['samples'],2):.0f}",
              f"{peak(java_result['samples'],2):.0f}"))
        print(fmt.format("Peak threads",
              f"{peak(rust_result['samples'],3):.0f}",
              f"{peak(java_result['samples'],3):.0f}"))
        for name, size in sorted(rust_result["outputs"].items()):
            print(fmt.format(f"  rust:{name}", f"{size/1024:.0f} KB", ""))
        for name, size in sorted(java_result["outputs"].items()):
            print(fmt.format(f"  java:{name}", "", f"{size/1024:.0f} KB"))
        speedup = java_result["wall_time"] / rust_result["wall_time"] if rust_result["wall_time"] > 0 else 0
        ram_ratio = peak(java_result["samples"],1) / peak(rust_result["samples"],1) if peak(rust_result["samples"],1) > 0 else 0
        print("-"*48)
        print(f"  Rust is {speedup:.1f}× faster, Java uses {ram_ratio:.1f}× more RAM")
        print("="*60)


if __name__ == "__main__":
    main()
