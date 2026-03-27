#!/usr/bin/env python3
"""
Shanghai City RTX Render - Natasha's OVRTX showcase
Creates a procedural Shanghai cityscape USD scene and renders it with RTX,
capturing a screenshot and a short animated flythrough video.
"""

import os
import sys
import math
import zlib
import struct
import subprocess
import numpy as np
from pathlib import Path

# Set up OVRTX library path
OVRTX_BIN = "/home/jkh/.local/lib/python3.12/site-packages/ovrtx/bin"
os.environ["LD_LIBRARY_PATH"] = OVRTX_BIN + ":" + os.environ.get("LD_LIBRARY_PATH", "")

OUTPUT_DIR = Path("/home/jkh/.openclaw/workspace/shanghai_output")
OUTPUT_DIR.mkdir(parents=True, exist_ok=True)

USD_PATH = str(OUTPUT_DIR / "shanghai.usda")
SCREENSHOT_PNG = str(OUTPUT_DIR / "shanghai_screenshot.png")
VIDEO_PATH = str(OUTPUT_DIR / "shanghai_flythrough.mp4")

RENDER_PRODUCT_PATH = "/Render/OmniverseKit/HydraTextures/FlyCamera"
RENDER_VAR_NAME = "LdrColor"

WIDTH, HEIGHT = 1280, 720  # sane resolution for GB10 unified memory


# ─────────────────────────────────────────────
# 1. Build the Shanghai USD scene
# ─────────────────────────────────────────────

