//! Pure Hunspell dictionary manifest, zip unpacker, engine, and cloud-source trait.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::io::{Cursor, Write};
use std::path::{Path, PathBuf};

use tg_storage::{UnpackOptions, UnpackReport, unpack_blob_bytes};

use crate::settings::backend::format_bytes;

/// Official Telegram cloud channel hosting Hunspell dictionary archives (`spellchecker_common.cpp:96`).
pub const DICTIONARIES_MANIFEST_CHANNEL: &str = "tdhbcfiles";

/// Pinned manifest post ID inside [`DICTIONARIES_MANIFEST_CHANNEL`] (`spellchecker_common.cpp:97`).
pub const DICTIONARIES_MANIFEST_POST_ID: i32 = 2949;

/// On-disk dictionary archive format revision (`kDictionariesRev` in `spellchecker_common.cpp:98`).
pub const DICTIONARIES_REV: i32 = 2;

/// Permitted file extensions inside a Hunspell dictionary archive (`kDictExtensions`).
pub const DICT_EXTENSIONS: [&str; 2] = ["dic", "aff"];

/// Maximum search query length in `ManageDictionariesBox` (`kMaxQueryLength = 15`).
pub const MAX_DICTIONARY_QUERY_LEN: usize = 15;

/// Qt6 `QLocale::Language` ID for Serbian (used for the `sr_Cyrl_RS` -> `sr_RS` rename quirk).
pub const LANG_ID_SERBIAN: i32 = 100;

/// Metadata for a single system or cloud Hunspell dictionary (`Spellchecker::Dict`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DictionaryEntry {
    /// Stable locale language ID (matching Qt `QLocale::Language` / LWC `(lang * 1000) + country`).
    pub id: i32,
    /// Normalized locale stem used for folder & `.dic`/`.aff` filenames (e.g. `"en_US"`, `"de_DE"`).
    pub locale_code: String,
    /// Human-readable dictionary name (e.g. `"English (US)"`, `"Deutsch"`).
    pub name: String,
    /// Compressed archive size in bytes advertised by the manifest.
    pub size_bytes: u64,
    /// Telegram channel username hosting the dictionary archive (e.g. `"tdhbcfiles"`).
    pub channel: String,
    /// Channel post ID carrying the zipped Hunspell `.dic`/`.aff` document.
    pub post_id: i32,
    /// Whether this dictionary is supplied by the host OS (`ISpellChecker`) rather than downloaded.
    pub is_system: bool,
}

/// Download / activation state for one row in `ManageDictionariesBox` (`Storage::CloudBlob::BlobState`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DictionaryState {
    /// Available in the cloud manifest but not yet downloaded to disk.
    Available {
        /// Archive download size in bytes.
        size_bytes: u64,
    },
    /// Actively downloading from Telegram cloud storage.
    Loading {
        /// Bytes received so far.
        loaded_bytes: u64,
        /// Total archive size in bytes.
        total_bytes: u64,
    },
    /// Installed on disk (or system-provided) and currently disabled.
    Ready,
    /// Installed on disk (or system-provided) and currently enabled.
    Active,
    /// Last download or unpack attempt failed.
    Failed(String),
}

impl DictionaryState {
    /// Human-readable status subtitle for a dictionary row (`StateDescription` in `dictionaries_manager.cpp`).
    #[must_use]
    pub fn description(&self) -> String {
        match self {
            Self::Available { size_bytes } => {
                if *size_bytes == 0 {
                    "Available".to_owned()
                } else {
                    format_bytes((*size_bytes).min(i64::MAX as u64) as i64)
                }
            }
            Self::Loading {
                loaded_bytes,
                total_bytes,
            } => {
                if *total_bytes > 0 {
                    let pct = ((*loaded_bytes as f64 / *total_bytes as f64) * 100.0)
                        .round()
                        .clamp(0.0, 100.0);
                    format!("Downloading {pct:.0}%")
                } else {
                    "Downloading...".to_owned()
                }
            }
            Self::Ready => "Downloaded".to_owned(),
            Self::Active => "Enabled".to_owned(),
            Self::Failed(err) => format!("Failed: {err}"),
        }
    }
}

/// Parses a manifest `"channel#postId"` location string (`ParseLocation` in `spellchecker_common.cpp:109`).
#[must_use]
pub fn parse_manifest_location(text: &str) -> Option<(String, i32)> {
    let sep = text.find('#')?;
    if sep == 0 || sep == text.len() - 1 {
        return None;
    }
    let channel = &text[..sep];
    let post_id = text[sep + 1..].parse::<i32>().ok()?;
    if post_id <= 0 {
        return None;
    }
    Some((channel.to_owned(), post_id))
}

