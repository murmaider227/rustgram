//! `approveqr`: signs a client in through its own login QR code. The client (the official
//! Telegram Desktop copy of the walk, on the test servers) shows a `tg://login?token=...` QR; the
//! runner saves a screenshot of it as raw 8-bit grey pixels; this decodes it and confirms the token
//! with an approver login of the same account (`auth.acceptLoginToken`). Prints no token.

use std::path::Path;

use crate::side::{Side, load_test_credentials};

/// Decodes every QR code in a raw 8-bit grey image and returns the first `tg://login` token.
pub fn login_token_from_grey(
    pixels: &[u8],
    width: usize,
    height: usize,
) -> Result<Vec<u8>, String> {
    if pixels.len() != width * height {
        return Err("grey image size does not match width x height".into());
    }
    let mut img =
        rqrr::PreparedImage::prepare_from_greyscale(width, height, |x, y| pixels[y * width + x]);
    let grids = img.detect_grids();
    let mut why = format!("{} QR grids found", grids.len());
    for grid in grids {
        match grid.decode() {
            Ok((_, content)) => {
                if let Some(t) = content.split("token=").nth(1) {
                    return crate::decode_b64url(t.split('&').next().unwrap_or(t))
                        .ok_or_else(|| "bad token in the QR code".to_owned());
                }
                why = "a QR code without a login token".into();
            }
            Err(e) => why = format!("QR decode error: {e:?}"),
        }
    }
    Err(format!("no tg://login QR code read ({why})"))
}

pub async fn approve(
    approver: &Path,
    grey: &Path,
    width: usize,
    height: usize,
) -> Result<(), String> {
    let pixels = std::fs::read(grey).map_err(|e| format!("read image: {e}"))?;
    let token = login_token_from_grey(&pixels, width, height)?;
    let creds = load_test_credentials()?;
    let side = Side::open("approver", approver, &creds).await?;
    side.call(
        "acceptLoginToken",
        &tg_tl::functions::auth::AcceptLoginToken { token },
    )
    .await?;
    println!("approved");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_a_plain_login_qr() {
        let code = qrcode::QrCode::new(b"tg://login?token=AQIDBA").expect("qr");
        let n = code.width();
        let scale = 8;
        let quiet = 4;
        let size = (n + 2 * quiet) * scale;
        let colors = code.to_colors();
        let mut px = vec![255u8; size * size];
        for y in 0..size {
            for x in 0..size {
                let (mx, my) = (x / scale, y / scale);
                if mx >= quiet
                    && my >= quiet
                    && mx < n + quiet
                    && my < n + quiet
                    && colors[(my - quiet) * n + (mx - quiet)] == qrcode::Color::Dark
                {
                    px[y * size + x] = 0;
                }
            }
        }
        assert_eq!(
            login_token_from_grey(&px, size, size).expect("token"),
            vec![1, 2, 3, 4]
        );
    }
}
