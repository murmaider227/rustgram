//! Build script for `tg-app`: embeds the Windows application icon (`RT_GROUP_ICON` / `RT_ICON`)
//! and `VERSIONINFO` metadata into `rustgram.exe`.
//!
//! Port origin: `Telegram/Resources/winrc/Telegram.rc` and `crates/tg-xtask/src/icon.rs`.
//!
//! - Uses `RUSTGRAM_ICON_PATH` (or legacy `TGFORK_ICON_PATH`) if set and present; otherwise renders
//!   workspace `assets/icon.svg` to a multi-size `.ico` in `OUT_DIR` via `resvg` + `ico`.
//! - Uses `RUSTGRAM_VERSION` (or legacy `TGFORK_VERSION`) if set, otherwise `CARGO_PKG_VERSION`.
//! - Attempts `winresource` (`rc.exe` / `windres`) first; when `rc.exe` is unavailable (such as
//!   `xwin` + MSVC environments without the full Windows SDK), emits a compliant Win32 `.res`
//!   binary directly and compiles it to COFF via MSVC `cvtres.exe`.

fn main() {
    println!("cargo:rerun-if-env-changed=RUSTGRAM_ICON_PATH");
    println!("cargo:rerun-if-env-changed=TGFORK_ICON_PATH");
    println!("cargo:rerun-if-env-changed=RUSTGRAM_VERSION");
    println!("cargo:rerun-if-env-changed=TGFORK_VERSION");
    println!("cargo:rerun-if-changed=../../assets/icon.svg");
    println!("cargo::rustc-check-cfg=cfg(rustgram_window_icon_rgba)");

    // Version printed by `rustgram --version` (same source as the VERSIONINFO resource).
    let version = ["RUSTGRAM_VERSION", "TGFORK_VERSION"]
        .iter()
        .filter_map(|name| std::env::var(name).ok())
        .map(|v| v.trim().to_owned())
        .find(|v| !v.is_empty())
        .unwrap_or_else(|| std::env::var("CARGO_PKG_VERSION").unwrap_or_default());
    println!("cargo:rustc-env=RUSTGRAM_BUILD_VERSION={version}");

    // 16 MB main-thread stack for rustgram.exe: the dev profile builds tg-app unoptimized, and the default 1 MB
    // Windows stack is too small for deep egui layouts without inlining.
    if std::env::var("CARGO_CFG_TARGET_ENV").as_deref() == Ok("msvc") {
        println!("cargo:rustc-link-arg-bins=/STACK:16777216");
    }

    #[cfg(windows)]
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        win_resources::embed_windows_resources();
    }
}

#[cfg(windows)]
mod win_resources {
    use std::env;
    use std::fs::{self, File};
    use std::io::Write;
    use std::path::{Path, PathBuf};
    use std::process::Command;

    use resvg::{tiny_skia, usvg};

    /// Standard multi-size Windows icon entries matching `crates/tg-xtask/src/icon.rs`.
    const ICON_SIZES: &[u32] = &[16, 24, 32, 48, 64, 128, 256];

    const PRODUCT_NAME: &str = "Rustgram";
    const FILE_DESCRIPTION: &str = "Rustgram Desktop";
    const ORIGINAL_FILENAME: &str = "rustgram.exe";
    const INTERNAL_NAME: &str = "rustgram";
    const COMPANY_NAME: &str = "Rustgram Authors";
    const LEGAL_COPYRIGHT: &str = "Copyright (c) 2026 Rustgram Authors (GPL-3.0)";

