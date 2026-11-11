//! The NRPE wire format.
//!
//! Every field is big-endian. A **version 2** packet has a fixed size that both
//! sides must agree on beforehand:
//!
//! ```text
//! int16  version (2)
//! int16  type    (1 query, 2 response, 3 response with more to follow)
//! uint32 crc32   (of the whole packet, with this field zeroed)
//! int16  result  (the Nagios status in a response)
//! char   buffer[payload length]   NUL terminated, 1024 unless reconfigured
//! char   padding[2]
//! ```
//!
//! Type 3 is an NSClient++ extension (`extended response`, on by default): an
//! answer too long for one buffer arrives as a run of type 3 packets closed by
//! a type 2 one.
//!
//! A **version 4** packet carries its own length instead:
//!
//! ```text
//! int16  version (4)
//! int16  type
//! uint32 crc32
//! int16  result
//! int16  alignment (0)
//! int32  buffer length
//! char   buffer[buffer length]
//! ```
//!
//! Version 3 has the same fields but counts three stray alignment bytes into
//! the packet. NSClient++ does not send it and neither does this client.

use anyhow::{Context, anyhow, bail};
use std::io::ErrorKind;
use tokio::io::{AsyncRead, AsyncReadExt};

pub const VERSION_2: i16 = 2;
pub const VERSION_4: i16 = 4;

pub const TYPE_QUERY: i16 = 1;
pub const TYPE_RESPONSE: i16 = 2;
/// NSClient++ extension: a response packet that is followed by another one.
pub const TYPE_MORE_RESPONSE: i16 = 3;

/// The payload length every stock NRPE daemon and client is compiled with.
pub const DEFAULT_PAYLOAD_LENGTH: usize = 1024;
/// The smallest payload NSClient++ agrees to start with.
pub const MIN_PAYLOAD_LENGTH: usize = 16;
/// Largest buffer accepted from the wire. NSClient++ applies the same cap.
pub const MAX_PAYLOAD_LENGTH: usize = 1024 * 1024;

/// Offset of the buffer in a version 2 packet.
const V2_HEADER: usize = 10;
/// Bytes after the buffer in a version 2 packet (struct padding in the C source).
const V2_PADDING: usize = 2;
/// Offset of the buffer in a version 4 packet.
const V4_HEADER: usize = 16;

/// Size on the wire of a version 2 packet with a `payload_length` byte buffer.
pub const fn v2_packet_length(payload_length: usize) -> usize {
    V2_HEADER + payload_length + V2_PADDING
}

/// One decoded packet.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Packet {
    pub version: i16,
    pub packet_type: i16,
    pub result_code: i16,
    /// The buffer up to (not including) its first NUL.
    pub payload: Vec<u8>,
}

/// CRC-32 as zlib computes it (reflected, polynomial 0xEDB88320), which is what
/// NRPE uses.
pub fn crc32(data: &[u8]) -> u32 {
    let mut crc = 0xFFFF_FFFFu32;
    for byte in data {
        crc ^= u32::from(*byte);
        for _ in 0..8 {
            crc = if crc & 1 == 1 {
                (crc >> 1) ^ 0xEDB8_8320
            } else {
                crc >> 1
            };
        }
    }
    !crc
}

/// Write the checksum of `packet` (taken with the checksum field zeroed) into it.
fn seal(packet: &mut [u8]) {
    packet[4..8].fill(0);
    let crc = crc32(packet);
    packet[4..8].copy_from_slice(&crc.to_be_bytes());
}

/// Does the checksum carried by `packet` match its content?
fn verify(packet: &[u8]) -> Result<(), (u32, u32)> {
    let carried = u32::from_be_bytes([packet[4], packet[5], packet[6], packet[7]]);
    let mut copy = packet.to_vec();
    copy[4..8].fill(0);
    let computed = crc32(&copy);
    if carried == computed {
        Ok(())
    } else {
        Err((carried, computed))
    }
}

fn header(version: i16, packet_type: i16, result_code: i16, packet: &mut [u8]) {
    packet[0..2].copy_from_slice(&version.to_be_bytes());
    packet[2..4].copy_from_slice(&packet_type.to_be_bytes());
    packet[8..10].copy_from_slice(&result_code.to_be_bytes());
}

/// Build a version 2 packet. The payload has to leave room for its NUL.
pub fn encode_v2(
    packet_type: i16,
    result_code: i16,
    payload: &[u8],
    payload_length: usize,
) -> anyhow::Result<Vec<u8>> {
    if payload.len() >= payload_length {
        bail!(
            "{} bytes do not fit in a version 2 packet, which holds at most {}",
            payload.len(),
            payload_length.saturating_sub(1)
        );
    }
    let mut packet = vec![0u8; v2_packet_length(payload_length)];
    header(VERSION_2, packet_type, result_code, &mut packet);
    packet[V2_HEADER..V2_HEADER + payload.len()].copy_from_slice(payload);
    seal(&mut packet);
    Ok(packet)
}

