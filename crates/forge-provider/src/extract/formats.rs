//! Readers of each archive format feeding the [`Sink`]. Every entry is classified
//! (file, directory, or something refused: links, devices, reparse points, encrypted
//! data) and its name validated before anything is written; zip and 7z are fully
//! scanned first (their index is cheap to read), so a malicious entry anywhere rejects
//! the archive before the first byte reaches the disk.

use std::fs::File;
use std::io::{self, BufReader, Read};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, SystemTime};

use forge_core::error::{ForgeError, IoResultExt, Result};

use super::Sink;
use super::entry::{sanitize, unsafe_entry};

/// `FILE_ATTRIBUTE_REPARSE_POINT` in the DOS/Windows attribute bits.
const ATTR_REPARSE_POINT: u32 = 0x400;
/// 7-Zip marks Windows attributes that carry a Unix mode in the high 16 bits.
const ATTR_UNIX_EXTENSION: u32 = 0x8000;
const S_IFMT: u32 = 0o170_000;
const S_IFREG: u32 = 0o100_000;
const S_IFDIR: u32 = 0o040_000;
const S_IFLNK: u32 = 0o120_000;
/// 100 ns intervals between 1601-01-01 (FILETIME) and 1970-01-01.
const FILETIME_UNIX_EPOCH: u64 = 116_444_736_000_000_000;

fn corrupt(archive: &Path, e: impl std::fmt::Display) -> ForgeError {
    ForgeError::VerificationFailed(format!("{} is not a valid archive: {e}", archive.display()))
}

/// Refuses Unix file types other than regular files and directories.
fn check_unix_mode(name: &str, mode: u32) -> Result<()> {
    match mode & S_IFMT {
        0 | S_IFREG | S_IFDIR => Ok(()),
        S_IFLNK => Err(unsafe_entry(name, "symbolic link")),
        _ => Err(unsafe_entry(name, "special file (device, fifo or socket)")),
    }
}

fn filetime_to_system(filetime: u64) -> Option<SystemTime> {
    let since_unix = filetime.checked_sub(FILETIME_UNIX_EPOCH)?;
    SystemTime::UNIX_EPOCH.checked_add(Duration::from_nanos(since_unix.checked_mul(100)?))
}

// ---------------------------------------------------------------- zip

struct ZipPlan {
    index: usize,
    name: String,
    path: Option<PathBuf>,
    is_dir: bool,
    size: u64,
}

/// Modification time of a zip entry: NTFS extra field (UTC, 100 ns), then the extended
/// timestamp field (UTC, seconds), then the DOS date, which has no time zone and is taken
/// as UTC.
fn zip_mtime<R: Read>(file: &zip::read::ZipFile<'_, R>) -> Option<SystemTime> {
    for field in file.extra_data_fields() {
        match field {
            zip::ExtraField::Ntfs(ntfs) => {
                if let Some(t) = filetime_to_system(ntfs.mtime()) {
                    return Some(t);
                }
            }
            zip::ExtraField::ExtendedTimestamp(ts) => {
                if let Some(secs) = ts.mod_time() {
                    return Some(SystemTime::UNIX_EPOCH + Duration::from_secs(u64::from(secs)));
                }
            }
            _ => {}
        }
    }
    let dos = file.last_modified()?;
    let civil = jiff::civil::DateTime::new(
        i16::try_from(dos.year()).ok()?,
        i8::try_from(dos.month()).ok()?,
        i8::try_from(dos.day()).ok()?,
        i8::try_from(dos.hour()).ok()?,
        i8::try_from(dos.minute()).ok()?,
        i8::try_from(dos.second()).ok()?,
        0,
    )
    .ok()?;
    let ts = civil.to_zoned(jiff::tz::TimeZone::UTC).ok()?.timestamp();
    Some(SystemTime::from(ts))
}

