use std::{
    collections::{HashMap, HashSet},
    path::Path,
};

use norad::Name;
use serde::{Deserialize, Serialize};

use layer::Layer;
use source::Source;

mod layer;
mod metadata;
mod source;

#[derive(Debug, Default, PartialEq)]
pub struct Fontgarden {
    pub sets: HashMap<Name, Set>,
}

#[derive(Debug, Default, PartialEq)]
pub struct Set {
    pub glyph_data: HashMap<Name, GlyphRecord>,
    pub sources: HashMap<Name, Source>,
}

#[derive(Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct GlyphRecord {
    pub postscript_name: Option<String>,
    #[serde(default)]
    pub codepoints: Vec<char>,
    // TODO: Make an enum
    pub opentype_category: Option<String>,
    // TODO: Write fn default that sets true here
    #[serde(default = "default_true")]
    pub export: bool,
}

fn default_true() -> bool {
    true
}

#[derive(thiserror::Error, Debug)]
pub enum LoadError {
    #[error("failed to load data from disk")]
    Io(#[from] std::io::Error),
    #[error("a fontgarden must be a directory")]
    NotAFontgarden,
    #[error("cannot import a glyph as it's in a different set already")]
    DuplicateGlyph,
    #[error("no default layer for source found")]
    NoDefaultLayer,
    #[error("duplicate default layer for source found")]
    DuplicateDefaultLayer,
}

#[derive(thiserror::Error, Debug)]
pub enum ExportError {
    #[error("failed to load data from disk")]
    Other(#[from] Box<dyn std::error::Error>),
}

impl Fontgarden {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn from_path(path: &Path) -> Result<Self, LoadError> {
        let mut fontgarden = Self::new();
        let mut seen_glyph_names: HashSet<Name> = HashSet::new();

        if path.is_dir() {
            for entry in std::fs::read_dir(path)? {
                let entry = entry?;
                let path = entry.path();
                let metadata = entry.metadata()?;
                if metadata.is_dir() {
                    let name = path
                        .file_name()
                        .expect("can't read filename")
                        .to_string_lossy();
                    if let Some(set_name) = name.strip_prefix("set.") {
                        let set = Set::from_path(&path)?;
                        let coverage = set.glyph_coverage();
                        if seen_glyph_names.intersection(&coverage).next().is_some() {
                            return Err(LoadError::DuplicateGlyph);
                        }
                        seen_glyph_names.extend(coverage);
                        fontgarden
                            .sets
                            .insert(Name::new(set_name).expect("can't read set name"), set);
                    }
                }
            }
        } else {
            return Err(LoadError::NotAFontgarden);
        }

        Ok(fontgarden)
    }

    pub fn save(&self, path: &Path) {
        if path.exists() {
            std::fs::remove_dir_all(path).expect("can't remove target dir");
        }
        std::fs::create_dir(path).expect("can't create target dir");

        for (set_name, set) in &self.sets {
            set.save(set_name, path);
        }
    }

    pub fn import(
        &mut self,
        font: &norad::Font,
        glyphs: &HashSet<Name>,
        set_name: &Name,
        source_name: &Name,
    ) -> Result<(), LoadError> {
        let set = self.sets.entry(set_name.clone()).or_default();
        let source = set.sources.entry(source_name.clone()).or_default();

        // TODO: check for glyph uniqueness per set
        // TODO: follow components and check if they are present in another set
        let glyph_data = crate::util::extract_glyph_data(font, glyphs);
        set.glyph_data.extend(glyph_data);

        for layer in font.iter_layers() {
            let mut our_layer = Layer::from_ufo_layer(layer, glyphs);
            if layer == font.default_layer() {
                our_layer.default = true;
                source.layers.insert(layer.name().clone(), our_layer);
            } else if !our_layer.glyphs.is_empty() {
                source.layers.insert(layer.name().clone(), our_layer);
            }
        }

        Ok(())
    }

