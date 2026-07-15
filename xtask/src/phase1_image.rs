use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
};

use md5::{Digest as _, Md5};

pub(crate) const CHIP: &str = "esp32s3";
pub(crate) const FLASH_SIZE: &str = "8mb";
pub(crate) const FLASH_MODE: &str = "dio";
pub(crate) const FLASH_FREQUENCY: &str = "80mhz";
pub(crate) const XTAL_FREQUENCY: &str = "40mhz";
pub(crate) const MINIMUM_CHIP_REVISION: &str = "0.0";
pub(crate) const IMAGE_FORMAT: &str = "esp-idf";
pub(crate) const CONFIG_POLICY: &str = "explicit-image-settings-plus-empty-local-global-config-v1";

const ESP_IMAGE_MAGIC: u8 = 0xe9;
const DIO_IMAGE_HEADER_MODE: u8 = 0x02;
const EIGHT_MIB_80_MHZ_IMAGE_HEADER_CONFIG: u8 = 0x3f;
const PARTITION_TABLE_OFFSET: usize = 0x8000;
const PARTITION_TABLE_BYTES: usize = 0x1000;
const PARTITION_ENTRY_BYTES: usize = 32;
const PARTITION_MAGIC: [u8; 2] = [0xaa, 0x50];
const PARTITION_MD5_MAGIC: [u8; 16] = [
    0xeb, 0xeb, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff,
];
const PARTITION_END_MARKER: [u8; PARTITION_ENTRY_BYTES] = [0xff; PARTITION_ENTRY_BYTES];
const PARTITION_TYPE_APP: u8 = 0x00;
const PARTITION_SUBTYPE_FACTORY: u8 = 0x00;
const APP_PARTITION_ALIGNMENT: usize = 0x10000;
const FLASH_BYTES: usize = 8 * 1024 * 1024;

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

pub(crate) fn validate_merged_image(path: &Path) -> Result<(), String> {
    let image = fs::read(path)
        .map_err(|error| format!("could not read merged image {}: {error}", path.display()))?;
    validate_merged_image_bytes(&image)
}

fn validate_merged_image_bytes(image: &[u8]) -> Result<(), String> {
    validate_image_header(image, 0, "bootloader")?;
    let factory_offset = find_factory_app_offset(image)?;
    validate_image_header(image, factory_offset, "factory application")
}

fn validate_image_header(image: &[u8], offset: usize, label: &str) -> Result<(), String> {
    let header = image.get(offset..offset + 4).ok_or_else(|| {
        format!("merged image is truncated before the {label} header at 0x{offset:08x}")
    })?;
    if header[0] != ESP_IMAGE_MAGIC {
        return Err(format!(
            "merged image {label} header at 0x{offset:08x} has magic 0x{:02x}, expected 0x{ESP_IMAGE_MAGIC:02x}",
            header[0]
        ));
    }
    if header[2] != DIO_IMAGE_HEADER_MODE {
        return Err(format!(
            "merged image {label} header at 0x{offset:08x} has flash mode 0x{:02x}, expected DIO (0x{DIO_IMAGE_HEADER_MODE:02x})",
            header[2]
        ));
    }
    if header[3] != EIGHT_MIB_80_MHZ_IMAGE_HEADER_CONFIG {
        return Err(format!(
            "merged image {label} header at 0x{offset:08x} has flash size/frequency 0x{:02x}, expected 8 MiB/80 MHz (0x{EIGHT_MIB_80_MHZ_IMAGE_HEADER_CONFIG:02x})",
            header[3]
        ));
    }
    Ok(())
}

