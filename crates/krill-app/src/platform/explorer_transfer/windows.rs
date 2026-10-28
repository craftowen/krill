#![expect(
    unsafe_code,
    reason = "Handle-relative Windows filesystem operations (CONVENTIONS U1)"
)]
use super::{Component, File, OsStr, OsString, Path, PathBuf, denied, io, normal};
use std::collections::VecDeque;
use std::fs::OpenOptions;
use std::os::windows::{
    ffi::{OsStrExt as _, OsStringExt as _},
    fs::{MetadataExt as _, OpenOptionsExt as _},
    io::{AsRawHandle as _, FromRawHandle as _},
};
use windows_sys::{
    Wdk::{Foundation::OBJECT_ATTRIBUTES, Storage::FileSystem as nt},
    Win32::{
        Foundation::{self as foundation, UNICODE_STRING},
        Storage::FileSystem as win,
        System::IO::IO_STATUS_BLOCK,
    },
};

pub(super) struct Directory {
    pub path: PathBuf,
    ancestors: Vec<File>,
    file: File,
}

fn nt_result(status: i32) -> io::Result<()> {
    if status >= 0 {
        return Ok(());
    }
    // SAFETY: Pure conversion of the returned NT status, with no pointers.
    let code = unsafe { foundation::RtlNtStatusToDosError(status) };
    Err(i32::try_from(code).map_or_else(
        |_| io::Error::other(format!("Windows filesystem status {status}")),
        io::Error::from_raw_os_error,
    ))
}

fn final_path(file: &File) -> io::Result<PathBuf> {
    let mut buffer = vec![0u16; 512];
    loop {
        let capacity = u32::try_from(buffer.len()).map_err(|_| denied())?;
        // SAFETY: file owns a live handle; buffer is writable for capacity UTF-16 units.
        let length = unsafe {
            win::GetFinalPathNameByHandleW(file.as_raw_handle(), buffer.as_mut_ptr(), capacity, 0)
        };
        if length == 0 {
            return Err(io::Error::last_os_error());
        }
        let length = usize::try_from(length).map_err(|_| denied())?;
        if length < buffer.len() {
            return Ok(PathBuf::from(OsString::from_wide(&buffer[..length])));
        }
        if length > 32768 {
            return Err(denied());
        }
        buffer.resize(length + 1, 0);
    }
}

fn validate(file: &File) -> io::Result<()> {
    let meta = file.metadata()?;
    if meta.file_attributes() & win::FILE_ATTRIBUTE_REPARSE_POINT != 0
        || (!meta.is_file() && !meta.is_dir())
    {
        return Err(denied());
    }
    // Normalized handle names include long names, so .git short-name aliases
    // are rejected without reopening a mutable path.
    if final_path(file)?
        .components()
        .any(|c| matches!(c, Component::Normal(n) if !normal(n)))
    {
        return Err(denied());
    }
    Ok(())
}

fn open_at(
    parent: &File,
    name: &OsStr,
    access: u32,
    disposition: u32,
    options: u32,
) -> io::Result<File> {
    validate(parent)?;
    if super::relative(Path::new(name))?.len() != 1 {
        return Err(denied());
    }
    let mut wide: Vec<u16> = name.encode_wide().collect();
    if wide.contains(&0) {
        return Err(denied());
    }
    let length =
        u16::try_from(wide.len().checked_mul(2).ok_or_else(denied)?).map_err(|_| denied())?;
    let unicode = UNICODE_STRING {
        Length: length,
        MaximumLength: length,
        Buffer: wide.as_mut_ptr(),
    };
    let attributes = OBJECT_ATTRIBUTES {
        Length: u32::try_from(std::mem::size_of::<OBJECT_ATTRIBUTES>()).map_err(|_| denied())?,
        RootDirectory: parent.as_raw_handle(),
        ObjectName: &raw const unicode,
        Attributes: foundation::OBJ_DONT_REPARSE,
        ..Default::default()
    };
    let mut status = IO_STATUS_BLOCK::default();
    let mut handle = std::ptr::null_mut();
    // SAFETY: All pointers reference initialized locals for this synchronous call.
    // The single component is opened relative to the owned parent, never a path
    // assembled from mutable ancestors; reparse processing is disabled.
    let result = unsafe {
        nt::NtCreateFile(
            &raw mut handle,
            access | win::FILE_READ_ATTRIBUTES | win::SYNCHRONIZE,
            &raw const attributes,
            &raw mut status,
            std::ptr::null(),
            win::FILE_ATTRIBUTE_NORMAL,
            win::FILE_SHARE_READ | win::FILE_SHARE_WRITE,
            disposition,
            options | nt::FILE_OPEN_REPARSE_POINT | nt::FILE_SYNCHRONOUS_IO_NONALERT,
            std::ptr::null(),
            0,
        )
    };
    nt_result(result)?;
    // SAFETY: A successful NtCreateFile returned a unique owned file handle.
    let file = unsafe { File::from_raw_handle(handle) };
    validate(&file)?;
    validate(parent)?;
    Ok(file)
}

