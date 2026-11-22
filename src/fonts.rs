//! Local font discovery and explicit installation into CutAd's per-user library.

use anyhow::{Context, Result, ensure};
use fontdb::{Database, FaceInfo, Source, Style};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashSet};
use std::fs::File;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock, RwLock};
use std::time::Duration;

const MAX_FONT: usize = 24 * 1024 * 1024;
pub(crate) const MAX_ARCHIVE: u64 = 64 * 1024 * 1024;
const FONT_ROOT: &str =
    "https://raw.githubusercontent.com/google/fonts/06bfe0d62a98375cb09c942fa18e68bd32cff057/ofl";

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct FontRef {
    pub family: String,
    pub postscript: String,
}

#[derive(Clone, Debug)]
pub struct FontFace {
    pub reference: FontRef,
    pub style: String,
    pub path: PathBuf,
    pub imported: bool,
}

pub struct CatalogFont {
    pub name: &'static str,
    pub category: &'static str,
    pub file: &'static str,
}

pub const CATALOG: &[CatalogFont] = &[
    CatalogFont {
        name: "Anton",
        category: "Heavy display",
        file: "anton/Anton-Regular.ttf",
    },
    CatalogFont {
        name: "Bangers",
        category: "Comic lettering",
        file: "bangers/Bangers-Regular.ttf",
    },
    CatalogFont {
        name: "Bebas Neue",
        category: "Tall condensed",
        file: "bebasneue/BebasNeue-Regular.ttf",
    },
    CatalogFont {
        name: "Bungee",
        category: "Block lettering",
        file: "bungee/Bungee-Regular.ttf",
    },
    CatalogFont {
        name: "Lilita One",
        category: "Rounded display",
        file: "lilitaone/LilitaOne-Regular.ttf",
    },
    CatalogFont {
        name: "Caveat Brush",
        category: "Brush lettering",
        file: "caveatbrush/CaveatBrush-Regular.ttf",
    },
    CatalogFont {
        name: "Press Start 2P",
        category: "Pixel lettering",
        file: "pressstart2p/PressStart2P-Regular.ttf",
    },
    CatalogFont {
        name: "Rubik Glitch",
        category: "Distorted display",
        file: "rubikglitch/RubikGlitch-Regular.ttf",
    },
];

static INDEX: OnceLock<RwLock<Option<Arc<Vec<FontFace>>>>> = OnceLock::new();

fn index() -> &'static RwLock<Option<Arc<Vec<FontFace>>>> {
    INDEX.get_or_init(|| RwLock::new(None))
}

pub fn user_library() -> Result<PathBuf> {
    if let Some(dir) = std::env::var_os("CUTAD_FONT_DIR").filter(|dir| !dir.is_empty()) {
        return Ok(std::path::absolute(dir)?);
    }
    #[cfg(windows)]
    let base = std::env::var_os("LOCALAPPDATA")
        .map(PathBuf::from)
        .map(|p| p.join("CutAd"));
    #[cfg(not(windows))]
    let base = std::env::var_os("XDG_DATA_HOME")
        .filter(|dir| !dir.is_empty())
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|dir| PathBuf::from(dir).join(".local/share")))
        .map(|p| p.join("cutad"));
    Ok(base
        .context("The current user has no application data folder.")?
        .join("fonts"))
}

pub fn available() -> Arc<Vec<FontFace>> {
    if let Some(cached) = index().read().unwrap().as_ref() {
        return cached.clone();
    }
    refresh()
}

