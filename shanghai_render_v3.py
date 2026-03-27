#!/usr/bin/env python3
"""
Shanghai Nighttime Cityscape Generator v3
Generates a dramatic, dense Shanghai skyline at night.
Coordinate system: Y-up, 1 unit = 50 meters
"""

import random
import math
import os

SEED = 42
random.seed(SEED)

OUTPUT_PATH = "/home/jkh/.openclaw/workspace/shanghai_output/shanghai_v3.usda"
os.makedirs(os.path.dirname(OUTPUT_PATH), exist_ok=True)

# ─────────────────────────────────────────────────────────────────────────────
# Math helpers
# ─────────────────────────────────────────────────────────────────────────────

def v3_sub(a, b): return (a[0]-b[0], a[1]-b[1], a[2]-b[2])
def v3_cross(a, b):
    return (a[1]*b[2]-a[2]*b[1], a[2]*b[0]-a[0]*b[2], a[0]*b[1]-a[1]*b[0])
def v3_dot(a, b): return a[0]*b[0]+a[1]*b[1]+a[2]*b[2]
def v3_norm(a):
    m = math.sqrt(v3_dot(a, a))
    if m < 1e-10: return (0, 1, 0)
    return (a[0]/m, a[1]/m, a[2]/m)

def look_at_matrix(eye, target, world_up=(0, 1, 0)):
    """Return USD-format matrix4d string for camera-to-world transform.
    USD row-major: rows are camera X(right), Y(up), Z(-forward), then translation.
    Camera looks along -Z by default."""
    fwd = v3_norm(v3_sub(target, eye))
    rgt = v3_norm(v3_cross(world_up, fwd))
    up2 = v3_cross(fwd, rgt)  # Already normalized if fwd & rgt are unit
    ex, ey, ez = eye
    rx, ry, rz = rgt
    ux, uy, uz = up2
    fx, fy, fz = fwd
    return (f'( ({rx:.6f}, {ry:.6f}, {rz:.6f}, 0), '
            f'({ux:.6f}, {uy:.6f}, {uz:.6f}, 0), '
            f'({-fx:.6f}, {-fy:.6f}, {-fz:.6f}, 0), '
            f'({ex:.6f}, {ey:.6f}, {ez:.6f}, 1) )')


# ─────────────────────────────────────────────────────────────────────────────
# Line buffer
# ─────────────────────────────────────────────────────────────────────────────

class Lines:
    def __init__(self):
        self._buf = []

    def __iadd__(self, line):
        self._buf.append(line)
        return self

    def blank(self):
        self._buf.append('')
        return self

    def text(self):
        return '\n'.join(self._buf) + '\n'


# ─────────────────────────────────────────────────────────────────────────────
# Material registry
# ─────────────────────────────────────────────────────────────────────────────

MAT_REG = {}   # name -> dict

def reg_mat(name, diffuse, emissive=(0, 0, 0), metallic=0.1, roughness=0.5):
    MAT_REG[name] = dict(d=diffuse, e=emissive, m=metallic, r=roughness)
    return f'/World/Materials/{name}'

def write_materials(L):
    L += '    def Scope "Materials"'
    L += '    {'
    for name, mat in MAT_REG.items():
        d, e, m, r = mat['d'], mat['e'], mat['m'], mat['r']
        L += f'        def Material "{name}"'
        L += '        {'
        L += (f'            token outputs:surface.connect = '
              f'</World/Materials/{name}/Shader.outputs:surface>')
        L += f'            def Shader "Shader"'
        L += '            {'
        L += '                uniform token info:id = "UsdPreviewSurface"'
        L += f'                color3f inputs:diffuseColor = ({d[0]:.4f}, {d[1]:.4f}, {d[2]:.4f})'
        L += f'                color3f inputs:emissiveColor = ({e[0]:.4f}, {e[1]:.4f}, {e[2]:.4f})'
        L += f'                float inputs:metallic = {m:.3f}'
        L += f'                float inputs:roughness = {r:.3f}'
        L += '                token outputs:surface'
        L += '            }'
        L += '        }'
    L += '    }'
    L.blank()


