//! A picture of a window.
//!
//! `PrintWindow` with `PW_RENDERFULLCONTENT` asks the window to render itself
//! into a device context this process owns. That is the route that works for a
//! window on a desktop that is not the active one: it is a message to the
//! window, not a read of the screen, so it does not depend on which desktop is
//! in front. A screen read would return whatever the user happens to be looking
//! at, which is the wrong window and a privacy problem at the same time.
//!
//! The limit is real and it is named rather than hidden: a window that draws
//! through an exclusive-fullscreen swap chain, or one whose content is protected,
//! has no redirection surface to render, and the capture comes back blank. The
//! caller gets an error saying so instead of a black rectangle that looks like a
//! successful capture.

use crate::error::AppError;

/// A PNG of the window's client area, base64, with its size in pixels.
///
/// `None` when the window has no client area to photograph — a zero-sized or
/// minimised window — because that is a state the caller can act on, not a
/// failure of the mechanism.
#[cfg(windows)]
pub fn window_png(
    hwnd: windows::Win32::Foundation::HWND,
) -> Result<Option<(String, u32, u32)>, AppError> {
    let (width, height) = client_size(hwnd)?;
    if width == 0 || height == 0 {
        return Ok(None);
    }

    let pixels = render(hwnd, width, height)?;
    let png = encode_png(&pixels, width, height)?;
    Ok(Some((crate::commands::base64(&png), width, height)))
}

/// The client area's size, which is the stage the guardrail works in.
#[cfg(windows)]
fn client_size(hwnd: windows::Win32::Foundation::HWND) -> Result<(u32, u32), AppError> {
    use windows::Win32::Foundation::RECT;
    use windows::Win32::UI::WindowsAndMessaging::GetClientRect;

    let mut rect = RECT::default();
    unsafe { GetClientRect(hwnd, &mut rect) }
        .map_err(|error| AppError::message(format!("could not measure the window: {error}")))?;
    Ok((
        (rect.right - rect.left).max(0) as u32,
        (rect.bottom - rect.top).max(0) as u32,
    ))
}

/// Render the window into a top-down 32-bit buffer, as RGBA.
///
/// A DIB section is used rather than a compatible bitmap because it hands back
/// the pixels directly: `GetDIBits` on a compatible bitmap would be a second
/// copy of the same data.
#[cfg(windows)]
fn render(
    hwnd: windows::Win32::Foundation::HWND,
    width: u32,
    height: u32,
) -> Result<Vec<u8>, AppError> {
    use windows::Win32::Graphics::Gdi::{
        CreateCompatibleDC, CreateDIBSection, DeleteDC, DeleteObject, SelectObject, BITMAPINFO,
        BITMAPINFOHEADER, BI_RGB, DIB_RGB_COLORS, HGDIOBJ,
    };
    use windows::Win32::Storage::Xps::{PrintWindow, PRINT_WINDOW_FLAGS};

    /// `PW_RENDERFULLCONTENT`: render the DWM redirection surface, not just what
    /// the window paints in response to `WM_PRINT`.
    const PW_RENDERFULLCONTENT: u32 = 0x0000_0002;

    let mut info = BITMAPINFO::default();
    info.bmiHeader = BITMAPINFOHEADER {
        biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
        biWidth: width as i32,
        // Negative height asks for a top-down buffer, so row 0 is the top of the
        // window and the encoder does not have to flip it.
        biHeight: -(height as i32),
        biPlanes: 1,
        biBitCount: 32,
        biCompression: BI_RGB.0,
        ..Default::default()
    };

    unsafe {
        let dc = CreateCompatibleDC(None);
        if dc.is_invalid() {
            return Err(AppError::message("could not create a device context"));
        }

        let mut bits: *mut std::ffi::c_void = std::ptr::null_mut();
        let dib = match CreateDIBSection(Some(dc), &info, DIB_RGB_COLORS, &mut bits, None, 0) {
            Ok(dib) => dib,
            Err(error) => {
                let _ = DeleteDC(dc);
                return Err(AppError::message(format!(
                    "could not allocate a {width}x{height} buffer: {error}"
                )));
            }
        };

        let previous = SelectObject(dc, HGDIOBJ(dib.0));
        let printed = PrintWindow(hwnd, dc, PRINT_WINDOW_FLAGS(PW_RENDERFULLCONTENT)).as_bool();
        SelectObject(dc, previous);

        // The pixels are copied out before the bitmap is freed: `bits` points
        // into the DIB's own storage, so reading it after `DeleteObject` is a
        // read of freed memory.
        let pixels = if printed {
            let count = (width as usize) * (height as usize) * 4;
            Some(std::slice::from_raw_parts(bits as *const u8, count).to_vec())
        } else {
            None
        };

        let _ = DeleteObject(HGDIOBJ(dib.0));
        let _ = DeleteDC(dc);

        let Some(bgra) = pixels else {
            return Err(AppError::message(
                "the window refused to render itself — it may be minimised, or drawing through a \
                 swap chain that has no redirection surface",
            ));
        };

        Ok(bgra_to_rgba(&bgra))
    }
}