    pub fn export(
        &self,
        set_names: &HashSet<Name>,
        glyph_names: &HashSet<Name>,
        source_names: &HashSet<Name>,
    ) -> Result<HashMap<Name, norad::Font>, ExportError> {
        let mut ufos: HashMap<Name, norad::Font> = HashMap::new();

        for (_, set) in self
            .sets
            .iter()
            .filter(|(name, _)| set_names.contains(*name))
        {
            for (source_name, source) in set
                .sources
                .iter()
                .filter(|(name, _)| source_names.contains(*name))
            {
                let ufo = ufos
                    .entry(source_name.clone())
                    .or_insert_with(norad::Font::new);
                for (layer_name, layer) in &source.layers {
                    let layer_glyphs: Vec<_> = layer
                        .glyphs
                        .values()
                        .filter(|g| glyph_names.contains(&*g.name))
                        .collect();
                    if layer_glyphs.is_empty() {
                        continue;
                    }
                    if layer.default {
                        {
                            let ufo_layer = ufo.layers.default_layer_mut();
                            for glyph in layer_glyphs {
                                ufo_layer.insert_glyph(glyph.clone());
                            }
                        }
                        // TODO: test renaming with mutatorsans
                        ufo.layers
                            .rename_layer(
                                &ufo.layers.default_layer().name().clone(),
                                layer_name,
                                false,
                            )
                            .unwrap();
                    } else {
                        match ufo.layers.get_mut(layer_name) {
                            Some(ufo_layer) => {
                                for glyph in layer_glyphs {
                                    ufo_layer.insert_glyph(glyph.clone());
                                }
                            }
                            None => {
                                let ufo_layer = ufo
                                    .layers
                                    .new_layer(layer_name)
                                    .expect("can't make new layer");
                                for glyph in layer_glyphs {
                                    ufo_layer.insert_glyph(glyph.clone());
                                }
                            }
                        }
                    }
                }
            }
        }

        Ok(ufos)
    }
}

impl Set {
    fn from_path(path: &Path) -> Result<Self, LoadError> {
        let glyph_data = metadata::load_glyph_data(&path.join("glyph_data.csv"));

        let mut sources = HashMap::new();
        for entry in std::fs::read_dir(path)? {
            let entry = entry?;
            let path = entry.path();
            let metadata = entry.metadata()?;
            if metadata.is_dir() {
                let name = path
                    .file_name()
                    .expect("can't read filename")
                    .to_string_lossy();
                if let Some(source_name) = name.strip_prefix("source.") {
                    let source = Source::from_path(&path)?;
                    sources.insert(
                        Name::new(source_name).expect("can't read source name"),
                        source,
                    );
                }
            }
        }

        Ok(Set {
            glyph_data,
            sources,
        })
    }

    pub fn save(&self, set_name: &Name, root_path: &Path) {
        let set_path = root_path.join(format!("set.{set_name}"));
        std::fs::create_dir(&set_path).expect("can't create set dir");

        metadata::write_glyph_data(&self.glyph_data, &set_path.join("glyph_data.csv"));

        for (source_name, source) in &self.sources {
            source.save(source_name, &set_path)
        }
    }

    pub fn glyph_coverage(&self) -> HashSet<Name> {
        let mut glyphs = HashSet::new();
        glyphs.extend(self.glyph_data.keys().cloned());
        for source in self.sources.values() {
            for layer in source.layers.values() {
                glyphs.extend(layer.glyphs.keys().cloned());
            }
        }
        glyphs
    }
}

#[cfg(test)]
mod tests {
    // use pretty_assertions::assert_eq;

    use super::*;

    const NOTO_TEMP: &str = r"C:\Users\nikolaus.waxweiler\AppData\Local\Dev\nototest";

    #[test]
    fn load_empty() {
        let tempdir = tempfile::TempDir::new().unwrap();

        let fontgarden = Fontgarden::new();
        fontgarden.save(tempdir.path());
        let fontgarden2 = Fontgarden::from_path(tempdir.path()).unwrap();

        assert_eq!(fontgarden, fontgarden2);
    }