fn find_factory_app_offset(image: &[u8]) -> Result<usize, String> {
    let table_end = PARTITION_TABLE_OFFSET + PARTITION_TABLE_BYTES;
    let table = image
        .get(PARTITION_TABLE_OFFSET..table_end)
        .ok_or_else(|| {
            format!(
                "merged image is truncated before the complete partition table at 0x{PARTITION_TABLE_OFFSET:08x}"
            )
        })?;
    let mut factory_offset = None;
    let mut checksum = Md5::new();
    let mut checksum_seen = false;
    let mut end_seen = false;
    for (index, entry) in table.chunks_exact(PARTITION_ENTRY_BYTES).enumerate() {
        if entry == PARTITION_END_MARKER {
            let trailing = &table[index * PARTITION_ENTRY_BYTES..];
            if !trailing.iter().all(|byte| *byte == 0xff) {
                return Err("partition table contains data after its end marker".to_owned());
            }
            end_seen = true;
            break;
        }
        if entry.starts_with(&PARTITION_MD5_MAGIC) {
            if checksum_seen {
                return Err("partition table contains more than one MD5 record".to_owned());
            }
            let computed: [u8; 16] = checksum.clone().finalize().into();
            if entry[16..32] != computed {
                return Err("partition-table MD5 record does not match its entries".to_owned());
            }
            checksum_seen = true;
            continue;
        }
        if checksum_seen {
            return Err("partition table contains an entry after its MD5 record".to_owned());
        }
        let magic = [entry[0], entry[1]];
        if magic != PARTITION_MAGIC {
            return Err(format!(
                "partition-table entry {index} has invalid magic 0x{:02x}{:02x}",
                entry[1], entry[0]
            ));
        }
        if entry[2] == PARTITION_TYPE_APP && entry[3] == PARTITION_SUBTYPE_FACTORY {
            let offset = u32::from_le_bytes(entry[4..8].try_into().expect("fixed-size slice"));
            let offset = usize::try_from(offset)
                .map_err(|_| "factory application offset does not fit usize".to_owned())?;
            let size = u32::from_le_bytes(entry[8..12].try_into().expect("fixed-size slice"));
            let size = usize::try_from(size)
                .map_err(|_| "factory application size does not fit usize".to_owned())?;
            let end = offset
                .checked_add(size)
                .ok_or_else(|| "factory application range overflows usize".to_owned())?;
            if offset < table_end
                || offset % APP_PARTITION_ALIGNMENT != 0
                || size == 0
                || end > FLASH_BYTES
            {
                return Err(format!(
                    "factory application has invalid flash range 0x{offset:08x}..0x{end:08x}"
                ));
            }
            if factory_offset.replace(offset).is_some() {
                return Err("partition table contains more than one factory application".to_owned());
            }
        }
        checksum.update(entry);
    }
    if !checksum_seen {
        return Err("partition table does not contain an MD5 record".to_owned());
    }
    if !end_seen {
        return Err("partition table does not contain a complete end marker".to_owned());
    }
    factory_offset
        .ok_or_else(|| "partition table does not contain a factory application".to_owned())
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
                "dio",
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

    fn valid_merged_image() -> Vec<u8> {
        let mut image = vec![0xff; 0x10004];
        image[0..4].copy_from_slice(&[
            ESP_IMAGE_MAGIC,
            3,
            DIO_IMAGE_HEADER_MODE,
            EIGHT_MIB_80_MHZ_IMAGE_HEADER_CONFIG,
        ]);
        let entry = &mut image[PARTITION_TABLE_OFFSET..PARTITION_TABLE_OFFSET + 32];
        entry[0..2].copy_from_slice(&PARTITION_MAGIC);
        entry[2] = PARTITION_TYPE_APP;
        entry[3] = PARTITION_SUBTYPE_FACTORY;
        entry[4..8].copy_from_slice(&0x0001_0000_u32.to_le_bytes());
        entry[8..12].copy_from_slice(&0x0001_0000_u32.to_le_bytes());
        entry[12..19].copy_from_slice(b"factory");
        let digest: [u8; 16] = Md5::digest(entry).into();
        let md5_entry = &mut image[PARTITION_TABLE_OFFSET + 32..PARTITION_TABLE_OFFSET + 64];
        md5_entry[0..16].copy_from_slice(&PARTITION_MD5_MAGIC);
        md5_entry[16..32].copy_from_slice(&digest);
        image[0x10000..0x10004].copy_from_slice(&[
            ESP_IMAGE_MAGIC,
            5,
            DIO_IMAGE_HEADER_MODE,
            EIGHT_MIB_80_MHZ_IMAGE_HEADER_CONFIG,
        ]);
        image
    }

    fn refresh_partition_checksum(image: &mut [u8]) {
        let entry = &image[PARTITION_TABLE_OFFSET..PARTITION_TABLE_OFFSET + 32];
        let digest: [u8; 16] = Md5::digest(entry).into();
        image[PARTITION_TABLE_OFFSET + 48..PARTITION_TABLE_OFFSET + 64].copy_from_slice(&digest);
    }

    #[test]
    fn merged_image_validator_accepts_dio_bootloader_and_factory_app() {
        let image = valid_merged_image();
        validate_merged_image_bytes(&image).unwrap();
        let factory_offset = find_factory_app_offset(&image).unwrap();
        assert_eq!(factory_offset, 0x10000);
    }

    #[test]
    fn merged_image_validator_rejects_qio_in_either_header() {
        for offset in [2, 0x10002] {
            let mut image = valid_merged_image();
            image[offset] = 0x00;
            assert!(
                validate_merged_image_bytes(&image)
                    .unwrap_err()
                    .contains("expected DIO")
            );
        }
    }

    #[test]
    fn merged_image_validator_rejects_wrong_flash_config_and_magic() {
        for (offset, value, expected) in [
            (3, 0x00, "expected 8 MiB/80 MHz"),
            (0x10003, 0x00, "expected 8 MiB/80 MHz"),
            (0, 0x00, "expected 0xe9"),
            (0x10000, 0x00, "expected 0xe9"),
        ] {
            let mut image = valid_merged_image();
            image[offset] = value;
            assert!(
                validate_merged_image_bytes(&image)
                    .unwrap_err()
                    .contains(expected)
            );
        }
    }

    #[test]
    fn merged_image_validator_rejects_truncation_missing_factory_and_bad_range() {
        assert!(validate_merged_image_bytes(&[0xff; PARTITION_TABLE_OFFSET]).is_err());

        let mut image = valid_merged_image();
        image[PARTITION_TABLE_OFFSET + 3] = 1;
        refresh_partition_checksum(&mut image);
        assert!(
            find_factory_app_offset(&image)
                .unwrap_err()
                .contains("does not contain a factory application")
        );

        let mut image = valid_merged_image();
        image[PARTITION_TABLE_OFFSET + 4..PARTITION_TABLE_OFFSET + 8]
            .copy_from_slice(&0x0000_8000_u32.to_le_bytes());
        assert!(
            validate_merged_image_bytes(&image)
                .unwrap_err()
                .contains("invalid flash range")
        );
    }

    #[test]
    fn merged_image_validator_rejects_partition_checksum_and_terminator_corruption() {
        let mut image = valid_merged_image();
        image[PARTITION_TABLE_OFFSET + 63] ^= 1;
        assert!(
            validate_merged_image_bytes(&image)
                .unwrap_err()
                .contains("MD5 record does not match")
        );

        let mut image = valid_merged_image();
        image[PARTITION_TABLE_OFFSET + 64] = 0;
        assert!(
            validate_merged_image_bytes(&image)
                .unwrap_err()
                .contains("entry after its MD5 record")
        );

        let mut image = valid_merged_image();
        image[PARTITION_TABLE_OFFSET + 96] = 0;
        assert!(
            validate_merged_image_bytes(&image)
                .unwrap_err()
                .contains("data after its end marker")
        );
    }
}
