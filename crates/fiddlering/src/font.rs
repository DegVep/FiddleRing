//! Digit atlas for the HUD numbers, rasterised once with GDI (Arial Bold) into a BGRA texture.

use crate::scene::Sprite;
use lolread::cache::Tex;
use windows::Win32::Graphics::Gdi::*;
use windows::core::w;

const CHARS: &str = "0123456789./:+";
const CELL_W: u32 = 32;
const CELL_H: u32 = 48;
const W: u32 = 512;
const H: u32 = 64;

pub fn atlas() -> Tex {
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
            let font = CreateFontW(44, 0, 0, 0, 700, 0, 0, 0, DEFAULT_CHARSET, OUT_DEFAULT_PRECIS, CLIP_DEFAULT_PRECIS, ANTIALIASED_QUALITY, 0, w!("Arial"));
            let oldf = SelectObject(dc, font.into());
            SetBkMode(dc, TRANSPARENT);
            SetTextColor(dc, windows::Win32::Foundation::COLORREF(0x00ffffff));
            for (i, c) in CHARS.encode_utf16().enumerate() {
                let _ = TextOutW(dc, (i as u32 * CELL_W + 4) as i32, 2, &[c]);
            }
            let _ = GdiFlush();
            let src = std::slice::from_raw_parts(bits as *const u8, px.len());
            for i in 0..(W * H) as usize {
                let a = src[i * 4 + 1];
                px[i * 4..i * 4 + 4].copy_from_slice(&[255, 255, 255, a]);
            }
            SelectObject(dc, oldf);
            SelectObject(dc, old);
            let _ = DeleteObject(font.into());
            let _ = DeleteObject(bmp.into());
        }
        let _ = DeleteDC(dc);
    }
    Tex { format: lolread::texture::DXGI_BGRA8, width: W, height: H, mips: vec![px] }
}

/// Text in the HUD reference frame; `size` is the glyph height in reference pixels. Centred on `cx`.
pub fn text(s: &str, cx: f32, y: f32, size: f32, color: [f32; 4]) -> Vec<Sprite> {
    let w = size * CELL_W as f32 / CELL_H as f32;
    let adv = w * 0.62;
    let n = s.chars().count() as f32;
    let mut x = cx - adv * n * 0.5 - (w - adv) * 0.5;
    let mut out = vec![];
    for ch in s.chars() {
        if let Some(i) = CHARS.find(ch) {
            let u0 = (i as u32 * CELL_W) as f32 / W as f32;
            let u1 = ((i as u32 + 1) * CELL_W) as f32 / W as f32;
            let v1 = CELL_H as f32 / H as f32;
            out.push(Sprite {
                tex: "font",
                additive: false,
                pos: [[x, y, 0.], [x + w, y, 0.], [x + w, y + size, 0.], [x, y + size, 0.]],
                uv: [[u0, 0.], [u1, 0.], [u1, v1], [u0, v1]],
                color,
            });
        }
        x += adv;
    }
    out
}