/// Build a version 4 packet.
///
/// The buffer is NUL terminated and padded until the packet is as large as a
/// stock version 2 packet, exactly as `check_nrpe` does it: a daemon reads that
/// many bytes before it knows which version it is looking at, and not all of
/// them cope with less.
pub fn encode_v4(packet_type: i16, result_code: i16, payload: &[u8]) -> anyhow::Result<Vec<u8>> {
    if payload.len() >= MAX_PAYLOAD_LENGTH {
        bail!(
            "{} bytes do not fit in an NRPE packet, which holds at most {}",
            payload.len(),
            MAX_PAYLOAD_LENGTH - 1
        );
    }
    let length = (V4_HEADER + payload.len() + 1).max(v2_packet_length(DEFAULT_PAYLOAD_LENGTH));
    let mut packet = vec![0u8; length];
    header(VERSION_4, packet_type, result_code, &mut packet);
    let buffer_length = (length - V4_HEADER) as i32;
    packet[12..16].copy_from_slice(&buffer_length.to_be_bytes());
    packet[V4_HEADER..V4_HEADER + payload.len()].copy_from_slice(payload);
    seal(&mut packet);
    Ok(packet)
}

fn until_nul(buffer: &[u8]) -> Vec<u8> {
    let end = buffer.iter().position(|b| *b == 0).unwrap_or(buffer.len());
    buffer[..end].to_vec()
}