def write_usda():
    """Write a procedural Shanghai-inspired cityscape as a USDA file."""

    rng = np.random.default_rng(42)

    # Pudong skyline cluster (tall towers, scaled for cm units → match simple_scene)
    # metersPerUnit = 0.01 means 1 unit = 1 cm, so 632m = 63200 units
    # Let's use metersPerUnit = 1.0 and keep real-world meters
    pudong_layout = [
        # (x, z, width, depth, height, r, g, b, name)
        (0,    0,  30, 30, 632, 0.85, 0.92, 1.00, "Shanghai_Tower"),
        (80,  10,  28, 28, 492, 0.95, 0.85, 0.70, "World_Financial_Center"),
        (-70, 20,  25, 25, 421, 0.70, 0.80, 1.00, "Jin_Mao_Tower"),
        (160,  5,  22, 22, 320, 0.60, 0.70, 0.90, "One_IFC"),
        (-150,15,  20, 20, 288, 0.75, 0.85, 0.75, "Two_IFC"),
    ]

    buildings = list(pudong_layout)

    # Mid-rise Pudong office blocks
    for i in range(30):
        x = rng.uniform(-350, 350)
        z = rng.uniform(-250, 250)
        if abs(x) < 110 and abs(z) < 110:
            continue
        w = rng.uniform(12, 28)
        d = rng.uniform(12, 28)
        h = rng.uniform(40, 200)
        r = rng.uniform(0.55, 0.90)
        g = rng.uniform(0.60, 0.90)
        b_  = rng.uniform(0.65, 1.00)
        buildings.append((float(x), float(z), float(w), float(d), float(h),
                          float(r), float(g), float(b_), f"Office_{i:02d}"))

    # Huangpu River bank – Bund-style lower blocks
    for i in range(20):
        x = rng.uniform(-450, 450)
        z = rng.uniform(-500, -320)
        w = rng.uniform(10, 22)
        d = rng.uniform(10, 22)
        h = rng.uniform(20, 70)
        r = rng.uniform(0.75, 0.95)
        g = rng.uniform(0.70, 0.88)
        b_ = rng.uniform(0.55, 0.72)
        buildings.append((float(x), float(z), float(w), float(d), float(h),
                          float(r), float(g), float(b_), f"Bund_{i:02d}"))

    lines = []
    lines.append('#usda 1.0')
    lines.append('(')
    lines.append('    defaultPrim = "Shanghai"')
    lines.append('    metersPerUnit = 1')
    lines.append('    upAxis = "Y"')
    lines.append('    startTimeCode = 0')
    lines.append('    endTimeCode = 240')
    lines.append('    timeCodesPerSecond = 24')
    lines.append(')')
    lines.append('')
    lines.append('def Xform "Shanghai"')
    lines.append('{')

    # Ground plane
    lines.append('    def Mesh "Ground" (')
    lines.append('        prepend apiSchemas = ["MaterialBindingAPI"]')
    lines.append('    )')
    lines.append('    {')
    lines.append('        float3[] extent = [(-800, 0, -800), (800, 1, 800)]')
    lines.append('        int[] faceVertexCounts = [4]')
    lines.append('        int[] faceVertexIndices = [0, 1, 2, 3]')
    lines.append('        point3f[] points = [(-800, 0, -800), (800, 0, -800), (800, 0, 800), (-800, 0, 800)]')
    lines.append('        normal3f[] normals = [(0, 1, 0), (0, 1, 0), (0, 1, 0), (0, 1, 0)] (interpolation = "faceVarying")')
    lines.append('        rel material:binding = </Shanghai/Materials/Ground>')
    lines.append('    }')
    lines.append('')

    # Materials
    lines.append('    def Xform "Materials"')
    lines.append('    {')
    lines.append('        def Material "Ground"')
    lines.append('        {')
    lines.append('            token outputs:mdl:displacement.connect = </Shanghai/Materials/Ground/Shader.outputs:out>')
    lines.append('            token outputs:mdl:surface.connect = </Shanghai/Materials/Ground/Shader.outputs:out>')
    lines.append('            token outputs:mdl:volume.connect = </Shanghai/Materials/Ground/Shader.outputs:out>')
    lines.append('            def Shader "Shader"')
    lines.append('            {')
    lines.append('                uniform token info:implementationSource = "sourceAsset"')
    lines.append('                uniform asset info:mdl:sourceAsset = @OmniPBR.mdl@')
    lines.append('                uniform token info:mdl:sourceAsset:subIdentifier = "OmniPBR"')
    lines.append('                color3f inputs:diffuse_color_constant = (0.10, 0.12, 0.15)')
    lines.append('                float inputs:metallic_constant = 0.05')
    lines.append('                float inputs:reflection_roughness_constant = 0.8')
    lines.append('                token outputs:out')
    lines.append('            }')
    lines.append('        }')

    for bx, bz, bw, bd, bh, br, bg, bb, bname in buildings:
        mat_name = f"Mat_{bname}"
        lines.append(f'        def Material "{mat_name}"')
        lines.append('        {')
        lines.append(f'            token outputs:mdl:displacement.connect = </Shanghai/Materials/{mat_name}/Shader.outputs:out>')
        lines.append(f'            token outputs:mdl:surface.connect = </Shanghai/Materials/{mat_name}/Shader.outputs:out>')
        lines.append(f'            token outputs:mdl:volume.connect = </Shanghai/Materials/{mat_name}/Shader.outputs:out>')
        lines.append('            def Shader "Shader"')
        lines.append('            {')
        lines.append('                uniform token info:implementationSource = "sourceAsset"')
        lines.append('                uniform asset info:mdl:sourceAsset = @OmniPBR.mdl@')
        lines.append('                uniform token info:mdl:sourceAsset:subIdentifier = "OmniPBR"')
        lines.append(f'                color3f inputs:diffuse_color_constant = ({br:.3f}, {bg:.3f}, {bb:.3f})')
        lines.append('                float inputs:metallic_constant = 0.7')
        lines.append('                float inputs:reflection_roughness_constant = 0.15')
        lines.append('                token outputs:out')
        lines.append('            }')
        lines.append('        }')

    lines.append('    }')  # end Materials
    lines.append('')

    # Buildings scope
    lines.append('    def Scope "Buildings"')
    lines.append('    {')
    for bx, bz, bw, bd, bh, br, bg, bb, bname in buildings:
        hw, hd = bw / 2, bd / 2
        verts = [
            (bx-hw, 0,  bz-hd), (bx+hw, 0,  bz-hd), (bx+hw, 0,  bz+hd), (bx-hw, 0,  bz+hd),
            (bx-hw, bh, bz-hd), (bx+hw, bh, bz-hd), (bx+hw, bh, bz+hd), (bx-hw, bh, bz+hd),
        ]
        pts_str = ", ".join(f"({v[0]:.1f}, {v[1]:.1f}, {v[2]:.1f})" for v in verts)
        lines.append(f'        def Mesh "{bname}" (')
        lines.append('            prepend apiSchemas = ["MaterialBindingAPI"]')
        lines.append('        )')
        lines.append('        {')
        lines.append(f'            float3[] extent = [({bx-hw:.1f}, 0, {bz-hd:.1f}), ({bx+hw:.1f}, {bh:.1f}, {bz+hd:.1f})]')
        lines.append(f'            point3f[] points = [{pts_str}]')
        lines.append('            int[] faceVertexCounts = [4, 4, 4, 4, 4, 4]')
        lines.append('            int[] faceVertexIndices = [0,1,2,3, 7,6,5,4, 0,1,5,4, 1,2,6,5, 2,3,7,6, 3,0,4,7]')
        lines.append(f'            rel material:binding = </Shanghai/Materials/Mat_{bname}>')
        lines.append('        }')
    lines.append('    }')  # end Buildings
    lines.append('')

    # Lights
    lines.append('    def DistantLight "Sun" (')
    lines.append('        prepend apiSchemas = ["ShapingAPI"]')
    lines.append('    )')
    lines.append('    {')
    lines.append('        float inputs:angle = 0.53')
    lines.append('        float inputs:intensity = 4000')
    lines.append('        color3f inputs:color = (1.0, 0.97, 0.88)')
    lines.append('        float3 xformOp:rotateXYZ = (-50, 30, 0)')
    lines.append('        uniform token[] xformOpOrder = ["xformOp:rotateXYZ"]')
    lines.append('    }')
    lines.append('')
    lines.append('    def DomeLight "Sky" (')
    lines.append('        prepend apiSchemas = ["ShapingAPI"]')
    lines.append('    )')
    lines.append('    {')
    lines.append('        float inputs:intensity = 600')
    lines.append('        color3f inputs:color = (0.53, 0.73, 1.0)')
    lines.append('        float inputs:shaping:cone:angle = 180')
    lines.append('    }')
    lines.append('')

    lines.append('}')  # end Shanghai Xform
    lines.append('')

    # Camera with animated flythrough
    num_frames = 241
    translate_samples = []
    rotate_samples = []
    for f in range(0, num_frames, 10):
        t = f / 240.0
        angle = math.radians(-90 + t * 180)
        radius = 900
        cam_x = radius * math.sin(angle)
        cam_z = radius * math.cos(angle)
        cam_y = 300 + 150 * math.sin(math.pi * t)

        dx = 0 - cam_x
        dy = 250 - cam_y
        dz = 0 - cam_z
        length = math.sqrt(dx*dx + dy*dy + dz*dz)
        dx /= length; dy /= length; dz /= length

        ry = math.degrees(math.atan2(dx, dz))
        rx = math.degrees(-math.asin(min(1.0, max(-1.0, dy))))

        translate_samples.append(f"{f}: ({cam_x:.1f}, {cam_y:.1f}, {cam_z:.1f})")
        rotate_samples.append(f"{f}: ({rx:.2f}, {ry:.2f}, 0)")

    ts_str = ", ".join(translate_samples)
    rs_str = ", ".join(rotate_samples)

    lines.append('def Camera "FlyCamera"')
    lines.append('{')
    lines.append('    float2 clippingRange = (1, 5000)')
    lines.append('    float focalLength = 18.0')
    lines.append('    float horizontalAperture = 20.955')
    lines.append('    float verticalAperture = 15.2908')
    lines.append(f'    double3 xformOp:translate.timeSamples = {{{ts_str}}}')
    lines.append(f'    float3 xformOp:rotateXYZ.timeSamples = {{{rs_str}}}')
    lines.append('    uniform token[] xformOpOrder = ["xformOp:translate", "xformOp:rotateXYZ"]')
    lines.append('}')
    lines.append('')

    # Render setup — follows exact schema from simple_scene.usda
    lines.append('def "Render" (')
    lines.append('    hide_in_stage_window = true')
    lines.append('    no_delete = true')
    lines.append(')')
    lines.append('{')
    lines.append('    def "OmniverseKit"')
    lines.append('    {')
    lines.append('        def "HydraTextures" (')
    lines.append('            hide_in_stage_window = true')
    lines.append('            no_delete = true')
    lines.append('        )')
    lines.append('        {')
    lines.append('            def RenderProduct "FlyCamera" (')
    lines.append('                hide_in_stage_window = true')
    lines.append('                no_delete = true')
    lines.append('            )')
    lines.append('            {')
    lines.append('                rel camera = </FlyCamera>')
    lines.append('                token omni:rtx:background:source:type = "sky"')
    lines.append('                token[] omni:rtx:waitForEvents = ["AllLoadingFinished", "OnlyOnFirstRequest"]')
    lines.append('                rel orderedVars = </Render/Vars/LdrColor>')
    lines.append(f'                uniform int2 resolution = ({WIDTH}, {HEIGHT})')
    lines.append('            }')
    lines.append('        }')
    lines.append('    }')
    lines.append('')
    lines.append('    def RenderSettings "OmniverseGlobalRenderSettings" (')
    lines.append('        no_delete = true')
    lines.append('    )')
    lines.append('    {')
    lines.append('        rel products = [')
    lines.append(f'            <{RENDER_PRODUCT_PATH}>,')
    lines.append('        ]')
    lines.append('    }')
    lines.append('')
    lines.append('    def "Vars"')
    lines.append('    {')
    lines.append('        def RenderVar "LdrColor" (')
    lines.append('            hide_in_stage_window = true')
    lines.append('            no_delete = true')
    lines.append('        )')
    lines.append('        {')
    lines.append('            uniform string sourceName = "LdrColor"')
    lines.append('        }')
    lines.append('    }')
    lines.append('}')

    with open(USD_PATH, "w") as f:
        f.write("\n".join(lines))
    print(f"[+] USD scene written: {USD_PATH}")
    return buildings