/// Maps a tdesktop dictionary language ID (`QLocale::Language` or LWC `lang * 1000 + country`)
/// to its canonical `ll_CC` filename stem (`LocaleFromLangId(langId).name()`).
#[must_use]
pub fn locale_code_for_lang_id(lang_id: i32) -> String {
    match lang_id {
        31 => "en_US".to_owned(),
        31_224 => "en_GB".to_owned(),
        31_038 => "en_CA".to_owned(),
        31_013 => "en_AU".to_owned(),
        96 => "ru_RU".to_owned(),
        42 => "de_DE".to_owned(),
        37 => "fr_FR".to_owned(),
        107 => "es_ES".to_owned(),
        58 => "it_IT".to_owned(),
        91 => "pt_BR".to_owned(),
        91_171 => "pt_PT".to_owned(),
        90 => "pl_PL".to_owned(),
        30 => "nl_NL".to_owned(),
        120 => "tr_TR".to_owned(),
        125 => "uk_UA".to_owned(),
        100 => "sr_RS".to_owned(),
        25 => "cs_CZ".to_owned(),
        111 => "sv_SE".to_owned(),
        8 => "ar_EG".to_owned(),
        _ => format!("lang_{lang_id}"),
    }
}

/// Converts a system BCP-47 language tag (e.g. `"en-US"`, `"de-DE"`) into a stable dictionary ID.
#[must_use]
pub fn lang_id_from_bcp47(tag: &str) -> i32 {
    let norm = tag.trim().replace('-', "_").to_ascii_lowercase();
    match norm.as_str() {
        "en" | "en_us" => 31,
        "en_gb" => 31_224,
        "en_ca" => 31_038,
        "en_au" => 31_013,
        "ru" | "ru_ru" => 96,
        "de" | "de_de" => 42,
        "fr" | "fr_fr" => 37,
        "es" | "es_es" => 107,
        "it" | "it_it" => 58,
        "pt" | "pt_br" => 91,
        "pt_pt" => 91_171,
        "pl" | "pl_pl" => 90,
        "nl" | "nl_nl" => 30,
        "tr" | "tr_tr" => 120,
        "uk" | "uk_ua" => 125,
        "sr" | "sr_rs" | "sr_cyrl_rs" => LANG_ID_SERBIAN,
        "cs" | "cs_cz" => 25,
        "sv" | "sv_se" => 111,
        "ar" | "ar_eg" => 8,
        _ => {
            // Stable positive synthetic ID for other OS locales.
            let mut hash: u32 = 2166136261;
            for b in norm.bytes() {
                hash ^= u32::from(b);
                hash = hash.wrapping_mul(16777619);
            }
            100_000 + (hash % 800_000) as i32
        }
    }
}

/// Parses the JSON manifest fetched from `tdhbcfiles#2949` (`ParseManifest` in `spellchecker_common.cpp:124`).
///
/// Expected schema:
/// ```json
/// { "dictionaries": [ { "id": 31, "size": 245760, "name": "English (US)", "location": "tdhbcfiles#2950" } ] }
/// ```
pub fn parse_dictionaries_manifest(bytes: &[u8]) -> Result<Vec<DictionaryEntry>, String> {
    let val: serde_json::Value = serde_json::from_slice(bytes)
        .map_err(|e| format!("Dictionary manifest JSON parse failed: {e}"))?;
    let arr = val
        .get("dictionaries")
        .and_then(|v| v.as_array())
        .ok_or_else(|| "Dictionary manifest missing 'dictionaries' array.".to_owned())?;

    let mut out = Vec::with_capacity(arr.len());
    for item in arr {
        let Some(obj) = item.as_object() else {
            continue;
        };
        let id = obj.get("id").and_then(|v| v.as_i64()).unwrap_or(0) as i32;
        let size_bytes = obj
            .get("size")
            .and_then(|v| v.as_u64().or_else(|| v.as_f64().map(|f| f.max(0.0) as u64)))
            .unwrap_or(0);
        let name = obj
            .get("name")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .trim()
            .to_owned();
        let location = obj.get("location").and_then(|v| v.as_str()).unwrap_or("");
        let Some((channel, post_id)) = parse_manifest_location(location) else {
            continue;
        };
        if id == 0 || name.is_empty() {
            continue;
        }
        let locale_code = obj
            .get("locale")
            .and_then(|v| v.as_str())
            .filter(|s| !s.trim().is_empty())
            .map(|s| s.trim().to_owned())
            .unwrap_or_else(|| locale_code_for_lang_id(id));
        out.push(DictionaryEntry {
            id,
            locale_code,
            name,
            size_bytes,
            channel,
            post_id,
            is_system: false,
        });
    }
    Ok(out)
}