fn identity(file: &File) -> io::Result<(u32, u32, u32)> {
    let mut info = win::BY_HANDLE_FILE_INFORMATION::default();
    // SAFETY: file is live and info is a writable correctly sized output struct.
    let result = unsafe { win::GetFileInformationByHandle(file.as_raw_handle(), &raw mut info) };
    if result == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok((
        info.dwVolumeSerialNumber,
        info.nFileIndexHigh,
        info.nFileIndexLow,
    ))
}

impl Directory {
    pub fn absolute(path: &Path) -> io::Result<Self> {
        if !path.is_absolute() {
            return Err(denied());
        }
        let mut parts = path.components();
        let prefix = parts.next().ok_or_else(denied)?;
        // Network/device namespace roots have no supported volume-root anchor.
        if !matches!(prefix, Component::Prefix(p) if matches!(p.kind(), std::path::Prefix::Disk(_) | std::path::Prefix::VerbatimDisk(_)))
            || !matches!(parts.next(), Some(Component::RootDir))
        {
            return Err(io::ErrorKind::Unsupported.into());
        }
        let mut root = PathBuf::from(prefix.as_os_str());
        root.push("\\");
        let file = OpenOptions::new()
            .access_mode(win::FILE_READ_ATTRIBUTES | win::FILE_TRAVERSE)
            .share_mode(win::FILE_SHARE_READ | win::FILE_SHARE_WRITE)
            .custom_flags(win::FILE_FLAG_OPEN_REPARSE_POINT | win::FILE_FLAG_BACKUP_SEMANTICS)
            .open(&root)?;
        validate(&file)?;
        let dir = Self {
            path: root,
            ancestors: Vec::new(),
            file,
        };
        let tail: PathBuf = parts.collect();
        dir.descend(&super::relative(&tail)?)
    }
    pub fn descend(&self, parts: &[OsString]) -> io::Result<Self> {
        let mut dir = Self {
            path: self.path.clone(),
            ancestors: self
                .ancestors
                .iter()
                .map(File::try_clone)
                .collect::<io::Result<_>>()?,
            file: self.file.try_clone()?,
        };
        for name in parts {
            let file = open_at(
                &dir.file,
                name,
                win::FILE_READ_ATTRIBUTES | win::FILE_TRAVERSE,
                nt::FILE_OPEN,
                nt::FILE_DIRECTORY_FILE,
            )?;
            dir.ancestors.push(dir.file);
            dir.file = file;
            dir.path.push(name);
        }
        Ok(dir)
    }
    pub fn open_node(&self, name: &OsStr, moving: bool) -> io::Result<File> {
        open_at(
            &self.file,
            name,
            win::FILE_GENERIC_READ | if moving { win::DELETE } else { 0 },
            nt::FILE_OPEN,
            nt::FILE_OPEN_FOR_BACKUP_INTENT,
        )
    }
    pub fn contains(&self, node: &File, _: &Path) -> io::Result<bool> {
        let source = identity(node)?;
        for file in self.ancestors.iter().chain(std::iter::once(&self.file)) {
            if identity(file)? == source {
                return Ok(true);
            }
        }
        Ok(false)
    }
    pub fn child(&self, name: &OsStr, file: File) -> Self {
        Self {
            path: self.path.join(name),
            ancestors: Vec::new(),
            file,
        }
    }
    pub fn names(&self) -> io::Result<Names> {
        Ok(Names {
            file: self.file.try_clone()?,
            pending: VecDeque::new(),
            restart: true,
            done: false,
        })
    }
    pub fn create_file(&self, name: &OsStr) -> io::Result<File> {
        open_at(
            &self.file,
            name,
            win::FILE_GENERIC_WRITE,
            nt::FILE_CREATE,
            nt::FILE_NON_DIRECTORY_FILE,
        )
    }
    pub fn create_dir(&self, name: &OsStr) -> io::Result<Self> {
        let file = open_at(
            &self.file,
            name,
            win::FILE_GENERIC_READ,
            nt::FILE_CREATE,
            nt::FILE_DIRECTORY_FILE,
        )?;
        Ok(self.child(name, file))
    }
    pub fn move_new(
        _source: &Self,
        _: &OsStr,
        node: &File,
        target: &Self,
        name: &OsStr,
    ) -> io::Result<()> {
        let wide: Vec<u16> = name.encode_wide().collect();
        let name_bytes = wide.len().checked_mul(2).ok_or_else(denied)?;
        let offset = std::mem::offset_of!(nt::FILE_RENAME_INFORMATION, FileName);
        let size = std::mem::size_of::<nt::FILE_RENAME_INFORMATION>() + name_bytes;
        let size_u32 = u32::try_from(size).map_err(|_| denied())?;
        let mut bytes = vec![0u8; size];
        let root_offset = std::mem::offset_of!(nt::FILE_RENAME_INFORMATION, RootDirectory);
        let root = target.file.as_raw_handle() as usize;
        bytes[root_offset..root_offset + std::mem::size_of::<usize>()]
            .copy_from_slice(&root.to_ne_bytes());
        let length_offset = std::mem::offset_of!(nt::FILE_RENAME_INFORMATION, FileNameLength);
        bytes[length_offset..length_offset + 4].copy_from_slice(
            &u32::try_from(name_bytes)
                .map_err(|_| denied())?
                .to_ne_bytes(),
        );
        for (slot, character) in bytes[offset..offset + name_bytes]
            .as_chunks_mut::<2>()
            .0
            .iter_mut()
            .zip(wide)
        {
            slot.copy_from_slice(&character.to_ne_bytes());
        }
        let aligned: Vec<usize> = bytes
            .chunks(std::mem::size_of::<usize>())
            .map(|chunk| {
                let mut word = [0u8; std::mem::size_of::<usize>()];
                word[..chunk.len()].copy_from_slice(chunk);
                usize::from_ne_bytes(word)
            })
            .collect();
        let mut status = IO_STATUS_BLOCK::default();
        // SAFETY: Both handles are owned and live. Aligned storage covers a zero
        // ReplaceIfExists, target root handle, and simple relative UTF-16 name.
        let result = unsafe {
            nt::NtSetInformationFile(
                node.as_raw_handle(),
                &raw mut status,
                aligned.as_ptr().cast(),
                size_u32,
                nt::FileRenameInformation,
            )
        };
        nt_result(result)
    }
}