# ─────────────────────────────────────────────────────────────────────────────
# Primitive helpers
# ─────────────────────────────────────────────────────────────────────────────

def _pad(indent): return '    ' * indent

def cube_prim(L, name, cx, cy, cz, sx, sy, sz, mat, indent=2):
    """Cube centered at (cx,cy,cz) with half-extents (sx,sy,sz)."""
    p = _pad(indent)
    L += f'{p}def Xform "{name}"'
    L += f'{p}{{'
    L += f'{p}    double3 xformOp:translate = ({cx:.5f}, {cy:.5f}, {cz:.5f})'
    L += f'{p}    float3 xformOp:scale = ({sx:.5f}, {sy:.5f}, {sz:.5f})'
    L += f'{p}    uniform token[] xformOpOrder = ["xformOp:translate", "xformOp:scale"]'
    L += f'{p}    def Cube "Mesh"'
    L += f'{p}    {{'
    L += f'{p}        double size = 2'
    L += f'{p}        rel material:binding = <{mat}>'
    L += f'{p}    }}'
    L += f'{p}}}'
    L.blank()

def building_prim(L, name, bx, bz, w, h, d, mat, indent=2):
    """Building with base at y=0."""
    cube_prim(L, name, bx, h/2, bz, w/2, h/2, d/2, mat, indent)

def sphere_prim(L, name, cx, cy, cz, radius, mat, indent=2):
    p = _pad(indent)
    L += f'{p}def Xform "{name}"'
    L += f'{p}{{'
    L += f'{p}    double3 xformOp:translate = ({cx:.5f}, {cy:.5f}, {cz:.5f})'
    L += f'{p}    uniform token[] xformOpOrder = ["xformOp:translate"]'
    L += f'{p}    def Sphere "Mesh"'
    L += f'{p}    {{'
    L += f'{p}        double radius = {radius:.5f}'
    L += f'{p}        rel material:binding = <{mat}>'
    L += f'{p}    }}'
    L += f'{p}}}'
    L.blank()

def cylinder_prim(L, name, cx, cy, cz, radius, height, mat, indent=2):
    """Cylinder centered at (cx,cy,cz), Y axis."""
    p = _pad(indent)
    L += f'{p}def Xform "{name}"'
    L += f'{p}{{'
    L += f'{p}    double3 xformOp:translate = ({cx:.5f}, {cy:.5f}, {cz:.5f})'
    L += f'{p}    uniform token[] xformOpOrder = ["xformOp:translate"]'
    L += f'{p}    def Cylinder "Mesh"'
    L += f'{p}    {{'
    L += f'{p}        double radius = {radius:.5f}'
    L += f'{p}        double height = {height:.5f}'
    L += f'{p}        token axis = "Y"'
    L += f'{p}        rel material:binding = <{mat}>'
    L += f'{p}    }}'
    L += f'{p}}}'
    L.blank()

def sphere_light(L, name, x, y, z, radius, intensity, color, indent=2):
    p = _pad(indent)
    r, g, b = color
    L += f'{p}def SphereLight "{name}"'
    L += f'{p}{{'
    L += f'{p}    float inputs:intensity = {intensity:.1f}'
    L += f'{p}    float inputs:radius = {radius:.5f}'
    L += f'{p}    color3f inputs:color = ({r:.4f}, {g:.4f}, {b:.4f})'
    L += f'{p}    double3 xformOp:translate = ({x:.4f}, {y:.4f}, {z:.4f})'
    L += f'{p}    uniform token[] xformOpOrder = ["xformOp:translate"]'
    L += f'{p}}}'
    L.blank()


# ─────────────────────────────────────────────────────────────────────────────
# Main scene builder
# ─────────────────────────────────────────────────────────────────────────────