/// Returns the root directory storing unpacked Hunspell dictionaries (`<working_dir>/tdata/dictionaries`).
#[must_use]
pub fn dictionaries_root(working_dir: &Path) -> PathBuf {
    working_dir.join("tdata").join("dictionaries")
}

/// Returns the per-language directory `<root>/<locale_code>`.
#[must_use]
pub fn dict_dir_for_lang(root: &Path, lang_id: i32, locale_code: Option<&str>) -> PathBuf {
    let stem = locale_code
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
        .unwrap_or_else(|| locale_code_for_lang_id(lang_id));
    root.join(stem)
}

/// Returns `true` if `name` is an allowed Hunspell dictionary archive entry (`.dic` or `.aff`,
/// matching `kDictExtensions` in `spellchecker_common.cpp`).
#[must_use]
pub fn is_good_dictionary_part_name(name: &str) -> bool {
    let trimmed = name.trim_end_matches('/');
    if trimmed.is_empty() || name.ends_with('/') {
        return false;
    }
    let file_name = trimmed.rsplit('/').next().unwrap_or(trimmed);
    DICT_EXTENSIONS
        .iter()
        .any(|ext| file_name.ends_with(&format!(".{ext}")))
}

/// Flattens any nested `.dic`/`.aff` entries extracted into `target_dir` and applies tdesktop's
/// Serbian stem rename quirk (`sr_Cyrl_RS.{dic,aff}` -> `sr_RS.{dic,aff}`).
pub fn finalize_unpacked_dictionary_dir(
    target_dir: &Path,
    lang_id: i32,
    extracted_files: &[String],
) {
    for rel in extracted_files {
        if let Some((_, file_name)) = rel.rsplit_once('/') {
            let from = target_dir.join(rel);
            let to = target_dir.join(file_name);
            if from.exists() && from != to {
                let _ = std::fs::rename(&from, &to);
            }
            // Best-effort cleanup of now-empty intermediate subdirectories.
            let mut cursor = from.parent();
            while let Some(dir) = cursor {
                if dir == target_dir {
                    break;
                }
                if std::fs::remove_dir(dir).is_err() {
                    break;
                }
                cursor = dir.parent();
            }
        }
    }

    // Serbian archive rename quirk (`spellchecker_common.cpp:617-626`).
    if lang_id == LANG_ID_SERBIAN {
        for ext in DICT_EXTENSIONS {
            let from = target_dir.join(format!("sr_Cyrl_RS.{ext}"));
            let to = target_dir.join(format!("sr_RS.{ext}"));
            if from.exists() && !to.exists() {
                let _ = std::fs::rename(from, to);
            }
        }
    }
}

/// Safely unpacks a zipped Hunspell dictionary archive into `<root>/<locale>/` via
/// `tg_storage::cloud_blob::unpack_blob_bytes` with optional constant-time SHA-256 verification,
/// zip-slip/symlink/size-limit protection, and atomic directory swap.
///
/// Any verification or extraction failure leaves a pre-existing `<root>/<locale>/` directory
/// completely untouched.
pub fn unpack_dictionary_zip_verified(
    zip_bytes: &[u8],
    root: &Path,
    lang_id: i32,
    locale_code: Option<&str>,
    expected_sha256: Option<[u8; 32]>,
) -> Result<(PathBuf, UnpackReport), String> {
    let target_dir = dict_dir_for_lang(root, lang_id, locale_code);
    let options = UnpackOptions {
        expected_sha256,
        ..UnpackOptions::default()
    };
    let report = unpack_blob_bytes(
        zip_bytes,
        &target_dir,
        &options,
        is_good_dictionary_part_name,
    )
    .map_err(|e| format!("Dictionary cloud blob unpack failed: {e}"))?;

    finalize_unpacked_dictionary_dir(&target_dir, lang_id, &report.extracted_files);
    Ok((target_dir, report))
}