    pub fn embed_windows_resources() {
        let out_dir = PathBuf::from(env::var("OUT_DIR").expect("OUT_DIR not set"));
        let icon_path = resolve_or_render_icon(&out_dir);
        write_window_icon_rgba(&out_dir);
        let version_str = env::var("RUSTGRAM_VERSION")
            .ok()
            .filter(|v| !v.trim().is_empty())
            .or_else(|| {
                env::var("TGFORK_VERSION")
                    .ok()
                    .filter(|v| !v.trim().is_empty())
            })
            .unwrap_or_else(|| env::var("CARGO_PKG_VERSION").unwrap_or_else(|_| "0.1.0".into()));
        let version_quad = parse_version_quad(&version_str);

        if try_winresource(&icon_path, &version_str, version_quad).is_ok() {
            return;
        }

        // Fallback when `rc.exe` is not installed (e.g. xwin + MSVC toolchain):
        // emit a standard Win32 binary `.res` file and convert it with MSVC `cvtres.exe`.
        let res_path = out_dir.join("rustgram_res.res");
        let obj_path = out_dir.join("rustgram_res.obj");
        let res_bytes = build_win32_res_file(&icon_path, &version_str, version_quad)
            .expect("failed to build Win32 .res resource file");
        fs::write(&res_path, &res_bytes).expect("failed to write rustgram_res.res");

        let cvtres = find_cvtres().expect(
            "neither rc.exe (for winresource) nor cvtres.exe (from MSVC) was found to embed Windows resources",
        );
        let machine = match env::var("CARGO_CFG_TARGET_ARCH").as_deref() {
            Ok("x86") => "X86",
            Ok("aarch64") => "ARM64",
            _ => "X64",
        };
        let status = Command::new(&cvtres)
            .arg("/NOLOGO")
            .arg(format!("/MACHINE:{machine}"))
            .arg(format!("/OUT:{}", obj_path.display()))
            .arg(&res_path)
            .status()
            .expect("failed to execute cvtres.exe");
        assert!(
            status.success(),
            "cvtres.exe failed with status {status} while compiling {}",
            res_path.display()
        );

        println!("cargo:rustc-link-arg-bin=rustgram={}", obj_path.display());
    }