def build_scene():
    random.seed(SEED)
    L = Lines()

    # ── Register all materials ────────────────────────────────────────────────
    M_RIVER   = reg_mat('River',   (0.02, 0.03, 0.08), (0.03, 0.05, 0.15), metallic=0.85, roughness=0.05)
    M_GROUND  = reg_mat('Ground',  (0.05, 0.05, 0.06), (0.01, 0.01, 0.02), metallic=0.05, roughness=0.9)

    # Bund / classical stone
    M_BUND = [
        reg_mat('Bund1', (0.76, 0.66, 0.46), (0.45, 0.38, 0.18), metallic=0.05, roughness=0.65),
        reg_mat('Bund2', (0.83, 0.73, 0.53), (0.50, 0.42, 0.20), metallic=0.05, roughness=0.60),
        reg_mat('Bund3', (0.70, 0.60, 0.40), (0.42, 0.34, 0.16), metallic=0.05, roughness=0.70),
        reg_mat('Bund4', (0.88, 0.78, 0.58), (0.52, 0.44, 0.22), metallic=0.05, roughness=0.55),
        reg_mat('Bund5', (0.79, 0.69, 0.49), (0.46, 0.38, 0.19), metallic=0.05, roughness=0.62),
    ]

    # Landmarks
    M_PEARL  = reg_mat('OrientalPearl', (0.9, 0.2, 0.45), (1.2, 0.35, 0.6),  metallic=0.3, roughness=0.2)
    M_SHTWR  = reg_mat('ShanghaiTwr',   (0.5, 0.78, 0.95), (0.5, 0.7,  0.95), metallic=0.65, roughness=0.15)
    M_JINMAO = reg_mat('JinMao',        (0.80, 0.70, 0.18), (0.55, 0.45, 0.10), metallic=0.75, roughness=0.25)
    M_SWFC   = reg_mat('SWFC',          (0.72, 0.78, 0.85), (0.45, 0.55, 0.70), metallic=0.75, roughness=0.18)

    # Background glass towers (blue-grey tones)
    M_GLASS = [
        reg_mat('Glass1', (0.28, 0.38, 0.50), (0.30, 0.50, 0.72), metallic=0.55, roughness=0.18),
        reg_mat('Glass2', (0.38, 0.48, 0.60), (0.35, 0.48, 0.70), metallic=0.55, roughness=0.20),
        reg_mat('Glass3', (0.22, 0.32, 0.44), (0.22, 0.42, 0.65), metallic=0.60, roughness=0.16),
        reg_mat('Glass4', (0.44, 0.50, 0.58), (0.38, 0.52, 0.68), metallic=0.50, roughness=0.22),
        reg_mat('Glass5', (0.32, 0.40, 0.52), (0.28, 0.46, 0.68), metallic=0.58, roughness=0.19),
        reg_mat('Glass6', (0.50, 0.55, 0.62), (0.40, 0.55, 0.72), metallic=0.52, roughness=0.21),
    ]

    # Background concrete/beige towers
    M_CONC = [
        reg_mat('Conc1', (0.60, 0.55, 0.44), (0.50, 0.40, 0.20), metallic=0.05, roughness=0.55),
        reg_mat('Conc2', (0.65, 0.60, 0.50), (0.52, 0.42, 0.22), metallic=0.05, roughness=0.58),
        reg_mat('Conc3', (0.55, 0.50, 0.40), (0.45, 0.36, 0.18), metallic=0.05, roughness=0.60),
        reg_mat('Conc4', (0.70, 0.65, 0.55), (0.55, 0.44, 0.23), metallic=0.05, roughness=0.52),
    ]

    # Mixed warm towers
    M_WARM = [
        reg_mat('Warm1', (0.45, 0.40, 0.55), (0.48, 0.38, 0.65), metallic=0.20, roughness=0.40),
        reg_mat('Warm2', (0.55, 0.45, 0.35), (0.55, 0.42, 0.20), metallic=0.10, roughness=0.50),
        reg_mat('Warm3', (0.38, 0.45, 0.35), (0.35, 0.48, 0.25), metallic=0.15, roughness=0.45),
    ]

    ALL_BG = M_GLASS + M_CONC + M_WARM

    # ── USD Header ────────────────────────────────────────────────────────────
    L += '#usda 1.0'
    L += '('
    L += '    defaultPrim = "World"'
    L += '    upAxis = "Y"'
    L += '    metersPerUnit = 50'
    L += '    startTimeCode = 0'
    L += '    endTimeCode = 240'
    L += '    timeCodesPerSecond = 24'
    L += ')'
    L.blank()
    L += 'def Xform "World"'
    L += '{'
    L.blank()

    # ── Materials ─────────────────────────────────────────────────────────────
    write_materials(L)

    # ── Night Sky (DomeLight) ─────────────────────────────────────────────────
    L += '    def DomeLight "NightSky"'
    L += '    {'
    L += '        float inputs:intensity = 0.04'
    L += '        color3f inputs:color = (0.005, 0.010, 0.030)'
    L += '    }'
    L.blank()

    # ── Distant city haze (RectLight facing down as fill) ────────────────────
    L += '    def RectLight "CityHaze"'
    L += '    {'
    L += '        float inputs:intensity = 80'
    L += '        float inputs:width = 30'
    L += '        float inputs:height = 30'
    L += '        color3f inputs:color = (0.3, 0.35, 0.5)'
    L += '        double3 xformOp:translate = (6, 18, 0)'
    L += '        float3 xformOp:rotateXYZ = (90, 0, 0)'
    L += '        uniform token[] xformOpOrder = ["xformOp:translate", "xformOp:rotateXYZ"]'
    L += '    }'
    L.blank()

    # ── Camera with animation ─────────────────────────────────────────────────
    # Sweep from south/upriver toward tower cluster
    cam_keys = [
        (0,   (-3.5, 1.8,  -14), (3.5, 4.5, 1.5)),   # Wide opening from downriver
        (48,  (-2.5, 2.2,  -10), (4.5, 5.5, 0.5)),   # Pulling in
        (96,  (-1.5, 2.8,   -6), (5.2, 6.5, 0.3)),   # Getting close
        (144, (-0.5, 3.4,   -3), (5.6, 7.5, 0.2)),   # Dramatic approach
        (192, ( 0.5, 4.0,   -1), (5.8, 9.0, 0.3)),   # Near the towers
        (240, ( 1.2, 5.0,    1), (6.0, 11.0, 0.3)),  # Looking up at Shanghai Tower
    ]
    L += '    def Camera "MainCamera"'
    L += '    {'
    L += '        float2 clippingRange = (0.01, 2000)'
    L += '        float focalLength = 28'
    L += '        float horizontalAperture = 36'
    L += '        float verticalAperture = 24'
    L += '        matrix4d xformOp:transform.timeSamples = {'
    for frame, eye, tgt in cam_keys:
        mat_str = look_at_matrix(eye, tgt)
        L += f'            {frame}: {mat_str},'
    L += '        }'
    L += '        uniform token[] xformOpOrder = ["xformOp:transform"]'
    L += '    }'
    L.blank()

    # ── Huangpu River ──────────────────────────────────────────────────────────
    # Wide flat plane: x from -3 to 3 (6 units = 300m), z from -16 to 16
    cube_prim(L, 'HuangpuRiver', 0, -0.06, 0, 3.0, 0.06, 16.0, M_RIVER, indent=1)

    # ── Ground planes ─────────────────────────────────────────────────────────
    cube_prim(L, 'GroundPudong', 9.5, -0.08, 0, 9.5, 0.08, 16.0, M_GROUND, indent=1)
    cube_prim(L, 'GroundBund',   -7.0, -0.08, 0, 5.0, 0.08, 16.0, M_GROUND, indent=1)

    # ═════════════════════════════════════════════════════════════════════════
    # LANDMARK BUILDINGS
    # ═════════════════════════════════════════════════════════════════════════
    L += '    def Xform "Landmarks"'
    L += '    {'
    L.blank()

    # ── Oriental Pearl Tower (x=3.2, z=1.8) ──────────────────────────────────
    # Hot pink spheres on a column with three diagonal legs
    px, pz = 3.2, 1.8
    L += '        def Xform "OrientalPearlTower"'
    L += '        {'
    L.blank()

    # Three outward-leaning legs
    for i, ang_deg in enumerate([30, 150, 270]):
        ang = math.radians(ang_deg)
        lx = px + math.cos(ang) * 0.65
        lz = pz + math.sin(ang) * 0.65
        cylinder_prim(L, f'Leg{i+1}', lx, 1.05, lz, 0.17, 2.1, M_PEARL, indent=3)

    # Diagonal support rings / connectors
    for i, ang_deg in enumerate([30, 150, 270]):
        ang = math.radians(ang_deg)
        # Small block connector
        cx2 = px + math.cos(ang) * 0.30
        cz2 = pz + math.sin(ang) * 0.30
        cube_prim(L, f'LegBase{i+1}', cx2, 0.4, cz2, 0.12, 0.4, 0.12, M_PEARL, indent=3)

    # Central column (thin, full height)
    cylinder_prim(L, 'MainColumn', px, 4.9, pz, 0.14, 9.8, M_PEARL, indent=3)

    # Lower base drum (thick section at bottom of column)
    cylinder_prim(L, 'BaseDrum', px, 1.0, pz, 0.30, 2.0, M_PEARL, indent=3)

    # LOWER LARGE SPHERE (center ~y=3.5, radius 1.35)
    sphere_prim(L, 'LowerSphere', px, 3.5, pz, 1.35, M_PEARL, indent=3)

    # Observation deck ring above lower sphere
    cylinder_prim(L, 'ObsDeck', px, 5.0, pz, 0.55, 0.18, M_PEARL, indent=3)

    # UPPER SPHERE (center ~y=6.6, radius 0.95)
    sphere_prim(L, 'UpperSphere', px, 6.6, pz, 0.95, M_PEARL, indent=3)

    # Small top sphere
    sphere_prim(L, 'TopSphere', px, 8.6, pz, 0.28, M_PEARL, indent=3)

    # Antenna mast
    cylinder_prim(L, 'Antenna', px, 9.32, pz, 0.035, 1.44, M_PEARL, indent=3)

    L += '        }'  # End OrientalPearlTower
    L.blank()

    # ── Shanghai Tower (x=6.1, z=0.3) — twisted tapering form ────────────────
    stx, stz = 6.1, 0.3
    L += '        def Xform "ShanghaiTower"'
    L += '        {'
    L.blank()

    # 9 stacked sections, each rotates ~7° and shrinks
    sh_sections = [
        # yBase, sectionH, w,    d,    rotY
        (0.0,   2.6,  0.88, 0.88,  0.0),
        (2.6,   2.1,  0.78, 0.78,  7.0),
        (4.7,   1.9,  0.68, 0.68, 14.0),
        (6.6,   1.7,  0.58, 0.58, 21.0),
        (8.3,   1.5,  0.48, 0.48, 28.0),
        (9.8,   1.2,  0.38, 0.38, 35.0),
        (11.0,  0.9,  0.28, 0.28, 42.0),
        (11.9,  0.6,  0.20, 0.20, 49.0),
        (12.5,  0.4,  0.12, 0.12, 56.0),
    ]
    for i, (yb, sh, sw, sd, ry) in enumerate(sh_sections):
        p3 = '            '
        p4 = '                '
        L += f'{p3}def Xform "Section{i+1:02d}"'
        L += f'{p3}{{'
        L += f'{p4}double3 xformOp:translate = ({stx:.4f}, {yb+sh/2:.4f}, {stz:.4f})'
        L += f'{p4}float xformOp:rotateY = {ry:.1f}'
        L += f'{p4}float3 xformOp:scale = ({sw/2:.5f}, {sh/2:.5f}, {sd/2:.5f})'
        L += f'{p4}uniform token[] xformOpOrder = ["xformOp:translate", "xformOp:rotateY", "xformOp:scale"]'
        L += f'{p4}def Cube "Mesh"'
        L += f'{p4}{{'
        L += f'{p4}    double size = 2'
        L += f'{p4}    rel material:binding = <{M_SHTWR}>'
        L += f'{p4}}}'
        L += f'{p3}}}'
        L.blank()

    # Shanghai Tower antenna
    cylinder_prim(L, 'Antenna', stx, 13.3, stz, 0.028, 1.5, M_SHTWR, indent=3)
    L += '        }'  # End ShanghaiTower
    L.blank()

    # ── Jin Mao Tower (x=5.3, z=0.6) — stepped pagoda ────────────────────────
    jmx, jmz = 5.3, 0.6
    L += '        def Xform "JinMaoTower"'
    L += '        {'
    L.blank()

    jm_sections = [
        # yBase, h,    w,    d
        (0.0,  3.0, 0.75, 0.75),
        (3.0,  1.5, 0.64, 0.64),
        (4.5,  1.1, 0.54, 0.54),
        (5.6,  0.9, 0.46, 0.46),
        (6.5,  0.7, 0.38, 0.38),
        (7.2,  0.6, 0.31, 0.31),
        (7.8,  0.4, 0.24, 0.24),
        (8.2,  0.3, 0.18, 0.18),
        (8.5,  0.15, 0.12, 0.12),
    ]
    for i, (yb, sh, sw, sd) in enumerate(jm_sections):
        cube_prim(L, f'Level{i+1:02d}', jmx, yb + sh/2, jmz, sw/2, sh/2, sd/2, M_JINMAO, indent=3)

    # Needle
    cylinder_prim(L, 'Needle', jmx, 8.78, jmz, 0.025, 0.56, M_JINMAO, indent=3)
    L += '        }'  # End JinMaoTower
    L.blank()

    # ── Shanghai World Financial Center (x=5.8, z=-0.4) ──────────────────────
    # Tall rectangular prism, slightly tapering at top, silver-blue
    swx, swz = 5.8, -0.4
    L += '        def Xform "SWFC"'
    L += '        {'
    L.blank()

    # Main body 3 stacked sections, slightly narrowing
    swfc_sects = [
        # yBase, h,    w,    d
        (0.0,  4.5, 0.56, 0.42),
        (4.5,  3.2, 0.52, 0.38),
        (7.7,  2.0, 0.44, 0.32),
        (9.7,  0.5, 0.32, 0.24),
    ]
    for i, (yb, sh, sw, sd) in enumerate(swfc_sects):
        cube_prim(L, f'Body{i+1:02d}', swx, yb + sh/2, swz, sw/2, sh/2, sd/2, M_SWFC, indent=3)

    # Signature trapezoid opening near top - approximate with two dark blocks
    # (just narrow the top and let the shape do it visually)
    cube_prim(L, 'TopBlock', swx, 10.35, swz, 0.14, 0.35, 0.12, M_SWFC, indent=3)
    cylinder_prim(L, 'Spire', swx, 11.0, swz, 0.022, 0.6, M_SWFC, indent=3)

    L += '        }'  # End SWFC
    L.blank()
    L += '    }'  # End Landmarks
    L.blank()

    # ═════════════════════════════════════════════════════════════════════════
    # PUDONG BACKGROUND BUILDINGS  (50–80 varied towers)
    # ═════════════════════════════════════════════════════════════════════════
    L += '    def Xform "PudongBuildings"'
    L += '    {'
    L.blank()

    placed = []
    bg_count = [0]

    def can_place(bx, bz, half_w, half_d):
        for (ox, oz, hw, hd) in placed:
            overlap_x = abs(bx - ox) < (half_w + hw + 0.08)
            overlap_z = abs(bz - oz) < (half_d + hd + 0.08)
            if overlap_x and overlap_z:
                return False
        return True

    def add_bg(bx, bz, min_h, max_h, min_w, max_w, mat_list=None):
        if mat_list is None:
            mat_list = ALL_BG
        h = random.uniform(min_h, max_h)
        w = random.uniform(min_w, max_w)
        d = random.uniform(min_w * 0.7, max_w * 0.9)
        if not can_place(bx, bz, w/2, d/2):
            return False
        placed.append((bx, bz, w/2, d/2))
        bg_count[0] += 1
        mat = random.choice(mat_list)
        name = f'BG{bg_count[0]:04d}'
        building_prim(L, name, bx, bz, w, h, d, mat, indent=2)
        return True

    def scatter_cluster(cx, cz_range, x_range, min_h, max_h, min_w, max_w, n, mat_list=None):
        attempts = 0
        placed_here = 0
        while placed_here < n and attempts < n * 8:
            bx = random.uniform(cx[0], cx[1])
            bz = random.uniform(cz_range[0], cz_range[1])
            if add_bg(bx, bz, min_h, max_h, min_w, max_w, mat_list):
                placed_here += 1
            attempts += 1

    # Near-river cluster (x=3.5–5.0) — taller buildings, dramatic silhouette
    scatter_cluster((3.5, 5.0), (-9, 9),   None, 3.5, 7.5, 0.28, 0.65, 22, M_GLASS)

    # Mid-Pudong cluster (x=5.0–8.5) — mix of heights
    scatter_cluster((5.0, 8.5), (-10, 10), None, 2.5, 6.0, 0.32, 0.80, 22)

    # Deep background (x=8.5–16) — shorter, denser
    scatter_cluster((8.5, 16.0), (-11, 11), None, 1.5, 4.0, 0.40, 1.10, 20)

    # Fill gaps near landmarks with small connector buildings
    scatter_cluster((3.5, 7.5), (-3, 3), None, 1.5, 3.5, 0.22, 0.50, 12)

    L += '    }'  # End PudongBuildings
    L.blank()

    # ═════════════════════════════════════════════════════════════════════════
    # THE BUND  (west bank — classical European low buildings)
    # ═════════════════════════════════════════════════════════════════════════
    L += '    def Xform "TheBund"'
    L += '    {'
    L.blank()

    bund_count = [0]

    def add_bund(bx, bz, w, h, d):
        bund_count[0] += 1
        mat = random.choice(M_BUND)
        name = f'Bund{bund_count[0]:04d}'
        building_prim(L, name, bx, bz, w, h, d, mat, indent=2)

    # Two rows of classical buildings
    z_vals = list(range(-13, 14))
    random.shuffle(z_vals)
    for zi, z in enumerate(z_vals):
        jitter_z = z + random.uniform(-0.3, 0.3)
        h = random.uniform(1.5, 3.2)
        w = random.uniform(0.85, 1.60)
        d = random.uniform(0.55, 1.0)
        row = random.choice([0, 1])
        bx = -3.8 - row * 0.9 - random.uniform(0.0, 0.4)
        add_bund(bx, jitter_z, w, h, d)

    # Extra back row
    for z in range(-12, 13, 2):
        jitter_z = z + random.uniform(-0.6, 0.6)
        h = random.uniform(1.2, 2.5)
        w = random.uniform(0.7, 1.3)
        d = random.uniform(0.5, 0.9)
        bx = -5.8 - random.uniform(0.0, 0.5)
        add_bund(bx, jitter_z, w, h, d)

    L += '    }'  # End TheBund
    L.blank()

    # ═════════════════════════════════════════════════════════════════════════
    # LIGHTS
    # ═════════════════════════════════════════════════════════════════════════
    L += '    def Xform "Lights"'
    L += '    {'
    L.blank()

    # Tower glow lights (volumetric-feel around each landmark)
    tower_glows = [
        ('PearlGlow1', px,   2.0, pz,   0.8, 600, (1.2, 0.25, 0.55)),
        ('PearlGlow2', px,   5.5, pz,   0.6, 400, (1.2, 0.25, 0.55)),
        ('PearlGlow3', px,   7.5, pz,   0.4, 250, (1.0, 0.20, 0.45)),
        ('ShTwrGlow1', stx,  4.0, stz,  0.5, 350, (0.30, 0.60, 1.0)),
        ('ShTwrGlow2', stx,  9.0, stz,  0.4, 300, (0.30, 0.65, 1.0)),
        ('ShTwrGlow3', stx, 13.0, stz,  0.3, 200, (0.25, 0.55, 0.95)),
        ('JinMaoGlow1', jmx, 4.0, jmz, 0.4, 280, (0.85, 0.65, 0.10)),
        ('JinMaoGlow2', jmx, 7.5, jmz, 0.3, 180, (0.85, 0.65, 0.10)),
        ('SWFCGlow1',   swx, 5.0, swz, 0.4, 250, (0.45, 0.60, 0.85)),
        ('SWFCGlow2',   swx, 9.5, swz, 0.3, 180, (0.45, 0.60, 0.85)),
    ]
    for nm, gx, gy, gz, gr, gi, gc in tower_glows:
        sphere_light(L, nm, gx, gy, gz, gr, gi, gc, indent=2)

    # Bund promenade streetlights
    for si, z in enumerate(range(-13, 14, 2)):
        sphere_light(L, f'BundStreet{si:03d}', -3.1, 0.45, float(z),
                     0.04, 280, (1.0, 0.92, 0.72), indent=2)
        # Bund building facade lights (warm amber)
        sphere_light(L, f'BundFacade{si:03d}', -4.2, 1.2, float(z),
                     0.12, 150, (1.0, 0.85, 0.55), indent=2)

    # Pudong street / podium lights
    for xi, gx in enumerate(range(4, 14, 3)):
        for zi, gz in enumerate([-8, -4, 0, 4, 8]):
            sphere_light(L, f'PudStreet{xi*10+zi:04d}', float(gx), 0.32, float(gz),
                         0.04, 220, (1.0, 0.88, 0.65), indent=2)

    # River surface reflection glows
    refl_lights = [
        ('Refl01', -0.5, 0.08,  -8, 0.5, 120, (0.25, 0.35, 0.65)),
        ('Refl02',  0.5, 0.08,  -5, 0.5, 140, (0.35, 0.45, 0.75)),
        ('Refl03', -0.3, 0.08,  -2, 0.6, 180, (0.55, 0.35, 0.75)),   # Pearl reflection
        ('Refl04',  0.4, 0.08,   1, 0.6, 200, (0.55, 0.35, 0.75)),
        ('Refl05', -0.5, 0.08,   4, 0.5, 160, (0.30, 0.45, 0.80)),
        ('Refl06',  0.5, 0.08,   7, 0.4, 120, (0.28, 0.40, 0.70)),
        ('Refl07',  0.0, 0.08,  -11, 0.4, 90, (0.20, 0.30, 0.60)),
    ]
    for nm, rx, ry, rz, rrad, ri, rc in refl_lights:
        sphere_light(L, nm, rx, ry, rz, rrad, ri, rc, indent=2)

    # General ambient city fill lights scattered through Pudong
    random.seed(SEED + 100)
    for fi in range(18):
        fx = random.uniform(3.5, 12.0)
        fz = random.uniform(-10, 10)
        fy = random.uniform(1.0, 5.0)
        fi_int = random.uniform(80, 200)
        fc = (random.uniform(0.3, 0.8), random.uniform(0.4, 0.8), random.uniform(0.5, 1.0))
        sphere_light(L, f'CityFill{fi:03d}', fx, fy, fz, 0.3, fi_int, fc, indent=2)

    L += '    }'  # End Lights
    L.blank()

    L += '}'  # End World

    # ── Write file ────────────────────────────────────────────────────────────
    with open(OUTPUT_PATH, 'w') as f:
        f.write(L.text())

    lines = L.text().splitlines()
    print(f"Wrote: {OUTPUT_PATH}")
    print(f"Lines: {len(lines)}")
    print(f"Buildings placed: {bg_count[0]} background + {bund_count[0]} Bund")


if __name__ == '__main__':
    build_scene()
