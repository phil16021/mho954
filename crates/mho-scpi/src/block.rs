//! IEEE 488.2 arbitrary block header parsing (`#<n><len digits><data>`).

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BlockHeader {
    /// Length of the header itself in bytes (`#`, digit count, digits).
    pub header_len: usize,
    /// Payload length, `None` for the indefinite form `#0` (data runs until `\n` + EOM).
    pub data_len: Option<usize>,
}

/// Parses a block header from the start of `buf`.
///
/// Returns `Ok(None)` if more bytes are needed, `Err` if the data is not a block.
pub fn parse_block_header(buf: &[u8]) -> Result<Option<BlockHeader>, String> {
    let Some(&first) = buf.first() else {
        return Ok(None);
    };
    if first != b'#' {
        let preview = String::from_utf8_lossy(&buf[..buf.len().min(40)]).into_owned();
        return Err(format!("expected '#' block header, got {preview:?}"));
    }
    let Some(&digits) = buf.get(1) else {
        return Ok(None);
    };
    if !digits.is_ascii_digit() {
        return Err(format!("invalid block digit count {:?}", digits as char));
    }
    let n = (digits - b'0') as usize;
    if n == 0 {
        return Ok(Some(BlockHeader {
            header_len: 2,
            data_len: None,
        }));
    }
    if buf.len() < 2 + n {
        return Ok(None);
    }
    let len_str = std::str::from_utf8(&buf[2..2 + n]).map_err(|e| e.to_string())?;
    let data_len = len_str
        .parse::<usize>()
        .map_err(|e| format!("block length {len_str:?}: {e}"))?;
    Ok(Some(BlockHeader {
        header_len: 2 + n,
        data_len: Some(data_len),
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_definite_header() {
        let h = parse_block_header(b"#9000001000abc").unwrap().unwrap();
        assert_eq!(
            h,
            BlockHeader {
                header_len: 11,
                data_len: Some(1000)
            }
        );
        let h = parse_block_header(b"#41234").unwrap().unwrap();
        assert_eq!(
            h,
            BlockHeader {
                header_len: 6,
                data_len: Some(1234)
            }
        );
    }

    #[test]
    fn needs_more_data() {
        assert_eq!(parse_block_header(b"").unwrap(), None);
        assert_eq!(parse_block_header(b"#").unwrap(), None);
        assert_eq!(parse_block_header(b"#900000").unwrap(), None);
    }

    #[test]
    fn indefinite_header() {
        let h = parse_block_header(b"#0data").unwrap().unwrap();
        assert_eq!(
            h,
            BlockHeader {
                header_len: 2,
                data_len: None
            }
        );
    }

    #[test]
    fn rejects_non_block() {
        assert!(parse_block_header(b"1.0E-3\n").is_err());
        assert!(parse_block_header(b"#x").is_err());
    }
}
