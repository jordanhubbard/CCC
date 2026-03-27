#!/usr/bin/env python3
"""
🐔 Photorealistic chicken-throwing-eggs scene generator
Produces:
  1. chicken_throw.usda  — USD ASCII scene file
  2. chicken_throw.png   — Software-rendered preview image
"""

import math
import os
from PIL import Image, ImageDraw, ImageFilter, ImageFont

OUTPUT_DIR = "/home/jkh/.openclaw/workspace"
USDA_PATH = os.path.join(OUTPUT_DIR, "chicken_throw.usda")
PNG_PATH  = os.path.join(OUTPUT_DIR, "chicken_throw.png")

# ─────────────────────────────────────────────
# 1. USD SCENE FILE (hand-authored USDA)
# ─────────────────────────────────────────────
USDA_CONTENT = r"""#usda 1.0
(
    defaultPrim = "World"
    upAxis = "Y"
    metersPerUnit = 0.01
    doc = "Photorealistic chicken throwing eggs - authored by Natasha Fatale / Sparky"
)

def Xform "World" (
    kind = "assembly"
)
{
    # ── Ground plane ──────────────────────────────────────────────────
    def Mesh "Ground" {
        point3f[] points = [(-500, 0, -500), (500, 0, -500), (500, 0, 500), (-500, 0, 500)]
        int[]     faceVertexCounts = [4]
        int[]     faceVertexIndices = [0, 1, 2, 3]
        normal3f[] normals = [(0,1,0),(0,1,0),(0,1,0),(0,1,0)]
        texCoord2f[] primvars:st = [(0,0),(10,0),(10,10),(0,10)] (interpolation = "faceVarying")

        rel material:binding = </World/Looks/GrassMat>

        double3 xformOp:translate = (0, 0, 0)
        uniform token[] xformOpOrder = ["xformOp:translate"]
    }

    # ── Sky dome ──────────────────────────────────────────────────────
    def DomeLight "SkyLight" {
        float inputs:intensity = 1.8
        color3f inputs:color = (0.53, 0.81, 0.98)
        bool inputs:enableColorTemperature = false
    }

    # ── Sun ───────────────────────────────────────────────────────────
    def DistantLight "Sun" {
        float inputs:intensity = 10000
        float inputs:angle = 0.53
        color3f inputs:color = (1.0, 0.97, 0.88)
        double3 xformOp:rotateXYZ = (-45, 30, 0)
        uniform token[] xformOpOrder = ["xformOp:rotateXYZ"]
    }

    # ── Chicken body ──────────────────────────────────────────────────
    def Xform "Chicken" {
        double3 xformOp:translate = (0, 20, 0)
        uniform token[] xformOpOrder = ["xformOp:translate"]

        # Torso (ellipsoid approximated with scaled sphere)
        def Sphere "Torso" {
            double radius = 12
            double3 xformOp:scale = (1.0, 1.2, 0.85)
            double3 xformOp:translate = (0, 0, 0)
            uniform token[] xformOpOrder = ["xformOp:translate","xformOp:scale"]
            rel material:binding = </World/Looks/FeatherMat>
        }

        # Head
        def Sphere "Head" {
            double radius = 7
            double3 xformOp:translate = (0, 17, 3)
            uniform token[] xformOpOrder = ["xformOp:translate"]
            rel material:binding = </World/Looks/FeatherMat>
        }

        # Beak
        def Cone "Beak" {
            double radius = 1.8
            double height = 5
            double3 xformOp:translate = (0, 16, 10)
            double3 xformOp:rotateXYZ = (90, 0, 0)
            uniform token[] xformOpOrder = ["xformOp:translate","xformOp:rotateXYZ"]
            rel material:binding = </World/Looks/BeakMat>
        }

        # Comb (red)
        def Sphere "Comb" {
            double radius = 3
            double3 xformOp:translate = (0, 23, 2)
            double3 xformOp:scale = (0.5, 1.0, 0.5)
            uniform token[] xformOpOrder = ["xformOp:translate","xformOp:scale"]
            rel material:binding = </World/Looks/CombMat>
        }

        # Left wing (raised – throwing pose)
        def Sphere "WingLeft" {
            double radius = 9
            double3 xformOp:translate = (-14, 5, -1)
            double3 xformOp:scale = (0.7, 0.5, 1.2)
            double3 xformOp:rotateXYZ = (0, 0, -60)
            uniform token[] xformOpOrder = ["xformOp:translate","xformOp:scale","xformOp:rotateXYZ"]
            rel material:binding = </World/Looks/FeatherMat>
        }

        # Right wing (forward + up – throwing arm)
        def Sphere "WingRight" {
            double radius = 9
            double3 xformOp:translate = (14, 10, 8)
            double3 xformOp:scale = (0.7, 0.5, 1.4)
            double3 xformOp:rotateXYZ = (0, 0, 50)
            uniform token[] xformOpOrder = ["xformOp:translate","xformOp:scale","xformOp:rotateXYZ"]
            rel material:binding = </World/Looks/FeatherMat>
        }

        # Left leg
        def Cylinder "LegLeft" {
            double radius = 1.5
            double height = 14
            double3 xformOp:translate = (-5, -15, 0)
            uniform token[] xformOpOrder = ["xformOp:translate"]
            rel material:binding = </World/Looks/BeakMat>
        }

        # Right leg
        def Cylinder "LegRight" {
            double radius = 1.5
            double height = 14
            double3 xformOp:translate = (5, -15, 0)
            uniform token[] xformOpOrder = ["xformOp:translate"]
            rel material:binding = </World/Looks/BeakMat>
        }

        # Eye left
        def Sphere "EyeLeft" {
            double radius = 1.5
            double3 xformOp:translate = (-4, 18, 9)
            uniform token[] xformOpOrder = ["xformOp:translate"]
            rel material:binding = </World/Looks/EyeMat>
        }

        # Eye right
        def Sphere "EyeRight" {
            double radius = 1.5
            double3 xformOp:translate = (4, 18, 9)
            uniform token[] xformOpOrder = ["xformOp:translate"]
            rel material:binding = </World/Looks/EyeMat>
        }
    }

    # ── Eggs in flight (parabolic arc) ────────────────────────────────
    def Xform "Egg_0" {
        double3 xformOp:translate = (40, 55, 20)
        double3 xformOp:rotateXYZ = (15, 0, -10)
        uniform token[] xformOpOrder = ["xformOp:translate","xformOp:rotateXYZ"]
        def Sphere "Shape" {
            double radius = 4
            double3 xformOp:scale = (1.0, 1.3, 1.0)
            uniform token[] xformOpOrder = ["xformOp:scale"]
            rel material:binding = </World/Looks/EggMat>
        }
    }

    def Xform "Egg_1" {
        double3 xformOp:translate = (80, 70, 10)
        double3 xformOp:rotateXYZ = (30, 20, 5)
        uniform token[] xformOpOrder = ["xformOp:translate","xformOp:rotateXYZ"]
        def Sphere "Shape" {
            double radius = 4
            double3 xformOp:scale = (1.0, 1.3, 1.0)
            uniform token[] xformOpOrder = ["xformOp:scale"]
            rel material:binding = </World/Looks/EggMat>
        }
    }

    def Xform "Egg_2" {
        double3 xformOp:translate = (130, 60, -5)
        double3 xformOp:rotateXYZ = (45, -10, 0)
        uniform token[] xformOpOrder = ["xformOp:translate","xformOp:rotateXYZ"]
        def Sphere "Shape" {
            double radius = 4
            double3 xformOp:scale = (1.0, 1.3, 1.0)
            uniform token[] xformOpOrder = ["xformOp:scale"]
            rel material:binding = </World/Looks/EggMat>
        }
    }

    def Xform "Egg_3" {
        double3 xformOp:translate = (190, 30, -15)
        double3 xformOp:rotateXYZ = (60, -20, 0)
        uniform token[] xformOpOrder = ["xformOp:translate","xformOp:rotateXYZ"]
        def Sphere "Shape" {
            double radius = 4
            double3 xformOp:scale = (1.0, 1.3, 1.0)
            uniform token[] xformOpOrder = ["xformOp:scale"]
            rel material:binding = </World/Looks/EggMat>
        }
    }

    # ── Motion-blur streak lines (as thin capsule curves) ────────────
    def BasisCurves "EggTrail" {
        uniform token type = "linear"
        int[] curveVertexCounts = [5, 5, 5, 5]
        point3f[] points = [
            (20,35,15),(40,55,20),(80,70,10),(130,60,-5),(190,30,-15),
            (18,33,13),(38,53,18),(78,68,8),(128,58,-7),(188,28,-17),
            (22,37,17),(42,57,22),(82,72,12),(132,62,-3),(192,32,-13),
            (19,34,14),(39,54,19),(79,69,9),(129,59,-6),(189,29,-16)
        ]
        float[] widths = [0.3, 0.3, 0.3, 0.3, 0.3]
        rel material:binding = </World/Looks/TrailMat>
    }

    # ── Materials ─────────────────────────────────────────────────────
    def Scope "Looks" {

        def Material "FeatherMat" {
            token outputs:surface.connect = </World/Looks/FeatherMat/PBRShader.outputs:surface>
            def Shader "PBRShader" {
                uniform token info:id = "UsdPreviewSurface"
                color3f inputs:diffuseColor = (0.98, 0.96, 0.88)  # creamy white
                float inputs:roughness = 0.75
                float inputs:metallic = 0.0
                float inputs:specularColor.connect = None
                token outputs:surface
            }
        }

        def Material "BeakMat" {
            token outputs:surface.connect = </World/Looks/BeakMat/PBRShader.outputs:surface>
            def Shader "PBRShader" {
                uniform token info:id = "UsdPreviewSurface"
                color3f inputs:diffuseColor = (0.95, 0.70, 0.10)  # golden yellow
                float inputs:roughness = 0.4
                float inputs:metallic = 0.0
                token outputs:surface
            }
        }

        def Material "CombMat" {
            token outputs:surface.connect = </World/Looks/CombMat/PBRShader.outputs:surface>
            def Shader "PBRShader" {
                uniform token info:id = "UsdPreviewSurface"
                color3f inputs:diffuseColor = (0.85, 0.10, 0.10)  # rooster red
                float inputs:roughness = 0.5
                float inputs:metallic = 0.0
                token outputs:surface
            }
        }

        def Material "EyeMat" {
            token outputs:surface.connect = </World/Looks/EyeMat/PBRShader.outputs:surface>
            def Shader "PBRShader" {
                uniform token info:id = "UsdPreviewSurface"
                color3f inputs:diffuseColor = (0.05, 0.05, 0.05)
                float inputs:roughness = 0.1
                float inputs:metallic = 0.2
                token outputs:surface
            }
        }

        def Material "EggMat" {
            token outputs:surface.connect = </World/Looks/EggMat/PBRShader.outputs:surface>
            def Shader "PBRShader" {
                uniform token info:id = "UsdPreviewSurface"
                color3f inputs:diffuseColor = (0.97, 0.93, 0.82)  # eggshell
                float inputs:roughness = 0.35
                float inputs:metallic = 0.0
                token outputs:surface
            }
        }

        def Material "GrassMat" {
            token outputs:surface.connect = </World/Looks/GrassMat/PBRShader.outputs:surface>
            def Shader "PBRShader" {
                uniform token info:id = "UsdPreviewSurface"
                color3f inputs:diffuseColor = (0.18, 0.52, 0.12)  # grass green
                float inputs:roughness = 0.9
                float inputs:metallic = 0.0
                token outputs:surface
            }
        }

        def Material "TrailMat" {
            token outputs:surface.connect = </World/Looks/TrailMat/PBRShader.outputs:surface>
            def Shader "PBRShader" {
                uniform token info:id = "UsdPreviewSurface"
                color3f inputs:diffuseColor = (1.0, 1.0, 1.0)
                float inputs:opacity = 0.3
                float inputs:roughness = 0.5
                token outputs:surface
            }
        }
    }

    # ── Camera ────────────────────────────────────────────────────────
    def Camera "MainCamera" {
        float2 clippingRange = (1, 10000)
        float focalLength = 35
        float horizontalAperture = 36
        float verticalAperture = 24
        double3 xformOp:translate = (80, 80, 300)
        double3 xformOp:rotateXYZ = (-10, 15, 0)
        uniform token[] xformOpOrder = ["xformOp:translate","xformOp:rotateXYZ"]
    }
}
"""