/// Safely unpacks a zipped Hunspell dictionary archive into `<root>/<locale>/`, extracting only
/// `.dic` and `.aff` files via `tg_storage::cloud_blob` atomic directory swap
/// (`UnpackDictionary` in `spellchecker_common.cpp:608`).
///
/// Rejects path traversal entries, symlinks, and zip bombs, and applies tdesktop's Serbian stem
/// rename (`sr_Cyrl_RS.{dic,aff}` -> `sr_RS.{dic,aff}`).
pub fn unpack_dictionary_zip(
    zip_bytes: &[u8],
    root: &Path,
    lang_id: i32,
    locale_code: Option<&str>,
) -> Result<PathBuf, String> {
    unpack_dictionary_zip_verified(zip_bytes, root, lang_id, locale_code, None).map(|(dir, _)| dir)
}

/// Returns `true` if both `<stem>.dic` and `<stem>.aff` exist and are non-empty on disk
/// (`DictionaryDownloaded` in `spellchecker_common.cpp:630`).
#[must_use]
pub fn is_dictionary_downloaded(root: &Path, lang_id: i32, locale_code: Option<&str>) -> bool {
    if lang_id == 0 {
        return true;
    }
    let stem = locale_code
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
        .unwrap_or_else(|| locale_code_for_lang_id(lang_id));
    let folder = root.join(&stem);
    DICT_EXTENSIONS.iter().all(|ext| {
        let p = folder.join(format!("{stem}.{ext}"));
        std::fs::metadata(&p)
            .map(|m| m.is_file() && m.len() > 0)
            .unwrap_or(false)
    })
}

/// Safely removes a downloaded dictionary folder after validating it contains only `.dic`/`.aff`
/// files with a single shared stem (`RemoveDictionary` / `DeletableDictionaryFolder` in
/// `spellchecker_common.cpp:352-412, 652-665`).
pub fn remove_dictionary(
    root: &Path,
    lang_id: i32,
    locale_code: Option<&str>,
) -> Result<bool, String> {
    if lang_id == 0 {
        return Ok(true);
    }
    let stem = locale_code
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
        .unwrap_or_else(|| locale_code_for_lang_id(lang_id));
    if stem.is_empty()
        || stem == "."
        || stem == ".."
        || stem.contains('/')
        || stem.contains('\\')
        || stem.contains(':')
    {
        return Err(format!("Unsafe dictionary folder name: {stem}"));
    }
    let folder = root.join(&stem);
    if !folder.exists() {
        return Ok(true);
    }
    let meta = std::fs::symlink_metadata(&folder)
        .map_err(|e| format!("Cannot inspect dictionary folder: {e}"))?;
    if meta.file_type().is_symlink() {
        return Err("Refusing to remove symlinked dictionary directory.".to_owned());
    }

    // Verify only `.dic` / `.aff` files with matching stem live inside `folder`.
    let mut detected_stem: Option<String> = None;
    let mut files_to_delete = Vec::new();
    let entries =
        std::fs::read_dir(&folder).map_err(|e| format!("Cannot read dictionary folder: {e}"))?;
    for entry in entries {
        let entry = entry.map_err(|e| format!("Directory iteration error: {e}"))?;
        let ft = entry
            .file_type()
            .map_err(|e| format!("File type error: {e}"))?;
        if ft.is_dir() || ft.is_symlink() {
            return Err(
                "Dictionary folder contains unexpected subdirectory or symlink.".to_owned(),
            );
        }
        let name = entry.file_name().to_string_lossy().into_owned();
        let Some((file_stem, ext)) = name.rsplit_once('.') else {
            return Err(format!("Unexpected file without extension: {name}"));
        };
        if !DICT_EXTENSIONS.contains(&ext) {
            return Err(format!("Unexpected non-dictionary file: {name}"));
        }
        if let Some(ref existing) = detected_stem {
            if existing != file_stem {
                return Err(format!("Mismatched dictionary stems in folder: {name}"));
            }
        } else {
            detected_stem = Some(file_stem.to_owned());
        }
        files_to_delete.push(entry.path());
    }

    for file in files_to_delete {
        std::fs::remove_file(&file)
            .map_err(|e| format!("Could not remove '{}': {e}", file.display()))?;
    }
    let _ = std::fs::remove_dir(&folder);
    Ok(!is_dictionary_downloaded(root, lang_id, Some(&stem)))
}

/// Reads the `.rev` marker inside `root` (`ReadDictionariesRev` in `spellchecker_common.cpp:442`).
#[must_use]
pub fn read_dictionaries_rev(root: &Path) -> i32 {
    std::fs::read_to_string(root.join(".rev"))
        .ok()
        .and_then(|s| s.trim().parse::<i32>().ok())
        .unwrap_or(0)
}

