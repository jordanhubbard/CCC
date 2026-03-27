"""
Ray-traced NVIDIA logo in the sky.
Pure Python + NumPy ray tracer with:
  - Gradient sky background
  - Clouds (sphere clusters)
  - NVIDIA green glowing logo letters (box/sphere primitives)
  - Sun light source with shadows
  - Specular highlights + ambient
"""

import numpy as np
from PIL import Image
import time

W, H = 800, 450
SAMPLES = 1  # set higher for AA (slow)

# ─── Vector helpers ───────────────────────────────────────────────────────────

def normalize(v):
    n = np.linalg.norm(v)
    return v / n if n > 0 else v

def reflect(d, n):
    return d - 2 * np.dot(d, n) * n

# ─── Scene primitives ─────────────────────────────────────────────────────────

class Sphere:
    def __init__(self, center, radius, color, specular=0.0, emission=0.0):
        self.center = np.array(center, dtype=float)
        self.radius = radius
        self.color = np.array(color, dtype=float)
        self.specular = specular
        self.emission = emission

    def intersect(self, ro, rd):
        oc = ro - self.center
        b = np.dot(oc, rd)
        c = np.dot(oc, oc) - self.radius ** 2
        disc = b * b - c
        if disc < 0:
            return None
        sq = np.sqrt(disc)
        t1 = -b - sq
        t2 = -b + sq
        t = t1 if t1 > 1e-4 else (t2 if t2 > 1e-4 else None)
        return t

    def normal(self, p):
        return normalize(p - self.center)


class Box:
    """Axis-aligned box."""
    def __init__(self, mn, mx, color, specular=0.0, emission=0.0):
        self.mn = np.array(mn, dtype=float)
        self.mx = np.array(mx, dtype=float)
        self.color = np.array(color, dtype=float)
        self.specular = specular
        self.emission = emission

    def intersect(self, ro, rd):
        t1 = (self.mn - ro) / (rd + 1e-12)
        t2 = (self.mx - ro) / (rd + 1e-12)
        tmin = np.maximum(np.minimum(t1, t2), 0)
        tmax = np.minimum(np.maximum(t1, t2), 1e30)
        t_enter = np.max(tmin)
        t_exit  = np.min(tmax)
        if t_exit < t_enter or t_exit < 1e-4:
            return None
        t = t_enter if t_enter > 1e-4 else t_exit
        return t if t > 1e-4 else None

    def normal(self, p):
        center = (self.mn + self.mx) * 0.5
        half   = (self.mx - self.mn) * 0.5
        d = p - center
        bias = 1.0 + 1e-4
        n = (d / (half * bias)).astype(float)
        abs_n = np.abs(n)
        idx = np.argmax(abs_n)
        out = np.zeros(3)
        out[idx] = np.sign(n[idx])
        return out


# ─── Build scene ──────────────────────────────────────────────────────────────

NVIDIA_GREEN = [0.46, 0.93, 0.22]
CLOUD_WHITE  = [0.95, 0.97, 1.0]
SUN_COLOR    = np.array([1.0, 0.95, 0.80])

objects = []

# Ground plane as a giant flat box
objects.append(Box([-200, -12, -200], [200, -10, 200],
                   color=[0.25, 0.55, 0.20], specular=0.05))

# ── NVIDIA logo letters (rough block-letter forms) ──
# Each letter is 1 unit wide, 2 units tall, 0.4 deep
# Baseline at y=0, centered around x=0, floating at z=-18

def letter_boxes(shapes):
    """shapes = list of (col, row_start, row_end, x_off) relative boxes"""
    return shapes

letter_data = []

scale = 1.0
depth = 0.5
y_base = -1.0
z_pos = -18.0
gap = 0.15

def add_letter(x_off, segments):
    """segments: list of (x0,y0,x1,y1) in letter-local coords [0..1 x 0..2]"""
    for (x0, y0, x1, y1) in segments:
        mn = [x_off + x0 * scale, y_base + y0 * scale, z_pos]
        mx = [x_off + x1 * scale, y_base + y1 * scale, z_pos + depth]
        objects.append(Box(mn, mx, NVIDIA_GREEN, specular=0.6, emission=0.15))

# Letter widths (approximate)
lw = 1.2   # letter width
sp = 0.3   # spacing between letters

# N
x = -4.5
add_letter(x, [
    (0.0, 0.0, 0.2, 2.0),   # left vert
    (0.8, 0.0, 1.0, 2.0),   # right vert
    (0.0, 1.5, 1.0, 2.0),   # diagonal (approximated as top bar)
    (0.0, 0.0, 0.3, 0.7),   # diagonal bottom
    (0.7, 1.3, 1.0, 2.0),   # diagonal top
])

# V
x = -4.5 + (lw + sp)
add_letter(x, [
    (0.0, 0.8, 0.2, 2.0),   # left upper
    (0.8, 0.8, 1.0, 2.0),   # right upper
    (0.0, 0.4, 0.5, 0.9),   # left lower
    (0.5, 0.4, 1.0, 0.9),   # right lower
    (0.4, 0.0, 0.6, 0.5),   # bottom point
])

# I
x = -4.5 + 2 * (lw + sp)
add_letter(x, [
    (0.35, 0.0, 0.65, 2.0),  # vertical bar
    (0.0, 1.75, 1.0, 2.0),   # top cap
    (0.0, 0.0, 1.0, 0.25),   # bottom cap
])

# D
x = -4.5 + 3 * (lw + sp)
add_letter(x, [
    (0.0, 0.0, 0.2, 2.0),    # left vert
    (0.0, 1.75, 0.85, 2.0),  # top bar
    (0.0, 0.0, 0.85, 0.25),  # bottom bar
    (0.8, 0.25, 1.05, 1.75), # right curve (approx)
])