pub(super) fn extract_zip(archive: &Path, sink: &mut Sink<'_>) -> Result<()> {
    let file = File::open(archive).at("opening", archive)?;
    let mut zip = zip::ZipArchive::new(BufReader::new(file)).map_err(|e| corrupt(archive, e))?;

    let mut plan = Vec::with_capacity(zip.len());
    let mut declared = 0u64;
    for index in 0..zip.len() {
        let entry = zip.by_index_data(index).map_err(|e| corrupt(archive, e))?;
        let name = entry.name().map_err(|e| corrupt(archive, e))?.into_owned();
        if entry.name_raw().contains(&0) {
            return Err(unsafe_entry(&name, "contains a NUL character"));
        }
        if entry.encrypted() {
            return Err(unsafe_entry(&name, "encrypted entries are not supported"));
        }
        let attributes = entry.external_attributes();
        if entry.is_symlink() {
            return Err(unsafe_entry(&name, "symbolic link"));
        }
        check_unix_mode(&name, attributes >> 16)?;
        if entry.system() != zip::System::Unix && attributes & ATTR_REPARSE_POINT != 0 {
            return Err(unsafe_entry(&name, "reparse point (link or junction)"));
        }
        let path = sanitize(&name)?;
        let is_dir = entry.is_dir();
        let size = if is_dir { 0 } else { entry.size() };
        declared = declared.saturating_add(size);
        sink.check_declared(declared)?;
        plan.push(ZipPlan {
            index,
            name,
            path,
            is_dir,
            size,
        });
    }
    sink.set_totals(Some(plan.len() as u64), Some(declared));

    for item in plan {
        sink.begin_entry()?;
        let Some(path) = item.path else { continue };
        if item.is_dir {
            sink.dir(&path)?;
            continue;
        }
        let mut file = zip.by_index(item.index).map_err(|e| corrupt(archive, e))?;
        let mtime = zip_mtime(&file);
        sink.file(&item.name, &path, &mut file, Some(item.size), mtime)?;
    }
    Ok(())
}

// ---------------------------------------------------------------- 7z

/// Validated path of a 7z entry; `None` for the archive root and for anti-items.
///
/// Anti-items are deletion markers of update archives: 7-Zip deletes the path when it
/// extracts them. Here they are validated like any entry and then skipped, never acted on.
/// (sevenz-rust2 0.20's writer also flags every empty-stream entry as anti-item.)
fn check_7z_entry(entry: &sevenz_rust2::ArchiveEntry) -> Result<Option<PathBuf>> {
    let name = entry.name();
    if entry.has_windows_attributes {
        let attributes = entry.windows_attributes();
        if attributes & ATTR_REPARSE_POINT != 0 {
            return Err(unsafe_entry(name, "reparse point (link or junction)"));
        }
        if attributes & ATTR_UNIX_EXTENSION != 0 {
            check_unix_mode(name, attributes >> 16)?;
        }
    }
    let path = sanitize(name)?;
    Ok(path.filter(|_| !entry.is_anti_item()))
}

pub(super) fn extract_7z(archive: &Path, sink: &mut Sink<'_>) -> Result<()> {
    let mut reader = sevenz_rust2::ArchiveReader::open(archive, sevenz_rust2::Password::empty())
        .map_err(|e| corrupt(archive, e))?;

    let mut declared = 0u64;
    let entries = &reader.archive().files;
    for entry in entries {
        check_7z_entry(entry)?;
        if !entry.is_directory() && !entry.is_anti_item() {
            declared = declared.saturating_add(entry.size());
            sink.check_declared(declared)?;
        }
    }
    sink.set_totals(Some(entries.len() as u64), Some(declared));

    let mut failure = None;
    let walked = reader.for_each_entries(|entry, data| {
        let result = (|| {
            sink.begin_entry()?;
            let Some(path) = check_7z_entry(entry)? else {
                return Ok(());
            };
            if entry.is_directory() {
                return sink.dir(&path);
            }
            let mtime = entry
                .has_last_modified_date
                .then(|| SystemTime::from(entry.last_modified_date()));
            let size = entry.has_stream().then(|| entry.size()).or(Some(0));
            sink.file(entry.name(), &path, data, size, mtime)
        })();
        match result {
            Ok(()) => Ok(true),
            Err(e) => {
                failure = Some(e);
                Ok(false)
            }
        }
    });
    if let Some(e) = failure {
        return Err(e);
    }
    walked.map_err(|e| corrupt(archive, e))
}