/// Writes [`DICTIONARIES_REV`] to `<root>/.rev` (`WriteDictionariesRev`).
pub fn write_dictionaries_rev(root: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(root)?;
    std::fs::write(root.join(".rev"), DICTIONARIES_REV.to_string())
}

// ============================================================================
// Pure-Rust Hunspell `.dic` + `.aff` loader & checker
// ============================================================================

#[derive(Debug, Clone)]
struct AffixRule {
    strip: String,
    add: String,
    condition: String,
}

impl AffixRule {
    fn matches_suffix(&self, word: &str) -> bool {
        if self.condition == "." || self.condition.is_empty() {
            return true;
        }
        word.ends_with(&self.condition)
    }

    fn matches_prefix(&self, word: &str) -> bool {
        if self.condition == "." || self.condition.is_empty() {
            return true;
        }
        word.starts_with(&self.condition)
    }
}

/// Loaded Hunspell dictionary parsed from `<stem>.dic` and `<stem>.aff`.
#[derive(Debug, Clone)]
pub struct HunspellDictionary {
    /// Locale stem (e.g. `"en_US"`).
    pub locale_code: String,
    words: HashSet<String>,
    words_lower: HashSet<String>,
}

impl HunspellDictionary {
    /// Loads `<stem>.dic` and `<stem>.aff` from `dir`.
    pub fn load_from_dir(dir: &Path, stem: &str) -> Result<Self, String> {
        let dic_path = dir.join(format!("{stem}.dic"));
        let aff_path = dir.join(format!("{stem}.aff"));
        let dic_bytes = std::fs::read(&dic_path)
            .map_err(|e| format!("Failed to read '{}': {e}", dic_path.display()))?;
        let aff_bytes = std::fs::read(&aff_path)
            .map_err(|e| format!("Failed to read '{}': {e}", aff_path.display()))?;
        Self::parse(stem, &dic_bytes, &aff_bytes)
    }

    /// Parses raw `.dic` and `.aff` bytes into an in-memory Hunspell dictionary.
    pub fn parse(
        locale_code: impl Into<String>,
        dic_bytes: &[u8],
        aff_bytes: &[u8],
    ) -> Result<Self, String> {
        let dic_text = String::from_utf8_lossy(dic_bytes);
        let aff_text = String::from_utf8_lossy(aff_bytes);

        let mut suffixes: HashMap<char, Vec<AffixRule>> = HashMap::new();
        let mut prefixes: HashMap<char, Vec<AffixRule>> = HashMap::new();

        for line in aff_text.lines() {
            let trimmed = line.trim();
            if trimmed.is_empty() || trimmed.starts_with('#') {
                continue;
            }
            let parts: Vec<&str> = trimmed.split_whitespace().collect();
            if parts.len() >= 5 && (parts[0] == "SFX" || parts[0] == "PFX") {
                let Some(flag) = parts[1].chars().next() else {
                    continue;
                };
                // Skip header lines like `SFX S Y 1`
                if parts[2] == "Y" || parts[2] == "N" {
                    continue;
                }
                let strip = if parts[2] == "0" {
                    String::new()
                } else {
                    parts[2].to_owned()
                };
                let add = if parts[3] == "0" {
                    String::new()
                } else {
                    parts[3].split('/').next().unwrap_or(parts[3]).to_owned()
                };
                let condition = parts[4].to_owned();
                let rule = AffixRule {
                    strip,
                    add,
                    condition,
                };
                if parts[0] == "SFX" {
                    suffixes.entry(flag).or_default().push(rule);
                } else {
                    prefixes.entry(flag).or_default().push(rule);
                }
            }
        }

        let mut words = HashSet::new();
        let mut words_lower = HashSet::new();
        let mut lines = dic_text.lines();
        // First line in Hunspell `.dic` is the approximate word count.
        if let Some(first) = lines.next() {
            let trimmed = first.trim();
            if trimmed.parse::<usize>().is_err() && !trimmed.is_empty() {
                Self::insert_dic_entry(trimmed, &prefixes, &suffixes, &mut words, &mut words_lower);
            }
        }
        for line in lines {
            let trimmed = line.trim();
            if trimmed.is_empty() || trimmed.starts_with('#') {
                continue;
            }
            Self::insert_dic_entry(trimmed, &prefixes, &suffixes, &mut words, &mut words_lower);
        }

        if words.is_empty() {
            return Err("Hunspell .dic file contained no words.".to_owned());
        }

        Ok(Self {
            locale_code: locale_code.into(),
            words,
            words_lower,
        })
    }

