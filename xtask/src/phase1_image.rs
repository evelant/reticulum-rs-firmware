use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
};

pub(crate) const CHIP: &str = "esp32s3";
pub(crate) const FLASH_SIZE: &str = "8mb";
pub(crate) const FLASH_MODE: &str = "qio";
pub(crate) const FLASH_FREQUENCY: &str = "80mhz";
pub(crate) const XTAL_FREQUENCY: &str = "40mhz";
pub(crate) const MINIMUM_CHIP_REVISION: &str = "0.0";
pub(crate) const IMAGE_FORMAT: &str = "esp-idf";
pub(crate) const CONFIG_POLICY: &str = "explicit-image-settings-plus-empty-local-global-config-v1";

#[derive(Debug)]
pub(crate) struct OfflineEspflashContext {
    workdir: PathBuf,
    home: PathBuf,
    xdg_config_home: PathBuf,
    tmpdir: PathBuf,
}

impl OfflineEspflashContext {
    pub(crate) fn create(parent: &Path) -> Result<Self, String> {
        let root = parent.join("espflash-context");
        let workdir = root.join("work");
        let home = root.join("home");
        let xdg_config_home = root.join("xdg-config");
        let tmpdir = root.join("tmp");
        fs::create_dir(&root).map_err(|error| {
            format!(
                "could not create isolated espflash root {}: {error}",
                root.display()
            )
        })?;
        for directory in [&workdir, &home, &xdg_config_home, &tmpdir] {
            fs::create_dir(directory).map_err(|error| {
                format!(
                    "could not create isolated espflash directory {}: {error}",
                    directory.display()
                )
            })?;
        }
        Ok(Self {
            workdir,
            home,
            xdg_config_home,
            tmpdir,
        })
    }

    pub(crate) fn workdir(&self) -> &Path {
        &self.workdir
    }

    pub(crate) fn environment(&self) -> BTreeMap<String, String> {
        BTreeMap::from([
            ("ESPFLASH_SKIP_UPDATE_CHECK".to_owned(), "true".to_owned()),
            ("HOME".to_owned(), self.home.to_string_lossy().into_owned()),
            (
                "XDG_CONFIG_HOME".to_owned(),
                self.xdg_config_home.to_string_lossy().into_owned(),
            ),
            (
                "TMPDIR".to_owned(),
                self.tmpdir.to_string_lossy().into_owned(),
            ),
        ])
    }
}

pub(crate) fn save_image_arguments(elf: &Path, image: &Path) -> Vec<String> {
    [
        "save-image".to_owned(),
        "--chip".to_owned(),
        CHIP.to_owned(),
        "--flash-size".to_owned(),
        FLASH_SIZE.to_owned(),
        "--flash-mode".to_owned(),
        FLASH_MODE.to_owned(),
        "--flash-freq".to_owned(),
        FLASH_FREQUENCY.to_owned(),
        "--xtal-freq".to_owned(),
        XTAL_FREQUENCY.to_owned(),
        "--min-chip-rev".to_owned(),
        MINIMUM_CHIP_REVISION.to_owned(),
        "--format".to_owned(),
        IMAGE_FORMAT.to_owned(),
        "--merge".to_owned(),
        "--skip-padding".to_owned(),
        elf.to_string_lossy().into_owned(),
        image.to_string_lossy().into_owned(),
    ]
    .into_iter()
    .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{env, time::SystemTime};

    #[test]
    fn context_is_empty_and_image_arguments_pin_every_effective_cli_setting() {
        let parent = env::temp_dir().join(format!(
            "phase1-image-test-{}-{:?}",
            std::process::id(),
            SystemTime::now()
        ));
        fs::create_dir(&parent).unwrap();
        let context = OfflineEspflashContext::create(&parent).unwrap();
        assert_eq!(fs::read_dir(context.workdir()).unwrap().count(), 0);
        assert!(!context.workdir().join("espflash.toml").exists());
        assert!(
            !context
                .workdir()
                .parent()
                .unwrap()
                .join("espflash.toml")
                .exists()
        );
        let environment = context.environment();
        assert_eq!(environment["ESPFLASH_SKIP_UPDATE_CHECK"], "true");
        assert!(environment.contains_key("HOME"));
        assert!(environment.contains_key("XDG_CONFIG_HOME"));
        assert!(environment.contains_key("TMPDIR"));
        assert_eq!(
            save_image_arguments(Path::new("firmware.elf"), Path::new("image.bin")),
            [
                "save-image",
                "--chip",
                "esp32s3",
                "--flash-size",
                "8mb",
                "--flash-mode",
                "qio",
                "--flash-freq",
                "80mhz",
                "--xtal-freq",
                "40mhz",
                "--min-chip-rev",
                "0.0",
                "--format",
                "esp-idf",
                "--merge",
                "--skip-padding",
                "firmware.elf",
                "image.bin",
            ]
            .into_iter()
            .map(str::to_owned)
            .collect::<Vec<_>>()
        );
        fs::remove_dir_all(parent).unwrap();
    }
}