// ---------------------------------------------------------------- tar

/// Counts the compressed bytes consumed, for progress of formats without an index.
pub(super) struct CountingReader<R> {
    inner: R,
    count: Arc<AtomicU64>,
}

impl<R: Read> Read for CountingReader<R> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let n = self.inner.read(buf)?;
        self.count.fetch_add(n as u64, Ordering::Relaxed);
        Ok(n)
    }
}

/// Which decompressor wraps the tar stream.
#[derive(Clone, Copy)]
pub(super) enum TarCompression {
    Gzip,
    Xz,
}

pub(super) fn extract_tar(
    archive: &Path,
    compression: TarCompression,
    sink: &mut Sink<'_>,
) -> Result<()> {
    let file = File::open(archive).at("opening", archive)?;
    let size = file.metadata().at("inspecting", archive)?.len();
    let count = Arc::new(AtomicU64::new(0));
    sink.set_stream_position(Arc::clone(&count), size);
    let counted = CountingReader {
        inner: BufReader::with_capacity(256 * 1024, file),
        count,
    };
    let stream: Box<dyn Read> = match compression {
        TarCompression::Gzip => Box::new(flate2::read::MultiGzDecoder::new(counted)),
        TarCompression::Xz => Box::new(lzma_rust2::XzReader::new(counted, true)),
    };
    let mut tar = tar::Archive::new(stream);
    for entry in tar.entries().map_err(|e| corrupt(archive, e))? {
        let mut entry = entry.map_err(|e| corrupt(archive, e))?;
        sink.begin_entry()?;
        let name = String::from_utf8(entry.path_bytes().into_owned())
            .map_err(|_| corrupt(archive, "entry name is not UTF-8"))?;
        let kind = entry.header().entry_type();
        let is_dir = match kind {
            tar::EntryType::Regular | tar::EntryType::Continuous => false,
            tar::EntryType::Directory => true,
            // Global PAX headers carry metadata only.
            tar::EntryType::XGlobalHeader => continue,
            tar::EntryType::Symlink => return Err(unsafe_entry(&name, "symbolic link")),
            tar::EntryType::Link => return Err(unsafe_entry(&name, "hard link")),
            tar::EntryType::Char | tar::EntryType::Block | tar::EntryType::Fifo => {
                return Err(unsafe_entry(&name, "special file (device or fifo)"));
            }
            other => {
                return Err(unsafe_entry(
                    &name,
                    &format!("unsupported tar entry type {other:?}"),
                ));
            }
        };
        let Some(path) = sanitize(&name)? else {
            continue;
        };
        if is_dir {
            sink.dir(&path)?;
            continue;
        }
        let size = entry.size();
        let mtime = entry
            .header()
            .mtime()
            .ok()
            .map(|secs| SystemTime::UNIX_EPOCH + Duration::from_secs(secs));
        sink.file(&name, &path, &mut entry, Some(size), mtime)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unix_modes() {
        check_unix_mode("a", 0).unwrap();
        check_unix_mode("a", 0o100_644).unwrap();
        check_unix_mode("a", 0o040_755).unwrap();
        assert!(check_unix_mode("a", 0o120_777).is_err());
        assert!(check_unix_mode("a", 0o020_644).is_err());
        assert!(check_unix_mode("a", 0o010_644).is_err());
    }

    #[test]
    fn filetime_conversion() {
        assert_eq!(
            filetime_to_system(FILETIME_UNIX_EPOCH),
            Some(SystemTime::UNIX_EPOCH)
        );
        assert_eq!(
            filetime_to_system(FILETIME_UNIX_EPOCH + 10_000_000),
            Some(SystemTime::UNIX_EPOCH + Duration::from_secs(1))
        );
        assert_eq!(filetime_to_system(0), None);
    }
}
