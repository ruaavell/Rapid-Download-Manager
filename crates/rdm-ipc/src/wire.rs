use rdm_common::{RdmError, Result};
use std::io;
use tokio::io::{AsyncBufRead, AsyncBufReadExt, AsyncWrite, AsyncWriteExt};

/// Read one newline-delimited message with a hard size cap.
///
/// `read_until` would let a misbehaving peer allocate unbounded memory before
/// the newline arrives, so this reads through the buffered reader directly.
pub async fn read_line_limited<R>(reader: &mut R, max: usize) -> io::Result<Option<Vec<u8>>>
where
    R: AsyncBufRead + Unpin,
{
    let mut output = Vec::new();
    loop {
        let available = reader.fill_buf().await?;
        if available.is_empty() {
            if output.is_empty() {
                return Ok(None);
            }
            return Ok(Some(output));
        }
        if let Some(position) = available.iter().position(|byte| *byte == b'\n') {
            output.extend_from_slice(&available[..position]);
            reader.consume(position + 1);
            if output.len() > max {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "IPC message exceeds the size limit",
                ));
            }
            trim_carriage_return(&mut output);
            return Ok(Some(output));
        }
        let length = available.len();
        output.extend_from_slice(available);
        reader.consume(length);
        if output.len() > max {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "IPC message exceeds the size limit",
            ));
        }
    }
}

fn trim_carriage_return(value: &mut Vec<u8>) {
    if value.last() == Some(&b'\r') {
        value.pop();
    }
}

pub async fn write_json_line<W, T>(writer: &mut W, value: &T) -> Result<()>
where
    W: AsyncWrite + Unpin,
    T: serde::Serialize,
{
    let mut encoded = serde_json::to_vec(value)
        .map_err(|error| RdmError::other(format!("encode IPC: {error}")))?;
    encoded.push(b'\n');
    writer
        .write_all(&encoded)
        .await
        .map_err(|error| RdmError::network(format!("write IPC message: {error}")))?;
    writer
        .flush()
        .await
        .map_err(|error| RdmError::network(format!("flush IPC message: {error}")))
}

pub async fn read_json_limited<R, T>(reader: &mut R, max: usize) -> Result<Option<T>>
where
    R: AsyncBufRead + Unpin,
    T: serde::de::DeserializeOwned,
{
    let line = read_line_limited(reader, max)
        .await
        .map_err(|error| RdmError::network(format!("read IPC message: {error}")))?;
    let Some(line) = line else {
        return Ok(None);
    };
    if line.is_empty() {
        return Err(RdmError::network("empty IPC message"));
    }
    serde_json::from_slice(&line)
        .map(Some)
        .map_err(|error| RdmError::network(format!("decode IPC message: {error}")))
}

/// Constant-time comparison for the IPC token.
pub fn token_matches(expected: &str, provided: &str) -> bool {
    let expected = expected.as_bytes();
    let provided = provided.as_bytes();
    if expected.len() != provided.len() {
        return false;
    }
    let mut difference = 0u8;
    for (left, right) in expected.iter().zip(provided.iter()) {
        difference |= left ^ right;
    }
    difference == 0
}

pub fn generate_token() -> String {
    use rand::RngCore;
    let mut bytes = [0u8; 32];
    rand::rngs::OsRng.fill_bytes(&mut bytes);
    hex::encode(bytes)
}

/// Effective uid of the peer that connected to a Unix socket.
pub fn peer_uid(stream: &tokio::net::UnixStream) -> io::Result<u32> {
    use std::os::fd::AsRawFd;
    let fd = stream.as_raw_fd();
    let mut credentials: libc::ucred = unsafe { std::mem::zeroed() };
    let mut length = std::mem::size_of::<libc::ucred>() as libc::socklen_t;
    let result = unsafe {
        libc::getsockopt(
            fd,
            libc::SOL_SOCKET,
            libc::SO_PEERCRED,
            &mut credentials as *mut libc::ucred as *mut libc::c_void,
            &mut length,
        )
    };
    if result != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(credentials.uid)
}

pub fn current_uid() -> u32 {
    unsafe { libc::geteuid() }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::BufReader;

    #[tokio::test]
    async fn reads_and_limits_lines() {
        let data = b"one\r\ntwo\nthree".to_vec();
        let mut reader = BufReader::new(std::io::Cursor::new(data));
        assert_eq!(
            read_line_limited(&mut reader, 16).await.unwrap().unwrap(),
            b"one"
        );
        assert_eq!(
            read_line_limited(&mut reader, 16).await.unwrap().unwrap(),
            b"two"
        );
        assert_eq!(
            read_line_limited(&mut reader, 16).await.unwrap().unwrap(),
            b"three"
        );
        assert!(read_line_limited(&mut reader, 16).await.unwrap().is_none());
    }

    #[tokio::test]
    async fn rejects_oversized_lines() {
        let data = b"0123456789abcdef\n".to_vec();
        let mut reader = BufReader::new(std::io::Cursor::new(data));
        let error = read_line_limited(&mut reader, 8).await.unwrap_err();
        assert_eq!(error.kind(), std::io::ErrorKind::InvalidData);
    }

    #[test]
    fn token_comparison_is_exact() {
        assert!(token_matches("abc", "abc"));
        assert!(!token_matches("abc", "abd"));
        assert!(!token_matches("abc", "abcd"));
    }
}