    fn insert_dic_entry(
        entry: &str,
        prefixes: &HashMap<char, Vec<AffixRule>>,
        suffixes: &HashMap<char, Vec<AffixRule>>,
        words: &mut HashSet<String>,
        words_lower: &mut HashSet<String>,
    ) {
        let token = entry.split_whitespace().next().unwrap_or(entry);
        let (root, flags) = match token.split_once('/') {
            Some((w, f)) => (w, f),
            None => (token, ""),
        };
        if root.is_empty() {
            return;
        }
        words.insert(root.to_owned());
        words_lower.insert(root.to_lowercase());

        for flag in flags.chars() {
            if let Some(rules) = suffixes.get(&flag) {
                for rule in rules {
                    if rule.matches_suffix(root)
                        && let Some(base) = root.strip_suffix(&rule.strip)
                    {
                        let derived = format!("{base}{}", rule.add);
                        words_lower.insert(derived.to_lowercase());
                        words.insert(derived);
                    }
                }
            }
            if let Some(rules) = prefixes.get(&flag) {
                for rule in rules {
                    if rule.matches_prefix(root)
                        && let Some(base) = root.strip_prefix(&rule.strip)
                    {
                        let derived = format!("{}{base}", rule.add);
                        words_lower.insert(derived.to_lowercase());
                        words.insert(derived);
                    }
                }
            }
        }
    }

    /// Returns `true` if `word` is accepted by this Hunspell dictionary.
    #[must_use]
    pub fn check_word(&self, word: &str) -> bool {
        let trimmed = word.trim();
        if trimmed.is_empty() {
            return true;
        }
        self.words.contains(trimmed) || self.words_lower.contains(&trimmed.to_lowercase())
    }

    /// Returns up to `max_suggestions` candidate corrections ordered by edit distance.
    #[must_use]
    pub fn suggestions(&self, word: &str, max_suggestions: usize) -> Vec<String> {
        let needle = word.trim().to_lowercase();
        if needle.is_empty() || max_suggestions == 0 {
            return Vec::new();
        }
        let mut scored: Vec<(usize, &String)> = self
            .words
            .iter()
            .filter_map(|cand| {
                let c_low = cand.to_lowercase();
                let len_diff = needle.chars().count().abs_diff(c_low.chars().count());
                if len_diff > 2 {
                    return None;
                }
                let dist = edit_distance(&needle, &c_low);
                (dist > 0 && dist <= 2).then_some((dist, cand))
            })
            .collect();
        scored.sort_by(|a, b| a.0.cmp(&b.0).then_with(|| a.1.cmp(b.1)));
        scored
            .into_iter()
            .take(max_suggestions)
            .map(|(_, s)| s.clone())
            .collect()
    }

    /// Number of expanded word forms stored in this dictionary.
    #[must_use]
    pub fn word_count(&self) -> usize {
        self.words.len()
    }
}

fn edit_distance(a: &str, b: &str) -> usize {
    let a: Vec<char> = a.chars().collect();
    let b: Vec<char> = b.chars().collect();
    let mut prev: Vec<usize> = (0..=b.len()).collect();
    let mut curr = vec![0usize; b.len() + 1];
    for (i, &ca) in a.iter().enumerate() {
        curr[0] = i + 1;
        for (j, &cb) in b.iter().enumerate() {
            let cost = usize::from(ca != cb);
            curr[j + 1] = (prev[j + 1] + 1).min(curr[j] + 1).min(prev[j] + cost);
        }
        std::mem::swap(&mut prev, &mut curr);
    }
    prev[b.len()]
}

/// Multi-dictionary Hunspell engine running alongside WinRT `ISpellChecker`.
#[derive(Debug, Default)]
pub struct HunspellEngine {
    dictionaries: BTreeMap<i32, HunspellDictionary>,
    custom_words: HashSet<String>,
    ignored_words: HashSet<String>,
}

impl HunspellEngine {
    /// Creates an empty [`HunspellEngine`].
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Reloads all enabled Hunspell dictionaries that exist under `root`.
    pub fn load_enabled(&mut self, root: &Path, enabled_ids: &[i32]) -> usize {
        self.dictionaries.clear();
        for &lang_id in enabled_ids {
            let stem = locale_code_for_lang_id(lang_id);
            let dir = root.join(&stem);
            if let Ok(dict) = HunspellDictionary::load_from_dir(&dir, &stem) {
                self.dictionaries.insert(lang_id, dict);
            }
        }
        self.dictionaries.len()
    }