pub(super) struct Names {
    file: File,
    pending: VecDeque<OsString>,
    restart: bool,
    done: bool,
}
impl Names {
    fn read_batch(&mut self) -> io::Result<()> {
        let mut aligned = [0u64; 1024];
        let class = if self.restart {
            win::FileIdBothDirectoryRestartInfo
        } else {
            win::FileIdBothDirectoryInfo
        };
        self.restart = false;
        // SAFETY: file is a live directory handle with listing access. aligned
        // is an initialized, 8-byte-aligned 8192-byte output buffer.
        let result = unsafe {
            win::GetFileInformationByHandleEx(
                self.file.as_raw_handle(),
                class,
                aligned.as_mut_ptr().cast(),
                8192,
            )
        };
        if result == 0 {
            let error = io::Error::last_os_error();
            if error.raw_os_error() == Some(18) {
                self.done = true;
                return Ok(());
            }
            return Err(error);
        }
        let bytes: Vec<u8> = aligned.into_iter().flat_map(u64::to_ne_bytes).collect();
        let field = |offset: usize| -> io::Result<u32> {
            Ok(u32::from_ne_bytes(
                bytes
                    .get(offset..offset + 4)
                    .ok_or_else(denied)?
                    .try_into()
                    .map_err(|_| denied())?,
            ))
        };
        let mut offset = 0usize;
        loop {
            let next = usize::try_from(field(offset)?).map_err(|_| denied())?;
            let len = usize::try_from(field(
                offset + std::mem::offset_of!(win::FILE_ID_BOTH_DIR_INFO, FileNameLength),
            )?)
            .map_err(|_| denied())?;
            let start = offset + std::mem::offset_of!(win::FILE_ID_BOTH_DIR_INFO, FileName);
            let name = bytes
                .get(start..start.checked_add(len).ok_or_else(denied)?)
                .ok_or_else(denied)?;
            if name.is_empty() || name.len() % 2 != 0 {
                return Err(denied());
            }
            let wide: Vec<u16> = name
                .as_chunks::<2>()
                .0
                .iter()
                .map(|c| u16::from_ne_bytes(*c))
                .collect();
            let name = OsString::from_wide(&wide);
            if name != "." && name != ".." {
                self.pending.push_back(name);
            }
            if next == 0 {
                break;
            }
            if next < std::mem::size_of::<win::FILE_ID_BOTH_DIR_INFO>() {
                return Err(denied());
            }
            offset = offset
                .checked_add(next)
                .filter(|n| *n < bytes.len())
                .ok_or_else(denied)?;
        }
        Ok(())
    }
}
impl Iterator for Names {
    type Item = io::Result<OsString>;
    fn next(&mut self) -> Option<Self::Item> {
        loop {
            if let Some(name) = self.pending.pop_front() {
                return Some(Ok(name));
            }
            if self.done {
                return None;
            }
            if let Err(error) = self.read_batch() {
                self.done = true;
                return Some(Err(error));
            }
        }
    }
}