pub fn refresh() -> Arc<Vec<FontFace>> {
    // Serialize initial discovery and installation refreshes so workers share the same snapshot.
    let mut state = index().write().unwrap();
    let library = user_library().ok();
    let mut database = Database::new();
    if let Some(path) = library.as_ref() {
        database.load_fonts_dir(path);
    }
    database.load_system_fonts();
    let mut faces = BTreeMap::new();
    for face in database.faces().filter(|face| face.index == 0) {
        let path = match &face.source {
            Source::File(path) | Source::SharedFile(path, _) => path,
            Source::Binary(_) => continue,
        };
        let Some(reference) = reference(face) else {
            continue;
        };
        faces
            .entry(reference.postscript.to_lowercase())
            .or_insert_with(|| FontFace {
                reference,
                style: face_style(face),
                path: path.to_owned(),
                imported: library.as_ref().is_some_and(|dir| path.starts_with(dir)),
            });
    }
    let mut faces: Vec<_> = faces.into_values().collect();
    faces.sort_by_cached_key(|face| (face.reference.family.to_lowercase(), face.style.clone()));
    let ready = Arc::new(faces);
    *state = Some(ready.clone());
    ready
}

pub fn resolve(font: &FontRef) -> Option<PathBuf> {
    let stem = filename_stem(&font.postscript);
    if !stem.is_empty()
        && let Ok(library) = user_library()
    {
        for extension in ["ttf", "otf"] {
            let path = library.join(format!("{stem}.{extension}"));
            if let Ok(file) = File::open(&path)
                && let Ok(bytes) = bounded(file, MAX_FONT)
                && identify(&bytes).is_ok_and(|current| current.postscript == font.postscript)
            {
                return Some(path);
            }
        }
    }
    let available = available();
    available
        .iter()
        .find(|face| {
            face.reference
                .postscript
                .eq_ignore_ascii_case(&font.postscript)
        })
        .or_else(|| {
            available.iter().find(|face| {
                face.reference.family.eq_ignore_ascii_case(&font.family) && face.style == "Regular"
            })
        })
        .map(|face| face.path.clone())
}

fn filename_stem(postscript: &str) -> String {
    postscript
        .chars()
        .filter(|ch| ch.is_ascii_alphanumeric() || *ch == '-')
        .take(72)
        .collect()
}

fn reference(face: &FaceInfo) -> Option<FontRef> {
    let family = face.families.first()?.0.clone();
    let postscript = face.post_script_name.clone();
    (!family.is_empty() && !postscript.is_empty()).then_some(FontRef { family, postscript })
}

fn face_style(face: &FaceInfo) -> String {
    let weight = match face.weight.0 {
        0..=150 => "Thin",
        151..=250 => "Extra Light",
        251..=350 => "Light",
        351..=450 => "Regular",
        451..=550 => "Medium",
        551..=650 => "Semi Bold",
        651..=750 => "Bold",
        751..=850 => "Extra Bold",
        _ => "Black",
    };
    match face.style {
        Style::Normal => weight.to_string(),
        Style::Italic if weight == "Regular" => "Italic".into(),
        Style::Oblique if weight == "Regular" => "Oblique".into(),
        Style::Italic => format!("{weight} Italic"),
        Style::Oblique => format!("{weight} Oblique"),
    }
}

fn font_suffix(name: &Path) -> bool {
    name.extension()
        .and_then(|s| s.to_str())
        .is_some_and(|ext| matches!(ext.to_ascii_lowercase().as_str(), "ttf" | "otf"))
}

pub(crate) fn bounded(mut source: impl Read, max: usize) -> Result<Vec<u8>> {
    let mut bytes = Vec::new();
    source
        .by_ref()
        .take(max as u64 + 1)
        .read_to_end(&mut bytes)?;
    ensure!(bytes.len() <= max, "The font file exceeds the size limit.");
    Ok(bytes)
}

pub fn import(path: &Path) -> Result<Vec<FontRef>> {
    let fonts = import_into(path, &user_library()?)?;
    refresh();
    Ok(fonts)
}

