use crate::model::{PatternSpec, StylePalette, StyleSpec};
use crate::patterns::CustomPatternDefinition;
use anyhow::{Context, Result};
use serde::Deserialize;
use std::path::{Path, PathBuf};

const CONFIG_FILE: &str = "config.toml";
const DEFAULT_PROJECT_CONFIG_FILE: &str = ".herdr-flash.toml";

/// User-editable global Herdr Flash configuration file.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
struct GlobalConfigFile {
    #[serde(default)]
    project: ProjectConfig,
    #[serde(default)]
    flash: FlashConfig,
    #[serde(default)]
    colors: ColorsConfig,
    #[serde(default)]
    patterns: Vec<PatternConfigEntry>,
}

/// `[colors]` overrides, one optional entry per picker style.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
struct ColorsConfig {
    unmatched: Option<StyleOverride>,
    #[serde(rename = "match")]
    matched: Option<StyleOverride>,
    label: Option<StyleOverride>,
    selection: Option<StyleOverride>,
    cursor: Option<StyleOverride>,
}

/// One style override: `fg`/`bg` take `"#rrggbb"` or `"none"` (clear the channel), `bold` a bool.
/// Omitted keys keep the default; invalid values warn and keep the default.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
struct StyleOverride {
    fg: Option<String>,
    bg: Option<String>,
    bold: Option<bool>,
}

/// Flash-mode behaviour from global config.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
struct FlashConfig {
    #[serde(default = "default_flash_exit_on_yank")]
    exit_on_yank: bool,
}

impl Default for FlashConfig {
    fn default() -> Self {
        Self {
            exit_on_yank: default_flash_exit_on_yank(),
        }
    }
}

fn default_flash_exit_on_yank() -> bool {
    true
}

/// Project-local pattern discovery settings from global config.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
struct ProjectConfig {
    #[serde(default = "default_project_patterns_enabled")]
    patterns: bool,
    #[serde(default = "default_project_pattern_files")]
    pattern_files: Vec<String>,
}

impl Default for ProjectConfig {
    fn default() -> Self {
        Self {
            patterns: default_project_patterns_enabled(),
            pattern_files: default_project_pattern_files(),
        }
    }
}

/// One custom regex pattern loaded from user or project configuration.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
struct PatternConfigEntry {
    name: String,
    regex: String,
    #[serde(default = "default_custom_priority")]
    priority: u16,
}

fn default_custom_priority() -> u16 {
    25
}

fn default_project_patterns_enabled() -> bool {
    true
}

fn default_project_pattern_files() -> Vec<String> {
    vec![DEFAULT_PROJECT_CONFIG_FILE.to_string()]
}

/// Whether a flash yank closes the picker (default) or leaves it open for the next grab.
///
/// Read in the action process — the picker pane does not inherit HERDR_PLUGIN_CONFIG_DIR — and
/// carried to the picker inside the snapshot, the same way custom patterns travel.
pub fn resolve_flash_exit_on_yank() -> bool {
    load_global_config().map_or(true, |config| config.flash.exit_on_yank)
}

/// Picker colors: defaults overridden per style by `[colors]`. Read in the action process and
/// carried in the snapshot, like `exit_on_yank`.
pub fn resolve_palette() -> StylePalette {
    load_global_config().map_or_else(
        |_| StylePalette::default(),
        |config| apply_colors(config.colors),
    )
}

fn apply_colors(colors: ColorsConfig) -> StylePalette {
    let mut palette = StylePalette::default();
    apply_style(&mut palette.unmatched, colors.unmatched, "unmatched");
    apply_style(&mut palette.matched, colors.matched, "match");
    apply_style(&mut palette.label, colors.label, "label");
    apply_style(&mut palette.selection, colors.selection, "selection");
    apply_style(&mut palette.cursor, colors.cursor, "cursor");
    palette
}

fn apply_style(spec: &mut StyleSpec, over: Option<StyleOverride>, name: &str) {
    let Some(over) = over else { return };
    apply_channel(&mut spec.fg, over.fg, name, "fg");
    apply_channel(&mut spec.bg, over.bg, name, "bg");
    if let Some(bold) = over.bold {
        spec.bold = bold;
    }
}

