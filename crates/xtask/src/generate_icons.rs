use anyhow::{Context, Result, bail};
use std::path::Path;

use crate::cmd::{self, ensure_dir, which, which_any};

pub fn run(root: &Path) -> Result<()> {
    let icons_dir = root.join("assets/icons");
    let svg = icons_dir.join("logo.svg");
    if !svg.is_file() {
        bail!("{} not found", svg.display());
    }

    if !which("rsvg-convert") {
        bail!("rsvg-convert not found — install librsvg (e.g. `brew install librsvg`)");
    }

    eprintln!("Generating icons from {}...", svg.display());

    // Master 1024×1024 PNG that the .ico packer slices down from.
    rsvg_convert(&svg, 1024, &icons_dir.join("icon-1024.png"))?;
    // Generic 512×512 used by the Linux .desktop file.
    rsvg_convert(&svg, 512, &icons_dir.join("icon.png"))?;

    // Windows multi-size .ico via ImageMagick. The auto-resize sizes
    // mirror what cargo-bundle's WiX template expects.
    let convert = pick_imagemagick().context(
        "ImageMagick not found — install `imagemagick` (provides `magick` or `convert`)",
    )?;
    let icon_1024 = icons_dir.join("icon-1024.png");
    let icon_ico = icons_dir.join("icon.ico");
    let mut convert_args: Vec<&str> = Vec::new();
    if convert == "magick" {
        // ImageMagick 7 wraps `convert` under a `magick` subcommand.
        convert_args.push("convert");
    }
    let icon_1024_str = icon_1024.to_string_lossy().into_owned();
    let icon_ico_str = icon_ico.to_string_lossy().into_owned();
    convert_args.extend([
        icon_1024_str.as_str(),
        "-define",
        "icon:auto-resize=256,128,64,48,32,16",
        icon_ico_str.as_str(),
    ]);
    cmd::run(convert, convert_args)?;
    eprintln!("  Created icon.ico");

    // macOS .icns. We try iconutil first (only on macOS), fall back to
    // png2icns on Linux/Windows builders. If neither is around we skip
    // — the generated .icns just doesn't get refreshed for that build.
    if which_any("iconutil") {
        build_icns_via_iconutil(&svg, &icons_dir)?;
    } else if which("png2icns") {
        build_icns_via_png2icns(&icons_dir)?;
    } else {
        eprintln!("  Skipped icon.icns (iconutil/png2icns not available)");
    }

    eprintln!("Done. Icons are in {}/", icons_dir.display());
    Ok(())
}

fn rsvg_convert(svg: &Path, size: u32, out: &Path) -> Result<()> {
    let size = size.to_string();
    let out_str = out.to_string_lossy().into_owned();
    let svg_str = svg.to_string_lossy().into_owned();
    cmd::run(
        "rsvg-convert",
        [
            "-w",
            size.as_str(),
            "-h",
            size.as_str(),
            svg_str.as_str(),
            "-o",
            out_str.as_str(),
        ],
    )
}

fn pick_imagemagick() -> Option<&'static str> {
    if which("magick") {
        Some("magick")
    } else if which("convert") {
        Some("convert")
    } else {
        None
    }
}

fn build_icns_via_iconutil(svg: &Path, icons_dir: &Path) -> Result<()> {
    let iconset = icons_dir.join("icon.iconset");
    ensure_dir(&iconset)?;
    for &size in &[16u32, 32, 64, 128, 256, 512] {
        let one_x = iconset.join(format!("icon_{size}x{size}.png"));
        let two_x = iconset.join(format!("icon_{size}x{size}@2x.png"));
        rsvg_convert(svg, size, &one_x)?;
        rsvg_convert(svg, size * 2, &two_x)?;
    }
    let icns = icons_dir.join("icon.icns");
    let iconset_str = iconset.to_string_lossy().into_owned();
    let icns_str = icns.to_string_lossy().into_owned();
    cmd::run(
        "iconutil",
        [
            "-c",
            "icns",
            iconset_str.as_str(),
            "-o",
            icns_str.as_str(),
        ],
    )?;
    std::fs::remove_dir_all(&iconset).ok();
    eprintln!("  Created icon.icns");
    Ok(())
}

fn build_icns_via_png2icns(icons_dir: &Path) -> Result<()> {
    let icns = icons_dir.join("icon.icns");
    let png = icons_dir.join("icon-1024.png");
    let icns_str = icns.to_string_lossy().into_owned();
    let png_str = png.to_string_lossy().into_owned();
    cmd::run("png2icns", [icns_str.as_str(), png_str.as_str()])?;
    eprintln!("  Created icon.icns (via png2icns)");
    Ok(())
}