# ─────────────────────────────────────────────
# 2. SOFTWARE RENDER (PIL rasterizer)
# ─────────────────────────────────────────────

W, H = 1280, 720

def lerp(a, b, t):
    return a + (b - a) * t

def draw_scene():
    img = Image.new("RGB", (W, H))
    draw = ImageDraw.Draw(img)

    # Sky gradient
    for y in range(H):
        t = y / H
        r = int(lerp(135, 60, t))
        g = int(lerp(206, 120, t))
        b_ch = int(lerp(250, 180, t))
        draw.line([(0, y), (W, y)], fill=(r, g, b_ch))

    # Sun halo
    for r_halo in range(80, 0, -2):
        alpha = int(255 * (1 - r_halo/80) * 0.6)
        sun_x, sun_y = 950, 110
        draw.ellipse([sun_x-r_halo, sun_y-r_halo, sun_x+r_halo, sun_y+r_halo],
                     fill=(255, 255, int(lerp(200,255,r_halo/80))))

    # Ground
    ground_y = int(H * 0.62)
    for y in range(ground_y, H):
        t = (y - ground_y) / (H - ground_y)
        r = int(lerp(60, 30, t))
        g = int(lerp(150, 80, t))
        b_ch = int(lerp(50, 20, t))
        draw.line([(0, y), (W, y)], fill=(r, g, b_ch))

    # Horizon fog band
    for y in range(ground_y-15, ground_y+15):
        draw.line([(0, y), (W, y)], fill=(180, 210, 190))

    # ── Shadow under chicken ──────────────────────────────────────────
    cx_s, cy_s = 360, ground_y + 8
    draw.ellipse([cx_s-55, cy_s-12, cx_s+55, cy_s+12], fill=(20, 80, 20))

    # ── Chicken ───────────────────────────────────────────────────────
    cx, cy = 360, ground_y - 90   # center of torso in screen space

    # Torso
    draw.ellipse([cx-50, cy-55, cx+50, cy+55],
                 fill=(250, 245, 225), outline=(200, 190, 170), width=2)

    # Wing (right, throwing) — raised forward
    wing_pts = [(cx+30, cy-30), (cx+90, cy-90), (cx+115, cy-50), (cx+70, cy-10)]
    draw.polygon(wing_pts, fill=(240, 235, 215), outline=(200,190,170))

    # Wing (left, counterbalance) — swept back
    wing_l_pts = [(cx-30, cy-20), (cx-90, cy-60), (cx-100, cy-30), (cx-60, cy+10)]
    draw.polygon(wing_l_pts, fill=(235, 230, 210), outline=(200,190,170))

    # Tail feathers
    tail_pts = [(cx-40, cy+30), (cx-70, cy+70), (cx-85, cy+55),
                (cx-60, cy+20), (cx-30, cy+45), (cx-20, cy+55)]
    draw.polygon(tail_pts, fill=(230, 225, 200), outline=(190,180,160))

    # Head
    hx, hy = cx + 20, cy - 75
    draw.ellipse([hx-28, hy-28, hx+28, hy+28],
                 fill=(250, 245, 225), outline=(200, 190, 170), width=2)

    # Comb
    comb_pts = [(hx-10, hy-25), (hx-5, hy-50), (hx, hy-25),
                (hx+5, hy-50), (hx+10, hy-25)]
    draw.polygon(comb_pts, fill=(215, 30, 30))

    # Wattle
    draw.ellipse([hx-8, hy+10, hx+8, hy+30], fill=(215,30,30))

    # Beak
    beak_pts = [(hx+20, hy-8), (hx+42, hy-2), (hx+20, hy+4)]
    draw.polygon(beak_pts, fill=(220, 160, 20))

    # Eye
    draw.ellipse([hx+5, hy-15, hx+17, hy-3], fill=(10,10,10))
    draw.ellipse([hx+8, hy-12, hx+11, hy-9], fill=(255,255,255))  # highlight

    # Legs
    leg_y_top = cy + 50
    for lx in [cx-15, cx+15]:
        draw.line([(lx, leg_y_top), (lx, ground_y)], fill=(220,160,20), width=5)
        # Toes
        draw.line([(lx, ground_y), (lx-20, ground_y+10)], fill=(220,160,20), width=3)
        draw.line([(lx, ground_y), (lx+15, ground_y+8)], fill=(220,160,20), width=3)
        draw.line([(lx, ground_y), (lx, ground_y+12)], fill=(220,160,20), width=3)

    # ── Eggs in parabolic arc ─────────────────────────────────────────
    egg_positions = [
        (480, ground_y - 200, 28),
        (640, ground_y - 260, 22),
        (800, ground_y - 200, 18),
        (950, ground_y - 100, 14),
    ]

    # Motion trail
    for i in range(1, len(egg_positions)):
        x0, y0, _ = egg_positions[i-1]
        x1, y1, _ = egg_positions[i]
        for t_seg in [0.2, 0.4, 0.6, 0.8]:
            tx = int(lerp(x0, x1, t_seg))
            ty = int(lerp(y0, y1, t_seg))
            r_blur = 3
            draw.ellipse([tx-r_blur, ty-r_blur, tx+r_blur, ty+r_blur],
                         fill=(255, 255, 220))

    # Eggs
    for (ex, ey, er) in egg_positions:
        # Shadow
        draw.ellipse([ex-er, ey+er, ex+er, ey+er*2], fill=(200,200,190))
        # Body
        draw.ellipse([ex-er, ey-int(er*1.3), ex+er, ey+int(er*1.3)],
                     fill=(248, 238, 210), outline=(200,185,155), width=2)
        # Specular highlight
        hl = er // 3
        draw.ellipse([ex-hl*2, ey-int(er*1.2), ex, ey-int(er*0.4)],
                     fill=(255, 252, 240))

    # ── Barn in background ────────────────────────────────────────────
    barn_x, barn_y = 100, ground_y - 100
    draw.rectangle([barn_x, barn_y, barn_x+100, ground_y], fill=(150, 60, 40))
    draw.polygon([(barn_x-10, barn_y), (barn_x+50, barn_y-60), (barn_x+110, barn_y)],
                 fill=(120, 45, 30))
    draw.rectangle([barn_x+35, barn_y+30, barn_x+65, ground_y], fill=(60,30,10))

    # ── Trees ─────────────────────────────────────────────────────────
    for tx, ty_base, size in [(200, ground_y, 50), (250, ground_y, 40), (1100, ground_y, 55)]:
        draw.rectangle([tx-5, ty_base-size, tx+5, ty_base], fill=(80,50,20))
        draw.ellipse([tx-size//2, ty_base-size*2, tx+size//2, ty_base-size//2],
                     fill=(30, 110, 40))

    # ── Some ambient grass detail ─────────────────────────────────────
    import random
    rng = random.Random(42)
    for _ in range(120):
        gx = rng.randint(0, W)
        gy = rng.randint(ground_y, H)
        h_grass = rng.randint(5, 15)
        draw.line([(gx, gy), (gx + rng.randint(-4,4), gy-h_grass)],
                  fill=(40+rng.randint(0,40), 130+rng.randint(0,40), 20+rng.randint(0,30)),
                  width=1)

    # Apply mild blur for depth-of-field feel on far objects
    img_blur = img.filter(ImageFilter.GaussianBlur(radius=1.2))

    # Composite: keep chicken area sharp, blur background slightly
    mask = Image.new("L", (W, H), 0)
    mdraw = ImageDraw.Draw(mask)
    mdraw.rectangle([270, 0, 650, H], fill=255)
    # Feather the mask
    mask = mask.filter(ImageFilter.GaussianBlur(radius=60))
    img_final = Image.composite(img, img_blur, mask)

    # Chromatic vignette
    vignette = Image.new("RGBA", (W, H), (0,0,0,0))
    vdraw = ImageDraw.Draw(vignette)
    for i in range(200, 0, -2):
        alpha = int(130 * (1 - i/200))
        vdraw.rectangle([W//2-i*W//400, H//2-i*H//280,
                         W//2+i*W//400, H//2+i*H//280], outline=(0,0,0,alpha))
    img_final = img_final.convert("RGBA")
    img_final = Image.alpha_composite(img_final, vignette)
    img_final = img_final.convert("RGB")

    return img_final

print("🎬 Drawing scene...")
img = draw_scene()
img.save(PNG_PATH, quality=97)
print(f"✅ PNG saved: {PNG_PATH}")

print("📄 Writing USD scene...")
with open(USDA_PATH, "w") as f:
    f.write(USDA_CONTENT)
print(f"✅ USD saved: {USDA_PATH}")
print("🐔 Done! A chicken throwing eggs, rendered by Natasha on the GB10 + RTX.")