    /// Registers a parsed [`HunspellDictionary`] directly.
    pub fn insert_dictionary(&mut self, lang_id: i32, dict: HunspellDictionary) {
        self.dictionaries.insert(lang_id, dict);
    }

    /// Adds a custom user word (`Platform::Spellchecker::AddWord`).
    pub fn add_word(&mut self, word: impl Into<String>) {
        self.custom_words.insert(word.into().to_lowercase());
    }

    /// Ignores a word for the current session.
    pub fn ignore_word(&mut self, word: impl Into<String>) {
        self.ignored_words.insert(word.into().to_lowercase());
    }

    /// Returns `true` if any loaded dictionary or custom/ignored set accepts `word`.
    #[must_use]
    pub fn check_word(&self, word: &str) -> bool {
        let low = word.trim().to_lowercase();
        if low.is_empty() || self.custom_words.contains(&low) || self.ignored_words.contains(&low) {
            return true;
        }
        if self.dictionaries.is_empty() {
            return true;
        }
        self.dictionaries.values().any(|d| d.check_word(word))
    }

    /// Collects replacement suggestions across all loaded Hunspell dictionaries.
    #[must_use]
    pub fn suggestions(&self, word: &str, max_suggestions: usize) -> Vec<String> {
        let mut out = Vec::new();
        for dict in self.dictionaries.values() {
            for cand in dict.suggestions(word, max_suggestions) {
                if !out.contains(&cand) {
                    out.push(cand);
                }
                if out.len() >= max_suggestions {
                    return out;
                }
            }
        }
        out
    }

    /// Returns the loaded dictionary IDs.
    #[must_use]
    pub fn loaded_ids(&self) -> Vec<i32> {
        self.dictionaries.keys().copied().collect()
    }
}

// ============================================================================
// Cloud Dictionary Channel Trait & Mock Implementation
// ============================================================================

/// Backend source for fetching the Hunspell manifest and zipped dictionary documents from the
/// Telegram cloud dictionary channel (`tdhbcfiles`).
pub trait DictionaryCloudSource {
    /// Fetches and parses the dictionary manifest JSON from `tdhbcfiles#2949`.
    fn fetch_manifest(&mut self) -> Result<Vec<DictionaryEntry>, String>;

    /// Downloads the zipped Hunspell `.dic`/`.aff` document from `channel#post_id`.
    fn download_dictionary_archive(
        &mut self,
        channel: &str,
        post_id: i32,
    ) -> Result<Vec<u8>, String>;

    /// Downloads, optionally SHA-256-verifies, and atomically unpacks a Hunspell dictionary
    /// archive from `channel#post_id` into `<root>/<locale>/` via `tg_storage::cloud_blob`.
    ///
    /// Default implementation downloads bytes via [`Self::download_dictionary_archive`] and
    /// unpacks via [`unpack_dictionary_zip_verified`]. Live RPC sources override this to stream
    /// through [`tg_storage::CloudBlobLoader`].
    #[allow(clippy::too_many_arguments)]
    fn download_and_unpack_dictionary(
        &mut self,
        channel: &str,
        post_id: i32,
        size_bytes: u64,
        root: &Path,
        lang_id: i32,
        locale_code: Option<&str>,
        expected_sha256: Option<[u8; 32]>,
    ) -> Result<PathBuf, String> {
        let _ = size_bytes;
        let bytes = self.download_dictionary_archive(channel, post_id)?;
        unpack_dictionary_zip_verified(&bytes, root, lang_id, locale_code, expected_sha256)
            .map(|(dir, _)| dir)
    }
}

/// Shared slot holding the live [`DictionaryCloudSource`] attached by the session backend
/// (`SettingsBackend::dictionary_cloud_source`), so `AdvancedState` stays `Clone + Debug`.
#[derive(Clone, Default)]
pub struct DictionarySourceSlot(
    Option<std::sync::Arc<std::sync::Mutex<Box<dyn DictionaryCloudSource + Send>>>>,
);

impl std::fmt::Debug for DictionarySourceSlot {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DictionarySourceSlot")
            .field("attached", &self.0.is_some())
            .finish()
    }
}

impl DictionarySourceSlot {
    /// Wraps `source` in a shareable slot.
    #[must_use]
    pub fn new(source: Box<dyn DictionaryCloudSource + Send>) -> Self {
        Self(Some(std::sync::Arc::new(std::sync::Mutex::new(source))))
    }

    /// Whether a live source has been attached.
    #[must_use]
    pub const fn is_attached(&self) -> bool {
        self.0.is_some()
    }