fn apply_channel(channel: &mut Option<[u8; 3]>, value: Option<String>, name: &str, key: &str) {
    match value.as_deref() {
        None => {}
        Some("none") => *channel = None,
        Some(hex) => match parse_hex_color(hex) {
            Some(rgb) => *channel = Some(rgb),
            None => eprintln!(
                "Herdr Flash: ignoring [colors] {name}.{key} = {hex:?}; expected \"#rrggbb\" or \"none\""
            ),
        },
    }
}

/// Parses `#rrggbb` (case-insensitive). Anything else is rejected.
fn parse_hex_color(value: &str) -> Option<[u8; 3]> {
    let hex = value.strip_prefix('#')?;
    if hex.len() != 6 {
        return None;
    }
    let parse = |range| u8::from_str_radix(hex.get(range)?, 16).ok();
    Some([parse(0..2)?, parse(2..4)?, parse(4..6)?])
}

/// Resolves picker pattern specs before launching the temporary picker pane.
pub fn resolve_pattern_specs(focused_pane_cwd: Option<&Path>) -> Vec<PatternSpec> {
    match try_resolve_pattern_specs(focused_pane_cwd) {
        Ok(patterns) => patterns,
        Err(error) => {
            eprintln!("Herdr Flash: failed to load custom patterns: {error:#}");
            Vec::new()
        }
    }
}

/// Compiles snapshot-provided custom pattern specs, ignoring invalid entries.
pub fn compile_pattern_specs(specs: &[PatternSpec]) -> Vec<CustomPatternDefinition> {
    specs
        .iter()
        .filter_map(|spec| {
            CustomPatternDefinition::compile(spec.name.clone(), spec.priority, &spec.regex)
                .inspect_err(|error| {
                    eprintln!(
                        "Herdr Flash: ignoring invalid pattern {}: {error}",
                        spec.name
                    );
                })
                .ok()
        })
        .collect()
}

fn try_resolve_pattern_specs(focused_pane_cwd: Option<&Path>) -> Result<Vec<PatternSpec>> {
    let global_config = load_global_config()?;
    let mut specs = Vec::new();

    if global_config.project.patterns {
        if let Some(cwd) = focused_pane_cwd {
            specs.extend(load_project_pattern_specs(
                cwd,
                &global_config.project.pattern_files,
            ));
        }
    }
    specs.extend(entries_to_specs(global_config.patterns));
    Ok(specs)
}

fn load_global_config() -> Result<GlobalConfigFile> {
    let Some(config_dir) = global_config_dir()? else {
        return Ok(GlobalConfigFile::default());
    };
    load_config_file(&config_dir.join(CONFIG_FILE)).map(|config| config.unwrap_or_default())
}

fn global_config_dir() -> Result<Option<PathBuf>> {
    if let Some(path) = std::env::var_os("HERDR_PLUGIN_CONFIG_DIR") {
        return Ok(Some(PathBuf::from(path)));
    }
    Ok(None)
}

fn load_project_pattern_specs(cwd: &Path, pattern_files: &[String]) -> Vec<PatternSpec> {
    let Some(git_root) = find_git_root(cwd) else {
        return Vec::new();
    };

    for dir in ancestors_until(cwd, &git_root) {
        for file_name in pattern_files {
            let path = dir.join(file_name);
            match load_config_file(&path) {
                Ok(Some(config)) => return entries_to_specs(config.patterns),
                Ok(None) => continue,
                Err(error) => {
                    eprintln!(
                        "Herdr Flash: ignoring project pattern config {}: {error:#}",
                        path.display()
                    );
                    return Vec::new();
                }
            }
        }
    }

    Vec::new()
}

fn load_config_file(path: &Path) -> Result<Option<GlobalConfigFile>> {
    let content = match std::fs::read_to_string(path) {
        Ok(content) => content,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => {
            return Err(error).with_context(|| format!("failed to read {}", path.display()))
        }
    };
    toml::from_str(&content)
        .map(Some)
        .with_context(|| format!("failed to parse {}", path.display()))
}

fn entries_to_specs(entries: Vec<PatternConfigEntry>) -> Vec<PatternSpec> {
    entries
        .into_iter()
        .map(|entry| PatternSpec {
            name: entry.name,
            regex: entry.regex,
            priority: entry.priority,
        })
        .collect()
}

