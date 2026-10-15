//! Glyph atlases for the HUD text (printable ASCII and Spanish letters), rasterised once with GDI into BGRA textures:
//! Arial Bold for the HUD, Palatino Linotype for titles (boss names), a serif between Elden Ring's and League's.

use crate::scene::Sprite;
use lolread::cache::Tex;
use windows::Win32::Graphics::Gdi::*;
use windows::core::w;

const CHARS: &str = " !\"#$%&'()*+,-./0123456789:;<=>?@ABCDEFGHIJKLMNOPQRSTUVWXYZ[\\]^_`abcdefghijklmnopqrstuvwxyz{|}~ÁÉÍÓÚÑáéíóúñ¿¡·⚔✦◆❄";
/// the last ones are stat icons drawn with Segoe UI Symbol (AD sword, AP star, mana diamond, frost)
const ICONS: &str = "⚔✦◆❄";
const CELL_W: u32 = 48; // wide enough for Arial Bold's W at 44 px
const CELL_H: u32 = 48;
const COLS: u32 = 20;
const W: u32 = COLS * CELL_W;
const H: u32 = 6 * CELL_H;

/// per-glyph advance as a fraction of the cell height, measured with GDI when the atlas is built
static ADVANCE: std::sync::OnceLock<Vec<f32>> = std::sync::OnceLock::new();
static TITLE_ADVANCE: std::sync::OnceLock<Vec<f32>> = std::sync::OnceLock::new();

fn advance_in(adv: &std::sync::OnceLock<Vec<f32>>, i: usize) -> f32 {
    adv.get().and_then(|a| a.get(i).copied()).unwrap_or(0.41)
}

pub fn atlas() -> Tex {
    build(w!("Arial"), 700, 44, &ADVANCE)
}

/// The title face ("font_title").
pub fn title_atlas() -> Tex {
    build(w!("Palatino Linotype"), 700, 42, &TITLE_ADVANCE)
}

fn build(face: windows::core::PCWSTR, weight: i32, height: i32, advances: &std::sync::OnceLock<Vec<f32>>) -> Tex {
    let mut px = vec![0u8; (W * H * 4) as usize];
    unsafe {
        let dc = CreateCompatibleDC(None);
        let bmi = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER {
                biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: W as i32,
                biHeight: -(H as i32),
                biPlanes: 1,
                biBitCount: 32,
                biCompression: BI_RGB.0,
                ..Default::default()
            },
            ..Default::default()
        };
        let mut bits = std::ptr::null_mut();
        if let Ok(bmp) = CreateDIBSection(Some(dc), &bmi, DIB_RGB_COLORS, &mut bits, None, 0) {
            let old = SelectObject(dc, bmp.into());
            let font = CreateFontW(height, 0, 0, 0, weight, 0, 0, 0, DEFAULT_CHARSET, OUT_DEFAULT_PRECIS, CLIP_DEFAULT_PRECIS, ANTIALIASED_QUALITY, 0, face);
            let oldf = SelectObject(dc, font.into());
            SetBkMode(dc, TRANSPARENT);
            SetTextColor(dc, windows::Win32::Foundation::COLORREF(0x00ffffff));
            let symbols = CreateFontW(42, 0, 0, 0, 400, 0, 0, 0, DEFAULT_CHARSET, OUT_DEFAULT_PRECIS, CLIP_DEFAULT_PRECIS, ANTIALIASED_QUALITY, 0, w!("Segoe UI Symbol"));
            let mut adv = vec![];
            for (i, c) in CHARS.encode_utf16().enumerate() {
                let (col, row) = (i as u32 % COLS, i as u32 / COLS);
                let icon = char::from_u32(c as u32).map(|ch| ICONS.contains(ch)).unwrap_or(false);
                SelectObject(dc, if icon { symbols.into() } else { font.into() });
                let _ = TextOutW(dc, (col * CELL_W + 4) as i32, (row * CELL_H + 2) as i32, &[c]);
                let mut sz = windows::Win32::Foundation::SIZE::default();
                let _ = GetTextExtentPoint32W(dc, &[c], &mut sz);
                adv.push(sz.cx as f32 / CELL_H as f32);
            }
            let _ = advances.set(adv);
            let _ = GdiFlush();
            let src = std::slice::from_raw_parts(bits as *const u8, px.len());
            for i in 0..(W * H) as usize {
                let a = src[i * 4 + 1];
                px[i * 4..i * 4 + 4].copy_from_slice(&[255, 255, 255, a]);
            }
            SelectObject(dc, oldf);
            let _ = DeleteObject(symbols.into());
            SelectObject(dc, old);
            let _ = DeleteObject(font.into());
            let _ = DeleteObject(bmp.into());
        }
        let _ = DeleteDC(dc);
    }
    Tex { format: lolread::texture::DXGI_BGRA8, width: W, height: H, mips: vec![px] }
}

/// Width of `s` in reference pixels at glyph height `size`.
pub fn width(s: &str, size: f32) -> f32 {
    width_in(&ADVANCE, s, size)
}

fn width_in(adv: &std::sync::OnceLock<Vec<f32>>, s: &str, size: f32) -> f32 {
    s.chars().map(|ch| CHARS.chars().position(|c| c == ch).map(|i| advance_in(adv, i)).unwrap_or(0.41) * size).sum()
}

/// Text in the HUD reference frame; `size` is the glyph height in reference pixels. Centred on `cx`.
pub fn text(s: &str, cx: f32, y: f32, size: f32, color: [f32; 4]) -> Vec<Sprite> {
    text_in("font", &ADVANCE, s, cx, y, size, color)
}

/// Like `text`, in the title face.
pub fn title_width(s: &str, size: f32) -> f32 {
    width_in(&TITLE_ADVANCE, s, size)
}

pub fn title_text(s: &str, cx: f32, y: f32, size: f32, color: [f32; 4]) -> Vec<Sprite> {
    text_in("font_title", &TITLE_ADVANCE, s, cx, y, size, color)
}

fn text_in(tex: &'static str, adv: &std::sync::OnceLock<Vec<f32>>, s: &str, cx: f32, y: f32, size: f32, color: [f32; 4]) -> Vec<Sprite> {
    let w = size * CELL_W as f32 / CELL_H as f32;
    // glyphs are drawn 4 px into their cell
    let mut x = cx - width_in(adv, s, size) * 0.5 - size * 4.0 / CELL_H as f32;
    let mut out = vec![];
    for ch in s.chars() {
        let Some(i) = CHARS.chars().position(|c| c == ch) else { continue };
        if ch != ' ' {
            let (col, row) = (i as u32 % COLS, i as u32 / COLS);
            let u0 = (col * CELL_W) as f32 / W as f32;
            let u1 = ((col + 1) * CELL_W) as f32 / W as f32;
            let v0 = (row * CELL_H) as f32 / H as f32;
            let v1 = ((row + 1) * CELL_H) as f32 / H as f32;
            out.push(Sprite {
                tex,
                additive: false,
                pos: [[x, y, 0.], [x + w, y, 0.], [x + w, y + size, 0.], [x, y + size, 0.]],
                uv: [[u0, v0], [u1, v0], [u1, v1], [u0, v1]],
                color,
            });
        }
        x += advance_in(adv, i) * size;
    }
    out
}