pub fn import_into(path: &Path, destination: &Path) -> Result<Vec<FontRef>> {
    let file = File::open(path)?;
    if font_suffix(path) {
        return Ok(vec![install_bytes(
            bounded(file, MAX_FONT)?,
            destination,
            None,
        )?]);
    }
    ensure!(
        path.extension()
            .is_some_and(|ext| ext.eq_ignore_ascii_case("zip")),
        "Choose a TTF, OTF, or ZIP font file."
    );
    ensure!(
        file.metadata()?.len() <= MAX_ARCHIVE,
        "The ZIP exceeds the 64 MiB limit."
    );
    let mut zip = zip::ZipArchive::new(file).context("Could not read the font ZIP.")?;
    ensure!(zip.len() <= 256, "The ZIP contains too many entries.");
    let mut ready = Vec::new();
    let mut total = 0_u64;
    for i in 0..zip.len() {
        let entry = zip.by_index(i)?;
        if entry.is_dir() || !font_suffix(Path::new(entry.name()?.as_ref())) {
            continue;
        }
        total = total.saturating_add(entry.size());
        ensure!(total <= MAX_ARCHIVE, "The extracted fonts exceed 64 MiB.");
        let bytes = bounded(entry, MAX_FONT)?;
        if identify(&bytes).is_ok() {
            ready.push(bytes);
        }
    }
    ensure!(
        !ready.is_empty(),
        "The ZIP contains no readable TTF or OTF font."
    );
    let mut found = Vec::new();
    let mut seen = HashSet::new();
    for bytes in ready {
        let font = install_bytes(bytes, destination, None)?;
        if seen.insert(font.postscript.clone()) {
            found.push(font);
        }
    }
    Ok(found)
}

pub fn get(catalog: &CatalogFont) -> Result<FontRef> {
    let (folder, _) = catalog
        .file
        .split_once('/')
        .context("Invalid catalog entry.")?;
    let agent = ureq::AgentBuilder::new()
        .timeout(Duration::from_secs(18))
        .build();
    let response = |url: &str| {
        agent
            .get(url)
            .call()
            .with_context(|| format!("Could not download {}.", catalog.name))
    };
    let license = bounded(
        response(&format!("{FONT_ROOT}/{folder}/OFL.txt"))?.into_reader(),
        64 * 1024,
    )?;
    ensure!(
        String::from_utf8_lossy(&license)
            .to_ascii_lowercase()
            .contains("sil open font license"),
        "The catalog returned an unrecognized font license."
    );
    let bytes = bounded(
        response(&format!("{FONT_ROOT}/{}", catalog.file))?.into_reader(),
        MAX_FONT,
    )?;
    let font = install_bytes(bytes, &user_library()?, Some(&license))?;
    refresh();
    Ok(font)
}

fn identify(bytes: &[u8]) -> Result<FontRef> {
    ensure!(
        !bytes.starts_with(b"ttcf"),
        "Choose an individual TTF or OTF font."
    );
    let mut database = Database::new();
    database.load_font_data(bytes.to_vec());
    let face = database
        .faces()
        .find_map(reference)
        .context("This is not a readable font.")?;
    Ok(face)
}

fn install_bytes(bytes: Vec<u8>, dir: &Path, license: Option<&[u8]>) -> Result<FontRef> {
    let font = identify(&bytes)?;
    std::fs::create_dir_all(dir)?;
    let stem = filename_stem(&font.postscript);
    ensure!(!stem.is_empty(), "The font has an invalid name.");
    let suffix = if bytes.starts_with(b"OTTO") {
        "otf"
    } else {
        "ttf"
    };
    if let Some(license) = license {
        atomic_write(dir, &dir.join(format!("{stem}-OFL.txt")), license)?;
    }
    atomic_write(dir, &dir.join(format!("{stem}.{suffix}")), &bytes)?;
    Ok(font)
}

fn atomic_write(dir: &Path, target: &Path, bytes: &[u8]) -> Result<()> {
    let mut temp = tempfile::NamedTempFile::new_in(dir)?;
    temp.write_all(bytes)?;
    temp.as_file().sync_all()?;
    temp.persist(target)?;
    Ok(())
}