/// Test-only real NTFS mutation; the production implementation has no injected syscall.
#[cfg(test)]
pub(super) fn attach_junction(directory: &Path, target: &Path) -> io::Result<()> {
    let file = OpenOptions::new()
        .write(true)
        .share_mode(win::FILE_SHARE_READ | win::FILE_SHARE_WRITE | win::FILE_SHARE_DELETE)
        .custom_flags(win::FILE_FLAG_OPEN_REPARSE_POINT | win::FILE_FLAG_BACKUP_SEMANTICS)
        .open(directory)?;
    let target = target.canonicalize()?;
    let wide: Vec<u16> = target.as_os_str().encode_wide().collect();
    if !wide.starts_with(&[92, 92, 63, 92]) {
        return Err(denied());
    }
    let substitute: Vec<u16> = [92, 63, 63, 92]
        .into_iter()
        .chain(wide[4..].iter().copied())
        .collect();
    let length = u16::try_from(substitute.len() * 2).map_err(|_| denied())?;
    let mut bytes = Vec::new();
    bytes.extend_from_slice(&0xA000_0003u32.to_ne_bytes()); // IO_REPARSE_TAG_MOUNT_POINT
    bytes.extend_from_slice(&(length + 12).to_ne_bytes());
    bytes.extend_from_slice(&0u16.to_ne_bytes()); // reserved
    bytes.extend_from_slice(&0u16.to_ne_bytes()); // substitute offset
    bytes.extend_from_slice(&length.to_ne_bytes());
    bytes.extend_from_slice(&(length + 2).to_ne_bytes()); // empty print name
    bytes.extend_from_slice(&0u16.to_ne_bytes());
    for character in substitute {
        bytes.extend_from_slice(&character.to_ne_bytes());
    }
    bytes.extend_from_slice(&[0; 4]);
    let size = u32::try_from(bytes.len()).map_err(|_| denied())?;
    let aligned: Vec<usize> = bytes
        .chunks(std::mem::size_of::<usize>())
        .map(|chunk| {
            let mut word = [0u8; std::mem::size_of::<usize>()];
            word[..chunk.len()].copy_from_slice(chunk);
            usize::from_ne_bytes(word)
        })
        .collect();
    let mut returned = 0;
    // SAFETY: An owned directory handle and initialized, aligned mount-point
    // reparse buffer remain live for the synchronous FSCTL_SET_REPARSE_POINT.
    let result = unsafe {
        windows_sys::Win32::System::IO::DeviceIoControl(
            file.as_raw_handle(),
            0x0009_00A4,
            aligned.as_ptr().cast(),
            size,
            std::ptr::null_mut(),
            0,
            &raw mut returned,
            std::ptr::null_mut(),
        )
    };
    if result == 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(())
    }
}