fn find_git_root(cwd: &Path) -> Option<PathBuf> {
    cwd.ancestors()
        .find(|ancestor| ancestor.join(".git").exists())
        .map(Path::to_path_buf)
}

fn ancestors_until<'a>(cwd: &'a Path, root: &'a Path) -> impl Iterator<Item = &'a Path> {
    cwd.ancestors()
        .take_while(move |ancestor| ancestor.starts_with(root))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hex_colors_parse_and_reject_garbage() {
        assert_eq!(parse_hex_color("#ff007c"), Some([0xff, 0x00, 0x7c]));
        assert_eq!(parse_hex_color("#FF007C"), Some([0xff, 0x00, 0x7c]));
        assert_eq!(parse_hex_color("ff007c"), None);
        assert_eq!(parse_hex_color("#f07"), None);
        assert_eq!(parse_hex_color("#ff007g"), None);
    }

    #[test]
    fn color_overrides_layer_onto_the_default_palette() {
        let colors: ColorsConfig = toml::from_str(
            r##"
            match = { fg = "#010203", bg = "none" }
            label = { bold = false }
            unmatched = { fg = "not-a-color" }
            "##,
        )
        .unwrap();

        let palette = apply_colors(colors);
        let defaults = StylePalette::default();

        // match: fg overridden, bg explicitly cleared with "none".
        assert_eq!(palette.matched.fg, Some([1, 2, 3]));
        assert_eq!(palette.matched.bg, None);
        // label: only bold flipped; colors keep their defaults.
        assert_eq!(palette.label.bg, defaults.label.bg);
        assert!(!palette.label.bold);
        // invalid hex warns and keeps the default rather than blanking the style.
        assert_eq!(palette.unmatched.fg, defaults.unmatched.fg);
        // untouched styles stay identical.
        assert_eq!(palette.cursor, defaults.cursor);
        assert_eq!(palette.selection, defaults.selection);
    }

    #[test]
    fn config_file_uses_default_priority_and_project_settings() {
        let dir = tempfile_dir("config-default-priority");
        let path = dir.join(CONFIG_FILE);
        std::fs::write(
            &path,
            r#"[[patterns]]
name = "ticket"
regex = "ABC-(?<match>[0-9]+)"
"#,
        )
        .unwrap();

        let config = load_config_file(&path).unwrap().unwrap();
        let specs = entries_to_specs(config.patterns);

        assert_eq!(specs.len(), 1);
        assert_eq!(specs[0].name, "ticket");
        assert_eq!(specs[0].priority, 25);
        assert!(config.project.patterns);
        assert_eq!(config.project.pattern_files, vec![".herdr-flash.toml"]);
    }

    #[test]
    fn project_config_is_discovered_up_to_git_root() {
        let root = tempfile_dir("project-discovery");
        std::fs::create_dir(root.join(".git")).unwrap();
        let nested = root.join("a/b");
        std::fs::create_dir_all(&nested).unwrap();
        std::fs::write(
            root.join(DEFAULT_PROJECT_CONFIG_FILE),
            r#"[[patterns]]
name = "project"
regex = "PROJECT-[0-9]+"
"#,
        )
        .unwrap();

        let specs = load_project_pattern_specs(&nested, &[DEFAULT_PROJECT_CONFIG_FILE.to_string()]);

        assert_eq!(specs.len(), 1);
        assert_eq!(specs[0].name, "project");
    }

    #[test]
    fn invalid_project_config_is_ignored() {
        let root = tempfile_dir("invalid-project");
        std::fs::create_dir(root.join(".git")).unwrap();
        std::fs::write(root.join(DEFAULT_PROJECT_CONFIG_FILE), "not toml =").unwrap();

        let specs = load_project_pattern_specs(&root, &[DEFAULT_PROJECT_CONFIG_FILE.to_string()]);

        assert!(specs.is_empty());
    }

    fn tempfile_dir(name: &str) -> PathBuf {
        let path = std::env::temp_dir().join(format!("herdr-flash-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&path);
        std::fs::create_dir_all(&path).unwrap();
        path
    }
}
