use std::{fs, io, path::Path};

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum ToolbarPosition {
    #[default]
    Top,
    Bottom,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Accent {
    #[default]
    Mint,
    Iris,
    Amber,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Settings {
    pub position: ToolbarPosition,
    pub accent: Accent,
    pub motion_ms: u32,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            position: ToolbarPosition::Top,
            accent: Accent::Mint,
            motion_ms: 220,
        }
    }
}

impl Settings {
    pub fn load(profile: &Path) -> Self {
        let mut settings = Self::default();
        let Ok(source) = fs::read_to_string(profile.join("browser-studio.ini")) else {
            return settings;
        };
        if !source.lines().any(|line| line == "version=1") {
            return settings;
        }
        for line in source.lines() {
            let Some((key, value)) = line.split_once('=') else {
                continue;
            };
            match (key, value) {
                ("position", "top") => settings.position = ToolbarPosition::Top,
                ("position", "bottom") => settings.position = ToolbarPosition::Bottom,
                ("accent", "mint") => settings.accent = Accent::Mint,
                ("accent", "iris") => settings.accent = Accent::Iris,
                ("accent", "amber") => settings.accent = Accent::Amber,
                ("motion_ms", _) => {
                    if let Ok(ms @ (0 | 180 | 220 | 250)) = value.parse() {
                        settings.motion_ms = ms;
                    }
                }
                _ => {}
            }
        }
        settings
    }

    pub fn save(self, profile: &Path) -> io::Result<()> {
        let position = match self.position {
            ToolbarPosition::Top => "top",
            ToolbarPosition::Bottom => "bottom",
        };
        let accent = match self.accent {
            Accent::Mint => "mint",
            Accent::Iris => "iris",
            Accent::Amber => "amber",
        };
        fs::create_dir_all(profile)?;
        let temporary = profile.join(format!("browser-studio.{}.tmp", std::process::id()));
        fs::write(
            &temporary,
            format!(
                "version=1\nposition={position}\naccent={accent}\nmotion_ms={}\n",
                self.motion_ms
            ),
        )?;
        let result = fs::rename(&temporary, profile.join("browser-studio.ini"));
        if result.is_err() {
            let _ = fs::remove_file(&temporary);
        }
        result
    }
}