/// Read one packet of either version.
///
/// `payload_length` is only used for version 2, which does not say how long it
/// is: a peer configured with another length shows up here as a checksum error
/// or a short read.
pub async fn read_packet<R: AsyncRead + Unpin>(
    reader: &mut R,
    payload_length: usize,
) -> anyhow::Result<Packet> {
    let mut packet = vec![0u8; V2_HEADER];
    reader.read_exact(&mut packet).await?;
    let version = i16::from_be_bytes([packet[0], packet[1]]);
    match version {
        VERSION_2 => {
            packet.resize(v2_packet_length(payload_length), 0);
            reader
                .read_exact(&mut packet[V2_HEADER..])
                .await
                .map_err(|e| match e.kind() {
                    // The cause adds nothing to this: it only says "end of file".
                    ErrorKind::UnexpectedEof => anyhow!(
                        "the answer is shorter than a version 2 packet with a {payload_length} \
                         byte payload; does the server use another `payload length`?"
                    ),
                    _ => e.into(),
                })?;
        }
        VERSION_4 => {
            packet.resize(V4_HEADER, 0);
            reader.read_exact(&mut packet[V2_HEADER..]).await?;
            let declared = i32::from_be_bytes([packet[12], packet[13], packet[14], packet[15]]);
            let buffer_length = usize::try_from(declared)
                .ok()
                .filter(|length| *length <= MAX_PAYLOAD_LENGTH)
                .with_context(|| format!("the answer declares a {declared} byte buffer"))?;
            packet.resize(V4_HEADER + buffer_length, 0);
            reader.read_exact(&mut packet[V4_HEADER..]).await?;
        }
        other => bail!(
            "the answer is not NRPE version 2 or 4 (it starts with version {other}); \
             is TLS enabled on one side only?"
        ),
    }
    if let Err((carried, computed)) = verify(&packet) {
        let hint = if version == VERSION_2 {
            "; does the server use another `payload length`?"
        } else {
            ""
        };
        bail!("the answer fails its checksum ({carried:#010x} != {computed:#010x}){hint}");
    }
    let packet_type = i16::from_be_bytes([packet[2], packet[3]]);
    let result_code = i16::from_be_bytes([packet[8], packet[9]]);
    let buffer = if version == VERSION_2 {
        &packet[V2_HEADER..V2_HEADER + payload_length]
    } else {
        &packet[V4_HEADER..]
    };
    Ok(Packet {
        version,
        packet_type,
        result_code,
        payload: until_nul(buffer),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn decode(bytes: &[u8], payload_length: usize) -> anyhow::Result<Packet> {
        let mut reader = bytes;
        read_packet(&mut reader, payload_length).await
    }

    #[test]
    fn crc32_matches_the_zlib_check_value() {
        assert_eq!(crc32(b""), 0);
        assert_eq!(crc32(b"123456789"), 0xCBF4_3926);
    }

    #[test]
    fn v2_packet_has_the_stock_layout() {
        let packet = encode_v2(TYPE_QUERY, 3, b"check_cpu!80", 1024).unwrap();
        assert_eq!(packet.len(), 1036);
        assert_eq!(&packet[0..4], &[0, 2, 0, 1]);
        assert_eq!(&packet[8..10], &[0, 3]);
        assert_eq!(&packet[10..22], b"check_cpu!80");
        assert!(packet[22..].iter().all(|b| *b == 0));
        // The checksum is that of the packet with the field zeroed.
        let mut zeroed = packet.clone();
        zeroed[4..8].fill(0);
        assert_eq!(&packet[4..8], &crc32(&zeroed).to_be_bytes());
    }

    #[test]
    fn v2_packet_keeps_room_for_the_terminator() {
        assert!(encode_v2(TYPE_QUERY, 3, &[b'a'; 15], 16).is_ok());
        let err = encode_v2(TYPE_QUERY, 3, &[b'a'; 16], 16).unwrap_err();
        assert_eq!(
            err.to_string(),
            "16 bytes do not fit in a version 2 packet, which holds at most 15"
        );
    }

    #[test]
    fn v4_packet_is_padded_to_the_v2_size() {
        let packet = encode_v4(TYPE_QUERY, 3, b"_NRPE_CHECK").unwrap();
        assert_eq!(packet.len(), 1036);
        assert_eq!(&packet[0..4], &[0, 4, 0, 1]);
        assert_eq!(&packet[10..12], &[0, 0]);
        assert_eq!(&packet[12..16], &1020i32.to_be_bytes());
        assert_eq!(&packet[16..27], b"_NRPE_CHECK");
        assert_eq!(packet[27], 0);
    }

    #[test]
    fn v4_packet_grows_with_its_payload() {
        let payload = vec![b'x'; 5000];
        let packet = encode_v4(TYPE_QUERY, 3, &payload).unwrap();
        assert_eq!(packet.len(), 16 + 5000 + 1);
        assert_eq!(&packet[12..16], &5001i32.to_be_bytes());
        assert!(encode_v4(TYPE_QUERY, 3, &vec![0u8; MAX_PAYLOAD_LENGTH]).is_err());
    }

    #[tokio::test]
    async fn packets_round_trip() {
        let v2 = encode_v2(TYPE_MORE_RESPONSE, 2, b"CRITICAL: no", 64).unwrap();
        assert_eq!(
            decode(&v2, 64).await.unwrap(),
            Packet {
                version: 2,
                packet_type: TYPE_MORE_RESPONSE,
                result_code: 2,
                payload: b"CRITICAL: no".to_vec(),
            }
        );
        let v4 = encode_v4(TYPE_RESPONSE, 1, b"WARNING: hm").unwrap();
        assert_eq!(
            decode(&v4, 64).await.unwrap(),
            Packet {
                version: 4,
                packet_type: TYPE_RESPONSE,
                result_code: 1,
                payload: b"WARNING: hm".to_vec(),
            }
        );
    }

    /// NSClient++ answers version 4 with a buffer that is exactly the text: no
    /// terminator and no padding.
    #[tokio::test]
    async fn reads_an_unterminated_v4_buffer() {
        let mut packet = vec![0u8; 16];
        packet[0..4].copy_from_slice(&[0, 4, 0, 2]);
        packet[12..16].copy_from_slice(&2i32.to_be_bytes());
        packet.extend_from_slice(b"OK");
        seal(&mut packet);
        let decoded = decode(&packet, 1024).await.unwrap();
        assert_eq!(decoded.payload, b"OK");
        assert_eq!(decoded.result_code, 0);
    }

    #[tokio::test]
    async fn rejects_a_damaged_packet() {
        let mut packet = encode_v2(TYPE_RESPONSE, 0, b"OK", 64).unwrap();
        packet[11] ^= 0xff;
        let err = decode(&packet, 64).await.unwrap_err().to_string();
        assert!(err.contains("fails its checksum"), "{err}");
        assert!(err.contains("payload length"), "{err}");
    }

    #[tokio::test]
    async fn rejects_a_payload_length_mismatch() {
        // The server sends 1024 byte payloads, we expect larger ones: short read.
        let packet = encode_v2(TYPE_RESPONSE, 0, b"OK", 1024).unwrap();
        let err = format!("{:#}", decode(&packet, 4096).await.unwrap_err());
        assert!(err.contains("another `payload length`"), "{err}");
        // And the other way around: the checksum covers the wrong bytes.
        let err = format!("{:#}", decode(&packet, 512).await.unwrap_err());
        assert!(err.contains("fails its checksum"), "{err}");
    }

    #[tokio::test]
    async fn rejects_what_is_not_nrpe() {
        // A TLS alert record, as a TLS-only server answers a plain text client.
        let err = decode(&[0x15, 0x03, 0x03, 0, 2, 2, 40, 0, 0, 0], 1024)
            .await
            .unwrap_err()
            .to_string();
        assert!(err.contains("not NRPE version 2 or 4"), "{err}");
    }

    #[tokio::test]
    async fn rejects_an_absurd_v4_length() {
        for declared in [-1i32, (MAX_PAYLOAD_LENGTH + 1) as i32] {
            let mut packet = vec![0u8; 16];
            packet[0..4].copy_from_slice(&[0, 4, 0, 2]);
            packet[12..16].copy_from_slice(&declared.to_be_bytes());
            let err = decode(&packet, 1024).await.unwrap_err().to_string();
            assert!(err.contains("byte buffer"), "{err}");
        }
    }
}
