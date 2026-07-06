use anyhow::{ensure, Context, Result};
use serde::Deserialize;
use std::path::Path;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Theme {
    Auto,
    Light,
    Dark,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum DesktopFilter {
    Current,
    All,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum DialogMonitor {
    Mouse,
    Primary,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    pub theme: Theme,
    pub scale: f32,
    pub show_selected_name: bool,
    pub desktop_filter: DesktopFilter,
    pub restore_minimized: bool,
    pub dialog_monitor: DialogMonitor,
    pub dialog_delay_ms: u32,
    pub check_updates: bool,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            theme: Theme::Auto,
            scale: 1.0,
            show_selected_name: true,
            desktop_filter: DesktopFilter::Current,
            restore_minimized: true,
            dialog_monitor: DialogMonitor::Mouse,
            dialog_delay_ms: 150,
            check_updates: true,
        }
    }
}

impl Config {
    pub fn parse(text: &str) -> Result<Self> {
        let config: Config = toml::from_str(text).context("invalid config.toml")?;
        ensure!(
            (0.25..=4.0).contains(&config.scale),
            "scale must be between 0.25 and 4.0, got {}",
            config.scale
        );
        Ok(config)
    }

    /// Missing file → defaults. Unreadable or invalid file → error.
    pub fn load(path: &Path) -> Result<Self> {
        match std::fs::read_to_string(path) {
            Ok(text) => Self::parse(&text),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(e) => Err(e).with_context(|| format!("reading {}", path.display())),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_file_is_defaults() {
        assert_eq!(Config::parse("").unwrap(), Config::default());
    }

    #[test]
    fn missing_file_is_defaults() {
        let cfg = Config::load(Path::new("/nonexistent/config.toml")).unwrap();
        assert_eq!(cfg, Config::default());
    }

    #[test]
    fn partial_file_overrides_only_given_keys() {
        let cfg = Config::parse("theme = \"dark\"\nscale = 1.5\n").unwrap();
        assert_eq!(cfg.theme, Theme::Dark);
        assert_eq!(cfg.scale, 1.5);
        let defaults = Config::default();
        assert_eq!(cfg.show_selected_name, defaults.show_selected_name);
        assert_eq!(cfg.desktop_filter, defaults.desktop_filter);
        assert_eq!(cfg.restore_minimized, defaults.restore_minimized);
        assert_eq!(cfg.dialog_monitor, defaults.dialog_monitor);
        assert_eq!(cfg.dialog_delay_ms, defaults.dialog_delay_ms);
    }

    #[test]
    fn all_keys_parse() {
        let cfg = Config::parse(
            r#"
            theme = "light"
            scale = 2.0
            show_selected_name = false
            desktop_filter = "all"
            restore_minimized = false
            dialog_monitor = "primary"
            dialog_delay_ms = 300
            check_updates = false
            "#,
        )
        .unwrap();
        assert_eq!(cfg.theme, Theme::Light);
        assert_eq!(cfg.scale, 2.0);
        assert!(!cfg.show_selected_name);
        assert_eq!(cfg.desktop_filter, DesktopFilter::All);
        assert!(!cfg.restore_minimized);
        assert_eq!(cfg.dialog_monitor, DialogMonitor::Primary);
        assert_eq!(cfg.dialog_delay_ms, 300);
        assert!(!cfg.check_updates);
    }

    #[test]
    fn bad_enum_value_is_an_error() {
        assert!(Config::parse("theme = \"blue\"").is_err());
    }

    #[test]
    fn unknown_key_is_an_error() {
        assert!(Config::parse("them = \"dark\"").is_err());
    }

    #[test]
    fn bad_syntax_is_an_error() {
        assert!(Config::parse("theme = ").is_err());
    }

    #[test]
    fn out_of_range_scale_is_an_error() {
        assert!(Config::parse("scale = 0.0").is_err());
        assert!(Config::parse("scale = 100.0").is_err());
    }

    #[test]
    fn example_file_is_all_comments_and_matches_defaults() {
        let example = include_str!("../config.example.toml");
        // As shipped (everything commented out) it must parse to the defaults.
        assert_eq!(Config::parse(example).unwrap(), Config::default());
        // With every setting line uncommented it must still equal the defaults,
        // so the documented values can never drift from the code.
        let uncommented: String = example
            .lines()
            .map(|line| {
                line.strip_prefix('#')
                    .filter(|rest| rest.starts_with(|c: char| c.is_ascii_lowercase()))
                    .unwrap_or(line)
            })
            .collect::<Vec<_>>()
            .join("\n");
        assert_eq!(Config::parse(&uncommented).unwrap(), Config::default());
    }
}