# I
x = -4.5 + 4 * (lw + sp)
add_letter(x, [
    (0.35, 0.0, 0.65, 2.0),
    (0.0, 1.75, 1.0, 2.0),
    (0.0, 0.0, 1.0, 0.25),
])

# A
x = -4.5 + 5 * (lw + sp)
add_letter(x, [
    (0.0, 0.0, 0.2, 1.8),    # left vert
    (0.8, 0.0, 1.0, 1.8),    # right vert
    (0.0, 1.75, 1.0, 2.0),   # top bar
    (0.0, 0.85, 1.0, 1.05),  # middle bar
])

# Clouds (sphere clusters)
cloud_positions = [
    ([-6, 4, -25], [1.5, 1.2, 1.6, 1.0, 1.3]),
    ([5, 5, -30], [1.8, 1.4, 1.2, 1.5]),
    ([-10, 3.5, -22], [1.0, 1.2, 0.9]),
    ([8, 3.0, -20], [1.3, 1.1, 1.4]),
    ([0, 6, -35], [2.0, 1.7, 1.5, 1.8]),
]
for base, radii in cloud_positions:
    cx, cy, cz = base
    offsets = [0, 1.5, -1.5, 2.5, -2.5]
    for i, r in enumerate(radii):
        objects.append(Sphere(
            [cx + offsets[i % len(offsets)], cy + (i % 2) * 0.5, cz - i * 0.3],
            r, CLOUD_WHITE, specular=0.1))

# Sun (emissive sphere, off-screen high right)
SUN_POS = np.array([30.0, 25.0, -40.0])

# ─── Sky background ───────────────────────────────────────────────────────────

def sky_color(rd):
    t = 0.5 * (rd[1] + 1.0)
    horizon = np.array([0.75, 0.88, 1.0])
    zenith  = np.array([0.18, 0.45, 0.85])
    col = (1 - t) * horizon + t * zenith

    # Sun glow
    sun_dir = normalize(SUN_POS)
    s = max(0, np.dot(rd, sun_dir))
    col += SUN_COLOR * (s ** 64) * 1.5
    col += SUN_COLOR * (s ** 8) * 0.3
    return np.clip(col, 0, 1)

# ─── Ray tracer ───────────────────────────────────────────────────────────────

def trace(ro, rd, depth=0):
    best_t = 1e30
    best_obj = None

    for obj in objects:
        t = obj.intersect(ro, rd)
        if t is not None and t < best_t:
            best_t = t
            best_obj = obj

    if best_obj is None:
        return sky_color(rd)

    hit = ro + best_t * rd
    normal = best_obj.normal(hit)
    if np.dot(normal, -rd) < 0:
        normal = -normal

    # Emission
    if best_obj.emission > 0:
        emitted = best_obj.color * best_obj.emission
    else:
        emitted = np.zeros(3)

    # Shadow
    light_dir = normalize(SUN_POS - hit)
    shadow = False
    for obj in objects:
        if obj is best_obj:
            continue
        st = obj.intersect(hit + normal * 1e-3, light_dir)
        if st is not None and st < np.linalg.norm(SUN_POS - hit):
            shadow = True
            break

    # Diffuse
    diff = max(0, np.dot(normal, light_dir))
    ambient = 0.15
    light_intensity = 0.0 if shadow else 1.0
    diffuse = best_obj.color * (ambient + diff * light_intensity)

    # Specular
    if best_obj.specular > 0 and not shadow:
        ref = reflect(-light_dir, normal)
        spec = max(0, np.dot(-rd, ref)) ** 32
        specular = np.array(SUN_COLOR) * spec * best_obj.specular
    else:
        specular = np.zeros(3)

    # Reflection (one bounce)
    reflect_col = np.zeros(3)
    if best_obj.specular > 0.3 and depth < 1:
        ref_dir = normalize(reflect(rd, normal))
        reflect_col = trace(hit + normal * 1e-3, ref_dir, depth + 1) * best_obj.specular * 0.4

    color = diffuse + specular + reflect_col + emitted
    return np.clip(color, 0, 1)

# ─── Camera ───────────────────────────────────────────────────────────────────

cam_pos   = np.array([0.0, 1.5, 5.0])
cam_look  = np.array([0.0, 0.5, -18.0])
cam_up    = np.array([0.0, 1.0, 0.0])

fwd   = normalize(cam_look - cam_pos)
right = normalize(np.cross(fwd, cam_up))
up    = np.cross(right, fwd)

fov = np.pi / 3  # 60°
half_w = np.tan(fov / 2)
half_h = half_w * H / W

# ─── Render ───────────────────────────────────────────────────────────────────

print(f"Rendering {W}x{H}...")
start = time.time()
pixels = np.zeros((H, W, 3))

for j in range(H):
    if j % 50 == 0:
        elapsed = time.time() - start
        print(f"  Row {j}/{H}  ({elapsed:.1f}s)")
    for i in range(W):
        u = (2 * (i + 0.5) / W - 1) * half_w
        v = (1 - 2 * (j + 0.5) / H) * half_h
        rd = normalize(fwd + u * right + v * up)
        pixels[j, i] = trace(cam_pos, rd)

# Gamma correction
pixels = np.clip(pixels, 0, 1) ** (1 / 2.2)
img_data = (pixels * 255).astype(np.uint8)
img = Image.fromarray(img_data, 'RGB')

out_path = "/home/jkh/.openclaw/workspace/raytrace/nvidia_sky.png"
img.save(out_path)
elapsed = time.time() - start
print(f"Done! Saved to {out_path}  ({elapsed:.1f}s)")