    fn resolve_or_render_icon(out_dir: &Path) -> PathBuf {
        for var in ["RUSTGRAM_ICON_PATH", "TGFORK_ICON_PATH"] {
            if let Ok(explicit) = env::var(var) {
                let trimmed = explicit.trim();
                if !trimmed.is_empty() {
                    let path = PathBuf::from(trimmed);
                    if path.is_file() {
                        println!("cargo:rerun-if-changed={}", path.display());
                        return path;
                    }
                }
            }
        }

        let manifest_dir =
            PathBuf::from(env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR"));
        let svg_path = manifest_dir
            .join("..")
            .join("..")
            .join("assets")
            .join("icon.svg");
        let ico_path = out_dir.join("rustgram.ico");
        render_svg_to_ico(&svg_path, &ico_path)
            .unwrap_or_else(|e| panic!("failed to render {}: {e}", svg_path.display()));
        ico_path
    }

    /// Window icon size used by `tg_app::startup::icon::DEFAULT_WINDOW_ICON_SIZE`.
    const WINDOW_ICON_SIZE: u32 = 64;

    /// Pre-renders the 64 px straight-alpha RGBA window icon into `OUT_DIR`, so the app does not
    /// load the system font database and rasterize the SVG at every start (~20 ms).
    fn write_window_icon_rgba(out_dir: &Path) {
        let manifest_dir =
            PathBuf::from(env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR"));
        let svg_path = manifest_dir
            .join("..")
            .join("..")
            .join("assets")
            .join("icon.svg");
        let rendered = fs::read(&svg_path)
            .map_err(|e| format!("read svg: {e}"))
            .and_then(|svg| parse_icon_tree(&svg))
            .and_then(|tree| render_tree_rgba(&tree, WINDOW_ICON_SIZE));
        if let Ok(rgba) = rendered
            && fs::write(out_dir.join("window_icon_64.rgba"), rgba).is_ok()
        {
            println!("cargo:rustc-cfg=rustgram_window_icon_rgba");
        }
    }

    fn render_svg_to_ico(svg_path: &Path, out_path: &Path) -> Result<(), String> {
        let svg_data = fs::read(svg_path).map_err(|e| format!("read svg: {e}"))?;
        let tree = parse_icon_tree(&svg_data)?;
        let mut icon_dir = ico::IconDir::new(ico::ResourceType::Icon);

        for &size in ICON_SIZES {
            let rgba = render_tree_rgba(&tree, size)?;
            let image = ico::IconImage::from_rgba_data(size, size, rgba);
            let entry = if size >= 256 {
                ico::IconDirEntry::encode_as_png(&image)
            } else {
                ico::IconDirEntry::encode(&image)
            }
            .map_err(|e| e.to_string())?;
            icon_dir.add_entry(entry);
        }

        let mut out = Vec::new();
        icon_dir.write(&mut out).map_err(|e| e.to_string())?;
        let mut file = File::create(out_path).map_err(|e| e.to_string())?;
        file.write_all(&out).map_err(|e| e.to_string())?;
        Ok(())
    }

    fn parse_icon_tree(svg_data: &[u8]) -> Result<usvg::Tree, String> {
        let mut options = usvg::Options::default();
        {
            let db = options.fontdb_mut();
            db.load_system_fonts();
            for family in [
                "Segoe UI",
                "Arial",
                "Liberation Sans",
                "DejaVu Sans",
                "Noto Sans",
            ] {
                if db.faces().any(|f| {
                    f.families
                        .iter()
                        .any(|(name, _)| name.eq_ignore_ascii_case(family))
                }) {
                    db.set_sans_serif_family(family);
                    break;
                }
            }
        }

        usvg::Tree::from_data(svg_data, &options).map_err(|e| e.to_string())
    }

    fn render_tree_rgba(tree: &usvg::Tree, size: u32) -> Result<Vec<u8>, String> {
        let mut pixmap = tiny_skia::Pixmap::new(size, size)
            .ok_or_else(|| format!("failed to allocate {size}x{size} pixmap"))?;
        let tree_size = tree.size();
        let sx = size as f32 / tree_size.width();
        let sy = size as f32 / tree_size.height();
        let transform = tiny_skia::Transform::from_scale(sx, sy);
        resvg::render(tree, transform, &mut pixmap.as_mut());

        let mut straight = Vec::with_capacity((size * size * 4) as usize);
        for px in pixmap.pixels() {
            let c = px.demultiply();
            straight.push(c.red());
            straight.push(c.green());
            straight.push(c.blue());
            straight.push(c.alpha());
        }
        Ok(straight)
    }

    fn parse_version_quad(version: &str) -> (u16, u16, u16, u16) {
        let core = version
            .trim()
            .trim_start_matches(['v', 'V'])
            .split(['-', '+'])
            .next()
            .unwrap_or("0.1.0");
        let mut parts = core.split('.').map(|s| s.parse::<u16>().unwrap_or(0));
        let major = parts.next().unwrap_or(0);
        let minor = parts.next().unwrap_or(1);
        let patch = parts.next().unwrap_or(0);
        let build = parts.next().unwrap_or(0);
        (major, minor, patch, build)
    }

    fn try_winresource(
        icon_path: &Path,
        version_str: &str,
        quad: (u16, u16, u16, u16),
    ) -> std::io::Result<()> {
        let packed = (u64::from(quad.0) << 48)
            | (u64::from(quad.1) << 32)
            | (u64::from(quad.2) << 16)
            | u64::from(quad.3);

        let mut res = winresource::WindowsResource::new();
        res.set_icon(&icon_path.to_string_lossy());
        res.set("ProductName", PRODUCT_NAME);
        res.set("FileDescription", FILE_DESCRIPTION);
        res.set("OriginalFilename", ORIGINAL_FILENAME);
        res.set("InternalName", INTERNAL_NAME);
        res.set("CompanyName", COMPANY_NAME);
        res.set("LegalCopyright", LEGAL_COPYRIGHT);
        res.set("FileVersion", version_str);
        res.set("ProductVersion", version_str);
        res.set_version_info(winresource::VersionInfo::FILEVERSION, packed);
        res.set_version_info(winresource::VersionInfo::PRODUCTVERSION, packed);
        res.set_version_info(winresource::VersionInfo::FILEOS, 0x0004_0004);
        res.set_version_info(winresource::VersionInfo::FILETYPE, 0x0001);
        res.compile()
    }

    fn find_cvtres() -> Option<PathBuf> {
        if let Some(p) = env::var_os("CVTRES") {
            let path = PathBuf::from(p);
            if path.is_file() {
                return Some(path);
            }
        }
        // Check sibling of configured MSVC tools (CC / AR / LINKER from cargo config.toml).
        for var in [
            "CC_x86_64_pc_windows_msvc",
            "AR_x86_64_pc_windows_msvc",
            "CARGO_TARGET_X86_64_PC_WINDOWS_MSVC_LINKER",
        ] {
            if let Some(val) = env::var_os(var)
                && let Some(dir) = Path::new(&val).parent()
            {
                let candidate = dir.join("cvtres.exe");
                if candidate.is_file() {
                    return Some(candidate);
                }
            }
        }
        if let Some(path_var) = env::var_os("PATH") {
            for dir in env::split_paths(&path_var) {
                let candidate = dir.join("cvtres.exe");
                if candidate.is_file() {
                    return Some(candidate);
                }
            }
        }
        // Scan standard Visual Studio MSVC toolsets.
        for base in [
            r"C:\Program Files\Microsoft Visual Studio",
            r"C:\Program Files (x86)\Microsoft Visual Studio",
        ] {
            if let Some(found) = find_cvtres_under_vs(Path::new(base)) {
                return Some(found);
            }
        }
        None
    }

    fn find_cvtres_under_vs(root: &Path) -> Option<PathBuf> {
        let years = fs::read_dir(root).ok()?;
        for year in years.flatten() {
            let Ok(editions) = fs::read_dir(year.path()) else {
                continue;
            };
            for edition in editions.flatten() {
                let msvc_dir = edition.path().join("VC").join("Tools").join("MSVC");
                let Ok(versions) = fs::read_dir(&msvc_dir) else {
                    continue;
                };
                for ver in versions.flatten() {
                    let candidate = ver
                        .path()
                        .join("bin")
                        .join("Hostx64")
                        .join("x64")
                        .join("cvtres.exe");
                    if candidate.is_file() {
                        return Some(candidate);
                    }
                }
            }
        }
        None
    }

    // --- Pure-Rust Win32 `.res` binary writer (RT_ICON, RT_GROUP_ICON, RT_VERSION) ---

    const RT_ICON: u16 = 3;
    const RT_GROUP_ICON: u16 = 14;
    const RT_VERSION: u16 = 16;
    const LANG_EN_US: u16 = 0x0409;

    fn build_win32_res_file(
        icon_path: &Path,
        version_str: &str,
        quad: (u16, u16, u16, u16),
    ) -> Result<Vec<u8>, String> {
        let ico_bytes = fs::read(icon_path).map_err(|e| format!("read ico: {e}"))?;
        let (icon_entries, grp_icon_data) = parse_ico_for_resources(&ico_bytes)?;
        let version_data = build_vs_version_info(version_str, quad);

        let mut res = Vec::with_capacity(ico_bytes.len() + version_data.len() + 512);
        // Standard 32-byte Win32 `.res` empty marker record.
        write_res_record(&mut res, 0, 0, 0, 0, &[]);

        // Emit RT_ICON entries (IDs 1..=N).
        for (idx, data) in icon_entries.iter().enumerate() {
            let id = (idx + 1) as u16;
            write_res_record(&mut res, RT_ICON, id, 0x1010, LANG_EN_US, data);
        }

        // Emit RT_GROUP_ICON (ID 1) referencing those RT_ICON IDs.
        write_res_record(
            &mut res,
            RT_GROUP_ICON,
            1,
            0x1030,
            LANG_EN_US,
            &grp_icon_data,
        );

        // Emit RT_VERSION (ID 1).
        write_res_record(&mut res, RT_VERSION, 1, 0x0030, LANG_EN_US, &version_data);

        Ok(res)
    }

    fn write_res_record(
        out: &mut Vec<u8>,
        res_type: u16,
        res_name: u16,
        memory_flags: u16,
        language_id: u16,
        data: &[u8],
    ) {
        let header_size: u32 = 32;
        out.extend_from_slice(&(data.len() as u32).to_le_bytes());
        out.extend_from_slice(&header_size.to_le_bytes());
        // Type ordinal: 0xFFFF followed by type u16.
        out.extend_from_slice(&0xFFFFu16.to_le_bytes());
        out.extend_from_slice(&res_type.to_le_bytes());
        // Name ordinal: 0xFFFF followed by name u16.
        out.extend_from_slice(&0xFFFFu16.to_le_bytes());
        out.extend_from_slice(&res_name.to_le_bytes());
        // DataVersion (4), MemoryFlags (2), LanguageId (2), Version (4), Characteristics (4).
        out.extend_from_slice(&0u32.to_le_bytes());
        out.extend_from_slice(&memory_flags.to_le_bytes());
        out.extend_from_slice(&language_id.to_le_bytes());
        out.extend_from_slice(&0u32.to_le_bytes());
        out.extend_from_slice(&0u32.to_le_bytes());
        out.extend_from_slice(data);
        pad_dword(out);
    }

    fn parse_ico_for_resources(ico: &[u8]) -> Result<(Vec<Vec<u8>>, Vec<u8>), String> {
        if ico.len() < 6 {
            return Err("ico file too short".into());
        }
        let reserved = u16::from_le_bytes([ico[0], ico[1]]);
        let ico_type = u16::from_le_bytes([ico[2], ico[3]]);
        let count = u16::from_le_bytes([ico[4], ico[5]]) as usize;
        if reserved != 0 || ico_type != 1 || count == 0 {
            return Err("invalid ICO header".into());
        }
        let dir_end = 6 + count * 16;
        if ico.len() < dir_end {
            return Err("truncated ICO directory".into());
        }

        // Build GRPICONDIR (6 bytes) + count * GRPICONDIRENTRY (14 bytes each).
        let mut grp = Vec::with_capacity(6 + count * 14);
        grp.extend_from_slice(&0u16.to_le_bytes());
        grp.extend_from_slice(&1u16.to_le_bytes());
        grp.extend_from_slice(&(count as u16).to_le_bytes());

        let mut images = Vec::with_capacity(count);
        for i in 0..count {
            let base = 6 + i * 16;
            let width = ico[base];
            let height = ico[base + 1];
            let color_count = ico[base + 2];
            let entry_reserved = ico[base + 3];
            let planes = u16::from_le_bytes([ico[base + 4], ico[base + 5]]);
            let bit_count = u16::from_le_bytes([ico[base + 6], ico[base + 7]]);
            let bytes_in_res =
                u32::from_le_bytes([ico[base + 8], ico[base + 9], ico[base + 10], ico[base + 11]]);
            let image_offset = u32::from_le_bytes([
                ico[base + 12],
                ico[base + 13],
                ico[base + 14],
                ico[base + 15],
            ]) as usize;

            let end = image_offset
                .checked_add(bytes_in_res as usize)
                .ok_or_else(|| "ICO entry offset overflow".to_string())?;
            if end > ico.len() {
                return Err("ICO entry extends past EOF".into());
            }
            images.push(ico[image_offset..end].to_vec());

            // GRPICONDIRENTRY: 12 bytes from ICONDIRENTRY + 2-byte nId (1-based).
            grp.push(width);
            grp.push(height);
            grp.push(color_count);
            grp.push(entry_reserved);
            grp.extend_from_slice(&planes.to_le_bytes());
            grp.extend_from_slice(&bit_count.to_le_bytes());
            grp.extend_from_slice(&bytes_in_res.to_le_bytes());
            grp.extend_from_slice(&((i + 1) as u16).to_le_bytes());
        }

        Ok((images, grp))
    }

    fn build_vs_version_info(version_str: &str, quad: (u16, u16, u16, u16)) -> Vec<u8> {
        // VS_FIXEDFILEINFO (52 bytes).
        let file_ver_ms = (u32::from(quad.0) << 16) | u32::from(quad.1);
        let file_ver_ls = (u32::from(quad.2) << 16) | u32::from(quad.3);
        let mut fixed = Vec::with_capacity(52);
        fixed.extend_from_slice(&0xFEEF_04BDu32.to_le_bytes()); // dwSignature
        fixed.extend_from_slice(&0x0001_0000u32.to_le_bytes()); // dwStrucVersion
        fixed.extend_from_slice(&file_ver_ms.to_le_bytes()); // dwFileVersionMS
        fixed.extend_from_slice(&file_ver_ls.to_le_bytes()); // dwFileVersionLS
        fixed.extend_from_slice(&file_ver_ms.to_le_bytes()); // dwProductVersionMS
        fixed.extend_from_slice(&file_ver_ls.to_le_bytes()); // dwProductVersionLS
        fixed.extend_from_slice(&0x0000_003Fu32.to_le_bytes()); // dwFileFlagsMask (VS_FFI_FILEFLAGSMASK)
        fixed.extend_from_slice(&0u32.to_le_bytes()); // dwFileFlags
        fixed.extend_from_slice(&0x0004_0004u32.to_le_bytes()); // dwFileOS (VOS_NT_WINDOWS32)
        fixed.extend_from_slice(&0x0000_0001u32.to_le_bytes()); // dwFileType (VFT_APP)
        fixed.extend_from_slice(&0u32.to_le_bytes()); // dwFileSubtype
        fixed.extend_from_slice(&0u32.to_le_bytes()); // dwFileDateMS
        fixed.extend_from_slice(&0u32.to_le_bytes()); // dwFileDateLS

        // StringTable ("040904b0" = US English, Unicode).
        let entries = [
            ("CompanyName", COMPANY_NAME),
            ("FileDescription", FILE_DESCRIPTION),
            ("FileVersion", version_str),
            ("InternalName", INTERNAL_NAME),
            ("LegalCopyright", LEGAL_COPYRIGHT),
            ("OriginalFilename", ORIGINAL_FILENAME),
            ("ProductName", PRODUCT_NAME),
            ("ProductVersion", version_str),
        ];
        let mut string_children = Vec::new();
        for (k, v) in entries {
            pad_dword(&mut string_children);
            string_children.extend_from_slice(&encode_version_string(k, v));
        }
        let string_table = encode_version_node("040904b0", 1, &[], &string_children);
        let string_file_info = encode_version_node("StringFileInfo", 1, &[], &string_table);

        // VarFileInfo -> Var ("Translation" = 0x0409, 0x04B0).
        let mut translation_val = Vec::with_capacity(4);
        translation_val.extend_from_slice(&LANG_EN_US.to_le_bytes());
        translation_val.extend_from_slice(&0x04B0u16.to_le_bytes());
        let var_node = encode_version_node("Translation", 0, &translation_val, &[]);
        let var_file_info = encode_version_node("VarFileInfo", 1, &[], &var_node);

        let mut root_children = Vec::new();
        root_children.extend_from_slice(&string_file_info);
        pad_dword(&mut root_children);
        root_children.extend_from_slice(&var_file_info);

        encode_version_node("VS_VERSION_INFO", 0, &fixed, &root_children)
    }

    fn encode_version_string(key: &str, value: &str) -> Vec<u8> {
        let val_w = to_utf16_nul(value);
        let val_len_words = (val_w.len() / 2) as u16;
        let mut buf = Vec::new();
        buf.extend_from_slice(&0u16.to_le_bytes()); // wLength placeholder
        buf.extend_from_slice(&val_len_words.to_le_bytes()); // wValueLength (in WORDs for wType=1)
        buf.extend_from_slice(&1u16.to_le_bytes()); // wType = 1 (text)
        buf.extend_from_slice(&to_utf16_nul(key));
        pad_dword(&mut buf);
        buf.extend_from_slice(&val_w);
        let total_len = buf.len() as u16;
        buf[0..2].copy_from_slice(&total_len.to_le_bytes());
        buf
    }

    fn encode_version_node(key: &str, w_type: u16, value: &[u8], children: &[u8]) -> Vec<u8> {
        let mut buf = Vec::new();
        buf.extend_from_slice(&0u16.to_le_bytes()); // wLength placeholder
        let val_len = if w_type == 1 {
            (value.len() / 2) as u16
        } else {
            value.len() as u16
        };
        buf.extend_from_slice(&val_len.to_le_bytes());
        buf.extend_from_slice(&w_type.to_le_bytes());
        buf.extend_from_slice(&to_utf16_nul(key));
        pad_dword(&mut buf);
        if !value.is_empty() {
            buf.extend_from_slice(value);
            if !children.is_empty() {
                pad_dword(&mut buf);
            }
        }
        buf.extend_from_slice(children);
        let total_len = buf.len() as u16;
        buf[0..2].copy_from_slice(&total_len.to_le_bytes());
        buf
    }

    fn to_utf16_nul(s: &str) -> Vec<u8> {
        let mut out = Vec::with_capacity((s.len() + 1) * 2);
        for u in s.encode_utf16().chain(std::iter::once(0)) {
            out.extend_from_slice(&u.to_le_bytes());
        }
        out
    }

    fn pad_dword(buf: &mut Vec<u8>) {
        while !buf.len().is_multiple_of(4) {
            buf.push(0);
        }
    }
}