    /// Runs `f` with the attached source, or returns `None` when nothing is attached.
    pub fn with<R>(&self, f: impl FnOnce(&mut dyn DictionaryCloudSource) -> R) -> Option<R> {
        let slot = self.0.as_ref()?;
        let mut guard = slot
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        Some(f(guard.as_mut()))
    }
}

/// Builds a valid in-memory `.zip` archive containing `<stem>.dic` and `<stem>.aff` for tests
/// and mock mode (zero network access).
pub fn build_dictionary_zip_bytes(stem: &str, dic_content: &str, aff_content: &str) -> Vec<u8> {
    let mut cursor = Cursor::new(Vec::new());
    {
        let mut writer = zip::ZipWriter::new(&mut cursor);
        let opts = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Deflated);
        let _ = writer.start_file(format!("{stem}.dic"), opts);
        let _ = writer.write_all(dic_content.as_bytes());
        let _ = writer.start_file(format!("{stem}.aff"), opts);
        let _ = writer.write_all(aff_content.as_bytes());
        let _ = writer.finish();
    }
    cursor.into_inner()
}

/// Offline mock [`DictionaryCloudSource`] with synthetic `.dic`/`.aff` archives.
#[derive(Debug, Clone)]
pub struct MockDictionaryCloudSource {
    entries: Vec<DictionaryEntry>,
    archives: HashMap<(String, i32), Vec<u8>>,
}

impl Default for MockDictionaryCloudSource {
    fn default() -> Self {
        let entries = vec![
            DictionaryEntry {
                id: 31,
                locale_code: "en_US".to_owned(),
                name: "English (US)".to_owned(),
                size_bytes: 245_760,
                channel: DICTIONARIES_MANIFEST_CHANNEL.to_owned(),
                post_id: 2950,
                is_system: false,
            },
            DictionaryEntry {
                id: 42,
                locale_code: "de_DE".to_owned(),
                name: "Deutsch".to_owned(),
                size_bytes: 512_000,
                channel: DICTIONARIES_MANIFEST_CHANNEL.to_owned(),
                post_id: 2951,
                is_system: false,
            },
            DictionaryEntry {
                id: 96,
                locale_code: "ru_RU".to_owned(),
                name: "Русский".to_owned(),
                size_bytes: 680_000,
                channel: DICTIONARIES_MANIFEST_CHANNEL.to_owned(),
                post_id: 2952,
                is_system: false,
            },
            DictionaryEntry {
                id: 107,
                locale_code: "es_ES".to_owned(),
                name: "Español".to_owned(),
                size_bytes: 390_000,
                channel: DICTIONARIES_MANIFEST_CHANNEL.to_owned(),
                post_id: 2953,
                is_system: false,
            },
        ];
        let mut archives = HashMap::new();
        archives.insert(
            (DICTIONARIES_MANIFEST_CHANNEL.to_owned(), 2950),
            build_dictionary_zip_bytes(
                "en_US",
                "3\nhello/S\nworld\ntelegram\n",
                "SET UTF-8\nSFX S Y 1\nSFX S 0 s .\n",
            ),
        );
        archives.insert(
            (DICTIONARIES_MANIFEST_CHANNEL.to_owned(), 2951),
            build_dictionary_zip_bytes("de_DE", "2\nHallo\nWelt\n", "SET UTF-8\n"),
        );
        archives.insert(
            (DICTIONARIES_MANIFEST_CHANNEL.to_owned(), 2952),
            build_dictionary_zip_bytes("ru_RU", "2\nпривет\nмир\n", "SET UTF-8\n"),
        );
        archives.insert(
            (DICTIONARIES_MANIFEST_CHANNEL.to_owned(), 2953),
            build_dictionary_zip_bytes("es_ES", "2\nhola\nmundo\n", "SET UTF-8\n"),
        );
        Self { entries, archives }
    }
}

impl DictionaryCloudSource for MockDictionaryCloudSource {
    fn fetch_manifest(&mut self) -> Result<Vec<DictionaryEntry>, String> {
        Ok(self.entries.clone())
    }

    fn download_dictionary_archive(
        &mut self,
        channel: &str,
        post_id: i32,
    ) -> Result<Vec<u8>, String> {
        self.archives
            .get(&(channel.to_owned(), post_id))
            .cloned()
            .ok_or_else(|| format!("Dictionary archive {channel}#{post_id} not found."))
    }
}

// ============================================================================
// ManageDictionariesState & egui Panel (`boxes/dictionaries_manager.cpp`)
// ============================================================================
