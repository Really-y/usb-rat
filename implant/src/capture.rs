//! Primary-monitor capture via GDI: BitBlt → GetDIBits → JPEG.
//! No external screenshot crates — fewer imports, smaller binary.
#![cfg(windows)]

use std::io::Cursor;

use windows::Win32::Foundation::HWND;
use windows::Win32::Graphics::Gdi::{
    BitBlt, CreateCompatibleBitmap, CreateCompatibleDC, DeleteDC, DeleteObject, GetDC, GetDIBits,
    ReleaseDC, SelectObject, BITMAPINFO, BITMAPINFOHEADER, DIB_RGB_COLORS, HGDIOBJ, SRCCOPY,
};
use windows::Win32::UI::WindowsAndMessaging::{
    GetSystemMetrics, SetProcessDPIAware, SM_CXSCREEN, SM_CYSCREEN,
};

/// Make the process DPI-aware so `GetSystemMetrics` and `GetDC` report real
/// pixel dimensions instead of DPI-virtualized ones (crisper capture, aligned
/// input mapping). Must run before the first screen DC is created.
pub fn init_dpi() {
    unsafe {
        let _ = SetProcessDPIAware();
    }
}

pub fn screen_size() -> (u32, u32) {
    unsafe {
        (
            GetSystemMetrics(SM_CXSCREEN).max(1) as u32,
            GetSystemMetrics(SM_CYSCREEN).max(1) as u32,
        )
    }
}

/// Capture, downscale by `scale`, JPEG-encode at `quality`.
/// Returns `(w, h, jpeg_bytes)` or `None` on any GDI failure.
pub fn grab(quality: u8, scale: f32) -> Option<(u32, u32, Vec<u8>)> {
    let (w, h) = screen_size();
    let bgra = unsafe { capture_bgra(w, h)? };

    // 32-bpp BGRA (top-down) -> packed RGB.
    let mut rgb = Vec::with_capacity((w * h * 3) as usize);
    for px in bgra.chunks_exact(4) {
        rgb.push(px[2]);
        rgb.push(px[1]);
        rgb.push(px[0]);
    }
    let img = image::RgbImage::from_raw(w, h, rgb)?;

    let scale = scale.clamp(0.3, 1.0);
    let (nw, nh) = (
        ((w as f32 * scale) as u32).max(1),
        ((h as f32 * scale) as u32).max(1),
    );
    let img = if nw == w && nh == h {
        img
    } else {
        image::imageops::resize(&img, nw, nh, image::imageops::FilterType::Triangle)
    };

    let mut buf = Cursor::new(Vec::new());
    let mut enc = image::codecs::jpeg::JpegEncoder::new_with_quality(&mut buf, quality);
    enc.encode(img.as_raw(), nw, nh, image::ExtendedColorType::Rgb8)
        .ok()?;
    Some((nw, nh, buf.into_inner()))
}

unsafe fn capture_bgra(w: u32, h: u32) -> Option<Vec<u8>> {
    let desktop = HWND(std::ptr::null_mut());
    let screen = GetDC(desktop);
    if screen.0.is_null() {
        return None;
    }
    let mem = CreateCompatibleDC(screen);
    if mem.0.is_null() {
        ReleaseDC(desktop, screen);
        return None;
    }
    let bmp = CreateCompatibleBitmap(screen, w as i32, h as i32);
    if bmp.0.is_null() {
        let _ = DeleteDC(mem);
        ReleaseDC(desktop, screen);
        return None;
    }
    let old = SelectObject(mem, HGDIOBJ(bmp.0));
    let _ = BitBlt(mem, 0, 0, w as i32, h as i32, screen, 0, 0, SRCCOPY);

    let mut bmi: BITMAPINFO = std::mem::zeroed();
    bmi.bmiHeader = BITMAPINFOHEADER {
        biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
        biWidth: w as i32,
        // Negative height => top-down rows. At 32 bpp the row pitch is w*4,
        // already DWORD-aligned, so a w*h*4 buffer is always exact.
        biHeight: -(h as i32),
        biPlanes: 1,
        biBitCount: 32,
        biCompression: 0, // BI_RGB == 0 (field is u32 in windows-rs 0.58)
        biSizeImage: 0,
        biXPelsPerMeter: 0,
        biYPelsPerMeter: 0,
        biClrUsed: 0,
        biClrImportant: 0,
    };

    let mut buf = vec![0u8; (w * h * 4) as usize];
    let rows = GetDIBits(
        mem,
        bmp,
        0,
        h,
        Some(buf.as_mut_ptr() as *mut core::ffi::c_void),
        &mut bmi,
        DIB_RGB_COLORS,
    );

    let _ = SelectObject(mem, old);
    let _ = DeleteObject(HGDIOBJ(bmp.0));
    let _ = DeleteDC(mem);
    ReleaseDC(desktop, screen);

    if rows == 0 {
        None
    } else {
        Some(buf)
    }
}
