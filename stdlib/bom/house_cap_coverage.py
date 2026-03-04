#!/usr/bin/env python3
"""Visualize house capacitor catalog"""

import matplotlib.pyplot as plt
import numpy as np

# Data extracted from HOUSE_CAPS_BY_PKG (package, dielectric)
caps_data = {
    ("0402", "C0G"): [
        {"cap": "12pF", "voltage": 50},
        {"cap": "22pF", "voltage": 50},
        {"cap": "30pF", "voltage": 50},
        {"cap": "47pF", "voltage": 50},
        {"cap": "100pF", "voltage": 50},
        {"cap": "330pF", "voltage": 50},
        {"cap": "10nF", "voltage": 35},
    ],
    ("0402", "X7R"): [
        {"cap": "10nF", "voltage": 50},
        {"cap": "22nF", "voltage": 50},
        {"cap": "100nF", "voltage": 50},
        {"cap": "1uF", "voltage": 10},
    ],
    ("0603", "X7R"): [
        {"cap": "1uF", "voltage": 25},
        {"cap": "2.2uF", "voltage": 25},
        {"cap": "4.7uF", "voltage": 16},
        {"cap": "10uF", "voltage": 10},
    ],
    ("0805", "X7R"): [
        {"cap": "4.7uF", "voltage": 50},
        {"cap": "10uF", "voltage": 25},
    ],
    ("1206", "X7R"): [
        {"cap": "4.7uF", "voltage": 50},
        {"cap": "10uF", "voltage": 25},
    ],
    ("1210", "X7R"): [
        {"cap": "4.7uF", "voltage": 50},
        {"cap": "10uF", "voltage": 25},
    ],
}


def parse_cap(cap_str):
    """Convert capacitance string to farads"""
    cap_str = cap_str.strip()
    multipliers = {"pF": 1e-12, "nF": 1e-9, "uF": 1e-6, "F": 1.0}
    
    for unit, mult in multipliers.items():
        if unit in cap_str:
            val = float(cap_str.replace(unit, "").strip())
            return val * mult
    return float(cap_str)


# Prepare data for plotting
plt.figure(figsize=(12, 8))

# Unique color for each package+dielectric combination
line_colors = {
    ("0402", "C0G"): "#0066cc",  # Dark blue
    ("0402", "X7R"): "#66b3ff",  # Light blue
    ("0603", "X7R"): "#ff7f0e",  # Orange
    ("0805", "X7R"): "#2ca02c",  # Green
    ("1206", "X7R"): "#d62728",  # Red
    ("1210", "X7R"): "#9467bd",  # Purple
}
pkg_markers = {"0402": "o", "0603": "s", "0805": "^", "1206": "D", "1210": "v"}

for (pkg, diel), parts in caps_data.items():
    # Sort by capacitance first
    sorted_parts = sorted(parts, key=lambda p: parse_cap(p["cap"]))
    caps = [parse_cap(p["cap"]) for p in sorted_parts]
    voltages = [p["voltage"] for p in sorted_parts]
    
    label = f"{pkg} {diel}"
    color = line_colors.get((pkg, diel), "#000000")
    marker = pkg_markers.get(pkg, "o")
    
    plt.scatter(caps, voltages, label=label, s=100, alpha=0.7, 
                color=color, marker=marker)
    plt.plot(caps, voltages, alpha=0.5, color=color, linewidth=2)

plt.xscale("log")
plt.xlabel("Capacitance (F)", fontsize=12)
plt.ylabel("Voltage (V)", fontsize=12)
plt.title("House Capacitor Coverage", fontsize=14, fontweight='bold')
plt.grid(True, alpha=0.3, which="both")
plt.legend(title="Package", fontsize=10)

# Format x-axis labels
ax = plt.gca()
ax.xaxis.set_major_formatter(plt.FuncFormatter(
    lambda x, p: f"{x*1e12:.0f}pF" if x < 1e-9 else 
                 f"{x*1e9:.0f}nF" if x < 1e-6 else 
                 f"{x*1e6:.1f}µF"
))

plt.tight_layout()
plt.show()
