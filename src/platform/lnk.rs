//! Minimal reader for Windows shell links (`.lnk`, [MS-SHLLINK]): just enough
//! to get the target path of Start Menu shortcuts. Platform-neutral so it is
//! unit-tested on every host.

const HEADER_SIZE: usize = 0x4C;
const HAS_ID_LIST: u32 = 0x1;
const HAS_LINK_INFO: u32 = 0x2;
const VOLUME_ID_AND_LOCAL_BASE_PATH: u32 = 0x1;

fn u16_at(d: &[u8], at: usize) -> Option<usize> {
    Some(u16::from_le_bytes(d.get(at..at + 2)?.try_into().ok()?).into())
}

fn u32_at(d: &[u8], at: usize) -> Option<usize> {
    usize::try_from(u32::from_le_bytes(d.get(at..at + 4)?.try_into().ok()?)).ok()
}

/// The local target path, e.g. `C:\Program Files\App\app.exe`. `None` for
/// shortcuts without one (advertised/MSI shortcuts, UWP apps, URLs, …).
pub fn target_path(d: &[u8]) -> Option<String> {
    if u32_at(d, 0)? != HEADER_SIZE {
        return None;
    }
    let flags = u32::try_from(u32_at(d, 0x14)?).ok()?;
    let mut at = HEADER_SIZE;
    if flags & HAS_ID_LIST != 0 {
        at += 2 + u16_at(d, at)?;
    }
    if flags & HAS_LINK_INFO == 0 {
        return None;
    }
    let info = d.get(at..at + u32_at(d, at)?)?;
    let header_size = u32_at(info, 4)?;
    if u32::try_from(u32_at(info, 8)?).ok()? & VOLUME_ID_AND_LOCAL_BASE_PATH == 0 {
        return None;
    }
    if header_size >= 0x24 {
        // LocalBasePathOffsetUnicode
        let start = u32_at(info, 0x1C)?;
        let units: Vec<u16> = info
            .get(start..)?
            .as_chunks::<2>()
            .0
            .iter()
            .map(|c| u16::from_le_bytes(*c))
            .take_while(|&u| u != 0)
            .collect();
        return String::from_utf16(&units).ok().filter(|s| !s.is_empty());
    }
    // LocalBasePath, in the system code page; Latin-1 is right for ASCII paths.
    let start = u32_at(info, 0x10)?;
    let bytes = info.get(start..)?;
    let end = bytes.iter().position(|&b| b == 0)?;
    Some(bytes[..end].iter().map(|&b| char::from(b)).collect()).filter(|s: &String| !s.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Builds a link with an ID list (to be skipped) and an ANSI `LinkInfo`.
    fn sample(path: &str) -> Vec<u8> {
        let mut d = vec![0u8; HEADER_SIZE];
        d[0..4].copy_from_slice(&0x4Cu32.to_le_bytes());
        d[0x14..0x18].copy_from_slice(&(HAS_ID_LIST | HAS_LINK_INFO).to_le_bytes());
        d.extend_from_slice(&3u16.to_le_bytes());
        d.extend_from_slice(&[9, 9, 9]); // opaque ID list
        let header = 0x1Cusize;
        let size = header + path.len() + 1;
        let mut info = vec![0u8; header];
        info[0..4].copy_from_slice(&u32::try_from(size).unwrap().to_le_bytes());
        info[4..8].copy_from_slice(&u32::try_from(header).unwrap().to_le_bytes());
        info[8..12].copy_from_slice(&VOLUME_ID_AND_LOCAL_BASE_PATH.to_le_bytes());
        info[0x10..0x14].copy_from_slice(&u32::try_from(header).unwrap().to_le_bytes());
        info.extend_from_slice(path.as_bytes());
        info.push(0);
        d.extend_from_slice(&info);
        d
    }

    #[test]
    fn reads_ansi_target() {
        let p = r"C:\Program Files\Mozilla Firefox\firefox.exe";
        assert_eq!(target_path(&sample(p)).as_deref(), Some(p));
    }

    #[test]
    fn rejects_garbage_without_panicking() {
        assert_eq!(target_path(b""), None);
        assert_eq!(target_path(&[0x4C, 0, 0, 0]), None);
        let mut truncated = sample(r"C:\x.exe");
        truncated.truncate(truncated.len() - 4);
        assert_eq!(target_path(&truncated), None);
        let mut no_info = sample(r"C:\x.exe");
        no_info[0x14] = HAS_ID_LIST.try_into().unwrap();
        assert_eq!(target_path(&no_info), None);
    }
}