# ─────────────────────────────────────────────
# 2. PNG save helper
# ─────────────────────────────────────────────

def save_png(arr: np.ndarray, path: str):
    """Save HxWx4 uint8 numpy array as PNG."""
    try:
        from PIL import Image
        img = Image.fromarray(arr[:, :, :3])
        img.save(path)
        return
    except Exception:
        pass
    # Minimal raw PNG encoder fallback
    h, w = arr.shape[:2]
    rgb = arr[:, :, :3].astype(np.uint8)
    def chunk(tag, data):
        c = struct.pack('>I', len(data)) + tag + data
        return c + struct.pack('>I', zlib.crc32(tag + data) & 0xffffffff)
    raw = b''.join(b'\x00' + bytes(row.tobytes()) for row in rgb)
    png = b'\x89PNG\r\n\x1a\n'
    png += chunk(b'IHDR', struct.pack('>IIBBBBB', w, h, 8, 2, 0, 0, 0))
    png += chunk(b'IDAT', zlib.compress(raw, 6))
    png += chunk(b'IEND', b'')
    Path(path).write_bytes(png)


# ─────────────────────────────────────────────
# 3. Render via OVRTX
# ─────────────────────────────────────────────

def render():
    import ovrtx

    print("[+] Initialising OVRTX renderer...")
    config = ovrtx.RendererConfig(log_file_path=str(OUTPUT_DIR / "ovrtx.log"))
    renderer = ovrtx.Renderer(config=config)
    print(f"[+] OVRTX version: {renderer.version}")

    print(f"[+] Loading USD: {USD_PATH}")
    renderer.add_usd(USD_PATH)

    RENDER_PRODUCTS = {RENDER_PRODUCT_PATH}

    def extract_frame(products):
        if not products or RENDER_PRODUCT_PATH not in products:
            return None
        prod = products[RENDER_PRODUCT_PATH]
        if not prod.frames:
            return None
        frame = prod.frames[0]
        if RENDER_VAR_NAME not in frame.render_vars:
            print(f"    [!] render_vars available: {list(frame.render_vars.keys())}")
            return None
        with frame.render_vars[RENDER_VAR_NAME].map(device=ovrtx.Device.CPU) as m:
            arr = np.from_dlpack(m.tensor)
            return arr.reshape(HEIGHT, WIDTH, -1).copy()

    # Warm up — PTX needs a few frames to converge
    print("[+] Warming up (10 frames)...")
    for i in range(10):
        renderer.update_from_usd_time(60.0)
        products = renderer.step(render_products=RENDER_PRODUCTS, delta_time=0.1)
        if i == 0 and products:
            print(f"    Products: {list(products.keys())}")

    # Screenshot at frame 60
    print("[+] Rendering screenshot (frame 60)...")
    renderer.update_from_usd_time(60.0)
    products = renderer.step(render_products=RENDER_PRODUCTS, delta_time=0.1)
    arr = extract_frame(products)
    if arr is not None:
        save_png(arr, SCREENSHOT_PNG)
        print(f"[+] Screenshot saved: {SCREENSHOT_PNG}  shape={arr.shape}")
    else:
        print("[!] Screenshot failed")

    # Video frames
    frames_dir = OUTPUT_DIR / "frames"
    frames_dir.mkdir(exist_ok=True)

    print("[+] Rendering video frames (every 4th of 240)...")
    frame_paths = []
    STEP = 4
    TOTAL = 241
    for f in range(0, TOTAL, STEP):
        renderer.update_from_usd_time(float(f))
        products = renderer.step(render_products=RENDER_PRODUCTS, delta_time=1.0 / 24.0)
        arr = extract_frame(products)
        if arr is not None:
            fpath = str(frames_dir / f"frame_{f:04d}.png")
            save_png(arr, fpath)
            frame_paths.append(fpath)
            if f % 40 == 0:
                print(f"    frame {f:3d}/{TOTAL}")
        else:
            if f % 40 == 0:
                print(f"    [!] no output at frame {f}")

    print(f"[+] Rendered {len(frame_paths)} frames")

    # Assemble video
    if len(frame_paths) >= 4:
        print("[+] Assembling video with ffmpeg...")
        result = subprocess.run([
            "ffmpeg", "-y",
            "-framerate", "24",
            "-pattern_type", "glob",
            "-i", str(frames_dir / "frame_*.png"),
            "-c:v", "libx264",
            "-pix_fmt", "yuv420p",
            "-crf", "18",
            "-preset", "fast",
            VIDEO_PATH,
        ], capture_output=True, text=True)
        if result.returncode == 0:
            print(f"[+] Video saved: {VIDEO_PATH}")
        else:
            print(f"[!] ffmpeg error:\n{result.stderr[-600:]}")

    print("\n✅ Done!")
    print(f"   USD scene:  {USD_PATH}")
    print(f"   Screenshot: {SCREENSHOT_PNG}")
    print(f"   Video:      {VIDEO_PATH}")


if __name__ == "__main__":
    write_usda()
    render()