/// The pixels behind a capture, for a test that has to check a colour.
///
/// Decoding the PNG would need a decoder this crate does not carry, so the test
/// reads the same buffer the encoder was handed. It proves the window's own
/// pixels came back; it does not prove the compressor is lossless, which is not
/// a claim anyone is making.
#[cfg(all(test, windows))]
pub fn render_for_test(
    hwnd: windows::Win32::Foundation::HWND,
    width: u32,
    height: u32,
) -> Result<Vec<u8>, AppError> {
    render(hwnd, width, height)
}

/// `PrintWindow` hands back BGRA with an unused fourth byte; PNG wants RGBA.
fn bgra_to_rgba(bgra: &[u8]) -> Vec<u8> {
    let mut rgba = Vec::with_capacity(bgra.len());
    for pixel in bgra.chunks_exact(4) {
        rgba.extend_from_slice(&[pixel[2], pixel[1], pixel[0], 255]);
    }
    rgba
}

/// Encode RGBA to PNG.
fn encode_png(rgba: &[u8], width: u32, height: u32) -> Result<Vec<u8>, AppError> {
    let mut out = Vec::new();
    {
        let mut encoder = png::Encoder::new(&mut out, width, height);
        encoder.set_color(png::ColorType::Rgba);
        encoder.set_depth(png::BitDepth::Eight);
        let mut writer = encoder
            .write_header()
            .map_err(|error| AppError::message(format!("could not start the picture: {error}")))?;
        writer
            .write_image_data(rgba)
            .map_err(|error| AppError::message(format!("could not write the picture: {error}")))?;
    }
    Ok(out)
}

#[cfg(all(test, windows))]
mod tests {
    use super::*;

    #[test]
    fn a_png_round_trips_through_the_encoder() {
        let rgba = vec![0x11u8, 0x22, 0x33, 0xFF].repeat(4 * 3);
        let png = encode_png(&rgba, 4, 3).unwrap();

        assert_eq!(&png[..8], b"\x89PNG\r\n\x1a\n", "a PNG has a fixed signature");
        // IHDR is the first chunk and carries the size, so a wrong size is
        // caught without decoding the pixels.
        assert_eq!(&png[12..16], b"IHDR");
        assert_eq!(u32::from_be_bytes(png[16..20].try_into().unwrap()), 4);
        assert_eq!(u32::from_be_bytes(png[20..24].try_into().unwrap()), 3);
    }

    #[test]
    fn bgra_is_reordered_to_rgba_and_made_opaque() {
        // PrintWindow leaves the fourth byte at whatever the window had, which
        // is not alpha. A capture that carried it through would be transparent
        // wherever that byte happened to be zero.
        let bgra = [0x10, 0x20, 0x30, 0x00, 0x40, 0x50, 0x60, 0x00];
        assert_eq!(
            bgra_to_rgba(&bgra),
            vec![0x30, 0x20, 0x10, 0xFF, 0x60, 0x50, 0x40, 0xFF]
        );
    }
}