    #[test]
    fn roundtrip() {
        let mut fontgarden = Fontgarden::new();

        let tmp_path = Path::new(NOTO_TEMP);

        let latin_glyphs: HashSet<Name> = HashSet::from(
            ["A", "B", "Adieresis", "dieresiscomb", "dieresis"].map(|v| Name::new(v).unwrap()),
        );
        let latin_set_name = Name::new("Latin").unwrap();

        let ufo1 = norad::Font::load(tmp_path.join("NotoSans-Light.ufo")).unwrap();
        let ufo2 = norad::Font::load(tmp_path.join("NotoSans-Bold.ufo")).unwrap();

        fontgarden
            .import(
                &ufo1,
                &latin_glyphs,
                &latin_set_name,
                &Name::new("Light").unwrap(),
            )
            .unwrap();
        fontgarden
            .import(
                &ufo2,
                &latin_glyphs,
                &latin_set_name,
                &Name::new("Bold").unwrap(),
            )
            .unwrap();

        let greek_glyphs: HashSet<Name> = HashSet::from(
            ["Alpha", "Alphatonos", "tonos.case", "tonos"].map(|v| Name::new(v).unwrap()),
        );
        let greek_set_name = Name::new("Greek").unwrap();

        fontgarden
            .import(
                &ufo1,
                &greek_glyphs,
                &greek_set_name,
                &Name::new("Light").unwrap(),
            )
            .unwrap();
        fontgarden
            .import(
                &ufo2,
                &greek_glyphs,
                &greek_set_name,
                &Name::new("Bold").unwrap(),
            )
            .unwrap();

        let tempdir = tempfile::TempDir::new().unwrap();
        let fg_path = tempdir.path().join("test2.fontgarden");
        fontgarden.save(&fg_path);
        let fontgarden2 = Fontgarden::from_path(&fg_path).unwrap();

        assert_eq!(fontgarden, fontgarden2);
    }

    #[test]
    fn roundtrip_big() {
        let tmp_path = Path::new(NOTO_TEMP);
        let mut fontgarden = Fontgarden::new();

        let ufo_paths = [
            "NotoSans-Bold.ufo",
            "NotoSans-Condensed.ufo",
            "NotoSans-CondensedBold.ufo",
            "NotoSans-CondensedLight.ufo",
            "NotoSans-CondensedSemiBold.ufo",
            "NotoSans-DisplayBold.ufo",
            "NotoSans-DisplayBoldCondensed.ufo",
            "NotoSans-DisplayCondensed.ufo",
            "NotoSans-DisplayLight.ufo",
            "NotoSans-DisplayLightCondensed.ufo",
            "NotoSans-DisplayRegular.ufo",
            "NotoSans-DisplaySemiBold.ufo",
            "NotoSans-DisplaySemiBoldCondensed.ufo",
            "NotoSans-Light.ufo",
            "NotoSans-Regular.ufo",
            "NotoSans-SemiBold.ufo",
        ];

        for ufo_path in ufo_paths {
            let font = norad::Font::load(tmp_path.join(ufo_path)).unwrap();
            let source_name = font
                .font_info
                .style_name
                .as_ref()
                .map(|v| Name::new(v).unwrap())
                .unwrap();
            let mut ufo_glyph_names: HashSet<Name> = font.iter_names().collect();

            // TODO: Make small inside-test list
            for set_path in ["Latin.txt", "Cyrillic.txt", "Greek.txt"] {
                let set_name = Name::new(set_path.split('.').next().unwrap()).unwrap();
                let set_list = crate::util::load_glyph_list(&tmp_path.join(set_path)).unwrap();

                fontgarden
                    .import(&font, &set_list, &set_name, &source_name)
                    .unwrap();
                ufo_glyph_names.retain(|n| !set_list.contains(n));
            }

            // Put remaining glyphs into default set.
            if !ufo_glyph_names.is_empty() {
                let set_name = Name::new("default").unwrap();
                fontgarden
                    .import(&font, &ufo_glyph_names, &set_name, &source_name)
                    .unwrap();
            }
        }

        // let tempdir = tempfile::TempDir::new().unwrap();
        // let fg_path = tempdir.path().join("test3.fontgarden");
        let fg_path = tmp_path.join("test3.fontgarden");
        fontgarden.save(&fg_path);
        let fontgarden2 = Fontgarden::from_path(&fg_path).unwrap();

        assert_eq!(fontgarden, fontgarden2);
    }
}
